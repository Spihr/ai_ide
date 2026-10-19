//! OpenAI Chat Completions 适配器（M2 唯一落地的协议适配器）。
//!
//! 选它的理由（`docs/04` §1 已注明）：Chat Completions 是事实上的通用方言，
//! DeepSeek / Kimi / 智谱 / OpenRouter / vLLM / 各类网关都兼容同一套
//! `/chat/completions` + SSE 分片格式，一个适配器就能覆盖绝大多数可用上游。
//!
//! 本模块**不直接碰网络**：一切都通过 [`HttpTransport`]，因此可以用测试替身
//! 在没有 API Key 的情况下跑通「分片拼接、取消、错误分类、用量上报」全链路。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use ai_ide_protocol::dto::{
    ChatMessage, ChatRole, FinishReason, ModelInfo, ModelRef, ToolChoice, ToolSpec,
};
use serde_json::{json, Map, Value};

use crate::error::LlmError;
use crate::provider::{CancelToken, Provider, ProviderSpec};
use crate::secret::SecretString;
use crate::sse::{translate_openai_chunk, SseDecoder, TranslatedChunk};
use crate::transport::{HttpRequest, HttpTransport, ResponseBody, Utf8Decoder};
use crate::types::{emit, ChatRequest, EventSink, HealthReport, StreamEvent, Usage};

/// Chat Completions 适配器。
pub struct OpenAiProvider {
    spec: ProviderSpec,
    api_key: Option<SecretString>,
    transport: Arc<dyn HttpTransport>,
}

impl OpenAiProvider {
    pub fn new(
        spec: ProviderSpec,
        api_key: Option<SecretString>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            spec,
            api_key,
            transport,
        }
    }

    /// 拼出完整接口地址。
    fn endpoint(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.spec.normalized_base_url(),
            path.trim_start_matches('/')
        )
    }

    /// 是否由用户自定义 Header 指定了鉴权（网关常用非标准头）。
    fn custom_authorization(&self) -> Option<&str> {
        self.spec
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.as_str())
    }

    /// 加上通用请求头与鉴权。
    fn authorize(&self, request: HttpRequest) -> Result<HttpRequest, LlmError> {
        let mut request = request.header("accept", "application/json").header(
            "user-agent",
            format!("ai-ide/{}", env!("CARGO_PKG_VERSION")),
        );
        for (name, value) in &self.spec.headers {
            request = request.header(name.clone(), value.clone());
        }
        match self.custom_authorization() {
            Some(_) => Ok(request),
            None => {
                let key = self
                    .api_key
                    .as_ref()
                    .ok_or_else(|| LlmError::ProviderNotConfigured {
                        provider_id: self.spec.id.clone(),
                    })?;
                Ok(request.header("authorization", format!("Bearer {}", key.expose())))
            }
        }
    }

    /// 组装 `chat/completions` 请求体。
    fn build_chat_body(&self, request: &ChatRequest) -> Value {
        let mut body = Map::new();
        body.insert("model".to_string(), json!(request.model.model_id));
        body.insert(
            "messages".to_string(),
            Value::Array(request.messages.iter().map(message_to_wire).collect()),
        );
        body.insert("stream".to_string(), Value::Bool(true));
        // 要求上游在流末尾回报 usage：否则只能靠本地估算，无法校正上下文占用。
        body.insert(
            "stream_options".to_string(),
            json!({ "include_usage": true }),
        );
        if !request.tools.is_empty() {
            body.insert(
                "tools".to_string(),
                Value::Array(request.tools.iter().map(tool_to_wire).collect()),
            );
        }
        if let Some(choice) = request.tool_choice {
            body.insert("tool_choice".to_string(), json!(tool_choice_wire(choice)));
        }
        if let Some(temperature) = request.params.temperature {
            body.insert("temperature".to_string(), json!(temperature));
        }
        if let Some(max_output_tokens) = request.params.max_output_tokens {
            body.insert("max_tokens".to_string(), json!(max_output_tokens));
        }
        if let Some(top_p) = request.params.top_p {
            body.insert("top_p".to_string(), json!(top_p));
        }
        if !request.params.stop.is_empty() {
            body.insert("stop".to_string(), json!(request.params.stop));
        }
        // 厂商私有参数：原样铺在顶层（例如 `{"top_k": 40}`）。
        if let Some(Value::Object(extra)) = &request.params.extra {
            for (key, value) in extra {
                body.insert(key.clone(), value.clone());
            }
        }
        Value::Object(body)
    }

    /// 非 200 响应的分类。
    fn classify_error(
        &self,
        status: u16,
        headers: &[(String, String)],
        body_text: &str,
    ) -> LlmError {
        let upstream = parse_upstream_error(body_text);
        let message = upstream_message(&upstream).unwrap_or_else(|| truncate(body_text, 400));
        match status {
            401 | 403 => LlmError::AuthFailed {
                status,
                message,
                upstream,
            },
            429 => LlmError::RateLimited {
                status,
                message,
                retry_after_ms: retry_after_ms(headers),
                upstream,
            },
            _ => LlmError::Unavailable {
                message: format!("HTTP {status}: {message}"),
                status: Some(status),
            },
        }
    }

    /// 把响应读成文本（非流式接口用）。
    fn require_text_body<'a>(
        &self,
        response: &'a crate::transport::HttpResponse,
    ) -> Result<&'a str, LlmError> {
        response
            .text_body()
            .ok_or_else(|| LlmError::BadResponse("上游返回了非文本响应体".to_string()))
    }
}

