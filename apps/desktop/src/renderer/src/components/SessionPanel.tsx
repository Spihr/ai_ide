/**
 * 会话面板（M5 外壳侧入口）：历史列表 + 当前会话的操作 + 审计回执。
 *
 * 一条原则：**这里只做「用户显式点了什么」的转发**。列表刷新、失败提示、动作进行中
 * 的禁用都在 `session-store` 里，组件不自己维护第二份状态——否则「刚建的会话在列表里
 * 但 store 里没有」这类不一致迟早出现。
 *
 * 流式生成中禁用会切换会话的按钮：切换会把对话面板的气泡整段换掉，正在生成的内容
 * 就无处可落（还会把那一轮落盘的消息接到别人身上）。
 */

import { useState } from 'react'

import type { SessionExportFormat, SessionView } from '@ai-ide/shared-types'

import { useAi } from '../ai-store'
import {
  AUDIT_KIND_FILTERS,
  auditKindLabel,
  isDeniedDecision,
  pluginAuditLine
} from '../audit-view'
import {
  createSession,
  deleteSession,
  dismissSessionHint,
  exportSession,
  forkSession,
  loadMoreAudit,
  loadMoreSessions,
  refreshAudit,
  renameSession,
  selectSession,
  setAuditKind,
  setAuditScope,
  setSessionQuery,
  useSessions
} from '../session-store'

const TIME_FORMAT = new Intl.DateTimeFormat('zh-CN', {
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit'
})

function formatTime(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return ''
  return TIME_FORMAT.format(new Date(ms))
}

/** 工作区目录名：会话跨工作区，标题之外得让人知道它是从哪个目录开的。 */
function workspaceLabel(path: string | null): string | null {
  if (!path) return null
  const parts = path.replace(/\\/g, '/').split('/').filter((part) => part.length > 0)
  return parts.length > 0 ? (parts[parts.length - 1] ?? null) : null
}

function wasEdited(view: SessionView): boolean {
  return view.updated_at_ms - view.created_at_ms > 1000
}

