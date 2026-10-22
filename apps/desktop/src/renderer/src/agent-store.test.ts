/**
 * M4 渲染侧（Agent 运行 + 权限审批）行为测试。
 *
 * 与 `ai-store.test.ts` 同一套路：不渲染 React，直接驱动导出的动作函数，用假的
 * `window.aiide` 顶替 IPC，断言「发了什么请求」与「事件怎么落到状态上」。
 *
 * 这里钉死的几条都是审批闭环上**错了就会出事**的地方：
 * - 发起运行必须带全 `model_ref` / `context_refs`，且**不能**有权限覆盖（外壳没有
 *   绕过内核闸门的入口）；
 * - 停止只标「停止中」，终态一律等 `agent.finished`（内核取消也会发它）；
 * - 待裁决询问不能丢：连「本进程没见过的 run」发来的询问也要入列，否则重启后的
 *   审批卡在无人看见的地方；
 * - 同一询问只入列一次、提交期间按钮锁死，避免二次裁决报出莫名其妙的错误。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  Methods,
  type AgentApprovalRequiredEvent,
  type AgentDecideParams,
  type AgentFinishedEvent,
  type AgentStartParams,
  type ModelRef
} from '@ai-ide/shared-types'

import type { AiEvent, AiIdeApi } from '../../shared/ipc'
import { handleAiEvent } from './ai-store'
import {
  decideApproval,
  getAgentState,
  handleAgentEvent,
  resetAgentState,
  startRun,
  stopRun,
  type AgentApproval,
  type AgentRun
} from './agent-store'
import type { ContextChip } from './context-completion'

interface Call {
  method: string
  params: Record<string, unknown> | undefined
}

const MODEL: ModelRef = { provider_id: 'p-openai', model_id: 'fake-mini' }
const RUN_ID = 'run-1'
const FILE_CHIP: ContextChip = { kind: 'file', path: 'src/app.ts', label: 'app.ts' }

/** 没被测试用到的成员：一旦被调到就炸，免得假实现悄悄替真实现兜底。 */
function unused(name: string): never {
  throw new Error(`测试未使用 ${name}`)
}

interface Harness {
  calls: Call[]
  /** 主进程 → 渲染进程的事件注入点。 */
  emit: (method: string, params: unknown) => void
  /** 让下一个请求失败；给了 `method` 就只让该方法的请求失败。 */
  failNext: (message: string, method?: string) => void
  /** `agent.start` 回执里的 run_id。 */
  setRunId: (runId: string) => void
  /** `agent.stop` 的应答。 */
  setStopResult: (ok: boolean, stepsDone: number) => void
  /** 挂起 `agent.decide` 的应答，用来测「提交期间重复点击」。 */
  holdDecide: () => void
  releaseDecide: () => void
}

function installHarness(): Harness {
  const calls: Call[] = []
  let runId = RUN_ID
  let stopResult = { ok: true, stepsDone: 0 }
  let failure: { message: string; method: string | null } | null = null
  let listener: ((event: AiEvent) => void) | null = null
  let decideGate: Promise<void> | null = null
  let releaseGate: (() => void) | null = null

  const api: AiIdeApi = {
    getCoreStatus: async () => unused('getCoreStatus'),
    restartCore: async () => unused('restartCore'),
    ping: async () => unused('ping'),
    // M5：导出会话的落点由主进程的系统对话框决定，渲染侧只拿返回的路径。
    pickSavePath: async () => null,
    pickWorkspaceDirectory: async () => null,
    request: async (method: string, params?: unknown) => {
      calls.push({ method, params: params as Record<string, unknown> | undefined })
      if (method === Methods.agent.decide && decideGate !== null) await decideGate
      if (failure && (failure.method === null || failure.method === method)) {
        const message = failure.message
        failure = null
        throw new Error(message)
      }
      switch (method) {
        case Methods.agent.start:
          return { run_id: runId }
        case Methods.agent.stop:
          return { ok: stopResult.ok, steps_done: stopResult.stepsDone }
        case Methods.agent.decide:
          return { ok: true }
        default:
          return { ok: true }
      }
    },
    onCoreStatus: () => () => undefined,
    onCoreLog: () => () => undefined,
    onWorkspaceChanged: () => () => undefined,
    onWorkspaceIndexed: () => () => undefined,
    getCredentialStatus: async () => ({
      encryptionAvailable: true,
      vaultPath: 'C:\\ud\\credentials.enc',
      managedProviderIds: [],
      lastError: null
    }),
    forgetCredential: async () => ({
      encryptionAvailable: true,
      vaultPath: 'C:\\ud\\credentials.enc',
      managedProviderIds: [],
      lastError: null
    }),
    onAiEvent: (next) => {
      listener = next
      return () => {
        listener = null
      }
    },
    onCredentialStatus: () => () => undefined,
    onTerminalData: () => () => undefined,
    onTerminalExit: () => () => undefined
  }

  vi.stubGlobal('window', { aiide: api })

  return {
    calls,
    emit: (method, params) => listener?.({ method, params }),
    failNext: (message, method) => {
      failure = { message, method: method ?? null }
    },
    setRunId: (next) => {
      runId = next
    },
    setStopResult: (ok, stepsDone) => {
      stopResult = { ok, stepsDone }
    },
    holdDecide: () => {
      decideGate = new Promise<void>((resolve) => {
        releaseGate = resolve
      })
    },
    releaseDecide: () => {
      releaseGate?.()
      decideGate = null
      releaseGate = null
    }
  }
}

