/**
 * 插件声明视图的渲染侧状态（docs/24 §7.3 第 3/4 刀）。
 *
 * 与 `plugin-activity-store` 同构（单例 + `useSyncExternalStore`），但数据来源不同：
 * 活动视图是**推送**（三条遥测通道），本 store 是**拉取**——`plugin:view` 事件只带
 * `{pluginId, viewId, rev}`，数据永远走 `plugin.call`（§4.5）。这条分工是刻意的：
 * 事件不带数据体，插件就没有绕过能力闸门、配额与审计往渲染进程灌字节的通道。
 *
 * 四个不变量：
 *
 * 1. **同一 rev 不重复拉**。插件可以在一次 `plugin.call` 里连发多次刷新通知，
 *    按 rev 去重比「按时间窗口」靠谱——rev 是插件自己给的单调值，不依赖时钟。
 * 2. **切换视图必须让旧请求作废**。用户连点两个视图时，先发的那个可能后到；
 *    用自增 `token` 丢弃过期响应，避免「面板标题是 A、内容是 B」。
 * 3. **拉取失败不保留上一次的树**。视图内容与插件状态强相关，留旧树等于骗人。
 * 4. **解析结果只存 `ParsedView`**。限幅、危险协议降级、命令未声明的禁用判定
 *    全在 `plugin-view.ts`，本 store 不二次判断，避免两套结论。
 */

import { useSyncExternalStore } from 'react'

import type { PluginViewRefreshEvent, PluginViewSummary } from '@ai-ide/shared-types'

import type { AiIdeApi } from '../../shared/ipc'
import { pluginApi } from './plugin-api'
import { parseView, type ParsedView } from './plugin-view'

/**
 * 事件载荷的类型定义在 `@ai-ide/shared-types`（preload 与渲染侧共用一份），
 * 这里只做转发，保持既有 `import type { PluginViewRefreshEvent } from './plugin-view-store'`
 * 的调用点不变。
 */
export type { PluginViewRefreshEvent }

export type PluginViewStatus = 'idle' | 'loading' | 'ready' | 'error'

export interface PluginViewState {
  /** 当前详情页对应的插件；`null` 表示没选中插件。 */
  pluginId: string | null
  /** 该插件在 `plugin.status` 里声明的视图（原样，不二次过滤）。 */
  views: readonly PluginViewSummary[]
  /** 当前选中的视图 id。 */
  activeViewId: string | null
  status: PluginViewStatus
  /** 已解析的视图；`status !== 'ready'` 时是 `null` 或上一次的 `rejected` 结果。 */
  view: ParsedView | null
  /** 已渲染的 rev；`null` 表示还没成功拉过。 */
  rev: number | null
  /** 失败人话（含内核错误、正文截断、JSON 坏掉、协议不符）。 */
  error: string | null
}

function initialState(): PluginViewState {
  return {
    pluginId: null,
    views: [],
    activeViewId: null,
    status: 'idle',
    view: null,
    rev: null,
    error: null
  }
}

let state: PluginViewState = initialState()

/** 请求代次：只让最后一次点击的响应落地。 */
let token = 0

const listeners = new Set<() => void>()

function emit(): void {
  for (const listener of listeners) listener()
}

function setState(patch: Partial<PluginViewState>): void {
  state = { ...state, ...patch }
  emit()
}

export function getPluginViewState(): PluginViewState {
  return state
}

