//! 连接生命周期与调用门面。
//!
//! [`McpClient`] 管一条到单个 MCP Server 的连接：握手 → 就绪 → 调用 → 失效 →（可选）重连。
//!
//! ## 状态机
//!
//! ```text
//!                  handshake ok
//!   Connecting ──────────────────► Ready ──┐
//!        │                           ▲     │ 可重试失败（超时 / 断连 / 5xx）
//!        │ handshake fail            │     ▼
//!        └──────────────────────► Closed ◄── Stale ──reconnect──► Ready
//! ```
//!
//! - `Stale` 只表示「这条连接别再用了」，不代表客户端已死：下一次调用会先重连。
//! - `Closed` 是终态，任何调用都直接失败（用户主动关掉的东西不该被偷偷拉起来）。
//!
//! ## 为什么不自动重放失败的请求
//!
//! 超时 / 断连时**不重发原请求**。MCP 的 `tools/call` 大多有副作用（写文件、发消息、
//! 提交 issue），重放可能执行两次；而「超时」本身并不说明 Server 没执行。
//! 所以这里只把连接标记为 `Stale`，把「要不要再来一次」交给上层按工具语义决定。
//! 这条取舍写成注释而不是文档，是因为它直接决定了本文件的错误分支结构。

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;

use crate::config::{HttpServerConfig, McpServerConfig, StdioServerConfig};
use crate::error::McpError;
use crate::http_transport::HttpTransport;
use crate::jsonrpc::{JsonRpcNotification, JsonRpcRequest, RequestId};
use crate::protocol::{
    negotiate_version, CallToolResult, InitializeResult, McpTool, ToolsPage, CLIENT_NAME,
    CLIENT_VERSION, LATEST_PROTOCOL_VERSION,
};
use crate::stdio_transport::StdioTransport;
use crate::transport::{
    timeout_error, to_call_result, Transport, TransportFactory, TransportResult,
    DEFAULT_REQUEST_TIMEOUT, DEFAULT_STARTUP_TIMEOUT,
};

/// `tools/list` 最多翻多少页。防住「Server 一直回同一个 cursor」这种死循环。
const MAX_LIST_PAGES: usize = 50;

/// 客户端连接状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientState {
    /// 尚未完成握手。
    Connecting,
    /// 已完成握手，可发请求。
    Ready,
    /// 连接已失效，下次调用前会尝试重连。
    Stale,
    /// 已关闭（终态）。
    Closed,
}

