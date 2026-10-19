//! HTTP 传输抽象。
//!
//! 上游适配器只依赖 [`HttpTransport`] 这个 trait，真实实现是 [`ReqwestTransport`]。
//! 之所以抽一层而不是直接调 reqwest，是为了让 M2 的核心行为（SSE 分帧、增量拼接、
//! 取消传播、错误映射）能在**没有网络、没有 API Key** 的情况下被单测完整覆盖——
//! 这正是「不依赖外部凭据的骨架」能立住的关键。

use std::pin::Pin;
use std::time::Duration;

use futures_util::{Stream, StreamExt};

use crate::error::LlmError;

/// 逐块字节流。
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, LlmError>> + Send>>;

/// 请求方法（M2 只需要这两种）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
}

/// 与具体 HTTP 库解耦的请求描述。
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    /// 单次请求超时（毫秒）。
    pub timeout_ms: u64,
    /// 是否以流式读取响应体（`llm.chat_stream` 为 true）。
    pub stream: bool,
}

impl HttpRequest {
    pub fn get(url: impl Into<String>, timeout_ms: u64) -> Self {
        Self {
            method: HttpMethod::Get,
            url: url.into(),
            headers: Vec::new(),
            body: None,
            timeout_ms,
            stream: false,
        }
    }

    pub fn post_json(url: impl Into<String>, body: impl Into<String>, timeout_ms: u64) -> Self {
        let mut request = Self::post(url, timeout_ms);
        request.body = Some(body.into());
        request
            .headers
            .push(("content-type".to_string(), "application/json".to_string()));
        request
    }

    pub fn post(url: impl Into<String>, timeout_ms: u64) -> Self {
        Self {
            method: HttpMethod::Post,
            url: url.into(),
            headers: Vec::new(),
            body: None,
            timeout_ms,
            stream: false,
        }
    }

    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    #[must_use]
    pub fn streaming(mut self) -> Self {
        self.stream = true;
        self
    }
}

/// 响应体形态。
pub enum ResponseBody {
    /// 非流式：整段文本（已按 UTF-8 增量解码）。
    Text(String),
    /// 流式：逐块字节（**未解码**，交给 [`Utf8Decoder`]）。
    Chunks(ByteStream),
    /// 无响应体（例如 204）。
    Empty,
}

/// 与具体 HTTP 库解耦的响应。
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: ResponseBody,
}

impl HttpResponse {
    /// 便捷构造：非流式响应（主要服务于测试替身）。
    pub fn text(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: ResponseBody::Text(body.into()),
        }
    }

    /// 取指定响应头（大小写不敏感）。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// 把非流式响应体取成字符串（流式响应会返回 `None`）。
    pub fn text_body(&self) -> Option<&str> {
        match &self.body {
            ResponseBody::Text(text) => Some(text),
            ResponseBody::Chunks(_) | ResponseBody::Empty => None,
        }
    }
}

/// HTTP 传输能力。
#[async_trait::async_trait]
pub trait HttpTransport: Send + Sync {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, LlmError>;
}

/// 基于 `reqwest` 的真实实现。这是 M2 中唯一需要真实网络才能跑到的代码路径。
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// 构建带连接池的客户端。
    pub fn new() -> Result<Self, LlmError> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|err| LlmError::Transport(format!("HTTP 客户端初始化失败: {err}")))?;
        Ok(Self { client })
    }

    /// 复用外部客户端（便于统一代理/证书配置）。
    pub fn from_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait::async_trait]
impl HttpTransport for ReqwestTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, LlmError> {
        let method = match request.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
        };
        let mut builder = self
            .client
            .request(method, &request.url)
            .timeout(Duration::from_millis(request.timeout_ms.max(1)));
        for (name, value) in &request.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }

        let response = builder
            .send()
            .await
            .map_err(|err| LlmError::Transport(describe_reqwest_error(&err)))?;

        let status = response.status().as_u16();
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();

        let body = if request.stream {
            let stream = response.bytes_stream().map(|item| {
                item.map(|bytes| bytes.to_vec())
                    .map_err(|err| LlmError::Transport(describe_reqwest_error(&err)))
            });
            ResponseBody::Chunks(Box::pin(stream))
        } else {
            let text = response
                .text()
                .await
                .map_err(|err| LlmError::Transport(describe_reqwest_error(&err)))?;
            ResponseBody::Text(text)
        };

        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

