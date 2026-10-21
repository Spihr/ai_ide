//! 审计日志的协议处理器与写入旁路（M5 切片四）。
//!
//! 分两半：
//!
//! - [`list`] 处理 `audit.list`，只读查询；
//! - [`record`] 是给会话处理器调的**旁路写入**：审计失败绝不能把已经成功的
//!   业务操作变成失败。删一个会话成功了，却因为审计表写不进去回一个 5003，
//!   用户会看到一个「删掉了但报错」的会话——比丢一条审计记录更糟。
//!
//! 脱敏不在这里做。`Store::record_audit` 是唯一写入入口，脱敏钉在那一层，
//! 免得将来多一个调用方就多一处能忘记脱敏的地方。

use ai_ide_protocol::dto::{AuditEventView, AuditListParams, AuditListResult};
use ai_ide_protocol::{ErrorCode, ErrorObject, Request};
use ai_ide_store::{AuditEvent, AuditQuery, NewAuditEvent, Store};

use crate::dispatch::parse_params;
use crate::peer::Peer;
use crate::session_api::store_of;
use crate::state::AppState;

/// `audit.list` 的默认 / 最大返回条数，与 store 侧上限保持一致。
const AUDIT_DEFAULT_LIMIT: u32 = 50;
const AUDIT_MAX_LIMIT: u32 = ai_ide_store::MAX_AUDIT_LIMIT as u32;

// ---------------------------------------------------------------------------
// audit.list
// ---------------------------------------------------------------------------

pub async fn list(peer: &Peer, state: &AppState, req: &Request) {
    let params: AuditListParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    // 归一化：空白串等于「不过滤」，否则前端传 `""` 会静默命中零条。
    let session_id = params
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty());
    let kind = params
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|kind| !kind.is_empty());

    if let (Some(since), Some(until)) = (params.since_ms, params.until_ms) {
        if since > until {
            let err = ErrorObject::invalid_params("since_ms 不能晚于 until_ms")
                .with_data(serde_json::json!({ "since_ms": since, "until_ms": until }));
            return peer.respond_error(&req.id, &err).await;
        }
    }

    let limit = params
        .limit
        .unwrap_or(AUDIT_DEFAULT_LIMIT)
        .clamp(1, AUDIT_MAX_LIMIT);
    let offset = params.offset.unwrap_or(0);

    let mut query = AuditQuery::new()
        .with_limit(limit as i64)
        .with_offset(offset as i64);
    if let Some(session_id) = session_id {
        query = query.session(session_id);
    }
    if let Some(kind) = kind {
        query = query.kind(kind);
    }
    if let Some(since) = params.since_ms {
        query = query.since(since);
    }
    if let Some(until) = params.until_ms {
        query = query.until(until);
    }

    // total 用「不含分页」的条件单独数：让前端能判断「还有没有下一页」。
    let total = match store.count_audit(&query.filter).await {
        Ok(total) => total.max(0) as u64,
        Err(err) => return peer.respond_error(&req.id, &store_error(err)).await,
    };
    let rows = match store.list_audit(query).await {
        Ok(rows) => rows,
        Err(err) => return peer.respond_error(&req.id, &store_error(err)).await,
    };

    let result = AuditListResult {
        events: rows.iter().map(to_view).collect(),
        total,
    };
    peer.respond(&req.id, &result).await;
}

// ---------------------------------------------------------------------------
// 写入旁路
// ---------------------------------------------------------------------------

/// 记一条审计事件，**永不**向上传播错误。
///
/// 调用点都是「业务已成功」的位置，所以这里只能告警：审计是旁路账本，
/// 不该反过来决定业务成败。
pub async fn record(store: &Store, event: NewAuditEvent) {
    let kind = event.kind.clone();
    if let Err(err) = store.record_audit(event).await {
        tracing::warn!(kind = %kind, error = %err, "审计事件写入失败（不影响业务结果）");
    }
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

fn store_error(err: ai_ide_store::StoreError) -> ErrorObject {
    ErrorObject::new(ErrorCode::STORE_ERROR, format!("会话库读写失败: {err}"))
}

fn to_view(event: &AuditEvent) -> AuditEventView {
    AuditEventView {
        event_id: event.id,
        kind: event.kind.clone(),
        session_id: event.session_id.clone(),
        actor: event.actor.clone(),
        summary: event.summary.clone(),
        detail: event.detail.clone(),
        created_at_ms: event.created_at,
    }
}
