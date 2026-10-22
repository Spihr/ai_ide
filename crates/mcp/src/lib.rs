//! M7 · 内核 MCP 客户端。
//!
//! 职责边界（docs/01 FR-5.1、docs/04 §8）：把「外部 MCP Server」变成内核可用的工具来源，
//! 只做协议与连接，**不做权限判定、不做工具路由、不碰 UI**。上层拿到
//! [`protocol::McpTool`] 后自行拼进工具表，命名空间规则见 [`protocol::tool_namespace`]。
//!
//! 分层：
//!
//! | 模块 | 内容 |
//! |---|---|
//! | [`jsonrpc`] | JSON-RPC 2.0 报文编解码、错误码常量 |
//! | [`sse`] | `text/event-stream` 分帧（跨 chunk 组帧、忽略注释） |
//! | [`transport`] | [`transport::Transport`] / [`transport::TransportFactory`] 抽象与超时常量 |
//! | [`stdio_transport`] | stdio：spawn 子进程 + 逐行帧 + 按 id 派发 |
//! | [`http_transport`] | Streamable HTTP：POST + JSON/SSE 响应 + session 头 |
//! | [`protocol`] | 协议数据模型、版本协商、工具命名空间 |
//! | [`config`] | `%APPDATA%/ai-ide/mcp.json` 解析与 `${ENV}` 展开 |
//! | [`client`] | 连接生命周期（握手 / 发现 / 调用 / 重连 / 关闭） |
//! | [`error`] | 错误枚举与分类（超时、断连、协议、RPC、配置） |
//!
//! 几个刻意的取舍：
//!
//! - **工具执行失败不是 `Err`**：`tools/call` 返回 `isError: true` 时给 `Ok`，
//!   让模型自己看到失败原因（MCP 的设计意图），只有协议/连接层面的问题才 `Err`。
//! - **Server 之间互相隔离**：一个 Server 挂了只会把它自己标成 `Stale`，
//!   不影响其他 Server，也不影响内置工具。
//! - **配置里的密钥不落日志**：`Debug` 只打键名，`${ENV}` 缺失时报变量名不报值。
//!
//! ```no_run
//! use ai_ide_mcp::{McpClient, McpConfig};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let config = McpConfig::load_default()?;
//! for (name, server) in &config.servers {
//!     let client = McpClient::connect(server).await?;
//!     for tool in client.list_tools().await? {
//!         println!("{}", ai_ide_mcp::tool_namespace(name, &tool.name));
//!     }
//! }
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod client;
pub mod config;
pub mod error;
pub mod http_transport;
pub mod jsonrpc;
pub mod protocol;
pub mod sse;
pub mod stdio_transport;
pub mod transport;

pub use client::{
    connect_http, connect_stdio, request_timeout_error, ClientState, ConfigConnector, McpClient,
};
pub use config::{
    default_config_path, expand_env, EnvSource, HttpServerConfig, McpConfig, McpServerConfig,
    ProcessEnv, StdioServerConfig, CONFIG_PATH_ENV,
};
pub use error::{ErrorClass, McpError};
pub use http_transport::HttpTransport;
pub use protocol::{
    negotiate_version, split_namespaced_tool, tool_namespace, CallToolResult, InitializeResult,
    McpContent, McpTool, ToolsPage, LATEST_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS,
    TOOL_NAMESPACE_PREFIX, TOOL_NAMESPACE_SEPARATOR,
};
pub use stdio_transport::StdioTransport;
pub use transport::{DEFAULT_REQUEST_TIMEOUT, DEFAULT_STARTUP_TIMEOUT};
