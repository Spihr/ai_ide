//! M2 流式对话的端到端链路：真实内核子进程 + 本地假上游。
//!
//! 为什么必须真起一个 HTTP 上游：`llm.*` 这条链路里最容易出错的地方恰恰都在
//! 「进程之间」——SSE 分帧、`[DONE]` 收尾、取消是否真的断开、鉴权头有没有带出去、
//! 上游 401/429 有没有折成可区分的错误码。用双重替身（trait 打桩）测这些，
//! 等于把最需要验证的一层换成自己写的假设。
//!
//! 假上游跑在回环 TCP 上，剧本由测试指定；内核仍然只通过 stdin / stdout 对话。
//!
//! **不依赖外网**：没有真实网关、没有真实密钥，CI 里也能跑。

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

mod common;

use common::{Core, TempDataDir, PLAINTEXT_KEY};

const MODEL: &str = "deepseek-chat";
/// 单条事件的等待上限。流式链路卡住时要失败得干脆，而不是把 CI 挂住。
const EVENT_WAIT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// 假上游
// ---------------------------------------------------------------------------

/// 一次 `chat/completions` 的剧本。
#[derive(Clone)]
enum Script {
    /// 正常结束：依次发出这些 SSE 负载，再补 `[DONE]`。
    Stream { frames: Vec<String> },
    /// 发完这些负载后把连接吊住 `hold`：用来区分「用户取消」与「上游断开」。
    Stall { frames: Vec<String>, hold: Duration },
    /// 非 200 响应。
    Failure {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    },
}

/// 假上游收到过的请求。用于断言「我们真的按协议发出去了什么」。
#[derive(Debug, Clone)]
struct SeenRequest {
    method: String,
    path: String,
    authorization: Option<String>,
    body: String,
}

struct Shared {
    scripts: Mutex<VecDeque<Script>>,
    fallback: Mutex<Script>,
    seen: Mutex<Vec<SeenRequest>>,
    models: String,
    stop: AtomicBool,
}

/// 回环假上游。
struct MockUpstream {
    base_url: String,
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
}

impl MockUpstream {
    fn start(scripts: Vec<Script>, models: Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定回环端口");
        let port = listener.local_addr().expect("本地地址").port();
        listener.set_nonblocking(true).expect("非阻塞监听");

        let shared = Arc::new(Shared {
            scripts: Mutex::new(scripts.into()),
            fallback: Mutex::new(Script::Stream { frames: Vec::new() }),
            seen: Mutex::new(Vec::new()),
            models: models.to_string(),
            stop: AtomicBool::new(false),
        });

        let accepting = Arc::clone(&shared);
        let join = std::thread::spawn(move || {
            while !accepting.stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let shared = Arc::clone(&accepting);
                        // 取消用例要求连接能长期吊住，所以每连接一个线程。
                        std::thread::spawn(move || {
                            let _ = serve(stream, shared);
                        });
                    }
                    Err(err) if err.kind() == ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            shared,
            join: Some(join),
        }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    /// 追加一个剧本（按请求顺序消费）。
    fn push_script(&self, script: Script) {
        self.shared
            .scripts
            .lock()
            .expect("剧本锁")
            .push_back(script);
    }

    fn requests(&self) -> Vec<SeenRequest> {
        self.shared.seen.lock().expect("请求记录锁").clone()
    }

    /// 最近一次 `chat/completions` 请求。
    fn last_chat(&self) -> SeenRequest {
        self.requests()
            .into_iter()
            .rfind(|seen| seen.path.ends_with("/chat/completions"))
            .expect("假上游应收到过 chat/completions 请求")
    }
}

impl Drop for MockUpstream {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

const SSE_HEAD: &str = "HTTP/1.1 200 OK\r\n\
     content-type: text/event-stream\r\n\
     cache-control: no-cache\r\n\
     transfer-encoding: chunked\r\n\
     connection: close\r\n\r\n";

/// 处理一条连接：读完整请求 → 按剧本响应。
fn serve(mut stream: TcpStream, shared: Arc<Shared>) -> std::io::Result<()> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let head = read_head(&mut stream)?;

    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut content_length = 0usize;
    let mut authorization = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse().unwrap_or(0);
        } else if name.eq_ignore_ascii_case("authorization") {
            authorization = Some(value.trim().to_string());
        }
    }

    let mut raw = vec![0u8; content_length];
    if content_length > 0 {
        stream.read_exact(&mut raw)?;
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    shared.seen.lock().expect("请求记录锁").push(SeenRequest {
        method,
        path: path.clone(),
        authorization,
        body: String::from_utf8_lossy(&raw).to_string(),
    });

    // `/v1/models`：连通性与清单转发共用，直接回固定清单。
    if path.ends_with("/models") {
        let body = shared.models.clone();
        return write_text(
            &mut stream,
            &format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            ),
        );
    }

    let script = {
        let mut queue = shared.scripts.lock().expect("剧本锁");
        match queue.pop_front() {
            Some(script) => script,
            None => shared.fallback.lock().expect("兜底剧本锁").clone(),
        }
    };

    match script {
        Script::Failure {
            status,
            headers,
            body,
        } => {
            let mut extra = String::new();
            for (name, value) in &headers {
                extra.push_str(&format!("{name}: {value}\r\n"));
            }
            write_text(
                &mut stream,
                &format!(
                    "HTTP/1.1 {status} MOCK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n{body}",
                    body.len()
                ),
            )
        }
        Script::Stream { frames } => {
            write_text(&mut stream, SSE_HEAD)?;
            for frame in &frames {
                write_chunk(&mut stream, frame)?;
            }
            // 正常收尾：零长块。少了它，客户端只能看到「流被掐断」。
            write_text(&mut stream, "0\r\n\r\n")
        }
        Script::Stall { frames, hold } => {
            write_text(&mut stream, SSE_HEAD)?;
            for frame in &frames {
                write_chunk(&mut stream, frame)?;
            }
            std::thread::sleep(hold);
            Ok(())
        }
    }
}

