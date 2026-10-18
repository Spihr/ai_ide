/**
 * 主进程 ↔ 渲染进程的 IPC 契约。
 *
 * 这是**唯一**允许被两侧共同引用的类型来源。Rust 协议类型不直接暴露给
 * 渲染进程，渲染进程能拿到的形状一律在这里描述。
 */

import type {
  HelloResult,
  WorkspaceChangedEvent,
  WorkspaceIndexedEvent
} from '@ai-ide/shared-types'

export type { WorkspaceChangedEvent, WorkspaceIndexedEvent }

/** IPC 通道名。集中定义，避免两侧字符串写歪。 */
export const IpcChannels = {
  /** 拉取内核状态 */
  coreStatus: 'core:status',
  /** 重启内核 */
  coreRestart: 'core:restart',
  /** 经策略闸门转发的方法调用 */
  coreRequest: 'core:request',
  /** 往返延迟基准测试 */
  corePingBenchmark: 'core:ping-benchmark',
  /** 主进程 → 渲染进程：状态变更推送 */
  coreStatusChanged: 'core:status-changed',
  /** 主进程 → 渲染进程：内核日志 */
  coreLog: 'core:log',
  /** 主进程 → 渲染进程：工作区内文件被外部程序改动 */
  workspaceChanged: 'workspace:changed',
  /** 主进程 → 渲染进程：后台全量索引完成 */
  workspaceIndexed: 'workspace:indexed',
  /** 弹出系统目录选择框（invoke，返回绝对路径或 null） */
  pickWorkspace: 'dialog:pick-workspace'
} as const

export type CoreState = 'idle' | 'starting' | 'ready' | 'failed' | 'stopped'

/** 内核运行状态快照。 */
export interface CoreStatus {
  state: CoreState
  /** 内核可执行文件路径 */
  binaryPath: string
  /** 内核进程 pid */
  pid: number | null
  /** 握手结果 */
  hello: HelloResult | null
  /** 失败原因 */
  error: string | null
  /** 退出码 */
  exitCode: number | null
  /** 退出信号 */
  exitSignal: string | null
}

/** ping 往返延迟统计（毫秒）。 */
export interface PingBenchmarkResult {
  count: number
  minMs: number
  maxMs: number
  meanMs: number
  p50Ms: number
  p95Ms: number
  samplesMs: number[]
}

/** 内核 stderr 日志片段。 */
export interface CoreLogChunk {
  stream: 'stderr'
  text: string
}

/** 渲染进程可见的 API 形状（由 preload 注入到 `window.aiide`）。 */
export interface AiIdeApi {
  getCoreStatus: () => Promise<CoreStatus>
  restartCore: () => Promise<CoreStatus>
  ping: (count?: number) => Promise<PingBenchmarkResult>
  request: (method: string, params?: unknown) => Promise<unknown>
  /** 弹出系统目录选择框；用户取消时返回 `null`。 */
  pickWorkspaceDirectory: () => Promise<string | null>
  onCoreStatus: (listener: (status: CoreStatus) => void) => () => void
  onCoreLog: (listener: (chunk: CoreLogChunk) => void) => () => void
  /** 订阅工作区外部改动事件，返回取消订阅函数。 */
  onWorkspaceChanged: (listener: (event: WorkspaceChangedEvent) => void) => () => void
  /** 订阅后台索引完成事件，返回取消订阅函数。 */
  onWorkspaceIndexed: (listener: (event: WorkspaceIndexedEvent) => void) => () => void
}
