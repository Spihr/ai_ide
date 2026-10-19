/**
 * 对话面板（M2 外壳侧）。
 *
 * 「发送」只等 `llm.chat_stream` 的 `{stream_id}` 回执就返回，正文由 `llm.delta`
 * 逐段追加；「停止」发 `llm.cancel`。异常路径统一在气泡下方给可读提示
 * （错误码 → 人话），而不是把 `[2002] ...` 直接甩给用户。
 */

import { useEffect, useRef, useState } from 'react'

import { cancelStream, clearChat, selectModel, sendMessage, useAi } from '../ai-store'

export function ChatPanel(): React.JSX.Element {
  const state = useAi()
  const [draft, setDraft] = useState('')
  const bubblesRef = useRef<HTMLDivElement | null>(null)

  // 新内容到达时贴住底部：流式追加时用户视线不该被甩开。
  useEffect(() => {
    const node = bubblesRef.current
    if (node) node.scrollTop = node.scrollHeight
  }, [state.bubbles])

  const modelValue = state.selectedModel
    ? `${state.selectedModel.provider_id}::${state.selectedModel.model_id}`
    : ''

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

      <div className="ai-row ai-row--compose">
        <textarea
          className="ai-input ai-textarea"
          data-testid="chat-input"
          value={draft}
          placeholder="说点什么…（Enter 发送，Shift+Enter 换行）"
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === 'Enter' && !event.shiftKey) {
              event.preventDefault()
              const text = draft
              setDraft('')
              void sendMessage(text)
            }
          }}
        />
      </div>

      <div className="ai-row">
        <button
          className="ai-button ai-button--primary"
          type="button"
          data-testid="chat-send"
          disabled={state.streaming || state.selectedModel === null}
          onClick={() => {
            const text = draft
            setDraft('')
            void sendMessage(text)
          }}
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
