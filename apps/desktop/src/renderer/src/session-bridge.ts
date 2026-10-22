/**
 * M5：对话流与会话落盘之间的那座桥。
 *
 * 存在的理由：`ai-store`（对话流）和 `session-store`（会话列表 / 历史）如果直接
 * 互相 import，必然成环——前者要「当前会话 id」，后者要「把历史消息灌回气泡」。
 * 所以把这两件共享的事（活动会话 id、追加消息）搬到一个只依赖 API 层的薄模块：
 *
 *   ai-store ──┐
 *              ├─→ session-bridge ─→ session-api ─→ window.aiide.request
 *   session-store ─┘
 *
 * 桥只做三件事，不含任何 UI 状态：
 * 1. 记住「现在这条对话属于哪个会话」（`activeSessionId`）；
 * 2. 发送时**先保证会话存在**：没有就建一个（绑定当前工作区 + 当前模型），
 *    这样用户不必先点「新建会话」才能保存历史；
 * 3. 把一条消息追加进该会话。落盘失败**绝不打断对话**——消息已经在屏幕上，
 *    失败的后果只是「这段历史没进库」，用一句提示告诉用户比抛错更合适。
 */

import type { ChatMessage, JsonValue, ModelRef, SessionView } from '@ai-ide/shared-types'

import { sessionApi } from './session-api'
import { getEditorState } from './store'

/**
 * 落盘结果。
 *
 * `messageId` 是「这条消息在库里的身份」——消息级分叉要以它为截断点，所以必须
 * 从这里带回到气泡上；只回一个布尔值的话，UI 手里就没有可分叉的锚点。
 */
export interface PersistResult {
  ok: boolean
  messageId: string | null
}

export interface SessionBridgeHooks {
  /** 桥自动建了一个会话（首条消息触发）：列表要插一条，并把它设为当前。 */
  onSessionCreated?: (session: SessionView) => void
  /** 活动会话被解除（用户清空对话 / 切到未落盘的新对话）：当前选中要落回 null。 */
  onSessionReleased?: () => void
  /** 落盘失败：把原因交给会话面板显示，不抛错、不挡对话。 */
  onPersistError?: (message: string) => void
}

let activeSessionId: string | null = null
let hooks: SessionBridgeHooks = {}

/** 最近一次上报过的错误：同一条原因连报会刷屏，压到只报一次。 */
let lastReportedError: string | null = null
/** 串行队列：追加消息必须按发生顺序入库，并发 `append_message` 会把顺序打乱。 */
let appendQueue: Promise<void> = Promise.resolve()

export function setSessionBridgeHooks(next: SessionBridgeHooks): void {
  hooks = { ...hooks, ...next }
}

export function getActiveSessionId(): string | null {
  return activeSessionId
}

/** 由 `session-store` 单向写入：它才是「当前选中会话」的权威，桥只是镜像。 */
export function setActiveSessionId(sessionId: string | null): void {
  activeSessionId = sessionId
}

/** 解除绑定：下一次发送会自动新建一个会话（用于「清空对话」= 开新话题）。 */
export function releaseActiveSession(): void {
  if (activeSessionId === null) return
  activeSessionId = null
  hooks.onSessionReleased?.()
}

function reportError(message: string): void {
  if (lastReportedError === message) return
  lastReportedError = message
  hooks.onPersistError?.(message)
}

/** 清掉错误去重标记：用户重新操作时应当允许再报一次。 */
export function clearPersistError(): void {
  lastReportedError = null
}

/**
 * 从首条用户消息生成标题。
 *
 * 取第一行、压掉连续空白、截到 40 字符（中英文都按字符数算）：既能在会话列表里
 * 一眼认出是哪一轮，又不会把列表撑成一堵墙。空文本退回内核的缺省标题。
 */
export function titleFromText(text: string): string | null {
  const line = text.trim().split(/\r?\n/, 1)[0]?.replace(/\s+/g, ' ').trim() ?? ''
  if (line.length === 0) return null
  return line.length > 40 ? `${line.slice(0, 40)}…` : line
}

/**
 * 保证当前有活动会话，没有就建一个。
 *
 * 绑定的工作区取编辑器此刻打开的那个（没有则建跨工作区会话）；模型取发送时选中的
 * 那个，之后会话就带着这个默认模型，历史回放时能显示「这条是什么模型答的」。
 */
async function ensureActiveSession(
  firstUserText: string,
  modelRef: ModelRef | null
): Promise<string | null> {
  if (activeSessionId !== null) return activeSessionId

  const workspace = getEditorState().workspace
  try {
    const created = await sessionApi.create({
      title: titleFromText(firstUserText),
      workspace_id: workspace?.workspace_id ?? null,
      model_ref: modelRef
    })
    activeSessionId = created.session_id
    lastReportedError = null
    hooks.onSessionCreated?.(created.session)
    return created.session_id
  } catch (error) {
    reportError(`会话未能落盘：${error instanceof Error ? error.message : String(error)}`)
    return null
  }
}

function toChatMessage(role: 'user' | 'assistant', text: string): ChatMessage {
  return {
    role,
    content: text,
    attachments: [],
    tool_calls: [],
    tool_call_id: null
  }
}

/**
 * 追加一条消息到当前会话。
 *
 * `modelRef` 只对 assistant 消息有意义（记录这轮回复是哪个模型产生的）；`usage`
 * 是上游原始用量，原样存下来，将来算账不用重放。失败只上报一次，不重试——
 * 重试会把一条消息写两遍的概率换成一个「历史里多一条重复消息」的结果，不值。
 */
export function persistChatMessage(
  role: 'user' | 'assistant',
  text: string,
  modelRef: ModelRef | null,
  usage: JsonValue | null = null
): Promise<PersistResult> {
  const content = text.trim()
  if (content.length === 0) return Promise.resolve({ ok: false, messageId: null })

  const job = appendQueue.then(async (): Promise<PersistResult> => {
    const sessionId = await ensureActiveSession(role === 'user' ? content : '', modelRef)
    if (sessionId === null) return { ok: false, messageId: null }
    try {
      const result = await sessionApi.appendMessage({
        session_id: sessionId,
        message: toChatMessage(role, content),
        parent_id: null,
        model_ref: role === 'assistant' ? modelRef : null,
        usage
      })
      return { ok: true, messageId: result.message?.message_id ?? null }
    } catch (error) {
      reportError(`消息未能落盘：${error instanceof Error ? error.message : String(error)}`)
      return { ok: false, messageId: null }
    }
  })

  // 队列自身不能因为某一环失败而断掉：吞掉异常，结果由返回值表达。
  appendQueue = job.then(
    () => undefined,
    () => undefined
  )
  return job
}
