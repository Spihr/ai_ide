//! LSP 客户端：把传输、握手、文档同步三块拼成一个可用的门面。
//!
//! ## 状态机
//!
//! ```text
//! connect()       → Starting（进程已起，但还没握手）
//! initialize()    → Ready
//! shutdown()      → Closed
//! ```
//!
//! `Starting` 阶段只允许发 `initialize`。规范对这一点是硬要求：服务器在回
//! `initialize` 之前收到的其他请求**必须**被丢弃（实际实现里往往直接崩）。
//! 所以除了 [`LspClient::initialize`]，其余所有出站操作都先过 `ensure_ready`。
//!
//! ## 为什么 `shutdown` 是四步而不是一步
//!
//! 规范给了「优雅退出」的固定流程：`shutdown` 请求 → 等 `null` → `exit` 通知 →
//! 进程自行退出。少发 `exit` 的话 `rust-analyzer` 这类服务器会挂住不退出，
//! 客户端只能 kill；而 kill 会让它来不及落盘索引缓存，下次启动又要重建。
//! 所以这里先按规范走完，**最后**才由传输层兜底 kill。

use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::config::ServerConfig;
use crate::document::{
    language_id_for_path, path_to_uri, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentSync, METHOD_DID_CHANGE, METHOD_DID_CLOSE, METHOD_DID_OPEN,
};
use crate::error::LspError;
use crate::initialize::{
    InitializeParams, InitializeResult, ServerCapabilities, ServerInfo, METHOD_EXIT,
    METHOD_INITIALIZE, METHOD_INITIALIZED, METHOD_SHUTDOWN,
};
use crate::jsonrpc::{JsonRpcNotification, JsonRpcRequest, RequestId};
use crate::stdio_transport::StdioTransport;
use crate::transport::{
    ServerNotification, Transport, DEFAULT_REQUEST_TIMEOUT, DEFAULT_STARTUP_TIMEOUT,
};

/// `shutdown` 请求自己的超时：服务器可能正忙于索引，给得比普通请求短，
/// 反正后面还有 `exit` 与 kill 兜底。
const SHUTDOWN_REQUEST_TIMEOUT: Duration = Duration::from_secs(1);

/// 客户端状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientState {
    /// 进程已起，未握手。
    Starting,
    /// 握手完成，可正常发请求。
    Ready,
    /// 已关闭（主动关闭或连接断裂）。
    Closed,
}

/// 请求 id 分配器。
///
/// 单调递增、不重用。单独抽出来是为了能脱离连接单测「id 会不会撞」——
/// id 撞了就是响应错配，是 LSP 最难查的一类 bug。
#[derive(Debug)]
struct IdAllocator(AtomicI64);

impl IdAllocator {
    fn new() -> Self {
        // 从 1 开始：0 在部分实现里被当成「空 id」。
        Self(AtomicI64::new(1))
    }

    fn next(&self) -> RequestId {
        // 超过 i64 上界要几十亿年，不考虑回绕。
        RequestId::Number(self.0.fetch_add(1, Ordering::SeqCst))
    }
}

/// 一个 LSP 连接。
pub struct LspClient {
    server: String,
    root_uri: Option<String>,
    transport: Arc<StdioTransport>,
    state: StdMutex<ClientState>,
    initialized: StdMutex<Option<InitializeResult>>,
    documents: DocumentSync,
    request_timeout: Duration,
    ids: IdAllocator,
}

