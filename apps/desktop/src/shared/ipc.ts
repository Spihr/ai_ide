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
  pickWorkspace: 'dialog:pick-workspace',
  /**
   * 弹出系统保存对话框（invoke，返回绝对路径或 null）。
   *
   * M5 的 `session.export` 会往磁盘写文件，导出的落点必须由用户亲手选：渲染进程
   * 拿不到「自己拼一个路径就写」的能力，只能提交默认文件名与扩展名。
   */
  pickSavePath: 'dialog:pick-save-path',
  /** 查询密钥托管状态 */
  credentialsStatus: 'credentials:status',
  /** 撤销某个 Provider 的密钥托管 */
  credentialsForget: 'credentials:forget',
  /** 主进程 → 渲染进程：密钥托管状态变更 */
  credentialsStatusChanged: 'credentials:status-changed',
  /** 主进程 → 渲染进程：M2/M4 事件（`provider.changed` / `llm.*` / `agent.*`） */
  aiEvent: 'ai:event'
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

/**
 * 密钥托管状态（docs/02 §6.3）。
 *
 * 这里刻意**不含任何密钥内容**——渲染进程只需要知道「有没有托管」与
 * 「加密是否可用」，明文只在主进程与内核内存之间流转。
 */
export interface CredentialStatus {
  /** `safeStorage` 是否可用。不可用时密钥只留内存，绝不落明文。 */
  encryptionAvailable: boolean
  /** 托管文件绝对路径（可能尚未创建）。 */
  vaultPath: string
  /** 已托管密钥的 provider id 列表。 */
  managedProviderIds: string[]
  /** 最近一次失败原因（解密失败 / 无系统密钥链等）。 */
  lastError: string | null
}

/** 内核 → 渲染进程的转发载荷：`provider.changed`、`llm.*` 与 `agent.*`。 */
export interface AiEvent {
  /** 内核通知的方法名，如 `llm.delta`、`agent.approval_required`。 */
  method: string
  /** 通知载荷（形状与 Rust 协议一致，字段为 snake_case）。 */
  params: unknown
}

/** 渲染进程可见的 API 形状（由 preload 注入到 `window.aiide`）。 */
export interface AiIdeApi {
  getCoreStatus: () => Promise<CoreStatus>
  restartCore: () => Promise<CoreStatus>
  ping: (count?: number) => Promise<PingBenchmarkResult>
  request: (method: string, params?: unknown) => Promise<unknown>
  /** 弹出系统目录选择框；用户取消时返回 `null`。 */
  pickWorkspaceDirectory: () => Promise<string | null>
  /**
   * 弹出系统保存对话框；用户取消时返回 `null`。
   *
   * @param defaultName 默认文件名（如 `会话标题.md`）
   * @param extensions  允许的扩展名（不含点，如 `['md']`）
   */
  pickSavePath: (defaultName: string, extensions: string[]) => Promise<string | null>
  onCoreStatus: (listener: (status: CoreStatus) => void) => () => void
  onCoreLog: (listener: (chunk: CoreLogChunk) => void) => () => void
  /** 订阅工作区外部改动事件，返回取消订阅函数。 */
  onWorkspaceChanged: (listener: (event: WorkspaceChangedEvent) => void) => () => void
  /** 订阅后台索引完成事件，返回取消订阅函数。 */
  onWorkspaceIndexed: (listener: (event: WorkspaceIndexedEvent) => void) => () => void
  /** 查询密钥托管状态（不含任何密钥内容）。 */
  getCredentialStatus: () => Promise<CredentialStatus>
  /** 撤销某个 Provider 的密钥托管，返回更新后的状态。 */
  forgetCredential: (providerId: string) => Promise<CredentialStatus>
  /** 订阅 M2/M4 事件（`provider.changed` / `llm.*` / `agent.*`），返回取消订阅函数。 */
  onAiEvent: (listener: (event: AiEvent) => void) => () => void
  /** 订阅密钥托管状态变更，返回取消订阅函数。 */
  onCredentialStatus: (listener: (status: CredentialStatus) => void) => () => void
}
