/**
 * M2 渲染侧状态：Provider / 模型配置 + 对话流。
 *
 * 与 `store.ts` 同构（单例 + `useSyncExternalStore`），但职责分开：
 * 编辑器状态与 AI 状态互不干扰，避免一次对话刷新把工作区面板一起重渲染。
 *
 * 事件来源是主进程转发的 `provider.changed` 与 `llm.*`；流式正文靠 `llm.delta`
 * 逐段追加，结束时以 `llm.finished` 收口。**不按 `stream_id` 过滤的事件一律丢弃**，
 * 否则上一轮被取消的流会把文字打进新一轮对话里。
 */

import { useSyncExternalStore } from 'react'

import {
  Methods,
  type ChatMessage,
  type FinishReason,
  type LlmDeltaEvent,
  type LlmErrorEvent,
  type LlmFinishedEvent,
  type LlmStartedEvent,
  type LlmUsageEvent,
  type ModelInfo,
  type ModelRef,
  type ModelView,
  type ProviderProtocol,
  type ProviderView
} from '@ai-ide/shared-types'

import type { AiEvent, CredentialStatus } from '../../shared/ipc'
import { aiApi } from './ai-api'

/** 协议错误码 → 人话提示（Rust `error.rs` 的 2xxx 分区）。 */
const ERROR_HINTS: Record<number, string> = {
  2001: '还没配置 Provider 或没选模型：先去「Provider」页填一个再发。',
  2002: '鉴权失败：API Key 不对或已失效，也可能它没有该模型的权限。',
  2003: '被上游限流：等一会儿再发，或减少并发。',
  2004: '上下文超出模型窗口：清空对话或换更大窗口的模型。',
  2005: '连不上上游：检查 base_url、网络与代理。'
}

/** 把错误码翻译成可读提示；未知码返回空串（由调用方决定是否回落到原 message）。 */
export function hintForErrorCode(code: number): string {
  return ERROR_HINTS[code] ?? ''
}

/** 从主进程抛上来的错误文本里抠出协议错误码，如 `[2002] ...`。 */
export function errorCodeFromMessage(message: string): number | null {
  const matched = /\[(-?\d+)\]/.exec(message)
  if (!matched) return null
  const code = Number(matched[1])
  return Number.isFinite(code) ? code : null
}

/** 对话气泡。`streaming` 表示这一条还在长，`error` 表示这一条以错误收尾。 */
export interface ChatBubble {
  role: 'user' | 'assistant'
  text: string
  status: 'done' | 'streaming' | 'error'
  /** 错误或附加说明（如「已取消」）。 */
  note: string | null
}

export interface ProviderFormInput {
  id: string | null
  name: string
  protocol: ProviderProtocol
  base_url: string
  /** 留空表示「不改密钥」（沿用内核已存的那把）。 */
  api_key: string
  enabled: boolean
  timeout_ms: number | null
}

export interface AiState {
  providers: ProviderView[]
  models: ModelView[]
  /** 当前在设置页选中的 Provider（模型列表与对话模型都跟着它走）。 */
  activeProviderId: string | null
  credentials: CredentialStatus | null
  loading: boolean
  error: string | null
  test: { running: boolean; ok: boolean | null; text: string | null }
  /** 上游 `/models` 清单（`llm.list_models` 的结果），仅作添加提示。 */
  upstreamModels: ModelInfo[]
  selectedModel: ModelRef | null
  bubbles: ChatBubble[]
  streaming: boolean
  streamId: string | null
  usage: { promptTokens: number; completionTokens: number } | null
  finishReason: FinishReason | null
  /** 最近一条可读错误提示（异常路径呈现用）。 */
  hint: string | null
}

function initialState(): AiState {
  return {
    providers: [],
    models: [],
    activeProviderId: null,
    credentials: null,
    loading: false,
    error: null,
    test: { running: false, ok: null, text: null },
    upstreamModels: [],
    selectedModel: null,
    bubbles: [],
    streaming: false,
    streamId: null,
    usage: null,
    finishReason: null,
    hint: null
  }
}

