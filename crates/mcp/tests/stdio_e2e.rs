//! stdio 端到端测试：真实子进程 + 真实管道。
//!
//! 这里刻意不 mock：`StdioTransport` 的正确性（逐行帧、id 派发、管道断开、
//! 进程退出）只有真起一个进程才验证得到。被测 Server 是本 crate 自带的
//! `src/bin/mcp_mock_server.rs`，不访问网络，不读任何真实凭据。

use std::collections::BTreeMap;
use std::time::Duration;

use ai_ide_mcp::{
    ClientState, ErrorClass, McpClient, McpConfig, McpServerConfig, StdioServerConfig,
};
use serde_json::json;

/// 测试专用 mock Server 的绝对路径（由 Cargo 注入）。
const MOCK_SERVER: &str = env!("CARGO_BIN_EXE_mcp-mock-server");

fn stdio_config(scenario: &str) -> McpServerConfig {
    McpServerConfig::Stdio(StdioServerConfig {
        name: "mock".to_string(),
        command: MOCK_SERVER.to_string(),
        args: vec![scenario.to_string()],
        env: BTreeMap::new(),
        cwd: None,
    })
}

#[tokio::test]
async fn handshake_lists_tools_and_calls_them() {
    let client = McpClient::connect(&stdio_config("ok"))
        .await
        .expect("握手应成功");

    assert_eq!(client.state(), ClientState::Ready);
    let version = client.protocol_version().expect("协议版本");
    assert!(
        ai_ide_mcp::SUPPORTED_PROTOCOL_VERSIONS.contains(&version.as_str()),
        "协商出的版本必须是客户端支持的：{version}"
    );
    let init = client.initialize_result().expect("initialize 结果");
    assert_eq!(init.server_info.name, "mock");

    let tools = client.list_tools().await.expect("tools/list");
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, vec!["echo", "fail_tool", "slow_tool"]);
    // 命名空间规则：上层拿到的名字必须是可解析的。
    let full = ai_ide_mcp::tool_namespace(client.server_name(), &tools[0].name);
    assert_eq!(
        ai_ide_mcp::split_namespaced_tool(&full),
        Some(("mock", "echo"))
    );

    let result = client
        .call_tool("echo", json!({"text": "你好"}))
        .await
        .expect("调用成功");
    assert!(!result.is_error);
    assert!(result.text().contains("echo:"), "{}", result.text());

    client.close().await;
    assert_eq!(client.state(), ClientState::Closed);
}

#[tokio::test]
async fn tool_execution_failure_is_a_normal_result() {
    let client = McpClient::connect(&stdio_config("ok"))
        .await
        .expect("握手应成功");
    let result = client
        .call_tool("fail_tool", json!({"text": "x"}))
        .await
        .expect("工具执行失败不应该变成 Err");
    assert!(result.is_error);
    assert!(result.text().contains("失败"), "{}", result.text());
    // 连接仍然可用：一次工具失败不代表连接坏了。
    assert_eq!(client.state(), ClientState::Ready);
    client.close().await;
}

#[tokio::test]
async fn tools_list_follows_pagination_cursor() {
    let client = McpClient::connect(&stdio_config("paginate"))
        .await
        .expect("握手应成功");
    let tools = client.list_tools().await.expect("tools/list");
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, vec!["echo", "second"]);
    client.close().await;
}

#[tokio::test]
async fn unsupported_protocol_version_fails_handshake() {
    let err = McpClient::connect(&stdio_config("unsupported-version"))
        .await
        .expect_err("版本不受支持必须失败");
    assert_eq!(err.class(), ErrorClass::Protocol);
    assert!(err.to_string().contains("1999-01-01"), "{err}");
}

#[tokio::test]
async fn invalid_jsonrpc_frame_is_a_protocol_error() {
    let err = McpClient::connect(&stdio_config("malformed"))
        .await
        .expect_err("非法帧必须失败");
    assert_eq!(err.class(), ErrorClass::Protocol);
}

