//! 端到端骨架测试：**不需要网络与 API Key**。
//!
//! 用测试替身替换 HTTP 传输，把「一段真实的 SSE 字节流」喂给 [`OpenAiProvider`]，
//! 断言事件序列、请求体、错误分类与取消行为。这是本阶段最重要的验收证据：
//! 它证明模型接入的**全部本地逻辑**已经闭合并可回归。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ai_ide_llm::openai::spec_of;
use ai_ide_llm::provider::CancelToken;
use ai_ide_llm::transport::{HttpRequest, HttpResponse, HttpTransport, ResponseBody};
use ai_ide_llm::{
    ChatRequest, LlmError, OpenAiProvider, Provider, SecretString, StreamEvent, Usage,
};
use ai_ide_protocol::dto::{ChatMessage, ChatParams, ChatRole, ModelRef, ToolSpec};

/// 测试替身：记下请求，回放预设响应。
struct FakeTransport {
    reply: Reply,
    requests: Mutex<Vec<HttpRequest>>,
}

enum Reply {
    /// 流式：按给定字节块依次下发（用于模拟任意位置的分片）。
    Stream(Vec<Vec<u8>>),
    /// 非流式：固定状态码 + 文本响应体。
    Text {
        status: u16,
        body: String,
        headers: Vec<(String, String)>,
    },
}

impl FakeTransport {
    fn stream(chunks: Vec<Vec<u8>>) -> Arc<Self> {
        Arc::new(Self {
            reply: Reply::Stream(chunks),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn text(status: u16, body: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            reply: Reply::Text {
                status,
                body: body.into(),
                headers: Vec::new(),
            },
            requests: Mutex::new(Vec::new()),
        })
    }

    fn text_with_headers(
        status: u16,
        body: impl Into<String>,
        headers: Vec<(String, String)>,
    ) -> Arc<Self> {
        Arc::new(Self {
            reply: Reply::Text {
                status,
                body: body.into(),
                headers,
            },
            requests: Mutex::new(Vec::new()),
        })
    }

    fn last_request(&self) -> HttpRequest {
        self.requests
            .lock()
            .expect("请求记录锁未被污染")
            .last()
            .cloned()
            .expect("至少应发出一个请求")
    }

    fn request_count(&self) -> usize {
        self.requests.lock().expect("请求记录锁未被污染").len()
    }
}

#[async_trait::async_trait]
impl HttpTransport for FakeTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, LlmError> {
        self.requests
            .lock()
            .expect("请求记录锁未被污染")
            .push(request);
        Ok(match &self.reply {
            Reply::Stream(chunks) => {
                let items: Vec<Result<Vec<u8>, LlmError>> =
                    chunks.iter().map(|chunk| Ok(chunk.clone())).collect();
                HttpResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: ResponseBody::Chunks(Box::pin(futures_util::stream::iter(items))),
                }
            }
            Reply::Text {
                status,
                body,
                headers,
            } => HttpResponse {
                status: *status,
                headers: headers.clone(),
                body: ResponseBody::Text(body.clone()),
            },
        })
    }
}

fn provider_with(transport: Arc<FakeTransport>, api_key: Option<&str>) -> OpenAiProvider {
    OpenAiProvider::new(
        spec_of(
            "local-gateway",
            "本地网关",
            "https://gw.example.com/v1/",
            BTreeMap::new(),
            Some(5_000),
        ),
        api_key.map(SecretString::new),
        transport,
    )
}

fn user_request() -> ChatRequest {
    ChatRequest {
        model: ModelRef {
            provider_id: "local-gateway".to_string(),
            model_id: "deepseek-chat".to_string(),
        },
        messages: vec![ChatMessage {
            role: ChatRole::User,
            content: "讲个笑话".to_string(),
            attachments: vec![],
            tool_calls: vec![],
            tool_call_id: None,
        }],
        tools: vec![ToolSpec {
            name: "read_file".to_string(),
            description: Some("读取文件".to_string()),
            parameters: serde_json::json!({ "type": "object" }),
        }],
        tool_choice: None,
        params: ChatParams {
            temperature: Some(0.3),
            max_output_tokens: Some(512),
            top_p: None,
            stop: vec![],
            extra: None,
        },
    }
}