let state: AiState = initialState()

const listeners = new Set<() => void>()

function setState(patch: Partial<AiState>): void {
  state = { ...state, ...patch }
  for (const listener of listeners) listener()
}

export function getAiState(): AiState {
  return state
}

export function subscribeAi(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function useAi(): AiState {
  return useSyncExternalStore(subscribeAi, getAiState, getAiState)
}

/** 单测与「重启内核后重连」时重置，避免上一轮的流状态粘住。 */
export function resetAiState(): void {
  state = initialState()
  for (const listener of listeners) listener()
}

function describe(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error)
  const code = errorCodeFromMessage(message)
  const hint = code !== null ? hintForErrorCode(code) : ''
  return hint.length > 0 && !message.includes(hint) ? `${message}｜${hint}` : message
}

// ---------------------------------------------------------------------------
// Provider / 模型
// ---------------------------------------------------------------------------

export async function refreshProviders(): Promise<void> {
  setState({ loading: true })
  try {
    const result = await aiApi.listProviders()
    const providers = result.providers ?? []
    const activeProviderId =
      state.activeProviderId && providers.some((item) => item.id === state.activeProviderId)
        ? state.activeProviderId
        : (providers[0]?.id ?? null)
    setState({ providers, activeProviderId, loading: false, error: null })
    await refreshModels(activeProviderId)
  } catch (error) {
    setState({ loading: false, error: describe(error) })
  }
}

export async function refreshModels(providerId: string | null): Promise<void> {
  if (!providerId) {
    setState({ models: [], selectedModel: null })
    return
  }
  try {
    const result = await aiApi.listModels(providerId)
    const models = result.models ?? []
    const keepSelected =
      state.selectedModel && state.selectedModel.provider_id === providerId
        ? state.selectedModel
        : null
    setState({
      models,
      selectedModel: keepSelected ?? (models[0] ? { provider_id: models[0].provider_id, model_id: models[0].model_id } : null)
    })
  } catch (error) {
    setState({ models: [], error: describe(error) })
  }
}

export function selectProvider(providerId: string | null): void {
  setState({ activeProviderId: providerId, upstreamModels: [], test: { running: false, ok: null, text: null } })
  void refreshModels(providerId)
}

/** 新建或更新 Provider，成功后刷新清单。返回是否成功。 */
export async function saveProvider(input: ProviderFormInput): Promise<boolean> {
  setState({ loading: true, error: null })
  try {
    const trimmedKey = input.api_key.trim()
    const result = await aiApi.upsertProvider({
      id: input.id,
      name: input.name.trim(),
      protocol: input.protocol,
      base_url: input.base_url.trim(),
      api_key: trimmedKey.length > 0 ? trimmedKey : null,
      headers: {},
      enabled: input.enabled,
      timeout_ms: input.timeout_ms
    })
    setState({ loading: false, activeProviderId: result.provider_id })
    await refreshProviders()
    return true
  } catch (error) {
    setState({ loading: false, error: describe(error) })
    return false
  }
}

export async function removeProvider(providerId: string): Promise<boolean> {
  setState({ loading: true, error: null })
  try {
    await aiApi.deleteProvider(providerId)
    if (state.activeProviderId === providerId) setState({ activeProviderId: null })
    await refreshProviders()
    return true
  } catch (error) {
    setState({ loading: false, error: describe(error) })
    return false
  }
}

export async function testProvider(providerId: string | null): Promise<void> {
  setState({ test: { running: true, ok: null, text: null } })
  try {
    const result = await aiApi.testProvider(providerId)
    setState({
      test: {
        running: false,
        ok: result.ok,
        text: result.ok
          ? `连接成功：${result.latency_ms}ms，发现 ${result.models_found} 个模型`
          : (result.message ?? '连接失败（上游未给出原因）')
      }
    })
  } catch (error) {
    const message = describe(error)
    const code = errorCodeFromMessage(message)
    const hint = code !== null ? hintForErrorCode(code) : ''
    setState({ test: { running: false, ok: false, text: hint.length > 0 ? hint : message } })
  }
}

