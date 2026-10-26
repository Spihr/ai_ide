/**
 * Tab 补全的纯逻辑层。
 *
 * 与 `lsp-providers.ts` 同一套分层理由：这一层不碰 `monaco-editor`、不碰 DOM，
 * 只做「候选从哪来 → 这次 Tab 该不该拦」的判断，因此在 vitest 里能直接跑。
 * 真正的键盘接线（`keydown` 捕获、`executeEdits`）留在 `CodeEditor.tsx`。
 *
 * **候选只有两个来源，且优先级不同**：
 * - `lsp`：语言服务器按语义给出的补全项（带 `detail`、已按相关度排序），最贴合
 *   「这个位置能写什么」；
 * - `symbol`：内核符号索引里的声明（函数 / 类 / 常量…），不需要语言服务器也能用，
 *   这是没装 `typescript-language-server` 时 Tab 仍然有意义的兜底。
 *
 * 两者按 `label` 去重，LSP 优先——同一个符号两边都有时，LSP 那份的
 * 替换区间与插入文本更准（可能带括号、可能带参数占位）。
 *
 * **不拦截 Tab 是默认行为**：没有候选（或候选补不出新字符）时必须原样放过，
 * 让 Monaco 继续做缩进 / 反向缩进 / 片段跳转。宁可少补一次，也不能把用户的
 * 缩进习惯改掉。同理，与既有的 `Ctrl+空格` 不抢：建议列表已经展开时由 Monaco
 * 自己处理 Tab（接受选中项），这一层不参与。
 */

import type { Symbol } from '@ai-ide/shared-types'

import type { CompletionHint, ZeroBasedRange } from './lsp-providers'

/** 一条 Tab 候选。`insertText` 才是真正写进编辑器的文本（`label` 只用于展示）。 */
export interface TabCandidate {
  label: string
  insertText: string
  detail?: string
  source: 'lsp' | 'symbol'
}

/**
 * 短于这个长度的前缀不参与 Tab 补全。
 *
 * 一个字母后面按 Tab 是最常见的缩进场景（`if` 里换行、`for` 里换行），而一个字母
 * 能前缀匹配上几百个符号——拦截下来插入第一条比不补更糟。
 */
export const MIN_TAB_PREFIX = 2

/** 候选上限：够用了，再多也不会有人靠反复 Tab 翻。 */
export const MAX_TAB_CANDIDATES = 50

export interface TabCandidatesInput {
  /** 光标前已经输入的单词（不含 `$` 之类非单词字符，与 Monaco 的 `word` 同义）。 */
  prefix: string
  /** 语言服务返回的补全项。 */
  completions?: readonly CompletionHint[]
  /** 符号索引里查到的声明。 */
  symbols?: readonly Symbol[]
  /** 当前文件相对工作区根的路径（`/` 分隔），用于把「本文件的符号」排前面。 */
  currentPath?: string | null
  limit?: number
}

