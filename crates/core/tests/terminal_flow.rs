//! `terminal.*` 端到端测试（M6 切片一）。
//!
//! 和其它 `*_flow` 一样跑真实子进程：终端这层的东西（ConPTY 的建会话、读线程、
//! 事件通道、子进程退出）在进程内单测里只能靠替身，唯有把内核当黑盒、只走
//! stdin / stdout，才能验证「真起了个 shell、真收到了输出、真杀掉子进程」。
//!
//! 覆盖四件事：
//!
//! 1. 能力清单里有 `terminal.*`（`core.hello` 报的就是 dispatch 能路由的集合）；
//! 2. 多会话：两条会话的输出不会串台，`terminal.list` 分别看得见；
//! 3. resize：回夹取后的真实尺寸（`0×0` 这类还没布局完的量法不该建出废终端）；
//! 4. 进程退出清理：shell 自己 `exit` 与 `terminal.kill` 都会推出 `terminal.exit`，
//!    并且会话从 `terminal.list` 里消失（之后再访问返回 6001）。

mod common;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use common::{Core, TempDataDir};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// 等一条事件的兜底上限：比 `common::TIMEOUT` 短，失败信息里还能带上已收到的内容。
const WAIT: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

/// 起一个内核并握手。
fn boot(tag: &str) -> (Core, TempDataDir) {
    let data = TempDataDir::new(tag);
    let mut core = Core::spawn(&data.root);
    core.capabilities();
    (core, data)
}

/// 建一个终端会话，返回它的 id。
///
/// 显式指定 shell 而不用平台缺省：测试要能在 Windows 与 Unix 上跑同一套断言，
/// 而 `%COMSPEC%` 在 CI 容器里未必存在。
fn create(core: &mut Core, cols: u16, rows: u16) -> String {
    let result = core.ok(
        "terminal.create",
        json!({ "shell": default_shell(), "cols": cols, "rows": rows }),
    );
    result["terminal"]["id"]
        .as_str()
        .expect("terminal.create 应返回 id")
        .to_string()
}

#[cfg(windows)]
fn default_shell() -> &'static str {
    "cmd.exe"
}

#[cfg(not(windows))]
fn default_shell() -> &'static str {
    "/bin/sh"
}

/// 写一段文本进会话（带 `\r`，终端里回车才是回车）。
fn send_text(core: &mut Core, id: &str, text: &str) -> u64 {
    let payload = format!("{text}\r\n");
    let result = core.ok(
        "terminal.write",
        json!({ "terminal_id": id, "data": BASE64.encode(payload.as_bytes()) }),
    );
    result["bytes"]
        .as_u64()
        .expect("terminal.write 应返回字节数")
}

/// 取下一帧：先看 `call` 期间攒下的通知，再看管道。
fn next_frame(core: &mut Core, timeout: Duration) -> Option<Value> {
    if let Some(frame) = core.pop_event() {
        return Some(frame);
    }
    core.try_read_frame(timeout)
}

/// 把某会话的输出累积到出现 `needle` 为止。
///
/// 必须**累积**：一只 shell 的首屏 banner、回显、命令输出会被读线程切成好几段
/// `terminal.data`，任何一段单独看都可能不含完整关键字。
fn wait_output(core: &mut Core, id: &str, needle: &str) -> String {
    let mut acc = String::new();
    let deadline = Instant::now() + WAIT;
    while !acc.contains(needle) {
        assert!(
            Instant::now() < deadline,
            "等待终端输出 {needle:?} 超时，已收到：{acc:?}"
        );
        let Some(frame) = next_frame(core, Duration::from_millis(200)) else {
            continue;
        };
        if frame["method"] != json!("terminal.data") {
            continue;
        }
        let params = &frame["params"];
        if params["terminal_id"] != json!(id) {
            continue;
        }
        let raw = BASE64
            .decode(params["data"].as_str().unwrap_or_default())
            .expect("terminal.data 的 data 必须是合法 base64");
        acc.push_str(&String::from_utf8_lossy(&raw));
    }
    acc
}

/// 等某会话的 `terminal.exit`，返回它的 params。
fn wait_exit(core: &mut Core, id: &str) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "等待 terminal.exit 超时");
        let Some(frame) = next_frame(core, Duration::from_millis(200)) else {
            continue;
        };
        if frame["method"] != json!("terminal.exit") {
            continue;
        }
        let params = frame["params"].clone();
        if params["terminal_id"] == json!(id) {
            return params;
        }
    }
}

