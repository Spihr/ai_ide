/**
 * 语言服务（LSP）渲染侧 API 封装：把 `window.aiide.request` 收窄成 `lsp.*` 五个动作，
 * 并把内核那一段错误码翻译成「降级 / 报错」两种处置。
 *
 * 与 `mcp-api.ts` / `git-api.ts` 同一套路——组件不拼方法名、不自己拼参数。区别在于
 * 这里**多一层降级判断**：内核给语言服务单开了 `10001`~`10003` 三个码（见
 * `crates/core/src/lsp_api.rs`），其中前两个不是「出错」，而是「这台机器上本来就没有
 * 可用的语言服务器」：
 *
 * - `10001` 命令不存在（没装 `typescript-language-server` 之类）——**静默降级**，
 *   状态栏显示「语言服务器不可用」，绝不弹错误框：用户只是没装某个可选组件，
 *   不该被当成故障打断编码。
 * - `10002` 该语言不支持——同样降级，理由同上。
 * - `10003` 句柄不存在 / 已关闭——这是时序问题（我们拿着一个过期连接去 sync），
 *   处置是「把本地连接状态清掉，等下次开文件重来」，也不是给用户看的错误。
 *
 * 所以本文件只把「怎么办」告诉调用方（`isDegradeCode`），不自己弹框；打点与状态
 * 全归 `lsp-store`，错误文案只进状态栏的 tooltip。
 */

import {
  Methods,
  type LspDiagnostic,
  type LspDiagnosticSeverity,
  type LspPosition,
  type LspRange,
  type LspRequestMethod,
  type LspRequestParams,
  type LspRequestResult,
  type LspServersResult,
  type LspStartResult,
  type LspSyncResult
} from '@ai-ide/shared-types'

import { decodeRpcErrorPayload, stripRpcErrorPayload } from '../../shared/ipc'

/** 命令不存在：这台机器上没装这个语言服务器。 */
export const LSP_ERROR_NOT_INSTALLED = 10001
/** 该语言没有注册表条目、也不支持按扩展名推。 */
export const LSP_ERROR_LANGUAGE_UNSUPPORTED = 10002
/** 句柄不存在或已经关掉（过期连接）。 */
export const LSP_ERROR_SERVER_UNKNOWN = 10003

/**
 * 从主进程抛上来的错误文本里抠出协议码。
 *
 * 优先认结构化信封（`decodeRpcErrorPayload`），其次退回 `[10001]` 这种裸写法——
 * 老版本日志与手工抛的错里只有后者。
 */
export function lspErrorCode(message: string): number | null {
  const payload = decodeRpcErrorPayload(message)
  if (payload !== null) return payload.code

  const matched = /\[(-?\d+)\]/.exec(message)
  if (!matched) return null
  const code = Number(matched[1])
  return Number.isFinite(code) ? code : null
}

/**
 * 这个码是否属于「没有语言服务器可用」的降级路径。
 *
 * 降级 = 不弹框、不重试、状态栏留一行说明。`10003` 不算降级（它是连接过期，
 * 调用方会把本地句柄清掉，下一个文件仍会正常启动）。
 */
export function isDegradeCode(code: number | null): boolean {
  return code === LSP_ERROR_NOT_INSTALLED || code === LSP_ERROR_LANGUAGE_UNSUPPORTED
}

/** 错误码 → 状态栏 tooltip 用的一行说明；认不出的码返回空串。 */
export function lspHintForError(code: number): string {
  switch (code) {
    case LSP_ERROR_NOT_INSTALLED:
      return '本机没找到这个语言服务器（未安装或不在 PATH 上）；语法高亮与编辑不受影响。'
    case LSP_ERROR_LANGUAGE_UNSUPPORTED:
      return '这个文件的语言不在语言服务注册表里，跳转 / 悬浮 / 补全暂不可用。'
    case LSP_ERROR_SERVER_UNKNOWN:
      return '语言服务连接已失效，下次打开文件会重新启动。'
    default:
      return ''
  }
}

/**
 * 异常里带的协议码。
 *
 * 单独抽出来是因为**必须先取码再润色文案**：`describeLspError` 会在原文后面
 * 追加人话，而结构化信封是靠「message 末尾那一段 JSON」认的，一旦追加在后面，
 * `JSON.parse` 就会因为尾随文字失败，码随之丢失。
 */
