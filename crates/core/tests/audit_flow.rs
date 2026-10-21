//! `audit.list` 与会话埋点的端到端测试（M5 切片四）。
//!
//! 和其余 core 集成测试同一套路：起真实内核子进程，只通过 stdin / stdout 说话。
//! 审计这东西最怕「代码里写了、跑起来没有」，所以这里不测内部函数，直接问内核：
//! 我做了这些操作，账本上到底有没有、脱没脱敏、删完会话还在不在。

mod common;

use common::{Core, TempDataDir};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 一个用完即删的临时目录（导出落点）。
struct TempDir {
    root: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "ai-ide-m5-audit-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("建临时目录");
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

fn create_session(core: &mut Core, title: &str) -> String {
    let created = core.ok("session.create", json!({ "title": title }));
    created["session_id"]
        .as_str()
        .expect("session_id")
        .to_string()
}

/// 该会话的全部审计事件（按内核返回顺序：时间倒序）。
fn events_of(core: &mut Core, session_id: &str) -> Vec<Value> {
    let listed = core.ok("audit.list", json!({ "session_id": session_id }));
    listed["events"].as_array().expect("events 是数组").clone()
}

fn kinds(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| event["kind"].as_str().expect("kind 是字符串").to_string())
        .collect()
}

#[test]
fn capabilities_advertise_audit_list() {
    let data = TempDataDir::new("audit-caps");
    let mut core = Core::spawn(&data.root);

    let capabilities = core.capabilities();
    assert!(
        capabilities.iter().any(|name| name == "audit.list"),
        "能力清单缺少 audit.list：{capabilities:?}"
    );
    // 台账是只读查询面，不该顺手多出别的方法。
    assert_eq!(
        capabilities
            .iter()
            .filter(|name| name.starts_with("audit."))
            .count(),
        1,
        "audit 域只应有 audit.list：{capabilities:?}"
    );
}

#[test]
fn session_lifecycle_is_recorded_in_order() {
    let data = TempDataDir::new("audit-lifecycle");
    let out_dir = TempDir::new("out");
    let mut core = Core::spawn(&data.root);

    let session_id = create_session(&mut core, "要留痕的会话");
    core.ok(
        "session.update",
        json!({ "session_id": session_id, "title": "改了名的会话" }),
    );
    core.ok(
        "session.append_message",
        json!({
            "session_id": session_id,
            "message": { "role": "user", "content": "第一问" },
        }),
    );
    core.ok(
        "session.export",
        json!({
            "session_id": session_id,
            "format": "markdown",
            "out_path": out_dir.path().join("会话.md").to_string_lossy(),
        }),
    );
    core.ok(
        "session.fork",
        json!({ "session_id": session_id, "title": "分支会话" }),
    );

    let events = events_of(&mut core, &session_id);
    // 倒序：最近的写在最前，所以把返回顺序反过来比对更直观。
    // 分叉事件挂在**新**会话上（见下），所以源会话这里只有三件。
    let mut recorded = kinds(&events);
    recorded.reverse();
    assert_eq!(
        recorded,
        vec!["session.created", "session.updated", "session.exported"],
        "实际事件：{events:#?}"
    );

    // 摘要必须是人话，且带上可追溯的信息。
    let created = events
        .iter()
        .find(|event| event["kind"] == json!("session.created"))
        .expect("session.created");
    assert_eq!(created["actor"], json!("core"));
    assert!(
        created["summary"]
            .as_str()
            .unwrap_or_default()
            .contains("要留痕的会话"),
        "摘要没带标题：{}",
        created["summary"]
    );

    // 导出事件要留下落盘路径，否则「谁把会话导走了」查不出来。
    let exported = events
        .iter()
        .find(|event| event["kind"] == json!("session.exported"))
        .expect("session.exported");
    assert!(
        exported["detail"]["path"]
            .as_str()
            .unwrap_or_default()
            .ends_with("会话.md"),
        "导出事件没记路径：{}",
        exported["detail"]
    );

    // 时间戳是 Unix 毫秒数字（不是字符串、不是 bigint）。2000 年之后就算合理。
    let created_at = created["created_at_ms"]
        .as_i64()
        .expect("created_at_ms 是数字");
    assert!(created_at > 946_684_800_000, "时间戳不像毫秒：{created_at}");
}

#[test]
fn fork_is_recorded_on_the_new_session_with_its_source() {
    let data = TempDataDir::new("audit-fork");
    let mut core = Core::spawn(&data.root);

    let source = create_session(&mut core, "源会话");
    let forked = core.ok("session.fork", json!({ "session_id": source }));
    let new_id = forked["new_session_id"].as_str().expect("new_session_id");

    // 新会话自己的账：创建（隐式）+ 分叉。
    let events = events_of(&mut core, new_id);
    let forked_event = events
        .iter()
        .find(|event| event["kind"] == json!("session.forked"))
        .unwrap_or_else(|| panic!("新会话上没有分叉事件：{events:#?}"));

    // 关键：从新会话能倒查到源会话，否则「这个分支哪儿来的」就断了。
    assert_eq!(forked_event["detail"]["source_session_id"], json!(source));
    assert_eq!(forked_event["detail"]["new_session_id"], json!(new_id));
    assert!(
        forked_event["summary"]
            .as_str()
            .unwrap_or_default()
            .contains("源会话"),
        "分叉摘要没交代源会话：{}",
        forked_event["summary"]
    );
}