#[async_trait::async_trait]
impl Provider for OpenAiProvider {
    fn spec(&self) -> &ProviderSpec {
        &self.spec
    }

    fn has_key(&self) -> bool {
        self.custom_authorization().is_some() || self.api_key.is_some()
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
        sink: EventSink,
        cancel: CancelToken,
    ) -> Result<Usage, LlmError> {
        if cancel.is_cancelled() {
            return Err(LlmError::Cancelled);
        }

        let body = self.build_chat_body(&request);
        let http = HttpRequest::post_json(
            self.endpoint("chat/completions"),
            body.to_string(),
            self.spec.timeout_ms_or_default(),
        )
        .header("accept", "text/event-stream")
        .streaming();
        let http = self.authorize(http)?;

        let response = self.transport.send(http).await?;
        if response.status != 200 {
            let text = response.text_body().unwrap_or_default().to_string();
            return Err(self.classify_error(response.status, &response.headers, &text));
        }

        let ResponseBody::Chunks(mut chunks) = response.body else {
            return Err(LlmError::BadResponse(
                "上游未返回流式响应体（期望 SSE）".to_string(),
            ));
        };

        let mut decoder = Utf8Decoder::new();
        let mut sse = SseDecoder::new();
        let mut state = StreamState::default();

        loop {
            // 每轮先同步判一次取消。上游若把整段响应一次性缓冲好（本地网关、代理缓存），
            // 下面的 select 每轮两支同时就绪、命中哪支是随机的，光靠 select 会让「取消」
            // 退化成概率事件；这里判一次，取消一定命中，且不会再往 sink 推任何事件。
            if cancel.is_cancelled() {
                return Err(LlmError::Cancelled);
            }
            let next = tokio::select! {
                // 流正等上游数据（Pending）时就地响应取消，不必等下一块分片到齐。
                _ = cancel.cancelled() => return Err(LlmError::Cancelled),
                item = futures_util::StreamExt::next(&mut chunks) => item,
            };
            let Some(item) = next else { break };
            // 主动让出一次执行权。从解码到 emit 全程没有 Pending 点，上游一旦是「本地已缓冲」，
            // 本任务会把 tokio 线程占满，点「停止」的那个任务根本排不上调度，取消要等整段读完。
            tokio::task::yield_now().await;
            let bytes = item?;
            let text = decoder.push(&bytes);
            if text.is_empty() {
                continue;
            }
            for frame in sse.push(&text) {
                state.absorb(translate_openai_chunk(&frame.data)?, &sink)?;
            }
            if state.done {
                break;
            }
        }

        if !state.done {
            if let Some(frame) = sse.finish() {
                state.absorb(translate_openai_chunk(&frame.data)?, &sink)?;
            }
        }

        if !state.done && state.finish_reason.is_none() {
            return Err(LlmError::Unavailable {
                message: "上游流在给出结束标记前中断".to_string(),
                status: None,
            });
        }

        emit(
            &sink,
            StreamEvent::Finished {
                reason: state.finish_reason.unwrap_or(FinishReason::Unknown),
                message_id: state.message_id.clone(),
            },
        );
        Ok(state.usage)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        let http = HttpRequest::get(self.endpoint("models"), self.spec.timeout_ms_or_default());
        let http = self.authorize(http)?;
        let response = self.transport.send(http).await?;
        if response.status != 200 {
            let text = response.text_body().unwrap_or_default().to_string();
            return Err(self.classify_error(response.status, &response.headers, &text));
        }
        let text = self.require_text_body(&response)?.to_string();
        let envelope: ModelsEnvelope = serde_json::from_str(&text)
            .map_err(|err| LlmError::BadResponse(format!("模型列表解析失败: {err}")))?;
        Ok(envelope
            .data
            .into_iter()
            .map(|item| ModelInfo {
                id: item.id,
                name: item.name,
                owned_by: item.owned_by,
            })
            .collect())
    }

