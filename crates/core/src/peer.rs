//! 与主进程通信的对端句柄。
//!
//! 负责把帧**原子地**写入 stdout：
//! - 所有写入经由同一个异步锁串行化，杜绝多任务并发写导致的帧交错；
//! - 每次写入后立即 flush，保证 `core.shutdown` 之前的数据已经离开进程缓冲区。

use ai_ide_protocol::{encode, ErrorObject, Notification, Request, Response, JSONRPC_VERSION};
use serde::Serialize;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::io::{AsyncWriteExt, BufWriter, Stdout};
use tokio::sync::Mutex;
use tracing::error;

/// stdout 通道句柄。
pub struct Peer {
    out: Mutex<BufWriter<Stdout>>,
    started_at: Instant,
    requests_handled: AtomicU64,
    tasks_active: AtomicU64,
}

impl Default for Peer {
    fn default() -> Self {
        Self::new()
    }
}

impl Peer {
    pub fn new() -> Self {
        Self {
            out: Mutex::new(BufWriter::new(tokio::io::stdout())),
            started_at: Instant::now(),
            requests_handled: AtomicU64::new(0),
            tasks_active: AtomicU64::new(0),
        }
    }

    /// 内核已运行时长（毫秒）。
    pub fn uptime_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    /// 已处理的请求总数。
    pub fn requests_handled(&self) -> u64 {
        self.requests_handled.load(Ordering::Relaxed)
    }

    /// 当前在途任务数。
    pub fn tasks_active(&self) -> u64 {
        self.tasks_active.load(Ordering::Relaxed)
    }

    /// 标记一个请求开始处理。
    pub fn begin_request(&self) {
        self.tasks_active.fetch_add(1, Ordering::Relaxed);
    }

    /// 标记一个请求处理结束。
    pub fn end_request(&self) {
        self.requests_handled.fetch_add(1, Ordering::Relaxed);
        self.tasks_active.fetch_sub(1, Ordering::Relaxed);
    }

    /// 写出一行已经拼好的 NDJSON。
    async fn write_line(&self, line: &str) {
        let mut out = self.out.lock().await;
        if let Err(err) = out.write_all(line.as_bytes()).await {
            error!(error = %err, "写入 stdout 失败");
            return;
        }
        if let Err(err) = out.flush().await {
            error!(error = %err, "flush stdout 失败");
        }
    }

    /// 写出任意可序列化的值（自动补换行）。
    pub async fn send<T: Serialize>(&self, value: &T) {
        match encode(value) {
            Ok(line) => self.write_line(&line).await,
            Err(err) => error!(error = %err, "帧序列化失败"),
        }
    }

    /// 回复成功结果。
    pub async fn respond<T: Serialize>(&self, id: &str, result: &T) {
        let result = match serde_json::to_value(result) {
            Ok(value) => value,
            Err(err) => {
                error!(error = %err, "结果序列化失败");
                self.respond_error(id, &ErrorObject::internal(format!("结果序列化失败: {err}")))
                    .await;
                return;
            }
        };
        self.send(&Response::new(id, result)).await;
    }

    /// 回复错误。
    pub async fn respond_error(&self, id: &str, error: &ErrorObject) {
        let frame = serde_json::json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "error": error,
        });
        self.send(&frame).await;
    }

    /// 回复「无法确定 id」的错误（解析阶段就失败的报文）。
    ///
    /// JSON-RPC 2.0 规定此时 `id` 必须为 `null`。
    pub async fn respond_anonymous_error(&self, error: &ErrorObject) {
        let frame = serde_json::json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": Value::Null,
            "error": error,
        });
        self.send(&frame).await;
    }

    /// 主动推送通知（事件）。
    pub async fn notify<T: Serialize>(&self, method: &str, params: &T) {
        let params = match serde_json::to_value(params) {
            Ok(value) => Some(value),
            Err(err) => {
                error!(error = %err, method = method, "通知载荷序列化失败");
                return;
            }
        };
        self.send(&Notification::new(method, params)).await;
    }

    /// 转发一条日志到主进程（便于主进程统一落盘）。
    pub async fn forward_log(&self, level: &str, target: &str, message: &str) {
        let frame = Notification::new(
            ai_ide_protocol::methods::event::LOG,
            Some(serde_json::json!({
                "level": level,
                "target": target,
                "message": message,
            })),
        );
        self.send(&frame).await;
    }

    /// 便捷构造：请求帧（供测试与主进程方向使用）。
    pub fn request(
        id: impl Into<String>,
        method: impl Into<String>,
        params: Option<Value>,
    ) -> Request {
        Request::new(id, method, params)
    }
}
