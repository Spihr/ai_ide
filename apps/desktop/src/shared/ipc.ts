/**
 * 主进程 ↔ 渲染进程的 IPC 契约。
 *
 * 这是**唯一**允许被两侧共同引用的类型来源。Rust 协议类型不直接暴露给
 * 渲染进程，渲染进程能拿到的形状一律在这里描述。
 */

import type {
  HelloResult,
  LspDiagnosticsEvent,
  PluginLogEvent,
  PluginProgressEvent,
  PluginStateChangedEvent,
  TerminalDataEvent,
  TerminalExitEvent,
  WorkspaceChangedEvent,
  WorkspaceIndexedEvent
} from '@ai-ide/shared-types'

export type {
  LspDiagnosticsEvent,
  PluginLogEvent,
  PluginProgressEvent,
  PluginStateChangedEvent,
  TerminalDataEvent,
  TerminalExitEvent,
  WorkspaceChangedEvent,
  WorkspaceIndexedEvent
}

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
  aiEvent: 'ai:event',
  /**
   * 主进程 → 渲染进程：终端输出增量（`terminal.data`）。
   *
   * 单独开一条通道而不是塞进 `ai:event`：终端输出是**高频字节流**（一次命令
   * 刷出几十条很常见），混进 AI 事件会让对话面板的监听函数被无谓唤醒上千次。
   */
  terminalData: 'terminal:data',
  /** 主进程 → 渲染进程：终端子进程退出（`terminal.exit`） */
  terminalExit: 'terminal:exit',
  /**
   * 主进程 → 渲染进程：插件日志（`plugin.log`）。
   *
   * 三条插件可观测事件各开一条通道，与 `ai:event` 分开：`plugin.log` /
   * `plugin.progress` 是插件自己刷的遥测（`host/log` 无条件转发、进度可以
   * 每秒好几帧），混进 AI 事件会让对话面板的监听无谓唤醒；而它们的订阅方
   * 只有一个「插件活动」视图。
   */
  pluginLog: 'plugin:log',
  /** 主进程 → 渲染进程：插件长任务进度（`plugin.progress`） */
  pluginProgress: 'plugin:progress',
  /**
   * 主进程 → 渲染进程：插件状态迁移（`plugin.state_changed`）。
   *
   * 这条同时承担「刷新插件清单」的职责：状态变了就意味着面板上的行过期了。
   */
  pluginStateChanged: 'plugin:state-changed',
  /**
   * 主进程 → 渲染进程：语言服务的诊断批次（`lsp.diagnostics`）。
   *
   * 单独开一条通道的理由与终端 / 插件事件一致：诊断是**服务器主动推**的，一次
   * 编辑就可能来一批（含「问题消失了」的空批次），塞进 `ai:event` 会让对话面板
   * 跟着无谓唤醒。订阅方只有渲染侧的 `lsp-store`。
   */
  lspDiagnostics: 'lsp:diagnostics'
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
  /**
   * 订阅终端输出增量（`terminal.data`），返回取消订阅函数。
   *
   * 载荷里的 `data` 是原始字节的 base64，渲染侧解码后再交给 xterm。
   */
  onTerminalData: (listener: (event: TerminalDataEvent) => void) => () => void
  /** 订阅终端子进程退出（`terminal.exit`），返回取消订阅函数。 */
  onTerminalExit: (listener: (event: TerminalExitEvent) => void) => () => void
  /** 订阅插件日志（`plugin.log`），返回取消订阅函数。 */
  onPluginLog: (listener: (event: PluginLogEvent) => void) => () => void
  /**
   * 订阅插件进度（`plugin.progress`），返回取消订阅函数。
   *
   * 同一个 `pluginId + token` 会来很多帧，归并由渲染侧的插件活动 store 负责
   * （preload 只转交，不改写载荷）。
   */
  onPluginProgress: (listener: (event: PluginProgressEvent) => void) => () => void
  /** 订阅插件状态迁移（`plugin.state_changed`），返回取消订阅函数。 */
  onPluginStateChanged: (listener: (event: PluginStateChangedEvent) => void) => () => void
  /**
   * 订阅语言服务诊断（`lsp.diagnostics`），返回取消订阅函数。
   *
   * 载荷里的 `range` 是 **1-based** 行列、`severity` 是枚举字符串，转换与打点都在
   * 渲染侧 `lsp-store` / `lsp-markers` 完成——preload 只转交，不改写载荷。
   */
  onLspDiagnostics: (listener: (event: LspDiagnosticsEvent) => void) => () => void
}

// ---------------------------------------------------------------------------
// 内核错误的结构化载荷（跨 IPC 的「错误信封」）
// ---------------------------------------------------------------------------

/**
 * 信封分隔串。
 *
 * IPC 抛错只序列化 `Error.message`，内核 `ErrorObject.data` 会在这一跳被丢掉——
 * 而 `plugin.install` 的 `6107` 恰恰**只有** `data.details[]` 有价值（一次列全
 * 所有清单问题，P1 的验收点）。所以主进程把整份载荷 JSON 追加在 message 尾部，
 * 渲染侧按这个分隔串切回去；前面的人类可读文案一字不动，日志/旧代码照常可读。
 *
 * 用不可见控制字符包夹，避免出现「插件清单里的字符串恰好包含分隔串」被误切。
 */
export const RPC_ERROR_PAYLOAD_MARKER = '\u0000[aiide-rpc-error]\u0000'

/** 内核错误的结构化载荷。 */
export interface RpcErrorPayload {
  /** 协议错误码，如 `6107`。 */
  code: number
  /** 内核原始文案（不含信封）。 */
  message: string
  /** 内核 `ErrorObject.data` 原样；未带 data 时为 `undefined`。 */
  data?: unknown
}

/** 主进程侧：把内核错误打包成可跨 IPC 的 message。 */
export function encodeRpcErrorPayload(payload: RpcErrorPayload): string {
  return `${payload.message}${RPC_ERROR_PAYLOAD_MARKER}${JSON.stringify({
    code: payload.code,
    data: payload.data
  })}`
}

/** 渲染侧：从 message 里还原载荷；不是信封（或无载荷）时返回 `null`。 */
export function decodeRpcErrorPayload(message: string): RpcErrorPayload | null {
  const at = message.lastIndexOf(RPC_ERROR_PAYLOAD_MARKER)
  if (at < 0) return null

  const rest = message.slice(at + RPC_ERROR_PAYLOAD_MARKER.length)
  let parsed: unknown
  try {
    parsed = JSON.parse(rest)
  } catch {
    // 信封坏了就当作没有：宁可少一段明细，也不要让面板显示一坨原始 JSON。
    return null
  }
  if (parsed === null || typeof parsed !== 'object') return null

  const record = parsed as Record<string, unknown>
  const code = record['code']
  if (typeof code !== 'number' || !Number.isFinite(code)) return null

  return {
    code,
    message: message.slice(0, at),
    ...(record['data'] === undefined ? {} : { data: record['data'] })
  }
}

/** 渲染侧：剥掉信封，只留人类可读的那一段。 */
export function stripRpcErrorPayload(message: string): string {
  const at = message.lastIndexOf(RPC_ERROR_PAYLOAD_MARKER)
  return at < 0 ? message : message.slice(0, at)
}
