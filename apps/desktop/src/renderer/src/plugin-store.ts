/**
 * P1 渲染侧状态：已安装插件清单、选中项详情与一次安装/卸载的结果。
 *
 * 与 `mcp-store` / `permission-store` 同一形态：单例 + `useSyncExternalStore`，
 * 动作返回布尔表示「是否真的做成了」，失败原因落到 `error`（外加 6107 的
 * `errorDetails`）。
 *
 * 两个刻意的取舍：
 *
 * 1. **清单只按动作拉**。插件没有推送通道（装/卸都由本面板发起），多一条轮询只是
 *    让内核白跑路由；进面板、点刷新、装完卸完各拉一次就够。
 * 2. **刷新不清错误**。6107 往往一次列好几条清单问题，用户得逐条对着改清单；如果
 *    刷新顺手把错误擦了，他就再也没法把那些 `path` 抄下来。错误只由「下一次动作」
 *    或用户主动「知道了」清掉。
 */

import { useSyncExternalStore } from 'react'

import type { PluginStatusResult, PluginSummary } from '@ai-ide/shared-types'

import {
  describePluginError,
  pluginApi,
  pluginErrorIssues,
  type PluginIssue
} from './plugin-api'

export interface PluginStoreState {
  /** 已安装插件（内核按安装时间排序）。 */
  plugins: PluginSummary[]
  loading: boolean
  /** 正在安装（安装按钮禁用；一次只允许一个包，避免并发覆盖同一个 id）。 */
  installing: boolean
  /** 正在卸载的插件 id（UI 只禁用那一行）。 */
  busyPlugin: string | null
  /** 安装路径输入框的原文：故意保留用户写坏的样子，方便改。 */
  installPath: string
  /** 当前选中的插件 id（详情从 `plugin.status` 拉）。 */
  selectedId: string | null
  /** 选中插件详情；`null` 表示没选或还没拉回来。 */
  detail: PluginStatusResult | null
  detailLoading: boolean
  hint: string | null
  error: string | null
  /** 6107 的 `details[]`；其他错误为空数组。 */
  errorDetails: PluginIssue[]
}

function initialState(): PluginStoreState {
  return {
    plugins: [],
    loading: false,
    installing: false,
    busyPlugin: null,
    installPath: '',
    selectedId: null,
    detail: null,
    detailLoading: false,
    hint: null,
    error: null,
    errorDetails: []
  }
}

let state: PluginStoreState = initialState()

const listeners = new Set<() => void>()

function setState(patch: Partial<PluginStoreState>): void {
  state = { ...state, ...patch }
  for (const listener of listeners) listener()
}

export function getPluginState(): PluginStoreState {
  return state
}

