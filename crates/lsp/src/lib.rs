//! LSP 客户端基础层。
//!
//! 目标不是「实现编辑器功能」，而是把**与语言服务器对话的协议底座**做扎实：
//! 分帧、id 关联、通知广播、进程生命周期、握手、文档同步。上层（`crates/core`）
//! 再在这个底座上翻译成 `lsp.*` 能力（hover / definition / diagnostics…）。
//!
//! ## 分层
//!
//! ```text
//! LspClient          client.rs      状态机、握手、文档同步、优雅关闭
//!   ├── document.rs                 didOpen / didChange(full) / didClose + 版本号表
//!   ├── initialize.rs               initialize / initialized / shutdown / exit + 能力声明
//!   └── Transport (trait)           transport.rs   抽象与常量
//!         └── StdioTransport        stdio_transport.rs  子进程 + 读写线程 + id 票表
//!               ├── framing.rs      Content-Length 分帧（字节边界、上限保护）
//!               └── jsonrpc.rs      JSON-RPC 2.0 编解码（LSP 方言）
//! error.rs                          错误分类（可重连 / 致命 / 业务错）
//! config.rs                         启动配置
//! ```
//!
//! ## 三条贯穿全层的约束
//!
//! 1. **帧边界错了就必须断开**。`Content-Length` 缺/非法/超限、消息体不是 UTF-8、
//!    JSON-RPC 结构不对——命中任一条，后续字节都不可信，继续读只会把错误数据
//!    配给别的请求。所以 [`LspError::is_fatal`] 为真的错误一律作废整条连接。
//! 2. **超时由传输层落实并撕票**。请求超时后迟到的响应还在管道里，传输层必须
//!    丢掉那张已过期的票，否则下一次同 id 响应会错配。
//! 3. **通知不能被消费方拖慢**。诊断可能成百上千条涌来，走广播通道，
//!    订阅者落后只收到 `Lagged`。
//!
//! ## 用法
//!
//! ```text
//! let config = ServerConfig::new("rust-analyzer", "rust-analyzer")
//!     .arg("--stdio")
//!     .cwd(workspace_root);
//! let client = LspClient::start(&config).await?;      // spawn + initialize + initialized
//! let uri = client.open_document(&path, &text).await?; // didOpen（版本从 1 开始）
//! let hover = client.request("textDocument/hover", Some(params)).await?;
//! client.change_document(&uri, &new_text).await?;      // didChange（full，版本 +1）
//! client.shutdown().await?;                            // shutdown → exit → kill 兜底
//! ```
//!
//! ## 本轮范围
//!
//! 只做 **full** 文档同步（`TextDocumentSyncKind::Full`）。增量同步需要客户端做
//! 编辑坐标换算，算错会让服务器文档与编辑器永久错位——留给后续切片配
//! `didChange` 的 range 支持，本层先把「整篇替换」这条最稳的路走通。

#![deny(missing_docs)]
#![deny(unsafe_code)]

mod client;
mod config;
mod document;
mod error;
mod framing;
mod initialize;
mod jsonrpc;
mod stdio_transport;
mod transport;

pub use client::LspClient;
pub use config::ServerConfig;
pub use document::{
    language_id_for_path, path_to_uri, uri_to_path, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DocumentSync,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    VersionedTextDocumentIdentifier, METHOD_DID_CHANGE, METHOD_DID_CLOSE, METHOD_DID_OPEN,
};
pub use error::{ErrorClass, LspError};
pub use framing::{
    encode_frame, parse_content_length, FrameReader, TransportLimits, DEFAULT_MAX_CONTENT_BYTES,
    DEFAULT_MAX_HEADER_BYTES,
};
pub use initialize::{
    ClientCapabilities, ClientInfo, InitializeParams, InitializeResult, Provider,
    ProviderCapability, ServerCapabilities, ServerInfo, TextDocumentSync, TextDocumentSyncKind,
    TextDocumentSyncOptions, WorkspaceFolder, METHOD_EXIT, METHOD_INITIALIZE, METHOD_INITIALIZED,
    METHOD_SHUTDOWN,
};
pub use jsonrpc::{
    decode_text, decode_value, encode_notification, encode_request, encode_response,
    error_response, Incoming, JsonRpcError, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    RequestId, INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, JSONRPC_VERSION, METHOD_NOT_FOUND,
    PARSE_ERROR,
};
pub use stdio_transport::StdioTransport;
pub use transport::{
    ServerNotification, Transport, TransportResult, DEFAULT_REQUEST_TIMEOUT,
    DEFAULT_STARTUP_TIMEOUT, EXIT_STATUS_GRACE, SHUTDOWN_GRACE,
};
