//! 会话与消息的协议处理器（M5 切片二 / 三）。
//!
//! 这一层只做三件事：解析参数、把协议字段折成 [`ai_ide_store`] 的入参、
//! 把仓储错误翻成错误码。落盘语义（事务、级联删除、`updated_at` 只增不减）全在
//! `crates/store` 里，这里不写 SQL，也不碰文件系统。
//!
//! 会话 `id` 由 store 生成（UUID v4），协议层不发放 id——否则「同一个会话被两条
//! 并发请求创建」这类竞态就得由上层自己兜。

use ai_ide_protocol::dto::{
    MessageSearchHit, MessageView, SessionAppendMessageParams, SessionAppendMessageResult,
    SessionCreateParams, SessionCreateResult, SessionDeleteParams, SessionDeleteResult,
    SessionExportParams, SessionExportResult, SessionForkParams, SessionForkResult,
    SessionGetParams, SessionGetResult, SessionListParams, SessionListResult,
    SessionMessagesParams, SessionMessagesResult, SessionSearchMessagesParams,
    SessionSearchMessagesResult, SessionUpdateParams, SessionView,
};
use ai_ide_protocol::{ErrorCode, ErrorObject, Request};
use ai_ide_store::{
    audit_kind, Message, NewAuditEvent, NewMessage, NewSession, Session, SessionQuery, Store,
    StoreError, MAX_FORK_MESSAGES,
};
use serde_json::json;

use crate::audit_api;
use crate::dispatch::parse_params;
use crate::peer::Peer;
use crate::session_export;
use crate::state::{now_ms, AppState};

/// 客户端不给标题时的兜底，保证列表里永远有话可说。
const DEFAULT_TITLE: &str = "新会话";
/// `session.list` 的默认 / 最大返回条数。
const LIST_DEFAULT_LIMIT: u32 = 50;
const LIST_MAX_LIMIT: u32 = 500;
/// `session.messages` 的默认 / 最大返回条数：长对话必须能分页读，不能一帧拉全量。
const MESSAGES_DEFAULT_LIMIT: u32 = 200;
const MESSAGES_MAX_LIMIT: u32 = 10_000;

