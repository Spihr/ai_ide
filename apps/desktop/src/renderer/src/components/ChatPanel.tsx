/**
 * 对话面板（M2 发送链路 + M3 外壳侧引用）。
 *
 * 「发送」只等 `llm.chat_stream` 的 `{stream_id}` 回执就返回，正文由 `llm.delta`
 * 逐段追加；「停止」发 `llm.cancel`。异常路径统一在气泡下方给可读提示
 * （错误码 → 人话），而不是把 `[2002] ...` 直接甩给用户。
 *
 * M3 加进来的是「输入框里怎么引用文件」：敲 `@` 弹补全（候选来自内核的
 * `workspace.tree` / `search.files`），选中后 chip 挂到输入框上方；编辑器里打开着的
 * 文件与选中的代码段也能一键变 chip（`+ 当前文件` / `+ 选区`，行号取自 Monaco 的真实选区）。
 * 发送时 chip 按顺序变成 `context.refs`；token 估算与超限提示都在 store 里，这里只负责显示。
 * 组件本身仍然只持有一个草稿字符串，没有引入新的状态库。
 */

import { useEffect, useRef, useState } from 'react'

import {
  acceptCompletion,
  addCurrentFileRef,
  addSelectionRef,
  cancelStream,
  clearChat,
  closeCompletion,
  moveCompletion,
  openCompletion,
  removeChip,
  selectModel,
  sendMessage,
  setCompletionActive,
  useAi
} from '../ai-store'
import {
  contextKey,
  findTrigger,
  stripTrigger,
  toRelativePath,
  type ContextChip
} from '../context-completion'
import { getEditorState, useEditorReference } from '../store'

/** chip 前缀图标：文件夹 `▸`、选区 `≡`、其余一律 `#`。 */
function chipIcon(chip: ContextChip): string {
  if (chip.kind === 'folder') return '▸'
  if (chip.kind === 'selection') return '≡'
  return '#'
}

/** chip 悬停文案：选区要把行号写出来——同一文件的两段选区只靠文件名分不清。 */
function chipTitle(chip: ContextChip): string {
  const range = chip.range ?? null
  if (chip.kind === 'selection' && range) {
    return `${chip.path} 第 ${range.startLine}-${range.endLine} 行`
  }
  return chip.path
}

