/**
 * 内联编辑浮层（`Ctrl+K`）：选区 + 一句指令 → 内核回建议 → diff 预览 → 接受/拒绝。
 *
 * 三个阶段共用一块遮罩，视觉骨架沿用 `.palette*`（与命令面板、符号选择器同一位置），
 * 只有「预览」阶段换成 Monaco 的 diff 编辑器——用户要看的是**代码差异**，一段
 * `<pre>` 里的 `+` / `-` 手绘文本既不可读也不可信（缩进、折行全是自己猜的）。
 *
 * 刻意的几个选择：
 *
 * - **组件不写盘、也不直接调 IPC**：所有动作都经 `inlineEditActions` 走
 *   `inline-edit-store`。这样「接受才写、拒绝不写」这条关键约束只存在于一处，
 *   组件里没有任何一条能绕过它的路径。
 * - **错误留在浮层里**：9001 / 9003 是「改一下再来」的即时反馈，浮层不关，用户能
 *   边看提示边改指令。notice 是几秒后飘走的东西，用来报「已写回」这种结果。
 * - **diff 只比选区**：内核回的 `original` 是选区原文、不是整份文件，所以 diff 展示
 *   的也是「这段 → 那段」。整份文件的差异对「改这五行」这件事没有信息量。
 * - **只在预览阶段建 diff**：Monaco 的 diff 编辑器是重量级对象，输入阶段就建会
 *   让按 `Ctrl+K` 之后的第一帧明显卡一下。
 */

import { useEffect, useRef } from 'react'

import { languageForPath, monaco } from '../monaco-setup'
import {
  inlineEditActions,
  useInlineEdit,
  type InlineEditState
} from '../inline-edit-store'
import { inlineEditRangeLabel } from '../inline-edit'

function phaseLabel(state: InlineEditState): string {
  switch (state.phase) {
    case 'input':
      return '回车生成建议'
    case 'loading':
      return '正在生成…'
    case 'preview':
      return '确认后才会写回文件'
    default:
      return '正在写回…'
  }
}

