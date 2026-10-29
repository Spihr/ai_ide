/**
 * Agent 面板（M4 外壳侧）：发起运行 + 权限询问的裁决入口。
 *
 * 这一块是**审批 UI 的闭环点**：内核在需要用户点头时发
 * `agent.approval_required`，卡片必须带着「它打算干什么」的全部信息（工具名、
 * 参数原文、风险等级、为什么要问）出现在这里，用户按下裁决后经
 * `agent.decide` 回去，运行才继续。
 *
 * 引用复用对话面板已经挑好的 chip（输入框下方那一排）：同一个工作区里
 * 「我要它看这几个文件」这件事只该挑一次，不该在两个面板里各挑一遍。
 * 权限覆盖（`permissions_override`）在这里**没有入口**——策略由内核的闸门决定，
 * 外壳不提供绕过闸门的开关。
 */

import { useState } from 'react'

import type { AgentMode, AgentRunStatus } from '@ai-ide/shared-types'

import { useAi } from '../ai-store'
import { decideApproval, runApprovals, startRun, stopRun, useAgent } from '../agent-store'
import { ApprovalCard } from './ApprovalCard'

const OUTCOME_TEXT: Record<AgentRunStatus, string> = {
  completed: '已完成',
  failed: '失败',
  cancelled: '已取消'
}

export function AgentPanel(): React.JSX.Element {
  const agent = useAgent()
  const ai = useAi()
  const [goal, setGoal] = useState('')
  const [mode, setMode] = useState<AgentMode>('agent')
  const [maxSteps, setMaxSteps] = useState('')

  const start = (): void => {
    const parsed = Number(maxSteps.trim())
    // 空 / 非法 / 非正数一律当「不设上限」交给内核默认值，不在这里编一个数字出来。
    const limit = maxSteps.trim() === '' || !Number.isFinite(parsed) || parsed <= 0 ? null : Math.floor(parsed)
    void startRun({
      goal,
      modelRef: ai.selectedModel,
      chips: ai.chips,
      mode,
      maxSteps: limit
    }).then((ok) => {
      // 只在这一条真的发出去了之后才清空目标：失败时把用户写的东西留着，
      // 否则他要凭记忆重打一遍。
      if (ok) setGoal('')
    })
  }

  return (
    <div className="ai-section" data-testid="agent-panel">
      <h3 className="ai-section__title">Agent 运行</h3>

      <textarea
        className="ai-input ai-textarea"
        data-testid="agent-goal"
        value={goal}
        placeholder="这次要完成什么？（自然语言目标，例如「给 config 模块补上超时配置」）"
        onChange={(event) => setGoal(event.target.value)}
      />

      <div className="ai-row">
        <select
          className="ai-input"
          data-testid="agent-mode"
          value={mode}
          onChange={(event) => setMode(event.target.value as AgentMode)}
        >
          <option value="agent">agent（可调工具）</option>
          <option value="chat">chat（只对话）</option>
          <option value="inline">inline（编辑器内联）</option>
        </select>
        <input
          className="ai-input ai-input--narrow"
          data-testid="agent-max-steps"
          value={maxSteps}
          inputMode="numeric"
          placeholder="步数上限"
          title="留空用内核默认上限"
          onChange={(event) => setMaxSteps(event.target.value)}
        />
        <button
          className="ai-button ai-button--primary"
          type="button"
          data-testid="agent-start"
          disabled={ai.selectedModel === null || goal.trim().length === 0}
          onClick={start}
        >
          启动
        </button>
      </div>

      <div className="ai-note" data-testid="agent-refs-note">
        {ai.selectedModel === null ? '先在上方对话区选一个模型。' : `模型：${ai.selectedModel.model_id}`}
        {ai.chips.length > 0 ? ` · 携带 ${ai.chips.length} 条引用（取自对话区的引用列表）` : ''}
      </div>

      {agent.hint !== null ? (
        <div className="ai-result ai-result--bad" data-testid="agent-hint">
          {agent.hint}
        </div>
      ) : null}

      {agent.runs.length === 0 ? (
        <div className="ai-note">还没有运行。写下目标并按下「启动」。</div>
      ) : (
        agent.runs.map((run) => (
          <div className="agent-run" key={run.runId} data-testid="agent-run">
            <div className="agent-run__head">
              <span className="agent-run__goal" title={run.goal}>
                {run.goal}
              </span>
              <span className="agent-run__status" data-testid="agent-run-status">
                {run.status === 'running'
                  ? run.stopping
                    ? '停止中…'
                    : '运行中…'
                  : OUTCOME_TEXT[run.outcome ?? 'completed']}
              </span>
            </div>
            <div className="ai-note">
              已走 {run.steps} 步 · {run.runId}
            </div>
            {run.summary !== null ? <div className="ai-note">{run.summary}</div> : null}
            {run.status === 'running' ? (
              <div className="ai-row">
                <button
                  className="ai-button"
                  type="button"
                  data-testid="agent-stop"
                  disabled={run.stopping}
                  onClick={() => void stopRun(run.runId)}
                >
                  {run.stopping ? '停止中…' : '停止'}
                </button>
              </div>
            ) : null}
          </div>
        ))
      )}

      {runApprovals(agent).length > 0 ? (
        <h4 className="ai-section__subtitle" data-testid="agent-approval-count">
          待裁决 {runApprovals(agent).length} 项
        </h4>
      ) : null}

      {runApprovals(agent).map((approval) => (
        <ApprovalCard
          key={approval.approvalId}
          approval={approval}
          onDecide={(decision, reason) => {
            void decideApproval(approval.approvalId, decision, reason)
          }}
        />
      ))}
    </div>
  )
}
