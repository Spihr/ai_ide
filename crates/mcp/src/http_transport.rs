//! HTTP 传输：Streamable HTTP，响应体可能是 JSON 也可能是 SSE 流。
//!
//! 规范允许 Server 对 `POST /mcp` 两种回答：
//!
//! - `Content-Type: application/json`：整个响应体就是一个 JSON-RPC 响应；
//! - `Content-Type: text/event-stream`：响应体是 SSE，一帧一个 JSON-RPC 消息。
//!
//! 这里按 `Content-Type` 分流，而不是按配置里的 `type` 猜——用户写 `http` 还是 `sse`
//! 只影响诊断标签，真正的判据永远是响应头。
//!
//! ## 会话
//!
//! Server 可能回 `Mcp-Session-Id`，后续每个请求都要带上（不带会被当成新会话，
//! `tools/call` 直接报「未初始化」）。这里只在**收到时更新**，从不主动生成 id：
//! 伪造的 session id 只会让 Server 回 404，更难查。

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use reqwest::StatusCode;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::config::HttpServerConfig;
use crate::error::McpError;
use crate::jsonrpc::{
    decode_value, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, RequestId,
};
use crate::sse::{SseDecoder, SseFrame};
use crate::transport::{timeout_error, Transport, TransportResult};

/// 会话 id 的响应/请求头名。
pub const SESSION_HEADER: &str = "mcp-session-id";

/// 响应体摘要的最大字符数（进错误消息）。
const BODY_SNIPPET_LIMIT: usize = 512;

/// HTTP 连接。
pub struct HttpTransport {
    server: String,
    url: String,
    headers: HeaderMap,
    client: reqwest::Client,
    session: Mutex<Option<String>>,
    closed: std::sync::atomic::AtomicBool,
}

impl HttpTransport {
    /// 准备一个 HTTP 连接（此时不发起任何网络请求）。
    pub fn new(config: &HttpServerConfig) -> TransportResult<Self> {
        let mut headers = header_map_from(&config.headers).map_err(|err| match err {
            McpError::Config(message) => {
                McpError::Config(format!("Server `{}`：{message}", config.name))
            }
            other => other,
        })?;
        // 同时声明两种可接受格式，由 Server 决定用哪个。
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );

        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|err| {
                McpError::transport(&config.name, format!("HTTP 客户端初始化失败：{err}"))
            })?;

        Ok(Self {
            server: config.name.clone(),
            url: config.url.clone(),
            headers,
            client,
            session: Mutex::new(None),
            closed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// 配置里的请求头（诊断用，值已脱敏）。
    pub fn header_names(&self) -> Vec<&str> {
        self.headers.keys().map(HeaderName::as_str).collect()
    }

    async fn do_request(&self, request: &JsonRpcRequest) -> TransportResult<JsonRpcResponse> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(McpError::disconnected(&self.server, "连接已关闭"));
        }
        let mut builder = self
            .client
            .post(&self.url)
            .headers(self.headers.clone())
            .json(request);
        if let Some(session) = self.session.lock().await.clone() {
            builder = builder.header(SESSION_HEADER, session);
        }
        let response = builder.send().await.map_err(|err| self.map_reqwest(err))?;

        if let Some(session) = response
            .headers()
            .get(SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
        {
            let session = session.to_string();
            *self.session.lock().await = Some(session);
        }

        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(self.http_error(status, &detail));
        }

        let is_sse = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.contains("text/event-stream"))
            .unwrap_or(false);
        if is_sse {
            return self.read_sse(response, &request.id).await;
        }

        let text = response
            .text()
            .await
            .map_err(|err| McpError::transport(&self.server, format!("读取响应体失败：{err}")))?;
        let value: Value = serde_json::from_str(&text).map_err(|err| {
            McpError::Protocol(format!(
                "响应体不是合法 JSON：{err}；内容摘要：{}",
                snippet(&text)
            ))
        })?;
        match decode_value(value)? {
            crate::jsonrpc::Incoming::Response(response) => Ok(response),
            other => Err(McpError::Protocol(format!(
                "期望 JSON-RPC 响应，收到 {other:?}"
            ))),
        }
    }

    async fn read_sse(
        &self,
        response: reqwest::Response,
        id: &RequestId,
    ) -> TransportResult<JsonRpcResponse> {
        let mut stream = response.bytes_stream();
        let mut decoder = SseDecoder::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(|err| McpError::transport(&self.server, format!("SSE 流中断：{err}")))?;
            for frame in decoder.push_bytes(&chunk) {
                if let Some(response) = match_frame(&frame, id)? {
                    return Ok(response);
                }
            }
        }
        if let Some(frame) = decoder.finish() {
            if let Some(response) = match_frame(&frame, id)? {
                return Ok(response);
            }
        }
        Err(McpError::Protocol(format!(
            "SSE 流已结束，但没有收到 id {id} 的响应"
        )))
    }

    async fn do_notify(&self, notification: &JsonRpcNotification) -> TransportResult<()> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(McpError::disconnected(&self.server, "连接已关闭"));
        }
        let mut builder = self
            .client
            .post(&self.url)
            .headers(self.headers.clone())
            .json(notification);
        if let Some(session) = self.session.lock().await.clone() {
            builder = builder.header(SESSION_HEADER, session);
        }
        let response = builder.send().await.map_err(|err| self.map_reqwest(err))?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(self.http_error(status, &detail));
        }
        // 通知的响应体没有语义，丢弃即可（部分 Server 会回 202 + 空体）。
        Ok(())
    }

    fn http_error(&self, status: StatusCode, detail: &str) -> McpError {
        McpError::Http {
            server: self.server.clone(),
            status: status.as_u16(),
            detail: snippet(detail),
        }
    }

    fn map_reqwest(&self, err: reqwest::Error) -> McpError {
        if err.is_timeout() {
            // 连接层的超时同样归到 Timeout（可重连），而不是 Transport。
            return McpError::Timeout {
                method: "http".into(),
                timeout_ms: 0,
            };
        }
        McpError::transport(&self.server, err)
    }
}