/** 取第 n 次请求（默认第一次）；拿不到就直接报错，免得断言落在 undefined 上悄悄通过。 */
function callAt(index = 0): Call {
  const call = harness.calls[index]
  if (!call) throw new Error(`没有第 ${index} 次请求`)
  return call
}

function runAt(index = 0): AgentRun {
  const run = getAgentState().runs[index]
  if (!run) throw new Error(`没有第 ${index} 次运行`)
  return run
}

function approvalAt(index = 0): AgentApproval {
  const approval = getAgentState().approvals[index]
  if (!approval) throw new Error(`没有第 ${index} 条询问`)
  return approval
}

function approvalEvent(patch: Partial<AgentApprovalRequiredEvent> = {}): AgentApprovalRequiredEvent {
  return {
    run_id: RUN_ID,
    approval_id: 'ap-1',
    tool: 'run_command',
    args: { command: 'cargo test -p ai-ide-core' },
    risk: 'medium',
    reason: '这条命令会执行本地进程',
    ...patch
  }
}

/** 造一条 `agent.finished`。 */
function finishedEvent(patch: Partial<AgentFinishedEvent> = {}): AgentFinishedEvent {
  return { run_id: RUN_ID, status: 'completed', steps: 1, summary: null, ...patch }
}

let harness: Harness

beforeEach(() => {
  vi.unstubAllGlobals()
  resetAgentState()
  harness = installHarness()
})

