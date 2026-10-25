/**
 * 语言服务状态 + 诊断标记的单一数据源。
 *
 * 分工与 `git-store` / `mcp-store` 一致：这里是**唯一**持有 `server_id` 的地方，
 * 组件（状态栏、编辑器）只读快照，不碰 IPC。几条不显然的取舍写在下面。
 *
 * **1. 降级不是错误。** `lsp.start` 拿回 `10001`/`10002` 时状态进 `unavailable`，
 * 只留一行说明给状态栏的 tooltip：用户没装 `typescript-language-server` 是常态，
 * 不是故障，弹窗会让人以为 IDE 坏了。真正认不出的错误才进 `error`。
 *
 * **2. 一个语言一条连接，按语言复用。** 内核按 `(language_id, root)` 复用连接，
 * 外壳这边的 `server_id` 也按 `languageId` 存：在 10 个 `.ts` 文件之间跳来跳去，
 * `lsp.start` 每次都调，但拿回同一个 `server_id`，`didOpen` 只在第一次发。
 *
 * **3. 诊断是整篇替换。** 每次 `lsp.diagnostics` 都是某文件的完整清单，空数组
 * 表示「问题全消失了」——必须照常 `setModelMarkers([])`，否则红点永久残留。
 *
 * **4. 切文件先清标记。** 从 `a.ts` 切到 `b.ts` 时，`a.ts` 的标记留在它自己的
 * model 上不碍事（Monaco 按 model 存标记）；但若切换到**没开语言服务**的文件，
 * 旧语言的连接不关（用户可能切回去），标记也不必动。真正要清的是同一 model 上
 * 因为服务器 stop 而失效的标记，见 `stop`。
 */

import { useSyncExternalStore } from 'react'

import type {
  LspDiagnostic,
  LspDiagnosticSeverity,
  LspDiagnosticsEvent,
  LspServerInfo,
  LspServerStatus
} from '@ai-ide/shared-types'

import {
  LSP_ERROR_SERVER_UNKNOWN,
  countProblems,
  describeLspError,
  diagnosticsToMarkers,
  isDegradeCode,
  lspApi,
  lspErrorCodeOf,
  type LspMarkerData
} from './lsp-api'

/**
 * 面向状态栏的状态。
 *
 * - `idle`：还没打开文件 / 还没试过启动。
 * - `starting`：`lsp.start` 在飞。
 * - `ready`：有 `server_id`，可以 sync / 查询。
 * - `unavailable`：本机没有这个语言的服务器（**降级**，灰点 + 说明，不弹框）。
 * - `stopped`：主动 stop 过（用户关掉了，或工作区关了）。
 * - `error`：意外失败（进程崩了、协议错），带原文。
 */
export type LspStatus = 'idle' | 'starting' | 'ready' | 'unavailable' | 'stopped' | 'error'

/** 一条连接的本地视角。 */
export interface LspConnection {
  serverId: string
  languageId: string
  /** 内核报的实际命令（可能是备选实现，例如 `pylsp` 替 `pyright-langserver`）。 */
  command: string
  status: LspServerStatus
  pid?: number
  /** 本会话自动重启次数；`> 0` 时状态栏可以给个「重连 N 次」的提示。 */
  restarts: number
  lastError?: string
}

/** 一个文件的诊断摘要（状态栏角标用）。 */
export interface LspFileDiagnostics {
  path: string
  /** error + warning 的条数。 */
  problems: number
  /** 全集条数（含 info / hint）。 */
  total: number
  /** 最高严重度，用来决定角标颜色。 */
  worst: LspDiagnosticSeverity
}

export interface LspState {
  status: LspStatus
  /** 当前文件的绝对路径；没有打开文件时为 `null`。 */
  path: string | null
  /** 当前文件对应的语言 id（状态栏要显示的那个名字）。 */
  languageId: string | null
  /** 按语言缓存的连接；key 是 `languageId`。 */
  connections: Record<string, LspConnection>
  /** 当前文件用的连接句柄。 */
  serverId: string | null
  /** 降级 / 出错时给状态栏 tooltip 的一行说明；正常时 `null`。 */
  notice: string | null
  /** 按文件路径索引的诊断摘要。 */
  diagnostics: Record<string, LspFileDiagnostics>
}

const EMPTY_STATE: LspState = {
  status: 'idle',
  path: null,
  languageId: null,
  connections: {},
  serverId: null,
  notice: null,
  diagnostics: {}
}

type Listener = () => void