impl LspClient {
    /// 起进程但不握手；之后必须调用 [`LspClient::initialize`] 才能发请求。
    pub async fn connect(config: &ServerConfig) -> Result<Self, LspError> {
        let transport = StdioTransport::spawn(config).await?;
        Ok(Self {
            server: config.name.clone(),
            root_uri: root_uri_of(config),
            transport: Arc::new(transport),
            state: StdMutex::new(ClientState::Starting),
            initialized: StdMutex::new(None),
            documents: DocumentSync::new(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            ids: IdAllocator::new(),
        })
    }

    /// 起进程并完成握手（`initialize` + `initialized`）。
    pub async fn start(config: &ServerConfig) -> Result<Self, LspError> {
        let client = Self::connect(config).await?;
        client.initialize().await?;
        Ok(client)
    }

    /// 覆盖单次请求超时（测试里会调小）。
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// 原地覆盖单次请求超时（连上之后再改，例如按服务器索引规模分级）。
    pub fn set_request_timeout(&mut self, timeout: Duration) {
        self.request_timeout = timeout;
    }

    /// 执行握手：`initialize` 请求 → `initialized` 通知。
    ///
    /// 幂等：已握手时直接返回既有结果，方便上层重试逻辑写一遍。
    pub async fn initialize(&self) -> Result<InitializeResult, LspError> {
        match *self.lock_state() {
            ClientState::Ready => {
                if let Some(result) = self.initialized_result() {
                    return Ok(result);
                }
            }
            ClientState::Closed => return Err(LspError::Closed(self.server.clone())),
            ClientState::Starting => {}
        }

        let params = InitializeParams::new(self.root_uri.clone());
        let params = serde_json::to_value(&params)
            .map_err(|err| LspError::protocol(format!("initialize 参数序列化失败：{err}")))?;
        let value = self
            .send_request(METHOD_INITIALIZE, Some(params), DEFAULT_STARTUP_TIMEOUT)
            .await?;
        let result: InitializeResult = serde_json::from_value(value)
            .map_err(|err| LspError::protocol(format!("initialize 回包字段非法：{err}")))?;

        // `initialized` 必须是通知；写成请求会被服务器回 -32601 并停止索引。
        self.send_notification(METHOD_INITIALIZED, Some(serde_json::json!({})))
            .await?;

        *self.lock_initialized() = Some(result.clone());
        *self.lock_state() = ClientState::Ready;
        Ok(result)
    }

    /// 握手结果（未握手时为 `None`）。
    pub fn initialized_result(&self) -> Option<InitializeResult> {
        self.lock_initialized().clone()
    }

    /// 服务器信息（`serverInfo`）。
    pub fn server_info(&self) -> Option<ServerInfo> {
        self.lock_initialized()
            .as_ref()
            .and_then(|result| result.server_info.clone())
    }

    /// 服务器能力。
    pub fn capabilities(&self) -> Option<ServerCapabilities> {
        self.lock_initialized()
            .as_ref()
            .map(|result| result.capabilities.clone())
    }

    /// 服务器名。
    pub fn server_name(&self) -> &str {
        &self.server
    }

    /// 文档同步表（版本号在这里，别在上层自己记）。
    pub fn documents(&self) -> &DocumentSync {
        &self.documents
    }

    /// 订阅服务器通知（诊断 / 日志 / 进度）。
    pub fn subscribe(&self) -> broadcast::Receiver<ServerNotification> {
        self.transport.subscribe()
    }

    /// 单次请求超时。
    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    /// 发请求并等同 id 的响应，返回原始 `result`。
    pub async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, LspError> {
        self.ensure_ready(method)?;
        self.send_request(method, params, self.request_timeout)
            .await
    }

    /// 发请求并把 `result` 解成具体类型。
    pub async fn request_typed<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<T, LspError> {
        let value = self.request(method, params).await?;
        serde_json::from_value(value)
            .map_err(|err| LspError::protocol(format!("`{method}` 的 result 字段非法：{err}")))
    }

    /// 发通知。
    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), LspError> {
        self.ensure_ready(method)?;
        self.send_notification(method, params).await
    }

    /// 打开文档并发送 `didOpen`，返回文档 uri。
    pub async fn open_document(&self, path: &Path, text: &str) -> Result<String, LspError> {
        self.ensure_ready(METHOD_DID_OPEN)?;
        let uri = path_to_uri(path);
        let params = self
            .documents
            .open(uri.clone(), language_id_for_path(path), text);
        self.send_params(METHOD_DID_OPEN, &params).await?;
        Ok(uri)
    }

