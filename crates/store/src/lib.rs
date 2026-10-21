//! # ai-ide-store
//!
//! 持久化层（M5 切片一）：sqlx + SQLite 的迁移脚本与 `session` / `message` 读写。
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`store`] | 连接与迁移：`Store::connect` / `Store::open_in_memory`，内嵌 `migrations/` |
//! | [`repo`] | 仓储读写：会话增删改查、消息追加与分页查询 |
//! | [`model`] | 领域模型：`Session` / `Message` / `NewSession` / `NewMessage` |
//! | [`error`] | [`StoreError`] 与 [`Result`] |
//!
//! ## 本切片的边界
//!
//! - 表结构覆盖 `docs/02` §6.2 的 `session` / `message`，并按 `docs/04` §4.3 的 `Message`
//!   补 `tool_calls_json` / `tool_call_id`（超集，不改变原设计）。
//! - 时间列统一 **Unix 毫秒**（SQLite `INTEGER`），模型用 `i64`，不引入 chrono 依赖。
//! - **不碰协议层**：`session.*` 方法常量、dispatch 接线与 `SessionStore` 事件属后续切片。
//! - 查询全部走运行时校验的 `sqlx::query` / `query_as`，编译期不需要 `DATABASE_URL`，
//!   也不依赖 `.sqlx` 离线缓存。
//! - 迁移在 [`Store::connect`] / [`Store::open_in_memory`] 内自动执行（幂等）。
//!
//! ## 用法
//!
//! ```no_run
//! use ai_ide_store::{Message, NewMessage, NewSession, Store};
//!
//! # async fn demo() -> Result<(), ai_ide_store::StoreError> {
//! let store = Store::open_in_memory().await?;
//! let session = store.create_session(NewSession::new("第一次对话")).await?;
//! store
//!     .append_message(NewMessage::user(&session.id, "你好"))
//!     .await?;
//!
//! let messages: Vec<Message> = store.list_messages(&session.id, 50, 0).await?;
//! assert_eq!(messages.len(), 1);
//! # Ok(())
//! # }
//! ```

pub mod error;
pub mod model;
pub mod repo;
pub mod store;

pub use error::{Result, StoreError};
pub use model::{
    now_millis, ForkOutcome, Message, NewMessage, NewSession, Session, SessionQuery,
    MAX_FORK_MESSAGES,
};
pub use store::{Store, MIGRATOR};
