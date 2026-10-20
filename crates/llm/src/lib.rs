//! # ai-ide-llm
//!
//! 模型接入层（M2）。本 crate 是「不依赖外部凭据也能跑通」的骨架，包含：
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`provider`] | Provider 抽象（`chat_stream` / `list_models` / `health`） |
//! | [`openai`] | OpenAI Chat Completions + SSE 适配器（M2 唯一落地的协议） |
//! | [`sse`] | SSE 分帧与厂商分片语义转译 |
//! | [`transport`] | HTTP 传输抽象与增量 UTF-8 解码（网络与逻辑解耦点） |
//! | [`registry`] | Provider 注册表与工厂 |
//! | [`store`] | Provider / 模型目录持久化（**结构上不含密钥**） |
//! | [`secret`] | 密钥内存保管与脱敏 |
//! | [`tokens`] | token 计数（精确分词 + 估算回落，`llm.count_tokens`） |
//!
//! ## 边界
//!
//! - **不含密钥落盘**：磁盘上只有 Provider 目录，密钥由主进程 safeStorage 加密保管；
//!   内核通过内存注入 [`SecretString`]，用完即弃（`docs/02` §6.3）。
//! - **不含对话编排**：消息历史、上下文注入、工具循环属于 M3，本 crate 只负责
//!   「把一次请求打到上游、把流翻译成事件」。
//! - **不含真实网络依赖的测试**：除 [`transport::ReqwestTransport`] 外，
//!   所有逻辑都能用测试替身离线覆盖，因此没有 API Key 也能验收。

pub mod error;
pub mod openai;
pub mod provider;
pub mod registry;
pub mod secret;
pub mod sse;
pub mod store;
pub mod tokens;
pub mod transport;
pub mod types;

pub use error::LlmError;
pub use openai::OpenAiProvider;
pub use provider::{CancelToken, Provider, ProviderSpec, DEFAULT_TIMEOUT_MS};
pub use registry::{build_provider, protocol_label, rebuild_registry, ProviderRegistry};
pub use secret::SecretString;
pub use store::{
    model_pk, JsonFileStore, ModelRecord, ProviderRecord, ProviderStore, StoreSnapshot,
    STORE_VERSION,
};
pub use transport::{HttpTransport, ReqwestTransport};
pub use types::{ChatRequest, EventSink, HealthReport, StreamEvent, Usage};
