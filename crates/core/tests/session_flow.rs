//! `session.*` 端到端测试（M5 切片二）。
//!
//! 和 `provider_flow` / `llm_flow` 一样跑真实子进程：会话要落盘到 SQLite，只有把内核
//! 当成黑盒、只走 stdin / stdout，才能顺带验证「迁移在启动时跑过」「重启后还在」
//! 这两件存储层单测管不到的事。

mod common;

use common::{Core, TempDataDir};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 一个用完即删的临时工作区目录。
struct TempDir {
    root: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "ai-ide-m5-ws-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("建工作区目录");
        Self { root }
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

/// 完整生命周期：建 → 追加 → 读 → 改名 → 分页 → 删。
#[test]
fn session_lifecycle_round_trip() {
    let data = TempDataDir::new("session-lifecycle");
    let workspace = TempDir::new("lifecycle");
    let mut core = Core::spawn(&data.root);

    // 能力清单先对一遍：`core.hello` 是客户端判断能不能用 `session.*` 的唯一依据。
    let capabilities = core.capabilities();
    for method in [
        "session.create",
        "session.list",
        "session.get",
        "session.update",
        "session.delete",
        "session.append_message",
        "session.messages",
    ] {
        assert!(
            capabilities.iter().any(|name| name == method),
            "{method} 应在能力清单里，实际：{capabilities:?}"
        );
    }

    let opened = core.ok("workspace.open", json!({ "path": workspace.path() }));
    let workspace_id = opened["workspace_id"]
        .as_str()
        .expect("workspace_id")
        .to_string();

    // 不给标题：内核兜底「新会话」，客户端不必自己编。
    let created = core.ok("session.create", json!({ "workspace_id": workspace_id }));
    let session_id = created["session_id"]
        .as_str()
        .expect("session_id")
        .to_string();
    assert_eq!(created["session"]["title"], json!("新会话"));
    assert_eq!(
        created["session"]["workspace_path"].as_str(),
        Some(
            workspace
                .path()
                .to_string_lossy()
                .replace('\\', "/")
                .as_str()
        ),
        "工作区路径按正斜杠落盘"
    );
    assert_eq!(
        created["session"]["created_at_ms"], created["session"]["updated_at_ms"],
        "新建会话两个时间戳相等"
    );

    // 两条消息：user 在前，assistant 挂在它下面。
    let user = core.ok(
        "session.append_message",
        json!({
            "session_id": session_id,
            "message": text_message("user", "帮我看看这个仓库"),
        }),
    );
    let user_message_id = user["message"]["message_id"]
        .as_str()
        .expect("message_id")
        .to_string();
    assert_eq!(user["message"]["role"], json!("user"));
    assert_eq!(user["message"]["content"], json!("帮我看看这个仓库"));

    let assistant = core.ok(
        "session.append_message",
        json!({
            "session_id": session_id,
            "message": text_message("assistant", "先看目录结构"),
            "parent_id": user_message_id,
            "usage": { "input_tokens": 12, "output_tokens": 5 },
        }),
    );
    assert_eq!(assistant["message"]["parent_id"], json!(user_message_id));
    assert_eq!(
        assistant["message"]["usage"]["input_tokens"],
        json!(12),
        "usage 原样保存"
    );

    // 列表：一条，且 total 反映过滤后总数。
    let listed = core.ok("session.list", json!({ "workspace_id": workspace_id }));
    assert_eq!(listed["total"], json!(1));
    assert_eq!(listed["sessions"][0]["session_id"], json!(session_id));

    // get 默认带消息，顺序按写入先后。
    let fetched = core.ok("session.get", json!({ "session_id": session_id }));
    let messages = fetched["messages"].as_array().expect("messages 数组");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], json!("user"));
    assert_eq!(messages[1]["role"], json!("assistant"));

    // include_messages=false 时只有会话本体，不白拖一份历史。
    let bare = core.ok(
        "session.get",
        json!({ "session_id": session_id, "include_messages": false }),
    );
    assert_eq!(bare["messages"].as_array().map(Vec::len), Some(0));

    // 分页：会话消息按时间正序，offset=1 应该只拿到第二条。
    let page = core.ok(
        "session.messages",
        json!({ "session_id": session_id, "limit": 1, "offset": 1 }),
    );
    assert_eq!(page["total"], json!(2));
    assert_eq!(page["messages"].as_array().map(Vec::len), Some(1));
    assert_eq!(page["messages"][0]["role"], json!("assistant"));

    // 改名：返回刷新后的快照，客户端不必再补一次 get。
    let renamed = core.ok(
        "session.update",
        json!({ "session_id": session_id, "title": "  仓库导览  " }),
    );
    assert_eq!(renamed["title"], json!("仓库导览"), "标题应去掉首尾空白");
    assert!(
        renamed["updated_at_ms"].as_i64().unwrap()
            >= created["session"]["updated_at_ms"].as_i64().unwrap()
    );

    // 关键词过滤：命中 / 不命中都试一次。这里命中的是**标题**。
    let hit = core.ok("session.list", json!({ "query": "导览" }));
    assert_eq!(hit["total"], json!(1));
    let miss = core.ok("session.list", json!({ "query": "不存在的关键词" }));
    assert_eq!(miss["total"], json!(0));

    // 关键词也搜**消息正文**：标题里没有「目录结构」，命中的是 assistant 那条。
    let by_body = core.ok("session.list", json!({ "query": "目录结构" }));
    assert_eq!(by_body["total"], json!(1), "正文命中也要算进 total");
    assert_eq!(by_body["sessions"][0]["session_id"], json!(session_id));

    // 正文搜索 + 分页：命中 1 条时 offset=1 必须给空页，且 total 仍是 1（不是 0）。
    let paged = core.ok(
        "session.list",
        json!({ "query": "目录结构", "limit": 10, "offset": 1 }),
    );
    assert_eq!(paged["total"], json!(1));
    assert_eq!(paged["sessions"].as_array().map(Vec::len), Some(0));

    // LIKE 通配符必须被当字面量：库里没有 `%`，所以搜 `%` 应当 0 命中。
    let wildcard = core.ok("session.list", json!({ "query": "%" }));
    assert_eq!(wildcard["total"], json!(0), "% 不能当通配符用");

    // 删除是幂等的：同一个 id 删两次，第二次 deleted = 0 而不是报错。
    let deleted = core.ok("session.delete", json!({ "session_ids": [session_id] }));
    assert_eq!(deleted["ok"], json!(true));
    assert_eq!(deleted["deleted"], json!(1));
    let again = core.ok("session.delete", json!({ "session_ids": [session_id] }));
    assert_eq!(again["deleted"], json!(0));

    // 删完之后再读，必须是 5001 而不是空会话——两者对 UI 的意义完全不同。
    assert_eq!(
        core.error_code("session.get", json!({ "session_id": session_id })),
        5001
    );
}

