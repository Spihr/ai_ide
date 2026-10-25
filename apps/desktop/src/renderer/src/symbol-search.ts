/**
 * 符号视图的纯逻辑：kind 的中文名 / 图标、按 kind 分组、分组后的扁平顺序。
 *
 * 为什么单独一个文件：`Ctrl+T` 快速选择器与搜索面板的「符号」模式必须**看起来是
 * 同一件事**——同一套分组顺序、同一套图标、同一个「第 N 行」。两处各写一遍分组，
 * 迟早出现「按 ↓ 选到第 3 行、回车却跳到第 5 个符号」这种只在键盘上才暴露的错位。
 *
 * 这里只有纯函数：取数在 `store.runSymbolSearch`，高亮状态在 `symbol-picker-store`，
 * 所以可以脱离 DOM 与内核直接单测。
 */

import type { Symbol, SymbolKind } from '@ai-ide/shared-types'

/**
 * 分组顺序。
 *
 * 按「调用得越频繁越靠前」排：用户按 Ctrl+T 时十有八九在找一个函数或方法，其次是
 * 类型定义，常量垫底。这个顺序**同时**是组内分组的顺序和扁平导航的顺序，两处共用
 * 一个常量才不会分叉。
 */
export const SYMBOL_KIND_ORDER: readonly SymbolKind[] = [
  'function',
  'method',
  'class',
  'interface',
  'struct',
  'trait',
  'enum',
  'const'
]

const SYMBOL_KIND_LABELS: Record<SymbolKind, string> = {
  function: '函数',
  method: '方法',
  class: '类',
  interface: '接口',
  struct: '结构体',
  trait: 'Trait',
  enum: '枚举',
  const: '常量'
}

/**
 * 每个 kind 的图标。
 *
 * 刻意用**单字符**而不是 emoji：等宽字体下宽度一致，列表不会因为图标宽度不同而
 * 左右参差；也避免 emoji 在 Windows 的某些字体回退里变成方块。
 */
const SYMBOL_KIND_GLYPHS: Record<SymbolKind, string> = {
  function: 'ƒ',
  method: 'ƒ',
  class: 'C',
  interface: 'I',
  struct: 'S',
  trait: 'T',
  enum: 'E',
  const: 'K'
}

/** 一个 kind 分组。 */
export interface SymbolGroup {
  kind: SymbolKind
  /** 中文名（界面上「函数 12」里的「函数」）。 */
  label: string
  /** 单字符图标。 */
  glyph: string
  /** 组内保持内核返回的相关度顺序。 */
  symbols: Symbol[]
}

/** 未知 kind 的兜底名次：排在所有已知 kind 之后，但仍然稳定有序。 */
export function symbolKindRank(kind: string): number {
  const index = SYMBOL_KIND_ORDER.indexOf(kind as SymbolKind)
  return index < 0 ? SYMBOL_KIND_ORDER.length : index
}

/** kind 的中文名；未知 kind 原样返回，不至于在界面上留空。 */
export function symbolKindLabel(kind: string): string {
  return SYMBOL_KIND_LABELS[kind as SymbolKind] ?? kind
}

/** kind 的单字符图标；未知 kind 给一个中性点。 */
export function symbolKindGlyph(kind: string): string {
  return SYMBOL_KIND_GLYPHS[kind as SymbolKind] ?? '·'
}

/**
 * 按 kind 分组。
 *
 * 组内不重排：内核已经按相关度排过，查询命中的位置（名字开头 / 中间包含）在组内
 * 仍然有意义。组间按 `SYMBOL_KIND_ORDER`；未知 kind 追加在末尾、按首次出现顺序
 * （`Map` 保序 + `sort` 稳定）。
 */
export function groupSymbols(symbols: readonly Symbol[]): SymbolGroup[] {
  const buckets = new Map<string, Symbol[]>()
  for (const symbol of symbols) {
    const bucket = buckets.get(symbol.kind)
    if (bucket) bucket.push(symbol)
    else buckets.set(symbol.kind, [symbol])
  }
  return [...buckets.entries()]
    .sort((a, b) => symbolKindRank(a[0]) - symbolKindRank(b[0]))
    .map(([kind, list]) => ({
      kind: kind as SymbolKind,
      label: symbolKindLabel(kind),
      glyph: symbolKindGlyph(kind),
      symbols: list
    }))
}

/** 分组后拍平的顺序——键盘导航的「第 N 行」以它为准。 */
export function visibleSymbols(symbols: readonly Symbol[]): Symbol[] {
  return groupSymbols(symbols).flatMap((group) => group.symbols)
}

/**
 * 位置的显示文案：`src/store.ts:42`。
 *
 * 用 `:行号` 而不是括号形式，跟编译器报错、`git grep -n` 的习惯一致，用户能把这段
 * 文案直接粘到终端里继续用。
 */
export function symbolLocationLabel(symbol: Symbol): string {
  return `${symbol.path}:${symbol.range.start_line}`
}

/** 行的显示文案（列表右侧那个 `:42`）。 */
export function symbolLineLabel(symbol: Symbol): string {
  return `:${symbol.range.start_line}`
}

/**
 * 行的唯一键与 DOM id：`path:line:name`。
 *
 * 为什么要三元组：同一个文件里可以有同名的重载函数 / 不同类里的同名方法，行号把它们
 * 区分开；而 `path:line` 又不足以在没有行号差异时（`const` 与同名函数理论上可同行）
 * 保证唯一，所以名字一起进来。React 的 `key` 与 `aria-activedescendant` 共用它。
 */
export function symbolKey(symbol: {
  name: string
  path: string
  range: { start_line: number }
}): string {
  return `${symbol.path}:${symbol.range.start_line}:${symbol.name}`
}
