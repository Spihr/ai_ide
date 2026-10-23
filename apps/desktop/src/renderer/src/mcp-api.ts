/**
 * M7 渲染侧 API 封装：把 `window.aiide.request` 收窄成 `mcp.*` 五个动作。
 *
 * 与 `git-api.ts` 同一套路——组件不拼方法名、不自己拼参数，错误码翻译也收在这里。
 * 内核给 MCP 单开了 `8001`~`8007` 一段错误码（见 `crates/core/src/mcp_api.rs`），
 * 每一码的处置完全不同：
 *
 * - `8001` 要用户去改 `mcp.json`（名字写错了）；
 * - `8002` 是连不上（命令不存在 / 端口不通 / 握手失败）；
 * - `8003` 只是「还没连」——UI 该给的是「连接」按钮，不是一段红色报错；
 * - `8004` / `8005` 是工具名写错与执行失败；
 * - `8006` 是超时；
 * - `8007` 是入参本身不是 JSON 对象。
 *
 * 所以这里不只翻译成人话，还额外给出 `recoverable`：前两类需要用户动手，后几类
 * 往往是「再点一次」或「改参数」就能过去，面板据此决定要不要把整块变成红色。
 */

import {
  Methods,
  type JsonValue,
  type McpCallParams,
  type McpCallResult,
  type McpConnectParams,
  type McpConnectResult,
  type McpDisconnectParams,
  type McpDisconnectResult,
  type McpServersParams,
  type McpServersResult,
  type McpToolsParams,
  type McpToolsResult
} from '@ai-ide/shared-types'

const MCP_ERROR_HINTS: Record<number, string> = {
  8001: '配置里没有这个 Server：请先在 `mcp.json` 里加上，再回来连接。',
  8002: '连接失败：命令不存在、端口不通或握手被拒，原始原因见上行。',
  8003: '这个 Server 还没连接：先点「连接」，再列工具或调用。',
  8004: '工具不存在：可能 Server 端已改名，试试点「刷新工具」。',
  8005: '工具执行失败：Server 明确返回了错误，正文里有它的原话。',
  8006: '调用超时：Server 没在期限内响应，可以重试或检查它是否卡住。',
  8007: '参数非法：`arguments` 必须是 JSON 对象，`server` 与工具名至少要给一个。'
}

/** 从主进程抛上来的错误文本里抠出 `[8002]` 这类协议码。 */
export function mcpErrorCode(message: string): number | null {
  const matched = /\[(-?\d+)\]/.exec(message)
  if (!matched) return null
  const code = Number(matched[1])
  return Number.isFinite(code) ? code : null
}

/** 错误码 → 人话；未知码返回空串，由调用方回落到原始 message。 */
export function mcpHintForError(code: number): string {
  return MCP_ERROR_HINTS[code] ?? ''
}

/**
 * 这个错误是不是「用户得去改点什么」。
 *
 * `8001` / `8002` / `8007` 属于配置或参数问题，重试没用；其余（未连接、工具不存在、
 * 执行失败、超时）都可以靠「连接 / 刷新 / 再点一次」解决，面板不该把它渲染成
 * 不可恢复的红色警告。
 */
export function isConfigError(code: number | null): boolean {
  return code === 8001 || code === 8002 || code === 8007
}

/** 把异常整理成「原文｜人话」；原文始终保留，否则用户贴不出可复现的报错。 */
export function describeMcpError(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error)
  const code = mcpErrorCode(message)
  const hint = code === null ? '' : mcpHintForError(code)
  return hint.length > 0 && !message.includes(hint) ? `${message}｜${hint}` : message
}

/** Server 状态 → 面板上那枚小标记的文案。认不出的状态原样显示，不藏。 */
export function serverStateLabel(state: string): string {
  switch (state) {
    case 'configured':
      return '未连接'
    case 'connecting':
      return '连接中'
    case 'ready':
      return '已连接'
    case 'stale':
      return '连接过期'
    case 'closed':
      return '已断开'
    default:
      return state
  }
}