    async fn health(&self) -> Result<HealthReport, LlmError> {
        let started = Instant::now();
        let result = self.list_models().await;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(match result {
            Ok(models) => HealthReport {
                ok: true,
                latency_ms,
                models_found: models.len(),
                message: None,
            },
            // 探测失败是「结果」而不是「异常」：把错误码与文案回给 UI，由用户决定下一步。
            Err(err) => HealthReport {
                ok: false,
                latency_ms,
                models_found: 0,
                message: Some(format!("[{}] {}", err.code(), err)),
            },
        })
    }
}

/// 一次流的状态机：累积用量、结束原因与响应 id，并负责把事件推给 sink。
#[derive(Default)]
struct StreamState {
    usage: Usage,
    finish_reason: Option<FinishReason>,
    message_id: Option<String>,
    done: bool,
}

impl StreamState {
    fn absorb(&mut self, translated: TranslatedChunk, sink: &EventSink) -> Result<(), LlmError> {
        if let Some(id) = translated.message_id {
            self.message_id = Some(id);
        }
        if let Some(reason) = translated.finish_reason {
            self.finish_reason = Some(reason);
        }
        if translated.done {
            self.done = true;
        }
        for event in translated.events {
            if let StreamEvent::Usage(usage) = &event {
                self.usage = *usage;
            }
            if !emit(sink, event) {
                // 接收端已关闭：UI 已经不等这条流了，立刻停止读上游，别空转。
                return Err(LlmError::Cancelled);
            }
        }
        Ok(())
    }
}

/// 消息 → 上游 wire 格式。
///
/// `attachments` 暂不透传：M2 只登记路径，真正读取与内容注入属于 M3 的 @上下文，
/// 到那时再按模型能力决定是走视觉输入还是文本注入。
fn message_to_wire(message: &ChatMessage) -> Value {
    let role = match message.role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    };
    let mut wire = Map::new();
    wire.insert("role".to_string(), json!(role));
    if message.role == ChatRole::Assistant && !message.tool_calls.is_empty() {
        // 纯工具调用轮：content 给 null，部分网关对空串会报参数错误。
        wire.insert("content".to_string(), Value::Null);
    } else {
        wire.insert("content".to_string(), json!(message.content));
    }
    if !message.tool_calls.is_empty() {
        wire.insert(
            "tool_calls".to_string(),
            Value::Array(
                message
                    .tool_calls
                    .iter()
                    .map(|call| {
                        json!({
                            "id": call.id,
                            "type": "function",
                            "function": { "name": call.name, "arguments": call.arguments },
                        })
                    })
                    .collect(),
            ),
        );
    }
    if let Some(tool_call_id) = &message.tool_call_id {
        wire.insert("tool_call_id".to_string(), json!(tool_call_id));
    }
    Value::Object(wire)
}