export function InlineEditOverlay(): React.JSX.Element | null {
  const state = useInlineEdit()
  const inputRef = useRef<HTMLTextAreaElement | null>(null)
  const diffHostRef = useRef<HTMLDivElement | null>(null)

  // 打开就把焦点给输入框：这是「键盘进来、键盘出去」的浮层。
  useEffect(() => {
    if (state.open) inputRef.current?.focus()
  }, [state.open])

  // 预览阶段才建 diff 编辑器；proposal 一变就整体重建（模型不能复用，语言也可能不同）。
  useEffect(() => {
    const host = diffHostRef.current
    const proposal = state.proposal
    if (!state.open || state.phase !== 'preview' || proposal === null || host === null) return undefined

    const language = languageForPath(state.path ?? '')
    const diff = monaco.editor.createDiffEditor(host, {
      automaticLayout: true,
      readOnly: true,
      renderSideBySide: true,
      // 两行以内的小改动并排看反而更累，交给 Monaco 自己按宽度决定。
      renderOverviewRuler: false,
      minimap: { enabled: false },
      fontSize: 12,
      scrollBeyondLastLine: false,
      originalEditable: false
    })
    const original = monaco.editor.createModel(proposal.original, language)
    const modified = monaco.editor.createModel(proposal.replacement, language)
    diff.setModel({ original, modified })

    return () => {
      // 先拆模型再 dispose：diff 编辑器会持有两个模型的引用，顺序反了会报
      // "model is disposed" 之类的噪音。
      diff.setModel(null)
      diff.dispose()
      original.dispose()
      modified.dispose()
    }
  }, [state.open, state.phase, state.proposal, state.path])

  if (!state.open) return null

  const busy = state.phase === 'loading' || state.phase === 'saving'
  const preview = state.phase === 'preview' && state.proposal !== null

  const onKeyDown = (event: React.KeyboardEvent<HTMLDivElement>): void => {
    if (event.key === 'Escape') {
      event.preventDefault()
      // 输入阶段关掉就等于放弃；预览阶段关掉同样不写盘（拒绝与 Esc 同一条路）。
      inlineEditActions.reject()
      return
    }
    if (event.key !== 'Enter') return
    // 输入框里 Enter 直接生成，Shift+Enter 换行；预览阶段 Enter 不当作接受——
    // 写盘是不可撤销的副作用，必须落在按钮（或 Ctrl+Enter）上。
    if (state.phase === 'input' && !event.shiftKey) {
      event.preventDefault()
      void inlineEditActions.submit()
      return
    }
    if (state.phase === 'preview' && event.ctrlKey) {
      event.preventDefault()
      void inlineEditActions.accept()
    }
  }

  return (
    <div
      className="palette-overlay"
      role="presentation"
      data-testid="inline-edit-overlay"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) inlineEditActions.reject()
      }}
    >
      <div
        className="palette inline-edit"
        role="dialog"
        aria-modal="true"
        aria-label="内联编辑"
        data-testid="inline-edit"
        onKeyDown={onKeyDown}
      >
        <div className="inline-edit__header">
          <span className="inline-edit__glyph" aria-hidden="true">
            ⌁
          </span>
          <span className="inline-edit__target" data-testid="inline-edit-target">
            {state.relativePath ?? ''}
            {state.proposal === null
              ? ''
              : ` · ${inlineEditRangeLabel(state.proposal.range)}`}
          </span>
          <span className="palette__hint">{phaseLabel(state)}</span>
        </div>

        <div className="inline-edit__input-row">
          <textarea
            ref={inputRef}
            className="inline-edit__input"
            data-testid="inline-edit-input"
            placeholder="要改成什么？（例如：改成早退返回、补上错误处理）"
            value={state.instruction}
            spellCheck={false}
            rows={2}
            disabled={busy}
            aria-label="内联编辑指令"
            onChange={(event) => inlineEditActions.setInstruction(event.target.value)}
          />
          <button
            type="button"
            className="btn btn--primary"
            data-testid="inline-edit-submit"
            disabled={busy || state.instruction.trim() === ''}
            onClick={() => void inlineEditActions.submit()}
          >
            {state.phase === 'loading' ? '生成中…' : '生成建议'}
          </button>
        </div>

        {state.error !== null && (
          <div className="palette__error" data-testid="inline-edit-error" role="alert">
            {state.error}
          </div>
        )}

        {state.phase === 'loading' && (
          <p className="palette__empty" data-testid="inline-edit-loading">
            正在读取选区上下文并生成建议…
          </p>
        )}

        {preview && (
          <>
            <div
              className="inline-edit__diff"
              data-testid="inline-edit-diff"
              ref={diffHostRef}
            />
            <div className="inline-edit__diff-note">
              左为当前代码，右为建议；内核不写盘，点「接受」才会经 `workspace.write_file` 落盘。
            </div>
          </>
        )}

        <div className="palette__footer">
          <span>{state.phase === 'saving' ? '正在写回…' : ''}</span>
          <span className="inline-edit__actions">
            {preview ? (
              <>
                <button
                  type="button"
                  className="btn btn--primary"
                  data-testid="inline-edit-accept"
                  disabled={busy}
                  onClick={() => void inlineEditActions.accept()}
                >
                  接受并写回（Ctrl+Enter）
                </button>
                <button
                  type="button"
                  className="btn"
                  data-testid="inline-edit-reject"
                  disabled={busy}
                  onClick={() => inlineEditActions.reject()}
                >
                  拒绝
                </button>
              </>
            ) : (
              <button
                type="button"
                className="btn"
                data-testid="inline-edit-close"
                disabled={busy}
                onClick={() => inlineEditActions.reject()}
              >
                取消
              </button>
            )}
            <span className="palette__hint">Esc 关闭且不改动文件</span>
          </span>
        </div>
      </div>
    </div>
  )
}
