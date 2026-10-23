//! `mcp.*` 端到端测试（M7 切片二）。
//!
//! 和 `terminal_flow` / `git_flow` 一样跑真子进程，但这一层比它们多跨一跳：
//! 内核进程 →（stdio JSON-RPC）→ mock MCP Server 进程。进程内单测能把管理器
//! 的账本逻辑测干净，却测不到「配置真的读进来了、子进程真的握上手了、参数真的
//! 被转发过去了、关掉之后真的没残留」——只有把内核当黑盒、只走 stdin / stdout
//! 才能验证。被连的 Server 是本仓库自带的 `mcp-mock-server`（`crates/mcp/src/bin`），
//! 不访问网络、不读任何真实凭据。
//!
//! 覆盖五件事：
//!
//! 1. 能力清单里有 `mcp.*`；
//! 2. `mcp.servers` 能把「配置里有」与「已连上」分开报；
//! 3. `mcp.connect` 真起子进程、真握手、真列工具，命名空间是 `mcp__<server>__<tool>`；
//! 4. `mcp.call` 真把参数送到 Server，工具**执行**失败（`is_error`）不算协议失败；
//! 5. 错误码各归其位：未知 Server 8001、没连 8003、没有这个工具 8004；
//!    断连幂等，且断开后工具真的下架。

mod common;

use common::{Core, TempDataDir};
use serde_json::{json, Value};
use std::path::PathBuf;

/// mock Server 的绝对路径。
///
/// 被测的是 `ai-ide-core`，拿不到 `ai-ide-mcp` 的 `CARGO_BIN_EXE_*`（那个变量只对
/// 同一个 package 的测试注入），所以按「测试可执行文件 → target 目录」反推。
fn mock_server() -> PathBuf {
    let exe = std::env::current_exe().expect("取当前测试进程路径");
    // target/<profile>/deps/mcp_flow-<hash>[.exe] → target/<profile>
    let dir = exe
        .parent()
        .and_then(|deps| deps.parent())
        .expect("定位 target 目录");
    let name = if cfg!(windows) {
        "mcp-mock-server.exe"
    } else {
        "mcp-mock-server"
    };
    let path = dir.join(name);
    assert!(
        path.is_file(),
        "mock Server 未构建：{}（先跑 cargo build -p ai-ide-mcp --bin mcp-mock-server）",
        path.display()
    );
    path
}

/// 起一个内核，并让它读测试专用的 `mcp.json`（只连一个 stdio mock Server）。
fn boot(tag: &str) -> (Core, TempDataDir) {
    let data = TempDataDir::new(tag);
    let config = data.root.join("mcp.json");
    let text = json!({
        "mcpServers": {
            "mock": {
                "type": "stdio",
                "command": mock_server().display().to_string(),
                "args": ["ok"],
            }
        }
    })
    .to_string();
    std::fs::write(&config, text).expect("写 mcp.json");

    let mut core = Core::spawn_with_env(
        &data.root,
        &[("AI_IDE_MCP_CONFIG", &config.display().to_string())],
    );
    core.capabilities();
    (core, data)
}

/// 连上 mock Server，并回它的 connect 结果。
fn connect(core: &mut Core) -> Value {
    core.ok("mcp.connect", json!({ "server": "mock" }))
}

/// 工具清单里的名字（保持响应顺序，便于断言）。
fn tool_names(value: &Value) -> Vec<String> {
    value["tools"]
        .as_array()
        .expect("tools 应是数组")
        .iter()
        .map(|tool| {
            tool["namespaced_name"]
                .as_str()
                .expect("应有全名")
                .to_string()
        })
        .collect()
}

#[test]
fn 能力清单里有_mcp_方法() {
    let (mut core, _data) = boot("mcp-capabilities");
    let caps = core.capabilities();
    for expected in [
        "mcp.servers",
        "mcp.connect",
        "mcp.disconnect",
        "mcp.tools",
        "mcp.call",
    ] {
        assert!(caps.contains(&expected.to_string()), "缺少 {expected}");
    }
}

#[test]
fn 未连接时_server_清单如实报配置状态() {
    let (mut core, _data) = boot("mcp-servers");

    let result = core.ok("mcp.servers", json!({}));
    let servers = result["servers"].as_array().expect("servers 应是数组");
    assert_eq!(servers.len(), 1, "{result}");
    let mock = &servers[0];
    assert_eq!(mock["name"], "mock");
    assert_eq!(mock["transport"], "stdio");
    assert_eq!(mock["configured"], true);
    // 关键点：配置里有 ≠ 已连上。内核不因为「配置文件写了」就预拉起子进程。
    assert_eq!(mock["connected"], false);
    assert_eq!(mock["state"], "configured");
    assert_eq!(mock["tool_count"], 0);

    // 没连过就没有工具可列。
    let tools = core.ok("mcp.tools", json!({}));
    assert_eq!(tools["tools"].as_array().map(Vec::len), Some(0));
}