/** 语言 id → 状态栏展示名。认不出的语言原样显示（`zig` 就叫 `zig`）。 */
const LANGUAGE_LABELS: Record<string, string> = {
  typescript: 'TypeScript',
  typescriptreact: 'TypeScript React',
  javascript: 'JavaScript',
  javascriptreact: 'JavaScript React',
  python: 'Python',
  rust: 'Rust',
  go: 'Go',
  java: 'Java',
  c: 'C',
  cpp: 'C++',
  csharp: 'C#',
  json: 'JSON',
  yaml: 'YAML',
  markdown: 'Markdown',
  html: 'HTML',
  css: 'CSS',
  scss: 'SCSS',
  vue: 'Vue',
  svelte: 'Svelte',
  php: 'PHP',
  ruby: 'Ruby'
}

export function languageLabel(languageId: string | null): string {
  if (languageId === null) return ''
  return LANGUAGE_LABELS[languageId] ?? languageId
}

/**
 * 状态栏那一行文案。
 *
 * 文案里**总是带语言名**：同时开着 `.ts` 和 `.py` 时，只写「语言服务器已就绪」
 * 没法判断当前文件到底有没有服务。
 */
export function statusLabel(state: LspState): string {
  const language = languageLabel(state.languageId)
  switch (state.status) {
    case 'ready':
      return language.length > 0 ? `${language} 语言服务已就绪` : '语言服务已就绪'
    case 'starting':
      return '语言服务启动中…'
    case 'unavailable':
      return language.length > 0 ? `${language} 语言服务器不可用` : '语言服务器不可用'
    case 'stopped':
      return language.length > 0 ? `${language} 语言服务已停止` : '语言服务已停止'
    case 'error':
      return '语言服务出错'
    default:
      return ''
  }
}

/** 当前文件的诊断摘要；没有诊断时返回 `null`（状态栏据此隐藏角标）。 */
export function currentFileDiagnostics(state: LspState): LspFileDiagnostics | null {
  if (state.path === null) return null
  return state.diagnostics[state.path] ?? null
}

function severityRank(severity: LspDiagnosticSeverity): number {
  switch (severity) {
    case 'error':
      return 3
    case 'warning':
      return 2
    case 'information':
      return 1
    default:
      return 0
  }
}

function worstSeverity(diagnostics: readonly LspDiagnostic[]): LspDiagnosticSeverity {
  let worst: LspDiagnosticSeverity = 'hint'
  let rank = -1
  for (const diagnostic of diagnostics) {
    const current = severityRank(diagnostic.severity)
    if (current > rank) {
      rank = current
      worst = diagnostic.severity
    }
  }
  return worst
}

/** 内核 `LspServerInfo` → 本地连接（剥掉用不上的字段，避免快照无谓变动）。 */
function toConnection(info: LspServerInfo): LspConnection {
  const connection: LspConnection = {
    serverId: info.server_id,
    languageId: info.language_id,
    command: info.command,
    status: info.status,
    restarts: info.restarts
  }
  if (info.pid !== undefined) connection.pid = info.pid
  if (info.last_error !== undefined) connection.lastError = info.last_error
  return connection
}

/**
 * 连接是否还活着。
 *
 * `crashed` / `stopped` 的句柄不能再 sync——内核会回 `10003`。判在这里，
 * 免得每处调用点各写一遍。
 */
export function isUsable(connection: LspConnection | null): connection is LspConnection {
  if (connection === null) return false
  return connection.status === 'ready' || connection.status === 'starting'
}

let state: LspState = EMPTY_STATE
const listeners = new Set<Listener>()

/** 同一时刻只允许一条 start 在飞；切文件太快时后来的请求直接复用前一条的结果。 */
let pendingStart: Promise<void> | null = null
/** 已卸载的诊断订阅（首次 subscribe 时挂上，`dispose` 时摘掉）。 */
let unsubscribeDiagnostics: (() => void) | null = null
/** 标记写入器：由 `lsp-markers` 注入，store 自己不 import monaco。 */
let markerWriter: ((path: string, markers: LspMarkerData[]) => void) | null = null

function emit(): void {
  for (const listener of listeners) listener()
}

function setState(patch: Partial<LspState>): void {
  state = { ...state, ...patch }
  emit()
}

function setConnection(languageId: string, connection: LspConnection): void {
  setState({ connections: { ...state.connections, [languageId]: connection } })
}

function dropConnection(languageId: string | null): void {
  if (languageId === null) return
  const { [languageId]: _removed, ...rest } = state.connections
  setState({ connections: rest })
}

/**
 * 接上诊断通知。
 *
 * 只在拿不到 `window.aiide` 的环境（单测里注入的假 API 没有这个方法）下跳过，
 * 不抛错——诊断是增强，不是启动前提。
 */
function ensureDiagnosticsSubscription(): void {
  if (unsubscribeDiagnostics !== null) return
  const api = window.aiide
  if (typeof api.onLspDiagnostics !== 'function') return
  unsubscribeDiagnostics = api.onLspDiagnostics(handleDiagnostics)
}

