//! 审计账本的集成测试（M5 切片四）。
//!
//! 覆盖四件事：
//!
//! 1. 写入 / 查询 / 计数 / 分页 / 过滤的基本语义；
//! 2. **脱敏**：密钥、token 无论写在摘要还是 detail 里，落库后都取不到明文；
//! 3. 审计是 append-only：会话删掉后审计行仍在（没有外键级联）；
//! 4. 按时间裁剪只删旧行。

use ai_ide_store::{
    audit_actor, audit_kind, AuditFilter, AuditQuery, NewAuditEvent, NewSession, Store,
};

fn text_event(kind: &str, summary: impl Into<String>) -> NewAuditEvent {
    NewAuditEvent::core(kind, summary)
}

#[tokio::test]
async fn record_then_query_round_trip() {
    let store = Store::open_in_memory().await.expect("打开内存库");

    let id = store
        .record_audit(
            NewAuditEvent::by(
                audit_actor::USER,
                audit_kind::SESSION_CREATED,
                "创建会话「甲」",
            )
            .with_session("s-1")
            .with_detail(serde_json::json!({ "title": "甲" }))
            .with_created_at(1_700_000_000_000),
        )
        .await
        .expect("写入审计");
    assert!(id > 0);

    let rows = store
        .list_audit(AuditQuery::new().session("s-1"))
        .await
        .expect("查询审计");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, audit_kind::SESSION_CREATED);
    assert_eq!(rows[0].actor, audit_actor::USER);
    assert_eq!(rows[0].session_id.as_deref(), Some("s-1"));
    assert_eq!(rows[0].summary, "创建会话「甲」");
    assert_eq!(rows[0].detail["title"], serde_json::json!("甲"));
    assert_eq!(rows[0].created_at, 1_700_000_000_000);
}

#[tokio::test]
async fn query_orders_newest_first_and_pages() {
    let store = Store::open_in_memory().await.expect("打开内存库");
    for i in 0..5 {
        store
            .record_audit(
                text_event(audit_kind::SESSION_CREATED, format!("第 {i} 条"))
                    .with_created_at(1_000 + i),
            )
            .await
            .expect("写入审计");
    }

    let first = store
        .list_audit(AuditQuery::new().with_limit(2))
        .await
        .expect("第一页");
    assert_eq!(first.len(), 2);
    // 时间倒序：最新写入的 4 在最前。
    assert_eq!(first[0].summary, "第 4 条");
    assert_eq!(first[1].summary, "第 3 条");

    let second = store
        .list_audit(AuditQuery::new().with_limit(2).with_offset(2))
        .await
        .expect("第二页");
    assert_eq!(second[0].summary, "第 2 条");

    // 同一毫秒的多条也要有稳定次序：按 id 倒序，翻页不会重复或漏。
    store
        .record_audit(text_event(audit_kind::SESSION_CREATED, "同刻 A").with_created_at(2_000))
        .await
        .expect("写入审计");
    store
        .record_audit(text_event(audit_kind::SESSION_CREATED, "同刻 B").with_created_at(2_000))
        .await
        .expect("写入审计");
    let same = store
        .list_audit(AuditQuery::new().with_limit(2))
        .await
        .expect("同刻排序");
    assert_eq!(
        same[0].summary, "同刻 B",
        "同刻时后写入的 id 更大，应排在前"
    );
    assert_eq!(same[1].summary, "同刻 A");
}

#[tokio::test]
async fn filters_by_kind_session_and_time_window() {
    let store = Store::open_in_memory().await.expect("打开内存库");
    for (kind, session, at) in [
        (audit_kind::SESSION_CREATED, "s-1", 100),
        (audit_kind::SESSION_UPDATED, "s-1", 200),
        (audit_kind::SESSION_DELETED, "s-2", 300),
        (audit_kind::SESSION_EXPORTED, "s-1", 400),
    ] {
        store
            .record_audit(
                text_event(kind, "x")
                    .with_session(session)
                    .with_created_at(at),
            )
            .await
            .expect("写入审计");
    }

    let by_session = store
        .list_audit(AuditQuery::new().session("s-1"))
        .await
        .expect("按会话过滤");
    assert_eq!(by_session.len(), 3);

    let by_kind = store
        .list_audit(AuditQuery::new().kind(audit_kind::SESSION_DELETED))
        .await
        .expect("按种类过滤");
    assert_eq!(by_kind.len(), 1);
    assert_eq!(by_kind[0].session_id.as_deref(), Some("s-2"));

    let windowed = store
        .list_audit(AuditQuery::new().since(150).until(350))
        .await
        .expect("按时间窗过滤");
    assert_eq!(windowed.len(), 2, "左闭右闭：200 与 300 都命中");

    let count = store
        .count_audit(&AuditFilter {
            session_id: Some("s-1"),
            ..AuditFilter::default()
        })
        .await
        .expect("计数");
    assert_eq!(count, 3, "计数不受分页影响");
}