export function lspErrorCodeOf(error: unknown): number | null {
  const message = error instanceof Error ? error.message : String(error)
  return lspErrorCode(message)
}

/**
 * 把异常整理成「原文｜人话」；原文始终保留，否则用户贴不出可复现的报错。
 *
 * 展示前会剥掉结构化信封：那串 `\0[aiide-rpc-error]\0{...}` 是给程序看的，
 * 塞进 tooltip 只会变成一坨乱码般的控制字符。
 */
export function describeLspError(error: unknown): string {
  const raw = error instanceof Error ? error.message : String(error)
  const message = stripRpcErrorPayload(raw)
  const code = lspErrorCode(raw)
  const hint = code === null ? '' : lspHintForError(code)
  return hint.length > 0 && !message.includes(hint) ? `${message}｜${hint}` : message
}

/**
 * Monaco 的标记严重度。
 *
 * 数值是 Monaco 自己的枚举（`MarkerSeverity`），直接写出来而不是 import
 * `monaco-editor` 的类型：本文件要能在 vitest 里被纯逻辑测试，不能把整个
 * 编辑器拖进单测环境。
 */
export const MarkerSeverity = {
  Hint: 1,
  Info: 2,
  Warning: 4,
  Error: 8
} as const

/** 标记的归属者。同 model 上不同 owner 的标记互不覆盖，卸载时按它清空。 */
export const LSP_MARKER_OWNER = 'ai-ide-lsp'

/**
 * 内核 severity（枚举字符串，或 LSP 线格式的 1..4）→ Monaco 数值。
 *
 * 两套口径都认：内核已经换成枚举字符串，但 mock 服务器 / 旧测试里直接喂 `1..4`
 * 更省事。`unknown` 落到 `Info`——内核的注释写着「不猜成 error，免得把提示画成
 * 红点」，这里保持一致：既不红也不至于完全看不见。
 */
export function severityToMarkerSeverity(severity: LspDiagnosticSeverity | number): number {
  if (typeof severity === 'number') {
    switch (severity) {
      case 1:
        return MarkerSeverity.Error
      case 2:
        return MarkerSeverity.Warning
      case 3:
        return MarkerSeverity.Info
      case 4:
        return MarkerSeverity.Hint
      default:
        return MarkerSeverity.Info
    }
  }

  switch (severity) {
    case 'error':
      return MarkerSeverity.Error
    case 'warning':
      return MarkerSeverity.Warning
    case 'information':
      return MarkerSeverity.Info
    case 'hint':
      return MarkerSeverity.Hint
    default:
      return MarkerSeverity.Info
  }
}

/** 0-based 的位置（内部运算用的中间形态）。 */
export interface ZeroBasedPosition {
  line: number
  character: number
}

/**
 * 1-based 位置 → 0-based。
 *
 * 内核的 `LspPosition` 行列都从 1 起算（与 Monaco 的模型 API 一致），而做区间运算
 * （夹取、判空、排序）时用 0-based 更不容易差一，所以统一先转成 0-based 再做数学，
 * 最后回写标记时加回去。越界（`< 1`）夹到 0：宁可把标记画在第一行第一列，
 * 也不要因为服务器给了 `0` 就让 `setModelMarkers` 抛错。
 */
export function positionToZeroBased(position: LspPosition): ZeroBasedPosition {
  const line = Number.isFinite(position.line) ? Math.max(0, Math.floor(position.line) - 1) : 0
  const character = Number.isFinite(position.character)
    ? Math.max(0, Math.floor(position.character) - 1)
    : 0
  return { line, character }
}

/** 半开区间 → 0-based 半开区间。 */
export function rangeToZeroBased(range: LspRange): {
  start: ZeroBasedPosition
  end: ZeroBasedPosition
} {
  const start = positionToZeroBased(range.start)
  const end = positionToZeroBased(range.end)
  // 服务器偶尔给出反向区间（end < start），夹成正向——否则 Monaco 会画出负长度标记。
  if (end.line < start.line || (end.line === start.line && end.character < start.character)) {
    return { start, end: start }
  }
  return { start, end }
}