/// 杀进程重启：会话与消息必须原样回来（M5 验收硬指标）。
#[test]
fn sessions_survive_core_restart() {
    let data = TempDataDir::new("session-restart");
    let session_id;

    {
        let mut core = Core::spawn(&data.root);
        let created = core.ok("session.create", json!({ "title": "重启前" }));
        session_id = created["session_id"]
            .as_str()
            .expect("session_id")
            .to_string();
        core.ok(
            "session.append_message",
            json!({
                "session_id": session_id,
                "message": text_message("user", "重启后我还得在"),
            }),
        );
        // Drop 里会发 core.shutdown 并回收进程，落盘的 WAL 在这里收尾。
    }

    assert!(
        data.root.join("sessions.db").exists(),
        "会话库应落在 AI_IDE_DATA_DIR 下"
    );

    let mut core = Core::spawn(&data.root);
    let listed = core.ok("session.list", json!({}));
    assert_eq!(listed["total"], json!(1), "重启后会话仍在");
    assert_eq!(listed["sessions"][0]["title"], json!("重启前"));

    let fetched = core.ok("session.get", json!({ "session_id": session_id }));
    let messages = fetched["messages"].as_array().expect("messages 数组");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["content"], json!("重启后我还得在"));
}

/// 错误分支要说清楚话：参数错报 -32602，对象不存在报 5001，工作区没开报 1001。
#[test]
fn session_errors_are_explicit() {
    let data = TempDataDir::new("session-errors");
    let mut core = Core::spawn(&data.root);

    // 未知会话
    assert_eq!(
        core.error_code("session.get", json!({ "session_id": "no-such-session" })),
        5001
    );
    assert_eq!(
        core.error_code(
            "session.messages",
            json!({ "session_id": "no-such-session" })
        ),
        5001
    );
    assert_eq!(
        core.error_code(
            "session.append_message",
            json!({
                "session_id": "no-such-session",
                "message": text_message("user", "hi"),
            })
        ),
        5001,
        "往不存在的会话里追加消息必须报错，不能凭空建出来"
    );

    // 参数本身不成立
    assert_eq!(
        core.error_code("session.update", json!({ "session_id": "x" })),
        -32602,
        "title 与 model_ref 都不给应报参数错误"
    );

    let created = core.ok("session.create", json!({ "title": "参数校验" }));
    let session_id = created["session_id"]
        .as_str()
        .expect("session_id")
        .to_string();
    assert_eq!(
        core.error_code(
            "session.update",
            json!({ "session_id": session_id, "title": "   " })
        ),
        -32602,
        "空白标题应被拒绝，而不是悄悄存成空串"
    );
    assert_eq!(
        core.error_code("session.delete", json!({ "session_ids": [] })),
        -32602
    );

    // 没打开的工作区
    assert_eq!(
        core.error_code(
            "session.create",
            json!({ "workspace_id": "00000000-0000-4000-8000-00000000dead" })
        ),
        1001,
        "工作区不存在应报 1001"
    );
    assert_eq!(
        core.error_code(
            "session.list",
            json!({ "workspace_id": "00000000-0000-4000-8000-00000000dead" })
        ),
        1001
    );
}