export async function fetchUpstreamModels(providerId: string): Promise<void> {
  try {
    const result = await aiApi.listUpstreamModels(providerId)
    setState({ upstreamModels: result.models ?? [], error: null })
  } catch (error) {
    setState({ upstreamModels: [], error: describe(error) })
  }
}

export async function addModel(providerId: string, modelId: string, displayName: string): Promise<boolean> {
  setState({ error: null })
  try {
    await aiApi.upsertModel({
      provider_id: providerId,
      model_id: modelId.trim(),
      display_name: displayName.trim().length > 0 ? displayName.trim() : null,
      caps: null,
      params: null
    })
    await refreshModels(providerId)
    return true
  } catch (error) {
    setState({ error: describe(error) })
    return false
  }
}

export async function removeModel(modelPk: string): Promise<boolean> {
  try {
    await aiApi.deleteModel(modelPk)
    await refreshModels(state.activeProviderId)
    return true
  } catch (error) {
    setState({ error: describe(error) })
    return false
  }
}

export function selectModel(modelRef: ModelRef | null): void {
  setState({ selectedModel: modelRef })
}

// ---------------------------------------------------------------------------
// 对话
// ---------------------------------------------------------------------------

function patchLastAssistant(patch: (bubble: ChatBubble) => ChatBubble): ChatBubble[] {
  const bubbles = [...state.bubbles]
  for (let index = bubbles.length - 1; index >= 0; index -= 1) {
    const bubble = bubbles[index]
    if (bubble && bubble.role === 'assistant') {
      bubbles[index] = patch(bubble)
      return bubbles
    }
  }
  return bubbles
}

/** 免碰撞的流 id：时间戳 + 随机后缀，`llm.cancel` 靠它定位这条流。 */
export function newStreamId(): string {
  return `stream-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`
}

export async function sendMessage(text: string): Promise<boolean> {
  const content = text.trim()
  if (content.length === 0) return false
  if (!state.selectedModel) {
    setState({ hint: ERROR_HINTS[2001] ?? null, error: '请先选择模型' })
    return false
  }
  if (state.streaming) {
    setState({ hint: '上一条还在生成中，先等它结束或点「停止」。' })
    return false
  }

  const streamId = newStreamId()
  const messages: ChatMessage[] = [
    ...state.bubbles
      .filter((bubble) => bubble.status !== 'error' && bubble.text.trim().length > 0)
      .map<ChatMessage>((bubble) => ({
        role: bubble.role,
        content: bubble.text,
        attachments: [],
        tool_calls: [],
        tool_call_id: null
      })),
    { role: 'user', content, attachments: [], tool_calls: [], tool_call_id: null }
  ]

  setState({
    bubbles: [
      ...state.bubbles,
      { role: 'user', text: content, status: 'done', note: null },
      { role: 'assistant', text: '', status: 'streaming', note: null }
    ],
    streamId,
    streaming: true,
    hint: null,
    error: null,
    usage: null,
    finishReason: null
  })

  try {
    const ack = await aiApi.chatStream({
      stream_id: streamId,
      session_id: null,
      model_ref: state.selectedModel,
      messages,
      tools: [],
      tool_choice: null,
      params: null,
      // M3 的上下文引用目前只在内核侧落地（@文件/@文件夹 的 UI 接入属后续切片），
      // 这里显式传 null，保持 M2 的「不注入」行为。
      context: null
    })
    // 协议约定内核原样回执调用方给的 `stream_id`（`llm.chat_stream` 的时序是
    // 「先回 id 再开流」），这里仍然以回执为准对齐一次：万一哪天内核改成自己生成
    // id，事件过滤才不会因为「本地的 id 对不上」把所有正文都丢掉。
    if (ack?.stream_id && ack.stream_id !== streamId) {
      setState({ streamId: ack.stream_id })
    }
    return true
  } catch (error) {
    const message = describe(error)
    setState({
      streaming: false,
      streamId: null,
      hint: message,
      bubbles: patchLastAssistant((bubble) => ({ ...bubble, status: 'error', note: message }))
    })
    return false
  }
}

