//! # ai-ide-store
//!
//! 持久化层（M5）：sqlx + SQLite 的迁移脚本，`session` / `message` / `audit_event`
//! 的读写。
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`store`] | 连接与迁移：`Store::connect` / `Store::open_in_memory`，内嵌 `migrations/` |
//! | [`repo`] | 仓储读写：会话增删改查、消息追加与分页查询、审计写入与查询、权限免问规则 |
//! | [`model`] | 领域模型：`Session` / `Message` / `AuditEvent` 及对应 `New*` |
//! | [`redact`] | 敏感字段脱敏：键名判定、取值打码、JSON 递归处理 |
//! | [`error`] | [`StoreError`] 与 [`Result`] |
//!
//! ## 边界
//!
//! - 表结构覆盖 `docs/02` §6.2 的 `session` / `message`，并按 `docs/04` §4.3 的 `Message`
//!   补 `tool_calls_json` / `tool_call_id`（超集，不改变原设计）。
//! - `audit_event`（迁移 0002）**不挂外键**：审计要能记录「会话被删」这件事，
//!   会话没了记录还得在。
//! - 时间列统一 **Unix 毫秒**（SQLite `INTEGER`），模型用 `i64`，不引入 chrono 依赖。
//! - 查询全部走运行时校验的 `sqlx::query` / `query_as`，编译期不需要 `DATABASE_URL`，
//!   也不依赖 `.sqlx` 离线缓存。
//! - 迁移在 [`Store::connect`] / [`Store::open_in_memory`] 内自动执行（幂等）。
//!
//! ## 用法
//!
//! ```no_run
//! use ai_ide_store::{Message, NewAuditEvent, NewMessage, NewSession, Store, audit_kind};
//!
//! # async fn demo() -> Result<(), ai_ide_store::StoreError> {
//! let store = Store::open_in_memory().await?;
//! let session = store.create_session(NewSession::new("第一次对话")).await?;
//! store
//!     .append_message(NewMessage::user(&session.id, "你好"))
//!     .await?;
//!
//! store
//!     .record_audit(
//!         NewAuditEvent::core(audit_kind::SESSION_CREATED, "创建会话")
//!             .with_session(&session.id),
//!     )
//!     .await?;
//!
//! let messages: Vec<Message> = store.list_messages(&session.id, 50, 0).await?;
//! assert_eq!(messages.len(), 1);
//! # Ok(())
//! # }
//! ```

pub mod error;
pub mod model;
pub mod redact;
pub mod repo;
pub mod store;

pub use error::{Result, StoreError};
pub use model::{
    audit_actor, audit_kind, now_millis, plugin_status, AuditEvent, AuditFilter, AuditQuery,
    ForkOutcome, Message, NewAuditEvent, NewMessage, NewPermissionRule, NewPlugin,
    NewPluginCapability, NewSession, PermissionRule, PluginCapability, PluginRecord, Session,
    SessionQuery, MAX_FORK_MESSAGES,
};
pub use redact::{is_sensitive_key, mask_secret, redact_text, redact_value, MASK};
pub use repo::audit::MAX_AUDIT_LIMIT;
pub use repo::permission::MAX_PERMISSION_RULES;
pub use store::{Store, MIGRATOR};
