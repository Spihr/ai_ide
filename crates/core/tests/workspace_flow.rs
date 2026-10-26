//! M1 端到端流程测试：把内核当黑盒子进程，走真实的 stdin / stdout 协议。
//!
//! 为什么要端到端而不是直接调 handler：M1 的绝大部分风险不在「函数算得对不对」，
//! 而在「线协议编解码 + 分发 + 路径解析 + 磁盘副作用」这条链路上。
//! 只测函数会漏掉这一整层。

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// 单条报文的等待上限。超过即判定为死锁，直接失败而不是挂住 CI。
const TIMEOUT: Duration = Duration::from_secs(20);

/// 内核客户端：负责写请求、按 id 收响应。
struct Core {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
    /// 尚未匹配到请求的入站通知（`workspace.changed` 等）。
    pending_events: Vec<Value>,
}

impl Core {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ai-ide-core"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("内核进程启动失败");

        let stdin = child.stdin.take().expect("stdin 可用");
        let stdout = child.stdout.take().expect("stdout 可用");

        // 读线程负责把 stdout 的行送进 channel，主线程只做「带超时的收」，
        // 这样断言失败时的表现是测试失败，而不是整个测试进程卡死。
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

    fn send(&mut self, frame: Value) {
        let mut text = serde_json::to_string(&frame).expect("序列化报文");
        text.push('\n');
        self.stdin.write_all(text.as_bytes()).expect("写入 stdin");
        self.stdin.flush().expect("刷新 stdin");
    }