#[test]
fn delete_is_recorded_and_the_trail_outlives_the_session() {
    let data = TempDataDir::new("audit-delete");
    let mut core = Core::spawn(&data.root);

    let session_id = create_session(&mut core, "待删除");
    let deleted = core.ok("session.delete", json!({ "session_ids": [session_id] }));
    assert_eq!(deleted["deleted"], json!(1));

    // 会话没了。
    let code = core.error_code("session.get", json!({ "session_id": session_id }));
    assert_eq!(code, 5001, "会话应已不存在");

    // 但账还在，而且记着「被删过」。
    let events = events_of(&mut core, &session_id);
    let recorded = kinds(&events);
    assert!(
        recorded.contains(&"session.deleted".to_string()),
        "删除没留痕：{recorded:?}"
    );
    assert!(
        recorded.contains(&"session.created".to_string()),
        "创建记录不该随会话消失：{recorded:?}"
    );
}

#[test]
fn secrets_in_titles_never_reach_the_ledger() {
    let data = TempDataDir::new("audit-redact");
    let mut core = Core::spawn(&data.root);

    // 标题里塞明文密钥是真实会发生的：用户从聊天窗口复制一段带 token 的文本当标题。
    let secret = "sk-live-0123456789abcdef";
    let session_id = create_session(&mut core, &format!("帮我查下 {secret} 是什么"));

    let raw = core.ok("audit.list", json!({}));
    let text = raw.to_string();
    assert!(
        !text.contains(secret),
        "audit.list 回包里有明文密钥：{text}"
    );

    // 具体落点也确认一遍：摘要与 detail 都脱敏。
    let events = events_of(&mut core, &session_id);
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert!(
        !event["summary"]
            .as_str()
            .unwrap_or_default()
            .contains(secret),
        "摘要没脱敏：{}",
        event["summary"]
    );
    assert!(
        !event["detail"].to_string().contains(secret),
        "detail 没脱敏：{}",
        event["detail"]
    );
    // 脱敏不该把整段文本抹平——标题的其余部分仍要可读。
    assert!(
        event["summary"]
            .as_str()
            .unwrap_or_default()
            .contains("帮我查下"),
        "脱敏过头了：{}",
        event["summary"]
    );
}

#[test]
fn filters_paging_and_total_are_consistent() {
    let data = TempDataDir::new("audit-filters");
    let mut core = Core::spawn(&data.root);

    let first = create_session(&mut core, "甲");
    let second = create_session(&mut core, "乙");

    // 全量：两条创建 + 一条重命名。
    core.ok(
        "session.update",
        json!({ "session_id": second, "title": "乙2" }),
    );
    let all = core.ok("audit.list", json!({}));
    assert_eq!(all["total"], json!(3), "total 应等于命中总数：{all}");
    assert_eq!(all["events"].as_array().unwrap().len(), 3);

    // 按会话过滤。
    let only_first = core.ok("audit.list", json!({ "session_id": first }));
    assert_eq!(only_first["total"], json!(1));

    // 按种类过滤。
    let updated = core.ok("audit.list", json!({ "kind": "session.updated" }));
    assert_eq!(updated["total"], json!(1));
    assert_eq!(updated["events"][0]["session_id"], json!(second));

    // 分页：total 不受 limit 影响，翻页不重不漏。
    let page1 = core.ok("audit.list", json!({ "limit": 2 }));
    assert_eq!(page1["total"], json!(3), "limit 不该改变 total");
    assert_eq!(page1["events"].as_array().unwrap().len(), 2);
    let page2 = core.ok("audit.list", json!({ "limit": 2, "offset": 2 }));
    assert_eq!(page2["events"].as_array().unwrap().len(), 1);
    let mut ids: Vec<i64> = page1["events"]
        .as_array()
        .unwrap()
        .iter()
        .chain(page2["events"].as_array().unwrap().iter())
        .map(|event| event["event_id"].as_i64().unwrap())
        .collect();
    let count = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), count, "两页出现了重复事件");

    // 空字符串等于不过滤：前端表单清空时不该静默变成「零命中」。
    let blank = core.ok("audit.list", json!({ "session_id": "", "kind": "   " }));
    assert_eq!(blank["total"], json!(3));

    // 时间窗倒挂是参数错误，不是「空结果」。
    let code = core.error_code(
        "audit.list",
        json!({ "since_ms": 2_000, "until_ms": 1_000 }),
    );
    assert_eq!(code, -32602, "since_ms > until_ms 应报参数错误");
}
