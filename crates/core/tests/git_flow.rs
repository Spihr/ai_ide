//! `git.*` 端到端测试（M6 切片三）。
//!
//! 与其它 `*_flow` 一样跑真实子进程：Git 这层的价值全在「真的调到了 git、真的改了
//! 索引、真的写出了一个提交」，进程内替身只能证明分支被走到，证明不了 git 认不认。
//! 因此这里现场 `git init` 一个临时仓库，走完整条链路：
//!
//! 1. 能力清单里有 `git.*`（`core.hello` 报的就是 dispatch 能路由的集合）；
//! 2. `git.status` 看得见未跟踪文件，`git.stage` + `git.commit` 后变干净；
//! 3. `git.diff` 工作区与暂存区两侧都能取到，且带真实的 `+` 行；
//! 4. `git.branches` 回当前分支，提交后指向新提交；
//! 5. 失败路径分得清：暂存区空返回 7005、不是仓库返回 7002、路径越界返回 7004。
//!
//! 仓库根与内核数据目录分开建：数据目录会被内核写 `providers.json` / `sessions.db`，
//! 混进仓库里会让 `git.status` 多出几条与测试意图无关的未跟踪文件。

mod common;

use common::{unique_temp_dir, Core, TempDataDir};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// 临时仓库
// ---------------------------------------------------------------------------

/// 一个现场 `git init` 出来的临时仓库，用完连目录一起删。
struct TempRepo {
    root: PathBuf,
}

impl TempRepo {
    fn new(tag: &str) -> Self {
        Self {
            root: unique_temp_dir("ai-ide-git", tag),
        }
    }

    fn init(&self) {
        self.git(&["init", "-q"]);
        // 提交需要身份；写进仓库本地配置，避免依赖跑测试的机器上有没有全局 user.name。
        self.git(&["config", "user.email", "test@example.com"]);
        self.git(&["config", "user.name", "AI IDE Test"]);
        // 全局开了 gpgsign 的机器上，--amend 之类的用例会卡在签名上。
        self.git(&["config", "commit.gpgsign", "false"]);
    }

    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()
            .expect("git 应当可用");
        assert!(
            output.status.success(),
            "git {args:?} 失败：{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write(&self, name: &str, content: &str) {
        let path = self.root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("建父目录");
        }
        std::fs::write(&path, content).expect("写文件");
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.root.join(name)).unwrap_or_default()
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

fn boot(tag: &str) -> (Core, TempDataDir) {
    let data = TempDataDir::new(tag);
    let mut core = Core::spawn(&data.root);
    core.capabilities();
    (core, data)
}

fn open_workspace(core: &mut Core, root: &Path) -> String {
    let opened = core.ok(
        "workspace.open",
        json!({ "path": root.display().to_string() }),
    );
    opened["workspace_id"]
        .as_str()
        .expect("workspace.open 应返回 workspace_id")
        .to_string()
}

/// 状态里的路径表（顺带校验每个条目都有 kind）。
fn entry_paths(status: &Value) -> Vec<String> {
    status["entries"]
        .as_array()
        .expect("entries 是数组")
        .iter()
        .map(|entry| {
            assert!(entry["kind"].is_string(), "每个条目都必须带 kind：{entry}");
            entry["path"].as_str().unwrap_or_default().to_string()
        })
        .collect()
}

fn find_entry<'a>(status: &'a Value, path: &str) -> Option<&'a Value> {
    status["entries"]
        .as_array()?
        .iter()
        .find(|entry| entry["path"] == json!(path))
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

#[test]
fn capabilities_include_git_methods() {
    let (mut core, _data) = boot("git-caps");
    let capabilities = core.capabilities();
    for method in [
        "git.status",
        "git.diff",
        "git.stage",
        "git.commit",
        "git.branches",
    ] {
        assert!(
            capabilities.iter().any(|item| item == method),
            "能力清单缺少 {method}"
        );
    }
}

