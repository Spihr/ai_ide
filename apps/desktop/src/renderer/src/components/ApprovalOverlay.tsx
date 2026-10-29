/**
 * 运行之外的审批浮层（P5）：插件命令与插件写盘的那一声询问。
 *
 * 为什么需要它，以及为什么不能塞进 `AgentPanel`：
 *
 * - 内核把插件的审批**接回同一条通道**（`agent.approval_required` + `agent.decide`），
 *   事件里的 `run_id` 是固定的 `plugin-host`——插件命令发生在任何一次 agent 运行
 *   之外，没有真的 run 可归属。
 * - `AgentPanel` 只在「AI」侧栏可见时才在屏幕上。而 `risk=write` 的命令最常见的
 *   两个发起方是命令面板（浮层）与插件视图按钮，用户此刻**看着的是别处**。
 * - 于是那条询问没人渲染，命令一直等到闸门超时，回一个 `3001`。用户看到的是
 *   「插件坏了」，实际是「没人看见的那一声询问超时了」——最贵的一类误导。
 *
 * 所以这里做成**常驻浮层**：不依赖当前活动视图，压在命令面板之上，询问一到就出现；
 * 与 `ApprovalCard` 两处共用，裁决入口只有一份。
 *
 * 与 `AgentPanel` 的分工是互斥的（见 `runApprovals` / `hostApprovals`）：同一条询问
 * 全应用只渲染一次，否则用户会对同一件事裁决两次，第二次必然报「询问不存在」。
 */

import { decideApproval, hostApprovals, useAgent } from '../agent-store'
import { ApprovalCard } from './ApprovalCard'

export function ApprovalOverlay(): React.JSX.Element | null {
  const agent = useAgent()
  const approvals = hostApprovals(agent)

  // 没有待裁决就整块不渲染：空壳浮层会挡住编辑器，也会把点击吞掉。
  if (approvals.length === 0) return null

  return (
    <div className="approval-overlay" data-testid="approval-overlay">
      <div
        className="approval-overlay__panel"
        role="dialog"
        aria-modal="true"
        aria-label="插件操作审批"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <div className="approval-overlay__head">
          <span>插件要动你的东西，等你点头</span>
          <span className="approval-overlay__count" data-testid="approval-overlay-count">
            待裁决 {approvals.length} 项
          </span>
        </div>
        {/* 这句话是给「按了命令之后什么都没发生」的处境写的：超时即拒，命令不会偷偷跑。 */}
        <p className="approval-overlay__hint">
          这条询问不属于某次 Agent 运行：命令按下去之后会停在这里等你，不理它会按拒绝处理
          （命令不会执行，插件那边只会收到一句 3001）。
        </p>
        {approvals.map((approval) => (
          <ApprovalCard
            key={approval.approvalId}
            approval={approval}
            onDecide={(decision, reason) => {
              void decideApproval(approval.approvalId, decision, reason)
            }}
          />
        ))}
      </div>
    </div>
  )
}