fn tool_to_wire(tool: &ToolSpec) -> Value {
    let mut function = Map::new();
    function.insert("name".to_string(), json!(tool.name));
    if let Some(description) = &tool.description {
        function.insert("description".to_string(), json!(description));
    }
    function.insert("parameters".to_string(), tool.parameters.clone());
    json!({ "type": "function", "function": Value::Object(function) })
}

fn tool_choice_wire(choice: ToolChoice) -> &'static str {
    match choice {
        ToolChoice::Auto => "auto",
        ToolChoice::None => "none",
        ToolChoice::Required => "required",
    }
}

/// 从上游错误响应体里提取 `error.message` / `message`。
fn parse_upstream_error(body: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(body).ok()?;
    Some(value)
}

fn upstream_message(upstream: &Option<Value>) -> Option<String> {
    let value = upstream.as_ref()?;
    value
        .get("error")
        .and_then(|error| error.get("message"))
        .or_else(|| value.get("message"))
        .and_then(|message| message.as_str())
        .map(|message| message.to_string())
}

fn truncate(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    let mut out: String = chars[..max_chars].iter().collect();
    out.push('…');
    out
}

/// 解析 `Retry-After`（秒）。
fn retry_after_ms(headers: &[(String, String)]) -> Option<u64> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
        .and_then(|(_, value)| value.trim().parse::<u64>().ok())
        .map(|seconds| seconds.saturating_mul(1000))
}

#[derive(serde::Deserialize)]
struct ModelsEnvelope {
    #[serde(default)]
    data: Vec<ModelItem>,
}

#[derive(serde::Deserialize)]
struct ModelItem {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    owned_by: Option<String>,
}

/// 便于测试与 core 组装：从若干字段直接建 spec。
#[must_use]
pub fn spec_of(
    id: &str,
    name: &str,
    base_url: &str,
    headers: BTreeMap<String, String>,
    timeout_ms: Option<u64>,
) -> ProviderSpec {
    ProviderSpec {
        id: id.to_string(),
        name: name.to_string(),
        protocol: ai_ide_protocol::dto::ProviderProtocol::OpenaiChatCompletions,
        base_url: base_url.to_string(),
        headers,
        enabled: true,
        timeout_ms,
    }
}

