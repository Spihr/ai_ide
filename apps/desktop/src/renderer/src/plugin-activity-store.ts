/**
 * P5 切片七：插件活动（日志 / 进度 / 状态迁移）的渲染侧状态。
 *
 * 数据来源是内核 → 外壳的三条推送通道（`plugin.log` / `plugin.progress` /
 * `plugin.state_changed`，见 `crates/plugin/src/events.rs`）。三条都是**遥测**：
 * 没有请求、没有回执、可能一秒来好几帧，因此这里的每一条规则都是为了「别让
 * 订阅方被遥测拖垮」而定的：
 *
 * 1. **同插件同 token 的进度合并成一行**。`host/progress` 的语义就是「同一次长
 *    任务的多次上报」，不合并的话一次扫描能刷出几百行，用户只会看到一片百分比。
 * 2. **上限 500 条，按新旧丢尾部**。活动视图是「最近发生了什么」的窗口，不是
 *    账本；真要看历史应该去审计库（`audit.list`）。丢弃条数单独计数，面板可以
 *    说清「更早的 N 条已省略」，而不是默默少一截。
 * 3. **`state_changed` 顺带刷新插件清单**。状态变了意味着面板上的行已经过期
 *    （可能刚崩、可能刚就绪），只追加一行日志却让列表停在旧状态，用户会看到
 *    「活动里写着 crashed，列表里还写着已就绪」这种自相矛盾的两处。
 * 4. **级别筛选只作用于日志**。进度与状态不是日志，被门槛挡住会让「有没有在
 *    跑」「有没有崩」这两件事一起消失——那正好是用户看这个面板的两个理由。
 *
 * 进度的 `at` 每次合并都刷新（它代表「最后一次上报时间」），排序按自增 `seq`
 * 而不是时间戳：同一毫秒内的多条事件必须仍然有序，否则列表会自己抖。
 */

import { useSyncExternalStore } from 'react'

import type {
  PluginLogEvent,
  PluginLogLevel,
  PluginProgressEvent,
  PluginState,
  PluginStateChangedEvent
} from '@ai-ide/shared-types'

import { notePluginBatchProgress, refreshPlugins } from './plugin-store'

/** 活动视图的条数上限。 */
export const MAX_PLUGIN_ACTIVITY = 500

/** 日志级别筛选的取值：`all` 表示不过滤。 */
export type PluginActivityLevel = PluginLogLevel | 'all'

/** 级别严重度，用于「只显示 warn 及以上」这种门槛比较。 */
export const PLUGIN_LEVEL_RANK: Record<PluginLogLevel, number> = {
  debug: 0,
  info: 1,
  warn: 2,
  error: 3
}

export type PluginActivityKind = 'log' | 'progress' | 'state'

/** 活动视图里的一行。 */
export interface PluginActivityEntry {
  /**
   * 稳定行键。
   *
   * 进度用 `progress:<pluginId>:<token>`——同一次长任务的所有上报落到同一个键上，
   * 这就是「合并而非新增」的实现方式；日志与状态用自增序号，永不碰撞。
   */
  key: string
  kind: PluginActivityKind
  pluginId: string
  /** 单调递增序号，越大越新；列表按它倒序。 */
  seq: number
  /** 最后一次发生时间（毫秒，进度为最后一次上报时间）。 */
  at: number
  /** 日志级别；进度与状态为 `null`。 */
  level: PluginLogLevel | null
  /** 展示文案。 */
  message: string
  /** 进度百分比（0~100）；非进度条目为 `null`。 */
  percent: number | null
  /** 状态迁移的目标状态；非状态条目为 `null`。 */
  state: PluginState | null
}

export interface PluginActivityState {
  /** 按时间倒序（最新的在最前）。 */
  entries: PluginActivityEntry[]
  /** 面板是否展开。 */
  open: boolean
  /** 日志级别门槛（进度与状态不受影响）。 */
  level: PluginActivityLevel
  /** 因上限被裁掉的条数，用于「更早的 N 条已省略」。 */
  dropped: number
}

function initialState(): PluginActivityState {
  return { entries: [], open: false, level: 'all', dropped: 0 }
}

let state: PluginActivityState = initialState()

/** 自增序号：跨重置也不回收，避免旧列表里的键与新条目撞上。 */
let seq = 0

const listeners = new Set<() => void>()

function emit(): void {
  for (const listener of listeners) listener()
}

function setState(patch: Partial<PluginActivityState>): void {
  state = { ...state, ...patch }
  emit()
}

export function getPluginActivityState(): PluginActivityState {
  return state
}

