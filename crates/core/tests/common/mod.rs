//! core 端到端测试的公共脚手架。
//!
//! `provider_flow.rs`（目录与密钥）和 `llm_flow.rs`（流式对话）都靠它跑真实子进程：
//! 内核当作黑盒，只通过 stdin / stdout 的 NDJSON 报文交互——这样线协议编解码、
//! 分发、事件推送这些真正容易出错的环节才会被覆盖到。
//!
//! 放在 `tests/common/` 下是 Rust 集成测试的约定：它不会被当作一个测试目标收集，
//! 只作为模块被 `mod common;` 引入。
#![allow(dead_code)] // 两个测试目标用到的子集不同，各自编译时不必互相迁就。

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

/// `AI_IDE_TEST_CORE_LOG` 指向目录时，给每个内核日志文件编号（进程内唯一）。
static CORE_LOG_SEQ: AtomicUsize = AtomicUsize::new(0);

/// 单条报文的等待上限。超过即判定为死锁，直接失败而不是挂住 CI。
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// 测试用明文密钥。断言它**不出现在任何响应、事件或落盘文件里**。
pub const PLAINTEXT_KEY: &str = "sk-abcdefghijklmnop";

// ---------------------------------------------------------------------------
// 内核客户端
// ---------------------------------------------------------------------------

pub struct Core {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
    /// 尚未匹配到请求的入站通知（`provider.changed`、`llm.*` 等）。
    pending_events: Vec<Value>,
}

impl Core {
    /// 启动内核，并把它的数据目录指到 `data_dir`。
    pub fn spawn(data_dir: &Path) -> Self {
        // 内核日志走 stderr。默认丢弃，免得污染测试输出；排查间歇性失败时，
        // 用 `AI_IDE_TEST_CORE_LOG=<文件或目录>` 把它落盘——「llm.error 到底报的是
        // 哪一类错误」这种问题，没有内核日志就只能靠反复复现去猜。
        // 给目录时按「测试进程号 + 序号」分文件：一个测试进程会拉起多个内核
        // （用例之间还会并发），共用一个文件会把它们的日志搅在一起。
        let stderr = match std::env::var_os("AI_IDE_TEST_CORE_LOG") {
            Some(path) => {
                let path = PathBuf::from(path);
                let path = if path.is_dir() {
                    let seq = CORE_LOG_SEQ.fetch_add(1, Ordering::Relaxed);
                    path.join(format!("core-{}-{seq}.log", std::process::id()))
                } else {
                    path
                };
                Stdio::from(
                    std::fs::File::create(&path)
                        .unwrap_or_else(|err| panic!("创建内核日志文件失败：{err}")),
                )
            }
            None => Stdio::null(),
        };

        let mut child = Command::new(env!("CARGO_BIN_EXE_ai-ide-core"))
            .env("AI_IDE_DATA_DIR", data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .expect("内核进程启动失败");

        let stdin = child.stdin.take().expect("stdin 可用");
        let stdout = child.stdout.take().expect("stdout 可用");

        // 读线程把 stdout 的行送进 channel，主线程只做「带超时的收」，
        // 这样断言失败时表现为测试失败，而不是整个测试进程卡死。
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            child,
            stdin,
            lines: rx,
            next_id: 1,
            pending_events: Vec::new(),
        }
    }

    pub fn send(&mut self, frame: Value) {
        let mut text = serde_json::to_string(&frame).expect("序列化报文");
        text.push('\n');
        self.stdin.write_all(text.as_bytes()).expect("写入 stdin");
        self.stdin.flush().expect("刷新 stdin");
    }