#[test]
fn 连接_真起子进程并发现带命名空间的工具() {
    let (mut core, _data) = boot("mcp-connect");

    let result = connect(&mut core);
    assert_eq!(result["server"], "mock");
    assert_eq!(result["transport"], "stdio");
    assert!(
        result["protocol_version"].is_string(),
        "握手应协商出版本：{result}"
    );
    assert_eq!(result["tool_count"], 3);
    assert_eq!(
        tool_names(&result),
        vec![
            "mcp__mock__echo",
            "mcp__mock__fail_tool",
            "mcp__mock__slow_tool",
        ]
    );
    // description 与 inputSchema 原样透传：不是内核自己编的。
    let echo = &result["tools"][0];
    assert!(
        echo["description"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "{result}"
    );
    assert!(echo["input_schema"].is_object(), "{result}");

    // 再问一次 servers：这一回是「已连上」。
    let servers = core.ok("mcp.servers", json!({}));
    let mock = &servers["servers"][0];
    assert_eq!(mock["connected"], true);
    assert_eq!(mock["state"], "ready");
    assert_eq!(mock["tool_count"], 3);

    // 按 Server 过滤与全量列都该看得到同一批工具。
    let all = core.ok("mcp.tools", json!({}));
    assert_eq!(tool_names(&all).len(), 3);
    let only = core.ok("mcp.tools", json!({ "server": "mock" }));
    assert_eq!(tool_names(&only), tool_names(&all));

    // 重复连接是幂等的：不该再起第二个子进程（否则 tool_count 会翻倍或报错）。
    let again = connect(&mut core);
    assert_eq!(again["tool_count"], 3);
}

#[test]
fn 调用_参数送到_server_且工具执行失败不算协议失败() {
    let (mut core, _data) = boot("mcp-call");
    connect(&mut core);

    // 用全名调：server 可省略。
    let echoed = core.ok(
        "mcp.call",
        json!({ "tool": "mcp__mock__echo", "arguments": { "text": "你好" } }),
    );
    assert_eq!(echoed["server"], "mock");
    assert_eq!(echoed["tool"], "echo");
    assert_eq!(echoed["namespaced_name"], "mcp__mock__echo");
    assert_eq!(echoed["is_error"], false);
    assert!(
        echoed["text"]
            .as_str()
            .is_some_and(|text| text.contains("echo:")),
        "参数应真的进了 Server：{echoed}"
    );

    // 用裸名 + 显式 server 调，同一件事的另一种写法。
    let echoed = core.ok(
        "mcp.call",
        json!({ "server": "mock", "tool": "echo", "arguments": { "text": "再来一次" } }),
    );
    assert_eq!(echoed["is_error"], false);
    assert!(
        echoed["text"]
            .as_str()
            .is_some_and(|text| text.contains("再来一次")),
        "{echoed}"
    );

    // 工具**执行**失败仍是成功响应：模型要能读到失败正文并换参数重试。
    let failed = core.ok("mcp.call", json!({ "tool": "mcp__mock__fail_tool" }));
    assert_eq!(failed["is_error"], true);
    assert!(
        failed["text"].as_str().is_some_and(|text| !text.is_empty()),
        "{failed}"
    );
}

#[test]
fn 错误码_各归其位() {
    let (mut core, _data) = boot("mcp-errors");

    // 配置里没有这个 Server：8001（要用户改配置，不是网络问题）。
    assert_eq!(
        core.error_code("mcp.connect", json!({ "server": "nope" })),
        8001
    );

    // 有配置但没连：8003（让模型先 connect）。
    assert_eq!(
        core.error_code("mcp.call", json!({ "tool": "mcp__mock__echo" })),
        8003
    );

    // 连上之后，名字不存在才是 8004。
    connect(&mut core);
    assert_eq!(
        core.error_code("mcp.call", json!({ "tool": "mcp__mock__nope" })),
        8004
    );

    // 裸名既没 server 也不是全名：参数错 8007，而不是拿去瞎猜一个 Server。
    assert_eq!(core.error_code("mcp.call", json!({ "tool": "echo" })), 8007);
}

#[test]
fn 断连_幂等且工具真的下架() {
    let (mut core, _data) = boot("mcp-disconnect");
    connect(&mut core);

    let first = core.ok("mcp.disconnect", json!({ "server": "mock" }));
    assert_eq!(first["server"], "mock");
    assert_eq!(first["closed"], true);
    assert_eq!(first["removed_tools"], 3);

    // 再断一次：幂等，不算错误。
    let again = core.ok("mcp.disconnect", json!({ "server": "mock" }));
    assert_eq!(again["closed"], false);
    assert_eq!(again["removed_tools"], 0);

    // 断开后工具下架，再调只能拿到 8003。
    let tools = core.ok("mcp.tools", json!({}));
    assert_eq!(tools["tools"].as_array().map(Vec::len), Some(0));
    assert_eq!(
        core.error_code("mcp.call", json!({ "tool": "mcp__mock__echo" })),
        8003
    );

    // 配置还在，所以 servers 里仍看得见它，只是回到 configured。
    let servers = core.ok("mcp.servers", json!({}));
    let mock = &servers["servers"][0];
    assert_eq!(mock["configured"], true);
    assert_eq!(mock["connected"], false);
    assert_eq!(mock["tool_count"], 0);
}
