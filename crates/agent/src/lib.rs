//! # ai-ide-agent
//!
//! AI IDE 的 **Agent 闭环内核**（M4 切片一）。
//!
//! ## 这一层负责什么
//!
//! 1. **循环调度**（[`runner`]）：多轮「模型 → 工具 → 模型」，直到模型收手，
//!    或者撞上轮次 / 步数上限，或者被取消。每一轮的助手消息与工具结果消息由
//!    [`message`] 按协议 [`ChatMessage`](ai_ide_protocol::dto::ChatMessage) 拼装。
//! 2. **工具注册表**（[`registry`]）：工具声明（JSON Schema）、按名派发、结果回传。
//! 3. **八个核心工具**（[`tools`]）：`read` / `list` / `grep` / `write` /
//!    `edit` / `create` 六个读写工具，加上切片二的 `run_command`（跑外部命令、
//!    管住进程树与输出）与 `apply_diff`（unified diff 的生成与逐块落地）。
//! 4. **diff 引擎**（[`diff`]）：unified diff 的生成、解析与按 hunk 应用，
//!    纯函数、不碰磁盘，工具与后续的 diff 视图共用同一套行/块模型。
//!
//! ## 复用既有约定，不另起一套
//!
//! - **路径安全**：所有工具都先过 `ai-ide-workspace` 的 `PathGuard`。越界回 `1002`、
//!   敏感路径回 `1003`，并且**不降级**成「读不到」——与 M3 上下文注入同口径。
//! - **大文件截断**：`read` 沿用 M3 的单文件口径（256 KiB / 2000 行，标 `truncated`），
//!   渲染成与 `@文件` 注入一致的 `<file path lines truncated>` 块。
//! - **错误分类**：见 [`AgentError::is_fatal`]。安全闸门与模型故障终止循环，
//!   其余失败回灌给模型让它自己纠正。
//!
//! ## 这一层**不**负责什么
//!
//! diff 视图、审批 UI、工具调用卡片 UI 都属 M4 的后续切片。[`permission`] 已把
//! 权限判定本身做扎实（三态 + 可注入裁决者 + 缺省判拒），并已挂到循环的派发点上
//! （[`AgentLoop::with_gate`]）：策略硬拒终止循环，审批被拒 / 超时只回灌一条工具错误。
//! `core` 侧的 `agent.*` 接线与审批 UI 仍属下一片切片。
//!
//! ```
//! use ai_ide_agent::{AgentError, AgentLoop, ModelTurn, ToolContext, ToolRegistry, TurnReply, TurnRequest};
//! # use async_trait::async_trait;
//! # struct Echo;
//! # #[async_trait]
//! # impl ModelTurn for Echo {
//! #     async fn complete(&self, _request: TurnRequest) -> Result<TurnReply, AgentError> {
//! #         Ok(TurnReply::text_only("好"))
//! #     }
//! # }
//! # #[allow(dead_code)]
//! # async fn demo(ctx: &ToolContext<'_>) {
//! let registry = ToolRegistry::core();
//! let model = Echo;
//! let outcome = AgentLoop::new(&model, &registry)
//!     .run(ctx, vec![ai_ide_agent::message::user_message("你好")])
//!     .await
//!     .unwrap();
//! assert!(outcome.is_complete());
//! # }
//! ```

pub mod diff;
pub mod error;
pub mod message;
pub mod permission;
pub mod registry;
pub mod runner;
pub mod tools;

#[cfg(test)]
pub(crate) mod testing;

pub use error::AgentError;
pub use permission::{
    ApprovalReply, ApprovalRequest, Decider, Grant, PermissionGate, Ruling, ToolCall,
    DEFAULT_APPROVAL_TIMEOUT,
};
pub use registry::{Tool, ToolContext, ToolOutcome, ToolRegistry};
pub use runner::{
    AgentLoop, ModelTurn, RunLimits, RunOutcome, StopReason, TurnReply, TurnRequest,
    DEFAULT_MAX_ROUNDS, DEFAULT_MAX_STEPS,
};
