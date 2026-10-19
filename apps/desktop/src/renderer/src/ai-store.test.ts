/**
 * M2 渲染侧（Provider / 模型 / 对话）行为测试。
 *
 * 与 `store.test.ts` 同一套路：不渲染 React，直接驱动导出的动作函数，用假的
 * `window.aiide` 顶替 IPC，断言「发了什么请求」与「事件怎么落到状态上」。
 *
 * 这里钉死的几条是 M2 外壳侧最容易写错、且错了以后**看不见**的地方：
 * 明文密钥只出现在请求参数里、流式事件必须按 `stream_id` 过滤、取消要本地收口
 * （内核取消不发 `llm.finished`）、错误码要变成人能看懂的话。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  Methods,
  type LlmChatStreamParams,
  type ModelView,
  type ProviderView
} from '@ai-ide/shared-types'

import type { AiEvent, AiIdeApi } from '../../shared/ipc'
import {
  cancelStream,
  clearChat,
  getAiState,
  handleAiEvent,
  initAi,
  refreshProviders,
  resetAiState,
  saveProvider,
  selectModel,
  sendMessage,
  testProvider
} from './ai-store'

interface Call {
  method: string
  params: Record<string, unknown> | undefined
}

const PROVIDER_ID = 'p-openai'
const MODEL_PK = 'm-1'

function providerView(patch: Partial<ProviderView> = {}): ProviderView {
  return {
    id: PROVIDER_ID,
    name: '本地假上游',
    protocol: 'openai_chat_completions',
    base_url: 'http://127.0.0.1:8899/v1',
    headers: {},
    enabled: true,
    has_key: true,
    key_hint: 'sk-a…f2c',
    timeout_ms: null,
    last_test_ok: null,
    created_at_ms: 1,
    updated_at_ms: 1,
    ...patch
  }
}

function modelView(patch: Partial<ModelView> = {}): ModelView {
  return {
    model_pk: MODEL_PK,
    provider_id: PROVIDER_ID,
    model_id: 'fake-mini',
    display_name: 'Fake Mini',
    caps: { streaming: true, tools: false, reasoning: false, vision: false, context_window: null },
    params: { temperature: null, max_output_tokens: null, top_p: null },
    created_at_ms: 1,
    ...patch
  }
}

interface Harness {
  calls: Call[]
  /** 主进程 → 渲染进程的 M2 事件注入点。 */
  emit: (method: string, params: unknown) => void
  failNext: (message: string) => void
  setProviders: (providers: ProviderView[]) => void
  setModels: (models: ModelView[]) => void
}