impl ClientState {
    /// 是否可发请求（不做重连）。
    pub fn is_usable(self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// 按 [`McpServerConfig`] 建立连接。
///
/// 对外暴露是因为上层可能需要「先建 factory、稍后再握手」（例如先做配置校验再并连），
/// 也可能想把同一个 factory 交给多个客户端复用。
pub struct ConfigConnector {
    config: McpServerConfig,
}

impl ConfigConnector {
    /// 绑定一份 Server 配置。
    pub fn new(config: McpServerConfig) -> Self {
        Self { config }
    }

    /// 关联的配置。
    pub fn config(&self) -> &McpServerConfig {
        &self.config
    }
}

#[async_trait]
impl TransportFactory for ConfigConnector {
    async fn create(&self) -> TransportResult<Arc<dyn Transport>> {
        match &self.config {
            McpServerConfig::Stdio(config) => {
                let transport = StdioTransport::spawn(config).await?;
                Ok(Arc::new(transport))
            }
            McpServerConfig::Http(config) => {
                let transport = HttpTransport::new(config)?;
                Ok(Arc::new(transport))
            }
        }
    }
}

struct Inner {
    state: ClientState,
    init: Option<Arc<InitializeResult>>,
    /// 记录上一次失效原因，进诊断（不参与状态判断）。
    last_failure: Option<String>,
}

/// 一条 MCP Server 连接。
pub struct McpClient {
    server: String,
    inner: Mutex<Inner>,
    transport: Mutex<Arc<dyn Transport>>,
    factory: Option<Arc<dyn TransportFactory>>,
    next_id: AtomicI64,
    request_timeout: Duration,
    startup_timeout: Duration,
    /// 串行化重连，避免并发调用同时拉起多个进程。
    reconnect_lock: AsyncMutex<()>,
}

impl McpClient {
    /// 按配置连接并完成握手。
    pub async fn connect(config: &McpServerConfig) -> Result<Self, McpError> {
        let factory = Arc::new(ConfigConnector::new(config.clone()));
        let transport = factory.create().await?;
        let client = Self::assemble(config.name().to_string(), transport, Some(factory));
        client.handshake().await?;
        Ok(client)
    }

    /// 用外部提供的传输完成握手（测试 / 复用已有连接时用；不支持自动重连）。
    pub async fn connect_with_transport(
        server: impl Into<String>,
        transport: Arc<dyn Transport>,
    ) -> Result<Self, McpError> {
        let client = Self::assemble(server.into(), transport, None);
        client.handshake().await?;
        Ok(client)
    }

    /// 用外部工厂建连（测试重连路径时用）。
    pub async fn connect_with_factory(
        server: impl Into<String>,
        factory: Arc<dyn TransportFactory>,
    ) -> Result<Self, McpError> {
        let transport = factory.create().await?;
        let client = Self::assemble(server.into(), transport, Some(factory));
        client.handshake().await?;
        Ok(client)
    }

    fn assemble(
        server: String,
        transport: Arc<dyn Transport>,
        factory: Option<Arc<dyn TransportFactory>>,
    ) -> Self {
        Self {
            server,
            inner: Mutex::new(Inner {
                state: ClientState::Connecting,
                init: None,
                last_failure: None,
            }),
            transport: Mutex::new(transport),
            factory,
            next_id: AtomicI64::new(1),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            reconnect_lock: AsyncMutex::new(()),
        }
    }

    /// 调整单次请求超时。
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// 调整握手超时。
    pub fn with_startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }

    /// Server 名。
    pub fn server_name(&self) -> &str {
        &self.server
    }

    /// 当前状态。
    pub fn state(&self) -> ClientState {
        self.lock_inner().state
    }

    /// 握手结果（未握手时 `None`）。
    pub fn initialize_result(&self) -> Option<InitializeResult> {
        self.lock_inner().init.as_ref().map(|init| (**init).clone())
    }

    /// 协商后的协议版本。
    pub fn protocol_version(&self) -> Option<String> {
        self.lock_inner()
            .init
            .as_ref()
            .map(|init| init.protocol_version.clone())
    }

    /// 最近一次失效原因（诊断用）。
    pub fn last_failure(&self) -> Option<String> {
        self.lock_inner().last_failure.clone()
    }

    fn lock_inner(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn current_transport(&self) -> Arc<dyn Transport> {
        self.transport
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn next_request_id(&self) -> RequestId {
        RequestId::Number(self.next_id.fetch_add(1, Ordering::SeqCst))
    }

    /// 握手：`initialize` → 校验版本 → `notifications/initialized`。
    async fn handshake(&self) -> Result<InitializeResult, McpError> {
        let transport = self.current_transport();
        let request = JsonRpcRequest::new(
            self.next_request_id(),
            "initialize",
            Some(json!({
                "protocolVersion": LATEST_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": CLIENT_NAME, "version": CLIENT_VERSION }
            })),
        );

        let response = transport.request(&request, self.startup_timeout).await?;
        let value = response.into_result()?;
        let mut result: InitializeResult = serde_json::from_value(value)
            .map_err(|err| McpError::Protocol(format!("initialize 结果结构非法：{err}")))?;

        let Some(negotiated) = negotiate_version(&result.protocol_version) else {
            let supported = crate::protocol::SUPPORTED_PROTOCOL_VERSIONS.join(" / ");
            return Err(McpError::Protocol(format!(
                "Server `{}` 要求协议版本 `{}`，本客户端仅支持 {supported}",
                self.server, result.protocol_version
            )));
        };
        // 以协商结果为准（Server 可能回一个我们支持的旧版本）。
        result.protocol_version = negotiated.to_string();

        transport
            .notify(&JsonRpcNotification::new("notifications/initialized", None))
            .await?;

        {
            let mut inner = self.lock_inner();
            inner.init = Some(Arc::new(result.clone()));
            inner.state = ClientState::Ready;
            inner.last_failure = None;
        }
        tracing::info!(
            target: "mcp",
            server = %self.server,
            version = %result.protocol_version,
            server_name = %result.server_info.name,
            "MCP 握手完成"
        );
        Ok(result)
    }

    /// 取一条可用连接；`Stale` 时先重连。
    async fn ensure_transport(&self) -> Result<Arc<dyn Transport>, McpError> {
        match self.state() {
            ClientState::Ready => Ok(self.current_transport()),
            ClientState::Closed => Err(McpError::disconnected(&self.server, "客户端已关闭")),
            ClientState::Connecting | ClientState::Stale => {
                if self.factory.is_none() {
                    return Err(McpError::NotInitialized(self.server.clone()));
                }
                self.reconnect().await?;
                Ok(self.current_transport())
            }
        }
    }

    /// 重连：丢掉旧连接，重建并重新握手。
    ///
    /// 已 `Closed` 的客户端不重连——用户关掉的东西不该被调用顺手拉起来。
    pub async fn reconnect(&self) -> Result<(), McpError> {
        let _guard = self.reconnect_lock.lock().await;
        match self.state() {
            ClientState::Closed => {
                return Err(McpError::disconnected(&self.server, "客户端已关闭"));
            }
            // 别的并发调用已经修好了，直接返回。
            ClientState::Ready => return Ok(()),
            ClientState::Connecting | ClientState::Stale => {}
        }
        let Some(factory) = self.factory.clone() else {
            return Err(McpError::NotInitialized(self.server.clone()));
        };

        let old = self.current_transport();
        if let Err(err) = old.shutdown().await {
            tracing::debug!(target: "mcp", server = %self.server, error = %err, "关闭旧连接时出错（忽略）");
        }

        let fresh = factory.create().await?;
        {
            let mut guard = self
                .transport
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = fresh;
        }
        self.handshake().await?;
        tracing::info!(target: "mcp", server = %self.server, "MCP 连接已重建");
        Ok(())
    }

    /// 一次请求（含状态检查与失败后的连接失效标记）。
    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, McpError> {
        let transport = self.ensure_transport().await?;
        let request = JsonRpcRequest::new(self.next_request_id(), method, params);
        // 两道超时：传输层自己也要守（它要清理过期的等待票），这里再兜一层，
        // 保证「任何传输实现失约」都不会把调用方永久挂住。
        let attempt = transport.request(&request, self.request_timeout);
        match tokio::time::timeout(self.request_timeout, attempt).await {
            Ok(Ok(response)) => response.into_result(),
            Ok(Err(err)) => {
                self.observe_failure(&err);
                Err(err)
            }
            Err(_) => {
                let err = timeout_error(method, self.request_timeout);
                self.observe_failure(&err);
                Err(err)
            }
        }
    }

    /// 可重试的失败把连接标成 `Stale`；不可重试的（配置/协议/参数错）保持原状。
    fn observe_failure(&self, err: &McpError) {
        if !err.is_retryable() {
            return;
        }
        let mut inner = self.lock_inner();
        if inner.state == ClientState::Ready {
            inner.state = ClientState::Stale;
        }
        inner.last_failure = Some(err.to_string());
    }

    /// 发现全部工具（自动翻页）。
    pub async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let params = cursor.as_ref().map(|cursor| json!({ "cursor": cursor }));
            let value = self.request("tools/list", params).await?;
            let page: ToolsPage = serde_json::from_value(value)
                .map_err(|err| McpError::Protocol(format!("tools/list 结果结构非法：{err}")))?;
            tools.extend(page.tools);
            match page.next_cursor.filter(|cursor| !cursor.is_empty()) {
                Some(next) => cursor = Some(next),
                None => return Ok(tools),
            }
        }
        Err(McpError::Protocol(format!(
            "tools/list 分页超过 {MAX_LIST_PAGES} 页，已中止（Server 可能重复返回同一 cursor）"
        )))
    }

    /// 调用工具。
    ///
    /// 注意 `is_error = true` 时会返回 `Ok`：那是工具**执行失败**，属于正常结果
    /// （MCP 刻意让模型看到失败原因），不是传输/协议错误。
    pub async fn call_tool(
        &self,
        tool: &str,
        arguments: Value,
    ) -> Result<CallToolResult, McpError> {
        if tool.trim().is_empty() {
            return Err(McpError::Protocol("工具名不能为空".into()));
        }
        let arguments = if arguments.is_null() {
            json!({})
        } else {
            arguments
        };
        let value = self
            .request(
                "tools/call",
                Some(json!({ "name": tool, "arguments": arguments })),
            )
            .await?;
        to_call_result(value)
    }

    /// 关闭连接（幂等，可重复调用）。
    pub async fn close(&self) {
        {
            let mut inner = self.lock_inner();
            if inner.state == ClientState::Closed {
                return;
            }
            inner.state = ClientState::Closed;
        }
        let transport = self.current_transport();
        if let Err(err) = transport.shutdown().await {
            tracing::debug!(target: "mcp", server = %self.server, error = %err, "关闭连接时出错（忽略）");
        }
    }

    /// 一次握手 + 一次 `tools/call` 的便捷包装：连接用完即关。
    ///
    /// 适合「查一次就不再用」的场景；频繁调用请复用同一个 [`McpClient`]。
    pub async fn call_once(
        config: &McpServerConfig,
        tool: &str,
        arguments: Value,
    ) -> Result<CallToolResult, McpError> {
        let client = Self::connect(config).await?;
        let outcome = client.call_tool(tool, arguments).await;
        client.close().await;
        outcome
    }
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.lock_inner();
        f.debug_struct("McpClient")
            .field("server", &self.server)
            .field("state", &inner.state)
            .field("transport", &self.current_transport().label())
            .field(
                "protocol_version",
                &inner.init.as_ref().map(|init| &init.protocol_version),
            )
            .field("can_reconnect", &self.factory.is_some())
            .finish_non_exhaustive()
    }
}