/// 把完整 SSE 文本切成每块 2 字节——刻意在汉字与行中间切断，
/// 逼出「增量 UTF-8 解码 + 跨块分帧」两条最容易写错的路径。
fn split_every_two_bytes(text: &str) -> Vec<Vec<u8>> {
    text.as_bytes()
        .chunks(2)
        .map(|chunk| chunk.to_vec())
        .collect()
}

const SSE_SCRIPT: &str = concat!(
    ": ping\n\n",
    "data: {\"id\":\"chatcmpl-9\",\"choices\":[{\"delta\":{\"reasoning_content\":\"先想\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"，世界\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7}}\n\n",
    "data: [DONE]\n\n",
);

#[tokio::test]
async fn streams_full_lifecycle_from_raw_sse_bytes() {
    let transport = FakeTransport::stream(split_every_two_bytes(SSE_SCRIPT));
    let provider = provider_with(transport.clone(), Some("sk-abcdefghijklmnop"));
    let (sink, mut receiver) = tokio::sync::mpsc::unbounded_channel();

    let usage = provider
        .chat_stream(user_request(), sink, CancelToken::new())
        .await
        .expect("完整脚本应当正常收流");

    let mut events = Vec::new();
    while let Some(event) = receiver.recv().await {
        events.push(event);
    }

    assert_eq!(
        events,
        vec![
            StreamEvent::Reasoning {
                text: "先想".to_string()
            },
            StreamEvent::Delta {
                text: "你好".to_string()
            },
            StreamEvent::Delta {
                text: "，世界".to_string()
            },
            StreamEvent::ToolCall {
                index: 0,
                id: Some("call_1".to_string()),
                name: Some("read_file".to_string()),
                arguments_delta: "{}".to_string(),
            },
            StreamEvent::Usage(Usage {
                prompt_tokens: 11,
                completion_tokens: 7,
                cached_tokens: None,
            }),
            StreamEvent::Finished {
                reason: ai_ide_protocol::dto::FinishReason::ToolCalls,
                message_id: Some("chatcmpl-9".to_string()),
            },
        ],
        "事件顺序与内容必须与上游脚本一一对应"
    );
    assert_eq!(
        usage,
        Usage {
            prompt_tokens: 11,
            completion_tokens: 7,
            cached_tokens: None,
        }
    );
}

#[tokio::test]
async fn request_is_wired_with_auth_streaming_and_model() {
    let transport = FakeTransport::stream(split_every_two_bytes(SSE_SCRIPT));
    let provider = provider_with(transport.clone(), Some("sk-abcdefghijklmnop"));
    let (sink, _receiver) = tokio::sync::mpsc::unbounded_channel();

    provider
        .chat_stream(user_request(), sink, CancelToken::new())
        .await
        .unwrap();

    let request = transport.last_request();
    assert_eq!(
        request.url, "https://gw.example.com/v1/chat/completions",
        "base_url 尾部斜杠应被归一化"
    );
    assert!(request.stream, "chat_stream 必须走流式读取");
    assert_eq!(request.timeout_ms, 5_000);
    let auth = request
        .headers
        .iter()
        .find(|(name, _)| name == "authorization")
        .map(|(_, value)| value.as_str());
    assert_eq!(auth, Some("Bearer sk-abcdefghijklmnop"));

    let body: serde_json::Value = serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
    assert_eq!(body["model"], "deepseek-chat");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "讲个笑话");
    assert_eq!(body["tools"][0]["function"]["name"], "read_file");
    assert_eq!(body["max_tokens"], 512);
}

#[tokio::test]
async fn cancel_before_send_never_touches_the_network() {
    let transport = FakeTransport::stream(split_every_two_bytes(SSE_SCRIPT));
    let provider = provider_with(transport.clone(), Some("sk-abcdefghijklmnop"));
    let (sink, _receiver) = tokio::sync::mpsc::unbounded_channel();

    let cancel = CancelToken::new();
    cancel.cancel();
    let err = provider
        .chat_stream(user_request(), sink, cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::Cancelled));
    assert_eq!(transport.request_count(), 0, "已取消的请求不该发出去");
}