/**
 * 解析面板里手写的参数文本。
 *
 * 只接受 JSON **对象**：`null` / 数组 / 字符串都退回到 `{}` 并给出提示——MCP 的
 * `arguments` 在协议里就是对象，放宽到「随便什么 JSON」只会让错误晚一步才暴露，
 * 而且报错来自 Server 内部，用户更难懂。
 */
export function parseToolArguments(
  text: string
): { ok: true; value: JsonValue } | { ok: false; reason: string } {
  const trimmed = text.trim()
  if (trimmed.length === 0) return { ok: true, value: {} }

  let parsed: unknown
  try {
    parsed = JSON.parse(trimmed)
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error)
    return { ok: false, reason: `参数不是合法 JSON：${detail}` }
  }

  if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
    return { ok: false, reason: '参数必须是 JSON 对象，例如 {"path": "README.md"}' }
  }
  return { ok: true, value: parsed as JsonValue }
}

/** 调用结果 → 面板正文：优先内核拼好的 `text`，空了再按内容块兜一层。 */
export function callResultText(result: McpCallResult): string {
  if (result.text.length > 0) return result.text
  if (result.content.length === 0) return '（Server 返回了空结果）'

  const parts: string[] = []
  for (const block of result.content) {
    if (block.kind === 'text' && block.text !== undefined) {
      parts.push(block.text)
    } else if (block.kind === 'image' || block.kind === 'audio') {
      // 面板不渲染二进制：如实说是哪一类、多大，比塞一段 base64 有用。
      const size = block.data === undefined ? 0 : Math.round((block.data.length * 3) / 4)
      parts.push(`[${block.kind} ${block.mime_type ?? 'unknown'} ${formatBytes(size)}]`)
    } else if (block.kind === 'resource_link') {
      parts.push(`[资源链接 ${block.name ?? ''} ${block.uri ?? ''}]`.trim())
    } else {
      parts.push(`[${block.kind} ${block.uri ?? ''}]`.trim())
    }
  }
  return parts.join('\n')
}

/** 字节数 → 人话（二进制块只显示个量级，不需要精确到字节）。 */
export function formatBytes(size: number): string {
  if (size < 1024) return `${size} B`
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`
  return `${(size / (1024 * 1024)).toFixed(1)} MB`
}

export const mcpApi = {
  /** 已配置 ∪ 已连接的 Server 清单。只读，随时可拉。 */
  servers: (): Promise<McpServersResult> =>
    window.aiide.request(Methods.mcp.servers, {} satisfies McpServersParams) as Promise<McpServersResult>,

  /** 建连 + 拉工具清单。连不上会抛 `8001` / `8002`。 */
  connect: (server: string): Promise<McpConnectResult> =>
    window.aiide.request(Methods.mcp.connect, {
      server
    } satisfies McpConnectParams) as Promise<McpConnectResult>,

  /** 断开（幂等）：本就没连时 `closed = false`，不算错误。 */
  disconnect: (server: string): Promise<McpDisconnectResult> =>
    window.aiide.request(Methods.mcp.disconnect, {
      server
    } satisfies McpDisconnectParams) as Promise<McpDisconnectResult>,

  /** 列工具；省略 `server` 即全部已连接 Server。 */
  tools: (server?: string): Promise<McpToolsResult> =>
    window.aiide.request(Methods.mcp.tools, {
      ...(server === undefined ? {} : { server })
    } satisfies McpToolsParams) as Promise<McpToolsResult>,

  /** 调一个工具。`server` 省略时 `tool` 必须是 `mcp__<server>__<tool>` 全名。 */
  call: (input: { server?: string; tool: string; args?: JsonValue }): Promise<McpCallResult> =>
    window.aiide.request(Methods.mcp.call, {
      ...(input.server === undefined ? {} : { server: input.server }),
      tool: input.tool,
      arguments: input.args ?? {}
    } satisfies McpCallParams) as Promise<McpCallResult>
}
