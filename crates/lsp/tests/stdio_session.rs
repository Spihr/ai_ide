//! 端到端测试：真的起一个子进程，走真正的 stdio 管道。
//!
//! 单测（`src/**` 里的 `mod tests`）只能验证编解码与状态机，**帧边界错一位、
//! 读写线程互相卡死、子进程被杀了但等待者还在排队**这些问题是单测永远抓不到的。
//! 所以这里用 `src/bin/lsp_mock_server.rs` 起一个可"按要求作恶"的假服务器，
//! 把异常路径也变成可重复的用例。
//!
//! 覆盖矩阵：
//!
//! - 握手：`initialize` → `initialized`，能力与 `serverInfo` 正确落地；
//! - 往返：请求 id 关联，`result` 原样回填（含非 ASCII，验证按字节计长）；
//! - 文档同步：`didOpen` / `didChange`（full）/ `didClose` 与版本号推进；
//! - 通知广播：`publishDiagnostics` 进广播通道；
//! - 未握手就发请求：被状态机拒绝，不该落到管道上；
//! - 服务器中途退出：等待中的请求必须**立刻**失败，而不是干等到超时；
//! - 非法帧：连接判死，后续请求快速失败；
//! - 超时：慢服务器触发 `Timeout`，连接不被污染；
//! - 体上限：超限消息直接判协议错误，不把整段读进内存。

use std::path::PathBuf;
use std::time::Duration;

use ai_ide_lsp::{
    ErrorClass, LspClient, LspError, ServerConfig, TextDocumentSyncKind, TransportLimits,
};
use serde_json::json;

/// 假服务器二进制（cargo 会为本 crate 的所有 bin 注入这个环境变量）。
const MOCK_BIN: &str = env!("CARGO_BIN_EXE_lsp-mock-server");

fn mock_config() -> ServerConfig {
    // 显式给一个受限的分帧上限：顺手确认配置能从 ServerConfig 一路传到读循环。
    ServerConfig::new("mock-server", MOCK_BIN).limits(TransportLimits::new(8 * 1024, 1024 * 1024))
}

fn sample_path() -> PathBuf {
    PathBuf::from("E:/ai_ide/crates/lsp/src/lib.rs")
}

#[tokio::test]
async fn handshake_reports_capabilities_and_marks_server_initialized() {
    let client = LspClient::start(&mock_config()).await.expect("握手应成功");

    let info = client.server_info().expect("必须有 serverInfo");
    assert_eq!(info.name, "lsp-mock-server");
    assert_eq!(info.version.as_deref(), Some("0.1.0"));

    let capabilities = client.capabilities().expect("必须有 capabilities");
    // 假服务器声明 `textDocumentSync: 1`，客户端要能归一化成「全文同步」。
    assert_eq!(capabilities.sync_kind(), TextDocumentSyncKind::Full);
    assert!(capabilities.supports_open_close());

    // `initialized` 是通知：服务器收不到它就不会真正开始服务。
    // 这里用一次状态查询反证通知确实到了（假服务器把它记成 initialized=true）。
    let state = client
        .request("mock/state", None)
        .await
        .expect("握手之后必须能发请求");
    assert_eq!(state["initialized"], json!(true));
    assert!(!client.is_closed());

    client.shutdown().await.expect("关闭应成功");
    assert!(client.is_closed());
}

#[tokio::test]
async fn requests_are_correlated_and_results_round_trip() {
    let client = LspClient::start(&mock_config()).await.expect("握手应成功");

    let echoed = client
        .request(
            "mock/echo",
            Some(json!({"n": 7, "text": "中文回显", "items": [1, 2]})),
        )
        .await
        .expect("往返应成功");
    assert_eq!(echoed["n"], json!(7));
    // 非 ASCII 必须靠 Content-Length 按字节数编码，否则帧会被切坏。
    assert_eq!(echoed["text"], json!("中文回显"));

    // 连续多个请求：id 单调递增且各自拿到自己的回包，不会串。
    let first = client.request("mock/echo", Some(json!({"seq": 1}))).await;
    let second = client.request("mock/echo", Some(json!({"seq": 2}))).await;
    assert_eq!(first.expect("第一次")["seq"], json!(1));
    assert_eq!(second.expect("第二次")["seq"], json!(2));

    let missing = client.request("mock/not-implemented", None).await;
    let error = missing.expect_err("未实现的方法必须回错误");
    assert!(matches!(error, LspError::Rpc { code: -32601, .. }));
    assert_eq!(error.class(), ErrorClass::Rpc);
    // JSON-RPC 错误只说明这次调用不合法，连接是好的。
    assert!(!error.is_fatal());
    assert!(!client.is_closed());

    client.shutdown().await.expect("关闭应成功");
}