export function ChatPanel(): React.JSX.Element {
  const state = useAi()
  // 只订阅「活动文件 + 选区」这一小块编辑器状态：打字不会让对话面板跟着重渲染。
  const reference = useEditorReference()
  const [draft, setDraft] = useState('')
  const bubblesRef = useRef<HTMLDivElement | null>(null)
  const textareaRef = useRef<HTMLTextAreaElement | null>(null)
  // 补全「接住」了这次的 `@`：紧随其后的 Enter 是「选中候选」而不是「发送」。
  // 只在事件处理里读写，用 ref 就够了，进 state 只会多一次无谓渲染。
  const acceptedAtRef = useRef(-1)

  // 新内容到达时贴住底部：流式追加时用户视线不该被甩开。
  useEffect(() => {
    const node = bubblesRef.current
    if (node) node.scrollTop = node.scrollHeight
  }, [state.bubbles])

  const modelValue = state.selectedModel
    ? `${state.selectedModel.provider_id}::${state.selectedModel.model_id}`
    : ''

  /** 工作区根；只用来把候选的绝对路径压成相对路径显示。 */
  const workspaceRoot = getEditorState().workspace?.root ?? null

  const displayPath = (path: string): string =>
    workspaceRoot === null ? path : toRelativePath(path, workspaceRoot)

  /** 只读当前光标位置：补全的开关不该依赖 Selection API 是否可用。 */
  const caretOf = (): number => {
    const node = textareaRef.current
    return node ? node.selectionStart : draft.length
  }

  const send = (): void => {
    if (state.streaming || state.selectedModel === null) return
    const text = draft
    setDraft('')
    acceptedAtRef.current = -1
    closeCompletion()
    void sendMessage(text)
  }

  /**
   * 确认当前候选：把草稿里的 `@查询` 换成 `@末段名 `。
   *
   * chip 已经把完整路径表示清楚了，正文里留一个可读的名字即可——既不丢信息，
   * 也不会让消息被一长串路径淹没。
   */
  const applyCompletion = (): void => {
    const caret = caretOf()
    // 点击候选时 DOM 里的光标可能还没落到新位置（caret 仍是 0），退回按末尾解析：
    // 菜单是从某次 `@` 弹出来的，末尾那次几乎总是同一个触发。
    const triggerAtCaret = findTrigger(draft, caret)
    const trigger = triggerAtCaret ?? (caret === 0 ? findTrigger(draft, draft.length) : null)
    if (!trigger) return
    const chip = acceptCompletion()
    if (!chip) return
    acceptedAtRef.current = trigger.start
    setDraft(`${stripTrigger(draft, trigger, caret)}@${chip.label} `)
  }

  const handleChange = (value: string, caret: number): void => {
    setDraft(value)
    // 上一次的 `@` 刚被换成 `@名字 `：这次变化是「应用补全」的结果，不是新触发。
    // 不拦一下，菜单会立刻弹回来，看着像卡住了。
    if (acceptedAtRef.current >= 0 && caret > acceptedAtRef.current) {
      acceptedAtRef.current = -1
      closeCompletion()
      return
    }
    acceptedAtRef.current = -1
    const next = findTrigger(value, caret)
    if (next) void openCompletion(next.query)
    else closeCompletion()
  }

  const handleKeyDown = (event: React.KeyboardEvent<HTMLTextAreaElement>): void => {
    const menu = state.completion
    // 菜单开着时方向键与 Enter / Tab 先归菜单用：否则「选候选」和「发出去」会打架。
    if (menu.open && menu.items.length > 0) {
      if (event.key === 'ArrowDown') {
        event.preventDefault()
        moveCompletion(1)
        return
      }
      if (event.key === 'ArrowUp') {
        event.preventDefault()
        moveCompletion(-1)
        return
      }
      if ((event.key === 'Enter' && !event.shiftKey) || event.key === 'Tab') {
        event.preventDefault()
        applyCompletion()
        return
      }
    }
    if (event.key === 'Escape') {
      closeCompletion()
      return
    }
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault()
      send()
    }
  }

  return (
    <div className="ai-section" data-testid="chat-panel">
      <h3 className="ai-section__title">对话</h3>

      <div className="ai-row">
        <select
          className="ai-input"
          data-testid="chat-model"
          value={modelValue}
          onChange={(event) => {
            const [providerId, modelIdPart] = event.target.value.split('::')
            selectModel(
              providerId && modelIdPart ? { provider_id: providerId, model_id: modelIdPart } : null
            )
          }}
        >
          <option value="">（未选择模型）</option>
          {state.models.map((model) => (
            <option
              key={model.model_pk}
              value={`${model.provider_id}::${model.model_id}`}
            >
              {model.display_name ?? model.model_id}
            </option>
          ))}
        </select>
        <button className="ai-button" type="button" data-testid="chat-clear" onClick={clearChat}>
          清空
        </button>
      </div>

      <div className="ai-bubbles" data-testid="chat-bubbles" ref={bubblesRef}>
        {state.bubbles.length === 0 ? (
          <div className="ai-note">还没有消息。选好模型后输入内容发送。</div>
        ) : (
          state.bubbles.map((bubble, index) => (
            <div
              key={`${bubble.role}-${index}`}
              className={`ai-bubble ai-bubble--${bubble.role}${bubble.status === 'error' ? ' ai-bubble--error' : ''}`}
            >
              <div className="ai-bubble__role">{bubble.role === 'user' ? '我' : '模型'}</div>
              <div className="ai-bubble__text">{bubble.text}</div>
              {bubble.status === 'streaming' ? <div className="ai-bubble__meta">生成中…</div> : null}
              {bubble.note ? <div className="ai-bubble__meta">{bubble.note}</div> : null}
            </div>
          ))
        )}
      </div>

      {state.hint !== null ? (
        <div className="ai-result ai-result--bad" data-testid="chat-hint">
          {state.hint}
        </div>
      ) : null}

      {state.usage !== null ? (
        <div className="ai-note" data-testid="chat-usage">
          用量：输入 {state.usage.promptTokens} / 输出 {state.usage.completionTokens}
          {state.finishReason !== null ? ` · 结束原因 ${state.finishReason}` : ''}
        </div>
      ) : null}

      {state.tokenEstimate !== null && state.tokenEstimate.hint !== null ? (
        <div className="ai-result ai-result--bad" data-testid="chat-token-hint">
          {state.tokenEstimate.hint}
        </div>
      ) : null}

      <div className="ai-refs" data-testid="chat-ref-actions">
        <button
          className="ai-button"
          type="button"
          data-testid="chat-add-current-file"
          disabled={reference.path === null}
          title={
            reference.path === null
              ? '先在编辑器里打开一个文件'
              : `引用当前文件：${displayPath(reference.path)}`
          }
          onClick={() => addCurrentFileRef()}
        >
          + 当前文件
        </button>
        <button
          className="ai-button"
          type="button"
          data-testid="chat-add-selection"
          disabled={reference.selection === null}
          title={
            reference.selection === null
              ? '先在编辑器里选中一段代码'
              : `引用选区：${displayPath(reference.selection.path)} 第 ${reference.selection.startLine}-${reference.selection.endLine} 行`
          }
          onClick={() => addSelectionRef()}
        >
          + 选区
        </button>
      </div>

      {state.chips.length > 0 ? (
        <div className="ai-chips" data-testid="chat-chips">
          {state.chips.map((chip) => (
            <span
              className="ai-chip"
              key={contextKey(chip.kind, chip.path, chip.range ?? null)}
              title={chipTitle(chip)}
              data-testid="chat-chip"
            >
              <span className="ai-chip__kind" aria-hidden="true">
                {chipIcon(chip)}
              </span>
              <span className="ai-chip__label">{chip.label}</span>
              <button
                className="ai-chip__remove"
                type="button"
                data-testid="chat-chip-remove"
                aria-label={`移除引用 ${chipTitle(chip)}`}
                title={`移除引用 ${chipTitle(chip)}`}
                onClick={() => removeChip(chip.kind, chip.path, chip.range ?? null)}
              >
                ×
              </button>
            </span>
          ))}
        </div>
      ) : null}

      <div className="ai-compose">
        <textarea
          ref={textareaRef}
          className="ai-input ai-textarea"
          data-testid="chat-input"
          value={draft}
          placeholder="说点什么…（@ 引用文件或文件夹，也可用下方按钮引用当前文件 / 选区；Enter 发送，Shift+Enter 换行）"
          onChange={(event) => handleChange(event.target.value, event.currentTarget.selectionStart)}
          onKeyDown={handleKeyDown}
          onClick={() => closeCompletion()}
          onBlur={() => closeCompletion()}
        />

        {state.completion.open ? (
          <div className="ai-completion" data-testid="chat-completion" role="listbox">
            {state.completion.loading && state.completion.items.length === 0 ? (
              <div className="ai-completion__note">正在查找…</div>
            ) : null}
            {state.completion.error !== null ? (
              <div className="ai-completion__note">{state.completion.error}</div>
            ) : null}
            {!state.completion.loading &&
            state.completion.error === null &&
            state.completion.items.length === 0 ? (
              <div className="ai-completion__note">没有匹配的文件或文件夹。</div>
            ) : null}
            {state.completion.items.map((item, index) => (
              <button
                key={`${item.kind}:${item.path}`}
                className={`ai-completion__item${
                  index === state.completion.activeIndex ? ' ai-completion__item--active' : ''
                }`}
                type="button"
                role="option"
                aria-selected={index === state.completion.activeIndex}
                data-testid="chat-completion-item"
                title={item.path}
                onMouseEnter={() => setCompletionActive(index)}
                // 用 mousedown 而不是 click：click 之前 textarea 会先 blur，菜单那时已经关掉了。
                onMouseDown={(event) => {
                  event.preventDefault()
                  applyCompletion()
                }}
              >
                <span className="ai-completion__kind">
                  {item.kind === 'folder' ? '文件夹' : '文件'}
                </span>
                <span className="ai-completion__path">{displayPath(item.path)}</span>
              </button>
            ))}
          </div>
        ) : null}
      </div>

      <div className="ai-row">
        <button
          className="ai-button ai-button--primary"
          type="button"
          data-testid="chat-send"
          disabled={state.streaming || state.selectedModel === null}
          onClick={send}
        >
          发送
        </button>
        <button
          className="ai-button"
          type="button"
          data-testid="chat-stop"
          disabled={!state.streaming}
          onClick={() => void cancelStream()}
        >
          停止
        </button>
      </div>
    </div>
  )
}
