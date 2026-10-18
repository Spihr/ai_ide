//! # ai-ide-protocol
//!
//! Main 进程（Node/TypeScript）与 Core 进程（Rust）之间的**唯一契约来源**。
//!
//! 设计要点（对应 `docs/02-系统架构设计.md` 的 ADR-3）：
//!
//! 1. 协议帧为 **JSON-RPC 2.0**，传输为 **NDJSON**（每行一条消息，UTF-8，`\n` 结尾）。
//! 2. 本 crate 是类型的唯一定义处，TypeScript 类型由 [`ts-rs`] 自动导出到
//!    `packages/shared-types/src/generated/`，禁止两端各写一份。
//! 3. `id` 一律使用字符串（UUID v4），由 Main 生成，Core 原样回传。
//!
//! ## 帧示例
//!
//! ```text
//! {"jsonrpc":"2.0","id":"1","method":"core.ping","params":{}}
//! {"jsonrpc":"2.0","id":"1","result":{"pong":true,"uptime_ms":12}}
//! {"jsonrpc":"2.0","method":"llm.delta","params":{"stream_id":"s1","text":"你"}}
//! ```

pub mod dto;
pub mod error;
pub mod frame;
pub mod methods;
pub mod version;

pub use dto::*;
pub use error::{ErrorCode, ErrorObject};
pub use frame::{
    decode, decode_value, encode, encode_to_string, Incoming, Notification, Request, Response,
    JSONRPC_VERSION,
};
pub use version::{ProtocolVersion, PROTOCOL_VERSION};