/// 构造一个 [`ModelRef`]（测试与 core 常用）。
#[must_use]
pub fn model_ref(provider_id: &str, model_id: &str) -> ModelRef {
    ModelRef {
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> OpenAiProvider {
        OpenAiProvider::new(
            spec_of(
                "p1",
                "本地网关",
                "https://gw.example.com/v1/",
                BTreeMap::new(),
                None,
            ),
            Some(SecretString::new("sk-test-key-123456")),
            Arc::new(crate::transport::ReqwestTransport::default()),
        )
    }

    #[test]
    fn endpoint_joins_without_double_slash() {
        let provider = provider();
        assert_eq!(
            provider.endpoint("/chat/completions"),
            "https://gw.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn authorize_adds_bearer_header() {
        let provider = provider();
        let request = provider
            .authorize(HttpRequest::get("https://gw.example.com/v1/models", 1000))
            .unwrap();
        let auth = request
            .headers
            .iter()
            .find(|(name, _)| name == "authorization")
            .map(|(_, value)| value.clone());
        assert_eq!(auth.as_deref(), Some("Bearer sk-test-key-123456"));
    }

    #[test]
    fn authorize_prefers_custom_authorization_header() {
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_string(), "Api-Key abc".to_string());
        let provider = OpenAiProvider::new(
            spec_of("p1", "网关", "https://gw.example.com/v1", headers, None),
            None,
            Arc::new(crate::transport::ReqwestTransport::default()),
        );
        assert!(provider.has_key(), "自定义鉴权头也算配好了密钥");
        let request = provider
            .authorize(HttpRequest::get("https://gw.example.com/v1/models", 1000))
            .unwrap();
        assert!(request
            .headers
            .iter()
            .any(|(name, value)| name == "Authorization" && value == "Api-Key abc"));
    }

    #[test]
    fn authorize_without_key_reports_not_configured() {
        let provider = OpenAiProvider::new(
            spec_of(
                "p1",
                "网关",
                "https://gw.example.com/v1",
                BTreeMap::new(),
                None,
            ),
            None,
            Arc::new(crate::transport::ReqwestTransport::default()),
        );
        let err = provider
            .authorize(HttpRequest::get("https://gw.example.com/v1/models", 1000))
            .unwrap_err();
        assert!(matches!(err, LlmError::ProviderNotConfigured { .. }));
    }

    #[test]
    fn chat_body_sets_stream_and_usage_options() {
        let provider = provider();
        let request = ChatRequest {
            model: model_ref("p1", "deepseek-chat"),
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "你好".to_string(),
                attachments: vec![],
                tool_calls: vec![],
                tool_call_id: None,
            }],
            tools: vec![ToolSpec {
                name: "read_file".to_string(),
                description: Some("读取文件".to_string()),
                parameters: json!({ "type": "object" }),
            }],
            tool_choice: Some(ToolChoice::Auto),
            params: ai_ide_protocol::dto::ChatParams {
                temperature: Some(0.2),
                max_output_tokens: Some(1024),
                top_p: None,
                stop: vec!["</end>".to_string()],
                extra: Some(json!({ "top_k": 40 })),
            },
        };
        let body = provider.build_chat_body(&request);
        assert_eq!(body["model"], "deepseek-chat");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        assert_eq!(body["temperature"], 0.2);
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["stop"][0], "</end>");
        assert_eq!(body["top_k"], 40, "厂商私有参数应原样透传");
    }

    #[test]
    fn assistant_with_tool_calls_sends_null_content() {
        let message = ChatMessage {
            role: ChatRole::Assistant,
            content: String::new(),
            attachments: vec![],
            tool_calls: vec![ai_ide_protocol::dto::ChatToolCall {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: "{}".to_string(),
            }],
            tool_call_id: None,
        };
        let wire = message_to_wire(&message);
        assert!(wire["content"].is_null());
        assert_eq!(wire["tool_calls"][0]["function"]["name"], "read_file");
    }

    #[test]
    fn tool_message_carries_call_id() {
        let message = ChatMessage {
            role: ChatRole::Tool,
            content: "文件内容".to_string(),
            attachments: vec![],
            tool_calls: vec![],
            tool_call_id: Some("call_1".to_string()),
        };
        let wire = message_to_wire(&message);
        assert_eq!(wire["role"], "tool");
        assert_eq!(wire["tool_call_id"], "call_1");
    }

    #[test]
    fn error_classification_covers_auth_rate_limit_and_5xx() {
        let provider = provider();
        let body = r#"{"error":{"message":"Invalid API key"}}"#;

        let auth = provider.classify_error(401, &[], body);
        assert!(matches!(auth, LlmError::AuthFailed { status: 401, .. }));
        assert_eq!(
            auth.code(),
            ai_ide_protocol::ErrorCode::PROVIDER_AUTH_FAILED
        );

        let rate_limited = provider.classify_error(
            429,
            &[("Retry-After".to_string(), "3".to_string())],
            r#"{"error":{"message":"slow down"}}"#,
        );
        assert_eq!(rate_limited.retry_after_ms(), Some(3000));
        assert_eq!(
            rate_limited.code(),
            ai_ide_protocol::ErrorCode::PROVIDER_RATE_LIMITED
        );

        let unavailable = provider.classify_error(503, &[], "<html>gateway</html>");
        assert!(matches!(
            unavailable,
            LlmError::Unavailable {
                status: Some(503),
                ..
            }
        ));
    }

    #[test]
    fn upstream_message_is_extracted_from_error_object() {
        let upstream = parse_upstream_error(r#"{"error":{"message":"配额不足"}}"#);
        assert_eq!(upstream_message(&upstream).as_deref(), Some("配额不足"));
        let flat = parse_upstream_error(r#"{"message":"模型不存在"}"#);
        assert_eq!(upstream_message(&flat).as_deref(), Some("模型不存在"));
        let not_json = parse_upstream_error("<html>");
        assert_eq!(upstream_message(&not_json), None);
    }

    #[test]
    fn truncate_keeps_char_boundaries() {
        assert_eq!(truncate("你好世界", 10), "你好世界");
        assert_eq!(truncate("你好世界", 2), "你好…");
    }
}
