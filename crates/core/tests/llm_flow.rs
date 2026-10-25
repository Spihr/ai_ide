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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ai_ide_llm::tokens::{estimate_text, TokenCounter};
use ai_ide_protocol::ErrorCode;
use serde_json::{json, Value};

mod common;

use common::{Core, TempDataDir, PLAINTEXT_KEY};

const MODEL: &str = "deepseek-chat";
/// 单条事件的等待上限。流式链路卡住时要失败得干脆，而不是把 CI 挂住。
const EVENT_WAIT: Duration = Duration::from_secs(10);

/// 假上游的诊断开关：设了 `AI_IDE_TEST_UPSTREAM_TRACE` 就把 accept / serve 的关键节点
/// 打到 stderr。用例失败时 libtest 会把它捕获的输出一并 dump 出来，这对付
/// 「偶发、跑一次就消失」的失败是必要的——否则只能靠反复复现去猜。
///
/// 平时不打：正常路径不该被调试噪音淹没。
macro_rules! utrace {
    ($($arg:tt)*) => {
        if std::env::var_os("AI_IDE_TEST_UPSTREAM_TRACE").is_some() {
            eprintln!("[mock-upstream] {}", format!($($arg)*));
        }
    };
}

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
                    Ok((stream, peer)) => {
                        utrace!("accept 成功 peer={peer}");
                        let shared = Arc::clone(&accepting);
                        // 取消用例要求连接能长期吊住，所以每连接一个线程。
                        std::thread::spawn(move || match serve(stream, shared) {
                            Ok(()) => utrace!("serve 正常结束 peer={peer}"),
                            Err(err) => utrace!("serve 出错 peer={peer}: {err:?}"),
                        });
                    }
                    Err(err)
                        if matches!(
                            err.kind(),
                            ErrorKind::WouldBlock
                                | ErrorKind::Interrupted
                                | ErrorKind::TimedOut
                                | ErrorKind::ConnectionAborted
                        ) =>
                    {
                        // 队列里的连接被对端提前掐掉时，accept 会直接报错（Windows 上尤其常见）；
                        // 那是某一条连接的事，不该把整个假上游带走。
                        utrace!("accept 跳过一条异常的排队连接: {err:?}");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(err) => {
                        // 静默退出会让「后续连接没人接」变成一堆看不懂的客户端超时，
                        // 所以这里必须留痕。
                        utrace!("accept 循环退出: {err:?}");
                        break;
                    }
                }
            }
            utrace!("accept 线程收工");
        });

        utrace!("假上游监听 http://127.0.0.1:{port}/v1");

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
        utrace!("关闭假上游 {}", self.base_url);
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
    // accept 出来的连接会继承监听套接字的非阻塞状态（Windows 上如此，而 Linux 不继承）。
    // 不显式改回阻塞，第一条 `read` 只要请求还没到就会立刻以 WouldBlock 失败、连接被关掉，
    // 客户端那侧看到的是一句没头没脑的「连接在消息完成前被关闭」——
    // 这正是「偶发、且只在回环上跑得快时才出现」的那类失败的来源。
    //
    // 非阻塞只该属于 accept 轮询，不该跟着连接走：连接上的读写要能等。
    stream.set_nonblocking(false)?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let head = read_head(&mut stream)?;
    utrace!("读到请求头 {} 字节", head.len());

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
        stream.read_exact(&mut raw).inspect_err(|err| {
            utrace!("读请求体失败（期望 {content_length} 字节）: {err:?}");
        })?;
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    utrace!("请求 {method} {path} content_length={content_length}");
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
            utrace!("剧本 Failure status={status}");
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
            utrace!("剧本 Stream 帧数={}", frames.len());
            write_text(&mut stream, SSE_HEAD)?;
            for frame in &frames {
                write_chunk(&mut stream, frame)?;
            }
            // 正常收尾：零长块。少了它，客户端只能看到「流被掐断」。
            write_text(&mut stream, "0\r\n\r\n")
        }
        Script::Stall { frames, hold } => {
            utrace!("剧本 Stall 帧数={} hold={hold:?}", frames.len());
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
            utrace!("请求头读到 EOF，仅 {} 字节", buf.len());
            break;
        }
        buf.push(byte[0]);
        if buf.len() > 64 * 1024 {
            utrace!("请求头超过 64KiB，放弃");
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
            // 把事件整条打出来：断言只报「出现了 llm.error」时，看不出到底是
            // 超时、断流还是上游错误帧，排查只能靠复现。
            other => panic!("流式链路不该出现 {other}：{event}"),
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

// ---------------------------------------------------------------------------
// M3 上下文引用
//
// 这一段的断言对象是**上游收到的请求体**：引用渲染、行号标注、裁剪都发生在
// 「出网之前」，只盯 `llm.*` 事件看不到模型究竟收到了什么。
// ---------------------------------------------------------------------------

/// 一个用完即删的临时工作区。引用渲染要真的读盘、真的列目录，因此不能打桩。
struct TempWorkspace {
    root: PathBuf,
}

impl TempWorkspace {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ai-ide-m3-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src").join("lib")).expect("建目录");
        std::fs::write(root.join("README.md"), "# 演示\n\n第一行\n第二行\n").expect("写 README");
        std::fs::write(
            root.join("src").join("main.ts"),
            "export const main = 1;\nexport const other = 2;\n",
        )
        .expect("写 main.ts");
        std::fs::write(
            root.join("src").join("lib").join("util.ts"),
            "export const util = 3;\n",
        )
        .expect("写 util.ts");
        Self { root }
    }

    fn display(&self) -> String {
        self.root.display().to_string()
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// 打开工作区并取回 id（引用默认落在活动工作区上，显式传 id 更稳）。
fn open_workspace(core: &mut Core, workspace: &TempWorkspace) -> String {
    let opened = core.ok("workspace.open", json!({ "path": workspace.display() }));
    opened["workspace_id"]
        .as_str()
        .expect("workspace_id 是字符串")
        .to_string()
}

/// 带上下文引用的 `llm.chat_stream` 参数。
fn chat_params_with_context(
    stream_id: &str,
    provider_id: &str,
    workspace_id: &str,
    refs: Value,
    compress: Value,
) -> Value {
    let mut params = chat_params(stream_id, provider_id);
    params["session_id"] = json!("sess-ctx");
    params["context"] = json!({
        "workspace_id": workspace_id,
        "refs": refs,
        "compress": compress,
    });
    params
}

/// 把一条流读到 `llm.finished` 为止，返回累计的增量文本。
fn drain_stream(core: &mut Core) -> String {
    let mut text = String::new();
    loop {
        let event = next_event(core);
        match event["method"].as_str().unwrap_or_default() {
            "llm.delta" => text.push_str(event["params"]["text"].as_str().unwrap_or_default()),
            "llm.error" => panic!("流意外报错：{}", event["params"]),
            "llm.finished" => return text,
            _ => {}
        }
    }
}

/// 取上游最近一次请求体里的 `messages`。
fn injected_messages(upstream: &MockUpstream) -> Vec<Value> {
    let seen = upstream.last_chat();
    let body: Value = serde_json::from_str(&seen.body).expect("请求体是 JSON");
    body["messages"]
        .as_array()
        .expect("messages 是数组")
        .clone()
}

#[test]
fn 上下文引用_渲染成前置系统消息且顺序与来源标注齐全() {
    let upstream = MockUpstream::start(
        vec![Script::Stream {
            frames: vec![
                delta("收到"),
                sse(json!({
                    "id": "chatcmpl-ctx",
                    "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                })),
            ],
        }],
        json!({ "data": [] }),
    );
    let data = TempDataDir::new("llm-context");
    let workspace = TempWorkspace::new("inject");
    let mut core = Core::spawn(&data.root);
    let provider_id = setup_provider(&mut core, &upstream);
    let workspace_id = open_workspace(&mut core, &workspace);

    let refs = json!([
        { "kind": "file", "path": "src/main.ts" },
        { "kind": "folder", "path": "src" },
        { "kind": "selection", "path": "README.md", "start_line": 3, "end_line": 4 },
    ]);
    let (_, accepted) = core.ok_async(
        "llm.chat_stream",
        chat_params_with_context("s-引用", &provider_id, &workspace_id, refs, Value::Null),
    );
    assert_eq!(accepted["stream_id"], json!("s-引用"));

    // 引用要在「发请求之前」就位，否则上游收到的就是一份没有引用的提问。
    assert_eq!(drain_stream(&mut core), "收到");

    let messages = injected_messages(&upstream);
    assert_eq!(
        messages.len(),
        2,
        "引用应合并成一条 system 消息，而不是给每条引用各加一条：{messages:?}"
    );
    assert_eq!(messages[0]["role"], json!("system"), "引用必须是系统消息");
    assert_eq!(
        messages[1]["content"],
        json!("你好"),
        "用户消息要原样保留，并排在引用之后"
    );

    let injected = messages[0]["content"]
        .as_str()
        .expect("system 内容是字符串");
    let file_at = injected
        .find("<file path=\"src/main.ts\"")
        .expect("文件引用要带来源标注");
    let folder_at = injected
        .find("<folder path=\"src\"")
        .expect("目录引用要带来源标注");
    let selection_at = injected
        .find("<selection path=\"README.md\" lines=\"3-4\">")
        .expect("选区要带行区间标注");
    assert!(
        file_at < folder_at && folder_at < selection_at,
        "注入顺序必须等于 refs 顺序：{injected}"
    );

    assert!(
        injected.contains("1| export const main = 1;"),
        "注入内容要带行号，模型才能引用具体位置：{injected}"
    );
    assert!(
        injected.contains("  util.ts"),
        "目录树要含子目录条目并缩进：{injected}"
    );
    assert!(
        injected.contains("3| 第一行") && injected.contains("4| 第二行"),
        "选区要注入请求的行区间：{injected}"
    );
    assert!(
        !injected.contains("# 演示"),
        "选区外的行不该被注入：{injected}"
    );
}

#[test]
fn 上下文引用_越界路径在派发前就被拒() {
    let upstream = MockUpstream::start(vec![], json!({ "data": [] }));
    let data = TempDataDir::new("llm-context-escape");
    let workspace = TempWorkspace::new("escape");
    let mut core = Core::spawn(&data.root);
    let provider_id = setup_provider(&mut core, &upstream);
    let workspace_id = open_workspace(&mut core, &workspace);

    // 越界不走「读不到就标注」那条容错路径：路径闸门是安全边界。
    let code = core.error_code(
        "llm.chat_stream",
        chat_params_with_context(
            "s-越界",
            &provider_id,
            &workspace_id,
            json!([{ "kind": "file", "path": "../outside.txt" }]),
            Value::Null,
        ),
    );
    assert_eq!(
        code,
        i64::from(ErrorCode::PATH_OUT_OF_WORKSPACE.as_i32()),
        "越界必须是路径错误码，而不是笼统的「请求不合法」"
    );

    // 被拒的请求既不该先回 `{stream_id}`，也不该把内容发到上游。
    assert!(
        core.events_named("llm.started").is_empty(),
        "渲染失败的请求不该先发 started：{:?}",
        core.events()
    );
    assert!(
        upstream
            .requests()
            .iter()
            .all(|seen| !seen.path.ends_with("/chat/completions")),
        "越界引用不该出网"
    );
}

#[test]
fn 上下文引用_预算不足时裁剪并留下裁剪说明() {
    let upstream = MockUpstream::start(
        vec![Script::Stream {
            frames: vec![
                delta("好"),
                sse(json!({
                    "id": "chatcmpl-budget",
                    "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                })),
            ],
        }],
        json!({ "data": [] }),
    );
    let data = TempDataDir::new("llm-context-budget");
    let workspace = TempWorkspace::new("budget");
    let mut core = Core::spawn(&data.root);
    let provider_id = setup_provider(&mut core, &upstream);
    let workspace_id = open_workspace(&mut core, &workspace);

    let mut big = String::new();
    for line in 0..800 {
        big.push_str(&format!("export const value_{line} = {line};\n"));
    }
    std::fs::write(workspace.root.join("big.ts"), big).expect("写大文件");

    let budget = 200u64;
    let (_, accepted) = core.ok_async(
        "llm.chat_stream",
        chat_params_with_context(
            "s-预算",
            &provider_id,
            &workspace_id,
            json!([{ "kind": "file", "path": "big.ts" }]),
            json!({ "enabled": true, "strategy": "truncate", "budget_tokens": budget }),
        ),
    );
    assert_eq!(accepted["stream_id"], json!("s-预算"));
    // 裁剪只影响引用内容，链路本身照常走完。
    assert_eq!(drain_stream(&mut core), "好");

    let messages = injected_messages(&upstream);
    let injected = messages[0]["content"]
        .as_str()
        .expect("system 内容是字符串");
    assert!(
        estimate_text(injected) <= budget,
        "注入内容必须落在预算内：{} > {budget}",
        estimate_text(injected)
    );
    assert!(
        injected.contains("因 token 预算不足"),
        "被裁掉的内容要有说明，否则模型会以为它看到了全文：{injected}"
    );
    assert!(
        injected.contains("value_0 = 0;"),
        "预算内的开头部分要留下：{injected}"
    );
    assert!(
        !injected.contains("value_700"),
        "预算之外的内容不该混进来：{injected}"
    );
}

#[test]
fn 上下文引用_summarize_策略按摘要裁剪而非截断() {
    let upstream = MockUpstream::start(
        vec![Script::Stream {
            frames: vec![
                delta("好"),
                sse(json!({
                    "id": "chatcmpl-summarize",
                    "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                })),
            ],
        }],
        json!({ "data": [] }),
    );
    let data = TempDataDir::new("llm-context-summarize");
    let workspace = TempWorkspace::new("summarize");
    let mut core = Core::spawn(&data.root);
    let provider_id = setup_provider(&mut core, &upstream);
    let workspace_id = open_workspace(&mut core, &workspace);

    let mut big = String::new();
    for line in 0..800 {
        big.push_str(&format!("export const value_{line} = {line};\n"));
    }
    std::fs::write(workspace.root.join("big.ts"), big).expect("写大文件");

    let budget = 200u64;
    let (_, accepted) = core.ok_async(
        "llm.chat_stream",
        chat_params_with_context(
            "s-摘要",
            &provider_id,
            &workspace_id,
            json!([{ "kind": "file", "path": "big.ts" }]),
            json!({ "enabled": true, "strategy": "summarize", "budget_tokens": budget }),
        ),
    );
    assert_eq!(accepted["stream_id"], json!("s-摘要"));
    assert_eq!(drain_stream(&mut core), "好");

    let messages = injected_messages(&upstream);
    let injected = messages[0]["content"]
        .as_str()
        .expect("system 内容是字符串");

    assert!(
        estimate_text(injected) <= budget,
        "摘要后同样必须落在预算内：{} > {budget}",
        estimate_text(injected)
    );
    assert!(
        injected.contains("<file path=\"big.ts\" lines=\"1-800\">"),
        "段头要留住：{injected}"
    );
    assert!(
        injected.contains("</file>"),
        "摘要是完整结构，闭合标签必须在：{injected}"
    );
    assert!(
        injected.contains("<!-- 摘要：原文正文共 800 行"),
        "摘要要写明原文规模与省略行数：{injected}"
    );
    assert!(
        injected.contains("value_0 = 0;") && injected.contains("value_799 = 799;"),
        "摘要要同时留住开头与结尾：{injected}"
    );
    assert!(
        !injected.contains("value_400"),
        "中间内容确实被摘要掉了：{injected}"
    );
    assert!(
        injected.contains("因 token 预算不足"),
        "被摘要的引用没完整进来，仍要出说明行：{injected}"
    );
}

/// 预算小到连摘要都摆不下时，内核按保底值处理并如实说明——**不静默丢内容**。
/// 此时注入正文允许略超调用方给的数字：把「有东西没进来」说清楚比死守一个
/// 小到放不下任何东西的预算更重要。
#[test]
fn 上下文引用_极小预算也不静默丢弃() {
    let upstream = MockUpstream::start(
        vec![Script::Stream {
            frames: vec![
                delta("好"),
                sse(json!({
                    "id": "chatcmpl-tiny",
                    "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                })),
            ],
        }],
        json!({ "data": [] }),
    );
    let data = TempDataDir::new("llm-context-tiny");
    let workspace = TempWorkspace::new("tiny");
    let mut core = Core::spawn(&data.root);
    let provider_id = setup_provider(&mut core, &upstream);
    let workspace_id = open_workspace(&mut core, &workspace);

    std::fs::write(workspace.root.join("big.ts"), "x\n".repeat(400)).expect("写文件");

    let (_, accepted) = core.ok_async(
        "llm.chat_stream",
        chat_params_with_context(
            "s-极小",
            &provider_id,
            &workspace_id,
            json!([{ "kind": "file", "path": "big.ts" }]),
            json!({ "enabled": true, "strategy": "truncate", "budget_tokens": 1 }),
        ),
    );
    assert_eq!(accepted["stream_id"], json!("s-极小"));
    assert_eq!(drain_stream(&mut core), "好");

    let messages = injected_messages(&upstream);
    assert_eq!(messages.len(), 2, "引用消息仍要插进去：{messages:?}");
    let injected = messages[0]["content"]
        .as_str()
        .expect("system 内容是字符串");

    assert!(
        injected.contains("因 token 预算不足"),
        "预算再小也要留下说明行，不能静默：{injected}"
    );
    assert!(!injected.trim().is_empty(), "抬头与说明行兜住了最坏情况");
}

#[test]
fn token_估算_合并_messages_与_texts_两个口径() {
    // 估算不查注册表：网关挂了，输入框的计数也该照常工作。
    let data = TempDataDir::new("llm-count");
    let mut core = Core::spawn(&data.root);
    let model_ref = json!({ "provider_id": "p-未注册", "model_id": MODEL });

    let plain = core.ok(
        "llm.count_tokens",
        json!({
            "model_ref": model_ref,
            "messages": [{ "role": "user", "content": "你好世界" }],
        }),
    );
    assert_eq!(
        plain["exact"],
        json!(false),
        "deepseek-chat 不在已接词表族内，回落估算，不能谎报精确"
    );
    assert!(
        plain["note"].is_string(),
        "口径说明要带出去，UI 才能提示这是估算值：{plain}"
    );
    let plain_tokens = plain["tokens"].as_u64().expect("tokens 是数字");
    assert!(plain_tokens > 0, "非空消息不该估出 0");

    // `texts` 是输入框里还没拼成消息的草稿/引用片段：与 messages 相加，不互相覆盖。
    let draft = "export const draft = 1;\n".repeat(10);
    let mixed = core.ok(
        "llm.count_tokens",
        json!({
            "model_ref": model_ref,
            "messages": [{ "role": "user", "content": "你好世界" }],
            "texts": [draft],
        }),
    );
    let mixed_tokens = mixed["tokens"].as_u64().expect("tokens 是数字");
    assert_eq!(
        mixed_tokens,
        plain_tokens + estimate_text(&draft),
        "两种口径要相加，且与内核估算器同口径"
    );

    let empty = core.ok("llm.count_tokens", json!({ "model_ref": model_ref }));
    assert_eq!(
        empty["tokens"],
        json!(0),
        "两个口径都缺省时回 0 而不是报错：输入框每敲一个字都会调它"
    );
}

#[test]
fn token_精确计数_认识模型族走分词器_其余回落估算() {
    // 计数不查注册表：Provider 没注册、上游没配，输入框的计数照常工作。
    let data = TempDataDir::new("llm-count-exact");
    let mut core = Core::spawn(&data.root);

    // 1) o200k_base 族：gpt-4o 走真实词表，与内核分词器同值。
    let o200k = core.ok(
        "llm.count_tokens",
        json!({
            "model_ref": { "provider_id": "p-未注册", "model_id": "gpt-4o" },
            "texts": ["你好世界"],
        }),
    );
    assert_eq!(o200k["exact"], json!(true), "gpt-4o 应走精确口径：{o200k}");
    assert_eq!(
        o200k["tokens"],
        json!(TokenCounter::for_model("gpt-4o").count_text("你好世界"))
    );
    let note = o200k["note"].as_str().expect("精确口径也要带口径说明");
    assert!(
        note.contains("o200k_base"),
        "note 要点明用的是哪套词表：{note}"
    );

    // 2) cl100k_base 族：gpt-4-turbo。
    let cl100k = core.ok(
        "llm.count_tokens",
        json!({
            "model_ref": { "provider_id": "p-未注册", "model_id": "gpt-4-turbo" },
            "texts": ["你好世界"],
        }),
    );
    assert_eq!(cl100k["exact"], json!(true));
    assert_eq!(
        cl100k["tokens"],
        json!(TokenCounter::for_model("gpt-4-turbo").count_text("你好世界"))
    );
    assert!(
        cl100k["note"]
            .as_str()
            .unwrap_or_default()
            .contains("cl100k_base"),
        "{cl100k}"
    );

    // 3) 未识别模型族：回落估算，且与既有估算器逐值一致（口径没被悄悄换掉）。
    let estimated = core.ok(
        "llm.count_tokens",
        json!({
            "model_ref": { "provider_id": "p-未注册", "model_id": "deepseek-chat" },
            "texts": ["你好世界"],
        }),
    );
    assert_eq!(estimated["exact"], json!(false));
    assert_eq!(estimated["tokens"], json!(estimate_text("你好世界")));

    // 4) 精确口径下 messages 与 texts 仍相加，且结构开销与估算口径一致（每消息 +4）。
    let counter = TokenCounter::for_model("gpt-4o");
    let mixed = core.ok(
        "llm.count_tokens",
        json!({
            "model_ref": { "provider_id": "p-未注册", "model_id": "gpt-4o" },
            "messages": [{ "role": "user", "content": "abcd" }],
            "texts": ["export const draft = 1;\n"],
        }),
    );
    assert_eq!(
        mixed["tokens"],
        json!(4 + counter.count_text("abcd") + counter.count_text("export const draft = 1;\n")),
        "两个口径相加，结构开销沿用每消息 +4：{mixed}"
    );
}

// ---------------------------------------------------------------------------
// M3 切片三：inline_edit.propose
//
// 它和对话面共用同一个假上游，但**不该**共用同一套对外语义：不推 `llm.*` 事件、
// 不占 `stream_id`、不写盘。用例除了看替换结果，就盯这三条边界——它们才是
// 「编辑器里的一个动作」与「一轮对话」的分水岭。
// ---------------------------------------------------------------------------

/// `inline_edit.propose` 的常用参数：在 `src/main.ts` 第 1 行选中 `main` 这四个字符。
///
/// 列号按 0 起算右开（`export const ` 恰好 13 个字符），与 Monaco 的选区口径一致。
fn propose_params(workspace_id: &str, provider_id: &str) -> Value {
    json!({
        "workspace_id": workspace_id,
        "path": "src/main.ts",
        "instruction": "把 main 改名为 entry",
        "selection": {
            "start_line": 1,
            "start_column": 13,
            "end_line": 1,
            "end_column": 17,
        },
        "context_lines": 2,
        "model_ref": { "provider_id": provider_id, "model_id": MODEL },
    })
}

#[test]
fn inline_edit_端到端_返回替换与diff且不写盘不推事件() {
    let upstream = MockUpstream::start(
        vec![Script::Stream {
            frames: vec![
                // 选区只有 `main` 四个字符，所以模型只需回选区内的替换文本；
                // 它照系统提示词用围栏包住，且围栏与正文被切在两个增量里。
                delta("```ts\n"),
                delta("entry"),
                delta("\n```"),
                sse(json!({
                    "id": "chatcmpl-ie",
                    "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                })),
                sse(json!({
                    "id": "chatcmpl-ie",
                    "choices": [],
                    "usage": { "prompt_tokens": 31, "completion_tokens": 7, "total_tokens": 38 },
                })),
            ],
        }],
        json!({ "data": [] }),
    );
    let data = TempDataDir::new("inline-edit-ok");
    let workspace = TempWorkspace::new("inline-edit-ok");
    let mut core = Core::spawn(&data.root);
    let provider_id = setup_provider(&mut core, &upstream);
    let workspace_id = open_workspace(&mut core, &workspace);

    let result = core.ok(
        "inline_edit.propose",
        propose_params(&workspace_id, &provider_id),
    );

    assert_eq!(result["path"], json!("src/main.ts"), "路径要原样回显");
    assert_eq!(
        result["range"],
        json!({
            "start_line": 1, "start_column": 13, "end_line": 1, "end_column": 17
        }),
        "被替换的选区必须与请求一致，越界不会被悄悄收拢"
    );
    assert_eq!(
        result["original"],
        json!("main"),
        "原文要按选区精确切出来，不含上下文"
    );
    assert_eq!(
        result["replacement"],
        json!("entry"),
        "替换文本只管选区内那段，围栏与行内其余字符都不该被模型改写"
    );

    let diff = result["diff"].as_str().expect("diff 是字符串");
    assert!(diff.contains("--- a/src/main.ts"), "{diff}");
    assert!(diff.contains("+++ b/src/main.ts"), "{diff}");
    assert!(diff.contains("-export const main = 1;"), "{diff}");
    assert!(
        diff.contains("+export const entry = 1;"),
        "替换要嵌回原行的列位置，而不是整行覆盖：{diff}"
    );
    assert_eq!(
        diff.matches("@@ -").count(),
        1,
        "只有一行变了，不该出现第二个 hunk：{diff}"
    );

    assert_eq!(result["usage"]["prompt_tokens"], json!(31));
    assert_eq!(result["usage"]["completion_tokens"], json!(7));
    assert_eq!(
        result["usage"]["cached_tokens"],
        json!(null),
        "上游没报缓存命中就是 null，不是 0"
    );

    // 边界 1：内核不写盘。落盘要由外壳带 expected_hash 走 workspace.write_file。
    let on_disk =
        std::fs::read_to_string(workspace.root.join("src").join("main.ts")).expect("读回原文件");
    assert_eq!(
        on_disk, "export const main = 1;\nexport const other = 2;\n",
        "propose 只描述「应该改成什么」，不该动磁盘"
    );

    // 边界 2：不进会话面。没有 llm.delta / llm.finished，也不占 stream_id。
    assert!(
        core.events().iter().all(|event| !event["method"]
            .as_str()
            .unwrap_or_default()
            .starts_with("llm.")),
        "Inline Edit 不该推对话事件：{:?}",
        core.events()
    );

    // 出网前就该带上选区标记与行号，否则模型只能靠猜自己改哪几行。
    let seen = upstream.last_chat();
    let body: Value = serde_json::from_str(&seen.body).expect("请求体是 JSON");
    assert_eq!(body["model"], json!(MODEL));
    assert_eq!(
        body["temperature"],
        json!(0.2),
        "改写要照指令改，不该带上创作的随机性"
    );
    let messages = injected_messages(&upstream);
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert_eq!(messages[0]["role"], json!("system"));
    let prompt = messages[1]["content"].as_str().expect("用户消息是字符串");
    assert!(prompt.contains("<<<SELECTION>>>"), "{prompt}");
    assert!(prompt.contains("<<<END SELECTION>>>"), "{prompt}");
    assert!(prompt.contains("1 | export const main = 1;"), "{prompt}");
    assert!(
        prompt.contains("2 | export const other = 2;"),
        "context_lines=2 要把下一行也带上：{prompt}"
    );
    assert!(prompt.contains("把 main 改名为 entry"), "{prompt}");
    assert!(prompt.contains("语言：TypeScript"), "{prompt}");
}

#[test]
fn inline_edit_选区越界报9001且不打上游() {
    let upstream = MockUpstream::start(vec![], json!({ "data": [] }));
    let data = TempDataDir::new("inline-edit-range");
    let workspace = TempWorkspace::new("inline-edit-range");
    let mut core = Core::spawn(&data.root);
    let provider_id = setup_provider(&mut core, &upstream);
    let workspace_id = open_workspace(&mut core, &workspace);

    let mut params = propose_params(&workspace_id, &provider_id);
    // main.ts 只有 3 行（末尾换行会 split 出一个空行），选到第 9 行属于画错了。
    params["selection"] = json!({
        "start_line": 9,
        "start_column": 0,
        "end_line": 9,
        "end_column": 1,
    });

    let response = core.call("inline_edit.propose", params);
    let error = response.get("error").expect("越界必须报错");
    assert_eq!(error["code"], json!(9001), "选区越界走 9001：{error}");
    assert_ne!(
        error["code"],
        json!(1002),
        "「选区画错了」与「报文不合协议」必须分开"
    );
    assert_eq!(
        error["data"]["lines"],
        json!(3),
        "带上文件总行数，外壳才能把选区收拢到合法范围：{error}"
    );
    assert_eq!(
        error["data"]["selection"]["start_line"],
        json!(9),
        "回显原始选区，避免用户以为是自己手抖改的值"
    );
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("越界"),
        "{error}"
    );
    assert!(
        upstream.requests().is_empty(),
        "选区都不合法就不该打上游——白花一次 token"
    );
}

#[test]
fn inline_edit_上游4xx折成provider错误码而非9003() {
    let upstream = MockUpstream::start(
        vec![Script::Failure {
            status: 429,
            headers: vec![("retry-after".to_string(), "3".to_string())],
            body: json!({ "error": { "message": "rate limit exceeded" } }).to_string(),
        }],
        json!({ "data": [] }),
    );
    let data = TempDataDir::new("inline-edit-4xx");
    let workspace = TempWorkspace::new("inline-edit-4xx");
    let mut core = Core::spawn(&data.root);
    let provider_id = setup_provider(&mut core, &upstream);
    let workspace_id = open_workspace(&mut core, &workspace);

    let code = core.error_code(
        "inline_edit.propose",
        propose_params(&workspace_id, &provider_id),
    );
    assert_eq!(
        code,
        ErrorCode::PROVIDER_RATE_LIMITED.as_i32() as i64,
        "上游限流沿用 2xxx 段：9003 是「模型没给代码」，不是「上游没答应」"
    );
    assert_eq!(
        upstream.requests().len(),
        1,
        "确实打到了上游，失败不是因为请求没发出去"
    );
}

#[test]
fn inline_edit_模型只回解释时报9003而不是空替换() {
    let upstream = MockUpstream::start(
        vec![Script::Stream {
            frames: vec![
                delta("这段代码看起来没问题，不需要修改。"),
                sse(json!({
                    "id": "chatcmpl-ie-empty",
                    "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                })),
            ],
        }],
        json!({ "data": [] }),
    );
    let data = TempDataDir::new("inline-edit-empty");
    let workspace = TempWorkspace::new("inline-edit-empty");
    let mut core = Core::spawn(&data.root);
    let provider_id = setup_provider(&mut core, &upstream);
    let workspace_id = open_workspace(&mut core, &workspace);

    let response = core.call(
        "inline_edit.propose",
        propose_params(&workspace_id, &provider_id),
    );
    let error = response.get("error").expect("提不出代码必须报错");
    assert_eq!(error["code"], json!(9003), "{error}");
    assert_eq!(
        error["data"]["response_chars"],
        json!("这段代码看起来没问题，不需要修改。".chars().count()),
        "把上游原话字数带出去，便于判断是空响应还是只有解释：{error}"
    );
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("提取不出代码"),
        "{error}"
    );
}
