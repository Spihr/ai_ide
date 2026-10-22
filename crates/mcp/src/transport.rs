//! 传输抽象。
//!
//! 上层（[`crate::McpClient`]）只关心三件事：发请求拿响应、发通知、关闭。
//! stdio 与 HTTP 的差异全部关在 trait 后面——这样超时、重连、状态机的单测
//! 可以用一个内存假传输来跑，不必真的起进程或连网络。
//!
//! 契约里有一条容易被低估：**超时必须由传输层落实**。因为超时后迟到的响应
//! 仍然会到，传输层必须自己把那张「已过期的 id → 等待者」的票撕掉，
//! 否则下一次同 id 的响应就会被错配。

use async_trait::async_trait;
use serde_json::Value;

use crate::error::McpError;
use crate::jsonrpc::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};

/// 单次请求默认超时。
pub const DEFAULT_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 启动（spawn / 首次连接）默认超时。
pub const DEFAULT_STARTUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// 传输层操作结果。
pub type TransportResult<T> = Result<T, McpError>;

/// 一条 MCP 连接。
#[async_trait]
pub trait Transport: Send + Sync {
    /// 传输类型标签（`stdio` / `http` / `http(sse)`），进诊断信息。
    fn label(&self) -> &'static str;

    /// 发送请求并等待**同 id** 的响应。
    ///
    /// 实现必须保证：`timeout` 到点即返回 [`McpError::Timeout`]，
    /// 并且不再让迟到的响应影响后续请求。
    async fn request(
        &self,
        request: &JsonRpcRequest,
        timeout: std::time::Duration,
    ) -> TransportResult<JsonRpcResponse>;

    /// 发送通知（不等响应）。
    async fn notify(&self, notification: &JsonRpcNotification) -> TransportResult<()>;

    /// 关闭连接并释放资源（幂等）。
    async fn shutdown(&self) -> TransportResult<()>;

    /// 连接是否已被判定为不可用。
    async fn is_closed(&self) -> bool {
        false
    }
}

/// 连接工厂。
///
/// 抽出来是为了让「重连」可测：真实现按配置重新 spawn / 重新建 HTTP 客户端，
/// 测试实现可以数一数到底重建了几次、每次给什么脚本。
#[async_trait]
pub trait TransportFactory: Send + Sync {
    /// 建一条新连接。失败必须是 [`McpError::Spawn`] / [`McpError::Config`] 之类的具体错误，
    /// 不要吞成「未知错误」——上层要靠分类决定是否提示用户改配置。
    async fn create(&self) -> TransportResult<std::sync::Arc<dyn Transport>>;
}

/// 统一构造超时错误，避免各处手拼毫秒数。
pub(crate) fn timeout_error(method: &str, timeout: std::time::Duration) -> McpError {
    McpError::Timeout {
        method: method.to_string(),
        // 亚毫秒预算上取整成 1，避免显示成 0ms 让人以为没设超时。
        timeout_ms: timeout.as_millis().max(1) as u64,
    }
}

/// 把一次 `tools/call` 的 `result` 收敛成 [`crate::protocol::CallToolResult`]。
///
/// 放在传输层旁边而不是 `protocol` 里，是因为「同一份结果可能来自 JSON body
/// 或 SSE data 帧」这个差异属于传输层知识。
pub(crate) fn to_call_result(value: Value) -> Result<crate::protocol::CallToolResult, McpError> {
    serde_json::from_value(value)
        .map_err(|err| McpError::Protocol(format!("tools/call 结果结构非法：{err}")))
}