#[async_trait]
impl Transport for HttpTransport {
    fn label(&self) -> &'static str {
        "http"
    }

    async fn request(
        &self,
        request: &JsonRpcRequest,
        timeout: Duration,
    ) -> TransportResult<JsonRpcResponse> {
        match tokio::time::timeout(timeout, self.do_request(request)).await {
            Ok(result) => result,
            Err(_) => Err(timeout_error(&request.method, timeout)),
        }
    }

    async fn notify(&self, notification: &JsonRpcNotification) -> TransportResult<()> {
        // 通知也要有预算，否则 Server 不响应时会一直挂着。
        match tokio::time::timeout(Duration::from_secs(10), self.do_notify(notification)).await {
            Ok(result) => result,
            Err(_) => Err(timeout_error(&notification.method, Duration::from_secs(10))),
        }
    }

    async fn shutdown(&self) -> TransportResult<()> {
        if self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Ok(());
        }
        let session = self.session.lock().await.clone();
        if let Some(session) = session {
            // 规范允许用 DELETE 显式结束会话；失败不影响本地关闭。
            let outcome = self
                .client
                .delete(&self.url)
                .headers(self.headers.clone())
                .header(SESSION_HEADER, session)
                .send()
                .await;
            match outcome {
                Ok(response) if !response.status().is_success() => tracing::debug!(
                    target: "mcp",
                    server = %self.server,
                    status = %response.status(),
                    "DELETE 会话未被接受（忽略）"
                ),
                Ok(_) => {}
                Err(err) => tracing::debug!(
                    target: "mcp",
                    server = %self.server,
                    error = %err,
                    "DELETE 会话请求失败（忽略）"
                ),
            }
        }
        Ok(())
    }

    async fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl std::fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpTransport")
            .field("server", &self.server)
            .field("url", &self.url)
            .field("header_names", &self.header_names())
            .finish_non_exhaustive()
    }
}

/// 在一帧 SSE 里找目标响应。
fn match_frame(frame: &SseFrame, id: &RequestId) -> TransportResult<Option<JsonRpcResponse>> {
    let data = frame.data.trim();
    if data.is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(data)
        .map_err(|err| McpError::Protocol(format!("SSE 帧不是合法 JSON：{err}")))?;
    match decode_value(value)? {
        crate::jsonrpc::Incoming::Response(response) => {
            if response.id.as_ref() == Some(id) {
                Ok(Some(response))
            } else {
                tracing::debug!(
                    target: "mcp",
                    response_id = ?response.id,
                    "SSE 帧的 id 与当前请求不匹配，忽略"
                );
                Ok(None)
            }
        }
        crate::jsonrpc::Incoming::Notification { method, .. } => {
            tracing::debug!(target: "mcp", method = %method, "SSE 通知帧，忽略");
            Ok(None)
        }
        crate::jsonrpc::Incoming::Request(request) => {
            tracing::warn!(
                target: "mcp",
                method = %request.method,
                "SSE 反向请求暂不支持，忽略"
            );
            Ok(None)
        }
    }
}