/// 检索默认返回命中数：够填满一屏搜索结果、又不至于把大仓库的回复撑爆。
const SEARCH_DEFAULT_LIMIT: u32 = 20;
/// 检索单页上限：与仓储侧 instr 全表扫描的成本折中，超出交给分页。
const SEARCH_MAX_LIMIT: u32 = 200;

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
            // 先记审计再回包：两者都在业务成功之后，顺序不影响成败。
            audit_api::record(
                store,
                NewAuditEvent::core(
                    audit_kind::SESSION_CREATED,
                    format!("创建会话「{}」", session.title),
                )
                .with_session(&session.id)
                .with_detail(json!({
                    "title": session.title,
                    "workspace_path": session.workspace_path,
                    "model_ref": session.model_ref,
                })),
            )
            .await;
            // 会话建好之后再投事件：插件收到 `session.started` 就能去读这条会话，
            // 早一步投会读到「还不存在」。载荷只有 id，不带标题/正文。
            state.plugins().publish(
                ai_ide_plugin::events::SESSION_STARTED,
                ai_ide_plugin::events::session_payload(&session.id),
            );
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
    // 关键词由 store 的 SQL 处理（标题 + 消息正文），分页与计数都在同一条件下做，
    // 所以不需要像早期那样「先取 500 条再在内存里过滤标题」——内存过滤会让
    // `total` 与真实命中数分离，也无法搜到正文里的内容。
    let keyword = params
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty());

    let query = SessionQuery {
        workspace_path: workspace_path.as_deref(),
        keyword,
        limit: limit as i64,
        offset: offset as i64,
    };
    let sessions = match store.list_sessions(query).await {
        Ok(rows) => rows,
        Err(err) => return peer.respond_error(&req.id, &map_store_error(err)).await,
    };
    let total = match store
        .count_sessions(workspace_path.as_deref(), keyword)
        .await
    {
        Ok(total) => total as u64,
        Err(err) => return peer.respond_error(&req.id, &map_store_error(err)).await,
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
    // 先把「改了什么」记下来：下面 `params.model_ref` 会被移进 `set_session_model`，
    // 回包时再判断就晚了。
    let model_changed = params.model_ref.is_some();
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
        Ok(session) => {
            // 改名的旧标题拿不到了（rename 已覆盖），所以摘要只描述「改成了什么」，
            // 新旧对照留给客户端自己在本地快照里比对。
            let mut bits: Vec<String> = Vec::new();
            if title.is_some() {
                bits.push(format!("改名为「{}」", session.title));
            }
            if model_changed {
                bits.push("切换默认模型".to_string());
            }
            audit_api::record(
                store,
                NewAuditEvent::core(
                    audit_kind::SESSION_UPDATED,
                    format!("更新会话「{}」：{}", session.title, bits.join("、")),
                )
                .with_session(&session.id)
                .with_detail(json!({
                    "title": session.title,
                    "model_ref": session.model_ref,
                    "renamed": title.is_some(),
                    "model_changed": model_changed,
                })),
            )
            .await;
            peer.respond(&req.id, &to_view(&session)).await
        }
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
        // 删之前先抓标题：删完就只剩一个 UUID，审计里读不出「哪个会话没了」。
        let title = match store.get_session(id).await {
            Ok(Some(session)) => Some(session.title),
            Ok(None) => None,
            Err(err) => return peer.respond_error(&req.id, &map_store_error(err)).await,
        };
        match store.delete_session(id).await {
            Ok(true) => {
                deleted += 1;
                // 会话真的从库里没了才广播，`Ok(false)`（本来就不存在）不投：
                // 插件对「一个从来没存在过的会话结束」做反应没有意义。
                state.plugins().publish(
                    ai_ide_plugin::events::SESSION_ENDED,
                    ai_ide_plugin::events::session_payload(id),
                );
                let summary = match &title {
                    Some(title) => format!("删除会话「{title}」"),
                    None => format!("删除会话 {id}"),
                };
                audit_api::record(
                    store,
                    NewAuditEvent::core(audit_kind::SESSION_DELETED, summary)
                        .with_session(id)
                        .with_detail(json!({ "session_id": id, "title": title })),
                )
                .await;
            }
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
    // 两种取页方向共用一个 offset 语义差：正序是「跳过最早 N 条」，尾序是
    // 「跳过最新 N 条」。`tail` 缺省取正序——老客户端不带这个字段，行为不变。
    let rows = match if params.tail.unwrap_or(false) {
        store
            .list_message_page_tail(&params.session_id, limit as i64, offset as i64)
            .await
    } else {
        store
            .list_messages(&params.session_id, limit as i64, offset as i64)
            .await
    } {
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
// session.search_messages
// ---------------------------------------------------------------------------

/// 检索命中的上下文宽度（按字符算）：命中词两侧各留这么多字符。
///
/// 按字符而不是字节截断——中文一个字三字节，按字节切会把词切成半个。
const SNIPPET_PAD: usize = 40;

pub async fn search_messages(peer: &Peer, state: &AppState, req: &Request) {
    let params: SessionSearchMessagesParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let query = params.query.trim();
    // 空关键词直接拒：`instr(x, '')` 恒为 1，等于把整个会话库倒出来当结果。
    if query.is_empty() {
        let err = ErrorObject::invalid_params("query 不能为空");
        return peer.respond_error(&req.id, &err).await;
    }

    // 限定会话时必须先确认它存在：否则「会话不存在」和「这会话里没有命中」
    // 在结果上都是空列表，用户分不清是自己搜错了还是会话被删了。
    if let Some(session_id) = params.session_id.as_deref() {
        if let Err(err) = load_session(store, session_id).await {
            return peer.respond_error(&req.id, &err).await;
        }
    }

    let limit = params
        .limit
        .unwrap_or(SEARCH_DEFAULT_LIMIT)
        .clamp(1, SEARCH_MAX_LIMIT);
    let offset = params.offset.unwrap_or(0);
    let scope = params.session_id.as_deref();

    let total = match store.count_matching_messages(query, scope).await {
        Ok(total) => total as u64,
        Err(err) => return peer.respond_error(&req.id, &map_store_error(err)).await,
    };
    let rows = match store
        .search_messages(query, scope, limit as i64, offset as i64)
        .await
    {
        Ok(rows) => rows,
        Err(err) => return peer.respond_error(&req.id, &map_store_error(err)).await,
    };

    // 会话标题按 id 去重后各查一次：一页最多 200 条命中，命中的会话通常个位数。
    let mut titles: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut hits = Vec::with_capacity(rows.len());
    for message in &rows {
        let title = match titles.get(&message.session_id) {
            Some(title) => title.clone(),
            None => {
                let title = match store.get_session(&message.session_id).await {
                    Ok(Some(session)) => session.title,
                    // 会话被并发删掉：命中仍然有正文，标题退回 id，不给整个检索报错。
                    Ok(None) => message.session_id.clone(),
                    Err(err) => return peer.respond_error(&req.id, &map_store_error(err)).await,
                };
                titles.insert(message.session_id.clone(), title.clone());
                title
            }
        };
        hits.push(MessageSearchHit {
            message_id: message.id.clone(),
            session_id: message.session_id.clone(),
            session_title: title,
            role: message.role,
            snippet: snippet_of(&message.content, query),
            created_at_ms: message.created_at,
        });
    }

    let result = SessionSearchMessagesResult { hits, total };
    peer.respond(&req.id, &result).await;
}

/// 截一段以命中词为中心的片段。
///
/// 命中词可能不在正文里（命中的是 `content_json`，命中点可能落在附件路径上）：
/// 此时退回正文开头，而不是返回空片段——空片段在列表里等于一条看不见的结果。
fn snippet_of(content: &str, query: &str) -> String {
    let chars: Vec<char> = content.chars().collect();
    let start = match find_hit_char_index(content, query) {
        Some(pos) => pos.saturating_sub(SNIPPET_PAD).min(chars.len()),
        None => 0,
    };
    let end = (start + SNIPPET_PAD * 2 + query.chars().count()).min(chars.len());
    let mut snippet: String = chars[start..end].iter().collect();
    // 单行显示：换行会在列表里把一条结果撑成好几行，把别的命中挤下去。
    snippet = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
    if start > 0 {
        snippet.insert(0, '…');
    }
    if end < chars.len() {
        snippet.push('…');
    }
    snippet
}

/// 命中词在正文里的**字符**下标（找不到返回 `None`）。
///
/// 不用 `String::find` 的返回值：那是字节偏移，而大小写折叠会改字节宽度
/// （`İ` 折叠成两个字符），拿折叠后的字节偏移回头切原串，可能切在半个字符上直接
/// panic。这里先把两边都折成「小写字符序列」再比，得到的是字符下标，切成
/// `&chars[start..end]` 永远安全。折叠后长度可能多一位，偏差只影响片段起点偏移
/// 一个字符，肉眼不可见。
fn find_hit_char_index(content: &str, query: &str) -> Option<usize> {
    let hay: Vec<char> = content.chars().flat_map(|ch| ch.to_lowercase()).collect();
    let needle: Vec<char> = query.chars().flat_map(|ch| ch.to_lowercase()).collect();
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&start| hay[start..start + needle.len()] == needle[..])
}

// ---------------------------------------------------------------------------
// session.export
// ---------------------------------------------------------------------------

pub async fn export(peer: &Peer, state: &AppState, req: &Request) {
    let params: SessionExportParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    // 先确认会话存在：导出空文件比报 5001 更糟——用户会以为会话真没内容。
    let session = match load_session(store, &params.session_id).await {
        Ok(session) => session,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let messages = match load_all_messages(store, &session.id).await {
        Ok(messages) => messages,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let view = to_view(&session);
    // 路径先解析再渲染：路径非法时没必要白跑一遍序列化。
    let path = match session_export::resolve_out_path(&params.out_path, params.format, &view) {
        Ok(path) => path,
        Err(message) => {
            let err = ErrorObject::invalid_params(message).with_data(json!({
                "out_path": params.out_path,
            }));
            return peer.respond_error(&req.id, &err).await;
        }
    };

    let body = session_export::render(
        &view,
        &messages.iter().map(to_message_view).collect::<Vec<_>>(),
        params.format,
        now_ms(),
    );

    match session_export::write(&path, &body) {
        Ok(bytes) => {
            // 导出把会话内容写到了工作区之外，是审计里最该留痕的一类操作：
            // 记路径、格式、字节数与消息条数，事后能对上「谁在什么时候导出了什么」。
            audit_api::record(
                store,
                NewAuditEvent::core(
                    audit_kind::SESSION_EXPORTED,
                    format!("导出会话「{}」到 {}", view.title, path.to_string_lossy()),
                )
                .with_session(&session.id)
                .with_detail(json!({
                    "session_id": session.id,
                    "title": view.title,
                    "path": path.to_string_lossy(),
                    "format": params.format,
                    "bytes": bytes,
                    "messages": messages.len() as u64,
                })),
            )
            .await;
            let result = SessionExportResult {
                ok: true,
                path: path.to_string_lossy().into_owned(),
                bytes,
                messages: messages.len() as u64,
            };
            peer.respond(&req.id, &result).await;
        }
        Err(err) => {
            let error = ErrorObject::new(ErrorCode::EXPORT_FAILED, format!("导出写盘失败: {err}"))
                .with_data(json!({ "path": path.to_string_lossy() }));
            peer.respond_error(&req.id, &error).await;
        }
    }
}

// ---------------------------------------------------------------------------
// session.fork
// ---------------------------------------------------------------------------

pub async fn fork(peer: &Peer, state: &AppState, req: &Request) {
    let params: SessionForkParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    // 复制语义全在 store 的事务里（含父子链重映射、截断点校验），这里只翻译结果。
    match store
        .fork_session(
            &params.session_id,
            params.from_message_id.as_deref(),
            params.title.as_deref(),
        )
        .await
    {
        Ok(outcome) => {
            // 分叉同时产生「源会话被读取」和「新会话被创建」两件事，一条事件里
            // 都写清楚：审计表按时间线性读，拆两条反而要读者自己配对。
            audit_api::record(
                store,
                NewAuditEvent::core(
                    audit_kind::SESSION_FORKED,
                    format!(
                        "从会话「{}」分叉出新会话「{}」",
                        params.session_id, outcome.session.title
                    ),
                )
                .with_session(&outcome.session.id)
                .with_detail(json!({
                    "source_session_id": params.session_id,
                    "new_session_id": outcome.session.id,
                    "from_message_id": params.from_message_id,
                    "title": outcome.session.title,
                    "messages": outcome.messages.len() as u64,
                })),
            )
            .await;
            let result = SessionForkResult {
                new_session_id: outcome.session.id.clone(),
                session: to_view(&outcome.session),
                messages: outcome.messages.len() as u64,
            };
            peer.respond(&req.id, &result).await;
        }
        Err(err) => peer.respond_error(&req.id, &map_store_error(err)).await,
    }
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

/// 取会话库；未就绪时返回 5002（内核能跑，只是持久化不可用）。
pub(crate) fn store_of(state: &AppState) -> Result<&Store, ErrorObject> {
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

/// 读取会话的全部消息（导出 / 分叉前用）。
///
/// 上限与 fork 一致：超过 10000 条就明确报 5005，不静默截断——导出一半历史
/// 比导出失败更难发现。
async fn load_all_messages(store: &Store, session_id: &str) -> Result<Vec<Message>, ErrorObject> {
    let total = match store.count_messages(session_id).await {
        Ok(total) => total,
        Err(err) => return Err(map_store_error(err)),
    };
    if total > MAX_FORK_MESSAGES {
        return Err(session_too_large(total));
    }
    if total == 0 {
        return Ok(Vec::new());
    }
    match store.list_messages(session_id, total, 0).await {
        Ok(rows) => Ok(rows),
        Err(err) => Err(map_store_error(err)),
    }
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
        // 截断点写错属于参数问题（客户端能改），不是库坏了。
        StoreError::MessageNotFound(id) => {
            ErrorObject::invalid_params(format!("from_message_id 不存在或不属于该会话: {id}"))
                .with_data(json!({ "message_id": id }))
        }
        StoreError::SessionTooLarge { count, .. } => session_too_large(count),
        other => ErrorObject::new(ErrorCode::STORE_ERROR, format!("会话库读写失败: {other}")),
    }
}

fn session_too_large(count: i64) -> ErrorObject {
    ErrorObject::new(
        ErrorCode::SESSION_TOO_LARGE,
        format!("消息过多：{count} 条，单次上限 {MAX_FORK_MESSAGES} 条"),
    )
    .with_data(json!({ "count": count, "max": MAX_FORK_MESSAGES }))
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
