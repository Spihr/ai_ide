//! stdio 传输：子进程 + 逐行 JSON 帧。
//!
//! ## 为什么要自己管 id → 等待者
//!
//! stdio 是**共享一条流**的：三个并发 `tools/call` 的响应会按 Server 自己的顺序
//! 回来，可能乱序。所以必须建 `id → oneshot` 票表，由读循环按 id 派发；
//! 请求超时时还要把票撕掉（见 [`StdioTransport::request`]），否则迟到的响应
//! 会污染后续请求。
//!
//! ## 为什么读循环用 `read_line` 而不是 select+tick
//!
//! `read_line` 被 timeout/select 中途取消时可能已往缓冲里塞了半行，重试会读到
//! 断裂的 JSON。这里改为「只在 EOF / 显式关闭时结束循环」，靠关 stdin 与 kill
//! 子进程终止，从根上避免取消语义。
//!
//! ## 日志
//!
//! stderr 与协议无关，但调试 Server 崩溃时最有用。这里统一降级为 `debug`，
//! 并且**按行截断**——Server 可能往 stderr 打二进制或超长 dump。

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};

use crate::config::StdioServerConfig;
use crate::error::McpError;
use crate::jsonrpc::{decode_line, encode_notification, encode_request, Incoming};
use crate::transport::{timeout_error, Transport, TransportResult};

/// 关闭时等待子进程退出的上限。
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// 读到 EOF 后等待退出码的上限（为了把退出码写进错误原因）。
const EXIT_STATUS_GRACE: Duration = Duration::from_secs(1);

/// stderr 单行日志的截断长度。
const STDERR_LINE_LIMIT: usize = 512;

type Pending = HashMap<
    crate::jsonrpc::RequestId,
    oneshot::Sender<TransportResult<crate::jsonrpc::JsonRpcResponse>>,
>;

/// 读循环与请求侧共享的状态。
struct Shared {
    server: String,
    pending: StdMutex<Pending>,
    closed: AtomicBool,
}

impl Shared {
    fn lock_pending(&self) -> MutexGuard<'_, Pending> {
        // 读循环 panic 会毒化锁；为了还能把「连接已断」传给所有等待者，
        // 这里取回内部数据继续用（数据本身就是一张票表，没有不变量可破坏）。
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn fail_all(&self, error: McpError) {
        let drained: Vec<_> = self
            .lock_pending()
            .drain()
            .map(|(_, sender)| sender)
            .collect();
        for sender in drained {
            let _ = sender.send(Err(error.clone()));
        }
    }
}

/// stdio 连接。
pub struct StdioTransport {
    server: String,
    stdin: Mutex<Option<ChildStdin>>,
    child: Arc<Mutex<Option<Child>>>,
    shared: Arc<Shared>,
}

impl StdioTransport {
    /// 启动子进程并接管它的 stdin/stdout/stderr。
    pub async fn spawn(config: &StdioServerConfig) -> TransportResult<Self> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // 客户端被 drop 时不能留下孤儿进程。
            .kill_on_drop(true);
        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in &config.env {
            command.env(key, value);
        }

        let mut child = command.spawn().map_err(|err| McpError::Spawn {
            server: config.name.clone(),
            // 只带命令名，不带 env（可能含凭据）。
            reason: format!("无法执行 `{}`：{err}", config.command),
        })?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| McpError::Spawn {
            server: config.name.clone(),
            reason: "子进程 stdout 未就绪".into(),
        })?;
        let stderr = child.stderr.take();

        let shared = Arc::new(Shared {
            server: config.name.clone(),
            pending: StdMutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        });
        let child = Arc::new(Mutex::new(Some(child)));

        if let Some(stderr) = stderr {
            spawn_stderr_drain(config.name.clone(), stderr);
        }
        spawn_reader(stdout, Arc::clone(&shared), Arc::clone(&child));

        Ok(Self {
            server: config.name.clone(),
            stdin: Mutex::new(stdin),
            child,
            shared,
        })
    }

    /// 把一行写入子进程 stdin。
    async fn write_line(&self, line: &str, hint: &str) -> TransportResult<()> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(McpError::disconnected(&self.server, "连接已关闭"));
        }
        let mut guard = self.stdin.lock().await;
        let Some(stdin) = guard.as_mut() else {
            return Err(McpError::disconnected(&self.server, "stdin 已关闭"));
        };
        let outcome = async {
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await
        }
        .await;
        if let Err(err) = outcome {
            // 写不进去说明管道断了：立刻标记不可用，让后续请求快速失败而不是排队等超时。
            self.shared.closed.store(true, Ordering::SeqCst);
            let error = McpError::transport(&self.server, format!("写入 {hint} 失败：{err}"));
            self.shared.fail_all(error.clone());
            return Err(error);
        }
        Ok(())
    }
}