/// 便捷构造：stdio 客户端。
pub async fn connect_stdio(config: &StdioServerConfig) -> Result<McpClient, McpError> {
    McpClient::connect(&McpServerConfig::Stdio(config.clone())).await
}

/// 便捷构造：HTTP 客户端。
pub async fn connect_http(config: &HttpServerConfig) -> Result<McpClient, McpError> {
    McpClient::connect(&McpServerConfig::Http(config.clone())).await
}

/// 把超时错误统一成带方法名的形态（对外暴露，方便上层自己兜超时）。
pub fn request_timeout_error(method: &str, timeout: Duration) -> McpError {
    timeout_error(method, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorClass;
    use crate::jsonrpc::JsonRpcResponse;
    use std::collections::VecDeque;

    /// 脚本化回复。
    enum Reply {
        Result(Value),
        /// JSON-RPC error。
        Rpc(i64, String),
        /// 传输层错误。
        Fail(McpError),
        /// 先睡一会儿再给结果（造超时）。
        Sleep(Duration, Box<Reply>),
    }

    #[derive(Default)]
    struct MockTransport {
        queue: Mutex<VecDeque<Reply>>,
        requests: Mutex<Vec<JsonRpcRequest>>,
        notifications: Mutex<Vec<String>>,
        closed: std::sync::atomic::AtomicBool,
        label: Option<&'static str>,
    }

    impl MockTransport {
        fn with(replies: Vec<Reply>) -> Self {
            Self {
                queue: Mutex::new(replies.into()),
                ..Default::default()
            }
        }

        fn record(&self) -> Vec<JsonRpcRequest> {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

        fn notify_count(&self) -> usize {
            self.notifications
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len()
        }
    }

    #[async_trait]
    impl Transport for MockTransport {
        fn label(&self) -> &'static str {
            self.label.unwrap_or("mock")
        }

        async fn request(
            &self,
            request: &JsonRpcRequest,
            _timeout: Duration,
        ) -> TransportResult<JsonRpcResponse> {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request.clone());
            let reply = self
                .queue
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop_front()
                .ok_or_else(|| McpError::transport("mock", "脚本已耗尽"))?;
            mock_reply(reply, request).await
        }

        async fn notify(&self, notification: &JsonRpcNotification) -> TransportResult<()> {
            self.notifications
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(notification.method.clone());
            Ok(())
        }

        async fn shutdown(&self) -> TransportResult<()> {
            self.closed.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn is_closed(&self) -> bool {
            self.closed.load(Ordering::SeqCst)
        }
    }

    async fn mock_reply(
        reply: Reply,
        request: &JsonRpcRequest,
    ) -> TransportResult<JsonRpcResponse> {
        match reply {
            Reply::Result(value) => Ok(JsonRpcResponse::success(request.id.clone(), value)),
            Reply::Rpc(code, message) => Ok(JsonRpcResponse::failure(
                Some(request.id.clone()),
                code,
                message,
            )),
            Reply::Fail(err) => Err(err),
            Reply::Sleep(duration, inner) => {
                tokio::time::sleep(duration).await;
                Box::pin(mock_reply(*inner, request)).await
            }
        }
    }

    fn init_value(version: &str) -> Value {
        json!({
            "protocolVersion": version,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "mock-server", "version": "0.1.0"}
        })
    }

    async fn client_with(replies: Vec<Reply>) -> McpClient {
        let transport = Arc::new(MockTransport::with(replies));
        McpClient::connect_with_transport("mock", transport)
            .await
            .expect("握手应成功")
    }

    #[tokio::test]
    async fn handshake_negotiates_canonical_version_and_sends_initialized() {
        let transport = Arc::new(MockTransport::with(vec![Reply::Result(init_value(
            "2025-03-26",
        ))]));
        let client =
            McpClient::connect_with_transport("mock", Arc::clone(&transport) as Arc<dyn Transport>)
                .await
                .expect("handshake");

        assert_eq!(client.state(), ClientState::Ready);
        assert_eq!(client.protocol_version().as_deref(), Some("2025-03-26"));
        assert_eq!(client.server_name(), "mock");
        assert_eq!(
            client.initialize_result().expect("init").server_info.name,
            "mock-server"
        );

        let requests = transport.record();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "initialize");
        assert_eq!(
            requests[0].params.as_ref().expect("params")["protocolVersion"],
            json!(LATEST_PROTOCOL_VERSION)
        );
        // 握手后必须发 initialized 通知，否则部分 Server 会拒绝后续请求。
        assert_eq!(transport.notify_count(), 1);
    }

    #[tokio::test]
    async fn unsupported_server_version_fails_handshake() {
        let transport = Arc::new(MockTransport::with(vec![Reply::Result(init_value(
            "1999-01-01",
        ))]));
        let err = McpClient::connect_with_transport("mock", transport)
            .await
            .expect_err("版本不受支持必须失败");
        assert_eq!(err.class(), ErrorClass::Protocol);
        assert!(err.to_string().contains("1999-01-01"), "{err}");
    }

    #[tokio::test]
    async fn handshake_rpc_error_surfaces_as_rpc_class() {
        let transport = Arc::new(MockTransport::with(vec![Reply::Rpc(-32603, "boom".into())]));
        let err = McpClient::connect_with_transport("mock", transport)
            .await
            .expect_err("必须失败");
        assert_eq!(err.class(), ErrorClass::Rpc);
    }

    #[tokio::test]
    async fn list_tools_collects_pages() {
        let client = client_with(vec![
            Reply::Result(init_value("2025-06-18")),
            Reply::Result(json!({
                "tools": [{"name": "echo", "inputSchema": {"type": "object"}}],
                "nextCursor": "p2"
            })),
            Reply::Result(json!({"tools": [{"name": "read"}]})),
        ])
        .await;
        let tools = client.list_tools().await.expect("list");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[1].name, "read");
        assert_eq!(tools[1].input_schema, json!({"type": "object"}));
    }

    #[tokio::test]
    async fn list_tools_stops_on_repeated_cursor() {
        let transport = Arc::new(MockTransport::with(
            std::iter::once(Reply::Result(init_value("2025-06-18")))
                .chain(std::iter::repeat_with(|| {
                    Reply::Result(json!({"tools": [], "nextCursor": "same"}))
                }))
                .take(MAX_LIST_PAGES + 1)
                .collect(),
        ));
        let client = McpClient::connect_with_transport("mock", transport)
            .await
            .expect("handshake");
        let err = client.list_tools().await.expect_err("必须中止");
        assert_eq!(err.class(), ErrorClass::Protocol);
    }

    #[tokio::test]
    async fn call_tool_success_returns_content() {
        let client = client_with(vec![
            Reply::Result(init_value("2025-06-18")),
            Reply::Result(json!({
                "content": [{"type": "text", "text": "你好"}],
                "isError": false
            })),
        ])
        .await;
        let result = client
            .call_tool("echo", json!({"text": "你好"}))
            .await
            .expect("call");
        assert_eq!(result.text(), "你好");
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn call_tool_sends_name_and_arguments() {
        let transport = Arc::new(MockTransport::with(vec![
            Reply::Result(init_value("2025-06-18")),
            Reply::Result(json!({"content": []})),
        ]));
        let client =
            McpClient::connect_with_transport("mock", Arc::clone(&transport) as Arc<dyn Transport>)
                .await
                .expect("handshake");
        client.call_tool("echo", Value::Null).await.expect("call");
        let requests = transport.record();
        let call = requests.last().expect("call request");
        assert_eq!(call.method, "tools/call");
        let params = call.params.as_ref().expect("params");
        assert_eq!(params["name"], json!("echo"));
        // arguments 缺省时要补 `{}`，有些 Server 不接受 null。
        assert_eq!(params["arguments"], json!({}));
    }

    #[tokio::test]
    async fn call_tool_tool_error_is_not_a_transport_error() {
        let client = client_with(vec![
            Reply::Result(init_value("2025-06-18")),
            Reply::Result(json!({
                "content": [{"type": "text", "text": "文件不存在"}],
                "isError": true
            })),
        ])
        .await;
        let result = client
            .call_tool("read", json!({"path": "x"}))
            .await
            .expect("call ok");
        assert!(result.is_error);
        assert_eq!(client.state(), ClientState::Ready);
    }

    #[tokio::test]
    async fn call_tool_rpc_error_keeps_connection_usable() {
        let client = client_with(vec![
            Reply::Result(init_value("2025-06-18")),
            Reply::Rpc(-32602, "invalid params".into()),
        ])
        .await;
        let err = client
            .call_tool("echo", json!({}))
            .await
            .expect_err("必须失败");
        assert_eq!(err.class(), ErrorClass::Rpc);
        // 参数错不是连接问题。
        assert_eq!(client.state(), ClientState::Ready);
    }

    #[tokio::test]
    async fn call_tool_timeout_marks_connection_stale() {
        let client = client_with(vec![
            Reply::Result(init_value("2025-06-18")),
            Reply::Sleep(
                Duration::from_millis(150),
                Box::new(Reply::Result(json!({"content": []}))),
            ),
        ])
        .await;
        let client = client.with_request_timeout(Duration::from_millis(20));
        let err = client
            .call_tool("slow", json!({}))
            .await
            .expect_err("必须超时");
        assert_eq!(err.class(), ErrorClass::Timeout);
        assert_eq!(client.state(), ClientState::Stale);
        assert!(client.last_failure().is_some());
    }

    #[tokio::test]
    async fn timeout_on_client_without_factory_reports_not_initialized() {
        let client = client_with(vec![
            Reply::Result(init_value("2025-06-18")),
            Reply::Fail(McpError::disconnected("mock", "pipe closed")),
        ])
        .await;
        let err = client.list_tools().await.expect_err("必须失败");
        assert_eq!(err.class(), ErrorClass::Disconnected);
        assert_eq!(client.state(), ClientState::Stale);
        // 没有工厂就没有重连手段，明确报状态错而不是假装能恢复。
        let err = client.list_tools().await.expect_err("必须失败");
        assert_eq!(err.class(), ErrorClass::State);
    }

    #[tokio::test]
    async fn stale_connection_reconnects_before_next_call() {
        struct Factory {
            created: AtomicI64,
            scripts: Mutex<VecDeque<Vec<Reply>>>,
        }

        #[async_trait]
        impl TransportFactory for Factory {
            async fn create(&self) -> TransportResult<Arc<dyn Transport>> {
                self.created.fetch_add(1, Ordering::SeqCst);
                let script = self
                    .scripts
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .pop_front()
                    .unwrap_or_default();
                Ok(Arc::new(MockTransport::with(script)))
            }
        }

        let factory = Arc::new(Factory {
            created: AtomicI64::new(0),
            scripts: Mutex::new(
                vec![
                    vec![
                        Reply::Result(init_value("2025-06-18")),
                        Reply::Fail(McpError::disconnected("mock", "进程已退出")),
                    ],
                    vec![
                        Reply::Result(init_value("2025-06-18")),
                        Reply::Result(json!({"tools": [{"name": "echo"}]})),
                    ],
                ]
                .into(),
            ),
        });

        let client = McpClient::connect_with_factory("mock", factory.clone())
            .await
            .expect("首次握手");
        assert_eq!(factory.created.load(Ordering::SeqCst), 1);

        let err = client.list_tools().await.expect_err("第一次必须失败");
        assert_eq!(err.class(), ErrorClass::Disconnected);
        assert_eq!(client.state(), ClientState::Stale);

        let tools = client.list_tools().await.expect("重连后应成功");
        assert_eq!(tools.len(), 1);
        assert_eq!(factory.created.load(Ordering::SeqCst), 2);
        assert_eq!(client.state(), ClientState::Ready);
        assert_eq!(client.last_failure(), None);
    }

    #[tokio::test]
    async fn closed_client_refuses_further_calls_and_reconnect() {
        let transport = Arc::new(MockTransport::with(vec![Reply::Result(init_value(
            "2025-06-18",
        ))]));
        let client =
            McpClient::connect_with_transport("mock", Arc::clone(&transport) as Arc<dyn Transport>)
                .await
                .expect("handshake");
        client.close().await;
        client.close().await; // 幂等
        assert_eq!(client.state(), ClientState::Closed);
        assert!(transport.closed.load(Ordering::SeqCst));

        let err = client.list_tools().await.expect_err("必须拒绝");
        assert_eq!(err.class(), ErrorClass::Disconnected);
        let err = client.reconnect().await.expect_err("必须拒绝重连");
        assert_eq!(err.class(), ErrorClass::Disconnected);
    }

    #[tokio::test]
    async fn empty_tool_name_is_rejected_locally() {
        let client = client_with(vec![Reply::Result(init_value("2025-06-18"))]).await;
        let err = client
            .call_tool("   ", json!({}))
            .await
            .expect_err("必须拒绝");
        assert_eq!(err.class(), ErrorClass::Protocol);
    }

    #[tokio::test]
    async fn debug_reports_state_and_transport_label() {
        let client = client_with(vec![Reply::Result(init_value("2025-06-18"))]).await;
        let text = format!("{client:?}");
        assert!(text.contains("McpClient"), "{text}");
        assert!(text.contains("Ready"), "{text}");
        assert!(text.contains("mock"), "{text}");
        client.close().await;
    }
}