export function subscribePlugins(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function usePlugins(): PluginStoreState {
  return useSyncExternalStore(subscribePlugins, getPluginState, getPluginState)
}

/** 测试用：模块级单例跨用例不清就会串状态。 */
export function resetPluginState(): void {
  state = initialState()
  for (const listener of listeners) listener()
}

/**
 * 拉清单。
 *
 * 装完/卸完都会走这里，所以必须**保住选中项**：如果选中的插件还在，详情不动；
 * 已经被卸载（或换了 id）就把它连同详情一起清掉，免得面板继续显示一个不存在的插件。
 */
export async function refreshPlugins(): Promise<boolean> {
  setState({ loading: true })
  try {
    const result = await pluginApi.list()
    const stillThere =
      state.selectedId !== null && result.plugins.some((plugin) => plugin.id === state.selectedId)

    setState({
      loading: false,
      plugins: result.plugins,
      selectedId: stillThere ? state.selectedId : null,
      detail: stillThere ? state.detail : null,
      detailLoading: stillThere ? state.detailLoading : false
    })
    return true
  } catch (error) {
    // 清单拉不回来时保留上一次的清单：空白列表 + 一句报错会让人以为插件全丢了。
    setState({
      loading: false,
      error: describePluginError(error),
      errorDetails: pluginErrorIssues(error)
    })
    return false
  }
}

/** 面板首次出现时拉一次；正在拉就不再叠一次。 */
export function initPlugins(): void {
  if (state.loading) return
  void refreshPlugins()
}

export function setInstallPath(path: string): void {
  setState({ installPath: path })
}

/**
 * 按本地包目录安装。
 *
 * 路径为空时**不发请求**：`-32602` 说的是同一件事，但让用户白等一次 IPC 往返、
 * 再读一句协议码，不如就地提示。
 */
export async function installPlugin(): Promise<boolean> {
  if (state.installing) return false
  const path = state.installPath.trim()
  if (path.length === 0) {
    setState({ error: '先填插件包目录的绝对路径（含 plugin.json 的那一层）。', errorDetails: [] })
    return false
  }

  setState({ installing: true, error: null, errorDetails: [], hint: null })
  try {
    const result = await pluginApi.install(path)
    const plugin = result.plugin
    setState({
      installing: false,
      installPath: '',
      hint: result.replaced
        ? `已更新「${plugin.name}」到 v${plugin.version}（覆盖了同 id 的旧版本）。`
        : `已安装「${plugin.name}」v${plugin.version}，未启动进程（P1 只做清单与安装）。`
    })
    await refreshPlugins()
    // 装完直接把新插件选中：用户下一步大概率就是看它的能力与工具。
    // 覆盖安装时它可能本来就是选中的，先清掉选中态，否则 `selectPlugin` 会按
    // 「同一个 id 不重复拉」直接返回，详情还停在上一个版本上。
    if (state.selectedId === plugin.id) setState({ selectedId: null, detail: null })
    await selectPlugin(plugin.id)
    return true
  } catch (error) {
    setState({
      installing: false,
      error: describePluginError(error),
      errorDetails: pluginErrorIssues(error)
    })
    return false
  }
}

/**
 * 卸载：删账本 + 删安装目录。
 *
 * `removed_dir = false` 不算失败——账本已经清了，只是目录被占用（比如编辑器开着
 * 插件里的文件）。这时要如实告诉用户「条目没了、目录还在」，而不是报一个红错。
 */
export async function uninstallPlugin(pluginId: string): Promise<boolean> {
  if (state.busyPlugin !== null) return false
  setState({ busyPlugin: pluginId, error: null, errorDetails: [], hint: null })
  try {
    const result = await pluginApi.uninstall(pluginId)
    const wasSelected = state.selectedId === pluginId
    setState({
      busyPlugin: null,
      hint: result.removed_dir
        ? `已卸载「${result.plugin_id}」，安装目录已删除。`
        : `「${result.plugin_id}」已从账本移除，但安装目录没能删掉（可能被占用），可手工清理。`,
      ...(wasSelected ? { selectedId: null, detail: null, detailLoading: false } : {})
    })
    await refreshPlugins()
    return true
  } catch (error) {
    setState({
      busyPlugin: null,
      error: describePluginError(error),
      errorDetails: pluginErrorIssues(error)
    })
    return false
  }
}

/**
 * 选中一个插件并拉详情；传 `null` 收起详情。
 *
 * 详情是「工具/命令清单」的唯一来源——账本只存计数，`plugin.status` 才回读安装
 * 目录里的清单。拉不到详情不影响「插件存在」这个事实，所以失败只落 error，列表照旧。
 */
export async function selectPlugin(pluginId: string | null): Promise<void> {
  if (pluginId === state.selectedId) return
  if (pluginId === null) {
    setState({ selectedId: null, detail: null, detailLoading: false })
    return
  }

  setState({ selectedId: pluginId, detail: null, detailLoading: true })
  try {
    const detail = await pluginApi.status(pluginId)
    // 期间用户可能又点了别的行：只接受仍然指向这个 id 的响应。
    if (state.selectedId !== pluginId) return
    setState({ detail, detailLoading: false })
  } catch (error) {
    if (state.selectedId !== pluginId) return
    setState({
      detailLoading: false,
      error: describePluginError(error),
      errorDetails: pluginErrorIssues(error)
    })
  }
}

/** 收起错误块（6107 的明细一起清）。 */
export function dismissPluginError(): void {
  setState({ error: null, errorDetails: [] })
}

export function dismissPluginHint(): void {
  setState({ hint: null })
}