#[tokio::test]
async fn sensitive_fields_are_masked_on_write() {
    let store = Store::open_in_memory().await.expect("打开内存库");

    store
        .record_audit(
            NewAuditEvent::core(
                audit_kind::SESSION_EXPORTED,
                "导出时用了 api_key=sk-live-abcdef123456 与 Authorization: Bearer tok-9f8e7d6c5b4a",
            )
            .with_session("s-1")
            .with_detail(serde_json::json!({
                "path": "E:/out/会话.md",
                "api_key": "sk-live-abcdef123456",
                "nested": { "refresh_token": "rt-000111222333", "provider": "openai" },
            }))
            .with_created_at(1_000),
        )
        .await
        .expect("写入审计");

    let rows = store.list_audit(AuditQuery::new()).await.expect("查询审计");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];

    // 摘要里的明文密钥不能留。
    assert!(
        !row.summary.contains("sk-live-abcdef123456"),
        "摘要未脱敏: {}",
        row.summary
    );
    assert!(
        !row.summary.contains("tok-9f8e7d6c5b4a"),
        "摘要未脱敏: {}",
        row.summary
    );
    // detail 里的敏感键按值脱敏，非敏感键原样保留。
    let detail = row.detail.to_string();
    assert!(
        !detail.contains("sk-live-abcdef123456"),
        "detail 未脱敏: {detail}"
    );
    assert!(
        !detail.contains("rt-000111222333"),
        "detail 未脱敏: {detail}"
    );
    assert_eq!(row.detail["path"], serde_json::json!("E:/out/会话.md"));
    assert_eq!(
        row.detail["nested"]["provider"],
        serde_json::json!("openai")
    );

    // 直接读原始列，确认库里存的也不是密文之外的东西（防「只在出口脱敏」）。
    let raw: String = sqlx::query_scalar("SELECT detail_json FROM audit_event WHERE id = ?")
        .bind(row.id)
        .fetch_one(store.pool())
        .await
        .expect("读原始行");
    assert!(!raw.contains("sk-live-abcdef123456"), "库里存了明文: {raw}");
    assert!(!raw.contains("rt-000111222333"), "库里存了明文: {raw}");
}

#[tokio::test]
async fn audit_survives_session_delete() {
    let store = Store::open_in_memory().await.expect("打开内存库");
    let session = store
        .create_session(NewSession::new("要删的会话"))
        .await
        .expect("建会话");

    store
        .record_audit(
            text_event(audit_kind::SESSION_CREATED, "创建会话")
                .with_session(&session.id)
                .with_created_at(1_000),
        )
        .await
        .expect("写入审计");
    assert!(store.delete_session(&session.id).await.expect("删会话"));

    // 审计没有外键：会话没了，账还在——这才叫留痕。
    let rows = store
        .list_audit(AuditQuery::new().session(&session.id))
        .await
        .expect("查询审计");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, audit_kind::SESSION_CREATED);
}

#[tokio::test]
async fn prune_removes_only_old_rows() {
    let store = Store::open_in_memory().await.expect("打开内存库");
    for at in [1_000i64, 2_000, 3_000] {
        store
            .record_audit(text_event(audit_kind::SESSION_CREATED, "x").with_created_at(at))
            .await
            .expect("写入审计");
    }

    let removed = store.prune_audit(2_500).await.expect("裁剪");
    assert_eq!(removed, 2);

    let left = store.list_audit(AuditQuery::new()).await.expect("查询审计");
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].created_at, 3_000);
}

#[tokio::test]
async fn limit_is_clamped() {
    let store = Store::open_in_memory().await.expect("打开内存库");
    store
        .record_audit(text_event(audit_kind::SESSION_CREATED, "x"))
        .await
        .expect("写入审计");

    // 0 与负数都被夹到 1，不能变成「不限条数」。
    let rows = store
        .list_audit(AuditQuery::new().with_limit(0))
        .await
        .expect("limit=0");
    assert_eq!(rows.len(), 1);

    let rows = store
        .list_audit(AuditQuery::new().with_limit(-5).with_offset(-5))
        .await
        .expect("负 limit");
    assert_eq!(rows.len(), 1);
}

#[tokio::test]
async fn audit_survives_reopen() {
    let dir = std::env::temp_dir().join(format!("ai-ide-store-audit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let path = dir.join("audit.db");

    {
        let store = Store::connect(&path).await.expect("打开文件库");
        store
            .record_audit(
                text_event(audit_kind::SESSION_FORKED, "分叉")
                    .with_session("s-9")
                    .with_created_at(1_234),
            )
            .await
            .expect("写入审计");
    }

    let reopened = Store::connect(&path).await.expect("重开文件库");
    let rows = reopened
        .list_audit(AuditQuery::new().session("s-9"))
        .await
        .expect("查询审计");
    assert_eq!(rows.len(), 1, "重开库后审计不应丢");
    assert_eq!(rows[0].created_at, 1_234);

    // 收尾：删掉临时库目录，避免测试在 %TEMP% 里越堆越多。
    let _ = std::fs::remove_dir_all(&dir);
}