/// 取消必须在流还在跑的时候就生效。上游若把整段响应一次性缓冲好（本地网关、代理缓存），
/// poll 永远 Ready——本用例同时守着「不许把 tokio 线程占满、停止按钮不许形同虚设」。
#[tokio::test]
async fn cancel_mid_stream_stops_reading() {
    let transport = FakeTransport::stream(split_every_two_bytes(SSE_SCRIPT));
    let provider = provider_with(transport.clone(), Some("sk-abcdefghijklmnop"));
    let (sink, mut receiver) = tokio::sync::mpsc::unbounded_channel();

    let cancel = CancelToken::new();
    let canceller = cancel.clone();
    // 收到第一段增量就取消，模拟用户点「停止」。
    let watcher = tokio::spawn(async move {
        if receiver.recv().await.is_some() {
            canceller.cancel();
        }
        receiver
    });

    let err = provider
        .chat_stream(user_request(), sink, cancel)
        .await
        .unwrap_err();
    let mut receiver = watcher.await.unwrap();
    assert!(
        matches!(err, LlmError::Cancelled),
        "取消必须返回 Cancelled: {err}"
    );
    // 取消后不应再有事件（尤其是不应出现 Finished）。
    let mut tail = Vec::new();
    while let Some(event) = receiver.recv().await {
        tail.push(event);
    }
    assert!(
        !tail
            .iter()
            .any(|event| matches!(event, StreamEvent::Finished { .. })),
        "取消的流不应给出 Finished 事件"
    );
}

#[tokio::test]
async fn closed_sink_ends_the_stream() {
    let transport = FakeTransport::stream(split_every_two_bytes(SSE_SCRIPT));
    let provider = provider_with(transport.clone(), Some("sk-abcdefghijklmnop"));
    let (sink, receiver) = tokio::sync::mpsc::unbounded_channel();
    drop(receiver);

    let err = provider
        .chat_stream(user_request(), sink, CancelToken::new())
        .await
        .unwrap_err();
    assert!(
        matches!(err, LlmError::Cancelled),
        "接收端关闭时应立刻停流而不是空转: {err}"
    );
}

#[tokio::test]
async fn missing_key_is_reported_before_any_request() {
    let transport = FakeTransport::stream(vec![]);
    let provider = provider_with(transport.clone(), None);
    let (sink, _receiver) = tokio::sync::mpsc::unbounded_channel();

    let err = provider
        .chat_stream(user_request(), sink, CancelToken::new())
        .await
        .unwrap_err();
    assert!(
        matches!(err, LlmError::ProviderNotConfigured { .. }),
        "缺密钥应报 2001 而不是等上游 401: {err}"
    );
    assert_eq!(transport.request_count(), 0);
}

