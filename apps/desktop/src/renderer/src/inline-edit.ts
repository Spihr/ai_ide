/**
 * Inline Edit（`Ctrl+K`）的纯逻辑：选区换算、片段拼接、错误码文案。
 *
 * 抽成纯函数有三个原因，和这层代码「只能靠按真键盘才验证得到」有关：
 *
 * 1. **选区换算是唯一容易错一位的地方**：编辑器给的是 1-based 行 + 0-based 列，
 *    内核 `TextSelection` 也是这套，但外壳的 `EditorSelection` 允许「只有行没有列」
 *    （对话面板的「引用」就是这么用的），两种形态都得能换成合法入参。
 * 2. **接受建议要自己拼文件**：内核只回 `original` / `replacement` / `range`，不写盘
 *    （这是 `inline_edit.propose` 的契约），拼错一个换行整份文件就被改坏了。
 * 3. **9001 / 9003 对用户没意义**：内核的错误码是给程序看的，浮层里必须给出「怎么办」。
 *
 * 这里不碰 DOM、不碰 IPC、不读模块级状态，所以 `inline-edit.test.ts` 能直接断言。
 */

import type { TextSelection } from '@ai-ide/shared-types'

import type { EditorSelection } from './store'

/**
 * 内联改写的错误码 → 可读提示。
 *
 * 只覆盖「用户能自己处理」的几档。9001 和 9003 是任务点名要求的两个：前者是选区
 * 与文件对不上（多半是文件在别处被改了），后者是模型只回了解释、没回代码。
 */
export const INLINE_EDIT_ERROR_TEXT: Record<number, string> = {
  9001: '选区越界：选中的行/列超出了文件当前的实际长度（文件可能已在别处被改动）。重新选中要改的代码再试。',
  9002: '模型不可用：还没有配置可用的 Provider / 模型，或这次调用失败了。先去设置里配好再试。',
  9003: '模型只给了说明、没给代码，因此没有可替换的内容。把指令写得更具体一点（例如「改成早退返回」）再试。',
  9004: '这次改动的范围太大，超过了内联编辑的上限。缩小选区，或分几次改。'
}

/** 内核错误码落在未收录档位时的兜底文案。 */
export const INLINE_EDIT_FALLBACK_TEXT = '内联编辑失败，文件未改动。'

/** 结构化后的错误：`code` 拿不到时为 `null`（纯网络错误、渲染端异常）。 */
export interface InlineEditErrorView {
  code: number | null
  text: string
}

/**
 * 把内联改写调用抛出的错误翻成人话。
 *
 * `extract` 由调用方注入（渲染端用 `decodeRpcErrorPayload`），这样本模块不必依赖
 * IPC 层，测试也能直接喂一个假的解码器。
 */
export function describeInlineEditError(
  error: unknown,
  extract: (error: unknown) => { code: number; message: string } | null
): InlineEditErrorView {
  const payload = extract(error)
  if (!payload) {
    const raw = error instanceof Error ? error.message : String(error)
    return { code: null, text: raw.trim() === '' ? INLINE_EDIT_FALLBACK_TEXT : raw }
  }
  const known = INLINE_EDIT_ERROR_TEXT[payload.code]
  return {
    code: payload.code,
    // 未收录的档位也别把用户丢在「未知错误 9007」里：带上内核原话更有用。
    text: known ?? `${INLINE_EDIT_FALLBACK_TEXT}（${payload.code}${payload.message ? `：${payload.message}` : ''}）`
  }
}

function clampColumn(value: number | undefined, max: number, fallback: number): number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0) return fallback
  return Math.min(value, max)
}

/**
 * 编辑器选区 → 协议里的 `TextSelection`。
 *
 * 只有行没有列时按**整行**算（起始 0、结束为该行长度），因为「用户选了第 7 行」在
 * 编辑器里的直观含义就是整行；有列则原样透传（都已经 0-based，不需要 ±1）。
 */
export function toTextSelection(selection: EditorSelection, content: string): TextSelection {
  const lines = content.split('\n')
  const startLine = selection.startLine
  const endLine = Math.max(selection.startLine, selection.endLine)
  const startText = lines[startLine - 1] ?? ''
  const endText = lines[endLine - 1] ?? ''
  const hasColumns =
    typeof selection.startColumn === 'number' && typeof selection.endColumn === 'number'
  const startColumn = hasColumns ? clampColumn(selection.startColumn, startText.length, 0) : 0
  const endColumn = hasColumns
    ? clampColumn(selection.endColumn, endText.length, endText.length)
    : endText.length
  return {
    start_line: startLine,
    start_column: startColumn,
    end_line: endLine,
    // 同一行内反着拖（列从大到小）也算合法输入，顺手摆正。
    end_column: endLine === startLine && endColumn < startColumn ? startColumn : endColumn
  }
}

/**
 * 把 `replacement` 贴回 `content` 的 `range` 位置，返回新全文。
 *
 * 行、列两端都按「缺省宽松」处理：列超出该行长度就截到行尾（内核回显的 range 一定
 * 合法，这里只是不让一次越界把整份文件写成坨乱码）；行号越界则返回 `null`，让调用方
 * 明确拒绝写盘而不是猜一个位置。
 *
 * 跨行选区的 `end_column` 是「结束行内的列」，所以尾行从该列起保留——这与
 * VS Code 的 `getValueInRange` 一致。
 */
export function spliceSelection(
  content: string,
  range: TextSelection,
  replacement: string
): string | null {
  const lines = content.split('\n')
  const startLine = range.start_line
  const endLine = range.end_line
  if (!Number.isInteger(startLine) || !Number.isInteger(endLine)) return null
  if (startLine < 1 || endLine < startLine || endLine > lines.length) return null
  const startText = lines[startLine - 1] ?? ''
  const endText = lines[endLine - 1] ?? ''
  const startColumn = clampColumn(range.start_column, startText.length, 0)
  const endColumn = clampColumn(range.end_column, endText.length, endText.length)
  const merged = `${startText.slice(0, startColumn)}${replacement}${endText.slice(endColumn)}`
  return [...lines.slice(0, startLine - 1), merged, ...lines.slice(endLine)].join('\n')
}

/** 「第 7 行」/「第 7–9 行」——浮层头部用来告诉用户「改的是哪一段」。 */
export function inlineEditRangeLabel(range: TextSelection): string {
  return range.start_line === range.end_line
    ? `第 ${range.start_line} 行`
    : `第 ${range.start_line}–${range.end_line} 行`
}