describe('agent-store · 发起运行', () => {
  it('目标为空时不发请求，只留下提示', async () => {
    const ok = await startRun({ goal: '   ', modelRef: MODEL, chips: [] })

    expect(ok).toBe(false)
    expect(harness.calls).toHaveLength(0)
    expect(getAgentState().hint).toBe('先说清这次要做什么：目标不能为空。')
  })

  it('没选模型时不发请求', async () => {
    const ok = await startRun({ goal: '补超时配置', modelRef: null, chips: [] })

    expect(ok).toBe(false)
    expect(harness.calls).toHaveLength(0)
    expect(getAgentState().hint).toContain('还没选模型')
  })

  it('请求体口径与对话流一致，且不带任何权限覆盖', async () => {
    await startRun({ goal: '  补超时配置  ', modelRef: MODEL, chips: [FILE_CHIP] })

    expect(callAt().method).toBe(Methods.agent.start)
    const params = callAt().params as unknown as AgentStartParams
    // 首尾空白要去掉：目标是要给模型看的，不该把用户敲的回车一起塞进去。
    expect(params.goal).toBe('补超时配置')
    expect(params.model_ref).toEqual(MODEL)
    expect(params.session_id).toBeNull()
    expect(params.mode).toBe('agent')
    expect(params.max_steps).toBeNull()
    // 外壳不提供绕过内核闸门的入口：这一项永远是 null。
    expect(params.permissions_override).toBeNull()
    // 引用与 `llm.chat_stream` 的 `context.refs` 同一形状、同一来源。
    expect(params.context_refs).toEqual([
      {
        kind: 'file',
        path: 'src/app.ts',
        start_line: null,
        end_line: null,
        text: null,
        label: 'app.ts'
      }
    ])
  })

  it('模式与步数上限按面板取值传下去', async () => {
    await startRun({ goal: '只聊两句', modelRef: MODEL, chips: [], mode: 'chat', maxSteps: 7 })

    const params = callAt().params as unknown as AgentStartParams
    expect(params.mode).toBe('chat')
    expect(params.max_steps).toBe(7)
  })

  it('回执的 run_id 落进运行表，状态为运行中', async () => {
    harness.setRunId('run-42')

    const ok = await startRun({ goal: '补超时配置', modelRef: MODEL, chips: [] })

    expect(ok).toBe(true)
    expect(getAgentState().runs).toHaveLength(1)
    expect(runAt()).toMatchObject({
      runId: 'run-42',
      goal: '补超时配置',
      status: 'running',
      outcome: null,
      steps: 0,
      stopping: false
    })
    expect(getAgentState().hint).toBeNull()
  })

  it('发起失败：不落运行表，提示带上错误码的解释', async () => {
    harness.failNext('[2001] 未配置模型')

    const ok = await startRun({ goal: '补超时配置', modelRef: MODEL, chips: [] })

    expect(ok).toBe(false)
    expect(getAgentState().runs).toHaveLength(0)
    expect(getAgentState().hint).toContain('还没配置 Provider')
  })
})

describe('agent-store · 停止', () => {
  it('只标「停止中」并记下步数，终态等 agent.finished 收口', async () => {
    await startRun({ goal: '补超时配置', modelRef: MODEL, chips: [] })
    harness.setStopResult(true, 3)

    await stopRun(RUN_ID)

    expect(callAt(1).method).toBe(Methods.agent.stop)
    expect(callAt(1).params).toEqual({ run_id: RUN_ID })
    // 关键：内核取消同样会发 finished，外壳提前收口会与随后的事件打架。
    expect(runAt()).toMatchObject({ status: 'running', stopping: true, steps: 3 })

    handleAgentEvent({
      method: Methods.event.agentFinished,
      params: finishedEvent({ status: 'cancelled', steps: 4, summary: '用户在第 4 步中止' })
    })

    expect(runAt()).toMatchObject({
      status: 'done',
      outcome: 'cancelled',
      steps: 4,
      summary: '用户在第 4 步中止',
      stopping: false
    })
  })

  it('内核说没有这次运行时，不改本地运行表，只提示', async () => {
    await startRun({ goal: '补超时配置', modelRef: MODEL, chips: [] })
    harness.setStopResult(false, 0)

    const ok = await stopRun(RUN_ID)

    expect(ok).toBe(false)
    expect(runAt().stopping).toBe(false)
    expect(getAgentState().hint).toBe('这次运行已经结束了。')
  })
})