#[async_trait]
impl Transport for StdioTransport {
    fn label(&self) -> &'static str {
        "stdio"
    }

    async fn request(
        &self,
        request: &crate::jsonrpc::JsonRpcRequest,
        timeout: Duration,
    ) -> TransportResult<crate::jsonrpc::JsonRpcResponse> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(McpError::disconnected(&self.server, "连接已关闭"));
        }
        let line = encode_request(request)?;
        let (sender, receiver) = oneshot::channel();
        self.shared
            .lock_pending()
            .insert(request.id.clone(), sender);

        if let Err(err) = self.write_line(&line, &request.method).await {
            self.shared.lock_pending().remove(&request.id);
            return Err(err);
        }

        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(Ok(response))) => Ok(response),
            Ok(Ok(Err(err))) => Err(err),
            Ok(Err(_)) => Err(McpError::disconnected(
                &self.server,
                format!("{} 的响应通道已销毁", request.method),
            )),
            Err(_) => {
                // 撕票：迟到的响应会在读循环里因找不到 id 被丢弃，不会错配给下一个请求。
                self.shared.lock_pending().remove(&request.id);
                Err(timeout_error(&request.method, timeout))
            }
        }
    }

    async fn notify(
        &self,
        notification: &crate::jsonrpc::JsonRpcNotification,
    ) -> TransportResult<()> {
        let line = encode_notification(notification)?;
        self.write_line(&line, &notification.method).await
    }

    async fn shutdown(&self) -> TransportResult<()> {
        let already_closed = self.shared.closed.swap(true, Ordering::SeqCst);
        // 关掉写端：正常退出的 Server 见到 EOF 会自行收尾。
        drop(self.stdin.lock().await.take());
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await;
        }
        if !already_closed {
            self.shared
                .fail_all(McpError::disconnected(&self.server, "连接已被客户端关闭"));
        }
        Ok(())
    }

    async fn is_closed(&self) -> bool {
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
    shared: Arc<Shared>,
    child: Arc<Mutex<Option<Child>>>,
) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    let reason = exit_reason(&child).await;
                    shared.closed.store(true, Ordering::SeqCst);
                    shared.fail_all(McpError::disconnected(&shared.server, reason));
                    return;
                }
                Ok(_) => {
                    let trimmed = line.trim_end_matches(['\r', '\n']);
                    match decode_line(trimmed) {
                        Ok(Incoming::Response(response)) => {
                            let Some(id) = response.id.clone() else {
                                // 解析错误响应（id=null）：没有票可配，只能记录。
                                tracing::warn!(
                                    target: "mcp",
                                    server = %shared.server,
                                    "收到无 id 的响应帧，已丢弃"
                                );
                                continue;
                            };
                            let sender = shared.lock_pending().remove(&id);
                            match sender {
                                Some(sender) => {
                                    let _ = sender.send(Ok(response));
                                }
                                None => tracing::debug!(
                                    target: "mcp",
                                    server = %shared.server,
                                    id = %id,
                                    "响应无等待者（可能已超时），丢弃"
                                ),
                            }
                        }
                        Ok(Incoming::Notification { method, .. }) => {
                            // 工具列表变更等通知：本层不解释，交给上层订阅者。
                            tracing::debug!(
                                target: "mcp",
                                server = %shared.server,
                                method = %method,
                                "收到 Server 通知"
                            );
                        }
                        Ok(Incoming::Request(request)) => {
                            tracing::warn!(
                                target: "mcp",
                                server = %shared.server,
                                method = %request.method,
                                "Server 反向请求暂不支持，已忽略"
                            );
                        }
                        Err(err) => {
                            // 分两种「坏帧」：
                            // - 根本不是 JSON：Server 往 stdout 打了日志之类，丢弃继续跑；
                            // - 是 JSON 但不符合 JSON-RPC：协议违约，不能再信任这条连接，
                            //   否则请求匹配不到 id，调用方只能干等到超时，错误还指错方向。
                            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                                shared.closed.store(true, Ordering::SeqCst);
                                shared.fail_all(McpError::Protocol(format!(
                                    "Server `{}` 发出非法 JSON-RPC 帧：{err}",
                                    shared.server
                                )));
                                return;
                            }
                            tracing::warn!(
                                target: "mcp",
                                server = %shared.server,
                                error = %err,
                                "无法解析的帧（非 JSON），已丢弃"
                            );
                        }
                    }
                }
                Err(err) => {
                    shared.closed.store(true, Ordering::SeqCst);
                    shared.fail_all(McpError::transport(
                        &shared.server,
                        format!("读取 stdout 失败：{err}"),
                    ));
                    return;
                }
            }
        }
    });
}

async fn exit_reason(child: &Arc<Mutex<Option<Child>>>) -> String {
    let mut guard = child.lock().await;
    match guard.as_mut() {
        Some(child) => match tokio::time::timeout(EXIT_STATUS_GRACE, child.wait()).await {
            Ok(Ok(status)) => format!("Server 进程已退出（{status}）"),
            Ok(Err(err)) => format!("Server 进程状态未知（{err}）"),
            Err(_) => "stdout 已关闭，进程仍未退出".to_string(),
        },
        None => "stdout 已关闭".to_string(),
    }
}

fn spawn_stderr_drain(server: String, stderr: tokio::process::ChildStderr) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
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
                    tracing::debug!(target: "mcp", server = %server, "stderr: {truncated}");
                }
                Err(_) => return,
            }
        }
    });
}
