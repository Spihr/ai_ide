//! stdio 传输：子进程 + `Content-Length` 分帧 + 按 id 派发。
//!
//! ## 为什么要自己管 id → 等待者
//!
//! stdio 是**共享一条流**的：并发的 `textDocument/definition` 与
//! `textDocument/documentSymbol` 响应会按服务器自己的顺序回来，可能乱序。
//! 所以必须建 `id → oneshot` 票表，由读循环按 id 派发；请求超时时还要把票撕掉
//! （见 [`StdioTransport::request`]），否则迟到的响应会污染后续请求。
//!
//! ## 为什么读循环只由 EOF / 坏帧结束，而不是 select + tick
//!
//! 分帧读的中间态（读了头没读体）不可重入：被 timeout 中途取消后缓冲里可能留着
//! 半个消息体，重试会读到错位的 JSON。这里让读循环跑到「流结束」或「帧边界不可信」
//! 为止，靠关 stdin 与 kill 子进程来终止，从根上避免取消语义。
//!
//! ## 通知为什么走 broadcast
//!
//! `textDocument/publishDiagnostics` 会在任何时候来，且可能有多个消费方
//! （编辑器标注、问题面板、日志）。广播通道让读循环只做派发、不等消费，
//! 订阅者落后时只会收到 `Lagged`，不会把管道堵死。
//!
//! ## 服务器反向请求
//!
//! 服务器会发 `client/registerCapability`、`window/workDoneProgress/create`、
//! `workspace/configuration` 这类请求。不回包会让某些服务器卡在初始化阶段，
//! 所以这里统一回：能力登记类回 `null`，配置类回等长 `null` 数组，其余回 `-32601`。

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, oneshot, Mutex};

use crate::config::ServerConfig;
use crate::error::LspError;
use crate::framing::{encode_frame, FrameReader};
use crate::jsonrpc::{
    decode_text, encode_notification, encode_request, encode_response, Incoming,
    JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, METHOD_NOT_FOUND,
};
use crate::transport::{
    timeout_error, ServerNotification, Transport, TransportResult, EXIT_STATUS_GRACE,
    SHUTDOWN_GRACE,
};

/// 通知广播的容量。诊断风暴（一次编译几百条）也不至于让订阅者立刻 `Lagged`。
const NOTIFICATION_CHANNEL_CAPACITY: usize = 256;

/// stderr 单行日志的截断长度。
const STDERR_LINE_LIMIT: usize = 512;

type Pending =
    HashMap<crate::jsonrpc::RequestId, oneshot::Sender<TransportResult<JsonRpcResponse>>>;

/// 读循环、写端与请求侧共享的状态。
struct Shared {
    server: String,
    pending: StdMutex<Pending>,
    closed: AtomicBool,
    notifications: broadcast::Sender<ServerNotification>,
}

impl Shared {
    fn lock_pending(&self) -> MutexGuard<'_, Pending> {
        // 读循环 panic 会毒化锁；为了还能把「连接已断」传给所有等待者，
        // 这里取回内部数据继续用（数据本身就是一张票表，没有不变量可破坏）。
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn fail_all(&self, error: LspError) {
        let drained: Vec<_> = self
            .lock_pending()
            .drain()
            .map(|(_, sender)| sender)
            .collect();
        for sender in drained {
            let _ = sender.send(Err(error.clone()));
        }
    }

    /// 标记关闭并把同一个错误抛给所有等待者（幂等：已关闭时只抛不重复标记）。
    fn close_with(&self, error: LspError) {
        self.closed.store(true, Ordering::SeqCst);
        self.fail_all(error);
    }
}

/// stdio 连接。
pub struct StdioTransport {
    server: String,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    child: Arc<Mutex<Option<Child>>>,
    shared: Arc<Shared>,
}

impl StdioTransport {
    /// 启动子进程并接管它的 stdin/stdout/stderr。
    ///
    /// 这一步**只起进程**，不发 `initialize`——握手是 [`crate::LspClient`] 的事。
    pub async fn spawn(config: &ServerConfig) -> TransportResult<Self> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // 客户端被 drop 时不能留下孤儿语言服务器（它会一直吃 CPU 建索引）。
            .kill_on_drop(true);
        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in &config.env {
            command.env(key, value);
        }

        let mut child = command.spawn().map_err(|err| LspError::Spawn {
            server: config.name.clone(),
            // 只带命令名，不带 env（可能含 token）。
            reason: format!("无法执行 `{}`：{err}", config.command),
        })?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| LspError::Spawn {
            server: config.name.clone(),
            reason: "子进程 stdout 未就绪".into(),
        })?;
        let stderr = child.stderr.take();

        let (notifications, _) = broadcast::channel(NOTIFICATION_CHANNEL_CAPACITY);
        let shared = Arc::new(Shared {
            server: config.name.clone(),
            pending: StdMutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            notifications,
        });
        let stdin = Arc::new(Mutex::new(stdin));
        let child = Arc::new(Mutex::new(Some(child)));

        if let Some(stderr) = stderr {
            spawn_stderr_drain(config.name.clone(), stderr);
        }
        spawn_reader(
            stdout,
            config.limits,
            Arc::clone(&shared),
            Arc::clone(&stdin),
            Arc::clone(&child),
        );

        Ok(Self {
            server: config.name.clone(),
            stdin,
            child,
            shared,
        })
    }

    /// 服务器名（进错误文本）。
    pub fn server_name(&self) -> &str {
        &self.server
    }

    /// 发一帧消息体到子进程 stdin。
    async fn write_body(&self, body: &str, hint: &str) -> TransportResult<()> {
        write_body(&self.stdin, &self.shared, body, hint).await
    }
}

