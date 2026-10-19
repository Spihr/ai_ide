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
  type FileMatch,
  type LlmChatStreamParams,
  type LlmCountTokensParams,
  type ModelView,
  type ProviderView,
  type TreeEntry
} from '@ai-ide/shared-types'

import type { AiEvent, AiIdeApi } from '../../shared/ipc'
import {
  acceptCompletion,
  addChip,
  cancelStream,
  clearChat,
  closeCompletion,
  getAiState,
  handleAiEvent,
  initAi,
  moveCompletion,
  openCompletion,
  refreshModels,
  refreshProviders,
  removeChip,
  resetAiState,
  saveProvider,
  selectModel,
  sendMessage,
  testProvider
} from './ai-store'
import { closeWorkspace, getEditorState, openWorkspace } from './store'

interface Call {
  method: string
  params: Record<string, unknown> | undefined
}

const PROVIDER_ID = 'p-openai'
const MODEL_PK = 'm-1'
const WORKSPACE_ID = 'ws-1'
const WORKSPACE_ROOT = 'E:\\proj'

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

/** 一条假的内核目录条目（`path` 用绝对路径，外壳负责压成相对工作区根）。 */
function treeEntry(name: string, kind: TreeEntry['kind'], ignored = false, dir = ''): TreeEntry {
  return {
    name,
    path: `${WORKSPACE_ROOT}\\${dir}${name}`,
    kind,
    size: kind === 'dir' ? 0 : 16,
    mtime: 0,
    ignored
  }
}

/**
 * 假目录树：根一层 + `src/` 一层。
 * `node_modules` 与 `src/util.ts` 故意命中忽略规则，用来钉「灰显项不进候选」。
 */
function treeEntries(path: string | null): TreeEntry[] {
  if (path === 'src') {
    return [treeEntry('app.ts', 'file', false, 'src\\'), treeEntry('util.ts', 'file', true, 'src\\')]
  }
  if (path !== null && path.length > 0) return []
  return [
    treeEntry('src', 'dir'),
    treeEntry('main.ts', 'file'),
    treeEntry('README.md', 'file'),
    treeEntry('node_modules', 'dir', true)
  ]
}

/** 假的全库文件名检索结果（`search.files` 只做包含匹配）。 */
const SEARCH_MATCHES: FileMatch[] = [
  { path: 'src/app.ts', score: 9 },
  { path: 'docs/notes.md', score: 4 }
]

interface Harness {
  calls: Call[]
  /** 主进程 → 渲染进程的 M2 事件注入点。 */
  emit: (method: string, params: unknown) => void
  /** 让下一个请求失败；给了 `method` 就只让该方法的请求失败。 */
  failNext: (message: string, method?: string) => void
  setProviders: (providers: ProviderView[]) => void
  setModels: (models: ModelView[]) => void
  /** `llm.count_tokens` 的返回值，用来构造超限 / 不超限两条路径。 */
  setCountTokens: (tokens: number) => void
}

function installHarness(): Harness {
  const calls: Call[] = []
  let providers: ProviderView[] = [providerView()]
  let models: ModelView[] = [modelView()]
  let failure: { message: string; method: string | null } | null = null
  let countTokens = 42
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
      // 发送一条消息会先估 token 再开流：只想模拟「发送被拒」时必须指定方法，
      // 否则被拦掉的是估算那一步，反而让发送悄悄成功。
      if (failure && (failure.method === null || failure.method === method)) {
        const message = failure.message
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
        case Methods.llm.countTokens:
          return { tokens: countTokens, exact: false, note: null }
        case Methods.workspace.open:
          return {
            workspace_id: WORKSPACE_ID,
            root: WORKSPACE_ROOT,
            name: 'proj',
            stats: { files: 4, dirs: 2, ignored: 2 },
            indexing: false
          }
        case Methods.workspace.close:
          return { ok: true }
        case Methods.workspace.tree:
          return {
            entries: treeEntries((params as { path?: string | null } | undefined)?.path ?? null)
          }
        case Methods.search.files: {
          const query = ((params as { query?: string } | undefined)?.query ?? '').toLowerCase()
          return {
            matches: SEARCH_MATCHES.filter((match) => match.path.toLowerCase().includes(query))
          }
        }
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
    failNext: (message, method) => {
      failure = { message, method: method ?? null }
    },
    setProviders: (next) => {
      providers = next
    },
    setModels: (next) => {
      models = next
    },
    setCountTokens: (next) => {
      countTokens = next
    }
  }
}

