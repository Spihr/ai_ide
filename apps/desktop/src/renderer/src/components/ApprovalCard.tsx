/**
 * 审批卡：一条待裁决的权限询问的**完整呈现与裁决入口**。
 *
 * 从 `AgentPanel` 里抽出来，是为了让「一条询问长什么样、怎么裁决」只在这里定义一次：
 * 后续插件视图（`PluginViewHost`）若也要就地征求用户点头，可以直接复用它，
 * 而不必把 `AgentPanel` 的整块 JSX 再抄一遍。
 *
 * 边界（刻意的）：
 * - 本组件**不订阅 store、不碰 IPC**：进来的是数据（`AgentApproval`），出去的是意图
 *   （`onDecide`）。提交失败后的 `error` / `busy` 由 store 回灌到 props 再渲染。
 * - 拒绝理由草稿是**卡片内部状态**：它只服务于这张卡片的输入框，没有跨卡片共享的必要。
 * - 风险配色仍走 `agent-approval--{risk}` 类名，样式表（`styles.css`）不动。
 */

import { useState } from 'react'

import type { ApprovalDecision } from '@ai-ide/shared-types'

import type { AgentApproval } from '../agent-store'

const RISK_TEXT: Record<AgentApproval['risk'], string> = {
  low: '低风险',
  medium: '中风险',
  high: '高风险'
}

/** 参数原文按 JSON 排版展示：调用的「原文」比任何摘要都可信。 */
function formatArgs(args: unknown): string {
  try {
    const text = JSON.stringify(args, null, 2)
    return text ?? String(args)
  } catch {
    return String(args)
  }
}

export interface ApprovalCardProps {
  /** 待裁决的询问（来自 `agent.approval_required`，经 store 归一）。 */
  approval: AgentApproval
  /**
   * 用户按下裁决。
   * `reason` 只在「拒绝并说明」这条路径上有意义；是空串时由 store 降级成普通 `deny`。
   */
  onDecide: (decision: ApprovalDecision, reason: string | null) => void
}

/**
 * 一张审批卡。裁决按钮在 `busy`（提交中）期间全部禁用，避免重复提交。
 *
 * 结构/类名/`data-testid` 与抽取前的内联 JSX 逐字一致——e2e 的既有选择器不受影响。
 */
export function ApprovalCard({ approval, onDecide }: ApprovalCardProps): React.JSX.Element {
  /** 拒绝理由草稿。 */
  const [reason, setReason] = useState('')
  /** 是否展开「拒绝并说明」输入框。 */
  const [reasonOpen, setReasonOpen] = useState(false)

  const submit = (decision: ApprovalDecision): void => onDecide(decision, reason)

  return (
    <div
      className={`agent-approval agent-approval--${approval.risk}`}
      data-testid="agent-approval"
    >
      <div className="agent-approval__head">
        <span className="agent-approval__risk">{RISK_TEXT[approval.risk]}</span>
        <span className="agent-approval__tool">{approval.tool}</span>
      </div>
      <p className="agent-approval__reason">{approval.reason}</p>
      <pre className="agent-approval__args">{formatArgs(approval.args)}</pre>
      {approval.error !== null ? <div className="ai-result ai-result--bad">{approval.error}</div> : null}
      <div className="ai-row">
        <button
          className="ai-button"
          type="button"
          data-testid="agent-allow-once"
          disabled={approval.busy}
          onClick={() => submit('allow_once')}
        >
          允许一次
        </button>
        <button
          className="ai-button"
          type="button"
          data-testid="agent-allow-always"
          disabled={approval.busy}
          title="同一工具同一范围在本进程内不再重复询问"
          onClick={() => submit('allow_always')}
        >
          始终允许
        </button>
        <button
          className="ai-button ai-button--danger"
          type="button"
          data-testid="agent-deny"
          disabled={approval.busy}
          onClick={() => submit('deny')}
        >
          拒绝
        </button>
        <button
          className="ai-button"
          type="button"
          data-testid="agent-deny-reason-toggle"
          disabled={approval.busy}
          onClick={() => setReasonOpen((current) => !current)}
        >
          拒绝并说明…
        </button>
      </div>
      {reasonOpen ? (
        <div className="ai-row">
          <input
            className="ai-input"
            data-testid="agent-deny-reason"
            value={reason}
            placeholder="为什么不行（会回灌给模型自纠）"
            onChange={(event) => setReason(event.target.value)}
          />
          <button
            className="ai-button ai-button--danger"
            type="button"
            data-testid="agent-deny-submit"
            disabled={approval.busy}
            onClick={() => {
              submit('deny_with_reason')
              setReasonOpen(false)
            }}
          >
            提交拒绝
          </button>
        </div>
      ) : null}
    </div>
  )
}