#[test]
fn status_stage_commit_and_branches_round_trip() {
    let (mut core, _data) = boot("git-roundtrip");
    let repo = TempRepo::new("roundtrip");
    repo.init();
    repo.write("a.txt", "line1\n");
    repo.write("nested/b.txt", "beta\n");
    let workspace_id = open_workspace(&mut core, &repo.root);

    // 两个未跟踪文件：status 必须报 `untracked`，且 clean=false。
    let status = core.ok("git.status", json!({ "workspace_id": workspace_id }));
    assert_eq!(status["workspace_id"], json!(workspace_id));
    assert_eq!(status["clean"], json!(false));
    let paths = entry_paths(&status);
    assert!(paths.contains(&"a.txt".to_string()), "{paths:?}");
    assert!(paths.contains(&"nested/b.txt".to_string()), "{paths:?}");
    let entry = find_entry(&status, "a.txt").expect("a.txt 应当在变更清单里");
    assert_eq!(entry["kind"], json!("untracked"));
    // 未跟踪文件的暂存区一侧是 `.`：git 还没管它。
    assert_eq!(entry["index"], json!("."));

    // 全部暂存：条目数应当与跟踪的文件数一致。
    let staged = core.ok(
        "git.stage",
        json!({ "workspace_id": workspace_id, "all": true }),
    );
    assert_eq!(staged["all"], json!(true));
    assert_eq!(staged["staged_total"], json!(2));

    let after_stage = core.ok("git.status", json!({ "workspace_id": workspace_id }));
    let entry = find_entry(&after_stage, "a.txt").expect("a.txt 应当还在清单里");
    assert_eq!(entry["index"], json!("A"), "暂存后索引区应为新增");
    assert_eq!(entry["worktree"], json!("."), "暂存后工作区一侧应干净");

    // 提交：回完整哈希与摘要，且分支名与 branches 一致。
    let commit = core.ok(
        "git.commit",
        json!({ "workspace_id": workspace_id, "message": "初始提交\n\n正文" }),
    );
    let oid = commit["oid"].as_str().expect("oid 是字符串").to_string();
    assert_eq!(oid.len(), 40, "SHA-1 应当是 40 位：{oid}");
    assert_eq!(commit["short_oid"].as_str().unwrap().len(), 7);
    assert_eq!(commit["summary"], json!("初始提交"));
    assert_eq!(commit["files"], json!(2));

    let branches = core.ok("git.branches", json!({ "workspace_id": workspace_id }));
    let current = branches["current"]
        .as_str()
        .expect("提交后必有当前分支")
        .to_string();
    assert_eq!(branches["detached"], json!(false));
    assert_eq!(commit["branch"], json!(current));
    let list = branches["branches"].as_array().expect("branches 是数组");
    assert!(
        list.iter().any(|item| item["name"] == json!(current)
            && item["is_current"] == json!(true)
            && item["oid"] == json!(oid)),
        "分支列表里应有当前分支：{branches}"
    );

    // 提交后工作区干净：entries 为空、clean=true。
    let clean = core.ok("git.status", json!({ "workspace_id": workspace_id }));
    assert_eq!(clean["clean"], json!(true));
    assert!(entry_paths(&clean).is_empty(), "{clean}");
}

#[test]
fn diff_covers_worktree_and_index_sides() {
    let (mut core, _data) = boot("git-diff");
    let repo = TempRepo::new("diff");
    repo.init();
    repo.write("a.txt", "line1\n");
    repo.write("b.txt", "keep\n");
    let workspace_id = open_workspace(&mut core, &repo.root);

    core.ok(
        "git.stage",
        json!({ "workspace_id": workspace_id, "all": true }),
    );
    core.ok(
        "git.commit",
        json!({ "workspace_id": workspace_id, "message": "基线" }),
    );

    // 只改 a.txt：工作区侧 diff 应当只含 a.txt，且带新增行。
    repo.write("a.txt", "line1\nline2\n");
    let worktree = core.ok(
        "git.diff",
        json!({ "workspace_id": workspace_id, "path": "a.txt" }),
    );
    assert_eq!(worktree["path"], json!("a.txt"));
    assert_eq!(worktree["staged"], json!(false));
    let text = worktree["diff"].as_str().expect("diff 是字符串");
    assert!(text.contains("+line2"), "diff 应含新增行：{text}");
    assert!(text.contains("a.txt"), "diff 头应带文件名：{text}");
    assert!(worktree["bytes"].as_u64().unwrap_or(0) > 0);
    assert_eq!(worktree["truncated"], json!(false));

    // 暂存后：工作区侧变空，暂存区侧出现同一处改动。
    core.ok(
        "git.stage",
        json!({ "workspace_id": workspace_id, "paths": ["a.txt"] }),
    );
    let empty = core.ok(
        "git.diff",
        json!({ "workspace_id": workspace_id, "path": "a.txt" }),
    );
    assert_eq!(empty["diff"], json!(""), "暂存后工作区侧应无差异");

    let cached = core.ok(
        "git.diff",
        json!({ "workspace_id": workspace_id, "path": "a.txt", "staged": true }),
    );
    assert_eq!(cached["staged"], json!(true));
    assert!(
        cached["diff"].as_str().unwrap().contains("+line2"),
        "暂存区 diff 应含新增行：{cached}"
    );

    // 全量 diff（不传 path）：暂存区里只有一个文件被改，但 b.txt 未动，故不含 b.txt。
    let all = core.ok(
        "git.diff",
        json!({ "workspace_id": workspace_id, "staged": true }),
    );
    assert_eq!(all["path"], Value::Null);
    assert!(!all["diff"].as_str().unwrap().contains("b.txt"), "{all}");

    // 取消暂存：暂存区回到空，改动退回工作区。
    let unstaged = core.ok(
        "git.stage",
        json!({ "workspace_id": workspace_id, "paths": ["a.txt"], "unstage": true }),
    );
    assert_eq!(unstaged["unstage"], json!(true));
    assert_eq!(unstaged["staged_total"], json!(0));

    let status = core.ok("git.status", json!({ "workspace_id": workspace_id }));
    let entry = find_entry(&status, "a.txt").expect("a.txt 应当回到变更清单");
    assert_eq!(entry["worktree"], json!("M"));
}

