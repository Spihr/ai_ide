/**
 * M5 渲染侧状态：会话列表 / 当前会话的消息 / 审计回执。
 *
 * 与 `ai-store` 分工：`ai-store` 管「正在发生的这一轮对话」（流式正文、用量、
 * 引用 chip），这里管「已经落盘的历史」。两者靠 `session-bridge` 单向衔接：
 *
 * - 这里改「当前会话」→ 写桥的 `activeSessionId`；
 * - 桥自动建会话 → 回调这里插入列表；
 * - 点开一条历史 → 把消息折成气泡灌回 `ai-store`（`loadBubbles`）。
 *
 * 仪表盘式的克制：所有动作都返回布尔表示「是否真的做成了」，失败原因落到
 * `hint`，UI 只负责显示，不再自己拼错误文案。
 */

import { useSyncExternalStore } from 'react'

import type {
  AuditEventView,
  MessageView,
  SessionExportFormat,
  SessionView
} from '@ai-ide/shared-types'

import { loadBubbles, getAiState, type ChatBubble } from './ai-store'
import { bubbleRoleOf, DEFAULT_SESSION_TITLE, sessionApi } from './session-api'
import {
  clearPersistError,
  releaseActiveSession,
  setActiveSessionId,
  setSessionBridgeHooks
} from './session-bridge'
import { getEditorState } from './store'

/** 会话列表一次拉多少条：与 `session.list` 的缺省分页一致。 */
const SESSION_PAGE = 50
/** 审计面板只看最近这些条：它是「刚才发生了什么」的回执，不是全量账本。 */
const AUDIT_PAGE = 20

export interface SessionState {
  sessions: SessionView[]
  total: number
  /** 标题过滤词；空串表示不过滤。 */
  query: string
  currentId: string | null
  current: SessionView | null
  messages: MessageView[]
  messagesTotal: number
  audit: AuditEventView[]
  auditTotal: number
  /** 列表首次加载 / 刷新中。 */
  loading: boolean
  /** 「加载更多」进行中：与首屏 `loading` 分开，否则翻页会把整列表打成占位文案。 */
  loadingMore: boolean
  /** 审计分页加载中。 */
  auditLoading: boolean
  /** 某个会话动作（切换 / 导出 / 分叉 / 删除）进行中：UI 据此禁用按钮。 */
  busy: boolean
  hint: string | null
  error: string | null
}

function initialState(): SessionState {
  return {
    sessions: [],
    total: 0,
    query: '',
    currentId: null,
    current: null,
    messages: [],
    messagesTotal: 0,
    audit: [],
    auditTotal: 0,
    loading: false,
    loadingMore: false,
    auditLoading: false,
    busy: false,
    hint: null,
    error: null
  }
}

let state: SessionState = initialState()
const listeners = new Set<() => void>()

function setState(patch: Partial<SessionState>): void {
  state = { ...state, ...patch }
  for (const listener of listeners) listener()
}

export function getSessionState(): SessionState {
  return state
}