    /// 打开一个 uri（调用方自己给 uri 与语言标识，例如虚拟文档）。
    pub async fn open_document_at(
        &self,
        uri: impl Into<String>,
        language_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<DidOpenTextDocumentParams, LspError> {
        self.ensure_ready(METHOD_DID_OPEN)?;
        let params = self.documents.open(uri, language_id, text);
        self.send_params(METHOD_DID_OPEN, &params).await?;
        Ok(params)
    }

    /// 全文变更并发送 `didChange`，返回新版本号。
    pub async fn change_document(&self, uri: &str, text: &str) -> Result<i32, LspError> {
        self.ensure_ready(METHOD_DID_CHANGE)?;
        let params = self.documents.change_full(uri, text)?;
        let version = params.text_document.version;
        self.send_params(METHOD_DID_CHANGE, &params).await?;
        Ok(version)
    }

    /// 关闭文档并发送 `didClose`。
    pub async fn close_document(&self, uri: &str) -> Result<(), LspError> {
        self.ensure_ready(METHOD_DID_CLOSE)?;
        let params: DidCloseTextDocumentParams = self.documents.close(uri)?;
        self.send_params(METHOD_DID_CLOSE, &params).await
    }

    /// 当前打开的文档数。
    pub fn open_document_count(&self) -> usize {
        self.documents.len()
    }

    /// 优雅关闭：`shutdown` → `exit` → 传输层兜底 kill。幂等。
    pub async fn shutdown(&self) -> Result<(), LspError> {
        if *self.lock_state() == ClientState::Closed {
            return Ok(());
        }
        // 先把状态挪到 Closed 再发 shutdown：这期间任何新请求都会被立刻拒绝，
        // 不会插进「已发 shutdown、还没收到回包」的空窗。
        *self.lock_state() = ClientState::Closed;

        let request = JsonRpcRequest::new(self.ids.next(), METHOD_SHUTDOWN, None);
        // shutdown 回包形态不规范（null / {} 都见过），这里只关心控制权，
        // 失败也不打断后续 exit。
        let _ = self
            .transport
            .request(&request, SHUTDOWN_REQUEST_TIMEOUT)
            .await;

        if !self.transport.is_closed() {
            let exit = JsonRpcNotification::new(METHOD_EXIT, None);
            let _ = self.transport.notify(&exit).await;
        }
        self.transport.shutdown().await
    }

    /// 连接是否已不可用。
    pub fn is_closed(&self) -> bool {
        *self.lock_state() == ClientState::Closed || self.transport.is_closed()
    }

    fn ensure_ready(&self, method: &str) -> Result<(), LspError> {
        if self.transport.is_closed() {
            return Err(LspError::Closed(self.server.clone()));
        }
        match *self.lock_state() {
            ClientState::Ready => Ok(()),
            ClientState::Starting => Err(LspError::NotInitialized(format!(
                "{}（`{method}` 必须在 initialize 之后）",
                self.server
            ))),
            ClientState::Closed => Err(LspError::Closed(self.server.clone())),
        }
    }

    async fn send_request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, LspError> {
        let request = JsonRpcRequest::new(self.ids.next(), method, params);
        let response = self.transport.request(&request, timeout).await?;
        response.into_result()
    }

    async fn send_notification(&self, method: &str, params: Option<Value>) -> Result<(), LspError> {
        let notification = JsonRpcNotification::new(method, params);
        self.transport.notify(&notification).await
    }

    async fn send_params<T: serde::Serialize>(
        &self,
        method: &str,
        params: &T,
    ) -> Result<(), LspError> {
        let value = serde_json::to_value(params)
            .map_err(|err| LspError::protocol(format!("`{method}` 参数序列化失败：{err}")))?;
        self.send_notification(method, Some(value)).await
    }

    fn lock_state(&self) -> MutexGuard<'_, ClientState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_initialized(&self) -> MutexGuard<'_, Option<InitializeResult>> {
        self.initialized
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl std::fmt::Debug for LspClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspClient")
            .field("server", &self.server)
            .field("state", &*self.lock_state())
            .field("open_documents", &self.documents.len())
            .finish_non_exhaustive()
    }
}

/// 从配置推根目录 uri：只有显式给了工作目录才声明工作区。
///
/// `rootUri` 声明错的代价很实在——服务器会按那个目录做索引，指错了就是
/// 「能连上但所有查询为空」。
fn root_uri_of(config: &ServerConfig) -> Option<String> {
    config.cwd.as_deref().map(path_to_uri)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_start_at_one_and_never_repeat() {
        let ids = IdAllocator::new();
        assert_eq!(ids.next(), RequestId::Number(1));
        assert_eq!(ids.next(), RequestId::Number(2));
        assert_eq!(ids.next(), RequestId::Number(3));
    }

    #[test]
    fn root_uri_requires_explicit_cwd() {
        let config = ServerConfig::new("mock", "mock");
        assert_eq!(root_uri_of(&config), None);
        let with_cwd = config.cwd(r"E:\ai_ide");
        assert_eq!(root_uri_of(&with_cwd).as_deref(), Some("file:///E:/ai_ide"));
    }
}
