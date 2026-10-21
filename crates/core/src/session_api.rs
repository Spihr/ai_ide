//! 会话与消息的协议处理器（M5 切片二）。
//!
//! 这一层只做三件事：解析参数、把协议字段折成 [`ai_ide_store`] 的入参、
//! 把仓储错误翻成错误码。落盘语义（事务、级联删除、`updated_at` 只增不减）全在
//! `crates/store` 里，这里不写 SQL，也不碰文件系统。
//!
//! 会话 `id` 由 store 生成（UUID v4），协议层不发放 id——否则「同一个会话被两条
//! 并发请求创建」这类竞态就得由上层自己兜。

use ai_ide_protocol::dto::{
    MessageView, SessionAppendMessageParams, SessionAppendMessageResult, SessionCreateParams,
    SessionCreateResult, SessionDeleteParams, SessionDeleteResult, SessionGetParams,
    SessionGetResult, SessionListParams, SessionListResult, SessionMessagesParams,
    SessionMessagesResult, SessionUpdateParams, SessionView,
};
use ai_ide_protocol::{ErrorCode, ErrorObject, Request};
use ai_ide_store::{Message, NewMessage, NewSession, Session, SessionQuery, Store, StoreError};
use serde_json::json;

use crate::dispatch::parse_params;
use crate::peer::Peer;
use crate::state::{now_ms, AppState};

/// 客户端不给标题时的兜底，保证列表里永远有话可说。
const DEFAULT_TITLE: &str = "新会话";
/// `session.list` 的默认 / 最大返回条数。
const LIST_DEFAULT_LIMIT: u32 = 50;
const LIST_MAX_LIMIT: u32 = 500;
/// `session.messages` 的默认 / 最大返回条数：长对话必须能分页读，不能一帧拉全量。
const MESSAGES_DEFAULT_LIMIT: u32 = 200;
const MESSAGES_MAX_LIMIT: u32 = 10_000;

// ---------------------------------------------------------------------------
// session.create
// ---------------------------------------------------------------------------

