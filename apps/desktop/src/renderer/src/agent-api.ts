/**
 * M4 渲染侧 API 封装：把 `window.aiide.request` 收窄成 agent 的三条业务动作。
 *
 * 与 `ai-api.ts` 同一套路——组件不直接拼方法名与参数。三条方法全部会被主进程的
 * 策略闸门检查一次：`agent.start` / `agent.stop` / `agent.decide` 都是「用户显式
 * 动作」的产物（写下目标并按启动、按停止、对某次询问按下裁决），不存在任何
 * 「渲染进程自动发起」的路径。
 */

import {
  Methods,
  type AgentDecideParams,
  type AgentDecideResult,
  type AgentStartParams,
  type AgentStartResult,
  type AgentStopParams,
  type AgentStopResult
} from '@ai-ide/shared-types'

function request<T>(method: string, params?: unknown): Promise<T> {
  return window.aiide.request(method, params) as Promise<T>
}

export const agentApi = {
  /** 发起一次运行：只等 `{run_id}` 回执，过程经 `agent.*` 事件回来。 */
  start: (params: AgentStartParams): Promise<AgentStartResult> =>
    request<AgentStartResult>(Methods.agent.start, params),

  /** 请求停止。**不代表运行已经结束**：终态仍以内核的 `agent.finished` 为准。 */
  stop: (runId: string): Promise<AgentStopResult> =>
    request<AgentStopResult>(Methods.agent.stop, { run_id: runId } satisfies AgentStopParams),

  /** 对一次权限询问下裁决。 */
  decide: (params: AgentDecideParams): Promise<AgentDecideResult> =>
    request<AgentDecideResult>(Methods.agent.decide, params)
}