#[tokio::test]
async fn rpc_error_from_server_is_classified_as_rpc() {
    let client = McpClient::connect(&stdio_config("rpc-error"))
        .await
        .expect("握手应成功");
    let err = client
        .call_tool("echo", json!({"text": "x"}))
        .await
        .expect_err("Server 回 JSON-RPC 错误，必须失败");
    assert_eq!(err.class(), ErrorClass::Rpc);
    assert!(
        !err.is_retryable(),
        "RPC 错误是逻辑错误，不该自动重试：{err}"
    );
    client.close().await;
}

#[tokio::test]
async fn slow_tool_hits_request_timeout_and_marks_stale() {
    let client = McpClient::connect(&stdio_config("silent"))
        .await
        .expect("握手应成功");
    let client = client.with_request_timeout(Duration::from_millis(200));
    let started = std::time::Instant::now();
    let err = client
        .call_tool("slow_tool", json!({"text": "x"}))
        .await
        .expect_err("必须超时");
    assert_eq!(err.class(), ErrorClass::Timeout);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "超时未生效：{:?}",
        started.elapsed()
    );
    assert_eq!(client.state(), ClientState::Stale);
    client.close().await;
}

#[tokio::test]
async fn server_exit_during_call_is_disconnected_and_reconnect_recovers() {
    let client = McpClient::connect(&stdio_config("exit-on-call"))
        .await
        .expect("握手应成功");
    let err = client
        .call_tool("echo", json!({"text": "x"}))
        .await
        .expect_err("Server 中途退出，必须失败");
    assert_eq!(err.class(), ErrorClass::Disconnected);
    assert_eq!(client.state(), ClientState::Stale);

    // 重连会重新 spawn 一个进程：能重新握手说明「断开后恢复」这条路径是通的。
    client.reconnect().await.expect("重连应成功");
    assert_eq!(client.state(), ClientState::Ready);
    assert_eq!(client.last_failure(), None);
    client.close().await;
}

#[tokio::test]
async fn crash_after_init_recovers_on_reconnect_then_fails_again() {
    let client = McpClient::connect(&stdio_config("crash-after-init"))
        .await
        .expect("握手应成功（握手帧已收到）");
    let err = client.list_tools().await.expect_err("进程已退出，必须失败");
    assert_eq!(err.class(), ErrorClass::Disconnected);
    assert_eq!(client.state(), ClientState::Stale);

    client.reconnect().await.expect("重连应成功");
    assert_eq!(client.state(), ClientState::Ready);
    // 新进程同样会在握手后退出，所以下一次调用仍会失败——这是 Server 的问题，不是客户端的。
    let err = client.list_tools().await.expect_err("必须再次失败");
    assert_eq!(err.class(), ErrorClass::Disconnected);
}

#[tokio::test]
async fn closed_client_refuses_new_work() {
    let client = McpClient::connect(&stdio_config("ok"))
        .await
        .expect("握手应成功");
    client.close().await;
    let err = client.list_tools().await.expect_err("关闭后必须拒绝");
    assert_eq!(err.class(), ErrorClass::Disconnected);
}

#[tokio::test]
async fn config_file_drives_a_real_connection() {
    // 配置解析 → 建连 → 调用，串起 mcp.json 到可用工具的整条链路。
    let text = format!(
        r#"{{"mcpServers":{{"mock":{{"type":"stdio","command":"{}","args":["ok"]}}}}}}"#,
        MOCK_SERVER.replace('\\', "\\\\")
    );
    let config = McpConfig::parse(&text).expect("配置应可解析");
    let server = config.server("mock").expect("mock 配置");
    let client = McpClient::connect(server).await.expect("握手应成功");
    assert_eq!(client.server_name(), "mock");
    let tools = client.list_tools().await.expect("tools/list");
    assert_eq!(tools.len(), 3);
    client.close().await;
}