export async function cancelStream(): Promise<boolean> {
  const streamId = state.streamId
  if (!streamId) return false
  try {
    await aiApi.cancelStream(streamId)
    // 内核对取消**不发** `llm.finished`（用户自己按的停止不是故障），所以这里必须
    // 本地收口；否则「生成中…」会一直挂着，发送按钮永远保持禁用。
    setState({
      streaming: false,
      streamId: null,
      bubbles: patchLastAssistant((bubble) => ({
        ...bubble,
        status: 'done',
        note: bubble.text.trim().length > 0 ? '已取消' : '已取消（未产出内容）'
      }))
    })
    return true
  } catch (error) {
    setState({ hint: describe(error) })
    return false
  }
}

export function clearChat(): void {
  setState({ bubbles: [], usage: null, finishReason: null, hint: null, streaming: false, streamId: null })
}

// ---------------------------------------------------------------------------
// 事件接入
// ---------------------------------------------------------------------------

/** 处理一条主进程转发的 M2 事件。导出以便单测直接驱动。 */
export function handleAiEvent(event: AiEvent): void {
  const { method, params } = event

  if (method === Methods.event.providerChanged) {
    void refreshProviders()
    return
  }

  if (!method.startsWith('llm.')) return

  const payload = params as { stream_id?: string }
  // 只认当前这条流：被取消/过期的流不能往新对话里灌字。`state.streamId` 为 null
  // 表示「此刻没有在途的流」，那么带 stream_id 的事件一律丢弃（慢一步的 `llm.delta`
  // 可能在 `llm.finished` 之后才到，放进来会污染上一条气泡）。
  if (payload?.stream_id && payload.stream_id !== state.streamId) return

  switch (method) {
    case Methods.event.llmStarted: {
      const started = params as LlmStartedEvent
      setState({ streamId: started.stream_id, streaming: true, hint: null })
      return
    }
    case Methods.event.llmDelta: {
      const delta = params as LlmDeltaEvent
      setState({
        bubbles: patchLastAssistant((bubble) => ({ ...bubble, text: bubble.text + delta.text }))
      })
      return
    }
    case Methods.event.llmUsage: {
      const usage = params as LlmUsageEvent
      setState({
        usage: { promptTokens: usage.prompt_tokens, completionTokens: usage.completion_tokens }
      })
      return
    }
    case Methods.event.llmFinished: {
      const finished = params as LlmFinishedEvent
      setState({
        streaming: false,
        streamId: null,
        finishReason: finished.finish_reason,
        bubbles: patchLastAssistant((bubble) => ({ ...bubble, status: 'done' }))
      })
      return
    }
    case Methods.event.llmError: {
      const failure = params as LlmErrorEvent
      const hint = hintForErrorCode(failure.code)
      const text = hint.length > 0 ? hint : failure.message
      setState({
        streaming: false,
        streamId: null,
        hint: text,
        bubbles: patchLastAssistant((bubble) => ({ ...bubble, status: 'error', note: text }))
      })
      return
    }
    default:
      // `llm.reasoning` / `llm.tool_call` 暂不呈现（M2 不做推理与工具面板）。
      return
  }
}

let unsubscribe: (() => void) | null = null

/**
 * 订阅主进程事件并拉一次初值。
 *
 * 这里**总是**先退订再重订：`window.aiide` 可能在「内核重启 / 页面重载 / 测试里换了
 * 桩」之后指向另一份 preload，只靠 `unsubscribe` 是否为空做幂等，会让守卫在事件源
 * 换过之后误判「已经订阅过了」，于是新数据源的事件全丢——表现就是对话一直停在
 * 「生成中」，正文一个字也不进来。
 */
export function initAi(): void {
  disposeAi()
  unsubscribe = window.aiide.onAiEvent(handleAiEvent)
  void window.aiide
    .getCredentialStatus()
    .then((credentials) => setState({ credentials }))
    .catch(() => undefined)
  void refreshProviders()
}

export function disposeAi(): void {
  unsubscribe?.()
  unsubscribe = null
}