/// `terminal.list` 的会话 id 列表。
fn list_ids(core: &mut Core) -> Vec<String> {
    let result = core.ok("terminal.list", json!({}));
    result["terminals"]
        .as_array()
        .expect("terminals 是数组")
        .iter()
        .map(|view| view["id"].as_str().unwrap_or_default().to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// 能力清单
// ---------------------------------------------------------------------------

#[test]
fn capabilities_include_terminal_methods() {
    let (mut core, _data) = boot("m6-cap");
    let caps = core.capabilities();
    for method in [
        "terminal.create",
        "terminal.write",
        "terminal.resize",
        "terminal.kill",
        "terminal.list",
    ] {
        assert!(
            caps.iter().any(|cap| cap == method),
            "能力清单缺少 {method}：{caps:?}"
        );
    }
    // 事件不是「可调用方法」，不该混进能力清单。
    assert!(!caps.iter().any(|cap| cap == "terminal.data"));
}

#[test]
fn create_write_and_list_roundtrip() {
    let (mut core, _data) = boot("m6-create");
    assert!(list_ids(&mut core).is_empty(), "刚启动时不该有会话");

    let id = create(&mut core, 80, 24);
    let list = core.ok("terminal.list", json!({}));
    assert_eq!(list["total"], json!(1));
    let view = &list["terminals"][0];
    assert_eq!(view["id"], json!(id));
    assert_eq!(view["cols"], json!(80));
    assert_eq!(view["rows"], json!(24));
    assert!(view["shell"].as_str().is_some_and(|s| !s.is_empty()));

    // 写入回的是解码后的字节数：多 2 个字节就是那个 `\r\n`。
    let bytes = send_text(&mut core, &id, "echo marker-one");
    assert_eq!(bytes, ("echo marker-one\r\n").len() as u64);

    let output = wait_output(&mut core, &id, "marker-one");
    // 回显 + 命令输出各一次，所以这个关键字至少出现两遍。
    assert!(
        output.matches("marker-one").count() >= 2,
        "应看到回显与输出：{output:?}"
    );

    core.ok("terminal.kill", json!({ "terminal_id": id }));
}

// ---------------------------------------------------------------------------
// 多会话
// ---------------------------------------------------------------------------

#[test]
fn multiple_sessions_do_not_cross_talk() {
    let (mut core, _data) = boot("m6-multi");
    let first = create(&mut core, 80, 24);
    let second = create(&mut core, 100, 30);
    assert_ne!(first, second, "两个会话应是不同 id");

    let ids = list_ids(&mut core);
    assert_eq!(ids.len(), 2);
    assert_eq!(ids, vec![first.clone(), second.clone()], "按建会话顺序");

    send_text(&mut core, &first, "echo only-in-first");
    wait_output(&mut core, &first, "only-in-first");

    send_text(&mut core, &second, "echo only-in-second");
    wait_output(&mut core, &second, "only-in-second");

    // 关键字是会话私有的：第二条会话的输出里不该出现第一条的标记。
    let mut acc = String::new();
    let deadline = Instant::now() + Duration::from_millis(800);
    while Instant::now() < deadline {
        let Some(frame) = next_frame(&mut core, Duration::from_millis(100)) else {
            continue;
        };
        if frame["method"] != json!("terminal.data") {
            continue;
        }
        let params = &frame["params"];
        if params["terminal_id"] != json!(second) {
            continue;
        }
        let raw = BASE64
            .decode(params["data"].as_str().unwrap_or_default())
            .unwrap_or_default();
        acc.push_str(&String::from_utf8_lossy(&raw));
    }
    assert!(
        !acc.contains("only-in-first"),
        "第二个会话收到了第一个会话的输出：{acc:?}"
    );

    core.ok("terminal.kill", json!({ "terminal_id": first }));
    core.ok("terminal.kill", json!({ "terminal_id": second }));
}

// ---------------------------------------------------------------------------
// resize
// ---------------------------------------------------------------------------

#[test]
fn resize_returns_clamped_size() {
    let (mut core, _data) = boot("m6-resize");
    let id = create(&mut core, 80, 24);

    let result = core.ok(
        "terminal.resize",
        json!({ "terminal_id": id, "cols": 120, "rows": 40 }),
    );
    assert_eq!(result["terminal"]["cols"], json!(120));
    assert_eq!(result["terminal"]["rows"], json!(40));

    // 列表里跟着变：尺寸是会话状态，不是一次性回执。
    let list = core.ok("terminal.list", json!({}));
    assert_eq!(list["terminals"][0]["cols"], json!(120));
    assert_eq!(list["terminals"][0]["rows"], json!(40));

    // 客户端在容器还没布局完时量到 0 是常态：夹到下限，而不是报错或建出 1×1。
    let clamped = core.ok(
        "terminal.resize",
        json!({ "terminal_id": id, "cols": 0, "rows": 0 }),
    );
    assert_eq!(clamped["terminal"]["cols"], json!(2));
    assert_eq!(clamped["terminal"]["rows"], json!(1));

    // 越界上界同样夹住。
    let clamped = core.ok(
        "terminal.resize",
        json!({ "terminal_id": id, "cols": 9999, "rows": 9999 }),
    );
    assert_eq!(clamped["terminal"]["cols"], json!(500));
    assert_eq!(clamped["terminal"]["rows"], json!(200));

    core.ok("terminal.kill", json!({ "terminal_id": id }));
}

// ---------------------------------------------------------------------------
// 进程退出清理
// ---------------------------------------------------------------------------

#[test]
fn shell_exit_emits_exit_event_and_cleans_up() {
    let (mut core, _data) = boot("m6-selfexit");
    let id = create(&mut core, 80, 24);

    // 让 shell 自己退出：这条路径和 kill 不同——没有请求触发，事件是读线程
    // 发现 EOF 后自己推的。
    send_text(&mut core, &id, "exit");
    let event = wait_exit(&mut core, &id);
    assert_eq!(event["terminal_id"], json!(id));
    assert_eq!(event["code"], json!(0), "shell 正常退出码应为 0");
    assert_eq!(event["signal"], json!(null), "Windows 没有信号名");
    assert!(event["at_ms"].as_i64().unwrap_or(0) > 0);

    // 会话必须已经从表里摘掉，否则客户端会留一个永远空白的标签页。
    let deadline = Instant::now() + WAIT;
    while list_ids(&mut core).contains(&id) {
        assert!(
            Instant::now() < deadline,
            "退出的会话仍留在 terminal.list 里"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // 已清理的会话再访问是 6001，不是 500。
    assert_eq!(
        core.error_code("terminal.write", json!({ "terminal_id": id, "data": "" })),
        6001
    );
    assert_eq!(
        core.error_code(
            "terminal.resize",
            json!({ "terminal_id": id, "cols": 80, "rows": 24 })
        ),
        6001
    );
    assert_eq!(
        core.error_code("terminal.kill", json!({ "terminal_id": id })),
        6001
    );
}

#[test]
fn kill_emits_exit_event_and_removes_session() {
    let (mut core, _data) = boot("m6-kill");
    let id = create(&mut core, 80, 24);

    let result = core.ok("terminal.kill", json!({ "terminal_id": id }));
    assert_eq!(result["ok"], json!(true));

    // kill 之后仍然会推 exit：客户端不管「谁杀的」，只认这条事件清标签页。
    let event = wait_exit(&mut core, &id);
    assert_eq!(event["terminal_id"], json!(id));
    assert!(list_ids(&mut core).is_empty());

    // 重复 kill 是 6001：会话不在了，这不是幂等成功。
    assert_eq!(
        core.error_code("terminal.kill", json!({ "terminal_id": id })),
        6001
    );
}

// ---------------------------------------------------------------------------
// 参数校验
// ---------------------------------------------------------------------------

#[test]
fn write_rejects_invalid_base64_with_6004() {
    let (mut core, _data) = boot("m6-base64");
    let id = create(&mut core, 80, 24);
    // `***` 不在标准字母表里，解不出字节。
    assert_eq!(
        core.error_code(
            "terminal.write",
            json!({ "terminal_id": id, "data": "***not-base64***" })
        ),
        6004
    );
    core.ok("terminal.kill", json!({ "terminal_id": id }));
}

#[test]
fn create_rejects_missing_shell_with_6002() {
    let (mut core, _data) = boot("m6-badshell");
    // 指定一个肯定不存在的可执行文件：建会话失败要如实报 6002，
    // 而不是回一个永远收不到输出的会话。
    let code = core.error_code(
        "terminal.create",
        json!({ "shell": "definitely-not-a-shell-xyz.exe" }),
    );
    assert_eq!(code, 6002);
    assert!(list_ids(&mut core).is_empty(), "失败的会话不该进登记表");
}