/// 读到 `\r\n\r\n` 为止。
fn read_head(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte)? == 0 {
            break;
        }
        buf.push(byte[0]);
        if buf.len() > 64 * 1024 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).to_string())
}

fn write_text(stream: &mut TcpStream, text: &str) -> std::io::Result<()> {
    stream.write_all(text.as_bytes())?;
    stream.flush()
}

/// 按 chunked 编码发一条 SSE 负载。
fn write_chunk(stream: &mut TcpStream, frame: &str) -> std::io::Result<()> {
    let payload = format!("{frame}\n\n");
    write_text(stream, &format!("{:x}\r\n{payload}\r\n", payload.len()))
}

// ---------------------------------------------------------------------------
// 剧本与夹具
// ---------------------------------------------------------------------------

/// 一条 `data:` 负载（OpenAI Chat Completions 分片）。
fn sse(payload: Value) -> String {
    format!("data: {payload}")
}

/// 一段文本增量分片。
fn delta(text: &str) -> String {
    sse(json!({
        "id": "chatcmpl-1",
        "object": "chat.completion.chunk",
        "choices": [{ "index": 0, "delta": { "content": text } }],
    }))
}

/// `model.upsert` + `provider.upsert`：把假上游登记成可用 Provider。
fn setup_provider(core: &mut Core, upstream: &MockUpstream) -> String {
    setup_provider_at(core, upstream.base_url())
}

/// 同上，但自行指定 `base_url`：像「上游不可达」这种场景没有假上游可指，
/// 只能给一个握不了手的地址。
fn setup_provider_at(core: &mut Core, base_url: &str) -> String {
    let created = core.ok(
        "provider.upsert",
        json!({
            "name": "本地假上游",
            "protocol": "openai_chat_completions",
            "base_url": base_url,
            "api_key": PLAINTEXT_KEY,
            "timeout_ms": 3_000,
        }),
    );
    let provider_id = created["provider_id"]
        .as_str()
        .expect("provider_id 是字符串")
        .to_string();

    core.ok(
        "model.upsert",
        json!({
            "provider_id": provider_id,
            "model_id": MODEL,
            "display_name": "假上游模型",
            "caps": { "streaming": true, "tools": false, "reasoning": false, "vision": false },
        }),
    );
    provider_id
}

fn chat_params(stream_id: &str, provider_id: &str) -> Value {
    json!({
        "stream_id": stream_id,
        "session_id": "sess-1",
        "model_ref": { "provider_id": provider_id, "model_id": MODEL },
        "messages": [{ "role": "user", "content": "你好" }],
    })
}

