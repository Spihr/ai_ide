//! `session.export` / `session.fork` 端到端测试（M5 切片三）。
//!
//! 与 `session_flow` 同一套路：起真实内核子进程，只走 stdin / stdout。导出要真的
//! 写文件、分叉要真的落库，只有黑盒跑一遍才算验完。

mod common;

use common::{unique_temp_dir, Core, TempDataDir};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 一个用完即删的临时目录（导出落点 / 工作区都复用它）。
struct TempDir {
    root: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        Self {
            root: unique_temp_dir("ai-ide-m5-export", tag),
        }
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn text_message(role: &str, content: &str) -> Value {
    json!({ "role": role, "content": content })
}

/// 建一个带三条消息的会话，返回 `(session_id, 消息 id 列表)`。
fn seed_session(core: &mut Core, title: &str) -> (String, Vec<String>) {
    let created = core.ok("session.create", json!({ "title": title }));
    let session_id = created["session_id"]
        .as_str()
        .expect("session_id")
        .to_string();

    let mut ids = Vec::new();
    for (index, content) in ["第一问", "第一答", "第二问"].iter().enumerate() {
        let mut params = json!({
            "session_id": session_id,
            "message": text_message(
                if index == 1 { "assistant" } else { "user" },
                content
            ),
        });
        // 串一条父链：fork 若不重映射 parent_id，新会话就会指向旧消息。
        if let Some(previous) = ids.last() {
            params["parent_id"] = json!(previous);
        }
        let appended = core.ok("session.append_message", params);
        ids.push(
            appended["message"]["message_id"]
                .as_str()
                .expect("message_id")
                .to_string(),
        );
    }
    (session_id, ids)
}

#[test]
fn export_writes_markdown_and_json_to_the_requested_path() {
    let data = TempDataDir::new("export-write");
    let out_dir = TempDir::new("out");
    let mut core = Core::spawn(&data.root);

    // 能力清单必须如实包含两个新方法，否则客户端根本不会去调。
    let capabilities = core.capabilities();
    for method in ["session.export", "session.fork"] {
        assert!(
            capabilities.iter().any(|name| name == method),
            "{method} 应在能力清单里，实际：{capabilities:?}"
        );
    }

    let (session_id, _) = seed_session(&mut core, "导出验证 / 会话");

    // 1) 目录入参 + 缺省格式 → 目录里的 .md
    let markdown = core.ok(
        "session.export",
        json!({ "session_id": session_id, "out_path": out_dir.path().to_string_lossy() }),
    );
    assert_eq!(markdown["ok"], json!(true));
    assert_eq!(markdown["messages"], json!(3));
    let markdown_path = PathBuf::from(markdown["path"].as_str().expect("path"));
    assert!(markdown_path.is_absolute(), "返回的必须是绝对路径");
    assert_eq!(
        markdown_path.parent(),
        Some(out_dir.path()),
        "目录入参应折算成目录内的文件"
    );
    assert_eq!(
        markdown_path.extension().and_then(|ext| ext.to_str()),
        Some("md"),
        "缺省格式是 markdown"
    );

    let body = std::fs::read_to_string(&markdown_path).expect("导出的文件应存在");
    assert_eq!(
        markdown["bytes"],
        json!(body.len() as u64),
        "bytes 要如实回报"
    );
    assert!(body.contains("# 导出验证 / 会话"), "Markdown 应带标题");
    assert!(body.contains("| 消息数 | 3 |"));
    assert!(body.contains("第一问") && body.contains("第一答") && body.contains("第二问"));

    // 2) 显式文件路径 + json
    let json_path = out_dir.path().join("会话备份.json");
    let exported = core.ok(
        "session.export",
        json!({
            "session_id": session_id,
            "format": "json",
            "out_path": json_path.to_string_lossy(),
        }),
    );
    assert_eq!(
        PathBuf::from(exported["path"].as_str().expect("path")),
        json_path,
        "指定了文件路径就按指定的写，不要自作主张改名"
    );

    let raw = std::fs::read_to_string(&json_path).expect("JSON 文件应存在");
    let document: Value = serde_json::from_str(&raw).expect("导出内容必须是合法 JSON");
    assert_eq!(document["format_version"], json!(1));
    assert_eq!(document["session"]["session_id"], json!(session_id));
    assert_eq!(document["session"]["title"], json!("导出验证 / 会话"));
    assert_eq!(document["messages"].as_array().map(Vec::len), Some(3));
    assert_eq!(document["messages"][0]["content"], json!("第一问"));
    assert_eq!(document["messages"][1]["role"], json!("assistant"));
    assert!(document["exported_at_ms"].as_i64().unwrap_or(0) > 0);
}

#[test]
fn export_creates_missing_parent_directories() {
    let data = TempDataDir::new("export-mkdir");
    let out_dir = TempDir::new("mkdir");
    let mut core = Core::spawn(&data.root);
    let (session_id, _) = seed_session(&mut core, "深目录");

    // 父目录不存在也要能写：用户给的路径不该由他手动 mkdir 才能用。
    let nested = out_dir.path().join("a").join("b").join("导出");
    let exported = core.ok(
        "session.export",
        json!({ "session_id": session_id, "out_path": nested.to_string_lossy() }),
    );
    let path = PathBuf::from(exported["path"].as_str().expect("path"));
    assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("md"));
    assert!(path.exists(), "父目录应被自动创建");
    assert!(std::fs::read_to_string(&path).unwrap().contains("深目录"));
}