function installHarness(): Harness {
  const calls: Call[] = []
  let providers: ProviderView[] = [providerView()]
  let models: ModelView[] = [modelView()]
  let failure: string | null = null
  let listener: ((event: AiEvent) => void) | null = null

  const api: AiIdeApi = {
    getCoreStatus: async () => {
      throw new Error('测试未使用 getCoreStatus')
    },
    restartCore: async () => {
      throw new Error('测试未使用 restartCore')
    },
    ping: async () => {
      throw new Error('测试未使用 ping')
    },
    pickWorkspaceDirectory: async () => null,
    request: async (method: string, params?: unknown) => {
      calls.push({ method, params: params as Record<string, unknown> | undefined })
      if (failure) {
        const message = failure
        failure = null
        throw new Error(message)
      }
      switch (method) {
        case Methods.provider.list:
          return { providers }
        case Methods.provider.upsert:
          return { provider_id: (params as { id?: string | null } | undefined)?.id ?? 'p-new' }
        case Methods.provider.delete:
          return { ok: true }
        case Methods.provider.test:
          return { ok: true, latency_ms: 12, models_found: 3, message: null }
        case Methods.model.list:
          return { models }
        case Methods.model.upsert:
          return { model_pk: MODEL_PK }
        case Methods.model.delete:
          return { ok: true }
        case Methods.llm.listModels:
          return { models: [{ id: 'fake-mini', owned_by: null, created: null }] }
        case Methods.llm.chatStream:
          return { stream_id: (params as LlmChatStreamParams | undefined)?.stream_id ?? 'srv-side' }
        case Methods.llm.cancel:
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
      managedProviderIds: [PROVIDER_ID],
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
    onCredentialStatus: () => () => undefined
  }

  vi.stubGlobal('window', { aiide: api })

  return {
    calls,
    emit: (method, params) => listener?.({ method, params }),
    failNext: (message) => {
      failure = message
    },
    setProviders: (next) => {
      providers = next
    },
    setModels: (next) => {
      models = next
    }
  }
}

let h: Harness

function callsOf(method: string): Call[] {
  return h.calls.filter((call) => call.method === method)
}

/** 等待已排入微任务队列的异步动作（动作函数本身不等全流程）。 */
async function flush(): Promise<void> {
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
}

beforeEach(async () => {
  resetAiState()
  h = installHarness()
  initAi()
  await flush()
})

describe('Provider 清单', () => {
  it('initAi 订阅事件并拉取密钥托管状态与 Provider 清单', async () => {
    expect(callsOf(Methods.provider.list)).toHaveLength(1)
    expect(callsOf(Methods.model.list)).toHaveLength(1)

    const state = getAiState()
    expect(state.activeProviderId).toBe(PROVIDER_ID)
    expect(state.providers).toHaveLength(1)
    expect(state.credentials?.managedProviderIds).toEqual([PROVIDER_ID])
    // 默认选中第一个模型：不选的话「发送」永远点不动，用户会以为坏了。
    expect(state.selectedModel).toEqual({ provider_id: PROVIDER_ID, model_id: 'fake-mini' })
  })

  it('保存 Provider 时把明文密钥交给主进程托管，状态里不残留密钥', async () => {
    const ok = await saveProvider({
      id: null,
      name: '新上游',
      protocol: 'openai_chat_completions',
      base_url: 'http://127.0.0.1:8899/v1',
      api_key: '  sk-secret-123  ',
      enabled: true,
      timeout_ms: null
    })

    expect(ok).toBe(true)
    const upsert = callsOf(Methods.provider.upsert)[0]
    expect(upsert?.params?.api_key).toBe('sk-secret-123')

    // 密钥只能活在「这次请求的参数里」：任何状态快照都不该带上它。
    expect(JSON.stringify(getAiState())).not.toContain('sk-secret-123')
  })

  it('密钥留空表示沿用内核已存的那把（传 null，不覆盖）', async () => {
    await saveProvider({
      id: PROVIDER_ID,
      name: '本地假上游',
      protocol: 'openai_chat_completions',
      base_url: 'http://127.0.0.1:8899/v1',
      api_key: '   ',
      enabled: true,
      timeout_ms: null
    })

    const upsert = callsOf(Methods.provider.upsert)[0]
    expect(upsert?.params?.api_key).toBeNull()
  })

  it('provider.changed 事件触发清单重新拉取（内核侧改动会自己冒上来）', async () => {
    const before = callsOf(Methods.provider.list).length
    handleAiEvent({ method: Methods.event.providerChanged, params: { provider_id: PROVIDER_ID } })
    await flush()
    expect(callsOf(Methods.provider.list).length).toBe(before + 1)
  })
})

describe('连接测试', () => {
  it('成功时给出延迟与模型数', async () => {
    await testProvider(PROVIDER_ID)
    expect(getAiState().test.ok).toBe(true)
    expect(getAiState().test.text).toContain('12ms')
    expect(getAiState().test.text).toContain('3 个模型')
  })

  it('失败时把协议错误码翻成人话', async () => {
    h.failNext('RPC 失败：[2002] 上游返回 401 unauthorized')
    await testProvider(PROVIDER_ID)

    const state = getAiState()
    expect(state.test.ok).toBe(false)
    expect(state.test.text).toContain('鉴权失败')
  })
})

describe('对话', () => {
  it('没选模型时直接提示，不发请求', async () => {
    selectModel(null)
    const sent = await sendMessage('你好')

    expect(sent).toBe(false)
    expect(callsOf(Methods.llm.chatStream)).toHaveLength(0)
    expect(getAiState().hint).toContain('还没配置 Provider 或没选模型')
  })

  it('发送后立刻进入流式态，并把历史气泡一并带给内核', async () => {
    await sendMessage('第一句')
    const streamId = getAiState().streamId
    expect(streamId).toBeTruthy()
    expect(getAiState().streaming).toBe(true)
    expect(getAiState().bubbles).toHaveLength(2)

    const params = callsOf(Methods.llm.chatStream)[0]?.params as LlmChatStreamParams
    expect(params.model_ref).toEqual({ provider_id: PROVIDER_ID, model_id: 'fake-mini' })
    expect(params.messages).toHaveLength(1)
    expect(params.messages[0]?.content).toBe('第一句')
    expect(params.stream_id).toBe(streamId)
  })

  it('started/delta/usage/finished 走完后收口成一条完整回答', async () => {
    await sendMessage('讲个笑话')
    const streamId = getAiState().streamId as string

    h.emit(Methods.event.llmStarted, {
      stream_id: streamId,
      model_ref: { provider_id: PROVIDER_ID, model_id: 'fake-mini' },
      session_id: null,
      started_at_ms: 1
    })
    h.emit(Methods.event.llmDelta, { stream_id: streamId, text: '从前' })
    h.emit(Methods.event.llmDelta, { stream_id: streamId, text: '有座山' })
    h.emit(Methods.event.llmUsage, {
      stream_id: streamId,
      prompt_tokens: 11,
      completion_tokens: 4,
      cached_tokens: null
    })
    h.emit(Methods.event.llmFinished, { stream_id: streamId, finish_reason: 'stop', message_id: 'msg-1' })

    const state = getAiState()
    const answer = state.bubbles[1]
    expect(answer?.role).toBe('assistant')
    expect(answer?.text).toBe('从前有座山')
    expect(answer?.status).toBe('done')
    expect(state.usage).toEqual({ promptTokens: 11, completionTokens: 4 })
    expect(state.finishReason).toBe('stop')
    expect(state.streaming).toBe(false)
    expect(state.streamId).toBeNull()
  })

  it('别的 stream_id 的事件一律丢弃（上一条被取消的流不能往新对话灌字）', async () => {
    await sendMessage('第一条')
    const streamId = getAiState().streamId as string

    h.emit(Methods.event.llmDelta, { stream_id: 'stream-别人的', text: '不该出现' })
    expect(getAiState().bubbles[1]?.text).toBe('')

    h.emit(Methods.event.llmDelta, { stream_id: streamId, text: '这是我的' })
    expect(getAiState().bubbles[1]?.text).toBe('这是我的')
  })

  it('收口之后迟到的 delta 不再追加（拿不到 stream_id 的脏事件也要挡住）', async () => {
    await sendMessage('第一条')
    const streamId = getAiState().streamId as string
    h.emit(Methods.event.llmDelta, { stream_id: streamId, text: '正文' })
    h.emit(Methods.event.llmFinished, { stream_id: streamId, finish_reason: 'stop', message_id: null })

    h.emit(Methods.event.llmDelta, { stream_id: streamId, text: '迟到的尾巴' })
    expect(getAiState().bubbles[1]?.text).toBe('正文')
  })

  it('llm.error 把气泡标成错误并给可读提示', async () => {
    await sendMessage('你好')
    const streamId = getAiState().streamId as string

    h.emit(Methods.event.llmError, {
      stream_id: streamId,
      code: 2005,
      message: 'connect refused',
      upstream: null
    })

    const state = getAiState()
    expect(state.streaming).toBe(false)
    expect(state.hint).toContain('连不上上游')
    expect(state.bubbles[1]?.status).toBe('error')
  })

  it('发送失败（内核拒绝）时错误落到这条气泡上，而不是静默卡住', async () => {
    h.failNext('RPC 失败：[2001] 未配置模型')
    const sent = await sendMessage('你好')

    expect(sent).toBe(false)
    const state = getAiState()
    expect(state.streaming).toBe(false)
    expect(state.hint).toContain('还没配置 Provider 或没选模型')
    expect(state.bubbles[1]?.status).toBe('error')
  })

  it('取消后本地收口：内核取消不发 finished，不本地收口就会永远停在生成中', async () => {
    await sendMessage('写一篇长文')
    h.emit(Methods.event.llmDelta, { stream_id: getAiState().streamId as string, text: '前半段' })

    const cancelled = await cancelStream()

    const state = getAiState()
    expect(cancelled).toBe(true)
    expect(callsOf(Methods.llm.cancel)).toHaveLength(1)
    expect(state.streaming).toBe(false)
    expect(state.streamId).toBeNull()
    expect(state.bubbles[1]?.status).toBe('done')
    expect(state.bubbles[1]?.note).toBe('已取消')
    expect(state.bubbles[1]?.text).toBe('前半段')
  })

  it('没有在途的流时取消是空操作', async () => {
    expect(await cancelStream()).toBe(false)
  })

  it('清空对话后可以重新开始（流状态一并复位）', async () => {
    await sendMessage('你好')
    clearChat()

    const state = getAiState()
    expect(state.bubbles).toHaveLength(0)
    expect(state.streaming).toBe(false)
    expect(state.usage).toBeNull()
  })
})

describe('清单刷新', () => {
  it('Provider 被外部删掉后不再指向不存在的 id', async () => {
    h.setProviders([])
    await refreshProviders()

    const state = getAiState()
    expect(state.providers).toHaveLength(0)
    expect(state.activeProviderId).toBeNull()
    expect(state.selectedModel).toBeNull()
  })
})