/// 取下一条事件：先看待处理队列，再从 socket 收。
///
/// 响应与通知谁先落盘取决于内核的写入时序，只认 socket 会在响应期间
/// 已经到达的事件上等超时。
fn next_event(core: &mut Core) -> Value {
    match core.pop_event() {
        Some(event) => event,
        None => core
            .try_read_frame(EVENT_WAIT)
            .unwrap_or_else(|| panic!("等待 llm.* 事件超时")),
    }
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

#[test]
fn 流式对话_端到端_事件有序且无重复() {
    let upstream = MockUpstream::start(
        vec![Script::Stream {
            frames: vec![
                delta("你好"),
                delta("，世界"),
                sse(json!({
                    "id": "chatcmpl-1",
                    "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                })),
                sse(json!({
                    "id": "chatcmpl-1",
                    "choices": [],
                    "usage": { "prompt_tokens": 7, "completion_tokens": 4, "total_tokens": 11 },
                })),
            ],
        }],
        json!({ "data": [{ "id": MODEL, "owned_by": "mock" }] }),
    );
    let data = TempDataDir::new("llm-happy");
    let mut core = Core::spawn(&data.root);
    let provider_id = setup_provider(&mut core, &upstream);

    let stream_id = "s-正常";
    let (_, accepted) = core.ok_async("llm.chat_stream", chat_params(stream_id, &provider_id));
    // 流式接口必须先把句柄回给调用方，否则「停止」按钮没有东西可按。
    assert_eq!(accepted["stream_id"], json!(stream_id));

    let mut methods: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut finished = Value::Null;
    while methods.last().map(String::as_str) != Some("llm.finished") {
        let event = next_event(&mut core);
        let method = event["method"].as_str().unwrap_or_default().to_string();
        if !method.starts_with("llm.") {
            continue; // provider.changed 等旁路通知
        }
        let params = &event["params"];
        assert_eq!(params["stream_id"], json!(stream_id), "事件必须带流 id");
        match method.as_str() {
            "llm.started" => {
                assert_eq!(params["model_ref"]["model_id"], json!(MODEL));
                assert_eq!(params["session_id"], json!("sess-1"), "会话 id 原样回落");
            }
            "llm.delta" => text.push_str(params["text"].as_str().unwrap_or_default()),
            "llm.usage" => {
                assert_eq!(params["prompt_tokens"], json!(7));
                assert_eq!(params["completion_tokens"], json!(4));
            }
            "llm.finished" => finished = params.clone(),
            other => panic!("流式链路不该出现 {other}"),
        }
        methods.push(method);
    }

    assert_eq!(
        methods,
        vec![
            "llm.started",
            "llm.delta",
            "llm.delta",
            "llm.usage",
            "llm.finished"
        ],
        "恰好一条 started / finished，且顺序固定：多一条 or 乱序都会让 UI 出现幽灵消息"
    );
    assert_eq!(text, "你好，世界");
    assert_eq!(finished["finish_reason"], json!("stop"));
    assert_eq!(finished["message_id"], json!("chatcmpl-1"));

    // 出网内容才是「真的接通了」的证据。
    let seen = upstream.last_chat();
    assert_eq!(seen.method, "POST");
    assert_eq!(seen.path, "/v1/chat/completions");
    assert_eq!(
        seen.authorization.as_deref(),
        Some(format!("Bearer {PLAINTEXT_KEY}").as_str()),
        "密钥必须以 Bearer 头出去，且是明文密钥本身"
    );
    let body: Value = serde_json::from_str(&seen.body).expect("请求体是 JSON");
    assert_eq!(body["model"], json!(MODEL));
    assert_eq!(body["stream"], json!(true));
    assert_eq!(
        body["stream_options"]["include_usage"],
        json!(true),
        "不索要 usage，用量就只能靠估算"
    );
    assert_eq!(body["messages"][0]["content"], json!("你好"));

    // 流结束后句柄必须释放：同一个 id 能立刻再用（`finish` 早于 `llm.finished`）。
    upstream.push_script(Script::Stream {
        frames: vec![
            delta("第二轮"),
            sse(json!({
                "id": "chatcmpl-2",
                "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            })),
        ],
    });
    let (_, again) = core.ok_async("llm.chat_stream", chat_params(stream_id, &provider_id));
    assert_eq!(again["stream_id"], json!(stream_id));
    let done = core.wait_notification("llm.finished");
    assert_eq!(done["params"]["message_id"], json!("chatcmpl-2"));
}

#[test]
fn 取消只终结本流_不报错_且句柄可复用() {
    let upstream = MockUpstream::start(
        vec![Script::Stall {
            frames: vec![delta("半句")],
            // 吊得比测试本身久：这样「流停了」只可能是取消生效。
            hold: Duration::from_secs(30),
        }],
        json!({ "data": [] }),
    );
    let data = TempDataDir::new("llm-cancel");
    let mut core = Core::spawn(&data.root);
    let provider_id = setup_provider(&mut core, &upstream);

    let stream_id = "s-取消";
    let (_, accepted) = core.ok_async("llm.chat_stream", chat_params(stream_id, &provider_id));
    assert_eq!(accepted["stream_id"], json!(stream_id));

    let started = core.wait_notification("llm.started");
    assert_eq!(started["params"]["stream_id"], json!(stream_id));
    let first = core.wait_notification("llm.delta");
    assert_eq!(first["params"]["text"], json!("半句"));

    assert_eq!(
        core.ok("llm.cancel", json!({ "stream_id": stream_id }))["ok"],
        json!(true)
    );

    // 取消是「正常收尾」而不是「异常」：既不该补一条 finished，
    // 也不该报 CANCELLED —— 用户自己按的停止，弹红字只会让人以为出了故障。
    let deadline = Instant::now() + Duration::from_millis(900);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let Some(event) = core.try_read_frame(remaining) else {
            break;
        };
        let method = event["method"].as_str().unwrap_or_default();
        assert!(
            !matches!(method, "llm.finished" | "llm.error"),
            "取消后不该再有收尾事件：{event}"
        );
    }
    assert!(
        core.events_named("llm.finished").is_empty() && core.events_named("llm.error").is_empty(),
        "待处理队列里也不该有收尾事件：{:?}",
        core.events()
    );

    // 句柄已释放，且旧流的清理没有留下僵尸登记：同 id 立刻可再用。
    upstream.push_script(Script::Stream {
        frames: vec![
            delta("重启后"),
            sse(json!({
                "id": "chatcmpl-3",
                "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            })),
        ],
    });
    let (_, again) = core.ok_async("llm.chat_stream", chat_params(stream_id, &provider_id));
    assert_eq!(again["stream_id"], json!(stream_id));
    let done = core.wait_notification("llm.finished");
    assert_eq!(done["params"]["message_id"], json!("chatcmpl-3"));
}

#[test]
fn 取消未命中的流不算错误() {
    let upstream = MockUpstream::start(vec![], json!({ "data": [] }));
    let data = TempDataDir::new("llm-cancel-miss");
    let mut core = Core::spawn(&data.root);
    setup_provider(&mut core, &upstream);

    // 取消请求常与「流刚好自己结束」竞态；此时报错只会变成一条毫无意义的红色提示。
    assert_eq!(
        core.ok("llm.cancel", json!({ "stream_id": "s-从未存在" }))["ok"],
        json!(true)
    );
}

#[test]
fn 上游报错_折成可区分的错误码与事件() {
    let upstream = MockUpstream::start(
        vec![
            Script::Failure {
                status: 401,
                headers: Vec::new(),
                body: json!({ "error": { "message": "invalid api key", "type": "authentication_error" } })
                    .to_string(),
            },
            Script::Failure {
                status: 429,
                headers: vec![("retry-after".to_string(), "3".to_string())],
                body: json!({ "error": { "message": "rate limit exceeded" } }).to_string(),
            },
        ],
        json!({ "data": [] }),
    );
    let data = TempDataDir::new("llm-errors");
    let mut core = Core::spawn(&data.root);
    let provider_id = setup_provider(&mut core, &upstream);

    // 401：换密钥（2002）
    let (_, accepted) = core.ok_async("llm.chat_stream", chat_params("s-401", &provider_id));
    assert_eq!(accepted["stream_id"], json!("s-401"));
    let event = core.wait_notification("llm.error");
    let params = &event["params"];
    assert_eq!(params["stream_id"], json!("s-401"));
    assert_eq!(params["code"], json!(2002), "PROVIDER_AUTH_FAILED");
    assert!(
        params["message"]
            .as_str()
            .unwrap_or_default()
            .contains("invalid api key"),
        "上游原话要带出来，否则用户只能猜：{params}"
    );
    assert_eq!(
        params["upstream"]["error"]["type"],
        json!("authentication_error"),
        "原始错误体要留着，便于自查配额与模型名"
    );
    assert_eq!(params["retry_after_ms"], json!(null));
    assert!(
        core.events_named("llm.finished").is_empty(),
        "报错就报错，不能伪装成正常结束"
    );

    // 429：稍后重试（2003），并把上游建议的等待时间带出去
    let (_, accepted) = core.ok_async("llm.chat_stream", chat_params("s-429", &provider_id));
    assert_eq!(accepted["stream_id"], json!("s-429"));
    let event = core.wait_notification("llm.error");
    let params = &event["params"];
    assert_eq!(params["code"], json!(2003), "PROVIDER_RATE_LIMITED");
    assert_eq!(
        params["retry_after_ms"],
        json!(3000),
        "「3 秒后重试」是限流场景里最有用的一半信息，不能丢在适配器里"
    );
    assert_eq!(
        params["upstream"]["error"]["message"],
        json!("rate limit exceeded")
    );
}

#[test]
fn 模型清单_转发上游结果() {
    let upstream = MockUpstream::start(
        vec![],
        json!({
            "data": [
                { "id": "deepseek-chat", "owned_by": "mock" },
                { "id": "deepseek-reasoner", "owned_by": "mock" },
            ]
        }),
    );
    let data = TempDataDir::new("llm-models");
    let mut core = Core::spawn(&data.root);
    let provider_id = setup_provider(&mut core, &upstream);

    let listed = core.ok("llm.list_models", json!({ "provider_id": provider_id }));
    let models = listed["models"].as_array().expect("models 是数组");
    let ids: Vec<&str> = models
        .iter()
        .map(|model| model["id"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(ids, vec!["deepseek-chat", "deepseek-reasoner"]);

    // 清单走的是 `/v1/models`，且沿用了同一个鉴权头。
    let seen = upstream
        .requests()
        .into_iter()
        .find(|seen| seen.path.ends_with("/models"))
        .expect("应请求过 /v1/models");
    assert_eq!(seen.method, "GET");
    assert_eq!(
        seen.authorization.as_deref(),
        Some(format!("Bearer {PLAINTEXT_KEY}").as_str())
    );
}

/// 一个确定没人监听的地址：端口 1 落在临时端口范围（49152+）之外，因此不会与
/// 其它用例的假上游抢端口。
///
/// 曾经这里写的是「先 bind 一个临时端口再 drop」——那是自找的并发回归：
/// 放掉的端口会被同一进程里另一个用例的假上游抢到，那个用例的剧本就会被
/// 本用例的请求吃掉（表现为「假上游在，但剧本对不上」这类幽灵失败）。
const UNREACHABLE_BASE_URL: &str = "http://127.0.0.1:1/v1";

#[test]
fn 上下文超限_给出可操作的错误码与原始错误体() {
    // 400 里混着「上下文超限」和「模型名写错」两种完全不同的处境。前者的用户
    // 该做的是裁剪上下文/换长窗口模型，后者该去改 Provider 配置——归错类，
    // 提示语就只能是一句没用的「请求不合法」。
    let upstream = MockUpstream::start(
        vec![Script::Failure {
            status: 400,
            headers: Vec::new(),
            body: json!({
                "error": {
                    "message": "This model's maximum context length is 8192 tokens. However, your messages resulted in 12000 tokens.",
                    "type": "invalid_request_error",
                    "code": "context_length_exceeded",
                }
            })
            .to_string(),
        }],
        json!({ "data": [] }),
    );
    let data = TempDataDir::new("llm-overflow");
    let mut core = Core::spawn(&data.root);
    let provider_id = setup_provider(&mut core, &upstream);

    let (_, accepted) = core.ok_async("llm.chat_stream", chat_params("s-超限", &provider_id));
    assert_eq!(accepted["stream_id"], json!("s-超限"));

    let event = core.wait_notification("llm.error");
    let params = &event["params"];
    assert_eq!(params["stream_id"], json!("s-超限"));
    assert_eq!(
        params["code"],
        json!(2004),
        "CONTEXT_OVERFLOW：UI 该给的是「裁剪上下文」而不是「请求不合法」"
    );
    assert!(
        params["message"]
            .as_str()
            .unwrap_or_default()
            .contains("maximum context length"),
        "上游原话要带出来，用户才知道超了多少：{params}"
    );
    assert_eq!(
        params["upstream"]["error"]["code"],
        json!("context_length_exceeded"),
        "原始错误体要留着，便于自查是哪家网关的哪条限制"
    );
    assert!(
        core.events_named("llm.finished").is_empty(),
        "报错不能伪装成正常结束"
    );
}

#[test]
fn 流中途的上游错误帧_折成同类错误而不是正常收尾() {
    // 上游在首字节之后才报错时发的是 `data: {"error":…}`：没有 choices、
    // 没有 finish_reason。此前这类帧会被静默忽略，整条流表现为「没有内容地
    // 正常结束」——用户看到半截回答配一句「完成」，日志里一无所有。
    let upstream = MockUpstream::start(
        vec![Script::Stream {
            frames: vec![
                delta("前半句"),
                sse(json!({
                    "error": {
                        "type": "rate_limit_exceeded",
                        "message": "Rate limit reached for requests",
                    }
                })),
            ],
        }],
        json!({ "data": [] }),
    );
    let data = TempDataDir::new("llm-mid-error");
    let mut core = Core::spawn(&data.root);
    let provider_id = setup_provider(&mut core, &upstream);

    let (_, accepted) = core.ok_async("llm.chat_stream", chat_params("s-流中错", &provider_id));
    assert_eq!(accepted["stream_id"], json!("s-流中错"));

    // 按到达顺序收集 llm.* 事件（`next_event` 会把事件取走，因此就地留存）。
    let mut seen: Vec<Value> = Vec::new();
    while seen
        .last()
        .map(|event| event["method"] == json!("llm.error"))
        != Some(true)
    {
        let event = next_event(&mut core);
        if event["method"]
            .as_str()
            .unwrap_or_default()
            .starts_with("llm.")
        {
            seen.push(event);
        }
    }

    let methods: Vec<&str> = seen
        .iter()
        .map(|event| event["method"].as_str().unwrap_or_default())
        .collect();
    // 已经吐出来的增量不能被吞掉：用户看到的前半句与报错要对得上。
    assert_eq!(
        methods,
        vec!["llm.started", "llm.delta", "llm.error"],
        "错误帧要终结这条流，而不是让它在沉默中「正常结束」：{}",
        serde_json::to_string(&seen).unwrap_or_default()
    );
    assert_eq!(
        seen[1]["params"]["text"],
        json!("前半句"),
        "报错之前已经下发的增量要原样保留"
    );
    assert_eq!(
        seen[2]["params"]["code"],
        json!(2003),
        "PROVIDER_RATE_LIMITED：流中错误与请求被拒要走同一套分类"
    );
    assert!(
        core.events_named("llm.finished").is_empty(),
        "既报了错就不该再有 finished"
    );
}

#[test]
fn 上游连不上_报网络错误而不是超时() {
    let data = TempDataDir::new("llm-unreachable");
    let mut core = Core::spawn(&data.root);
    // 端口 1 没人监听：连接被拒是「立刻」发生的，所以这条用例不该靠 3 秒超时兜底。
    let provider_id = setup_provider_at(&mut core, UNREACHABLE_BASE_URL);

    let (_, accepted) = core.ok_async("llm.chat_stream", chat_params("s-断网", &provider_id));
    assert_eq!(accepted["stream_id"], json!("s-断网"));

    let event = core.wait_notification("llm.error");
    let params = &event["params"];
    assert_eq!(
        params["code"],
        json!(2005),
        "PROVIDER_UNAVAILABLE：UI 该提示的是「查网络 / 查网关地址」"
    );
    assert!(
        params["message"]
            .as_str()
            .unwrap_or_default()
            .contains("网络错误"),
        "连接被拒要跟「上游超时/5xx」区分开：{params}"
    );
    assert_eq!(params["retry_after_ms"], json!(null));

    // 反例的对称面：清单方法走的是请求-响应，同一个不可达地址也必须报错而不是空清单。
    assert_eq!(
        core.error_code("llm.list_models", json!({ "provider_id": provider_id })),
        2005
    );
}
