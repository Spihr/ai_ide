//! 审计仓储：写入、查询与按时间裁剪。
//!
//! 与 `repo::session` / `repo::message` 的差别：这里没有外键、没有级联。审计是
//! **append-only 的旁路账本**，只做「写一条」和「按条件翻页读」，不提供更新接口——
//! 能被改写的审计记录等于没有审计。

use sqlx::{QueryBuilder, Sqlite};

use crate::model::{now_millis, AuditEvent, AuditFilter, AuditQuery, NewAuditEvent};
use crate::redact;
use crate::{Result, Store};

/// 单次查询的条数上限：审计表是随时间线性增长的，不设上限迟早被一次「全要」拖死。
pub const MAX_AUDIT_LIMIT: i64 = 500;

const SELECT_AUDIT: &str =
    "SELECT id, kind, session_id, actor, summary, detail_json, created_at FROM audit_event";

/// `audit_event` 表原始行。
#[derive(Debug, sqlx::FromRow)]
struct AuditRow {
    id: i64,
    kind: String,
    session_id: Option<String>,
    actor: String,
    summary: String,
    detail_json: String,
    created_at: i64,
}

impl AuditRow {
    fn into_event(self) -> Result<AuditEvent> {
        let detail: serde_json::Value = serde_json::from_str(&self.detail_json)?;
        Ok(AuditEvent {
            id: self.id,
            kind: self.kind,
            session_id: self.session_id,
            actor: self.actor,
            // 读侧再脱敏一次：写入侧只保护「本版本写进来的行」，老库与手工改过的库
            // 得靠这里兜底。
            summary: redact::redact_text(&self.summary),
            detail: redact::redact_value(&detail),
            created_at: self.created_at,
        })
    }
}

/// 把过滤条件拼进 `WHERE`；`1 = 1` 作为锚点，后续条件统一用 `AND` 追加。
fn push_filter(builder: &mut QueryBuilder<'_, Sqlite>, filter: &AuditFilter<'_>) {
    builder.push(" WHERE 1 = 1");
    if let Some(session_id) = filter.session_id {
        builder
            .push(" AND session_id = ")
            .push_bind(session_id.to_string());
    }
    if let Some(kind) = filter.kind {
        builder.push(" AND kind = ").push_bind(kind.to_string());
    }
    if let Some(since_ms) = filter.since_ms {
        builder.push(" AND created_at >= ").push_bind(since_ms);
    }
    if let Some(until_ms) = filter.until_ms {
        builder.push(" AND created_at <= ").push_bind(until_ms);
    }
}

impl Store {
    /// 追加一条审计事件，返回新行的 id。
    ///
    /// 脱敏在这里做，而不是在调用方：[`NewAuditEvent`] 的字段是公开的，
    /// 只守住这一个入口才能保证「库里没有明文密钥」。
    pub async fn record_audit(&self, event: NewAuditEvent) -> Result<i64> {
        let created_at = event.created_at.unwrap_or_else(now_millis);
        let summary = redact::redact_text(&event.summary);
        let detail = match event.detail {
            serde_json::Value::Null => serde_json::json!({}),
            other => redact::redact_value(&other),
        };
        let detail_json = serde_json::to_string(&detail)?;

        let result = sqlx::query(
            "INSERT INTO audit_event (kind, session_id, actor, summary, detail_json, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(&event.kind)
        .bind(event.session_id.as_deref())
        .bind(&event.actor)
        .bind(&summary)
        .bind(&detail_json)
        .bind(created_at)
        .execute(&self.pool)
        .await?;

        Ok(result.last_insert_rowid())
    }

    /// 按条件查询审计事件，按时间倒序（同刻按 id 倒序，保证翻页稳定）。
    pub async fn list_audit(&self, query: AuditQuery<'_>) -> Result<Vec<AuditEvent>> {
        let limit = query.limit.clamp(1, MAX_AUDIT_LIMIT);
        let offset = query.offset.max(0);

        let mut builder = QueryBuilder::<Sqlite>::new(SELECT_AUDIT);
        push_filter(&mut builder, &query.filter);
        builder
            .push(" ORDER BY created_at DESC, id DESC LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);

        let rows = builder
            .build_query_as::<AuditRow>()
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(AuditRow::into_event).collect()
    }

    /// 统计满足条件的审计事件总数（不受分页影响）。
    pub async fn count_audit(&self, filter: &AuditFilter<'_>) -> Result<i64> {
        let mut builder = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM audit_event");
        push_filter(&mut builder, filter);
        let count = builder
            .build_query_scalar::<i64>()
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    /// 删除早于 `cutoff_ms` 的事件，返回删除条数。
    ///
    /// 只为「留一段时间的账」服务：审计表不能无限长，但也不能由调用方随手清空，
    /// 所以只暴露按时间的裁剪，不提供无条件 `DELETE`。
    pub async fn prune_audit(&self, cutoff_ms: i64) -> Result<u64> {
        let result = sqlx::query("DELETE FROM audit_event WHERE created_at < ?")
            .bind(cutoff_ms)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}