let h: Harness

function callsOf(method: string): Call[] {
  return h.calls.filter((call) => call.method === method)
}

/** 打开假工作区：`@` 引用必须归属某个工作区，所以 M3 的用例都从这里起手。 */
async function openProject(): Promise<void> {
  await openWorkspace(WORKSPACE_ROOT)
  expect(getEditorState().workspace?.workspace_id).toBe(WORKSPACE_ID)
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
    h.failNext('RPC 失败：[2001] 未配置模型', Methods.llm.chatStream)
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

describe('@ 引用补全（M3）', () => {
  it('没有工作区时给可读提示，而不是弹一个空菜单', async () => {
    await closeWorkspace()
    await openCompletion('')

    const completion = getAiState().completion
    expect(completion.open).toBe(true)
    expect(completion.items).toHaveLength(0)
    expect(completion.error).toContain('先打开一个工作区')
  })

  it('敲 @ 后候选来自内核目录树，命中忽略规则的条目不出现', async () => {
    await openProject()
    await openCompletion('')

    const completion = getAiState().completion
    expect(completion.loading).toBe(false)
    const paths = completion.items.map((item) => item.path)
    expect(paths).toContain('main.ts')
    expect(paths).toContain('README.md')
    expect(paths).toContain('src')
    expect(paths).not.toContain('node_modules')
    expect(completion.items[0]?.kind).toBe('folder')
  })

  it('`@src/` 只列该目录，不再叠加全库模糊命中', async () => {
    await openProject()
    await openCompletion('src/')

    expect(getAiState().completion.items.map((item) => item.path)).toEqual(['src/app.ts'])
    expect(callsOf(Methods.search.files)).toHaveLength(0)
    // 目录前缀是交给内核 tree 的，外壳不自己遍历磁盘
    const treeCalls = callsOf(Methods.workspace.tree)
    expect(treeCalls[treeCalls.length - 1]?.params?.path).toBe('src')
  })

  it('没有目录前缀时叠加 search.files 的名字命中', async () => {
    await openProject()
    await openCompletion('app')

    // 根一层没有 app.*：命中的是 search.files 给出的 src/app.ts
    expect(getAiState().completion.items.map((item) => item.path)).toEqual(['src/app.ts'])
    expect(callsOf(Methods.search.files)).toHaveLength(1)
  })

  it('上下键选择会绕回，确认后生成 chip、关菜单，并从候选里去掉', async () => {
    await openProject()
    await openCompletion('')
    const total = getAiState().completion.items.length
    expect(total).toBeGreaterThan(1)

    moveCompletion(-1)
    expect(getAiState().completion.activeIndex).toBe(total - 1)
    moveCompletion(1)
    expect(getAiState().completion.activeIndex).toBe(0)

    const chip = acceptCompletion()
    expect(chip).not.toBeNull()
    expect(getAiState().chips).toEqual([chip])
    expect(getAiState().completion.open).toBe(false)

    await openCompletion('')
    expect(getAiState().completion.items.map((item) => item.path)).not.toContain(chip?.path)
  })

  it('关闭菜单后不残留候选', async () => {
    await openProject()
    await openCompletion('')
    closeCompletion()

    expect(getAiState().completion).toEqual({
      open: false,
      query: '',
      items: [],
      activeIndex: 0,
      loading: false,
      error: null
    })
  })

  it('同一路径重复加入只留一条（反斜杠写法归一后算同一条），移除后可再选', async () => {
    await openProject()
    expect(addChip({ kind: 'file', path: 'src/app.ts', label: 'app.ts' })).toBe(true)
    expect(addChip({ kind: 'file', path: 'src\\app.ts', label: 'app.ts' })).toBe(false)
    expect(getAiState().chips).toHaveLength(1)
    expect(getAiState().chips[0]?.path).toBe('src/app.ts')

    await openCompletion('app')
    expect(getAiState().completion.items).toHaveLength(0)

    removeChip('file', 'src/app.ts')
    expect(getAiState().chips).toHaveLength(0)
    await openCompletion('app')
    expect(getAiState().completion.items.map((item) => item.path)).toEqual(['src/app.ts'])
  })
})

describe('引用随请求透传与 token 预算（M3）', () => {
  it('发送时把 chip 按顺序构造成 context.refs', async () => {
    await openProject()
    addChip({ kind: 'file', path: 'src/app.ts', label: 'app.ts' })
    addChip({ kind: 'folder', path: 'src', label: 'src' })

    await sendMessage('看看这两处')

    const params = callsOf(Methods.llm.chatStream)[0]?.params as LlmChatStreamParams
    expect(params.context?.workspace_id).toBe(WORKSPACE_ID)
    expect(params.context?.refs.map((ref) => `${ref.kind}:${ref.path}`)).toEqual([
      'file:src/app.ts',
      'folder:src'
    ])
    expect(params.context?.compress).toBeNull()
    // 行区间与选区原文属后续切片，本轮一律为 null（整文件注入）
    expect(params.context?.refs.every((ref) => ref.start_line === null && ref.text === null)).toBe(
      true
    )
  })

  it('没有引用时 context 为 null：M2 的行为不能被 M3 改掉', async () => {
    await openProject()
    await sendMessage('普通提问')

    const params = callsOf(Methods.llm.chatStream)[0]?.params as LlmChatStreamParams
    expect(params.context).toBeNull()
  })

  it('发送前估算引用与消息，超限时给可读提示（但消息照发）', async () => {
    await openProject()
    h.setModels([modelView({ caps: { ...modelView().caps, context_window: 8_000 } })])
    await refreshModels(PROVIDER_ID)
    h.setCountTokens(12_345)
    addChip({ kind: 'file', path: 'src/app.ts', label: 'app.ts' })

    await sendMessage('帮我总结这个文件')

    const estimate = getAiState().tokenEstimate
    expect(estimate?.tokens).toBe(12_345)
    expect(estimate?.limit).toBe(8_000)
    expect(estimate?.exceeded).toBe(true)
    expect(estimate?.hint).toContain('上下文窗口')
    expect(estimate?.hint).toContain('Fake Mini')

    // 引用片段走 texts 口径、消息走 messages 口径
    const countCall = callsOf(Methods.llm.countTokens)[0]?.params as LlmCountTokensParams
    expect(countCall.texts).toEqual(['@file src/app.ts'])
    expect(countCall.messages[countCall.messages.length - 1]?.content).toBe('帮我总结这个文件')

    expect(callsOf(Methods.llm.chatStream)).toHaveLength(1)
  })

  it('未超限时安静通过：估算值本身不该打扰用户', async () => {
    await openProject()
    h.setCountTokens(120)
    await sendMessage('你好')

    const estimate = getAiState().tokenEstimate
    expect(estimate?.exceeded).toBe(false)
    expect(estimate?.hint).toBeNull()
  })

  it('估算失败不挡发送：它只是提前说一声，不能变成新的失败点', async () => {
    await openProject()
    h.failNext('RPC 失败：估算不可用', Methods.llm.countTokens)
    const sent = await sendMessage('你好')

    expect(sent).toBe(true)
    expect(getAiState().tokenEstimate).toBeNull()
    expect(callsOf(Methods.llm.chatStream)).toHaveLength(1)
  })

  it('加删引用或换模型都会作废旧估算（窗口变了，旧数字就是误导）', async () => {
    await openProject()
    h.setCountTokens(120)
    await sendMessage('你好')
    expect(getAiState().tokenEstimate).not.toBeNull()

    addChip({ kind: 'file', path: 'src/app.ts', label: 'app.ts' })
    expect(getAiState().tokenEstimate).toBeNull()
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
