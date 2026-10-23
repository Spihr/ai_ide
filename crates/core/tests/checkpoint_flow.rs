//! `checkpoint.*` 的端到端测试（M5 切片五）。
//!
//! 检查点是「出事能回去」的最后一道保险，它最怕的不是算法错，而是**链路假装通**：
//! 快照建在别处、回滚写完没校验、审计里查不到。所以这里一律起真实内核子进程，
//! 只通过 stdin / stdout 说话，断言落在真实磁盘内容上。

mod common;

use common::{unique_temp_dir, Core, TempDataDir};
use serde_json::json;
use std::path::{Path, PathBuf};

/// 一个用完即删的临时工作区。
struct TempWorkspace {
    root: PathBuf,
}

impl TempWorkspace {
    fn new(tag: &str) -> Self {
        let root = unique_temp_dir("ai-ide-m5-ckpt", tag);
        std::fs::create_dir_all(root.join("src")).expect("建工作区");
        // 覆盖规则文件也顺手铺一个：快照要按忽略规则跳过它，才谈得上「快照里没有噪音」。
        std::fs::write(root.join(".gitignore"), "target/\nnode_modules/\n").expect("写忽略规则");
        std::fs::write(root.join("a.txt"), "v1\n").expect("写 a.txt");
        std::fs::write(root.join("src").join("main.rs"), "fn main() {}\n").expect("写 main.rs");
        Self { root }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path(rel)).expect("读工作区文件")
    }

    fn write(&self, rel: &str, content: &str) {
        std::fs::write(self.path(rel), content).expect("写工作区文件");
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
    result["workspace_id"]
        .as_str()
        .expect("workspace_id 必须是字符串")
        .to_string()
}