#[tokio::test]
async fn auth_failure_surfaces_upstream_message() {
    let transport = FakeTransport::text(401, r#"{"error":{"message":"Invalid API key provided"}}"#);
    let provider = provider_with(transport, Some("sk-abcdefghijklmnop"));
    let (sink, _receiver) = tokio::sync::mpsc::unbounded_channel();

    let err = provider
        .chat_stream(user_request(), sink, CancelToken::new())
        .await
        .unwrap_err();
    match err {
        LlmError::AuthFailed {
            status, message, ..
        } => {
            assert_eq!(status, 401);
            assert!(
                message.contains("Invalid API key"),
                "应透传上游文案: {message}"
            );
        }
        other => panic!("期望 AuthFailed，实际: {other}"),
    }
}

#[tokio::test]
async fn rate_limit_keeps_retry_after_hint() {
    let transport = FakeTransport::text_with_headers(
        429,
        r#"{"error":{"message":"rate limited"}}"#,
        vec![("Retry-After".to_string(), "2".to_string())],
    );
    let provider = provider_with(transport, Some("sk-abcdefghijklmnop"));
    let (sink, _receiver) = tokio::sync::mpsc::unbounded_channel();

    let err = provider
        .chat_stream(user_request(), sink, CancelToken::new())
        .await
        .unwrap_err();
    assert_eq!(err.retry_after_ms(), Some(2000));
    let object = err.to_error_object();
    assert_eq!(object.data.unwrap()["retry_after_ms"], 2000);
}

#[tokio::test]
async fn truncated_stream_is_reported_as_unavailable() {
    // 只有内容、没有 finish_reason、也没有 [DONE]：上游把连接掐了。
    let transport = FakeTransport::stream(split_every_two_bytes(
        "data: {\"choices\":[{\"delta\":{\"content\":\"半句\"}}]}\n\n",
    ));
    let provider = provider_with(transport, Some("sk-abcdefghijklmnop"));
    let (sink, _receiver) = tokio::sync::mpsc::unbounded_channel();

    let err = provider
        .chat_stream(user_request(), sink, CancelToken::new())
        .await
        .unwrap_err();
    assert!(
        matches!(err, LlmError::Unavailable { .. }),
        "半截流必须显式报错而不是当作正常结束: {err}"
    );
}

#[tokio::test]
async fn stream_ending_without_final_blank_line_still_completes() {
    // 与上一个用例互为对照：这次上游给出的是一段**正常但没以空行收尾**的流——
    // 最后一帧连换行符都没有。SSE 的分帧规则要求以空行关帧，所以这最后一帧必须由
    // `SseDecoder::finish` 补出来；少了它，带 `finish_reason` 的尾帧会被静默丢掉，
    // 一次正常收流会被误报成「上游提前中断」。
    let script = concat!(
        "data: {\"id\":\"chatcmpl-tail\",\"choices\":[{\"delta\":{\"content\":\"尾帧也要在\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]",
    );
    let transport = FakeTransport::stream(split_every_two_bytes(script));
    let provider = provider_with(transport, Some("sk-abcdefghijklmnop"));
    let (sink, mut receiver) = tokio::sync::mpsc::unbounded_channel();

    let usage = provider
        .chat_stream(user_request(), sink, CancelToken::new())
        .await
        .expect("尾帧没有空行收尾时，也必须按正常结束处理");

    let mut events = Vec::new();
    while let Some(event) = receiver.recv().await {
        events.push(event);
    }

    assert_eq!(
        events,
        vec![
            StreamEvent::Delta {
                text: "尾帧也要在".to_string()
            },
            StreamEvent::Finished {
                reason: ai_ide_protocol::dto::FinishReason::Stop,
                message_id: Some("chatcmpl-tail".to_string()),
            },
        ],
        "被补出的尾帧里，结束原因与响应 id 都不能丢"
    );
    assert_eq!(usage, Usage::default());
}

#[tokio::test]
async fn list_models_parses_openai_envelope() {
    let transport = FakeTransport::text(
        200,
        r#"{"object":"list","data":[{"id":"deepseek-chat","name":"DeepSeek Chat","owned_by":"deepseek"},{"id":"deepseek-reasoner"}]}"#,
    );
    let provider = provider_with(transport, Some("sk-abcdefghijklmnop"));

    let models = provider.list_models().await.unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "deepseek-chat");
    assert_eq!(models[0].owned_by.as_deref(), Some("deepseek"));
    assert!(models[1].name.is_none());
}

#[tokio::test]
async fn health_failure_is_a_result_not_an_error() {
    let transport = FakeTransport::text(500, "<html>bad gateway</html>");
    let provider = provider_with(transport, Some("sk-abcdefghijklmnop"));

    let report = provider.health().await.expect("探测失败不应抛错");
    assert!(!report.ok);
    assert_eq!(report.models_found, 0);
    let message = report.message.expect("失败必须带原因");
    assert!(message.contains("2005"), "失败原因应带错误码: {message}");
}

#[tokio::test]
async fn health_success_reports_model_count() {
    let transport = FakeTransport::text(200, r#"{"data":[{"id":"m1"}]}"#);
    let provider = provider_with(transport, Some("sk-abcdefghijklmnop"));

    let report = provider.health().await.unwrap();
    assert!(report.ok);
    assert_eq!(report.models_found, 1);
    assert!(report.message.is_none());
}