/**
 * 处理一条 `lsp.diagnostics`。
 *
 * 即使文件已经不在前台也照常打点：用户可能开着「分屏看两个文件」，或者切走再
 * 切回来时希望标记还在。清标记只在 `stop` / 卸载时做。
 */
export function handleDiagnostics(event: LspDiagnosticsEvent): void {
  const markers = diagnosticsToMarkers(event.diagnostics)
  markerWriter?.(event.path, markers)

  const summary: LspFileDiagnostics = {
    path: event.path,
    problems: countProblems(event.diagnostics),
    total: event.diagnostics.length,
    worst: worstSeverity(event.diagnostics)
  }
  setState({ diagnostics: { ...state.diagnostics, [event.path]: summary } })
}

/**
 * 打开一个文件：切到它的语言、必要时起服务、把当前缓冲同步上去。
 *
 * `content` 必须传编辑器里的**当前文本**（未保存的也算）：只让内核读磁盘的话，
 * 改了一半的代码在服务器眼里还是旧版本，跳转会跳到已经删掉的行上。
 *
 * 返回 `true` 表示这条文件已经有可用连接（含「复用已有连接」），`false` 表示
 * 降级或失败——调用方据此决定要不要注册 provider（本片不做）。
 */
export async function openFile(input: {
  path: string
  languageId: string
  content: string
  root?: string
}): Promise<boolean> {
  ensureDiagnosticsSubscription()

  const switched = state.path !== input.path || state.languageId !== input.languageId
  if (switched) {
    setState({ path: input.path, languageId: input.languageId })
  }

  const existing = state.connections[input.languageId] ?? null
  if (isUsable(existing)) {
    // 复用连接：只要同步这一份。`serverId` 指回当前语言，状态栏跟着切。
    if (state.serverId !== existing.serverId || state.status !== 'ready') {
      setState({ serverId: existing.serverId, status: 'ready', notice: null })
    }
    await syncFile(input.path, input.content, existing.serverId)
    return true
  }

  const started = await startServer(input)
  if (!started) return false
  await syncFile(input.path, input.content)
  return true
}

/**
 * 启动当前语言的服务器。
 *
 * 并发保护：切文件很快时（按住方向键翻文件），`lsp.start` 会连发好几次，
 * 每次都真的起进程。这里用 `pendingStart` 串行化，后到的那次等前一次结束再判
 * 「连接已经可用」，于是内核只被调一次。
 */
async function startServer(input: {
  path: string
  languageId: string
  root?: string
}): Promise<boolean> {
  if (pendingStart !== null) {
    await pendingStart
    // 前一次的 start 可能刚好把这条语言的连接建好了，那就直接复用。
    const connection = state.connections[input.languageId] ?? null
    if (isUsable(connection)) {
      setState({ serverId: connection.serverId, status: 'ready' })
      return true
    }
  }

  setState({ status: 'starting', notice: null })

  const run = (async (): Promise<void> => {
    try {
      const result = await lspApi.start(
        input.root === undefined ? { path: input.path } : { path: input.path, root: input.root }
      )
      const connection = toConnection(result.server)
      setConnection(connection.languageId, connection)
      setState({ serverId: connection.serverId, status: 'ready', notice: null })
    } catch (error) {
      const code = lspErrorCodeOf(error)
      const message = describeLspError(error)
      if (isDegradeCode(code)) {
        // 降级：清掉可能残留的旧连接，状态栏留一行说明，不抛给调用方。
        dropConnection(input.languageId)
        setState({ serverId: null, status: 'unavailable', notice: message })
        return
      }
      dropConnection(input.languageId)
      setState({ serverId: null, status: 'error', notice: message })
    }
  })()

  pendingStart = run
  try {
    await run
  } finally {
    if (pendingStart === run) pendingStart = null
  }

  return state.status === 'ready'
}

/**
 * 同步一份正文。
 *
 * `serverId` 缺省取当前连接。失败分两种：`10003`（句柄过期）就把本地连接丢掉，
 * 下次开文件重来；其它错误只记 notice，不打断编辑——一次 sync 失败不该让整个
 * 语言服务进入 error 态。
 */
export async function syncFile(path: string, content: string, serverId?: string): Promise<boolean> {
  const id = serverId ?? state.serverId
  if (id === null) return false

  try {
    await lspApi.sync(id, path, content)
    if (state.status === 'error' || state.status === 'unavailable') {
      setState({ status: 'ready', notice: null })
    }
    return true
  } catch (error) {
    const code = lspErrorCodeOf(error)
    const message = describeLspError(error)
    if (code === LSP_ERROR_SERVER_UNKNOWN) {
      const language = state.languageId
      dropConnection(language)
      setState({ serverId: null, status: 'stopped', notice: message })
      return false
    }
    setState({ notice: message })
    return false
  }
}