#[test]
fn empty_index_and_invalid_paths_report_distinct_codes() {
    let (mut core, _data) = boot("git-errors");
    let repo = TempRepo::new("errors");
    repo.init();
    repo.write("a.txt", "line1\n");
    let workspace_id = open_workspace(&mut core, &repo.root);

    core.ok(
        "git.stage",
        json!({ "workspace_id": workspace_id, "all": true }),
    );
    core.ok(
        "git.commit",
        json!({ "workspace_id": workspace_id, "message": "基线" }),
    );

    // 暂存区空：7005，而不是笼统的 7003。
    let code = core.error_code(
        "git.commit",
        json!({ "workspace_id": workspace_id, "message": "空提交" }),
    );
    assert_eq!(code, 7005);

    // 空提交信息：7004（参数问题，不该走到 git）。
    let code = core.error_code(
        "git.commit",
        json!({ "workspace_id": workspace_id, "message": "   " }),
    );
    assert_eq!(code, 7004);

    // 路径越界：7004，且不执行 git。
    let code = core.error_code(
        "git.diff",
        json!({ "workspace_id": workspace_id, "path": "../outside.txt" }),
    );
    assert_eq!(code, 7004);

    // 上下文行数越界不报错而是夹取：外壳不该为了「让 diff 更宽一点」被拒，
    // 上限由内核兜住（`MAX_CONTEXT_LINES`），返回的仍是合法 diff。
    let clamped = core.ok(
        "git.diff",
        json!({ "workspace_id": workspace_id, "context_lines": 500 }),
    );
    assert_eq!(clamped["staged"], json!(false));
    assert!(clamped["diff"].is_string());
}

#[test]
fn workspace_without_repository_reports_7002() {
    let (mut core, _data) = boot("git-not-repo");
    // 数据目录本身是空目录（已建），当作工作区打开——它不是仓库。
    let plain = TempDataDir::new("git-plain");
    let workspace_id = open_workspace(&mut core, &plain.root);

    let code = core.error_code("git.status", json!({ "workspace_id": workspace_id }));
    assert_eq!(code, 7002);

    // 不是仓库时的 stage 同样是 7002：环境问题，不是「暂存失败」。
    let code = core.error_code(
        "git.stage",
        json!({ "workspace_id": workspace_id, "all": true }),
    );
    assert_eq!(code, 7002);
}

#[test]
fn amend_rewrites_head_and_allow_empty_creates_commit() {
    let (mut core, _data) = boot("git-amend");
    let repo = TempRepo::new("amend");
    repo.init();
    repo.write("a.txt", "line1\n");
    let workspace_id = open_workspace(&mut core, &repo.root);

    core.ok(
        "git.stage",
        json!({ "workspace_id": workspace_id, "all": true }),
    );
    let first = core.ok(
        "git.commit",
        json!({ "workspace_id": workspace_id, "message": "第一版" }),
    );
    let first_oid = first["oid"].as_str().unwrap().to_string();

    // 改内容后 amend：哈希必须变，且 HEAD 只有一条提交（父提交仍是空树）。
    repo.write("a.txt", "line1\nline2\n");
    core.ok(
        "git.stage",
        json!({ "workspace_id": workspace_id, "paths": ["a.txt"] }),
    );
    let amended = core.ok(
        "git.commit",
        json!({ "workspace_id": workspace_id, "message": "第一版（补）", "amend": true }),
    );
    let amended_oid = amended["oid"].as_str().unwrap().to_string();
    assert_ne!(amended_oid, first_oid, "amend 应当改写提交哈希");
    assert_eq!(amended["summary"], json!("第一版（补）"));
    assert_eq!(repo.read("a.txt"), "line1\nline2\n");

    let count = Command::new("git")
        .arg("-C")
        .arg(&repo.root)
        .args(["rev-list", "--count", "HEAD"])
        .output()
        .expect("rev-list");
    assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "1");

    // 空提交：不给 allow_empty 报 7005，给了就真的建出一条提交。
    let code = core.error_code(
        "git.commit",
        json!({ "workspace_id": workspace_id, "message": "没有变更" }),
    );
    assert_eq!(code, 7005);

    let empty = core.ok(
        "git.commit",
        json!({ "workspace_id": workspace_id, "message": "空提交", "allow_empty": true }),
    );
    assert_eq!(empty["files"], json!(0));
    let count = Command::new("git")
        .arg("-C")
        .arg(&repo.root)
        .args(["rev-list", "--count", "HEAD"])
        .output()
        .expect("rev-list");
    assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "2");
}