async fn write_body(
    stdin: &Arc<Mutex<Option<ChildStdin>>>,
    shared: &Arc<Shared>,
    body: &str,
    hint: &str,
) -> TransportResult<()> {
    if shared.closed.load(Ordering::SeqCst) {
        return Err(LspError::Closed(shared.server.clone()));
    }
    let frame = encode_frame(body);
    let mut guard = stdin.lock().await;
    let Some(stdin) = guard.as_mut() else {
        return Err(LspError::Closed(shared.server.clone()));
    };
    let outcome = async {
        use tokio::io::AsyncWriteExt as _;
        stdin.write_all(&frame).await?;
        stdin.flush().await
    }
    .await;
    if let Err(err) = outcome {
        // 写不进去说明管道断了：立刻标记不可用，让后续请求快速失败而不是排队等超时。
        let error = LspError::transport(&shared.server, format!("写入 {hint} 失败：{err}"));
        shared.close_with(error.clone());
        return Err(error);
    }
    Ok(())
}

#[async_trait]
impl Transport for StdioTransport {
    fn label(&self) -> &'static str {
        "stdio"
    }

    async fn request(
        &self,
        request: &JsonRpcRequest,
        timeout: Duration,
    ) -> TransportResult<JsonRpcResponse> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(LspError::Closed(self.server.clone()));
        }
        let body = encode_request(request)?;
        let (sender, receiver) = oneshot::channel();
        self.shared
            .lock_pending()
            .insert(request.id.clone(), sender);

        if let Err(err) = self.write_body(&body, &request.method).await {
            self.shared.lock_pending().remove(&request.id);
            return Err(err);
        }

        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(Ok(response))) => Ok(response),
            Ok(Ok(Err(err))) => Err(err),
            Ok(Err(_)) => Err(LspError::Closed(self.server.clone())),
            Err(_) => {
                // 撕票：迟到的响应会在读循环里因找不到 id 被丢弃，不会错配给下一个请求。
                self.shared.lock_pending().remove(&request.id);
                Err(timeout_error(&request.method, timeout))
            }
        }
    }

    async fn notify(&self, notification: &JsonRpcNotification) -> TransportResult<()> {
        let body = encode_notification(notification)?;
        self.write_body(&body, &notification.method).await
    }

    fn subscribe(&self) -> broadcast::Receiver<ServerNotification> {
        self.shared.notifications.subscribe()
    }

    async fn shutdown(&self) -> TransportResult<()> {
        let already_closed = self.shared.closed.swap(true, Ordering::SeqCst);
        // 关掉写端：正常退出的服务器见到 EOF 会自行收尾。
        drop(self.stdin.lock().await.take());
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await;
        }
        if !already_closed {
            self.shared.fail_all(LspError::Closed(self.server.clone()));
        }
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::SeqCst)
    }
}

impl std::fmt::Debug for StdioTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StdioTransport")
            .field("server", &self.server)
            .field("label", &"stdio")
            .finish_non_exhaustive()
    }
}

fn spawn_reader(
    stdout: tokio::process::ChildStdout,
    limits: crate::framing::TransportLimits,
    shared: Arc<Shared>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    child: Arc<Mutex<Option<Child>>>,
) {
    tokio::spawn(async move {
        let mut reader = FrameReader::new(stdout, limits, shared.server.clone());
        loop {
            match reader.read_frame().await {
                Ok(Some(body)) => {
                    if let Err(err) = dispatch_body(&body, &shared, &stdin).await {
                        // 帧边界或 JSON 结构出问题：不能再信任这条连接，否则后续请求
                        // 匹配不到 id，调用方只能干等到超时，错误还指错方向。
                        shared.close_with(err);
                        return;
                    }
                }
                Ok(None) => {
                    let reason = exit_reason(&child).await;
                    shared.close_with(LspError::disconnected(&shared.server, reason));
                    return;
                }
                Err(err) => {
                    shared.close_with(match err {
                        LspError::Protocol(reason) => LspError::Protocol(format!(
                            "语言服务器 `{}` 发出非法帧：{reason}",
                            shared.server
                        )),
                        other => other,
                    });
                    return;
                }
            }
        }
    });
}

