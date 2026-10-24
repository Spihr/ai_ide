/**
 * 全局命令面板（FR-1.7）：`Ctrl/Cmd+Shift+P` 唤出，Esc 关闭，输入实时过滤，
 * ↑/↓ 选、回车执行，按「内置命令 / 插件命令」分组。
 *
 * 这个组件只负责**渲染与按键转发**：候选从 `command-palette-store` 的
 * `paletteGroups` 来，按键落到 `paletteActions`，执行分发（内置动作 vs
 * `plugin.command` IPC）与错误码文案都在 store 层，因此可以脱离 DOM 单测。
 *
 * 交互上两个刻意的选择：
 *
 * - **Esc 是两级的**：有查询词先清词，词空了才关面板——误按 Esc 不该丢掉刚输入的一半。
 * - **高亮是扁平下标**：分组的视觉顺序与扁平列表顺序一致（内置在前），所以只用
 *   一个下标就能表达「第几行」，不必在组间传 emoji 似的坐标。
 */

import { useEffect, useMemo, useRef } from 'react'

import { PLUGIN_COMMAND_RISK_LABELS } from '../command-registry'
import {
  paletteActions,
  paletteGroups,
  usePalette,
  visibleCommands
} from '../command-palette-store'

export function CommandPalette(): React.JSX.Element | null {
  const state = usePalette()
  const inputRef = useRef<HTMLInputElement | null>(null)
  const listRef = useRef<HTMLDivElement | null>(null)

  const visible = useMemo(() => visibleCommands(state), [state])
  const groups = useMemo(() => paletteGroups(state), [state])

  // 每次打开都把焦点抢到输入框：面板是「键盘进来、键盘出去」的东西，
  // 焦点留在编辑器里会让第一次输入落到文件上。
  useEffect(() => {
    if (state.open) inputRef.current?.focus()
  }, [state.open])

  // 高亮行滚进视野：翻到第 20 条还看不见，等于没有高亮。
  useEffect(() => {
    if (!state.open) return
    const active = listRef.current?.querySelector<HTMLElement>('[data-active="true"]')
    active?.scrollIntoView({ block: 'nearest' })
  }, [state.open, state.activeIndex, state.query])

  if (!state.open) return null

  const activeId = visible[state.activeIndex]?.id ?? null

  const onKeyDown = (event: React.KeyboardEvent<HTMLDivElement>): void => {
    if (event.key === 'ArrowDown') {
      event.preventDefault()
      paletteActions.moveActive(1)
    } else if (event.key === 'ArrowUp') {
      event.preventDefault()
      paletteActions.moveActive(-1)
    } else if (event.key === 'Enter') {
      event.preventDefault()
      void paletteActions.executeActive()
    } else if (event.key === 'Escape') {
      event.preventDefault()
      paletteActions.escape()
    }
  }

  let flatIndex = -1

  return (
    <div
      className="palette-overlay"
      role="presentation"
      // 点遮罩关面板（与 Esc 不同：这里不需要两段式，点击本身就表示「离开」）。
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) paletteActions.close()
      }}
    >
      <div
        className="palette"
        role="dialog"
        aria-modal="true"
        aria-label="命令面板"
        data-testid="command-palette"
        onKeyDown={onKeyDown}
      >
        <div className="palette__search">
          <span className="palette__glyph" aria-hidden="true">
            ⌘
          </span>
          <input
            ref={inputRef}
            className="palette__input"
            data-testid="command-palette-input"
            placeholder="输入命令名或关键词…（↑↓ 选择 · 回车执行 · Esc 关闭）"
            value={state.query}
            spellCheck={false}
            onChange={(event) => paletteActions.setQuery(event.target.value)}
            aria-label="搜索命令"
            aria-controls="command-palette-list"
            aria-activedescendant={activeId ?? undefined}
          />
          {state.loading ? <span className="palette__spinner">插件命令加载中…</span> : null}
        </div>

        {state.loadError !== null ? (
          <div className="palette__error" data-testid="command-palette-load-error">
            {state.loadError}
            <button
              type="button"
              className="palette__dismiss"
              onClick={() => paletteActions.dismissLoadError()}
            >
              知道了
            </button>
          </div>
        ) : null}

        <div className="palette__list" id="command-palette-list" role="listbox" ref={listRef}>
          {groups.length === 0 ? (
            <p className="palette__empty" data-testid="command-palette-empty">
              没有匹配的命令。换个关键词，或清空输入看全部。
            </p>
          ) : (
            groups.map((group) => (
              <div className="palette__group" key={group.section}>
                <div className="palette__group-title">
                  {group.label}
                  <span className="palette__group-count">{group.commands.length}</span>
                </div>
                {group.commands.map((command) => {
                  flatIndex += 1
                  const index = flatIndex
                  const active = command.id === activeId
                  return (
                    <button
                      type="button"
                      key={command.id}
                      id={command.id}
                      role="option"
                      aria-selected={active}
                      data-active={active ? 'true' : 'false'}
                      data-testid={`command-palette-item-${command.id}`}
                      className={`palette__item${active ? ' palette__item--active' : ''}`}
                      // onMouseDown 而不是 onClick：避免输入框先失焦带来的闪烁。
                      onMouseDown={(event) => {
                        event.preventDefault()
                        paletteActions.setActive(index)
                        void paletteActions.execute(command)
                      }}
                      onMouseEnter={() => paletteActions.setActive(index)}
                    >
                      <span className="palette__item-title">{command.title}</span>
                      {command.risk !== undefined ? (
                        <span className={`palette__tag palette__tag--${command.risk}`}>
                          {PLUGIN_COMMAND_RISK_LABELS[command.risk]}
                        </span>
                      ) : null}
                      <span className="palette__item-desc">{command.description}</span>
                    </button>
                  )
                })}
              </div>
            ))
          )}
        </div>

        <div className="palette__footer">
          <span>{visible.length} 条命令</span>
          <span className="palette__hint">
            内置 {state.builtin.length} · 插件 {state.plugin.length}
          </span>
        </div>
      </div>
    </div>
  )
}