    /// 发一条通知（没有响应，例如 `$/cancelRequest`）。
    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
    }

    /// 发请求并等到对应 id 的响应；期间收到的通知先攒着。
    pub fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id_string();
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));

        loop {
            let value = self.read_frame(method);
            if value.get("id").and_then(Value::as_str) == Some(id.as_str()) {
                return value;
            }
            self.pending_events.push(value);
        }
    }

    /// 发请求但**不等响应**，返回请求 id——流式接口要边收事件边等响应。
    pub fn call_async(&mut self, method: &str, params: Value) -> String {
        let id = self.next_id_string();
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        id
    }

    /// 等到指定 id 的响应；期间的通知进 `pending_events`。
    pub fn await_response(&mut self, id: &str) -> Value {
        loop {
            let value = self.read_frame(id);
            if value.get("id").and_then(Value::as_str) == Some(id) {
                return value;
            }
            self.pending_events.push(value);
        }
    }

    fn next_id_string(&mut self) -> String {
        let seq = self.next_id;
        self.next_id += 1;
        format!("00000000-0000-4000-8000-{seq:012}")
    }

    fn read_frame(&mut self, waiting_for: &str) -> Value {
        self.try_read_frame(TIMEOUT)
            .unwrap_or_else(|| panic!("等待 {waiting_for} 的响应超时"))
    }

    /// 带超时的收帧；超时返回 `None`（用于断言「之后没有别的事件了」）。
    pub fn try_read_frame(&mut self, timeout: Duration) -> Option<Value> {
        match self.lines.recv_timeout(timeout) {
            Ok(line) => Some(serde_json::from_str(&line).expect("响应必须是合法 JSON")),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => panic!("内核 stdout 已关闭"),
        }
    }

    /// 一直收到指定事件为止；期间的通知进 `pending_events`。
    pub fn wait_notification(&mut self, method: &str) -> Value {
        if let Some(index) = self
            .pending_events
            .iter()
            .position(|event| event["method"] == json!(method))
        {
            return self.pending_events.remove(index);
        }
        loop {
            let value = self.read_frame(method);
            if value["method"] == json!(method) {
                return value;
            }
            self.pending_events.push(value);
        }
    }

    /// 取请求的 `result`；若返回错误则直接失败。
    pub fn ok(&mut self, method: &str, params: Value) -> Value {
        let response = self.call(method, params);
        if let Some(err) = response.get("error") {
            panic!("{method} 期望成功，实际返回错误：{err}");
        }
        response.get("result").cloned().unwrap_or(Value::Null)
    }

    pub fn ok_async(&mut self, method: &str, params: Value) -> (String, Value) {
        let id = self.call_async(method, params);
        let response = self.await_response(&id);
        if let Some(err) = response.get("error") {
            panic!("{method} 期望成功，实际返回错误：{err}");
        }
        (id, response.get("result").cloned().unwrap_or(Value::Null))
    }

    /// 取请求的协议错误码。
    pub fn error_code(&mut self, method: &str, params: Value) -> i64 {
        let response = self.call(method, params);
        response
            .get("error")
            .and_then(|err| err.get("code"))
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("{method} 期望报错，实际返回：{response}"))
    }

    /// 已收到的通知（`provider.changed`、`llm.*` 等）。
    pub fn events(&self) -> &[Value] {
        &self.pending_events
    }

    /// 取走最早的一条待处理通知。
    ///
    /// 流式接口要「按到达顺序」检查事件，而事件可能已经在上一次 `call` 期间
    /// 落进待处理队列（响应与通知谁先到取决于写入时序），因此取事件必须
    /// 先看队列再看 socket，否则会漏掉或等错。
    pub fn pop_event(&mut self) -> Option<Value> {
        if self.pending_events.is_empty() {
            return None;
        }
        Some(self.pending_events.remove(0))
    }

    /// 指定方法名的通知，按到达顺序。
    pub fn events_named<'a>(&'a self, method: &str) -> Vec<&'a Value> {
        self.pending_events
            .iter()
            .filter(|event| event["method"] == json!(method))
            .collect()
    }

    /// 握手，返回能力清单。
    pub fn capabilities(&mut self) -> Vec<String> {
        let hello = self.ok(
            "core.hello",
            json!({
                "client_version": "0.1.0",
                "protocol_version": { "major": 1, "minor": 0 },
                "platform": "test",
                "locale": "zh-CN",
            }),
        );
        hello["capabilities"]
            .as_array()
            .expect("capabilities 是数组")
            .iter()
            .map(|value| value.as_str().unwrap_or_default().to_string())
            .collect()
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        // 尽力优雅退出；退不掉就直接杀，避免残留进程拖住 CI。
        let _ = self.stdin.write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":\"00000000-0000-4000-8000-000000009999\",\"method\":\"core.shutdown\"}\n",
        );
        let _ = self.stdin.flush();
        std::thread::sleep(Duration::from_millis(120));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// 临时数据目录
// ---------------------------------------------------------------------------

/// 一个用完即删的临时数据目录。
pub struct TempDataDir {
    pub root: PathBuf,
}

impl TempDataDir {
    pub fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "ai-ide-m2-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("建数据目录");
        Self { root }
    }

    pub fn store_file(&self) -> PathBuf {
        self.root.join("providers.json")
    }

    /// 落盘目录原文（用于断言「密钥没被写进去」）。
    pub fn store_text(&self) -> String {
        std::fs::read_to_string(self.store_file()).unwrap_or_default()
    }
}

impl Drop for TempDataDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// `provider.upsert` 的常用参数（不带密钥时仅建目录记录）。
pub fn upsert_params(api_key: Option<&str>) -> Value {
    let mut params = json!({
        "name": "本地网关",
        "protocol": "openai_chat_completions",
        "base_url": "https://gw.example.com/v1",
        "headers": { "X-Api-Version": "2024-08-01" },
    });
    if let Some(key) = api_key {
        params["api_key"] = json!(key);
    }
    params
}
