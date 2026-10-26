//! 高层能力端到端测试：注册表 → 自动同步 → 四个查询方法 → 结果归一。
//!
//! 全部走真的子进程（`lsp-mock-server`）与真的 stdio 管道，理由和
//! `stdio_session.rs` 一样：单测能保证「解析逻辑对」，但保证不了「请求前真的
//! 先发了 didOpen」「同一份文本没有重复 didChange」——而这两件事恰恰是真实
//! 服务器上最难查的故障来源（表现为永远查不到结果，或索引被无谓重算）。
//!
//! 假服务器对未打开文档的查询回 `-32000`，所以「漏了自动 didOpen」在这里是
//! **硬失败**，不会被一个空结果蒙混过去。

use std::path::PathBuf;

use ai_ide_lsp::{
    CommandSpec, LanguageRegistry, LanguageServerSpec, LspClient, LspError, Position, ServerConfig,
    ServerOverrides,
};
use serde_json::json;

const MOCK_BIN: &str = env!("CARGO_BIN_EXE_lsp-mock-server");

fn mock_config() -> ServerConfig {
    ServerConfig::new("mock-server", MOCK_BIN)
}

/// `.tsx` 用细粒度语言标识（`typescriptreact`），同时验证中文路径的 uri 编码。
fn tsx_path() -> PathBuf {
    PathBuf::from(r"E:\ai_ide\crates\lsp\示例\组件.tsx")
}

#[tokio::test]
async fn definition_normalizes_mixed_locations_to_one_based_positions() {
    let client = LspClient::start(&mock_config()).await.expect("握手应成功");

    // 入参 0-based：(3, 7) 即第 4 行第 8 列。
    let locations = client
        .definition(&tsx_path(), "const value = 1;", Position::new(3, 7))
        .await
        .expect("definition 应成功");

    // 服务器回了三条，第三条缺 range，必须被丢掉而不是给个坐标为空的位置。
    assert_eq!(locations.len(), 2, "坏条目应被跳过：{locations:?}");

    // 普通 Location：服务器按 (line+2, character) 回，客户端 +1 后给 1-based。
    assert_eq!(locations[0].range.start.line, 3 + 2 + 1);
    assert_eq!(locations[0].range.start.character, 7 + 1);
    assert_eq!(locations[0].range.end.character, 7 + 5 + 1);
    assert_eq!(
        locations[0].path,
        Some(PathBuf::from("E:/ai_ide/crates/lsp/示例/组件.tsx"))
    );
    // uri 里的中文必须是百分号编码后的形态，脱过一轮再比对。
    assert!(
        locations[0].uri.contains("%E7%A4%BA%E4%BE%8B"),
        "{}",
        locations[0].uri
    );

    // LocationLink：优先用 targetSelectionRange（标识符本身），不是 targetRange。
    assert_eq!(
        locations[1].path,
        Some(PathBuf::from("E:/ai_ide/src/中文.ts"))
    );
    assert_eq!(locations[1].range.start.line, 5);
    assert_eq!(locations[1].range.start.character, 7);
    assert_eq!(locations[1].range.end.line, 5);
    assert_eq!(locations[1].range.end.character, 12);

    // 两端都要确认：服务器真的收到了查询（而不是客户端自己编的结果）。
    let state = client.request("mock/state", None).await.expect("查询状态");
    assert_eq!(state["requests"]["textDocument/definition"], json!(1));

    client.shutdown().await.expect("关闭应成功");
}

