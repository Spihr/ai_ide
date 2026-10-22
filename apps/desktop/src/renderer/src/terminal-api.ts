/**
 * 终端协议调用的薄封装 + base64 编解码。
 *
 * 为什么要自己写 base64：协议两侧的 `data` 都是**原始字节的 base64**（`\r`、
 * `Ctrl-C`、方向键序列过字符串会在 JSON 转义里丢层），而渲染进程里既没有
 * Node 的 `Buffer`（不能开 `nodeIntegration`），`btoa` / `atob` 又只认 latin1
 * 字符串。于是这里做两组转换，都是纯函数，可单测：
 *
 * - 写：`TextEncoder`（UTF-8）→ 字节 → base64
 * - 读：base64 → 字节 → 交给 xterm 的 `write(Uint8Array)`
 *
 * 解码**不做** UTF-8 还原：xterm 的 `write` 接受 `Uint8Array`，让它自己按
 * 终端状态机处理跨 chunk 的半截多字节字符（我们按块解码成字符串反而会切碎）。
 */

import {
  Methods,
  type OkResult,
  type TerminalCreateParams,
  type TerminalCreateResult,
  type TerminalListResult,
  type TerminalResizeResult,
  type TerminalView,
  type TerminalWriteResult
} from '@ai-ide/shared-types'

/** base64 字母表（标准，带 `=` 填充），与内核 `base64::STANDARD` 对齐。 */
const BASE64_ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'

/** 字节 → base64。手写而非 `btoa`：`btoa` 对 `>0xFF` 与超长入参都不友好。 */
export function encodeBase64(bytes: Uint8Array): string {
  let out = ''
  for (let index = 0; index < bytes.length; index += 3) {
    const first = bytes[index] ?? 0
    const second = bytes[index + 1]
    const third = bytes[index + 2]

    out += BASE64_ALPHABET[first >> 2]
    out += BASE64_ALPHABET[((first & 0x03) << 4) | ((second ?? 0) >> 4)]
    out += second === undefined ? '=' : BASE64_ALPHABET[((second & 0x0f) << 2) | ((third ?? 0) >> 6)]
    out += third === undefined ? '=' : BASE64_ALPHABET[third & 0x3f]
  }
  return out
}

/**
 * base64 → 字节。
 *
 * 容错目标是「不要因为一个坏块把整个终端打死」：非法字符跳过、缺填充按实际
 * 长度收尾。内核只会发标准 base64，这里的容错是给日志里手抄的串留活路。
 */
export function decodeBase64(text: string): Uint8Array {
  const clean = text.replace(/[^A-Za-z0-9+/]/g, '')
  const byteLength = Math.floor((clean.length * 3) / 4)
  const bytes = new Uint8Array(byteLength)

  let byteIndex = 0
  let buffer = 0
  let bits = 0

  for (const char of clean) {
    const value = BASE64_ALPHABET.indexOf(char)
    if (value < 0) continue
    buffer = (buffer << 6) | value
    bits += 6
    if (bits >= 8) {
      bits -= 8
      bytes[byteIndex++] = (buffer >> bits) & 0xff
    }
  }

  return bytes.subarray(0, byteIndex)
}

/** 把用户输入（字符串）编码成协议要的 base64。 */
export function encodeInput(text: string): string {
  return encodeBase64(new TextEncoder().encode(text))
}

/** 终端协议调用的失败原因统一成一句话，供 UI 直接展示。 */
function describe(error: unknown): string {
  if (error instanceof Error) return error.message
  return String(error)
}

export interface CreateTerminalOptions {
  /** 起始工作目录；不给就继承内核进程的 cwd。 */
  cwd?: string | null
  cols?: number
  rows?: number
}

/** `terminal.create`：开一个会话。 */
export async function createTerminal(
  options: CreateTerminalOptions = {}
): Promise<TerminalView> {
  const params: TerminalCreateParams = {
    args: [],
    env: {}
  }
  if (options.cwd) params.cwd = options.cwd
  if (options.cols && options.cols > 0) params.cols = options.cols
  if (options.rows && options.rows > 0) params.rows = options.rows

  const result = (await window.aiide.request(
    Methods.terminal.create,
    params
  )) as TerminalCreateResult
  return result.terminal
}

/** `terminal.write`：把按键字节写进 PTY。 */
export async function writeTerminal(terminalId: string, data: string): Promise<number> {
  const result = (await window.aiide.request(Methods.terminal.write, {
    terminal_id: terminalId,
    data
  })) as TerminalWriteResult
  return result.bytes
}

/** `terminal.resize`：改尺寸，返回内核夹取后的会话视图。 */
export async function resizeTerminal(
  terminalId: string,
  cols: number,
  rows: number
): Promise<TerminalView> {
  const result = (await window.aiide.request(Methods.terminal.resize, {
    terminal_id: terminalId,
    cols,
    rows
  })) as TerminalResizeResult
  return result.terminal
}

/**
 * `terminal.kill`：杀掉会话。
 *
 * 子进程的退出信息仍会经 `terminal.exit` 推来；这里的错误多半是「会话已经没了」，
 * 调用方按幂等处理（关标签页不必因为这次失败而卡住）。
 */
export async function killTerminal(terminalId: string): Promise<void> {
  await window.aiide.request(Methods.terminal.kill, { terminal_id: terminalId })
}

/** `terminal.list`：列出内核侧存活会话。 */
export async function listTerminals(): Promise<TerminalView[]> {
  const result = (await window.aiide.request(Methods.terminal.list, {})) as TerminalListResult
  return result.terminals
}

export { describe as describeTerminalError }

/** `terminal.kill` 的响应形状（`{ ok: true }`），导出仅为类型完整。 */
export type TerminalKillResult = OkResult
