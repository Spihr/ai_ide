/**
 * 「转到符号」快速选择器（`Ctrl+T`）的状态机。
 *
 * 取数**不在**本文件：符号检索的请求与结果都落在 `store.ts`（`runSymbolSearch` /
 * `symbolMatches`）。浮层与搜索面板的「符号」模式因此看的是同一份结果——同一份
 * 结果 + 同一套分组（`symbol-search.ts`），两处才可能对得上；否则「浮层里第 3 行」
 * 和「面板里第 3 行」会是两个符号。
 *
 * 与命令面板对齐的四个选择：
 *
 * - **扁平下标导航**：分组只影响视觉，`activeIndex` 始终是扁平顺序的下标。
 * - **Esc 两级**：有查询词先清词并重新列出全部符号，词空了才关。
 * - **错误不吞**：检索失败留在 store 的 `symbolError` 上，浮层里照常显示。
 * - **回车先关再跳**：跳转会打开文件、切标签，浮层留着会压在编辑器上面。
 */

import { useSyncExternalStore } from 'react'

import type { Symbol } from '@ai-ide/shared-types'

import { moveActiveIndex, normalizeActiveIndex } from './command-registry'
import { getEditorState, openSymbol, runSymbolSearch } from './store'
import { groupSymbols, visibleSymbols, type SymbolGroup } from './symbol-search'

export interface SymbolPickerState {
  open: boolean
  query: string
  /** 扁平列表里的下标；没有候选时没有意义（读取一律走 `activeSymbol`）。 */
  activeIndex: number
}

function initialState(): SymbolPickerState {
  return { open: false, query: '', activeIndex: 0 }
}

let state: SymbolPickerState = initialState()

const listeners = new Set<() => void>()

function setState(patch: Partial<SymbolPickerState>): void {
  state = { ...state, ...patch }
  for (const listener of listeners) listener()
}

export function getSymbolPickerState(): SymbolPickerState {
  return state
}

export function subscribeSymbolPicker(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function useSymbolPicker(): SymbolPickerState {
  return useSyncExternalStore(subscribeSymbolPicker, getSymbolPickerState, getSymbolPickerState)
}

/** 测试用：模块级单例跨用例不清就会串状态。 */
export function resetSymbolPickerState(): void {
  cancelScheduled()
  state = initialState()
  for (const listener of listeners) listener()
}

/**
 * 防抖窗口。
 *
 * 比搜索面板的 250ms 短：`Ctrl+T` 是「一边打字一边看结果」的场景，长防抖会让人
 * 觉得按键丢帧；而内核侧符号检索是内存里的轻量扫描（不是全库读盘），120ms 足够
 * 把连打合并掉。
 */
export const SYMBOL_SEARCH_DEBOUNCE_MS = 120

let debounceTimer: ReturnType<typeof setTimeout> | null = null

function cancelScheduled(): void {
  if (debounceTimer !== null) {
    clearTimeout(debounceTimer)
    debounceTimer = null
  }
}

function schedule(run: () => void): void {
  cancelScheduled()
  debounceTimer = setTimeout(() => {
    debounceTimer = null
    run()
  }, SYMBOL_SEARCH_DEBOUNCE_MS)
}

/** 当前要渲染的分组（来自 store 里最后一次检索的结果）。 */
export function symbolPickerGroups(): SymbolGroup[] {
  return groupSymbols(getEditorState().symbolMatches)
}

/** 扁平列表——键盘导航与「第 N 行」的唯一口径。 */
export function symbolPickerRows(): Symbol[] {
  return visibleSymbols(getEditorState().symbolMatches)
}

/** 当前高亮的符号（没有候选时为 `null`）。 */
export function activeSymbol(): Symbol | null {
  const rows = symbolPickerRows()
  const index = normalizeActiveIndex(state.activeIndex, rows.length)
  return index < 0 ? null : rows[index] ?? null
}

/**
 * 唤出浮层。
 *
 * 打开即用空 query 拉一次全部符号：选择器空着等用户打字，等于让用户先猜「这里能搜
 * 什么」。空 query 在内核侧是「列全部」（上限 200 条），正是快速浏览符号表的效果。
 */
export function openSymbolPicker(): void {
  cancelScheduled()
  setState({ open: true, query: '', activeIndex: 0 })
  void runSymbolSearch('', { allowEmpty: true })
}

export function closeSymbolPicker(): void {
  cancelScheduled()
  setState({ open: false, query: '', activeIndex: 0 })
}

export function toggleSymbolPicker(): void {
  if (state.open) closeSymbolPicker()
  else openSymbolPicker()
}

/** Esc：有词先清词（并重新列出全部符号），词空了才关。 */
export function escapeSymbolPicker(): void {
  if (state.query.length > 0) {
    setState({ query: '', activeIndex: 0 })
    void runSymbolSearch('', { allowEmpty: true })
    return
  }
  closeSymbolPicker()
}

/**
 * 输入变化：立刻写进面板状态（输入框受控，不能等防抖），检索走防抖。
 *
 * 空输入也走防抖后的 `runSymbolSearch('')`，但**不**带 `allowEmpty`——浮层刚打开时
 * 已经列过全部，用户删空输入就等于回到那次列表，不必再扫一遍。
 */
export function setSymbolPickerQuery(query: string): void {
  setState({ query, activeIndex: 0 })
  schedule(() => void runSymbolSearch(query))
}

export function setSymbolPickerActive(index: number): void {
  setState({ activeIndex: normalizeActiveIndex(index, symbolPickerRows().length) })
}

/** ↑ / ↓：越界回绕，空列表时归 0。 */
export function moveSymbolPickerActive(delta: number): void {
  const total = symbolPickerRows().length
  const next = moveActiveIndex(state.activeIndex, delta, total)
  setState({ activeIndex: next < 0 ? 0 : next })
}

/** 选中某个符号：先关浮层，再打开文件并定位到 `range.start_line`。 */
export async function executeSymbol(symbol: Symbol): Promise<void> {
  closeSymbolPicker()
  await openSymbol(symbol)
}

/** 回车：跳转到当前高亮行。没有候选时什么都不做（也不关浮层，用户可能还要改词）。 */
export async function executeActiveSymbol(): Promise<void> {
  const symbol = activeSymbol()
  if (!symbol) return
  await executeSymbol(symbol)
}

/** 供组件直接调用的动作集合。 */
export const symbolPickerActions = {
  open: openSymbolPicker,
  close: closeSymbolPicker,
  toggle: toggleSymbolPicker,
  escape: escapeSymbolPicker,
  setQuery: setSymbolPickerQuery,
  setActive: setSymbolPickerActive,
  moveActive: moveSymbolPickerActive,
  execute: executeSymbol,
  executeActive: executeActiveSymbol
}