#[tokio::test]
async fn references_forward_include_declaration_to_the_server() {
    let client = LspClient::start(&mock_config()).await.expect("握手应成功");
    let text = "function alpha() {}";

    let with_declaration = client
        .references(&tsx_path(), text, Position::start(), true)
        .await
        .expect("references 应成功");
    assert_eq!(with_declaration.len(), 2, "含声明应是 2 条");
    assert_eq!(
        with_declaration[0].range.start,
        ai_ide_lsp::SourcePosition::new(1, 1),
        "0-based (0,0) 必须映射成 1-based (1,1)"
    );
    assert_eq!(
        with_declaration[1].range.start.line, 2,
        "第 2 条回的是 0-based line 1"
    );

    // 同一份文本、同一个 uri：不该再触发 didChange。
    let without_declaration = client
        .references(&tsx_path(), text, Position::start(), false)
        .await
        .expect("references 应成功");
    assert_eq!(without_declaration.len(), 1, "不含声明应是 1 条");

    let state = client.request("mock/state", None).await.expect("查询状态");
    assert_eq!(state["requests"]["textDocument/references"], json!(2));
    assert_eq!(state["didOpen"], json!(1));
    assert_eq!(state["didChange"], json!(0), "文本没变就不该推版本");

    client.shutdown().await.expect("关闭应成功");
}

#[tokio::test]
async fn hover_returns_markup_and_maps_null_to_none() {
    let client = LspClient::start(&mock_config()).await.expect("握手应成功");

    let hover = client
        .hover(&tsx_path(), "const value = 1;", Position::new(2, 4))
        .await
        .expect("hover 应成功")
        .expect("非空文档必须有悬停内容");
    // MarkedString 数组按顺序拼接。
    assert_eq!(hover.contents, "const value: number\n\n来自 mock 的说明");
    let range = hover.range.expect("mock 回了 range");
    assert_eq!(range.start, ai_ide_lsp::SourcePosition::new(3, 1));
    assert_eq!(range.end, ai_ide_lsp::SourcePosition::new(3, 6));

    // 空文档：服务器回 null，客户端归一成 None（不是错误）。
    let empty = client
        .hover(&tsx_path(), "", Position::start())
        .await
        .expect("空文档 hover 不应报错");
    assert!(empty.is_none());

    client.shutdown().await.expect("关闭应成功");
}

#[tokio::test]
async fn completion_normalizes_items_from_a_list_response() {
    let client = LspClient::start(&mock_config()).await.expect("握手应成功");

    let completion = client
        .completion(&tsx_path(), "al", Position::new(0, 2))
        .await
        .expect("completion 应成功");
    assert!(completion.is_incomplete, "服务器声明结果不完整");
    assert_eq!(completion.len(), 2);
    assert_eq!(completion.items[0].label, "alpha");
    assert_eq!(completion.items[0].kind, Some(3));
    assert_eq!(completion.items[0].detail.as_deref(), Some("() => void"));
    assert_eq!(completion.items[0].insert_text.as_deref(), Some("alpha()"));
    assert_eq!(completion.items[1].label, "beta");
    assert_eq!(completion.items[1].kind, Some(6));
    assert!(completion.items[1].detail.is_none());

    client.shutdown().await.expect("关闭应成功");
}

#[tokio::test]
async fn queries_open_the_document_once_and_only_resync_on_real_changes() {
    let client = LspClient::start(&mock_config()).await.expect("握手应成功");

    // 第一次查询：文档没打开过，必须自动 didOpen（版本 1）。
    client
        .hover(&tsx_path(), "const a = 1;", Position::new(0, 0))
        .await
        .expect("首次查询应成功");
    let state = client.request("mock/state", None).await.expect("查询状态");
    assert_eq!(state["didOpen"], json!(1));
    assert_eq!(state["didChange"], json!(0));
    assert_eq!(state["documents"][0]["version"], json!(1));
    // didOpen 的 languageId 用细粒度标识，服务器才能按 TSX 语法解析。
    assert_eq!(
        state["documents"][0]["languageId"],
        json!("typescriptreact")
    );

    // 内容没变：不重复 didOpen、不推版本。
    client
        .definition(&tsx_path(), "const a = 1;", Position::new(0, 0))
        .await
        .expect("第二次查询应成功");
    let state = client.request("mock/state", None).await.expect("查询状态");
    assert_eq!(state["didOpen"], json!(1), "不该重复 didOpen");
    assert_eq!(state["didChange"], json!(0), "同文本不该 didChange");
    assert_eq!(state["documents"][0]["version"], json!(1));

    // 内容变了：didChange(full) 且版本推进到 2，服务器拿到整篇新文本。
    client
        .hover(&tsx_path(), "const a = 2;", Position::new(0, 0))
        .await
        .expect("变更后查询应成功");
    let state = client.request("mock/state", None).await.expect("查询状态");
    assert_eq!(state["didChange"], json!(1));
    assert_eq!(state["documents"][0]["version"], json!(2));
    assert_eq!(state["documents"][0]["text"], json!("const a = 2;"));

    // 再查同一份新文本：版本停在 2。
    client
        .completion(&tsx_path(), "const a = 2;", Position::new(0, 0))
        .await
        .expect("第三次查询应成功");
    let state = client.request("mock/state", None).await.expect("查询状态");
    assert_eq!(state["didChange"], json!(1));
    assert_eq!(state["documents"][0]["version"], json!(2));

    client.shutdown().await.expect("关闭应成功");
}

