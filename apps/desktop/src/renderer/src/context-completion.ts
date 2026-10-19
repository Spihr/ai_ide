/**
 * M3 外壳侧：`@` 引用的纯逻辑（触发解析、候选过滤排序、请求体构造、超限提示）。
 *
 * 这里刻意不碰 IPC 与 React：补全怎么触发、候选怎么筛、chip 怎么变成
 * `context.refs`、超限那句话怎么说，全都是「输入 → 输出」的纯函数，能被单测直接钉死；
 * 真正的内核调用（`search.files` / `workspace.tree` / `llm.count_tokens`）在 `ai-store.ts`。
 */

import type { ChatContext, ContextRef, ContextRefKind } from '@ai-ide/shared-types'

/** 本轮只支持文件与文件夹两类引用（选区 / 当前文件留后续切片）。 */
export type ChipKind = Extract<ContextRefKind, 'file' | 'folder'>

/** 一条已选中的引用（UI 上的 chip）。 */
export interface ContextChip {
  kind: ChipKind
  /** 相对工作区根、以 `/` 分隔的路径——内核按工作区根展开，别塞绝对路径。 */
  path: string
  /** chip 文案；只用于界面，不注入对话内容。 */
  label: string
}

/** 补全候选。 */
export interface CompletionItem {
  kind: ChipKind
  /** 相对工作区根、以 `/` 分隔。 */
  path: string
  label: string
}

/** 一次 `@` 触发：`@` 在文本中的下标，以及它后面已经敲入的查询串。 */
export interface TriggerInfo {
  start: number
  query: string
}

/** 发送前的 token 估算结果。 */
export interface TokenEstimate {
  tokens: number
  /** 模型窗口；模型未声明时为 `null`（此时不判定超限）。 */
  limit: number | null
  exceeded: boolean
  /** 超限时给用户看的整句提示；未超限时为 `null`。 */
  hint: string | null
}

export interface RankOptions {
  /** 已经在 chip 里的引用键（见 `contextKey`），不再重复推荐。 */
  exclude?: string[]
  limit?: number
}

/** 补全菜单一次最多展示几条：太长会盖住输入框，用户也不会往下翻。 */
export const MAX_COMPLETIONS = 8