export function SessionPanel(): React.JSX.Element {
  const state = useSessions()
  const { streaming } = useAi()
  const [format, setFormat] = useState<SessionExportFormat>('markdown')
  const [editing, setEditing] = useState(false)
  const [titleDraft, setTitleDraft] = useState('')
  const [auditOpen, setAuditOpen] = useState(false)

  const current = state.current
  const busy = state.busy
  // 流式生成中一律不许换会话：气泡被替换后，这一轮的正文就落在别人的会话里了。
  const locked = streaming || busy

  const beginRename = (): void => {
    if (!current) return
    setTitleDraft(current.title)
    setEditing(true)
  }

  const commitRename = async (): Promise<void> => {
    if (!current) return
    const ok = await renameSession(current.session_id, titleDraft)
    if (ok) setEditing(false)
  }

  const toggleAudit = (): void => {
    const next = !auditOpen
    setAuditOpen(next)
    if (next) void refreshAudit(state.currentId)
  }

  return (
    <div className="ai-section" data-testid="session-panel">
      <h3 className="ai-section__title">会话</h3>

      <div className="ai-row">
        <input
          className="ai-input"
          data-testid="session-search"
          placeholder="搜标题或消息正文…"
          value={state.query}
          onChange={(event) => setSessionQuery(event.target.value)}
        />
        <button
          className="ai-button"
          type="button"
          data-testid="session-create"
          disabled={locked}
          title={streaming ? '生成中不能新建会话' : '新建一个空会话'}
          onClick={() => void createSession()}
        >
          新建
        </button>
        <button
          className="ai-button"
          type="button"
          data-testid="session-refresh"
          disabled={state.loading}
          onClick={() => setSessionQuery(state.query)}
        >
          刷新
        </button>
      </div>

      {state.error !== null ? (
        <div className="ai-result ai-result--bad" data-testid="session-error">
          {state.error}
        </div>
      ) : null}
      {state.hint !== null ? (
        <div className="ai-result ai-result--ok" data-testid="session-hint">
          {state.hint}
          <button
            className="ai-button ai-button--tiny"
            type="button"
            data-testid="session-hint-dismiss"
            onClick={dismissSessionHint}
          >
            知道了
          </button>
        </div>
      ) : null}

      <ul className="ai-list" data-testid="session-list">
        {state.sessions.length === 0 ? (
          <li className="ai-note">
            {state.loading ? '正在加载会话…' : '还没有会话：发一条消息会自动建一个，也可以点「新建」。'}
          </li>
        ) : (
          state.sessions.map((session) => {
            const active = session.session_id === state.currentId
            const scope = workspaceLabel(session.workspace_path)
            return (
              <li
                key={session.session_id}
                className="ai-list__item"
                data-testid="session-item"
                data-active={active ? 'true' : 'false'}
                style={active ? { color: 'var(--accent)' } : undefined}
              >
                <button
                  className="ai-button ai-button--tiny"
                  type="button"
                  data-testid="session-open"
                  disabled={locked && !active}
                  title={streaming ? '生成中不能切换会话' : `打开「${session.title}」`}
                  onClick={() => void selectSession(session.session_id)}
                >
                  {active ? '当前' : '打开'}
                </button>
                <span className="ai-list__meta" title={session.workspace_path ?? '跨工作区会话'}>
                  {session.title}
                  {scope ? ` · ${scope}` : ''}
                  {wasEdited(session) ? ` · ${formatTime(session.updated_at_ms)}` : ''}
                </span>
                <button
                  className="ai-button ai-button--tiny"
                  type="button"
                  data-testid="session-fork"
                  disabled={locked}
                  title="复制这条会话的全部消息到一个新会话"
                  onClick={() => void forkSession(session.session_id, null)}
                >
                  分叉
                </button>
                <button
                  className="ai-button ai-button--tiny ai-button--danger"
                  type="button"
                  data-testid="session-delete"
                  disabled={busy}
                  title="删除这条会话（消息一并删除，不可恢复）"
                  onClick={() => void deleteSession(session.session_id)}
                >
                  删除
                </button>
              </li>
            )
          })
        )}
      </ul>

      {state.total > state.sessions.length ? (
        <div className="ai-note">
          共 {state.total} 条，当前显示最近 {state.sessions.length} 条。
          <button
            className="ai-button ai-button--tiny"
            type="button"
            data-testid="session-load-more"
            disabled={state.loadingMore}
            title="再拉一页更早的会话"
            onClick={() => void loadMoreSessions()}
          >
            {state.loadingMore ? '加载中…' : '加载更多'}
          </button>
        </div>
      ) : null}

      {current !== null ? (
        <>
          <h4 className="ai-section__subtitle">当前会话</h4>

          {editing ? (
            <div className="ai-row">
              <input
                className="ai-input"
                data-testid="session-title-input"
                value={titleDraft}
                autoFocus
                onChange={(event) => setTitleDraft(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === 'Enter') void commitRename()
                  if (event.key === 'Escape') setEditing(false)
                }}
              />
              <button
                className="ai-button"
                type="button"
                data-testid="session-title-save"
                disabled={busy}
                onClick={() => void commitRename()}
              >
                保存
              </button>
              <button
                className="ai-button"
                type="button"
                data-testid="session-title-cancel"
                onClick={() => setEditing(false)}
              >
                取消
              </button>
            </div>
          ) : (
            <div className="ai-row">
              <span className="ai-list__meta" data-testid="session-current-title">
                {current.title}
              </span>
              <button
                className="ai-button ai-button--tiny"
                type="button"
                data-testid="session-rename"
                disabled={busy}
                onClick={beginRename}
              >
                改名
              </button>
            </div>
          )}

          <div className="ai-row">
            <select
              className="ai-input"
              data-testid="session-export-format"
              value={format}
              onChange={(event) => setFormat(event.target.value === 'json' ? 'json' : 'markdown')}
            >
              <option value="markdown">Markdown</option>
              <option value="json">JSON</option>
            </select>
            <button
              className="ai-button"
              type="button"
              data-testid="session-export"
              disabled={busy}
              // 落点由系统保存对话框决定：外壳只能给默认文件名与扩展名。
              title="导出当前会话（保存位置由系统对话框选择）"
              onClick={() => void exportSession(current.session_id, format)}
            >
              导出
            </button>
          </div>

        </>
      ) : (
        <div className="ai-note">
          没有选中会话：发一条消息会自动建一个（绑定当前工作区与模型）。
        </div>
      )}

      <h4 className="ai-section__subtitle">审计回执</h4>
      <div className="ai-row">
        <button
          className="ai-button"
          type="button"
          data-testid="session-audit-toggle"
          title="插件的能力变更与调用也记在这里（切到「全部（含插件）」）"
          onClick={toggleAudit}
        >
          {auditOpen ? '收起审计' : '展开审计'}
        </button>
        <span className="ai-note">
          {state.auditScope === 'session' ? '当前范围：本会话' : '当前范围：全部（含插件）'}
        </span>
      </div>
        {auditOpen ? (
          <>
            {/*
              插件事件（能力变更 / 插件调用）不挂会话，按会话过滤永远读不到——所以
              这里必须有「全部」这一档，否则 P4 的审计在 UI 上等于不存在。
              点插件类别的筛选项会自动把范围切到全局（见 session-store.setAuditKind）。
            */}
            <div className="ai-row" data-testid="session-audit-scope">
              <button
                className={`ai-button ai-button--tiny${state.auditScope === 'session' ? ' ai-button--primary' : ''}`}
                type="button"
                data-testid="session-audit-scope-session"
                disabled={state.currentId === null}
                title="只看这条会话的审计"
                onClick={() => void setAuditScope('session')}
              >
                本会话
              </button>
              <button
                className={`ai-button ai-button--tiny${state.auditScope === 'global' ? ' ai-button--primary' : ''}`}
                type="button"
                data-testid="session-audit-scope-global"
                title="看全部审计，包含不挂会话的插件事件"
                onClick={() => void setAuditScope('global')}
              >
                全部（含插件）
              </button>
              {AUDIT_KIND_FILTERS.filter((item) => item.kind !== null).map((item) => (
                <button
                  key={item.kind}
                  className={`ai-button ai-button--tiny${state.auditKind === item.kind ? ' ai-button--primary' : ''}`}
                  type="button"
                  data-testid={`session-audit-kind-${item.kind}`}
                  title={`只看 ${item.kind}`}
                  onClick={() =>
                    void setAuditKind(state.auditKind === item.kind ? null : item.kind)
                  }
                >
                  {item.label}
                </button>
              ))}
            </div>
            <ul className="ai-list" data-testid="session-audit">
              {state.audit.length === 0 ? (
                <li className="ai-note">
                  {state.auditScope === 'session'
                    ? '这条会话还没有审计事件。插件的能力变更与调用不挂会话，切到「全部（含插件）」看。'
                    : '这条会话还没有审计事件。'}
                </li>
              ) : (
                state.audit.map((event) => {
                  const line = pluginAuditLine(event)
                  const denied = isDeniedDecision(event)
                  return (
                    <li
                      key={event.event_id}
                      className={`ai-list__item ai-list__item--audit${denied ? ' ai-list__item--bad' : ''}`}
                      data-testid="session-audit-item"
                      data-kind={event.kind}
                      title={`${event.actor} · ${formatTime(event.created_at_ms)}`}
                    >
                      <span className="ai-list__meta">
                        {auditKindLabel(event.kind)} · {event.summary}
                      </span>
                      {line !== null ? (
                        <span className="ai-note" data-testid="session-audit-facts">
                          {line}
                        </span>
                      ) : null}
                      <span className="ai-note">{formatTime(event.created_at_ms)}</span>
                    </li>
                  )
                })
              )}
            {state.auditTotal > state.audit.length ? (
              <li className="ai-note">
                共 {state.auditTotal} 条，当前显示最近 {state.audit.length} 条。
                <button
                  className="ai-button ai-button--tiny"
                  type="button"
                  data-testid="session-audit-more"
                  disabled={state.auditLoading}
                  title="再拉一页更早的审计事件"
                  onClick={() => void loadMoreAudit()}
                >
                  {state.auditLoading ? '加载中…' : '加载更多'}
                </button>
              </li>
            ) : null}
          </ul>
          </>
        ) : null}
      )}
    </div>
  )
}
