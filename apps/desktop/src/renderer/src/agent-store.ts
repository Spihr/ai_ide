/**
 * M4 渲染侧状态：Agent 运行 + 权限询问的裁决入口。
 *
 * 与 `ai-store.ts` 同构（单例 + `useSyncExternalStore`），职责分开：对话流与
 * Agent 运行互不干扰，一次审批刷新不该把对话气泡一起重渲染。
 *
 * 三条与「看起来更省事」的写法相反的取舍，都在下面各自的位置写了理由：
 *
 * 1. **审批卡片是安全关键路径**：询问一旦到达就必须能被用户看见，所以不按
 *    `run_id` 过滤——外壳重启（或页面重载）后运行表是空的，若按「不认识的运行
 *    就丢弃」处理，那条询问会永远等不到裁决。
 * 2. **停止不由外壳本地收口**：内核在取消路径同样会发 `agent.finished`
 *    （`status = cancelled`），外壳抢先把运行标成已结束会与随后到达的事件打架。
 *    这与 `llm.cancel`（内核不发收尾事件，外壳必须本地收口）正好相反。
 * 3. **裁决提交期间禁用按钮**：同一次询问被裁决两次，第二次必然是 -32602 或
 *    「运行里没有这条询问」，用户看到的是一句莫名其妙的报错。
 */

import { useSyncExternalStore } from 'react'

import {
  Methods,
  type AgentApprovalRequiredEvent,
  type AgentDecideParams,
  type AgentFinishedEvent,
  type AgentMode,
  type AgentRunStatus,
  type ApprovalDecision,
  type ApprovalRisk,
  type ModelRef
} from '@ai-ide/shared-types'

import type { AiEvent } from '../../shared/ipc'
import { agentApi } from './agent-api'
import { buildContextRefs, type ContextChip } from './context-completion'

/** 协议错误码 → 人话提示（覆盖内核 1xxx / 2xxx / 3xxx 中与运行相关的分区）。 */
const AGENT_ERROR_HINTS: Record<number, string> = {
  1001: '还没打开工作区：Agent 要在工作区里干活，先打开一个目录。',
  1002: '路径越出工作区：这次调用被内核拦下了。',
  1003: '路径在敏感名单里（如凭据文件）：内核不允许读写。',
  2001: '还没配置 Provider 或没选模型：先去「Provider」页填一个再启动。',
  2002: '鉴权失败：API Key 不对或已失效。',
  2003: '被上游限流：等一会儿再启动。',
  2004: '上下文超出模型窗口：换更大窗口的模型，或减少引用。',
  2005: '连不上上游：检查 base_url、网络与代理。',
  3001: '工具被权限闸门拦下：内核按策略拒绝了这次调用。',
  3002: '工具执行超时：命令跑太久被中断。',
  3003: '改动没落地：文件在期间被外部改过，diff 对不上。',
  4001: '运行已被取消。'
}

function describe(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error)
  const matched = /\[(-?\d+)\]/.exec(message)
  const code = matched ? Number(matched[1]) : Number.NaN
  const hint = AGENT_ERROR_HINTS[code] ?? ''
  return hint.length > 0 && !message.includes(hint) ? `${message}｜${hint}` : message
}

/** 一条待用户裁决的权限询问。 */
export interface AgentApproval {
  /** 裁决时回填的询问 id（来自 `agent.approval_required`）。 */
  approvalId: string
  /** 归属的运行。 */
  runId: string
  /** 工具名（`read` / `edit` / `run_command` …）。 */
  tool: string
  /** 工具参数原文，直接给用户看「它打算干什么」。 */
  args: unknown
  /** 风险等级，供 UI 标色。 */
  risk: ApprovalRisk
  /** 为什么问这一声（内核给的人话）。 */
  reason: string
  /** 正在提交裁决：提交期间按钮禁用。 */
  busy: boolean
  /** 裁决提交失败的原因；询问仍在列里，用户可重试。 */
  error: string | null
}

