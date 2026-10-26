//! 传输抽象与共享常量。
//!
//! 上层（[`crate::LspClient`]）只关心几件事：发请求拿响应、发通知、订阅服务器通知、关闭。
//! 抽象成 trait 的价值在于：以后接 `--pipe`/socket 传输时，客户端的状态机与文档同步
//! 不用改；单测也能用内存假传输验证「超时撕票」「关闭后拒绝发送」这类契约。
//!
//! 契约里有一条容易被低估：**超时必须由传输层落实**。超时后迟到的响应仍在管道里，
//! 传输层必须自己把那张「已过期的 id → 等待者」的票撕掉，否则下一次同 id 的响应
//! 就会被错配。见 [`crate::StdioTransport`]。

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::error::LspError;
use crate::jsonrpc::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};

/// 单次请求默认超时。
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 启动（spawn + initialize 握手）默认超时。
///
/// 比请求超时短：语言服务器起不来通常几秒内就能看出来，
/// 启动慢到 10 秒以上基本是索引卡住或命令配错。
pub const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// 关闭时等待子进程退出的上限。
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// 读到 EOF 后等待退出码的上限（为了把退出码写进错误原因）。
pub const EXIT_STATUS_GRACE: Duration = Duration::from_secs(1);

/// 传输层操作结果。
pub type TransportResult<T> = Result<T, LspError>;

/// 服务器主动发来的一条通知。
///
/// 本层不解释语义（`publishDiagnostics` 的参数怎么解是上层的事），只保证
/// **方法名 + 参数原样到达**。诊断先落到广播通道，UI 订阅后自行渲染。
#[derive(Debug, Clone, PartialEq)]
pub struct ServerNotification {
    /// 方法名，例如 `textDocument/publishDiagnostics`。
    pub method: String,
    /// 原始参数。
    pub params: Option<Value>,
}

/// 一条 LSP 连接。
#[async_trait]
pub trait Transport: Send + Sync {
    /// 传输类型标签（目前只有 `stdio`），进诊断信息。
    fn label(&self) -> &'static str;

    /// 发送请求并等待**同 id** 的响应。
    ///
    /// 实现必须保证：`timeout` 到点即返回 [`LspError::Timeout`]，
    /// 并且不再让迟到的响应影响后续请求。
    async fn request(
        &self,
        request: &JsonRpcRequest,
        timeout: Duration,
    ) -> TransportResult<JsonRpcResponse>;

    /// 发送通知（不等响应）。
    async fn notify(&self, notification: &JsonRpcNotification) -> TransportResult<()>;

    /// 订阅服务器通知（`publishDiagnostics` / `window/logMessage` 等）。
    ///
    /// 广播语义：多个订阅者各收一份；订阅者落后太多时只会收到
    /// [`broadcast::error::RecvError::Lagged`]，不会拖慢读循环。
    fn subscribe(&self) -> broadcast::Receiver<ServerNotification>;

    /// 关闭连接并释放资源（幂等）。
    async fn shutdown(&self) -> TransportResult<()>;

    /// 连接是否已被判定为不可用（服务器退出 / 帧非法 / 主动关闭）。
    fn is_closed(&self) -> bool;
}

/// 统一构造超时错误，避免各处手拼毫秒数。
pub(crate) fn timeout_error(method: &str, timeout: Duration) -> LspError {
    LspError::Timeout {
        method: method.to_string(),
        // 亚毫秒预算上取整成 1，避免显示成 0ms 让人以为没设超时。
        timeout_ms: timeout.as_millis().max(1) as u64,
    }
}