export function subscribePluginActivity(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function usePluginActivity(): PluginActivityState {
  return useSyncExternalStore(
    subscribePluginActivity,
    getPluginActivityState,
    getPluginActivityState
  )
}

/** 测试用：模块级单例跨用例不清就会串状态。 */
export function resetPluginActivity(): void {
  state = initialState()
  emit()
}

/**
 * 把一条新活动并入列表（纯函数，便于单测）。
 *
 * 规则：先按 `key` 摘掉同键旧条目（这就是进度的「更新而非新增」），再插到队首，
 * 最后按上限裁尾。返回被裁掉的条数，调用方负责累加到 `dropped`。
 */
export function mergePluginActivity(
  entries: readonly PluginActivityEntry[],
  entry: PluginActivityEntry,
  limit: number = MAX_PLUGIN_ACTIVITY
): { entries: PluginActivityEntry[]; dropped: number } {
  const rest = entries.filter((item) => item.key !== entry.key)
  const merged = [entry, ...rest]
  if (limit <= 0) return { entries: [], dropped: merged.length }
  return {
    entries: merged.slice(0, limit),
    dropped: Math.max(0, merged.length - limit)
  }
}

function push(entry: PluginActivityEntry): void {
  const result = mergePluginActivity(state.entries, entry)
  setState({ entries: result.entries, dropped: state.dropped + result.dropped })
}

/** 级别归一：内核只做原样透传，未知级别按 `info`（与内核的默认值同口径）。 */
export function normalizeLevel(level: string | null | undefined): PluginLogLevel {
  return level === 'debug' || level === 'warn' || level === 'error' ? level : 'info'
}

/** 百分比归一到 0~100；非有限值为 `null`（内核已夹过一次，这里防手写载荷）。 */
function normalizePercent(percent: number | null | undefined): number | null {
  if (typeof percent !== 'number' || !Number.isFinite(percent)) return null
  return Math.min(100, Math.max(0, percent))
}

/** 收到一条 `plugin.log`。 */
export function receivePluginLog(event: PluginLogEvent): void {
  seq += 1
  push({
    key: `log:${seq}`,
    kind: 'log',
    pluginId: event.pluginId,
    seq,
    at: Date.now(),
    level: normalizeLevel(event.level),
    message: event.message ?? '',
    percent: null,
    state: null
  })
}

/**
 * 收到一条 `plugin.progress`。
 *
 * 同插件同 token 覆盖同一行；`token` 缺失时退回「唯一次」键，宁可多一行也
 * 不能把两次无关的长任务合并成一条（百分比会来回跳，读起来像程序错乱）。
 */
export function receivePluginProgress(event: PluginProgressEvent): void {
  seq += 1
  const token = typeof event.token === 'string' && event.token.length > 0 ? event.token : `~${seq}`
  push({
    key: `progress:${event.pluginId}:${token}`,
    kind: 'progress',
    pluginId: event.pluginId,
    seq,
    at: Date.now(),
    level: null,
    message: event.message ?? '',
    percent: normalizePercent(event.percent),
    state: null
  })
}

/**
 * 收到一条 `plugin.state_changed`。
 *
 * 除了追加一行，还要拉一次插件清单：这条事件的全部意义就是「面板上的行已经
 * 过期了」。刷新失败不影响事件条目——活动视图照旧把这次迁移记下来。
 */
export function receivePluginStateChanged(event: PluginStateChangedEvent): void {
  seq += 1
  push({
    key: `state:${seq}`,
    kind: 'state',
    pluginId: event.pluginId,
    seq,
    at: Date.now(),
    level: null,
    message: `状态变为 ${event.state}`,
    percent: null,
    state: event.state
  })
  // 「全部启用」的进度就挂在这条事件上：协议没有 `onProgress` 回调，而内核每落一次
  // 账本就广播一次状态，这是唯一一条**实时**能看到「5 个里已起来 3 个」的通道。
  notePluginBatchProgress(event.pluginId, event.state)
  void refreshPlugins()
}

/** 面板开关。 */
export function setPluginActivityOpen(open: boolean): void {
  setState({ open })
}

export function togglePluginActivity(): void {
  setState({ open: !state.open })
}

/** 日志级别门槛。 */
export function setPluginActivityLevel(level: PluginActivityLevel): void {
  setState({ level })
}

/** 清空列表（用户主动「清空」；`dropped` 一并归零）。 */
export function clearPluginActivity(): void {
  setState({ entries: [], dropped: 0 })
}

/**
 * 按级别门槛过滤后的可见条目。
 *
 * 进度与状态条目始终可见：它们是「在跑什么 / 有没有崩」的唯一线索。
 */
export function visiblePluginActivity(
  activity: PluginActivityState = state
): PluginActivityEntry[] {
  if (activity.level === 'all') return activity.entries
  const threshold = PLUGIN_LEVEL_RANK[activity.level]
  return activity.entries.filter(
    (entry) => entry.level === null || PLUGIN_LEVEL_RANK[entry.level] >= threshold
  )
}

/** 仍在进行的进度行数（面板标题上显示「N 项进行中」）。 */
export function activeProgressCount(activity: PluginActivityState = state): number {
  return activity.entries.filter((entry) => entry.kind === 'progress').length
}

/**
 * 常驻订阅三条插件通道。
 *
 * 返回退订函数；与 `initTerminalEvents` 同一范式（由 App 挂载时调一次）。
 * 订阅失败（例如测试环境没有 `window.aiide`）不该炸掉整个应用：活动视图是
 * 附属信息，缺了它插件面板照样能用。
 */
export function initPluginActivityEvents(): () => void {
  const api = typeof window === 'undefined' ? undefined : window.aiide
  if (!api) return () => undefined

  const offLog = api.onPluginLog((event) => receivePluginLog(event))
  const offProgress = api.onPluginProgress((event) => receivePluginProgress(event))
  const offState = api.onPluginStateChanged((event) => receivePluginStateChanged(event))

  return () => {
    offLog()
    offProgress()
    offState()
  }
}