export function subscribePluginView(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function usePluginView(): PluginViewState {
  return useSyncExternalStore(subscribePluginView, getPluginViewState, getPluginViewState)
}

/** 测试用：模块级单例跨用例不清就会串状态。 */
export function resetPluginViewState(): void {
  token += 1
  state = initialState()
  emit()
}

/**
 * 详情页拿到新的 `views` 时同步进来。
 *
 * 切换插件（`pluginId` 变了）或当前选中的视图已不在声明里，一律清空内容：
 * 留着上一个插件的树，用户会以为新插件渲染出了那个界面。
 */
export function syncPluginViews(pluginId: string, views: readonly PluginViewSummary[]): void {
  const samePlugin = state.pluginId === pluginId
  const keepId =
    samePlugin && state.activeViewId !== null && views.some((v) => v.id === state.activeViewId)
      ? state.activeViewId
      : null

  if (keepId === state.activeViewId && samePlugin && sameDeclarations(views, state.views)) {
    // 声明没变（同一插件重复拉 status），保留当前渲染结果，别无谓重拉。
    return
  }

  token += 1
  setState({
    pluginId,
    views,
    activeViewId: keepId,
    status: keepId === null ? 'idle' : state.status,
    view: keepId === null ? null : state.view,
    rev: keepId === null ? null : state.rev,
    error: keepId === null ? null : state.error
  })
}

function sameDeclarations(
  a: readonly PluginViewSummary[],
  b: readonly PluginViewSummary[]
): boolean {
  if (a.length !== b.length) return false
  return a.every((item, index) => item.id === b[index]?.id && item.entry === b[index]?.entry)
}

/** 选中某个视图并拉取。重复选中同一视图只在没有内容时重试。 */
export async function selectPluginView(
  pluginId: string,
  view: PluginViewSummary
): Promise<void> {
  const already = state.activeViewId === view.id && state.status === 'ready'
  setState({
    activeViewId: view.id,
    status: already ? 'ready' : 'loading',
    error: already ? state.error : null
  })
  if (already) return
  await fetchView(pluginId, view)
}

/** 重拉当前视图（面板上的「刷新」按钮与 `plugin:view` 事件共用）。 */
export async function refreshPluginView(): Promise<void> {
  const { pluginId, activeViewId, views } = state
  if (pluginId === null || activeViewId === null) return
  const view = views.find((item) => item.id === activeViewId)
  if (view === undefined) return
  setState({ status: 'loading', error: null })
  await fetchView(pluginId, view)
}

/**
 * 处理一条 `plugin:view` 刷新通知。
 *
 * 返回 `true` 表示真的发起了重拉——单测据此验证「同一 rev 不重复拉」，
 * 面板也据此避免对无关通知闪一下 loading。
 */
export function handlePluginViewRefresh(event: PluginViewRefreshEvent): boolean {
  if (event.pluginId !== state.pluginId) return false
  if (event.viewId !== state.activeViewId) return false
  if (state.rev !== null && event.rev <= state.rev) return false
  void refreshPluginView()
  return true
}

/**
 * 常驻订阅 `plugin:view`（docs/24 §7.3 第 4 刀）。
 *
 * 与 `initPluginActivityEvents` 同一范式（由 App 挂载时调一次）：**常驻**而不是
 * 面板打开才订阅——插件在面板收起时刷新也是常态，等展开再订阅只会让用户第一眼
 * 看到旧内容。事件本身不带数据，这里只做「命中当前视图 → 重拉」一次判断，真正的
 * 取数仍在 `fetchView`（`plugin.call`，过闸门与审计）。
 *
 * 旧版外壳的 preload 没有 `onPluginView`，此时返回空退订函数：面板上的「刷新」
 * 按钮照样能手动拉，功能降级而不是整块白屏。
 */
export function initPluginViewEvents(): () => void {
  const api = typeof window === 'undefined' ? undefined : window.aiide
  const subscribe: AiIdeApi['onPluginView'] | undefined = api?.onPluginView
  if (typeof subscribe !== 'function') return () => undefined
  return subscribe((event) => {
    handlePluginViewRefresh(event)
  })
}

async function fetchView(pluginId: string, view: PluginViewSummary): Promise<void> {
  const mine = ++token

  let result: Awaited<ReturnType<typeof pluginApi.call>>
  try {
    result = await pluginApi.call(pluginId, view.entry, {})
  } catch (error) {
    if (mine !== token) return
    setState({ status: 'error', view: null, error: describeFetchError(error) })
    return
  }
  if (mine !== token) return

  if (result.truncated) {
    setState({
      status: 'error',
      view: null,
      error: `视图正文超过 64 KB 已被内核截断（${view.entry}），描述不完整，已拒绝渲染。让插件缩小一次下发的数据量。`
    })
    return
  }

  let payload: unknown
  try {
    payload = JSON.parse(result.content)
  } catch {
    setState({
      status: 'error',
      view: null,
      error: `工具 ${view.entry} 的返回不是合法 JSON，无法作为 ${'aiide.view/1'} 渲染。`
    })
    return
  }

  const parsed = parseView(payload)
  setState({
    status: 'ready',
    view: parsed,
    rev: parsed.rev,
    error: parsed.status === 'rejected' ? parsed.error : null
  })
}

function describeFetchError(error: unknown): string {
  const text = error instanceof Error ? error.message : String(error)
  // 跨 IPC 的信封已在 `plugin-api` 里剥过，这里只需要一句能让人决定下一步的话。
  return text.trim() === '' ? '拉取视图失败（内核未给原因）' : text
}