/**
 * 插件宿主那条审批通道的固定 `run_id`。
 *
 * 与内核 `crates/core/src/agent_api.rs` 的 `PLUGIN_HOST_RUN_ID` 逐字一致。插件写盘
 * （`fs.write`）与 `risk=write` 的插件命令都发生在**任何一次 agent 运行之外**，
 * 没有真正的 run 可归属，内核就用这个名字记账，回执仍按 `approval_id` 派发。
 *
 * 外壳据此把询问分成两类：属于某次运行的交给 Agent 面板，属于这里的交给常驻的
 * 审批浮层。少了这一刀，用户从命令面板执行导出命令时那张卡只会出现在 Agent 面板
 * 里——而面板此刻正被命令面板的遮罩挡着，用户只看到命令一直不返回，最后等来一个
 * 3001。这不是「插件坏了」，是没人看见的那一声询问超时了。
 */
export const PLUGIN_HOST_RUN_ID = 'plugin-host'

/** 一次 Agent 运行的视图。 */
export interface AgentRun {
  runId: string
  goal: string
  status: 'running' | 'done'
  /** 终态；`status === 'running'` 时为 `null`。 */
  outcome: AgentRunStatus | null
  /** 已完成的步数（`agent.stop` 的回执与 `agent.finished` 都会刷新它）。 */
  steps: number
  /** 收尾一句话（模型总结或失败原因）。 */
  summary: string | null
  /** 用户已按「停止」，等内核的 `agent.finished` 收口。 */
  stopping: boolean
}

export interface AgentState {
  runs: AgentRun[]
  /** 待裁决的询问，按到达顺序；跨运行共用一条队列。 */
  approvals: AgentApproval[]
  /** 发起阶段的提示（目标为空、没选模型、请求失败）。 */
  hint: string | null
}

function initialState(): AgentState {
  return { runs: [], approvals: [], hint: null }
}

let state: AgentState = initialState()

const listeners = new Set<() => void>()

function setState(patch: Partial<AgentState>): void {
  state = { ...state, ...patch }
  for (const listener of listeners) listener()
}

export function getAgentState(): AgentState {
  return state
}