pub async fn create(peer: &Peer, state: &AppState, req: &Request) {
    let params: SessionCreateParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let workspace_path = match workspace_path_of(state, params.workspace_id.as_deref()) {
        Ok(path) => path,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let mut new = NewSession::new(normalize_title(params.title.as_deref()));
    if let Some(path) = workspace_path {
        new = new.with_workspace_path(path);
    }
    if let Some(model_ref) = params.model_ref {
        new = new.with_model_ref(model_ref);
    }

    match store.create_session(new).await {
        Ok(session) => {
            let result = SessionCreateResult {
                session_id: session.id.clone(),
                session: to_view(&session),
            };
            peer.respond(&req.id, &result).await;
        }
        Err(err) => peer.respond_error(&req.id, &map_store_error(err)).await,
    }
}

// ---------------------------------------------------------------------------
// session.list
// ---------------------------------------------------------------------------

pub async fn list(peer: &Peer, state: &AppState, req: &Request) {
    let params: SessionListParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let workspace_path = match workspace_path_of(state, params.workspace_id.as_deref()) {
        Ok(path) => path,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let limit = params
        .limit
        .unwrap_or(LIST_DEFAULT_LIMIT)
        .clamp(1, LIST_MAX_LIMIT);
    let offset = params.offset.unwrap_or(0);
    let keyword = params
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_lowercase);

    // 标题过滤在 store 里没有对应 SQL（它只按工作区过滤），所以带关键词时先取
    // 一页最多 500 条再在内存里过滤。单机 IDE 的会话量远小于这个量级；分页仍按
    // 「过滤之后」切，语义不会错位（不会出现过滤后 total 与页内容对不上）。
    let (sessions, total) = match &keyword {
        Some(keyword) => {
            let probe = SessionQuery {
                workspace_path: workspace_path.as_deref(),
                limit: LIST_MAX_LIMIT as i64,
                offset: 0,
            };
            match store.list_sessions(probe).await {
                Ok(rows) => {
                    let hits: Vec<Session> = rows
                        .into_iter()
                        .filter(|row| row.title.to_lowercase().contains(keyword))
                        .collect();
                    let total = hits.len() as u64;
                    let page = hits
                        .into_iter()
                        .skip(offset as usize)
                        .take(limit as usize)
                        .collect::<Vec<_>>();
                    (page, total)
                }
                Err(err) => return peer.respond_error(&req.id, &map_store_error(err)).await,
            }
        }
        None => {
            let query = SessionQuery {
                workspace_path: workspace_path.as_deref(),
                limit: limit as i64,
                offset: offset as i64,
            };
            let rows = match store.list_sessions(query).await {
                Ok(rows) => rows,
                Err(err) => return peer.respond_error(&req.id, &map_store_error(err)).await,
            };
            let total = match store.count_sessions(workspace_path.as_deref()).await {
                Ok(total) => total as u64,
                Err(err) => return peer.respond_error(&req.id, &map_store_error(err)).await,
            };
            (rows, total)
        }
    };

    let result = SessionListResult {
        sessions: sessions.iter().map(to_view).collect(),
        total,
    };
    peer.respond(&req.id, &result).await;
}

// ---------------------------------------------------------------------------
// session.get
// ---------------------------------------------------------------------------

pub async fn get(peer: &Peer, state: &AppState, req: &Request) {
    let params: SessionGetParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let session = match load_session(store, &params.session_id).await {
        Ok(session) => session,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let mut messages = Vec::new();
    if params.include_messages {
        // 条数先数一遍再取：`list_messages` 的 limit 会被 store 夹到自身上限，
        // 直接传一个大数会把「历史被截断」这件事藏起来，客户端无从判断。
        let total = match store.count_messages(&session.id).await {
            Ok(total) => total,
            Err(err) => return peer.respond_error(&req.id, &map_store_error(err)).await,
        };
        if total > 0 {
            match store.list_messages(&session.id, total, 0).await {
                Ok(rows) => messages = rows.iter().map(to_message_view).collect(),
                Err(err) => return peer.respond_error(&req.id, &map_store_error(err)).await,
            }
        }
    }

    let result = SessionGetResult {
        session: to_view(&session),
        messages,
    };
    peer.respond(&req.id, &result).await;
}

// ---------------------------------------------------------------------------
// session.update
// ---------------------------------------------------------------------------

pub async fn update(peer: &Peer, state: &AppState, req: &Request) {
    let params: SessionUpdateParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let title = params.title.as_deref().map(str::trim);
    if title.is_none() && params.model_ref.is_none() {
        let err = ErrorObject::invalid_params("title 与 model_ref 至少要给一个");
        return peer.respond_error(&req.id, &err).await;
    }
    if let Some(title) = title {
        if title.is_empty() {
            let err = ErrorObject::invalid_params("title 不能是空白字符串");
            return peer.respond_error(&req.id, &err).await;
        }
    }

    if let Some(title) = title {
        match store
            .rename_session(&params.session_id, title, now_ms())
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                return peer
                    .respond_error(&req.id, &session_not_found(&params.session_id))
                    .await
            }
            Err(err) => return peer.respond_error(&req.id, &map_store_error(err)).await,
        }
    }
    if let Some(model_ref) = params.model_ref {
        match store
            .set_session_model(&params.session_id, &model_ref, now_ms())
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                return peer
                    .respond_error(&req.id, &session_not_found(&params.session_id))
                    .await
            }
            Err(err) => return peer.respond_error(&req.id, &map_store_error(err)).await,
        }
    }

    // 回最新快照，客户端不必再补一次 `session.get` 就能刷新列表项。
    match load_session(store, &params.session_id).await {
        Ok(session) => peer.respond(&req.id, &to_view(&session)).await,
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

// ---------------------------------------------------------------------------
// session.delete
// ---------------------------------------------------------------------------

pub async fn delete(peer: &Peer, state: &AppState, req: &Request) {
    let params: SessionDeleteParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    if params.session_ids.is_empty() {
        let err = ErrorObject::invalid_params("session_ids 不能为空");
        return peer.respond_error(&req.id, &err).await;
    }

    let mut deleted = 0u64;
    for id in &params.session_ids {
        match store.delete_session(id).await {
            Ok(true) => deleted += 1,
            // 不存在的 id 静默跳过：删除是幂等的，客户端重试不该报错。
            Ok(false) => {}
            Err(err) => return peer.respond_error(&req.id, &map_store_error(err)).await,
        }
    }

    let result = SessionDeleteResult { ok: true, deleted };
    peer.respond(&req.id, &result).await;
}

// ---------------------------------------------------------------------------
// session.append_message
// ---------------------------------------------------------------------------

pub async fn append_message(peer: &Peer, state: &AppState, req: &Request) {
    let params: SessionAppendMessageParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let mut new = NewMessage::from_chat(&params.session_id, &params.message);
    if let Some(parent_id) = params.parent_id {
        new = new.with_parent(parent_id);
    }
    if let Some(model_ref) = params.model_ref {
        new = new.with_model_ref(model_ref);
    }
    if let Some(usage) = params.usage {
        new = new.with_usage(usage);
    }

    // 会话存在性由 store 在事务里校验：这里先查一次反而会留下「查完之后会话被删」
    // 的窗口，最终仍要靠 `SessionNotFound` 兜底。
    match store.append_message(new).await {
        Ok(message) => {
            let result = SessionAppendMessageResult {
                message: to_message_view(&message),
            };
            peer.respond(&req.id, &result).await;
        }
        Err(err) => peer.respond_error(&req.id, &map_store_error(err)).await,
    }
}

// ---------------------------------------------------------------------------
// session.messages
// ---------------------------------------------------------------------------

pub async fn messages(peer: &Peer, state: &AppState, req: &Request) {
    let params: SessionMessagesParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    // 空会话与「不存在的会话」都返回零条消息就分不清了，所以先确认会话存在。
    if let Err(err) = load_session(store, &params.session_id).await {
        return peer.respond_error(&req.id, &err).await;
    }

    let limit = params
        .limit
        .unwrap_or(MESSAGES_DEFAULT_LIMIT)
        .clamp(1, MESSAGES_MAX_LIMIT);
    let offset = params.offset.unwrap_or(0);

    let total = match store.count_messages(&params.session_id).await {
        Ok(total) => total as u64,
        Err(err) => return peer.respond_error(&req.id, &map_store_error(err)).await,
    };
    let rows = match store
        .list_messages(&params.session_id, limit as i64, offset as i64)
        .await
    {
        Ok(rows) => rows,
        Err(err) => return peer.respond_error(&req.id, &map_store_error(err)).await,
    };

    let result = SessionMessagesResult {
        messages: rows.iter().map(to_message_view).collect(),
        total,
    };
    peer.respond(&req.id, &result).await;
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

/// 取会话库；未就绪时返回 5002（内核能跑，只是持久化不可用）。
fn store_of(state: &AppState) -> Result<&Store, ErrorObject> {
    state.store().ok_or_else(|| {
        ErrorObject::new(
            ErrorCode::STORE_UNAVAILABLE,
            "会话库未就绪：落盘目录不可写或迁移失败",
        )
    })
}

/// 把协议里的 `workspace_id` 折成落盘的绝对路径。
///
/// 落盘用正斜杠：Windows 下 `Workspace::root` 是反斜杠，同一台机器换种拼写就会
/// 被当成两个工作区，跨平台迁移更是直接失配。
fn workspace_path_of(
    state: &AppState,
    workspace_id: Option<&str>,
) -> Result<Option<String>, ErrorObject> {
    match workspace_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => match state.workspaces().get(id) {
            Ok(workspace) => Ok(Some(workspace.root.to_string_lossy().replace('\\', "/"))),
            Err(err) => Err(ErrorObject::new(
                ErrorCode::WORKSPACE_NOT_OPENED,
                format!("工作区未打开: {err}"),
            )),
        },
        None => Ok(None),
    }
}

/// 标题归一化：空白一律回落默认值。
fn normalize_title(title: Option<&str>) -> String {
    title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(DEFAULT_TITLE)
        .to_string()
}

async fn load_session(store: &Store, session_id: &str) -> Result<Session, ErrorObject> {
    match store.get_session(session_id).await {
        Ok(Some(session)) => Ok(session),
        Ok(None) => Err(session_not_found(session_id)),
        Err(err) => Err(map_store_error(err)),
    }
}

fn session_not_found(session_id: &str) -> ErrorObject {
    ErrorObject::new(
        ErrorCode::SESSION_NOT_FOUND,
        format!("会话不存在: {session_id}"),
    )
    .with_data(json!({ "session_id": session_id }))
}

/// 仓储错误 → 协议错误码。只有「会话不存在」是客户端能处理的，其余都算库故障。
fn map_store_error(err: StoreError) -> ErrorObject {
    match err {
        StoreError::SessionNotFound(id) => session_not_found(&id),
        other => ErrorObject::new(ErrorCode::STORE_ERROR, format!("会话库读写失败: {other}")),
    }
}

fn to_view(session: &Session) -> SessionView {
    SessionView {
        session_id: session.id.clone(),
        title: session.title.clone(),
        workspace_path: session.workspace_path.clone(),
        model_ref: session.model_ref.clone(),
        created_at_ms: session.created_at,
        updated_at_ms: session.updated_at,
    }
}

fn to_message_view(message: &Message) -> MessageView {
    MessageView {
        message_id: message.id.clone(),
        session_id: message.session_id.clone(),
        parent_id: message.parent_id.clone(),
        role: message.role,
        content: message.content.clone(),
        attachments: message.attachments.clone(),
        tool_calls: message.tool_calls.clone(),
        tool_call_id: message.tool_call_id.clone(),
        model_ref: message.model_ref.clone(),
        usage: message.usage.clone(),
        created_at_ms: message.created_at,
    }
}