describe('agent-store · 权限审批', () => {
  it('询问到达即入列，字段原样保留给 UI', () => {
    handleAgentEvent({ method: Methods.event.agentApprovalRequired, params: approvalEvent() })

    expect(getAgentState().approvals).toHaveLength(1)
    expect(approvalAt()).toMatchObject({
      approvalId: 'ap-1',
      runId: RUN_ID,
      tool: 'run_command',
      risk: 'medium',
      reason: '这条命令会执行本地进程',
      busy: false,
      error: null
    })
    // 参数原文要能原样摆到卡片上，不能只剩一句摘要。
    expect(approvalAt().args).toEqual({ command: 'cargo test -p ai-ide-core' })
  })

  it('同一 approval_id 重复到达只入列一次', () => {
    handleAgentEvent({ method: Methods.event.agentApprovalRequired, params: approvalEvent() })
    handleAgentEvent({ method: Methods.event.agentApprovalRequired, params: approvalEvent() })

    expect(getAgentState().approvals).toHaveLength(1)
  })

  it('本进程没见过的 run 发来的询问也要入列（重启后仍要看得见）', () => {
    handleAgentEvent({
      method: Methods.event.agentApprovalRequired,
      params: approvalEvent({ run_id: 'run-unknown', approval_id: 'ap-x' })
    })

    expect(getAgentState().runs).toHaveLength(0)
    expect(getAgentState().approvals.map((item) => item.approvalId)).toEqual(['ap-x'])
  })

  it('裁决提交成功后出列，参数按面板选择', async () => {
    handleAgentEvent({ method: Methods.event.agentApprovalRequired, params: approvalEvent() })

    const ok = await decideApproval('ap-1', 'allow_once')

    expect(ok).toBe(true)
    expect(callAt().method).toBe(Methods.agent.decide)
    expect(callAt().params).toEqual({
      run_id: RUN_ID,
      approval_id: 'ap-1',
      decision: 'allow_once',
      reason: null
    } satisfies AgentDecideParams)
    expect(getAgentState().approvals).toHaveLength(0)
  })

  it('提交期间重复点击不会发第二次请求', async () => {
    handleAgentEvent({ method: Methods.event.agentApprovalRequired, params: approvalEvent() })
    harness.holdDecide()

    const first = decideApproval('ap-1', 'allow_once')
    expect(approvalAt().busy).toBe(true)

    const second = await decideApproval('ap-1', 'allow_once')
    expect(second).toBe(false)
    expect(harness.calls.filter((call) => call.method === Methods.agent.decide)).toHaveLength(1)

    harness.releaseDecide()
    expect(await first).toBe(true)
    expect(getAgentState().approvals).toHaveLength(0)
  })

  it('拒绝但没写理由时降级成普通拒绝，不给模型灌空理由', async () => {
    handleAgentEvent({ method: Methods.event.agentApprovalRequired, params: approvalEvent() })

    await decideApproval('ap-1', 'deny_with_reason', '   ')

    expect(callAt().params).toMatchObject({ decision: 'deny', reason: null })
  })

  it('写了理由就按 deny_with_reason 送上去', async () => {
    handleAgentEvent({ method: Methods.event.agentApprovalRequired, params: approvalEvent() })

    await decideApproval('ap-1', 'deny_with_reason', ' 别动这个文件 ')

    expect(callAt().params).toMatchObject({ decision: 'deny_with_reason', reason: '别动这个文件' })
  })

  it('裁决失败：卡片留在列里并带上原因，可重试', async () => {
    handleAgentEvent({ method: Methods.event.agentApprovalRequired, params: approvalEvent() })
    harness.failNext('[3001] 权限闸门拒绝', Methods.agent.decide)

    const ok = await decideApproval('ap-1', 'allow_once')

    expect(ok).toBe(false)
    expect(approvalAt().busy).toBe(false)
    expect(approvalAt().error).toContain('权限闸门拦下')

    const retry = await decideApproval('ap-1', 'allow_once')
    expect(retry).toBe(true)
    expect(getAgentState().approvals).toHaveLength(0)
  })

  it('运行收工会清掉它名下未决的询问', () => {
    handleAgentEvent({ method: Methods.event.agentApprovalRequired, params: approvalEvent() })
    handleAgentEvent({
      method: Methods.event.agentApprovalRequired,
      params: approvalEvent({ approval_id: 'ap-2' })
    })
    handleAgentEvent({
      method: Methods.event.agentApprovalRequired,
      params: approvalEvent({ run_id: 'run-other', approval_id: 'ap-3' })
    })

    handleAgentEvent({
      method: Methods.event.agentFinished,
      params: finishedEvent({ status: 'completed', steps: 9, summary: '改完并跑了测试' })
    })

    // 本运行的两条清掉（它名下不会再有裁决了），别的运行的那条留着。
    expect(getAgentState().approvals.map((item) => item.approvalId)).toEqual(['ap-3'])
  })
})

describe('agent-store · 事件路由', () => {
  it('经 ai-store 的统一分发也能送到（订阅只有一条）', () => {
    handleAiEvent({ method: Methods.event.agentApprovalRequired, params: approvalEvent() })

    expect(getAgentState().approvals).toHaveLength(1)

    handleAiEvent({
      method: Methods.event.agentFinished,
      params: finishedEvent({ status: 'failed', steps: 1, summary: '上游 500' })
    })

    expect(getAgentState().approvals).toHaveLength(0)
  })
})