#[tokio::test]
async fn document_sync_open_change_close_tracks_versions() {
    let client = LspClient::start(&mock_config()).await.expect("握手应成功");

    let uri = client
        .open_document(&sample_path(), "fn main() {}")
        .await
        .expect("didOpen 应成功");
    assert!(uri.starts_with("file:///"), "uri 实际为 {uri}");
    assert_eq!(client.open_document_count(), 1);

    let version = client
        .change_document(&uri, "fn main() { println!(\"v2\"); }")
        .await
        .expect("didChange 应成功");
    assert_eq!(version, 2, "didOpen 是 v1，第一次变更是 v2");

    let state = client.request("mock/state", None).await.expect("查询状态");
    assert_eq!(state["didOpen"], json!(1));
    assert_eq!(state["didChange"], json!(1));
    let document = &state["documents"][0];
    assert_eq!(document["uri"], json!(uri));
    assert_eq!(document["languageId"], json!("rust"));
    assert_eq!(document["version"], json!(2));
    // full 同步：服务器拿到的必须是整篇新文本，不是增量片段。
    assert_eq!(document["text"], json!("fn main() { println!(\"v2\"); }"));

    client.close_document(&uri).await.expect("didClose 应成功");
    assert_eq!(client.open_document_count(), 0);

    let state = client.request("mock/state", None).await.expect("查询状态");
    assert_eq!(state["didClose"], json!(1));
    assert_eq!(state["documents"], json!([]));

    client.shutdown().await.expect("关闭应成功");
}

#[tokio::test]
async fn publish_diagnostics_reaches_notification_subscribers() {
    let client = LspClient::start(&mock_config()).await.expect("握手应成功");
    let mut notifications = client.subscribe();

    client
        .request(
            "mock/diagnostics",
            Some(json!({
                "uri": "file:///E:/ai_ide/src/lib.rs",
                "version": 3,
                "message": "未使用的变量"
            })),
        )
        .await
        .expect("触发诊断应成功");

    let notification = tokio::time::timeout(Duration::from_secs(5), notifications.recv())
        .await
        .expect("通知必须在 5s 内到达")
        .expect("广播通道应可用");
    assert_eq!(notification.method, "textDocument/publishDiagnostics");
    let params = notification.params.expect("诊断必须带 params");
    assert_eq!(params["uri"], json!("file:///E:/ai_ide/src/lib.rs"));
    assert_eq!(params["version"], json!(3));
    let diagnostics = params["diagnostics"].as_array().expect("诊断数组");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["message"], json!("未使用的变量"));

    client.shutdown().await.expect("关闭应成功");
}

#[tokio::test]
async fn requests_before_initialize_are_rejected_without_touching_the_pipe() {
    // connect 不起握手，模拟「点了连接但还没初始化」。
    let client = LspClient::connect(&mock_config())
        .await
        .expect("起进程应成功");

    let error = client
        .request("mock/echo", None)
        .await
        .expect_err("未初始化必须拒绝");
    assert!(matches!(error, LspError::NotInitialized(_)));
    assert_eq!(error.class(), ErrorClass::State);
    assert!(!error.is_retryable());

    // 拒绝发生在本地状态机里，连接本身没被污染：补上握手就能用。
    client.initialize().await.expect("补握手应成功");
    let echoed = client
        .request("mock/echo", Some(json!({"ok": true})))
        .await
        .expect("握手后应可请求");
    assert_eq!(echoed["ok"], json!(true));

    client.shutdown().await.expect("关闭应成功");
}