/** 路径归一：反斜杠转正斜杠、去掉开头的 `./` 与 `/`，便于比较相对路径。 */
function normalizeRelativePath(path: string): string {
  return path.replace(/\\/g, '/').replace(/^\.?\//, '')
}

/**
 * 合并两个来源的候选：过滤前缀 → 去重 → 排序 → 截断。
 *
 * 排序规则（稳定、可测）：本文件的符号 > LSP 项（保持服务端顺序）> 其它文件的符号
 * （按名字）。「本文件优先」是因为在当前文件里写代码时，最常补的就是同文件的
 * 私有函数/常量。
 */
export function toTabCandidates(input: TabCandidatesInput): TabCandidate[] {
  const prefix = input.prefix
  if (prefix.length < MIN_TAB_PREFIX) return []

  const lower = prefix.toLowerCase()
  const limit = Math.max(0, input.limit ?? MAX_TAB_CANDIDATES)
  const currentPath = input.currentPath ? normalizeRelativePath(input.currentPath) : null
  const seen = new Set<string>()
  const lsp: TabCandidate[] = []
  const local: TabCandidate[] = []
  const foreign: TabCandidate[] = []

  const push = (bucket: TabCandidate[], candidate: TabCandidate): void => {
    if (candidate.insertText.length === 0) return
    // 补不出新字符的候选没有意义（用户已经打完了），拦下来只会让 Tab 变成哑键。
    if (candidate.insertText === prefix) return
    // 按 `label` 去重：同一个符号名在 LSP 与符号索引里各有一份时，保留先到的
    // 那份（LSP 优先）——即便 LSP 的 `insertText` 多了括号占位，它也是同一个候选，
    // 让用户看到两个 `format` 只是噪音。
    const key = candidate.label.toLowerCase()
    if (seen.has(key)) return
    seen.add(key)
    bucket.push(candidate)
  }

  for (const hint of input.completions ?? []) {
    const label = hint.label ?? ''
    if (!label.toLowerCase().startsWith(lower)) continue
    const candidate: TabCandidate = {
      label,
      insertText: hint.insertText ?? label,
      source: 'lsp'
    }
    if (hint.detail !== undefined && hint.detail.length > 0) candidate.detail = hint.detail
    push(lsp, candidate)
  }

  for (const symbol of input.symbols ?? []) {
    const name = symbol.name ?? ''
    if (!name.toLowerCase().startsWith(lower)) continue
    const candidate: TabCandidate = {
      label: name,
      insertText: name,
      source: 'symbol'
    }
    if (symbol.kind.length > 0) candidate.detail = symbol.kind
    const isLocal =
      currentPath !== null && normalizeRelativePath(symbol.path ?? '') === currentPath
    push(isLocal ? local : foreign, candidate)
  }

  foreign.sort((a, b) => a.label.localeCompare(b.label))
  return [...local, ...lsp, ...foreign].slice(0, limit)
}

/** 这次 Tab 的处置结果。`pass` 表示「不拦」，交给 Monaco 的原有行为。 */
export type TabDecision =
  | { kind: 'insert'; insertText: string; candidate: TabCandidate; extended: boolean }
  | { kind: 'pass'; reason: 'no-candidate' | 'no-gain' }

/** 一组字符串的最长公共前缀。 */
export function longestCommonPrefix(values: readonly string[]): string {
  if (values.length === 0) return ''
  let prefix = values[0] ?? ''
  for (const value of values.slice(1)) {
    let index = 0
    const max = Math.min(prefix.length, value.length)
    while (index < max && prefix[index] === value[index]) index += 1
    prefix = prefix.slice(0, index)
    if (prefix.length === 0) break
  }
  return prefix
}

/**
 * 决定这次 Tab 干什么。
 *
 * - 无候选 → `pass`（缩进照旧）；
 * - 单候选 → 直接补；
 * - 多候选 → 先尝试「补到公共前缀」（这一步一定是对的，不会替用户做选择）；
 *   公共前缀已用尽时插第一条：LSP 的顺序本身就是相关度排序，符号也按
 *   「本文件优先」排过，取第一条是此时最好的猜测。
 */
export function decideTabAction(
  candidates: readonly TabCandidate[],
  prefix: string
): TabDecision {
  if (candidates.length === 0) return { kind: 'pass', reason: 'no-candidate' }
  const first = candidates[0]
  if (!first) return { kind: 'pass', reason: 'no-candidate' }

  if (candidates.length === 1) {
    if (first.insertText === prefix) return { kind: 'pass', reason: 'no-gain' }
    return { kind: 'insert', insertText: first.insertText, candidate: first, extended: true }
  }

  const common = longestCommonPrefix(candidates.map((item) => item.insertText))
  if (common.length > prefix.length) {
    return { kind: 'insert', insertText: common, candidate: first, extended: true }
  }
  if (first.insertText === prefix) return { kind: 'pass', reason: 'no-gain' }
  return { kind: 'insert', insertText: first.insertText, candidate: first, extended: false }
}

/**
 * 光标前那段前缀在 Monaco 里的替换区间（0-based 半开）。
 *
 * `position` 用 Monaco 的 **1-based** 行列（`editor.getPosition()` 的原样值）；
 * 返回的 `character` 是 0-based 列，与 `lsp-providers` 的中间形态一致。
 */
export function replaceRangeFor(
  position: { lineNumber: number; column: number },
  prefix: string
): { start: { line: number; character: number }; end: { line: number; character: number } } {
  const line = Math.max(0, position.lineNumber - 1)
  const caret = Math.max(0, position.column - 1)
  const start = Math.max(0, caret - prefix.length)
  return { start: { line, character: start }, end: { line, character: caret } }
}

/** 一路候选来源；失败一律降级成空数组，绝不把异常抛给键盘处理。 */
async function settle<T>(read: () => Promise<readonly T[]>): Promise<readonly T[]> {
  try {
    return await read()
  } catch {
    // 语言服务器崩了、索引还没就绪、主进程拒绝……对 Tab 来说都只是「这次没有候选」。
    return []
  }
}

/** 键盘处理需要的全部依赖；由 `CodeEditor` 注入，测试里给假实现。 */
export interface TabRunnerHost {
  /** 光标前已经输入的单词（Monaco 的 `getWordUntilPosition().word`）。 */
  prefix: string
  /** 光标位置（Monaco 的 1-based 行列原样传入）。 */
  position: { lineNumber: number; column: number }
  /** 当前文件相对工作区根的路径（`/` 分隔），用于「本文件符号优先」。 */
  currentPath?: string | null
  /** 语言服务补全项。 */
  completions: () => Promise<readonly CompletionHint[]>
  /** 符号索引里的声明。 */
  symbols: () => Promise<readonly Symbol[]>
  /** 真要补：把 `prefix` 那段换成 `text`。 */
  insert: (text: string, range: ZeroBasedRange, candidate: TabCandidate) => void
  /** 不拦：交回 Monaco 的缩进 / 反向缩进 / 片段跳转。 */
  pass: () => void
}

/**
 * Tab 键的完整决策流程（除 Monaco 调用之外的部分都在这里）。
 *
 * 与 `decideTabAction` 的分工：那个只判断「候选怎么用」，这个负责「候选从哪来、
 * 这次到底动不动编辑器」。三条硬规则：
 *
 * - **前缀太短不查来源**：一个字母按 Tab 是最常见的缩进，连请求都不该发；
 * - **两路来源并发查、各自降级**：LSP 没连接、符号索引未就绪都只是少一路候选，
 *   不能因此把 Tab 变成哑键（更不能让缩进失效）；
 * - **`pass` 一定会被调**：只要不补，就必须显式还回 Monaco 的默认行为——因为
 *   接线的 action 抢占了 `Tab`，不还回去缩进就彻底没了。
 */
export async function runTabCompletion(host: TabRunnerHost): Promise<TabDecision> {
  if (host.prefix.length < MIN_TAB_PREFIX) {
    host.pass()
    return { kind: 'pass', reason: 'no-candidate' }
  }

  const [completions, symbols] = await Promise.all([
    settle(host.completions),
    settle(host.symbols)
  ])

  const candidates = toTabCandidates({
    prefix: host.prefix,
    completions,
    symbols,
    ...(host.currentPath === undefined ? {} : { currentPath: host.currentPath })
  })
  const decision = decideTabAction(candidates, host.prefix)

  if (decision.kind === 'insert') {
    host.insert(decision.insertText, replaceRangeFor(host.position, host.prefix), decision.candidate)
  } else {
    host.pass()
  }
  return decision
}