/**
 * 关掉当前语言的连接并清标记。
 *
 * 清标记是 `stop` 的必要动作：内核不再推诊断了，旧的红点会永久留着，看起来像
 * 「这个文件一直有错」。标记按 owner 清空，不误伤其它来源（例如 ESLint 插件）的标记。
 */
export async function stop(input?: { languageId?: string }): Promise<void> {
  const languageId = input?.languageId ?? state.languageId
  const connection = languageId === null ? null : (state.connections[languageId] ?? null)

  if (connection !== null) {
    try {
      await lspApi.stop(connection.serverId)
    } catch {
      // stop 幂等：内核说「本来就关了」（10003 / stopped=false）也算成功，
      // 网络层真断了也没有补救手段——本地状态照样清掉。
    }
    dropConnection(languageId)
  }

  // 当前文件的所有标记都属于这条连接，清空；诊断摘要一起丢，免得状态栏
  // 还挂着「2 个错误」但编辑器里已经没有红点了。
  if (state.path !== null) {
    markerWriter?.(state.path, [])
    const { [state.path]: _removed, ...rest } = state.diagnostics
    setState({ diagnostics: rest })
  }

  if (languageId === null || languageId === state.languageId) {
    setState({ serverId: null, status: 'stopped', notice: null })
  }
}

/** 工作区关闭 / 切走：把所有连接都停掉（内核那边进程不关会一直占内存）。 */
export async function stopAll(): Promise<void> {
  const connections = Object.values(state.connections)
  await Promise.all(
    connections.map(async (connection) => {
      try {
        await lspApi.stop(connection.serverId)
      } catch {
        // 同上：尽力而为。
      }
    })
  )

  if (state.path !== null) markerWriter?.(state.path, [])
  setState({
    status: 'stopped',
    serverId: null,
    connections: {},
    diagnostics: {},
    notice: null
  })
}

/**
 * 拉一遍连接清单并回写本地缓存。
 *
 * 用途两条：①内核重启后本地缓存的 `server_id` 全是废的，靠它对齐；
 * ②状态栏「重连 N 次」需要最新值。失败静默——这是探测性调用。
 */
export async function refreshServers(): Promise<LspConnection[]> {
  try {
    const result = await lspApi.servers()
    const connections: Record<string, LspConnection> = {}
    for (const info of result.servers) {
      connections[info.language_id] = toConnection(info)
    }
    const current = state.languageId === null ? null : (connections[state.languageId] ?? null)
    setState({
      connections,
      serverId: current === null ? null : current.serverId,
      status: current === null ? state.status : 'ready'
    })
    return result.servers.map(toConnection)
  } catch {
    return []
  }
}

/**
 * 卸载（工作区关闭、应用退出）。
 *
 * `markerWriter` 一并摘掉：留着会让 `handleDiagnostics` 往一个已经销毁的 model
 * 上打点（Monaco 会抛 `Model is disposed`）。
 */
export function dispose(): void {
  unsubscribeDiagnostics?.()
  unsubscribeDiagnostics = null
  markerWriter = null
  pendingStart = null
  state = EMPTY_STATE
  emit()
}

/** 注入标记写入器（`lsp-markers` 里调 `monaco.editor.setModelMarkers`）。 */
export function setMarkerWriter(writer: (path: string, markers: LspMarkerData[]) => void): void {
  markerWriter = writer
}

/** 只读快照，给 `useSyncExternalStore` 用。引用只在 `setState` 时变。 */
export function getLspState(): LspState {
  return state
}

/** 订阅状态变化；返回退订函数。 */
export function subscribeLsp(listener: Listener): () => void {
  listeners.add(listener)
  // 第一次有人订阅时顺手把诊断通道挂上：状态栏一渲染就该收到推送，
  // 不必等 `openFile` 触发（用户可能先打开了文件再挂 UI）。
  ensureDiagnosticsSubscription()
  return () => {
    listeners.delete(listener)
  }
}

/** 组件侧入口：`const lsp = useLspState()`，快照引用只在状态真变时更新。 */
export function useLspState(): LspState {
  return useSyncExternalStore(subscribeLsp, getLspState, getLspState)
}

/** 仅供单测复位（生产代码不要调）。 */
export function resetLspStoreForTests(): void {
  unsubscribeDiagnostics?.()
  unsubscribeDiagnostics = null
  markerWriter = null
  pendingStart = null
  state = EMPTY_STATE
  listeners.clear()
}
