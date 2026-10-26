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
        // 全局开了 `core.autocrlf=true` 的机器上，checkout 会把 `\n` 改写成 `\r\n`，
        // 断言「取回的内容与写入一致」就会莫名其妙地失败。仓库级关掉它。
        self.git(&["config", "core.autocrlf", "false"]);
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

    /// 建成裸仓库（当远端用）。
    ///
    /// 远端必须是**真的** git 仓库：`git push` / `fetch` / `pull --ff-only` 的判定全在
    /// 远端一侧（引用是否前进、是否分叉），进程内替身根本没法产生那些回答。
    fn init_bare(&self) {
        self.git(&["init", "--bare", "-q"]);
    }

    /// 裸仓库的本地路径——git 认本地路径当远端地址，不必起服务。
    fn url(&self) -> String {
        self.root.display().to_string()
    }

    /// 从远端克隆到本对象已建好的空目录。
    fn clone_from(&self, url: &str) {
        let output = Command::new("git")
            // 必须在 clone 时就关掉换行转换：等到 clone 完再 `git config` 已经晚了，
            // 工作区已被按 `core.autocrlf` 检出成 CRLF，随后 `add -A` 会把整份换行
            // 改动当成内容提交（用例里会表现为「另一个克隆莫名改了 a.txt」）。
            .args(["-c", "core.autocrlf=false", "clone", "-q", url])
            .arg(&self.root)
            .output()
            .expect("git 应当可用");
        assert!(
            output.status.success(),
            "clone 失败：{}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.git(&["config", "user.email", "test@example.com"]);
        self.git(&["config", "user.name", "AI IDE Test"]);
        self.git(&["config", "commit.gpgsign", "false"]);
        self.git(&["config", "core.autocrlf", "false"]);
    }

    /// 当前分支名（不硬编码 `main` / `master`：取决于机器的 `init.defaultBranch`）。
    fn current_branch(&self) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["symbolic-ref", "--short", "HEAD"])
            .output()
            .expect("git 应当可用");
        assert!(output.status.success(), "读取当前分支失败");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// 不 panic 的 git：给「预期会失败」的断言用（例如确认某分支真的不存在）。
    fn try_git(&self, args: &[&str]) -> (bool, String) {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()
            .expect("git 应当可用");
        let mut text = String::from_utf8_lossy(&output.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        (output.status.success(), text)
    }

    /// 提交全部改动（测试自己造历史，不经过内核，避免用例之间互相牵连）。
    fn commit_all(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    /// HEAD 的完整哈希。
    fn head(&self) -> String {
        let (ok, text) = self.try_git(&["rev-parse", "HEAD"]);
        assert!(ok, "读 HEAD 失败：{text}");
        text.trim().to_string()
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

/// 取完整的错误对象（不只是码）。
///
/// 7006 / 7010 的价值有一半在 `data` 里：`files` 是外壳渲染「哪些文件挡住了」的清单，
/// `stash_kept` 决定「取出失败」要不要提示用户「记录还在」。只断言码会让这些字段
/// 悄悄消失而无人发现。
fn error_body(core: &mut Core, method: &str, params: Value) -> (i64, Value) {
    let response = core.call(method, params);
    let err = response
        .get("error")
        .unwrap_or_else(|| panic!("{method} 期望报错，实际返回：{response}"))
        .clone();
    let code = err["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("{method} 的错误没有 code：{err}"));
    (code, err)
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
        // 缺口第二批 Git 深化：九个方法都必须在能力清单里，且下面各有真实调用用例，
        // 所以它们进的是 `IMPLEMENTED`（dispatch 能路由）而不是「仅声明」。
        "git.checkout",
        "git.fetch",
        "git.pull",
        "git.push",
        "git.stash",
        "git.stash_list",
        "git.stash_pop",
        "git.stash_apply",
        "git.stash_drop",
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

// ---------------------------------------------------------------------------
// 缺口第二批：分支切换 / 远端 / stash
// ---------------------------------------------------------------------------

#[test]
fn checkout_refuses_dirty_workspace_then_switches() {
    let (mut core, _data) = boot("git-checkout");
    let repo = TempRepo::new("checkout");
    repo.init();
    repo.write("a.txt", "base\n");
    repo.commit_all("base");
    let base = repo.current_branch();
    let workspace_id = open_workspace(&mut core, &repo.root);

    // 脏工作区切换：7006，`data.files` 必须点名到底是谁挡住了。
    repo.write("a.txt", "dirty\n");
    let (code, err) = error_body(
        &mut core,
        "git.checkout",
        json!({ "workspace_id": workspace_id, "branch": "feature", "create": true }),
    );
    assert_eq!(code, 7006, "脏工作区切换必须是 7006：{err}");
    let files = err["data"]["files"]
        .as_array()
        .expect("7006 必须带 files 清单");
    assert!(files.iter().any(|item| item == "a.txt"), "{err}");

    // 被拒之后必须什么都没发生：分支没切、新分支也没建出来。
    assert_eq!(repo.current_branch(), base);
    let (exists, text) = repo.try_git(&["rev-parse", "--verify", "feature"]);
    assert!(!exists, "feature 不该被建出来：{text}");

    // 恢复干净后新建并切过去：created / previous 都是真值。
    repo.git(&["checkout", "--", "a.txt"]);
    let created = core.ok(
        "git.checkout",
        json!({ "workspace_id": workspace_id, "branch": "feature", "create": true }),
    );
    assert_eq!(created["created"], json!(true));
    assert_eq!(created["branch"], json!("feature"));
    assert_eq!(created["previous"], json!(base));
    assert_eq!(created["oid"].as_str().unwrap().len(), 40);
    assert_eq!(created["short_oid"].as_str().unwrap().len(), 7);
    assert_eq!(repo.current_branch(), "feature");

    // 切回已有分支：created=false，previous 是刚才那条。
    let back = core.ok(
        "git.checkout",
        json!({ "workspace_id": workspace_id, "branch": base }),
    );
    assert_eq!(back["created"], json!(false));
    assert_eq!(back["previous"], json!("feature"));
    assert_eq!(back["branch"], json!(base));
    assert_eq!(repo.current_branch(), base);
}

#[test]
fn push_needs_upstream_then_tracks_remote() {
    let (mut core, _data) = boot("git-push");
    let remote = TempRepo::new("push-remote");
    remote.init_bare();
    let repo = TempRepo::new("push-local");
    repo.init();
    repo.write("a.txt", "base\n");
    repo.commit_all("base");
    let branch = repo.current_branch();
    repo.git(&["remote", "add", "origin", &remote.url()]);
    let workspace_id = open_workspace(&mut core, &repo.root);

    // 没有上游：7009，而不是 7003（用户该做的是「设上游」，不是「重试」）。
    let (code, err) = error_body(
        &mut core,
        "git.push",
        json!({ "workspace_id": workspace_id }),
    );
    assert_eq!(code, 7009, "无上游推送必须是 7009：{err}");
    assert_eq!(err["data"]["branch"], json!(branch));

    // 带 `set_upstream`：真的推到裸仓库，远端分支与本地 HEAD 同指一个提交。
    let pushed = core.ok(
        "git.push",
        json!({ "workspace_id": workspace_id, "set_upstream": true }),
    );
    let head = repo.head();
    assert_eq!(pushed["branch"], json!(branch));
    assert_eq!(pushed["remote"], json!("origin"));
    assert_eq!(pushed["remote_branch"], json!(branch));
    assert_eq!(pushed["forced"], json!(false));
    assert_eq!(pushed["up_to_date"], json!(false));
    assert_eq!(pushed["oid"], json!(head));
    assert_eq!(pushed["short_oid"], json!(&head[..7]));
    let (ok, text) = remote.try_git(&["rev-parse", &format!("refs/heads/{branch}")]);
    assert!(ok, "裸仓库里应当有这条分支：{text}");
    assert_eq!(text.trim(), head, "远端必须指向同一条提交");

    // `push -u` 已经顺手建好 `refs/remotes/origin/<branch>`，所以紧接着的 fetch 不该
    // 把它当「新更新」再报一次——内核按「哈希是否变化」判，而不是「fetch 跑过就算」。
    let target = format!("origin/{branch}");
    let synced = core.ok(
        "git.fetch",
        json!({ "workspace_id": workspace_id, "prune": true }),
    );
    assert_eq!(synced["remote"], json!("origin"));
    assert_eq!(
        synced["up_to_date"],
        json!(true),
        "跟踪引用已同步时 fetch 应报同步：{synced}"
    );
    assert!(
        synced["updated"].as_array().unwrap().is_empty(),
        "不该把 `origin/HEAD` 这类符号引用当成远端分支上报：{synced}"
    );
    // 再推一次也是同步。
    let noop = core.ok("git.push", json!({ "workspace_id": workspace_id }));
    assert_eq!(noop["up_to_date"], json!(true));

    // 另一个克隆推一条上来：fetch 看得见引用前进。
    let other = TempRepo::new("push-other");
    other.clone_from(&remote.url());
    other.write("other.txt", "from other\n");
    other.commit_all("other");
    other.git(&["push", "-q", "origin", &format!("HEAD:refs/heads/{branch}")]);
    let advanced = core.ok("git.fetch", json!({ "workspace_id": workspace_id }));
    assert_eq!(advanced["up_to_date"], json!(false));
    let item = advanced["updated"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["ref_name"] == json!(target))
        .unwrap_or_else(|| panic!("fetch 应报出 {target} 的更新：{advanced}"))
        .clone();
    assert_eq!(item["old_short"], json!(&head[..7]));
    assert_eq!(item["new_short"], json!(&other.head()[..7]));
}

#[test]
fn pull_fast_forwards_and_rejects_divergence() {
    let (mut core, _data) = boot("git-pull");
    let remote = TempRepo::new("pull-remote");
    remote.init_bare();
    let repo = TempRepo::new("pull-local");
    repo.init();
    repo.write("a.txt", "base\n");
    repo.commit_all("base");
    let branch = repo.current_branch();
    repo.git(&["remote", "add", "origin", &remote.url()]);
    let workspace_id = open_workspace(&mut core, &repo.root);

    // 先推基线并设上游：pull 的远端/分支缺省值全靠上游。
    core.ok(
        "git.push",
        json!({ "workspace_id": workspace_id, "set_upstream": true }),
    );
    let before = repo.head();

    // 另一个克隆往前推一条：pull 应当快进，并回带带进来的文件。
    let other = TempRepo::new("pull-other");
    other.clone_from(&remote.url());
    other.write("other.txt", "from other\n");
    other.commit_all("other");
    other.git(&["push", "-q", "origin", &format!("HEAD:refs/heads/{branch}")]);
    let other_head = other.head();

    let pulled = core.ok("git.pull", json!({ "workspace_id": workspace_id }));
    assert_eq!(pulled["branch"], json!(branch));
    assert_eq!(pulled["upstream"], json!(format!("origin/{branch}")));
    assert_eq!(pulled["updated"], json!(true));
    assert_eq!(pulled["previous_oid"], json!(before));
    assert_eq!(pulled["oid"], json!(other_head));
    assert_eq!(pulled["short_oid"], json!(&other_head[..7]));
    assert_eq!(pulled["commits"], json!(1));
    assert_eq!(pulled["files"], json!(1));
    assert_eq!(pulled["changed_paths"], json!(["other.txt"]));
    assert_eq!(repo.read("other.txt"), "from other\n", "快进必须真的落盘");

    // 已是最新：updated=false、commits=0、清单空。
    let noop = core.ok("git.pull", json!({ "workspace_id": workspace_id }));
    assert_eq!(noop["updated"], json!(false));
    assert_eq!(noop["commits"], json!(0));
    assert_eq!(noop["files"], json!(0));
    assert!(
        noop["changed_paths"].as_array().unwrap().is_empty(),
        "{noop}"
    );

    // 分叉：本地一条、远端一条，`--ff-only` 必须拒绝（7007），且 HEAD 一动不动。
    repo.write("local.txt", "local\n");
    repo.commit_all("local");
    other.write("other.txt", "from other v2\n");
    other.commit_all("other v2");
    other.git(&["push", "-q", "origin", &format!("HEAD:refs/heads/{branch}")]);
    let local_head = repo.head();

    let (code, err) = error_body(
        &mut core,
        "git.pull",
        json!({ "workspace_id": workspace_id }),
    );
    assert_eq!(code, 7007, "分叉时 pull 必须是 7007：{err}");
    assert!(
        err["data"]["detail"]
            .as_str()
            .unwrap_or("")
            .contains("fast-forward")
            || err["data"]["detail"].is_string(),
        "7007 应带 git 原文摘要：{err}"
    );
    assert_eq!(repo.head(), local_head, "非快进被拒后 HEAD 不能动");
    assert_eq!(repo.read("other.txt"), "from other\n", "工作区不能被改写");
}

#[test]
fn stash_round_trip_applies_pops_and_drops() {
    let (mut core, _data) = boot("git-stash");
    let repo = TempRepo::new("stash");
    repo.init();
    repo.write("a.txt", "base\n");
    repo.commit_all("base");
    let workspace_id = open_workspace(&mut core, &repo.root);

    // 干净工作区：git 什么都不存，不能虚报一条记录给外壳。
    let none = core.ok("git.stash", json!({ "workspace_id": workspace_id }));
    assert_eq!(none["stashed"], json!(false));
    assert_eq!(none["entry"], Value::Null);
    assert_eq!(none["total"], json!(0));

    // 改动 + 未跟踪文件一起收：条目带回 message / oid / short_oid。
    repo.write("a.txt", "base\nwip\n");
    repo.write("scratch.txt", "tmp\n");
    let stashed = core.ok(
        "git.stash",
        json!({
            "workspace_id": workspace_id,
            "message": "wip-检查点",
            "include_untracked": true,
        }),
    );
    assert_eq!(stashed["stashed"], json!(true));
    assert_eq!(stashed["total"], json!(1));
    let entry = stashed["entry"].clone();
    assert_eq!(entry["index"], json!(0));
    assert!(
        entry["message"]
            .as_str()
            .unwrap_or("")
            .contains("wip-检查点"),
        "message 应原样带回：{entry}"
    );
    assert_eq!(entry["oid"].as_str().unwrap().len(), 40);
    assert_eq!(entry["short_oid"].as_str().unwrap().len(), 7);
    assert_eq!(repo.read("a.txt"), "base\n", "改动必须真的离开工作区");
    assert!(
        !repo.root.join("scratch.txt").exists(),
        "未跟踪文件也应收走"
    );

    let list = core.ok("git.stash_list", json!({ "workspace_id": workspace_id }));
    let entries = list["entries"].as_array().expect("entries 是数组");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["oid"], entry["oid"]);

    // apply：改动回来，条目保留（这是 pop 与 apply 的唯一差别）。
    let applied = core.ok(
        "git.stash_apply",
        json!({ "workspace_id": workspace_id, "index": 0 }),
    );
    assert_eq!(applied["action"], json!("apply"));
    assert_eq!(applied["index"], json!(0));
    assert_eq!(applied["dropped"], json!(false));
    assert_eq!(applied["total"], json!(1));
    assert_eq!(repo.read("a.txt"), "base\nwip\n");

    // 清回提交态再 pop：pop 成功后条目消失、栈清空。
    repo.git(&["reset", "--hard", "-q", "HEAD"]);
    let _ = std::fs::remove_file(repo.root.join("scratch.txt"));
    let popped = core.ok("git.stash_pop", json!({ "workspace_id": workspace_id }));
    assert_eq!(popped["action"], json!("pop"));
    assert_eq!(popped["dropped"], json!(true));
    assert_eq!(popped["total"], json!(0));
    assert_eq!(repo.read("a.txt"), "base\nwip\n");
    let emptied = core.ok("git.stash_list", json!({ "workspace_id": workspace_id }));
    assert!(
        emptied["entries"].as_array().unwrap().is_empty(),
        "{emptied}"
    );

    // 再来一条并 drop：回带 oid / message 供事后追认「删的是哪一条」。
    repo.write("a.txt", "base\nwip2\n");
    let second = core.ok(
        "git.stash",
        json!({ "workspace_id": workspace_id, "message": "第二条" }),
    );
    let second_oid = second["entry"]["oid"].as_str().unwrap().to_string();
    let dropped = core.ok(
        "git.stash_drop",
        json!({ "workspace_id": workspace_id, "index": 0 }),
    );
    assert_eq!(dropped["index"], json!(0));
    assert_eq!(dropped["oid"], json!(second_oid));
    assert!(
        dropped["message"].as_str().unwrap_or("").contains("第二条"),
        "{dropped}"
    );
    assert_eq!(dropped["total"], json!(0));

    // 越界序号是参数问题（7004），不是「git 失败了」（7003）。
    let code = core.error_code(
        "git.stash_drop",
        json!({ "workspace_id": workspace_id, "index": 7 }),
    );
    assert_eq!(code, 7004);
    let code = core.error_code(
        "git.stash_pop",
        json!({ "workspace_id": workspace_id, "index": 7 }),
    );
    assert_eq!(code, 7004);
}

#[test]
fn stash_pop_conflict_reports_7010_and_keeps_entry() {
    let (mut core, _data) = boot("git-stash-conflict");
    let repo = TempRepo::new("stash-conflict");
    repo.init();
    repo.write("a.txt", "base\n");
    repo.commit_all("base");
    let base_oid = repo.head();
    let workspace_id = open_workspace(&mut core, &repo.root);

    // 把「改成 stashed」收进 stash。
    repo.write("a.txt", "stashed\n");
    let stashed = core.ok(
        "git.stash",
        json!({ "workspace_id": workspace_id, "message": "冲突前" }),
    );
    assert_eq!(stashed["stashed"], json!(true));

    // 同一个文件改成另一版并提交：再 pop 必然撞冲突。
    repo.write("a.txt", "committed\n");
    repo.commit_all("另一版");

    let (code, err) = error_body(
        &mut core,
        "git.stash_pop",
        json!({ "workspace_id": workspace_id }),
    );
    assert_eq!(code, 7010, "stash 冲突必须是 7010：{err}");
    let files = err["data"]["files"]
        .as_array()
        .expect("7010 必须带冲突文件清单");
    assert!(files.iter().any(|item| item == "a.txt"), "{err}");
    assert_eq!(
        err["data"]["stash_kept"],
        json!(true),
        "冲突时 stash 必须保留：{err}"
    );

    // `stash_kept` 不是文案承诺：去栈里数一遍。
    let list = core.ok("git.stash_list", json!({ "workspace_id": workspace_id }));
    assert_eq!(list["entries"].as_array().unwrap().len(), 1, "{list}");

    // 冲突就地解决（回到 base 版本）后仍能再取：取出成功、条目删除。
    repo.git(&["reset", "--hard", "-q", &base_oid]);
    let popped = core.ok("git.stash_pop", json!({ "workspace_id": workspace_id }));
    assert_eq!(popped["dropped"], json!(true));
    assert_eq!(popped["total"], json!(0));
    assert_eq!(repo.read("a.txt"), "stashed\n");
}