#[tokio::test]
async fn server_exit_mid_request_fails_the_waiter_immediately() {
    let mut client = LspClient::start(&mock_config()).await.expect("握手应成功");
    // 故意给一个大超时：若实现靠超时才返回，这个用例会明显变慢并暴露问题。
    client.set_request_timeout(Duration::from_secs(30));

    let started = std::time::Instant::now();
    let outcome = client.request("mock/crash", None).await;
    let elapsed = started.elapsed();

    let error = outcome.expect_err("服务器中途退出，请求必须失败");
    assert!(
        matches!(error, LspError::Disconnected { .. }),
        "期望 Disconnected，实际 {error:?}"
    );
    assert!(error.is_fatal());
    assert!(error.is_retryable(), "断连可以重连后重试");
    assert!(
        elapsed < Duration::from_secs(5),
        "中途退出必须立刻唤醒等待者，不能等超时（实际耗时 {elapsed:?}）"
    );
    assert!(client.is_closed(), "连接必须被标记为关闭");

    // 已关闭的连接上再发请求：快速失败，不能再写管道。
    let after = client
        .request("mock/echo", None)
        .await
        .expect_err("关闭后必须失败");
    assert!(matches!(after, LspError::Closed(_)));
}

#[tokio::test]
async fn malformed_frame_invalidates_the_whole_connection() {
    let mut client = LspClient::start(&mock_config()).await.expect("握手应成功");
    client.set_request_timeout(Duration::from_secs(30));

    let outcome = client.request("mock/malformed", None).await;
    let error = outcome.expect_err("非法帧必须让请求失败");
    assert!(
        matches!(error, LspError::Protocol(_)),
        "期望 Protocol，实际 {error:?}"
    );
    assert_eq!(error.class(), ErrorClass::Protocol);
    // 帧边界错了，重连同一台服务器也没用，必须报上去。
    assert!(!error.is_retryable());
    assert!(client.is_closed());

    let after = client
        .request("mock/state", None)
        .await
        .expect_err("非法帧之后连接不可用");
    assert!(matches!(after, LspError::Closed(_)));

    // 关闭流程在已断裂的连接上仍必须幂等。
    client.shutdown().await.expect("关闭应成功");
}

#[tokio::test]
async fn slow_server_times_out_and_leaves_connection_usable() {
    let mut client = LspClient::start(&mock_config()).await.expect("握手应成功");
    client.set_request_timeout(Duration::from_millis(150));

    let outcome = client
        .request("mock/sleep", Some(json!({"ms": 5_000})))
        .await;
    let error = outcome.expect_err("慢请求必须超时");
    match error {
        LspError::Timeout { ref method, .. } => assert_eq!(method, "mock/sleep"),
        other => panic!("期望 Timeout，实际 {other:?}"),
    }
    assert!(error.is_retryable());
    // 超时只是本地放弃等待；连接没坏（迟到的响应会被读循环按 id 丢弃）。
    assert!(!client.is_closed());

    client.shutdown().await.expect("关闭应成功");
}

#[tokio::test]
async fn oversize_message_body_is_rejected_by_limits() {
    // 把体上限压到 64 字节：握手回包本身就超了，读循环必须直接判协议错误，
    // 而不是把几百字节全读进内存再说。
    let config =
        ServerConfig::new("mock-server", MOCK_BIN).limits(TransportLimits::new(8 * 1024, 64));
    let client = LspClient::connect(&config).await.expect("起进程应成功");

    let outcome = tokio::time::timeout(Duration::from_secs(10), client.initialize()).await;
    let error = match outcome {
        Ok(Err(error)) => error,
        Ok(Ok(_)) => panic!("超限消息不该被接受"),
        Err(_) => panic!("超限保护必须立刻生效，不能挂住"),
    };
    assert!(
        matches!(error, LspError::Protocol(_)),
        "期望 Protocol，实际 {error:?}"
    );
    assert!(client.is_closed());

    client.shutdown().await.expect("关闭应成功");
}