/// 处理一帧消息体；返回 `Err` 表示帧级错误（必须断开）。
async fn dispatch_body(
    body: &[u8],
    shared: &Arc<Shared>,
    stdin: &Arc<Mutex<Option<ChildStdin>>>,
) -> Result<(), LspError> {
    let text = std::str::from_utf8(body)
        .map_err(|err| LspError::protocol(format!("消息体不是合法 UTF-8：{err}")))?;
    match decode_text(text)? {
        Incoming::Response(response) => {
            let Some(id) = response.id.clone() else {
                // 解析错误响应（id=null）：没有票可配，只能记录。
                tracing::warn!(
                    target: "lsp",
                    server = %shared.server,
                    "收到无 id 的响应帧，已丢弃"
                );
                return Ok(());
            };
            match shared.lock_pending().remove(&id) {
                Some(sender) => {
                    let _ = sender.send(Ok(response));
                }
                None => tracing::debug!(
                    target: "lsp",
                    server = %shared.server,
                    id = %id,
                    "响应无等待者（可能已超时），丢弃"
                ),
            }
            Ok(())
        }
        Incoming::Notification { method, params } => {
            // 本层不解释语义，交给订阅者（诊断 / 日志 / 进度）。
            tracing::debug!(
                target: "lsp",
                server = %shared.server,
                method = %method,
                "收到服务器通知"
            );
            // 没有订阅者时 send 会报错，属正常情况（例如客户端还没接 UI）。
            let _ = shared
                .notifications
                .send(ServerNotification { method, params });
            Ok(())
        }
        Incoming::Request(request) => {
            let response = server_request_response(&request);
            let body = encode_response(&response)?;
            write_body(stdin, shared, &body, "服务器反向请求的响应").await?;
            Ok(())
        }
    }
}

/// 对服务器反向请求给出响应。
fn server_request_response(request: &JsonRpcRequest) -> JsonRpcResponse {
    let id = Some(request.id.clone());
    match request.method.as_str() {
        // 能力登记：客户端只需确认收到，回 null 即可。
        "client/registerCapability" | "client/unregisterCapability" => {
            JsonRpcResponse::success(request.id.clone(), Value::Null)
        }
        "window/workDoneProgress/create" => {
            JsonRpcResponse::success(request.id.clone(), Value::Null)
        }
        // 配置项：按请求的 item 数回等长 null 数组（本层还没有设置存储）。
        "workspace/configuration" => {
            let count = request
                .params
                .as_ref()
                .and_then(|params| params.get("items"))
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            JsonRpcResponse::success(request.id.clone(), json!(vec![Value::Null; count]))
        }
        other => {
            JsonRpcResponse::failure(id, METHOD_NOT_FOUND, format!("客户端未实现该请求：{other}"))
        }
    }
}

async fn exit_reason(child: &Arc<Mutex<Option<Child>>>) -> String {
    let mut guard = child.lock().await;
    match guard.as_mut() {
        Some(child) => match tokio::time::timeout(EXIT_STATUS_GRACE, child.wait()).await {
            Ok(Ok(status)) => format!("服务器进程已退出（{status}）"),
            Ok(Err(err)) => format!("服务器进程状态未知（{err}）"),
            Err(_) => "stdout 已关闭，进程仍未退出".to_string(),
        },
        None => "stdout 已关闭".to_string(),
    }
}

fn spawn_stderr_drain(server: String, stderr: tokio::process::ChildStderr) {
    use tokio::io::AsyncBufReadExt;

    tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => return,
                Ok(_) => {
                    let text = line.trim_end();
                    let truncated = if text.chars().count() > STDERR_LINE_LIMIT {
                        let head: String = text.chars().take(STDERR_LINE_LIMIT).collect();
                        format!("{head}…")
                    } else {
                        text.to_string()
                    };
                    tracing::debug!(target: "lsp", server = %server, "stderr: {truncated}");
                }
                Err(_) => return,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_server_request_gets_method_not_found() {
        let request = JsonRpcRequest::new(
            crate::jsonrpc::RequestId::Number(9),
            "workspace/applyEdit",
            None,
        );
        let response = server_request_response(&request);
        let error = response.into_result().expect_err("必须回错误");
        assert!(matches!(
            error,
            LspError::Rpc {
                code: METHOD_NOT_FOUND,
                ..
            }
        ));
    }

    #[test]
    fn capability_registration_is_acknowledged() {
        let request = JsonRpcRequest::new(
            crate::jsonrpc::RequestId::String("srv-2".into()),
            "client/registerCapability",
            Some(json!({"registrations": []})),
        );
        let response = server_request_response(&request);
        assert_eq!(response.into_result().expect("ok"), Value::Null);
    }

    #[test]
    fn configuration_request_gets_one_null_per_item() {
        let request = JsonRpcRequest::new(
            crate::jsonrpc::RequestId::Number(3),
            "workspace/configuration",
            Some(json!({"items": [{"section": "rust-analyzer"}, {"section": "editor"}]})),
        );
        let response = server_request_response(&request);
        assert_eq!(
            response.into_result().expect("ok"),
            json!([Value::Null, Value::Null])
        );
    }

    #[test]
    fn unknown_request_frame_is_not_a_dispatch_error() {
        // 反向请求写不出去时才会返回 Err；这里只验证「不认识的请求」走正常回包路径。
        let request =
            JsonRpcRequest::new(crate::jsonrpc::RequestId::Number(1), "unknown/method", None);
        let response = server_request_response(&request);
        assert!(response.error.is_some());
    }
}