/// 把 reqwest 的错误压成可读、可分类的一句话（超时 vs 连接失败对用户意味着不同动作）。
fn describe_reqwest_error(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        format!("请求超时: {err}")
    } else if err.is_connect() {
        format!("连接失败（检查网络或 base_url）: {err}")
    } else if err.is_decode() {
        format!("响应解码失败: {err}")
    } else {
        err.to_string()
    }
}

/// 增量 UTF-8 解码器。
///
/// 上游的 SSE 分片是按字节切的，一个汉字很可能被切成两个 chunk。
/// 直接对每个 chunk 做 `from_utf8` 会产生乱码或直接报错，所以必须把
/// 「尾部不完整的多字节序列」留到下一个 chunk 一起解。
#[derive(Debug, Default)]
pub struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一块字节，返回本次能确定解出的文本（可能为空串）。
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    out.push_str(text);
                    self.pending.clear();
                    break;
                }
                Err(err) => {
                    let valid = err.valid_up_to();
                    if valid > 0 {
                        // 前缀已由 from_utf8 校验为合法 UTF-8，unwrap 不会触发。
                        out.push_str(
                            std::str::from_utf8(&self.pending[..valid])
                                .expect("valid_up_to 之前必定是合法 UTF-8"),
                        );
                    }
                    match err.error_len() {
                        // 真的出现了非法字节：丢弃这一段，继续尝试后续内容，
                        // 避免一个坏字节把整条流卡死。
                        Some(len) => {
                            self.pending.drain(..valid + len);
                            if self.pending.is_empty() {
                                break;
                            }
                        }
                        // 尾部是半个多字节字符：留着等下一个 chunk。
                        None => {
                            self.pending.drain(..valid);
                            break;
                        }
                    }
                }
            }
        }
        out
    }

    /// 是否还有未解出的残留字节（流结束时可用于判断「上游把字符切断了」）。
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_passes_through_ascii() {
        let mut decoder = Utf8Decoder::new();
        assert_eq!(decoder.push(b"data: hi\n"), "data: hi\n");
        assert!(!decoder.has_pending());
    }

    #[test]
    fn decoder_joins_split_multibyte_char() {
        let mut decoder = Utf8Decoder::new();
        let bytes = "你好".as_bytes();
        // 在第一个汉字中间切开
        let first = decoder.push(&bytes[..2]);
        assert_eq!(first, "", "半个汉字不能先吐出来");
        assert!(decoder.has_pending());
        let second = decoder.push(&bytes[2..]);
        assert_eq!(second, "你好");
        assert!(!decoder.has_pending());
    }

    #[test]
    fn decoder_drops_invalid_bytes_and_keeps_going() {
        let mut decoder = Utf8Decoder::new();
        let mut payload = b"ok".to_vec();
        payload.push(0xFF); // 非法字节
        payload.extend_from_slice(b"done");
        let out = decoder.push(&payload);
        assert!(out.contains("ok"));
        assert!(out.contains("done"));
    }

    #[test]
    fn request_builders_set_expected_shape() {
        let request =
            HttpRequest::post_json("https://example.com/v1/chat/completions", "{}", 30_000)
                .header("authorization", "Bearer sk-test")
                .streaming();
        assert_eq!(request.method, HttpMethod::Post);
        assert!(request.stream);
        assert!(request
            .headers
            .iter()
            .any(|(name, _)| name == "content-type"));
        assert!(request
            .headers
            .iter()
            .any(|(name, _)| name == "authorization"));
    }

    #[test]
    fn response_header_lookup_is_case_insensitive() {
        let mut response = HttpResponse::text(200, "{}");
        response
            .headers
            .push(("Retry-After".to_string(), "2".to_string()));
        assert_eq!(response.header("retry-after"), Some("2"));
        assert_eq!(response.text_body(), Some("{}"));
    }
}
