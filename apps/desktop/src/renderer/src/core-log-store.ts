/**
 * P5 切片八：内核日志的渲染侧状态（`core.log` 订阅）。
 *
 * 数据来源是 `core:log` 通道上的 [`CoreLogChunk`]：外壳把**内核 stderr** 与
 * **内核事件通知**都从这条通道推下来，磁盘上的副本由主进程写（见
 * `main/core-log-file.ts`），这里只管「用户眼前能看到最近发生了什么」。
 *
 * 三条规则与插件活动视图同源，理由也一样：
 *
 * 1. **按行切分**。主进程推的是任意粒度的文本块（stderr 一次 `read` 可能是半行，
 *    也可能一口气十行），不切分的话列表里会出现「一条超长条目」，既不能滚动
 *    也不能定位。末尾没有换行的那半行先留在缓冲区，等下一块拼完再入列表。
 * 2. **上限 500 行，丢最旧的**。内核日志是排查线索，不是账本；真要看全量去
 *    磁盘上的 `core-shell-*.log`（或内核自己的 `core-*.jsonl`）。
 * 3. **不改写内容**。日志就该是原样的字节，任何「友好化」都会让用户按日志
 *    搜代码时搜不到。唯一的加工是去掉行尾 `\r`（Windows 换行）。
 */

import { useSyncExternalStore } from 'react'

import type { CoreLogChunk } from '../../shared/ipc'

/** 渲染侧保留的最大行数。 */
export const MAX_CORE_LOG_LINES = 500

/** 一行的来源：内核 stderr，或内核主动推的事件通知。 */
export type CoreLogStream = CoreLogChunk['stream']

export interface CoreLogLine {
  /** 自增序号，越大越新（同一毫秒内仍然有序）。 */
  seq: number
  /** 到达时间（毫秒）。 */
  at: number
  stream: CoreLogStream
  text: string
}

export interface CoreLogState {
  /** 按到达顺序（最新在最前）。 */
  lines: CoreLogLine[]
  /** 因上限被裁掉的行数（面板可以说清「更早的 N 行已省略」）。 */
  dropped: number
}

function initialState(): CoreLogState {
  return { lines: [], dropped: 0 }
}

let state: CoreLogState = initialState()
let seq = 0
/** 上一块末尾没换行的残行；与 `coreLogStream` 绑定，切换来源就丢弃。 */
let pending = ''
let pendingStream: CoreLogStream = 'stderr'

const listeners = new Set<() => void>()

function emit(): void {
  for (const listener of listeners) listener()
}

export function getCoreLogState(): CoreLogState {
  return state
}

export function subscribeCoreLog(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function useCoreLog(): CoreLogState {
  return useSyncExternalStore(subscribeCoreLog, getCoreLogState, getCoreLogState)
}

/** 测试用：模块级单例跨用例不清就会串状态。 */
export function resetCoreLog(): void {
  state = initialState()
  seq = 0
  pending = ''
  pendingStream = 'stderr'
  emit()
}

/**
 * 把一段文本切成的行并入列表（纯函数，便于单测）。
 *
 * 从尾部裁到 `limit` 行，返回被裁掉的条数；`limit <= 0` 视为不保留任何行。
 */
export function mergeCoreLogLines(
  lines: readonly CoreLogLine[],
  added: readonly CoreLogLine[],
  limit: number = MAX_CORE_LOG_LINES
): { lines: CoreLogLine[]; dropped: number } {
  const merged = [...added].reverse().concat(lines)
  if (limit <= 0) return { lines: [], dropped: merged.length }
  return { lines: merged.slice(0, limit), dropped: Math.max(0, merged.length - limit) }
}

/** 收到一块日志文本：按行切分，残行留到下一块。 */
export function receiveCoreLog(chunk: CoreLogChunk): void {
  if (typeof chunk?.text !== 'string' || chunk.text.length === 0) return
  const stream: CoreLogStream = chunk.stream === 'event' ? 'event' : 'stderr'
  // 换了来源还没收尾的残行就当做完了：它是上一块的尾巴，不属于这一块。
  if (stream !== pendingStream) {
    pending = ''
    pendingStream = stream
  }

  const buffer = pending + chunk.text
  const parts = buffer.split('\n')
  pending = parts.pop() ?? ''

  const at = Date.now()
  const added: CoreLogLine[] = []
  for (const raw of parts) {
    const text = raw.endsWith('\r') ? raw.slice(0, -1) : raw
    // 纯空行直接丢：一堆空行只会把真正的内容挤出窗口。
    if (text.trim().length === 0) continue
    seq += 1
    added.push({ seq, at, stream, text })
  }
  if (added.length === 0) return

  const merged = mergeCoreLogLines(state.lines, added)
  state = { lines: merged.lines, dropped: state.dropped + merged.dropped }
  emit()
}

/** 清空列表（用户主动「清空」；`dropped` 一并归零）。 */
export function clearCoreLog(): void {
  state = initialState()
  emit()
}

/**
 * 常驻订阅 `core:log`。
 *
 * 与 `initPluginActivityEvents` 同一范式（App 挂载时调一次）：内核日志必须
 * 从窗口一打开就开始收，否则「内核启动失败」的那几行恰好落在订阅之前，
 * 用户看到的是一片空白加一句 `failed`。测试环境没有 `window.aiide` 时返回
 * 空退订函数，不能让整个应用因为拿不到日志而起不来。
 */
export function initCoreLogEvents(): () => void {
  const api = typeof window === 'undefined' ? undefined : window.aiide
  if (!api) return () => undefined
  return api.onCoreLog((chunk) => receiveCoreLog(chunk))
}