#[test]
fn export_and_fork_errors_are_explicit() {
    let data = TempDataDir::new("export-errors");
    let out_dir = TempDir::new("errors");
    let mut core = Core::spawn(&data.root);

    // 会话不存在：先报 5001，不要让用户以为「导出成功但文件是空的」。
    assert_eq!(
        core.error_code(
            "session.export",
            json!({ "session_id": "no-such-session", "out_path": out_dir.path().to_string_lossy() })
        ),
        5001
    );

    let (session_id, _) = seed_session(&mut core, "错误分支");

    // 相对路径 / 空路径都是参数问题。
    assert_eq!(
        core.error_code(
            "session.export",
            json!({ "session_id": session_id, "out_path": "导出.md" })
        ),
        -32602,
        "相对路径应被拒绝，避免文件落到内核 cwd"
    );
    assert_eq!(
        core.error_code(
            "session.export",
            json!({ "session_id": session_id, "out_path": "   " })
        ),
        -32602
    );

    assert_eq!(
        core.error_code("session.fork", json!({ "session_id": "no-such-session" })),
        5001
    );
    assert_eq!(
        core.error_code(
            "session.fork",
            json!({ "session_id": session_id, "from_message_id": "no-such-message" })
        ),
        -32602,
        "截断点不存在应报参数错误"
    );
}

#[test]
fn fork_copies_whole_conversation_and_truncates_on_request() {
    let data = TempDataDir::new("fork-copy");
    let mut core = Core::spawn(&data.root);
    let (source_id, message_ids) = seed_session(&mut core, "原会话");

    // 全量分叉
    let forked = core.ok("session.fork", json!({ "session_id": source_id }));
    let new_id = forked["new_session_id"]
        .as_str()
        .expect("new_session_id")
        .to_string();
    assert_ne!(new_id, source_id, "必须换一个会话 id");
    assert_eq!(forked["messages"], json!(3));
    assert_eq!(forked["session"]["title"], json!("原会话（分支）"));

    let copied = core.ok("session.get", json!({ "session_id": new_id }));
    let messages = copied["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["content"], json!("第一问"));
    assert_eq!(messages[2]["content"], json!("第二问"));
    assert!(
        messages
            .iter()
            .all(|message| message["session_id"] == json!(new_id)),
        "复制出的消息要挂在新会话下"
    );
    assert_ne!(
        messages[0]["message_id"],
        json!(message_ids[0]),
        "消息 id 也必须换新，否则两个会话会共享同一条消息"
    );
    assert_eq!(
        messages[1]["parent_id"], messages[0]["message_id"],
        "父链要在新 id 空间内重连"
    );

    // 源会话毫发无损
    let original = core.ok("session.get", json!({ "session_id": source_id }));
    assert_eq!(original["messages"].as_array().map(Vec::len), Some(3));
    assert_eq!(original["session"]["title"], json!("原会话"));
    assert_eq!(core.ok("session.list", json!({}))["total"], json!(2));

    // 截断分叉：只到第二条，且允许自定义标题
    let truncated = core.ok(
        "session.fork",
        json!({
            "session_id": source_id,
            "from_message_id": message_ids[1],
            "title": "只到第一答",
        }),
    );
    assert_eq!(truncated["messages"], json!(2));
    assert_eq!(truncated["session"]["title"], json!("只到第一答"));
    let truncated_id = truncated["new_session_id"].as_str().expect("id");
    let truncated_view = core.ok("session.get", json!({ "session_id": truncated_id }));
    assert_eq!(truncated_view["messages"].as_array().map(Vec::len), Some(2));

    // 跨会话借消息 id 当截断点：必须报错，不能悄悄复制全部
    let other = core.ok("session.create", json!({ "title": "别的会话" }));
    let other_id = other["session_id"].as_str().expect("id");
    let foreign = core.ok(
        "session.append_message",
        json!({
            "session_id": other_id,
            "message": text_message("user", "别人的消息"),
        }),
    );
    let foreign_message_id = foreign["message"]["message_id"].as_str().expect("id");
    assert_eq!(
        core.error_code(
            "session.fork",
            json!({ "session_id": source_id, "from_message_id": foreign_message_id })
        ),
        -32602
    );

    // 失败不产生半截会话：源 + 两个分叉 + 别的会话 = 4
    assert_eq!(core.ok("session.list", json!({}))["total"], json!(4));
}

#[test]
fn exported_file_survives_and_matches_store_after_restart() {
    let data = TempDataDir::new("export-restart");
    let out_dir = TempDir::new("restart-out");
    let (session_id, path) = {
        let mut core = Core::spawn(&data.root);
        let (session_id, _) = seed_session(&mut core, "重启前导出");
        let exported = core.ok(
            "session.export",
            json!({ "session_id": session_id, "out_path": out_dir.path().to_string_lossy() }),
        );
        (
            session_id,
            PathBuf::from(exported["path"].as_str().expect("path")),
        )
    };

    assert!(path.exists(), "内核退出后导出文件仍要在");

    // 重启后重新导出到另一个文件，内容应一致（导出纯粹由库内数据决定）。
    let mut core = Core::spawn(&data.root);
    let second = out_dir.path().join("重启后再导出.md");
    let exported = core.ok(
        "session.export",
        json!({ "session_id": session_id, "out_path": second }),
    );
    let first_body = std::fs::read_to_string(&path).unwrap();
    let second_body = std::fs::read_to_string(exported["path"].as_str().unwrap()).unwrap();
    // 头部含导出时间，只比对消息区段。
    let tail = |text: &str| text.split("---").last().unwrap_or_default().to_string();
    assert_eq!(tail(&first_body), tail(&second_body));
    assert!(second_body.contains("第二问"));
}
