/**
 * 「转到符号」快速选择器（`Ctrl+T`）：输入实时过滤，按 kind 分组，↑/↓ 选择、回车跳转。
 *
 * 这个组件只做**渲染与按键转发**——候选来自 `symbol-picker-store` 的分组与扁平列表，
 * 按键落到 `symbolPickerActions`，跳转（打开文件 + 定位到 `range.start_line`）在
 * store 层，因此可以脱离 DOM 单测。
 *
 * 三个刻意的选择：
 *
 * - **复用命令面板的骨架样式**（`palette*`）：两个浮层在同一个位置、同样的宽度与
 *   阴影，用户不该感觉到「换了个控件」。差异只有内容：这里是符号而不是命令。
 * - **高亮是扁平下标**：分组的视觉顺序与扁平顺序一致，一个下标就能表达「第几行」。
 * - **打开即列出全部符号**：空 query 直接列出内核返回的前 200 条，省掉「先打字」。
 */

import { useEffect, useMemo, useRef } from 'react'

import { useEditor } from '../store'
import { activeSymbol, symbolPickerActions, useSymbolPicker } from '../symbol-picker-store'
import {
  groupSymbols,
  symbolKey,
  symbolLineLabel,
  symbolLocationLabel,
  visibleSymbols
} from '../symbol-search'

export function SymbolPicker(): React.JSX.Element | null {
  const state = useSymbolPicker()
  const editor = useEditor()
  const inputRef = useRef<HTMLInputElement | null>(null)
  const listRef = useRef<HTMLDivElement | null>(null)

  // 分组与扁平顺序都从同一份 `symbolMatches` 推出来：分组给渲染，扁平给 ↑↓ 与回车。
  const groups = useMemo(() => groupSymbols(editor.symbolMatches), [editor.symbolMatches])
  const rows = useMemo(() => visibleSymbols(editor.symbolMatches), [editor.symbolMatches])

  // 打开就把焦点抢到输入框：浮层是「键盘进来、键盘出去」的东西。
  useEffect(() => {
    if (state.open) inputRef.current?.focus()
  }, [state.open])

  // 高亮行滚进视野（翻到第 20 条还看不见等于没有高亮）。
  useEffect(() => {
    if (!state.open) return
    const active = listRef.current?.querySelector<HTMLElement>('[data-active="true"]')
    active?.scrollIntoView({ block: 'nearest' })
  }, [state.open, state.activeIndex, state.query, editor.symbolMatches])

  if (!state.open) return null

  const active = activeSymbol()
  const activeKey = active ? symbolKey(active) : undefined

  const onKeyDown = (event: React.KeyboardEvent<HTMLDivElement>): void => {
    if (event.key === 'ArrowDown') {
      event.preventDefault()
      symbolPickerActions.moveActive(1)
    } else if (event.key === 'ArrowUp') {
      event.preventDefault()
      symbolPickerActions.moveActive(-1)
    } else if (event.key === 'Enter') {
      event.preventDefault()
      void symbolPickerActions.executeActive()
    } else if (event.key === 'Escape') {
      event.preventDefault()
      symbolPickerActions.escape()
    }
  }

  let flatIndex = -1

  const emptyText = ((): string => {
    if (editor.symbolSearching) return '检索中…'
    if (editor.workspace === null) return '先打开一个工作区，再来这里找符号。'
    if (state.query.trim() !== '') return '没有匹配的符号。换个名字试试，或清空输入看全部。'
    return '这个工作区里还没抽到符号（可能需要先让索引跑完）。'
  })()

  return (
    <div
      className="palette-overlay"
      role="presentation"
      data-testid="symbol-picker-overlay"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) symbolPickerActions.close()
      }}
    >
      <div
        className="palette symbol-picker"
        role="dialog"
        aria-modal="true"
        aria-label="转到符号"
        data-testid="symbol-picker"
        onKeyDown={onKeyDown}
      >
        <div className="palette__search">
          <span className="palette__glyph" aria-hidden="true">
            ⌗
          </span>
          <input
            ref={inputRef}
            className="palette__input"
            data-testid="symbol-picker-input"
            placeholder="按名字查找符号…（↑↓ 选择 · 回车跳转 · Esc 关闭）"
            value={state.query}
            spellCheck={false}
            onChange={(event) => symbolPickerActions.setQuery(event.target.value)}
            aria-label="搜索符号"
            aria-controls="symbol-picker-list"
            aria-activedescendant={activeKey}
          />
          {editor.symbolSearching ? <span className="palette__spinner">检索中…</span> : null}
        </div>

        {editor.symbolError !== null ? (
          <div className="palette__error" data-testid="symbol-picker-error">
            {editor.symbolError}
          </div>
        ) : null}

        <div className="palette__list" id="symbol-picker-list" role="listbox" ref={listRef}>
          {groups.length === 0 ? (
            <p className="palette__empty" data-testid="symbol-picker-empty">
              {emptyText}
            </p>
          ) : (
            groups.map((group) => (
              <div className="palette__group" key={group.kind}>
                <div className="palette__group-title">
                  <span className="symbol-picker__glyph" aria-hidden="true">
                    {group.glyph}
                  </span>
                  {group.label}
                  <span className="palette__group-count">{group.symbols.length}</span>
                </div>
                {group.symbols.map((symbol) => {
                  flatIndex += 1
                  const index = flatIndex
                  const key = symbolKey(symbol)
                  const isActive = key === activeKey
                  return (
                    <button
                      type="button"
                      key={key}
                      id={key}
                      role="option"
                      aria-selected={isActive}
                      data-active={isActive ? 'true' : 'false'}
                      data-testid={`symbol-picker-item-${key}`}
                      className={`palette__item${isActive ? ' palette__item--active' : ''}`}
                      // onMouseDown 而非 onClick：避免输入框先失焦带来的闪烁。
                      onMouseDown={(event) => {
                        event.preventDefault()
                        symbolPickerActions.setActive(index)
                        void symbolPickerActions.execute(symbol)
                      }}
                      onMouseEnter={() => symbolPickerActions.setActive(index)}
                    >
                      <span className="palette__item-title">{symbol.name}</span>
                      <span className="symbol-picker__line">{symbolLineLabel(symbol)}</span>
                      <span className="palette__item-desc">{symbolLocationLabel(symbol)}</span>
                    </button>
                  )
                })}
              </div>
            ))
          )}
        </div>

        <div className="palette__footer">
          <span>{rows.length} 个符号</span>
          <span className="palette__hint">↑↓ 选择 · 回车跳转 · Esc 关闭</span>
        </div>
      </div>
    </div>
  )
}