export function subscribeAgent(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function useAgent(): AgentState {
  return useSyncExternalStore(subscribeAgent, getAgentState, getAgentState)
}

/**
 * 属于某次 agent 运行的询问（Agent 面板渲染这些）。
 *
 * 面板只该管「我正在跑的这一轮要问你什么」。把宿主的询问一起塞进去，会在同一个
 * 屏幕上出现两张卡（面板一张、浮层一张），用户对同一件事裁决两次，第二次必然
 * 报错——所以这里是**互斥**的切分，不是「面板多显示一点」。
 */
export function runApprovals(current: AgentState = state): AgentApproval[] {
  return current.approvals.filter((item) => item.runId !== PLUGIN_HOST_RUN_ID)
}

/** 运行之外的询问（插件命令 / 插件写盘），由常驻审批浮层渲染。 */
export function hostApprovals(current: AgentState = state): AgentApproval[] {
  return current.approvals.filter((item) => item.runId === PLUGIN_HOST_RUN_ID)
}

/** 单测与「重启内核后重连」时重置。 */
export function resetAgentState(): void {
  state = initialState()
  for (const listener of listeners) listener()
}

export interface StartRunInput {
  goal: string
  /** 本次运行使用的模型；没选就发不出去——内核要求 `model_ref` 必填。 */
  modelRef: ModelRef | null
  /** 输入框里的引用 chip，口径与 `llm.chat_stream` 的 `context.refs` 完全一致。 */
  chips: ContextChip[]
  /** 缺省 `agent`：外壳默认让模型能调工具，`chat` 只说话不干活。 */
  mode?: AgentMode
  /** 步数上限；`null` 表示用内核默认值。 */
  maxSteps?: number | null
}

/** 发起一次运行。返回是否真的发出去了（参数不全时只更新提示）。 */
export async function startRun(input: StartRunInput): Promise<boolean> {
  const goal = input.goal.trim()
  if (goal.length === 0) {
    setState({ hint: '先说清这次要做什么：目标不能为空。' })
    return false
  }
  if (input.modelRef === null) {
    setState({ hint: '还没选模型：Agent 运行必须指定一个模型。' })
    return false
  }

  const params = {
    session_id: null,
    model_ref: input.modelRef,
    goal,
    mode: input.mode ?? ('agent' as AgentMode),
    context_refs: buildContextRefs(input.chips),
    max_steps: input.maxSteps ?? null,
    // 权限覆盖一律留空：策略由内核的权限闸门决定，外壳没有「绕过闸门」的入口。
    permissions_override: null
  }

  try {
    const ack = await agentApi.start(params)
    setState({
      runs: [
        ...state.runs,
        {
          runId: ack.run_id,
          goal,
          status: 'running',
          outcome: null,
          steps: 0,
          summary: null,
          stopping: false
        }
      ],
      hint: null
    })
    return true
  } catch (error) {
    setState({ hint: describe(error) })
    return false
  }
}

/**
 * 请求停止某次运行。
 *
 * 这里**只**把运行标成「停止中」并记下内核回执的步数，不把它移出运行表也不标成
 * 已结束：终态由 `agent.finished` 说了算，外壳提前收口会让随后到达的那条事件
 * 找不到落点（也就丢了 `summary` 与真实步数）。
 */
export async function stopRun(runId: string): Promise<boolean> {
  try {
    const result = await agentApi.stop(runId)
    if (!result.ok) {
      // 内核说「没有这次运行」：多半是它已经跑完了。让用户按刷新语义重来，
      // 但不擅自改本地运行表——真结束了就会收到 `agent.finished`。
      setState({ hint: '这次运行已经结束了。' })
      return false
    }
    setState({
      runs: state.runs.map((run) =>
        run.runId === runId ? { ...run, stopping: true, steps: result.steps_done } : run
      ),
      hint: null
    })
    return true
  } catch (error) {
    setState({ hint: describe(error) })
    return false
  }
}

/**
 * 对一次询问下裁决。
 *
 * `deny_with_reason` 但没写理由时**降级成普通 `deny`**：给模型灌一条空理由，
 * 不如干脆不给它「有理由」的错觉。
 */
export async function decideApproval(
  approvalId: string,
  decision: ApprovalDecision,
  reason?: string | null
): Promise<boolean> {
  const approval = state.approvals.find((item) => item.approvalId === approvalId)
  if (!approval) return false
  if (approval.busy) return false

  const text = (reason ?? '').trim()
  const effective: ApprovalDecision = decision === 'deny_with_reason' && text.length === 0 ? 'deny' : decision

  const patchApproval = (patch: Partial<AgentApproval>): void => {
    setState({
      approvals: state.approvals.map((item) =>
        item.approvalId === approvalId ? { ...item, ...patch } : item
      )
    })
  }

  patchApproval({ busy: true, error: null })
  try {
    await agentApi.decide({
      run_id: approval.runId,
      approval_id: approvalId,
      decision: effective,
      reason: effective === 'deny_with_reason' ? text : null
    } satisfies AgentDecideParams)
    // 裁决已送达内核，询问就从列里摘掉；内核随后会发新的询问（下一段要审批的
    // 调用）或 `agent.finished`，都走既有通道。
    setState({ approvals: state.approvals.filter((item) => item.approvalId !== approvalId) })
    return true
  } catch (error) {
    // 裁决没送到：卡片留在原地，按钮解禁，把原因写在卡片上。
    patchApproval({ busy: false, error: describe(error) })
    return false
  }
}

/** 处理一条主进程转发的 agent 事件。导出以便单测直接驱动。 */
export function handleAgentEvent(event: AiEvent): void {
  const { method, params } = event

  if (method === Methods.event.agentApprovalRequired) {
    const ask = params as AgentApprovalRequiredEvent
    // 同一询问只入列一次：内核不会重发，但消息重放 / 双订阅会。重复入列会让用户
    // 对同一件事按两次裁决，第二次必然失败。
    if (state.approvals.some((item) => item.approvalId === ask.approval_id)) return
    setState({
      approvals: [
        ...state.approvals,
        {
          approvalId: ask.approval_id,
          runId: ask.run_id,
          tool: ask.tool,
          args: ask.args,
          risk: ask.risk,
          reason: ask.reason,
          busy: false,
          error: null
        }
      ],
      hint: null
    })
    return
  }

  if (method === Methods.event.agentFinished) {
    const done = params as AgentFinishedEvent
    setState({
      runs: state.runs.map((run) =>
        run.runId === done.run_id
          ? {
              ...run,
              status: 'done',
              outcome: done.status,
              steps: done.steps,
              summary: done.summary,
              stopping: false
            }
          : run
      ),
      // 运行收工后它名下不会再有裁决了（内核在结束路径同样会丢掉待决询问）。
      // 留着这些卡片，用户会以为自己还能放行一次早已结束的调用。
      approvals: state.approvals.filter((item) => item.runId !== done.run_id)
    })
  }
}