/** 统一成正斜杠，并压掉 `//`——内核两种分隔符都认，但 chip 与候选要同一形状才能去重。 */
export function normalizePath(path: string): string {
  return path.replace(/\\/g, '/').replace(/\/{2,}/g, '/').replace(/^\.\//, '')
}

/** 一条引用的稳定键，用于去重与「已选过的别再出现在候选里」。 */
export function contextKey(kind: ChipKind, path: string): string {
  return `${kind}:${normalizePath(path)}`.toLowerCase()
}

/** 绝对路径 → 相对工作区根的路径；落在工作区外时原样返回（交给内核报错）。 */
export function toRelativePath(absPath: string, root: string): string {
  const target = normalizePath(absPath)
  const base = normalizePath(root).replace(/\/+$/, '')
  if (base.length === 0) return target
  // Windows 路径大小写不敏感，前缀比较统一折叠后再判断。
  const folded = target.toLowerCase()
  const foldedBase = base.toLowerCase()
  if (folded === foldedBase) return ''
  return folded.startsWith(`${foldedBase}/`) ? target.slice(base.length + 1) : target
}

export function baseNameOf(path: string): string {
  const norm = normalizePath(path)
  return norm.slice(norm.lastIndexOf('/') + 1)
}

/** chip 文案：默认取末段名（`src/main.ts` → `main.ts`）。 */
export function chipLabel(path: string): string {
  return baseNameOf(path)
}

/**
 * 从光标位置向左找最近的 `@`，判断它是否构成一次补全触发。
 *
 * 三条否决规则都来自「别在正常句子里乱弹菜单」：
 * 1. `@` 前面紧挨着非空白字符（`a@b`、邮箱）——不算触发；
 * 2. `@` 与光标之间出现空白或第二个 `@`——用户已经在写句子，不是在选文件；
 * 3. 没有 `@`——直接不触发。
 */
export function findTrigger(text: string, caret: number): TriggerInfo | null {
  const end = Math.max(0, Math.min(caret, text.length))
  const start = text.lastIndexOf('@', end - 1)
  if (start < 0) return null
  const before = start > 0 ? (text[start - 1] ?? '') : ''
  if (before !== '' && !/\s/.test(before)) return null
  const query = text.slice(start + 1, end)
  if (/[\s@]/.test(query)) return null
  return { start, query }
}

/** 把这次触发的 `@查询` 从文本里摘掉（chip 已经表达了引用，文本里留一份是重复）。 */
export function stripTrigger(text: string, trigger: TriggerInfo, caret: number): string {
  const end = Math.max(0, Math.min(caret, text.length))
  return text.slice(0, trigger.start) + text.slice(end)
}

/** 单个候选的匹配得分：末段名优先于整条路径，0 分表示不相关（会被丢掉）。 */
function scoreOf(item: CompletionItem, query: string): number {
  if (query.length === 0) return 0
  const needle = query.toLowerCase()
  const name = baseNameOf(item.path).toLowerCase()
  const path = item.path.toLowerCase()
  if (name === needle) return 4
  if (name.startsWith(needle)) return 3
  if (name.includes(needle)) return 2
  if (path.includes(needle)) return 1
  return 0
}

/**
 * 候选过滤 + 排序：`query` 是 `@` 后面那截（**不含目录前缀**，目录前缀已经体现在
 * 候选的 `path` 里，再拿它参与打分会让所有候选同分）。同分时目录排在文件前，
 * 因为「引用整个目录」通常是更粗的第一步。
 */
export function rankCompletions(
  items: CompletionItem[],
  query: string,
  options: RankOptions = {}
): CompletionItem[] {
  const exclude = new Set((options.exclude ?? []).map((key) => key.toLowerCase()))
  const limit = options.limit ?? MAX_COMPLETIONS
  const seen = new Set<string>()
  const ranked: { item: CompletionItem; score: number }[] = []

  for (const raw of items) {
    const path = normalizePath(raw.path)
    const item: CompletionItem = {
      kind: raw.kind,
      path,
      label: raw.label.length > 0 ? raw.label : chipLabel(path)
    }
    const key = contextKey(item.kind, path)
    if (seen.has(key) || exclude.has(key)) continue
    const score = scoreOf(item, query)
    if (query.length > 0 && score <= 0) continue
    seen.add(key)
    ranked.push({ item, score })
  }

  ranked.sort((a, b) => {
    if (a.score !== b.score) return b.score - a.score
    const aRank = a.item.kind === 'folder' ? 0 : 1
    const bRank = b.item.kind === 'folder' ? 0 : 1
    if (aRank !== bRank) return aRank - bRank
    return a.item.path.localeCompare(b.item.path, 'zh-Hans-CN')
  })

  return ranked.slice(0, Math.max(0, limit)).map((entry) => entry.item)
}

/**
 * chip 按顺序变成内核要的 `context.refs`。**数组顺序即注入顺序**，所以这里不排序、
 * 不去重（去重发生在 `addChip`，那时才知道用户是不是重复选）。
 */
export function buildContextRefs(chips: ContextChip[]): ContextRef[] {
  return chips.map((chip) => ({
    kind: chip.kind,
    path: normalizePath(chip.path),
    // 行区间与选区原文属「选区 / 当前文件」切片，本轮一律为 null（整文件注入）。
    start_line: null,
    end_line: null,
    text: null,
    label: chip.label
  }))
}

/** 组装 `llm.chat_stream` 的 `context`；没有引用时返回 `null`（与 M2 行为一致）。 */
export function buildChatContext(
  chips: ContextChip[],
  workspaceId: string | null
): ChatContext | null {
  if (chips.length === 0) return null
  return { workspace_id: workspaceId, refs: buildContextRefs(chips), compress: null }
}

/** 千分位：12,345 比 12345 更容易一眼看出量级。 */
function withThousands(value: number): string {
  return String(Math.round(value)).replace(/\B(?=(\d{3})+(?!\d))/g, ',')
}

/** 是否超出窗口。模型没声明窗口（`limit === null`）时无从判断，一律视为不超限。 */
export function estimateExceeded(tokens: number, limit: number | null): boolean {
  return limit !== null && Number.isFinite(limit) && limit > 0 && tokens > limit
}

/**
 * 超限提示；没超限（或模型没声明窗口）时返回 `null`——估算值本身不需要打扰用户，
 * 只有「这次发出去大概会被上游按窗口打回」这一件事值得提前说。
 */
export function formatTokenHint(input: {
  tokens: number
  limit: number | null
  label: string | null
}): string | null {
  if (!estimateExceeded(input.tokens, input.limit)) return null
  const limit = input.limit ?? 0
  const who = input.label !== null && input.label.length > 0 ? `「${input.label}」` : '当前模型'
  return (
    `估算输入约 ${withThousands(input.tokens)} tokens，超出${who}的上下文窗口 ` +
    `${withThousands(limit)}：请减少引用或缩短消息后再发送。`
  )
}

export function makeTokenEstimate(
  tokens: number,
  limit: number | null,
  label: string | null
): TokenEstimate {
  return {
    tokens,
    limit,
    exceeded: estimateExceeded(tokens, limit),
    hint: formatTokenHint({ tokens, limit, label })
  }
}
