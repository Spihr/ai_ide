/**
 * M5 渲染侧 API 封装：把 `window.aiide.request` 收窄成会话业务动作。
 *
 * 与 `ai-api.ts` 同一套路——组件不直接拼方法名与参数，避免「方法名拼错但到
 * 运行时才发现」。所有调用仍要过主进程的策略闸门（M5 十条已在白名单内）。
 *
 * 内核侧的 `Option` 字段一律显式传 `null`：协议里没有「字段缺省」的约定，
 * 少传一个字段是编解码错误，不是「用默认值」。
 */

import {
  Methods,
  type AuditListParams,
  type AuditListResult,
  type MessageView,
  type SessionAppendMessageParams,
  type SessionAppendMessageResult,
  type SessionCreateParams,
  type SessionCreateResult,
  type SessionDeleteParams,
  type SessionDeleteResult,
  type SessionExportFormat,
  type SessionExportParams,
  type SessionExportResult,
  type SessionForkParams,
  type SessionForkResult,
  type SessionGetParams,
  type SessionGetResult,
  type SessionListParams,
  type SessionListResult,
  type SessionMessagesParams,
  type SessionMessagesResult,
  type SessionUpdateParams,
  type SessionView
} from '@ai-ide/shared-types'

/** 会话列表默认分页：够一屏滚动，又不至于一次拉回几千条。 */
export const DEFAULT_SESSION_PAGE = 50

function request<T>(method: string, params: unknown): Promise<T> {
  return window.aiide.request(method, params) as Promise<T>
}

export const sessionApi = {
  create: (params: Partial<SessionCreateParams> = {}): Promise<SessionCreateResult> =>
    request<SessionCreateResult>(Methods.session.create, {
      title: params.title ?? null,
      workspace_id: params.workspace_id ?? null,
      model_ref: params.model_ref ?? null
    } satisfies SessionCreateParams),

  list: (params: Partial<SessionListParams> = {}): Promise<SessionListResult> =>
    request<SessionListResult>(Methods.session.list, {
      workspace_id: params.workspace_id ?? null,
      query: params.query ?? null,
      limit: params.limit ?? DEFAULT_SESSION_PAGE,
      offset: params.offset ?? 0
    } satisfies SessionListParams),

  get: (sessionId: string, includeMessages = true): Promise<SessionGetResult> =>
    request<SessionGetResult>(Methods.session.get, {
      session_id: sessionId,
      include_messages: includeMessages
    } satisfies SessionGetParams),

  /** 改名 / 换默认模型；内核要求两者至少给一个。 */
  update: async (params: SessionUpdateParams): Promise<SessionView> => {
    if (params.title === null && params.model_ref === null) {
      throw new Error('session.update 至少需要 title 或 model_ref 之一')
    }
    const result = await request<{ session: SessionView }>(Methods.session.update, params)
    return result.session
  },

  remove: (sessionIds: string[]): Promise<SessionDeleteResult> =>
    request<SessionDeleteResult>(Methods.session.delete, {
      session_ids: sessionIds
    } satisfies SessionDeleteParams),

  appendMessage: (params: SessionAppendMessageParams): Promise<SessionAppendMessageResult> =>
    request<SessionAppendMessageResult>(Methods.session.appendMessage, params),

  messages: (sessionId: string, limit = 200, offset = 0): Promise<SessionMessagesResult> =>
    request<SessionMessagesResult>(Methods.session.messages, {
      session_id: sessionId,
      limit,
      offset
    } satisfies SessionMessagesParams),

  exportSession: (
    sessionId: string,
    format: SessionExportFormat,
    outPath: string
  ): Promise<SessionExportResult> =>
    request<SessionExportResult>(Methods.session.export, {
      session_id: sessionId,
      format,
      out_path: outPath
    } satisfies SessionExportParams),

  fork: (
    sessionId: string,
    fromMessageId: string | null = null,
    title: string | null = null
  ): Promise<SessionForkResult> =>
    request<SessionForkResult>(Methods.session.fork, {
      session_id: sessionId,
      from_message_id: fromMessageId,
      title
    } satisfies SessionForkParams),

  auditList: (params: Partial<AuditListParams> = {}): Promise<AuditListResult> =>
    request<AuditListResult>(Methods.audit.list, {
      session_id: params.session_id ?? null,
      kind: params.kind ?? null,
      since_ms: params.since_ms ?? null,
      until_ms: params.until_ms ?? null,
      limit: params.limit ?? 20,
      offset: params.offset ?? 0
    } satisfies AuditListParams)
}

/**
 * 把消息视图折成对话面板能渲染的气泡角色。
 *
 * `system` / `tool` 不是对话气泡：前者是提示词上下文，后者是工具结果的回执，
 * 回放时折叠进它所属的那一轮，不单独占一个气泡。
 */
export function bubbleRoleOf(message: MessageView): 'user' | 'assistant' | null {
  if (message.role === 'user') return 'user'
  if (message.role === 'assistant') return 'assistant'
  return null
}

/** 会话标题缺省值：与内核 `新会话` 保持一致，便于「按标题找会话」时行为可预期。 */
export const DEFAULT_SESSION_TITLE = '新会话'