export function subscribeSessions(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function useSessions(): SessionState {
  return useSyncExternalStore(subscribeSessions, getSessionState, getSessionState)
}

/** 单测与「重启内核后重连」时重置。 */
export function resetSessionState(): void {
  state = initialState()
  for (const listener of listeners) listener()
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

/** 文件系统的非法字符：标题会直接进导出文件名，不清理就是保存失败。 */
function safeFileName(title: string): string {
  const cleaned = title
    .replace(/[\\/:*?"<>|]/g, ' ')
    .replace(/\s+/g, ' ')
    .trim()
  return cleaned.length > 0 ? cleaned.slice(0, 80) : DEFAULT_SESSION_TITLE
}

/**
 * 消息 → 气泡。
 *
 * `system` / `tool` 不进对话面板（提示词上下文与工具回执不是「谁说了什么」），
 * 空正文的 assistant 也跳过：它是「这轮没答出内容」的残留，回放时只会显示一个
 * 空气泡，看着像渲染坏了。
 */
export function bubblesOf(messages: MessageView[]): ChatBubble[] {
  const bubbles: ChatBubble[] = []
  for (const message of messages) {
    const role = bubbleRoleOf(message)
    if (!role) continue
    const text = message.content.trim()
    if (text.length === 0) continue
    // 带上 message_id：消息级分叉要以某条历史消息为截断点，气泡上没有 id 就无从下手。
    bubbles.push({
      role,
      text: message.content,
      status: 'done',
      note: null,
      messageId: message.message_id
    })
  }
  return bubbles
}

/** 清空「当前会话」的一切：选中态、消息、对话面板里的气泡。 */
function resetCurrent(): void {
  setState({ currentId: null, current: null, messages: [], messagesTotal: 0, audit: [], auditTotal: 0 })
  loadBubbles([])
}

// ---------------------------------------------------------------------------
// 列表
// ---------------------------------------------------------------------------

/**
 * 拉会话列表。
 *
 * **故意不按 `workspace_id` 过滤**：会话一旦按工作区过滤，切个工作区就像「历史全丢了」；
 * 跨工作区会话更是根本看不到。列表项里带上 `workspace_path`，由 UI 显示目录名区分即可。
 */
export async function refreshSessions(query = state.query): Promise<boolean> {
  setState({ loading: true, loadingMore: false, error: null })
  try {
    const result = await sessionApi.list({
      workspace_id: null,
      query: query.trim().length > 0 ? query.trim() : null,
      limit: SESSION_PAGE,
      offset: 0
    })
    const sessions = result.sessions ?? []
    // 当前会话可能因为过滤词被筛掉了：选中态保留，但列表里已经没有它，
    // 这里不动选中态（用户只是搜了一下，不该把正在看的对话丢掉）。
    setState({ sessions, total: result.total ?? sessions.length, query, loading: false })
    return true
  } catch (error) {
    setState({ loading: false, error: describe(error) })
    return false
  }
}

/** 设置标题过滤词并重拉列表（输入即搜，防抖交给 UI）。 */
export function setSessionQuery(query: string): void {
  void refreshSessions(query)
}

/**
 * 列表翻页：把下一页追加到现有列表尾部。
 *
 * offset 直接用 `sessions.length`——列表恒为「从 0 开始的连续页」，中间没有删洞；
 * 期间若别的窗口删了会话导致重复，按 id 去重兜住（重复项比漏项更难看出错）。
 * `total` 是内核在**同一过滤条件**下数出来的，所以「已加载 >= total」就是到底了。
 */
export async function loadMoreSessions(): Promise<boolean> {
  if (state.loading || state.loadingMore) return false
  const offset = state.sessions.length
  if (offset >= state.total) return false
  setState({ loadingMore: true, error: null })
  try {
    const query = state.query.trim()
    const result = await sessionApi.list({
      workspace_id: null,
      query: query.length > 0 ? query : null,
      limit: SESSION_PAGE,
      offset
    })
    const known = new Set(state.sessions.map((item) => item.session_id))
    const extra = (result.sessions ?? []).filter((item) => !known.has(item.session_id))
    setState({
      loadingMore: false,
      sessions: [...state.sessions, ...extra],
      total: result.total ?? state.total
    })
    return true
  } catch (error) {
    setState({ loadingMore: false, error: describe(error) })
    return false
  }
}

// ---------------------------------------------------------------------------
// 当前会话
// ---------------------------------------------------------------------------

/** 打开一条会话：拉全量消息、设为活动会话、把历史灌回对话面板。 */
export async function selectSession(sessionId: string): Promise<boolean> {
  if (state.busy) return false
  setState({ busy: true, error: null })
  try {
    const result = await sessionApi.get(sessionId, true)
    const messages = result.messages ?? []
    clearPersistError()
    setState({
      busy: false,
      currentId: sessionId,
      current: result.session,
      messages,
      messagesTotal: messages.length,
      hint: null
    })
    setActiveSessionId(sessionId)
    loadBubbles(bubblesOf(messages))
    void refreshAudit(sessionId)
    return true
  } catch (error) {
    setState({ busy: false, error: describe(error) })
    return false
  }
}

/**
 * 新建会话并选中。
 *
 * 标题交给内核生成（`新会话`），而不是外壳造一个「新会话 1/2/3」：用户发第一条消息后
 * 会话标题不会自动改，所以宁可保持一个中性名字，让用户自己改名。
 */
export async function createSession(): Promise<boolean> {
  if (state.busy) return false
  setState({ busy: true, error: null })
  try {
    const workspace = getEditorState().workspace
    const created = await sessionApi.create({
      title: null,
      workspace_id: workspace?.workspace_id ?? null,
      model_ref: getAiState().selectedModel
    })
    clearPersistError()
    setState({
      busy: false,
      sessions: [created.session, ...state.sessions.filter((item) => item.session_id !== created.session_id)],
      total: state.total + 1,
      currentId: created.session_id,
      current: created.session,
      messages: [],
      messagesTotal: 0,
      audit: [],
      auditTotal: 0,
      hint: null
    })
    setActiveSessionId(created.session_id)
    loadBubbles([])
    return true
  } catch (error) {
    setState({ busy: false, error: describe(error) })
    return false
  }
}

/**
 * 开始一段新对话（不清库）。
 *
 * 「清空对话」的语义是「换个话题」，不是「删掉历史」——所以这里只是解除绑定，
 * 下一条消息落盘时由桥自动建一个新会话。想真删要用 `deleteSession`。
 */
export function startNewChat(): void {
  clearPersistError()
  resetCurrent()
  releaseActiveSession()
  setActiveSessionId(null)
}

/** 改名。空标题没有意义，直接拒。 */
export async function renameSession(sessionId: string, title: string): Promise<boolean> {
  const trimmed = title.trim()
  if (trimmed.length === 0) {
    setState({ hint: '会话标题不能为空。' })
    return false
  }
  setState({ busy: true, error: null })
  try {
    const updated = await sessionApi.update({ session_id: sessionId, title: trimmed, model_ref: null })
    setState({
      busy: false,
      hint: null,
      sessions: state.sessions.map((item) => (item.session_id === sessionId ? updated : item)),
      current: state.currentId === sessionId ? updated : state.current
    })
    return true
  } catch (error) {
    setState({ busy: false, error: describe(error) })
    return false
  }
}

/**
 * 删除会话。
 *
 * 删的如果是当前会话，面板要立刻退回到「没有选中」——留着一个已删除会话的标题
 * 和消息，用户下一条消息会追加进一个已经不存在的 id（内核报 not found）。
 */
export async function deleteSession(sessionId: string): Promise<boolean> {
  setState({ busy: true, error: null })
  try {
    const result = await sessionApi.remove([sessionId])
    const deletingCurrent = state.currentId === sessionId
    setState({
      busy: false,
      hint: `已删除 ${result.deleted} 个会话。`,
      sessions: state.sessions.filter((item) => item.session_id !== sessionId),
      total: Math.max(0, state.total - result.deleted)
    })
    if (deletingCurrent) {
      resetCurrent()
      setActiveSessionId(null)
    }
    return true
  } catch (error) {
    setState({ busy: false, error: describe(error) })
    return false
  }
}

/**
 * 导出会话。
 *
 * 落点只能由系统保存对话框产生（主进程 `dialog:pick-save-path`）：外壳能提供的
 * 只有默认文件名与扩展名。用户取消对话框时静默返回 false，不当作错误。
 */
export async function exportSession(
  sessionId: string,
  format: SessionExportFormat
): Promise<boolean> {
  const session = state.sessions.find((item) => item.session_id === sessionId) ?? state.current
  const title = session?.title ?? DEFAULT_SESSION_TITLE
  const extension = format === 'json' ? 'json' : 'md'
  setState({ busy: true, error: null })
  try {
    const outPath = await window.aiide.pickSavePath(`${safeFileName(title)}.${extension}`, [extension])
    if (!outPath) {
      setState({ busy: false })
      return false
    }
    const result = await sessionApi.exportSession(sessionId, format, outPath)
    setState({
      busy: false,
      hint: `已导出 ${result.messages} 条消息到 ${result.path}（${result.bytes} 字节）。`
    })
    return true
  } catch (error) {
    setState({ busy: false, error: describe(error) })
    return false
  }
}

/**
 * 从某条消息分叉出新会话。
 *
 * 走一遍 `selectSession` 而不是信任 `fork` 的返回值：分叉结果只带回新会话快照与
 * 消息条数，正文还得拉一次；顺手也让「活动会话 id」与气泡同步，不留半截状态。
 */
export async function forkSession(
  sessionId: string,
  fromMessageId: string | null = null
): Promise<boolean> {
  if (state.busy) return false
  setState({ busy: true, error: null })
  try {
    const forked = await sessionApi.fork(sessionId, fromMessageId, null)
    setState({
      busy: false,
      sessions: [
        forked.session,
        ...state.sessions.filter((item) => item.session_id !== forked.new_session_id)
      ],
      total: state.total + 1
    })
    const selected = await selectSession(forked.new_session_id)
    // 提示要等切换完成后才写：`selectSession` 成功时会清 `hint`，
    // 先写就会被自己清掉，用户看不到「分叉成功」这条回执。
    if (selected) {
      setState({ hint: `已分叉出「${forked.session.title}」，复制 ${forked.messages} 条消息。` })
    }
    return selected
  } catch (error) {
    setState({ busy: false, error: describe(error) })
    return false
  }
}

// ---------------------------------------------------------------------------
// 审计
// ---------------------------------------------------------------------------

/** 拉当前会话的审计回执；`session_id` 为空时看全局（含跨会话事件）。 */
export async function refreshAudit(sessionId: string | null = state.currentId): Promise<boolean> {
  try {
    const result = await sessionApi.auditList({ session_id: sessionId, limit: AUDIT_PAGE, offset: 0 })
    setState({ audit: result.events ?? [], auditTotal: result.total ?? 0 })
    return true
  } catch (error) {
    setState({ audit: [], auditTotal: 0, error: describe(error) })
    return false
  }
}

/**
 * 审计翻页。审计是**追加式的账本**（新的在最前），翻页期间来了新事件会让 offset
 * 窗口错位；这里按 `event_id` 去重，重复条目不会再插一遍。
 */
export async function loadMoreAudit(): Promise<boolean> {
  if (state.auditLoading) return false
  const offset = state.audit.length
  if (offset >= state.auditTotal) return false
  setState({ auditLoading: true, error: null })
  try {
    const result = await sessionApi.auditList({
      session_id: state.currentId,
      limit: AUDIT_PAGE,
      offset
    })
    const known = new Set(state.audit.map((event) => event.event_id))
    const extra = (result.events ?? []).filter((event) => !known.has(event.event_id))
    setState({
      auditLoading: false,
      audit: [...state.audit, ...extra],
      auditTotal: result.total ?? state.auditTotal
    })
    return true
  } catch (error) {
    setState({ auditLoading: false, error: describe(error) })
    return false
  }
}

/** 清掉一次性提示（UI 关闭提示条时调用）。 */
export function dismissSessionHint(): void {
  if (state.hint === null && state.error === null) return
  setState({ hint: null, error: null })
}

// ---------------------------------------------------------------------------
// 初始化
// ---------------------------------------------------------------------------

let bridgeBound = false

/**
 * 绑定桥的回调并拉一次初值。
 *
 * `bridgeBound` 只防「同一份模块重复 init」；`window.aiide` 换代（内核重启 / 页面
 * 重载）不影响这里——桥的回调是模块级的，不挂在任何 preload 对象上。
 */
export function initSessions(): void {
  if (!bridgeBound) {
    setSessionBridgeHooks({
      onSessionCreated: (session) => {
        // 桥在发第一条消息时自动建的会话：插到列表头并设为当前，用户不必手动刷新。
        setState({
          sessions: [session, ...state.sessions.filter((item) => item.session_id !== session.session_id)],
          total: state.total + 1,
          currentId: session.session_id,
          current: session,
          messages: [],
          messagesTotal: 0,
          audit: [],
          auditTotal: 0,
          hint: null,
          error: null
        })
      },
      onSessionReleased: () => {
        resetCurrent()
      },
      onPersistError: (message) => {
        setState({ hint: message })
      }
    })
    bridgeBound = true
  }
  void refreshSessions()
}