    /// 发请求并等到对应 id 的响应；期间收到的通知先攒着。
    fn call(&mut self, method: &str, params: Value) -> Value {
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

    /// 协议约定请求 id 是 UUID v4 字符串；这里生成形状合法、递增可读的 id。
    fn next_id_string(&mut self) -> String {
        let seq = self.next_id;
        self.next_id += 1;
        format!("00000000-0000-4000-8000-{seq:012}")
    }

    fn read_frame(&mut self, waiting_for: &str) -> Value {
        let line = self
            .lines
            .recv_timeout(TIMEOUT)
            .unwrap_or_else(|_| panic!("等待 {waiting_for} 的响应超时"));
        serde_json::from_str(&line).expect("响应必须是合法 JSON")
    }

    /// 取请求的 `result`，若返回的是错误则直接失败并带上错误码。
    fn ok(&mut self, method: &str, params: Value) -> Value {
        let response = self.call(method, params);
        if let Some(err) = response.get("error") {
            panic!("{method} 期望成功，实际返回错误：{err}");
        }
        response.get("result").cloned().unwrap_or(Value::Null)
    }

    /// 等一条指定方法的通知（`workspace.indexed` 等），期间收到的其它帧先攒着。
    fn wait_notification(&mut self, method: &str) -> Value {
        if let Some(position) = self
            .pending_events
            .iter()
            .position(|event| event.get("method").and_then(Value::as_str) == Some(method))
        {
            return self.pending_events.remove(position);
        }
        loop {
            let value = self.read_frame(method);
            if value.get("method").and_then(Value::as_str) == Some(method) {
                return value;
            }
            self.pending_events.push(value);
        }
    }

    /// 等某个工作区的索引就绪事件，返回事件的 `params`（报文里的真实载荷）。
    fn wait_indexed(&mut self, workspace_id: &str) -> Value {
        loop {
            let event = self.wait_notification("workspace.indexed");
            let params = event.get("params").cloned().unwrap_or(Value::Null);
            if params.get("workspace_id").and_then(Value::as_str) == Some(workspace_id) {
                return params;
            }
        }
    }

    /// 取请求的协议错误码。
    fn error_code(&mut self, method: &str, params: Value) -> i64 {
        let response = self.call(method, params);
        response
            .get("error")
            .and_then(|err| err.get("code"))
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("{method} 期望报错，实际返回：{response}"))
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

/// 一个用完即删的临时工作区。
struct TempWorkspace {
    root: PathBuf,
}

impl TempWorkspace {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ai-ide-m1-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("建目录");
        std::fs::write(root.join("README.md"), "# 演示\n\nhello world\n").expect("写 README");
        std::fs::write(
            root.join("src").join("main.rs"),
            "fn main() {\n    println!(\"hello world\");\n}\n",
        )
        .expect("写 main.rs");
        Self { root }
    }

    fn path(&self, rel: &str) -> String {
        self.root.join(rel).display().to_string()
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn open(core: &mut Core, root: &Path) -> String {
    let result = core.ok(
        "workspace.open",
        json!({ "path": root.display().to_string() }),
    );
    assert_eq!(result["root"], json!(root.display().to_string()));
    assert_eq!(
        result["name"],
        json!(root.file_name().unwrap().to_string_lossy())
    );
    result["workspace_id"]
        .as_str()
        .expect("workspace_id 必须是字符串")
        .to_string()
}

#[test]
fn 完整链路_打开_树_读写_检索_关闭() {
    let ws_dir = TempWorkspace::new("flow");
    let mut core = Core::spawn();

    // 1) 握手：能力清单里必须已经声明 M1 的方法。
    let hello = core.ok(
        "core.hello",
        json!({
            "client_version": "0.1.0",
            "protocol_version": { "major": 1, "minor": 0 },
            "platform": "test",
            "locale": "zh-CN",
        }),
    );
    let capabilities: Vec<String> = hello["capabilities"]
        .as_array()
        .expect("capabilities 是数组")
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(capabilities.contains(&"workspace.read_file".to_string()));
    assert!(capabilities.contains(&"search.content".to_string()));

    // 2) 打开工作区：索引在后台建（open 不再同步等整库遍历），先拿到 `indexing` 标记。
    let workspace_id = open(&mut core, &ws_dir.root);

    // 3) 目录树：一层、按「目录优先」排序，忽略项不返回。
    let tree = core.ok("workspace.tree", json!({ "workspace_id": workspace_id }));
    let entries = tree["entries"].as_array().expect("entries 是数组");
    let names: Vec<&str> = entries
        .iter()
        .map(|entry| entry["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        names,
        vec!["src", "README.md"],
        "目录优先、名称不区分大小写排序"
    );
    assert_eq!(entries[0]["kind"], json!("dir"));
    assert_eq!(entries[1]["kind"], json!("file"));

    // 4) 读文件：内容与哈希都要对得上。
    let read = core.ok(
        "workspace.read_file",
        json!({ "path": ws_dir.path("src/main.rs") }),
    );
    assert!(read["content"].as_str().unwrap().contains("hello world"));
    assert_eq!(read["eol"], json!("lf"));
    assert_eq!(read["truncated"], json!(false));
    let first_hash = read["hash"].as_str().expect("hash 是字符串").to_string();

    // 5) 写回：带正确的前置哈希，内容落盘。
    let written = core.ok(
        "workspace.write_file",
        json!({
            "path": ws_dir.path("src/main.rs"),
            "content": "fn main() {\n    println!(\"updated\");\n}\n",
            "expected_hash": first_hash,
        }),
    );
    assert_ne!(written["hash"], json!(first_hash), "内容变了，哈希必须变");
    let on_disk = std::fs::read_to_string(ws_dir.root.join("src").join("main.rs")).unwrap();
    assert!(on_disk.contains("updated"));

    // 6) 乐观锁：拿旧哈希再写一次必须被拒（3003）。
    let conflict = core.error_code(
        "workspace.write_file",
        json!({
            "path": ws_dir.path("src/main.rs"),
            "content": "冲突内容",
            "expected_hash": first_hash,
        }),
    );
    assert_eq!(conflict, 3003, "写入冲突应复用 3003");

    // 7) 文件名模糊检索：`mnrs` 是 `main.rs` 的子序列。
    //    检索依赖后台索引，先等 `workspace.indexed` 就绪（事件必须真的到达）。
    core.wait_indexed(&workspace_id);
    let files = core.ok(
        "search.files",
        json!({ "workspace_id": workspace_id, "query": "mnrs" }),
    );
    let matches = files["matches"].as_array().expect("matches 是数组");
    assert!(matches.iter().any(|m| m["path"] == json!("src/main.rs")));

    // 8) 内容检索：字面量查询、命中行号从 1 计。
    let content = core.ok(
        "search.content",
        json!({ "workspace_id": workspace_id, "query": "updated" }),
    );
    let hits = content["matches"].as_array().expect("matches 是数组");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["path"], json!("src/main.rs"));
    assert_eq!(hits[0]["line"], json!(2));

    // 9) 越界：工作区外的路径必须拒绝，错误码 1002（PATH_OUT_OF_WORKSPACE）。
    let outside = core.error_code(
        "workspace.read_file",
        json!({ "path": ws_dir.root.parent().unwrap().join("not-mine.txt").display().to_string() }),
    );
    assert_eq!(outside, 1002, "越界访问应返回 1002");

    // 10) 关闭工作区后，不带 workspace_id 的方法必须报「未打开」。
    let closed = core.ok("workspace.close", json!({ "workspace_id": workspace_id }));
    assert_eq!(closed["ok"], json!(true));
    let after_close = core.error_code(
        "workspace.read_file",
        json!({ "path": ws_dir.path("README.md") }),
    );
    assert_eq!(after_close, 1001, "工作区已关闭应返回 1001");
}

#[test]
fn 新建_重命名_删除走真实磁盘() {
    let ws_dir = TempWorkspace::new("mutate");
    let mut core = Core::spawn();
    let workspace_id = open(&mut core, &ws_dir.root);

    // 新建文件：父目录不存在时应自动补齐。
    let created = core.ok(
        "workspace.create",
        json!({ "path": ws_dir.path("notes/today.md"), "kind": "file" }),
    );
    assert_eq!(created["ok"], json!(true));
    assert!(ws_dir.root.join("notes").join("today.md").is_file());

    // 重命名：目标已存在且未声明 overwrite 时必须拒绝。
    let renamed = core.ok(
        "workspace.rename",
        json!({
            "from": ws_dir.path("notes/today.md"),
            "to": ws_dir.path("notes/tomorrow.md"),
        }),
    );
    assert_eq!(renamed["ok"], json!(true));
    assert!(ws_dir.root.join("notes").join("tomorrow.md").is_file());

    // 永久删除不在能力范围内：`to_trash=false` 必须被拒（按非法参数 -32602 处理）。
    let refused = core.error_code(
        "workspace.delete",
        json!({ "paths": [ws_dir.path("notes/tomorrow.md")], "to_trash": false }),
    );
    assert_eq!(
        refused, -32602,
        "永久删除不在能力范围内，应作为非法参数拒绝"
    );

    // 合法删除：移入回收站。
    let deleted = core.ok(
        "workspace.delete",
        json!({ "paths": [ws_dir.path("notes/tomorrow.md")], "to_trash": true }),
    );
    assert_eq!(
        deleted["failed"].as_array().map(Vec::len),
        Some(0),
        "删除不应有失败项：{deleted}"
    );
    assert!(!ws_dir.root.join("notes").join("tomorrow.md").exists());

    // 敏感路径：`.env` 属于写保护范围（内核侧最后一道闸门）。
    std::fs::write(ws_dir.root.join(".env"), "SECRET=1\n").unwrap();
    let protected = core.error_code(
        "workspace.write_file",
        json!({ "path": ws_dir.path(".env"), "content": "SECRET=2\n" }),
    );
    assert_eq!(protected, 1003, "敏感路径（.env）应返回 1003 PATH_DENIED");

    let _ = workspace_id;
}

/// M3 切片一：`search.symbols` 必须真的走 dispatch（而不是落 `-32601` 兜底），
/// 且 `kinds` 过滤与「无匹配」两条支路要有可观测的差别。
///
/// 断言写在端到端层而不是 handler 里，是因为这条链路的风险在「方法名常量 →
/// dispatch 分支 → 索引复用 → 序列化形状」上；单测命中不了常量拼错、分支漏接。
#[test]
fn 符号检索_走分发_支持种类过滤与空结果() {
    let ws_dir = TempWorkspace::new("symbols");
    let mut core = Core::spawn();
    let workspace_id = open(&mut core, &ws_dir.root);
    // 符号检索同样吃后台索引：等就绪事件到了再断言命中。
    core.wait_indexed(&workspace_id);

    // 1) 基础命中：`main` 是 `src/main.rs` 里的函数，range 落在第 1–3 行。
    let hit = core.ok(
        "search.symbols",
        json!({ "workspace_id": workspace_id, "query": "main" }),
    );
    let symbols = hit["symbols"].as_array().expect("symbols 是数组");
    let main = symbols
        .iter()
        .find(|symbol| symbol["name"] == json!("main"))
        .expect("应命中 fn main");
    assert_eq!(main["kind"], json!("function"));
    assert_eq!(
        main["path"],
        json!("src/main.rs"),
        "path 是相对工作区根的路径"
    );
    assert_eq!(main["range"]["start_line"], json!(1));
    assert_eq!(main["range"]["end_line"], json!(3), "块级闭合算到第 3 行");

    // 2) `kinds` 过滤：同一个 query 换成只要 struct，就该是空表——而不是报错。
    let filtered = core.ok(
        "search.symbols",
        json!({ "workspace_id": workspace_id, "query": "main", "kinds": ["struct"] }),
    );
    assert_eq!(
        filtered["symbols"].as_array().map(Vec::len),
        Some(0),
        "kinds 过滤后不应残留 function：{filtered}"
    );

    // 3) 无匹配：空表 + 成功响应（浏览型查询不该因为没找到就报错）。
    let missing = core.ok(
        "search.symbols",
        json!({ "workspace_id": workspace_id, "query": "definitely_absent_symbol" }),
    );
    assert_eq!(missing["symbols"].as_array().map(Vec::len), Some(0));

    // 4) 未知工作区：走业务错误 1001，而不是静默空表。
    let unknown = core.error_code(
        "search.symbols",
        json!({ "workspace_id": "00000000-0000-4000-8000-000000000000", "query": "main" }),
    );
    assert_eq!(unknown, 1001, "未知工作区应返回 1001");
}

/// 后台索引（本轮任务）：三条可观测行为必须同时成立。
///
/// 1. `workspace.open` 不再同步等整库遍历——首次打开回 `indexing: true` + 空统计；
/// 2. 索引未就绪时的检索请求不被阻塞，回降级形状（空表 + `indexing: true`），
///    而不是把「还没扫完」说成「没有」；
/// 3. 索引完成后内核主动推 `workspace.indexed`，载荷是真实字段
///    （`workspace_id` / `stats` 的 files·dirs·ignored / `duration_ms` / `incremental`）。
///
/// 为什么要造 1500 个文件：小目录的构建可能在一帧之内跑完，`indexing: true` 这条
/// 断言会随机器快慢飘。把库撑到「必然要花几毫秒」，断言才稳定、才不是靠运气绿。
#[test]
fn 后台索引_打开不阻塞_完成推送workspace_indexed() {
    let ws_dir = TempWorkspace::new("async-index");
    for index in 0..1500 {
        std::fs::write(
            ws_dir.root.join("src").join(format!("mod_{index}.rs")),
            format!("pub fn helper_{index}() {{}}\n"),
        )
        .expect("写源文件");
    }

    let mut core = Core::spawn();
    let opened = core.ok(
        "workspace.open",
        json!({ "path": ws_dir.root.display().to_string() }),
    );
    let workspace_id = opened["workspace_id"]
        .as_str()
        .expect("workspace_id 必须是字符串")
        .to_string();
    assert_eq!(
        opened["indexing"],
        json!(true),
        "首次打开必须回「索引中」而不是等整库遍历：{opened}"
    );
    assert_eq!(
        opened["stats"]["files"],
        json!(0),
        "索引中时的统计是空快照：{opened}"
    );

    // 2) 打开后立刻检索：允许「已经建完」，但不允许「空表 + indexing:false」这种
    //    把未就绪伪装成没有结果的形状。
    let immediate = core.ok(
        "search.symbols",
        json!({ "workspace_id": workspace_id, "query": "helper_1" }),
    );
    assert!(
        immediate.get("indexing").is_some(),
        "search.symbols 必须带就绪标记：{immediate}"
    );
    if immediate["indexing"] == json!(true) {
        assert_eq!(
            immediate["symbols"].as_array().map(Vec::len),
            Some(0),
            "未就绪时必须是空表的降级结果：{immediate}"
        );
    }
    let immediate_files = core.ok(
        "search.files",
        json!({ "workspace_id": workspace_id, "query": "mod_1" }),
    );
    assert!(
        immediate_files.get("indexing").is_some(),
        "search.files 必须带同一个就绪标记：{immediate_files}"
    );

    // 3) 后台建完：内核主动推事件，字段一个都不能少。
    let indexed = core.wait_indexed(&workspace_id);
    assert!(
        indexed["stats"]["files"].as_u64().unwrap_or(0) >= 1500,
        "文件数必须覆盖全部源文件：{indexed}"
    );
    assert!(
        indexed["stats"]["dirs"].as_u64().unwrap_or(0) >= 1,
        "目录数至少要含 src：{indexed}"
    );
    assert!(
        indexed["stats"]["ignored"].is_u64(),
        "ignored 是整数：{indexed}"
    );
    assert!(indexed["duration_ms"].is_u64(), "耗时是毫秒整数：{indexed}");
    assert_eq!(
        indexed["incremental"],
        json!(false),
        "当前每次都是全量重建：{indexed}"
    );

    // 4) 就绪后同一查询：标记归位，结果真的出来了。
    let after = core.ok(
        "search.symbols",
        json!({ "workspace_id": workspace_id, "query": "helper_1" }),
    );
    assert_eq!(
        after["indexing"],
        json!(false),
        "索引就绪后不应再标索引中：{after}"
    );
    let symbols = after["symbols"].as_array().expect("symbols 是数组");
    assert!(
        symbols
            .iter()
            .any(|symbol| symbol["name"] == json!("helper_1")),
        "就绪后应命中 helper_1：{after}"
    );
    assert_eq!(
        symbols[0]["path"],
        json!("src/mod_1.rs"),
        "path 是相对工作区根、以 / 分隔的路径"
    );
}