/// 截断成一行摘要，避免把整个 HTML 错误页塞进日志。
fn snippet(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() > BODY_SNIPPET_LIMIT {
        let head: String = compact.chars().take(BODY_SNIPPET_LIMIT).collect();
        format!("{head}…")
    } else {
        compact
    }
}

/// 把 `BTreeMap` 形式的头转成 `HeaderMap`（供上层/测试复用）。
pub fn header_map_from(pairs: &BTreeMap<String, String>) -> TransportResult<HeaderMap> {
    let mut headers = HeaderMap::new();
    for (key, value) in pairs {
        let name = HeaderName::from_bytes(key.as_bytes())
            .map_err(|err| McpError::Config(format!("请求头 `{key}` 非法：{err}")))?;
        let value = HeaderValue::from_str(value)
            .map_err(|_| McpError::Config(format!("请求头 `{key}` 的值含非法字符")))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorClass;
    use serde_json::json;

    fn config(name: &str, url: &str) -> HttpServerConfig {
        HttpServerConfig {
            name: name.to_string(),
            url: url.to_string(),
            headers: BTreeMap::new(),
            declared_sse: false,
        }
    }

    #[test]
    fn accepts_json_and_sse_in_accept_header() {
        let transport = HttpTransport::new(&config("gh", "https://example.com/mcp")).expect("new");
        let accept = transport
            .headers
            .get(ACCEPT)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(accept.contains("application/json"), "{accept}");
        assert!(accept.contains("text/event-stream"), "{accept}");
    }

    #[test]
    fn invalid_header_name_is_a_config_error() {
        let mut cfg = config("gh", "https://example.com/mcp");
        cfg.headers.insert("bad header".into(), "x".into());
        let err = HttpTransport::new(&cfg).expect_err("must fail");
        assert_eq!(err.class(), ErrorClass::Config);
    }

    #[test]
    fn header_value_with_newline_is_a_config_error() {
        let mut cfg = config("gh", "https://example.com/mcp");
        cfg.headers
            .insert("Authorization".into(), "Bearer a\nb".into());
        let err = HttpTransport::new(&cfg).expect_err("must fail");
        assert_eq!(err.class(), ErrorClass::Config);
    }

    #[test]
    fn debug_output_redacts_header_values() {
        let mut cfg = config("gh", "https://example.com/mcp");
        cfg.headers
            .insert("Authorization".into(), "Bearer s3cret".into());
        let transport = HttpTransport::new(&cfg).expect("new");
        let text = format!("{transport:?}");
        assert!(!text.contains("s3cret"), "{text}");
        assert!(text.contains("authorization"), "{text}");
    }

    #[test]
    fn match_frame_returns_only_matching_id() {
        let frame = SseFrame {
            event: Some("message".into()),
            data: json!({"jsonrpc": "2.0", "id": 2, "result": {"ok": true}}).to_string(),
        };
        assert!(match_frame(&frame, &RequestId::Number(1))
            .expect("no error")
            .is_none());
        let response = match_frame(&frame, &RequestId::Number(2))
            .expect("no error")
            .expect("matched");
        assert_eq!(response.result, Some(json!({"ok": true})));
    }

    #[test]
    fn match_frame_skips_notifications_and_empty_frames() {
        let empty = SseFrame {
            event: None,
            data: "   ".into(),
        };
        assert!(match_frame(&empty, &RequestId::Number(1))
            .expect("ok")
            .is_none());
        let notification = SseFrame {
            event: Some("message".into()),
            data: json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
                .to_string(),
        };
        assert!(match_frame(&notification, &RequestId::Number(1))
            .expect("ok")
            .is_none());
    }

    #[test]
    fn match_frame_rejects_malformed_payload() {
        let frame = SseFrame {
            event: Some("message".into()),
            data: "{oops".into(),
        };
        let err = match_frame(&frame, &RequestId::Number(1)).expect_err("must fail");
        assert_eq!(err.class(), ErrorClass::Protocol);
    }

    #[test]
    fn snippet_collapses_whitespace_and_truncates() {
        let text = "a\nb\tc".repeat(200);
        let out = snippet(&text);
        assert!(!out.contains('\n'));
        assert!(out.chars().count() <= BODY_SNIPPET_LIMIT + 1);
    }

    #[test]
    fn header_map_from_reports_bad_pairs() {
        let mut pairs = BTreeMap::new();
        pairs.insert("X-Test".to_string(), "ok".to_string());
        assert!(header_map_from(&pairs).is_ok());
        pairs.insert("X-Bad".to_string(), "a\rb".to_string());
        assert!(header_map_from(&pairs).is_err());
    }
}