fn kinds(core: &mut Core) -> Vec<String> {
    let listed = core.ok("audit.list", json!({ "limit": 100 }));
    listed["events"]
        .as_array()
        .expect("events 是数组")
        .iter()
        .map(|event| event["kind"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn capabilities_advertise_the_whole_checkpoint_domain() {
    let data = TempDataDir::new("ckpt-caps");
    let mut core = Core::spawn(&data.root);

    let capabilities = core.capabilities();
    let mut domain: Vec<&str> = capabilities
        .iter()
        .filter(|name| name.starts_with("checkpoint."))
        .map(String::as_str)
        .collect();
    domain.sort_unstable();
    assert_eq!(
        domain,
        vec![
            "checkpoint.create",
            "checkpoint.delete",
            "checkpoint.list",
            "checkpoint.restore",
        ],
        "检查点只该有这四个方法：{capabilities:?}"
    );
}

#[test]
fn snapshot_restore_delete_round_trip() {
    let data = TempDataDir::new("ckpt-roundtrip");
    let ws = TempWorkspace::new("roundtrip");
    let mut core = Core::spawn(&data.root);
    let workspace_id = open(&mut core, &ws.root);

    // 1) 拍快照：清单里带相对路径，忽略规则里的目录不该出现。
    let created = core.ok(
        "checkpoint.create",
        json!({ "workspace_id": workspace_id, "label": "改之前" }),
    );
    let checkpoint = &created["checkpoint"];
    let checkpoint_id = checkpoint["checkpoint_id"]
        .as_str()
        .expect("checkpoint_id")
        .to_string();
    assert_eq!(checkpoint["label"], json!("改之前"));
    assert_eq!(checkpoint["origin"], json!("manual"));
    let files = checkpoint["files"].as_array().expect("files 是数组");
    let paths: Vec<&str> = files
        .iter()
        .map(|file| file["path"].as_str().unwrap_or_default())
        .collect();
    assert!(paths.contains(&"a.txt"), "快照应覆盖 a.txt：{paths:?}");
    assert!(
        paths.contains(&"src/main.rs"),
        "快照应覆盖子目录文件：{paths:?}"
    );
    assert!(
        !paths.iter().any(|path| path.starts_with("target/")),
        "忽略规则内的文件不该进快照：{paths:?}"
    );

    // 2) 改动磁盘：改一个已有文件、删一个、再新建一个。
    ws.write("a.txt", "v2 — 被 Agent 改坏了\n");
    std::fs::remove_file(ws.path("src/main.rs")).expect("删 main.rs");
    ws.write("b.txt", "快照之后才有的文件\n");

    // 3) 回滚：逐字节校验，报告要说清每个文件被怎么处理的。
    let restored = core.ok(
        "checkpoint.restore",
        json!({ "checkpoint_id": checkpoint_id }),
    );
    assert_eq!(restored["verified"], json!(true), "回滚必须回读校验通过");
    let report = restored["files"].as_array().expect("files 是数组");
    let status_of = |path: &str| -> String {
        report
            .iter()
            .find(|item| item["path"] == json!(path))
            .map(|item| item["status"].as_str().unwrap_or_default().to_string())
            .unwrap_or_else(|| panic!("回滚报告里缺 {path}：{report:?}"))
    };
    assert_eq!(status_of("a.txt"), "restored");
    // 快照时它就在，回滚口径是「按快照内容写回」，所以即便中途被删也是 `restored`
    // ——`recreated` 留给「快照里登记为已删除、回滚时补回来」的情形。
    assert_eq!(status_of("src/main.rs"), "restored");

    // 4) 磁盘真的回去了：内容逐字节一致。
    assert_eq!(ws.read("a.txt"), "v1\n");
    assert_eq!(ws.read("src/main.rs"), "fn main() {}\n");
    // 全量快照的语义：清单外的外来文件不擅自删除——删用户的文件比留着更危险。
    assert!(ws.path("b.txt").exists(), "快照之外的文件不该被顺手删掉");

    // 5) 列表与删除：total 跟随，重复删返回 false 而不是报错。
    let listed = core.ok("checkpoint.list", json!({ "workspace_id": workspace_id }));
    assert_eq!(listed["total"], json!(1));
    assert_eq!(
        listed["checkpoints"][0]["checkpoint_id"],
        json!(checkpoint_id)
    );

    let deleted = core.ok(
        "checkpoint.delete",
        json!({ "checkpoint_id": checkpoint_id }),
    );
    assert_eq!(deleted["removed"], json!(true));
    assert!(
        deleted["freed_bytes"].as_u64().unwrap_or(0) > 0,
        "删掉的检查点应报出释放了多少字节：{deleted}"
    );
    let again = core.ok(
        "checkpoint.delete",
        json!({ "checkpoint_id": checkpoint_id }),
    );
    assert_eq!(again["removed"], json!(false), "重复删除应幂等返回 false");

    let listed = core.ok("checkpoint.list", json!({ "workspace_id": workspace_id }));
    assert_eq!(listed["total"], json!(0));

    // 6) 审计留痕：建、回滚、删三件事都能在账本上查到。
    let kinds = kinds(&mut core);
    for expected in [
        "checkpoint.created",
        "checkpoint.restored",
        "checkpoint.deleted",
    ] {
        assert!(
            kinds.iter().any(|kind| kind == expected),
            "审计缺 {expected}：{kinds:?}"
        );
    }
}

#[test]
fn missing_checkpoint_reports_a_typed_error() {
    let data = TempDataDir::new("ckpt-missing");
    let ws = TempWorkspace::new("missing");
    let mut core = Core::spawn(&data.root);
    open(&mut core, &ws.root);

    let code = core.error_code(
        "checkpoint.restore",
        json!({ "checkpoint_id": "不存在的检查点" }),
    );
    // CHECKPOINT_NOT_FOUND：前端据此提示「这个检查点已被清理」，而不是弹一个通用错误。
    assert_eq!(code, 5006, "应为 CHECKPOINT_NOT_FOUND（实际 {code}）");
}

#[test]
fn checkpoint_requires_an_open_workspace() {
    let data = TempDataDir::new("ckpt-no-workspace");
    let mut core = Core::spawn(&data.root);

    let code = core.error_code("checkpoint.create", json!({}));
    assert_eq!(
        code, 1001,
        "未打开工作区应报 WORKSPACE_NOT_OPENED（实际 {code}）"
    );
}