#[tokio::test]
async fn sync_document_is_idempotent_and_reports_versions() {
    let client = LspClient::start(&mock_config()).await.expect("握手应成功");

    let first = client
        .sync_document(&tsx_path(), "let x = 1;")
        .await
        .expect("首次同步");
    assert!(first.opened && !first.changed);
    assert_eq!(first.version, 1);

    let second = client
        .sync_document(&tsx_path(), "let x = 1;")
        .await
        .expect("重复同步");
    assert!(!second.opened && !second.changed);
    assert_eq!(second.version, 1);

    let third = client
        .sync_document(&tsx_path(), "let x = 2;")
        .await
        .expect("内容变更同步");
    assert!(!third.opened && third.changed);
    assert_eq!(third.version, 2);

    client.shutdown().await.expect("关闭应成功");
}

#[tokio::test]
async fn registry_resolves_the_mock_server_and_reports_missing_commands() {
    let registry = LanguageRegistry::builtin();

    // 用覆盖把 mock 二进制冒充成 ts 服务器：验证覆盖真的落地到 ServerConfig。
    let overrides = ServerOverrides::new().command(MOCK_BIN);
    let resolved = registry
        .resolve_for_path(&tsx_path(), &overrides)
        .expect("覆盖命令后应可解析");
    assert_eq!(resolved.language_id, "typescript");
    assert_eq!(resolved.config.command, MOCK_BIN);
    assert_eq!(resolved.config.args, vec!["--stdio".to_string()]);
    assert!(!resolved.used_fallback);

    // 命令不存在：明确的 NotInstalled，而不是 spawn 失败或 panic。
    let error = registry
        .resolve_for_path(
            &tsx_path(),
            &ServerOverrides::new().command("ai-ide-missing-lsp-binary-xyz"),
        )
        .expect_err("命令不存在必须报错");
    assert!(matches!(error, LspError::NotInstalled { .. }), "{error}");
    assert!(error.to_string().contains("未安装"), "{error}");

    // 未登记扩展名：UnsupportedLanguage（可修，不是致命错）。
    let error = registry
        .resolve_for_path(
            &PathBuf::from("E:/ai_ide/notes.unknownext"),
            &ServerOverrides::new(),
        )
        .expect_err("未登记扩展名必须报错");
    assert!(
        matches!(error, LspError::UnsupportedLanguage { .. }),
        "{error}"
    );
}

#[tokio::test]
async fn resolved_config_can_actually_start_a_session() {
    // 注册表 → ServerConfig → 真起进程：串起「配置」和「能跑」两件事。
    let registry = LanguageRegistry::new(vec![LanguageServerSpec::new(
        "typescript",
        &["tsx"],
        CommandSpec::new(MOCK_BIN, &[]),
    )
    .with_hint("装 typescript-language-server")]);
    let resolved = registry
        .resolve_for_path(&tsx_path(), &ServerOverrides::new())
        .expect("应可解析");

    let client = LspClient::start(&resolved.config)
        .await
        .expect("应能起会话");
    let locations = client
        .definition(&tsx_path(), "const a = 1;", Position::new(0, 0))
        .await
        .expect("起会话后查询应成功");
    assert_eq!(locations.len(), 2, "坏条目仍应被过滤");

    client.shutdown().await.expect("关闭应成功");
}