/**
 * Monaco `IMarkerData` 的最小形状。
 *
 * 手写而不是引 `monaco.editor.IMarkerData`：一是单测环境没有 monaco，
 * 二是这里只用到这几个字段，收窄之后 `lsp-markers` 传进去必然是合法的。
 */
export interface LspMarkerData {
  severity: number
  message: string
  source?: string
  code?: string
  startLineNumber: number
  startColumn: number
  endLineNumber: number
  endColumn: number
}

/**
 * 一条诊断 → 一条标记。
 *
 * **注意最后那个 +1**：内部区间先转成 0-based 做运算，但 Monaco 的
 * `setModelMarkers` 收的是 **1-based** 行列（`startLineNumber` 从 1 起），
 * 所以回写时必须加回去。少这个 +1 会让所有标记整体左上偏一格，是最常见的
 * 「诊断位置看起来差一个字符」的来源。
 */
export function diagnosticToMarker(diagnostic: LspDiagnostic): LspMarkerData {
  const { start, end } = rangeToZeroBased(diagnostic.range)
  const marker: LspMarkerData = {
    severity: severityToMarkerSeverity(diagnostic.severity),
    message: diagnostic.message,
    startLineNumber: start.line + 1,
    startColumn: start.character + 1,
    endLineNumber: end.line + 1,
    endColumn: end.character + 1
  }
  if (diagnostic.source !== undefined) marker.source = diagnostic.source
  if (diagnostic.code !== undefined) marker.code = diagnostic.code
  return marker
}

/** 一批诊断 → 一批标记（整篇替换语义：返回的就是该文件应显示的全集）。 */
export function diagnosticsToMarkers(
  diagnostics: readonly LspDiagnostic[]
): LspMarkerData[] {
  return diagnostics.map(diagnosticToMarker)
}

/** 这批诊断里 error / warning 的条数（状态栏那个角标用它）。 */
export function countProblems(diagnostics: readonly LspDiagnostic[]): number {
  let count = 0
  for (const diagnostic of diagnostics) {
    if (diagnostic.severity === 'error' || diagnostic.severity === 'warning') count += 1
  }
  return count
}

export const lspApi = {
  /**
   * 启动（或复用）一条连接。
   *
   * 只给 `path` + `root`：语言由内核按扩展名推、命令由注册表决定。渲染进程
   * **不**传 `command`，也就没有「让它跑任意可执行文件」的通道。
   */
  start: (input: { path: string; root?: string }): Promise<LspStartResult> =>
    window.aiide.request(Methods.lsp.start, {
      path: input.path,
      ...(input.root === undefined ? {} : { root: input.root }),
      args: []
    }) as Promise<LspStartResult>,

  /** 关掉一条连接（幂等：本来就关了时 `stopped = false`，不算错误）。 */
  stop: (serverId: string): Promise<void> =>
    window.aiide.request(Methods.lsp.stop, { server_id: serverId }) as Promise<void>,

  /**
   * 同步一份正文（`didOpen` / `didChange`）。
   *
   * 幂等由内核保证：同一份文本重复同步不推版本、不重发 `didChange`，所以外壳
   * 不必自己维护「上次发的是哪版」——保存后无脑再 sync 一次是安全的。
   */
  sync: (serverId: string, path: string, content?: string): Promise<LspSyncResult> =>
    window.aiide.request(Methods.lsp.sync, {
      server_id: serverId,
      path,
      ...(content === undefined ? {} : { content })
    }) as Promise<LspSyncResult>,

  /** 当前活着的连接清单（只读，随时可拉）。 */
  servers: (): Promise<LspServersResult> =>
    window.aiide.request(Methods.lsp.servers, {}) as Promise<LspServersResult>,

  /**
   * 四个只读查询（定义 / 引用 / 悬浮 / 补全）。
   *
   * `method` 是枚举报文，不是线格式方法名——外壳拿不到 `workspace/executeCommand`
   * 这类有副作用的通道。
   */
  request: (input: {
    serverId: string
    method: LspRequestMethod
    path: string
    position: LspPosition
  }): Promise<LspRequestResult> =>
    window.aiide.request(Methods.lsp.request, {
      server_id: input.serverId,
      method: input.method,
      path: input.path,
      position: input.position
    } satisfies LspRequestParams) as Promise<LspRequestResult>
}
