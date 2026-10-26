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
        // 缺口第五批：合并冲突的两个方法同样在 `IMPLEMENTED` 里（dispatch 有真分支），
        // 下面的 UU / AA / DU / UD 用例都是走能力清单里的这条路由去打的。
        "git.conflict_read",
        "git.conflict_resolve",
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

// ---------------------------------------------------------------------------
// 缺口第四批：单 hunk 暂存（git.stage_hunk / git.unstage_hunk）
// ---------------------------------------------------------------------------

/// 从**真实** `git diff` 输出里切出的一块 hunk。
///
/// 用例的定位参数一律出自这里，不手写「第 2 块应该在 -22,6」：手写行号等于自己造
/// 一份期望值，内核解析口径一旦偏了，用例会跟着一起偏。
#[derive(Debug, Clone)]
struct DiffHunk {
    index: u32,
    old_start: u32,
    old_lines: u32,
    new_start: u32,
    new_lines: u32,
    /// 正文行（含 ` ` / `+` / `-` / `\` 前缀，原样保留）。
    body: Vec<String>,
}

impl DiffHunk {
    fn header(&self) -> String {
        format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_lines, self.new_start, self.new_lines
        )
    }
}

/// `-1,6` → (1, 6)。git 在行数为 1 时省略 `,1`，必须按 1 补齐。
fn parse_range(text: &str, prefix: char) -> (u32, u32) {
    let rest = text.strip_prefix(prefix).unwrap_or(text);
    let mut parts = rest.split(',');
    let start = parts.next().unwrap_or_default().parse().unwrap_or(0);
    let lines = parts.next().map(|v| v.parse().unwrap_or(1)).unwrap_or(1);
    (start, lines)
}

fn parse_hunks(diff: &str) -> Vec<DiffHunk> {
    let mut hunks: Vec<DiffHunk> = Vec::new();
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("@@ ") {
            let mut fields = rest.split(' ');
            let old = fields.next().unwrap_or("-0,0");
            let new = fields.next().unwrap_or("+0,0");
            let (old_start, old_lines) = parse_range(old, '-');
            let (new_start, new_lines) = parse_range(new, '+');
            hunks.push(DiffHunk {
                index: hunks.len() as u32,
                old_start,
                old_lines,
                new_start,
                new_lines,
                body: Vec::new(),
            });
        } else if let Some(last) = hunks.last_mut() {
            // 只收正文四类行。`--- a/x` / `+++ b/x` 出现在第一块之前，这里进不来；
            // `\ No newline at end of file` 必须以 `\` 开头原样留着，否则下面那条
            // 断言就验证不了「末尾有没有换行」。
            if line.starts_with('+')
                || line.starts_with('-')
                || line.starts_with(' ')
                || line.starts_with('\\')
            {
                last.body.push(line.to_string());
            }
        }
    }
    hunks
}

impl TempRepo {
    /// 写二进制内容（构造「根本没有 hunk」的场景）。
    fn write_bytes(&self, name: &str, bytes: &[u8]) {
        let path = self.root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("建父目录");
        }
        std::fs::write(&path, bytes).expect("写文件");
    }

    /// 跑一条 git 命令，返回 stdout 原始字节——不经过 lossy 转换，断言才敢说「精确」。
    fn stdout_of(&self, args: &[&str]) -> Vec<u8> {
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
        output.stdout
    }

    /// 工作区侧（`false`）或索引侧（`true`）的完整 diff 文本。
    fn diff_text(&self, cached: bool) -> String {
        let args: Vec<&str> = if cached {
            vec!["diff", "--no-color", "--cached", "--"]
        } else {
            vec!["diff", "--no-color", "--"]
        };
        String::from_utf8(self.stdout_of(&args)).expect("diff 输出是 UTF-8")
    }

    /// 某一侧有改动的文件名（用来断言「什么都没被偷偷暂存」）。
    fn changed_paths(&self, cached: bool) -> Vec<String> {
        let args: Vec<&str> = if cached {
            vec!["diff", "--cached", "--name-only"]
        } else {
            vec!["diff", "--name-only"]
        };
        String::from_utf8_lossy(&self.stdout_of(&args))
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect()
    }

    /// 索引里的 blob 原始字节（`:a.txt` 这种 rev 语法）。
    fn blob(&self, spec: &str) -> Vec<u8> {
        self.stdout_of(&["cat-file", "blob", spec])
    }
}

fn hunk_params(workspace_id: &str, path: &str, hunk: &DiffHunk) -> Value {
    json!({
        "workspace_id": workspace_id,
        "path": path,
        "index": hunk.index,
        "old_start": hunk.old_start,
        "old_lines": hunk.old_lines,
        "new_start": hunk.new_start,
        "new_lines": hunk.new_lines,
    })
}

/// 30 行带编号的基线：编号让「哪一行的改动进了索引」一眼可验。
fn numbered(lines: usize) -> String {
    (1..=lines).map(|n| format!("line{n:02}\n")).collect()
}

/// 场景一：同一文件两块 hunk，只暂存第 2 块。
///
/// 断言口径是**精确**的：暂存区 diff 切出来的块，头与正文必须与工作区 diff 里那
/// 一块逐行相等；同时第 1 块必须原封不动留在工作区，文件内容一字不改。
#[test]
fn stage_hunk_stages_only_the_selected_block() {
    let (mut core, _data) = boot("git-hunk-stage");
    let repo = TempRepo::new("hunk-stage");
    repo.init();
    let base = numbered(30);
    repo.write("a.txt", &base);
    repo.commit_all("基线");
    let workspace_id = open_workspace(&mut core, &repo.root);

    // 第 3 行与第 25 行改动，中间隔着 21 行，`-U3` 下必然是两块独立 hunk。
    let worktree = base
        .replace("line03\n", "line03 changed\n")
        .replace("line25\n", "line25 changed\n");
    repo.write("a.txt", &worktree);

    let before = parse_hunks(&repo.diff_text(false));
    assert_eq!(before.len(), 2, "应当是两块独立 hunk：{before:?}");
    let (first, second) = (before[0].clone(), before[1].clone());
    assert!(
        first.body.iter().any(|line| line == "+line03 changed"),
        "第 1 块应含第 3 行的改动：{first:?}"
    );
    assert!(
        second.body.iter().any(|line| line == "+line25 changed"),
        "第 2 块应含第 25 行的改动：{second:?}"
    );

    let staged = core.ok(
        "git.stage_hunk",
        hunk_params(&workspace_id, "a.txt", &second),
    );
    assert_eq!(staged["hunk"]["index"], json!(second.index));
    assert_eq!(staged["hunk"]["old_start"], json!(second.old_start));
    assert_eq!(staged["hunk"]["old_lines"], json!(second.old_lines));
    assert_eq!(staged["hunk"]["new_start"], json!(second.new_start));
    assert_eq!(staged["hunk"]["new_lines"], json!(second.new_lines));
    assert_eq!(staged["staged_total"], json!(1));

    // 1) 索引里**就是**那两块中的第 2 块：只有一块、头一致、正文逐行一致。
    let cached = parse_hunks(&repo.diff_text(true));
    assert_eq!(cached.len(), 1, "暂存区应当只有一块：{cached:?}");
    assert_eq!(cached[0].header(), second.header(), "暂存块的头必须对上");
    assert_eq!(cached[0].body, second.body, "暂存块正文必须逐行相等");
    assert!(
        !repo.diff_text(true).contains("line03 changed"),
        "第 1 块不得被带进索引"
    );

    // 2) 第 1 块仍在工作区，且工作区文件内容一字未动。
    let rest = parse_hunks(&repo.diff_text(false));
    assert_eq!(rest.len(), 1, "工作区应当只剩第 1 块：{rest:?}");
    assert_eq!(rest[0].header(), first.header());
    assert_eq!(rest[0].body, first.body);
    assert_eq!(repo.read("a.txt"), worktree, "暂存不得碰工作区文件");

    // 3) 状态两侧都要如实：索引 M、工作区 M。
    let status = core.ok("git.status", json!({ "workspace_id": workspace_id }));
    let entry = find_entry(&status, "a.txt").expect("a.txt 应当在变更清单");
    assert_eq!(entry["index"], json!("M"));
    assert_eq!(entry["worktree"], json!("M"));
}

/// 场景二：文件末尾没有换行符。
///
/// `\ No newline at end of file` 不是注释而是「上一行无换行」的语义，喂给
/// `git apply` 的补丁一旦漏掉它，索引里就会多出一个换行符——所以这里直接读索引
/// blob 的字节，验证末尾真的没有 `\n`。
#[test]
fn stage_hunk_keeps_missing_newline_marker() {
    let (mut core, _data) = boot("git-hunk-nonewline");
    let repo = TempRepo::new("hunk-nonewline");
    repo.init();
    repo.write("a.txt", "a\nb\n");
    repo.commit_all("基线");
    let workspace_id = open_workspace(&mut core, &repo.root);

    // 只把末尾换行删掉。
    repo.write("a.txt", "a\nb");

    let hunks = parse_hunks(&repo.diff_text(false));
    assert_eq!(hunks.len(), 1, "只该有一块：{hunks:?}");
    assert_eq!(
        hunks[0].body.last().map(String::as_str),
        Some("\\ No newline at end of file"),
        "git 必须把「末尾无换行」写成标记行：{hunks:?}"
    );

    let staged = core.ok(
        "git.stage_hunk",
        hunk_params(&workspace_id, "a.txt", &hunks[0]),
    );
    let returned = staged["hunk"]["lines"]
        .as_array()
        .expect("回显的 hunk.lines 是数组");
    assert_eq!(
        returned.last().and_then(|item| item.as_str()),
        Some("\\ No newline at end of file"),
        "回显里必须保留标记行，外壳才能单独渲染：{staged}"
    );
    assert_eq!(staged["staged_total"], json!(1));

    // 真正落进索引的是「没有末尾换行」的两个字节序列：a \n b。
    assert_eq!(
        repo.blob(":a.txt"),
        b"a\nb".to_vec(),
        "索引里的内容必须末尾无换行"
    );
    assert!(
        repo.diff_text(true)
            .contains("\\ No newline at end of file"),
        "索引侧 diff 也必须带标记：{}",
        repo.diff_text(true)
    );
    assert!(
        repo.diff_text(false).is_empty(),
        "该块已全部进索引，工作区侧应当无差异"
    );
}

/// 场景三：二进制文件被 7012 明确拒绝，而不是 7003 或者「暂存了个空」。
#[test]
fn stage_hunk_rejects_binary_with_7012() {
    let (mut core, _data) = boot("git-hunk-binary");
    let repo = TempRepo::new("hunk-binary");
    repo.init();
    // 带 NUL 的内容 git 才认定为二进制。
    repo.write_bytes("bin.dat", &[0u8, 1, 2, 3, 0, 4]);
    repo.commit_all("基线");
    let workspace_id = open_workspace(&mut core, &repo.root);
    repo.write_bytes("bin.dat", &[0u8, 1, 2, 9, 0, 4, 5]);

    assert!(
        repo.diff_text(false).contains("Binary files"),
        "git 应当把该文件报成二进制：{}",
        repo.diff_text(false)
    );

    let (code, err) = error_body(
        &mut core,
        "git.stage_hunk",
        json!({
            "workspace_id": workspace_id,
            "path": "bin.dat",
            "index": 0,
            "old_start": 1,
            "old_lines": 1,
            "new_start": 1,
            "new_lines": 1,
        }),
    );
    assert_eq!(code, 7012, "二进制必须是 7012，不能退化成 7003：{err}");
    assert_eq!(
        err["data"]["path"],
        json!("bin.dat"),
        "7012 要带上是哪个文件：{err}"
    );
    assert!(
        repo.changed_paths(true).is_empty(),
        "被拒绝后索引里不得有任何改动：{:?}",
        repo.changed_paths(true)
    );
}

/// 场景四：hunk 定位失效必须**安全失败**——报 7011，且绝不按序号猜着暂存。
#[test]
fn stage_hunk_reports_7011_when_location_drifts() {
    let (mut core, _data) = boot("git-hunk-drift");
    let repo = TempRepo::new("hunk-drift");
    repo.init();
    let base = numbered(30);
    repo.write("a.txt", &base);
    repo.commit_all("基线");
    let workspace_id = open_workspace(&mut core, &repo.root);

    let worktree = base
        .replace("line03\n", "line03 changed\n")
        .replace("line25\n", "line25 changed\n");
    repo.write("a.txt", &worktree);

    let before = parse_hunks(&repo.diff_text(false));
    assert_eq!(before.len(), 2, "{before:?}");
    let stale = before[0].clone();

    // 外壳拿着旧定位、用户又改了一次文件：第 1 块区域多插了一行，块头从 `+1,6` 变成 `+1,7`。
    repo.write(
        "a.txt",
        &worktree.replace("line02\n", "line02\nline02 inserted\n"),
    );
    let now = parse_hunks(&repo.diff_text(false));
    assert_eq!(now.len(), 2, "{now:?}");
    assert_ne!(
        now[0].header(),
        stale.header(),
        "用例前提：块头必须真的变了，否则这条测的不是漂移"
    );

    let (code, err) = error_body(
        &mut core,
        "git.stage_hunk",
        hunk_params(&workspace_id, "a.txt", &stale),
    );
    assert_eq!(code, 7011, "定位失效必须是 7011：{err}");
    assert_eq!(err["data"]["path"], json!("a.txt"));
    assert_eq!(err["data"]["index"], json!(stale.index));
    assert_eq!(
        err["data"]["expected"],
        json!(stale.header()),
        "7011 要回显用户点的位置：{err}"
    );
    assert_eq!(
        err["data"]["actual"],
        json!(now[0].header()),
        "7011 要回显它现在真正在哪：{err}"
    );
    assert!(
        repo.changed_paths(true).is_empty(),
        "定位失效后绝不能暂存任何东西：{:?}",
        repo.changed_paths(true)
    );

    // 序号越界同样是 7011（安全失败），不是 panic、也不是笼统的 7003。
    let (code, err) = error_body(
        &mut core,
        "git.stage_hunk",
        json!({
            "workspace_id": workspace_id,
            "path": "a.txt",
            "index": 99,
            "old_start": 1,
            "old_lines": 6,
            "new_start": 1,
            "new_lines": 6,
        }),
    );
    assert_eq!(code, 7011, "越界序号也必须 7011：{err}");
    assert_eq!(
        err["data"]["actual"],
        Value::Null,
        "找不到块时 actual 应当为空，让外壳提示「刷新」而不是「位置变了」：{err}"
    );
    assert!(repo.changed_paths(true).is_empty());
}

/// 反向用例：两块都暂存后，撤销其中一块，另一块必须原封不动留在索引里。
#[test]
fn unstage_hunk_removes_only_the_selected_block() {
    let (mut core, _data) = boot("git-hunk-unstage");
    let repo = TempRepo::new("hunk-unstage");
    repo.init();
    let base = numbered(30);
    repo.write("a.txt", &base);
    repo.commit_all("基线");
    let workspace_id = open_workspace(&mut core, &repo.root);

    let worktree = base
        .replace("line03\n", "line03 changed\n")
        .replace("line25\n", "line25 changed\n");
    repo.write("a.txt", &worktree);

    // 先把两块分别暂存（第二次的定位按暂存后的工作区 diff 重新取，与外壳刷新一致）。
    let both = parse_hunks(&repo.diff_text(false));
    assert_eq!(both.len(), 2, "{both:?}");
    core.ok(
        "git.stage_hunk",
        hunk_params(&workspace_id, "a.txt", &both[0]),
    );
    let rest = parse_hunks(&repo.diff_text(false));
    assert_eq!(rest.len(), 1, "第 1 块进索引后工作区只剩第 2 块：{rest:?}");
    assert!(
        rest[0].body.iter().any(|line| line == "+line25 changed"),
        "{rest:?}"
    );
    core.ok(
        "git.stage_hunk",
        hunk_params(&workspace_id, "a.txt", &rest[0]),
    );

    assert!(
        repo.diff_text(false).is_empty(),
        "两块都暂存后工作区侧应当没有差异"
    );
    let staged_all = parse_hunks(&repo.diff_text(true));
    assert_eq!(staged_all.len(), 2, "{staged_all:?}");
    let keep = staged_all[0].clone();
    let drop = staged_all[1].clone();
    assert!(
        keep.body.iter().any(|line| line == "+line03 changed"),
        "{keep:?}"
    );
    assert!(
        drop.body.iter().any(|line| line == "+line25 changed"),
        "{drop:?}"
    );

    let unstaged = core.ok(
        "git.unstage_hunk",
        hunk_params(&workspace_id, "a.txt", &drop),
    );
    assert_eq!(unstaged["hunk"]["index"], json!(drop.index));
    assert_eq!(unstaged["staged_total"], json!(1));

    // 索引里只剩另一块，仍是逐行精确相等。
    let cached = parse_hunks(&repo.diff_text(true));
    assert_eq!(cached.len(), 1, "索引应当只剩一块：{cached:?}");
    assert_eq!(cached[0].header(), keep.header());
    assert_eq!(cached[0].body, keep.body);
    assert!(
        !repo.diff_text(true).contains("line25 changed"),
        "被撤销的块必须离开索引"
    );

    // 撤销的那块退回工作区，文件本身一字未动。
    let back = parse_hunks(&repo.diff_text(false));
    assert_eq!(back.len(), 1, "工作区应当只回来一块：{back:?}");
    assert_eq!(back[0].header(), drop.header());
    assert_eq!(back[0].body, drop.body);
    assert_eq!(repo.read("a.txt"), worktree, "取消暂存不得改工作区文件");
}

// ---------------------------------------------------------------------------
// 缺口第五批：合并冲突（git.conflict_read / git.conflict_resolve）
// ---------------------------------------------------------------------------

impl TempRepo {
    /// 打开 `merge.conflictStyle=diff3`。
    ///
    /// 默认的 `merge` 风格不写 `|||||||` 段，base 段就永远是 `None`，用例也只能验
    /// 「两段对不对」。改成 diff3 之后 Git 才会把共同祖先版本也写进文件，本批新增的
    /// base 段解析才有东西可对。
    fn use_diff3(&self) {
        self.git(&["config", "merge.conflictStyle", "diff3"]);
    }

    /// 从当前提交切出新分支（`checkout -b`）。
    fn branch_off(&self, branch: &str) {
        self.git(&["checkout", "-q", "-b", branch]);
    }

    fn checkout(&self, branch: &str) {
        self.git(&["checkout", "-q", branch]);
    }

    /// 造一次**真的**冲突 merge，并断言 Git 确实拒绝了自动合并。
    ///
    /// 用 `--no-commit` 留住未合并状态：用例要读的是「正在冲突的仓库」，一旦 git 自己
    /// 提交了，后面读到的就是干净的树，「冲突读错成空」这种 bug 会悄悄通过。
    fn merge_expect_conflict(&self, branch: &str) {
        let (ok, text) = self.try_git(&["merge", "--no-ff", "--no-commit", branch]);
        assert!(!ok, "merge {branch} 应当冲突，实际成功：{text}");
    }

    /// `git status --porcelain=v2` 里该文件的未合并 XY（`UU` / `AA` / `DU` / `UD` …）。
    ///
    /// 直接读 git 自己给的两位字母，而不是拿内核的 `kind` 反推——否则「内核把 DU 认成
    /// UD」这类错位，用例会跟着一起错到两边一致，白测一场。
    fn unmerged_xy(&self, path: &str) -> Option<String> {
        let text =
            String::from_utf8_lossy(&self.stdout_of(&["status", "--porcelain=v2"])).to_string();
        text.lines().find_map(|line| {
            let fields: Vec<&str> = line.split(' ').collect();
            if fields.first() != Some(&"u") {
                return None;
            }
            if fields.last() != Some(&path) {
                return None;
            }
            fields.get(1).map(|xy| xy.to_string())
        })
    }

    /// merge base 的短哈希：diff3 风格里 `|||||||` 后面跟的就是它。
    fn short_merge_base(&self) -> String {
        let oid = String::from_utf8_lossy(&self.stdout_of(&["merge-base", "HEAD", "MERGE_HEAD"]))
            .trim()
            .to_string();
        String::from_utf8_lossy(&self.stdout_of(&["rev-parse", "--short", &oid]))
            .trim()
            .to_string()
    }

    /// 索引侧（`--cached`）的 `A` / `M` / `D` 状态行，用于确认「解决结果真的进了索引」。
    fn staged_status(&self) -> String {
        String::from_utf8_lossy(&self.stdout_of(&["diff", "--cached", "--name-status"]))
            .trim()
            .to_string()
    }
}

/// 一个 UU 冲突仓库：`s.txt` 三行，双方各改第 2 行。
///
/// 三行是为了让**整份文件**都落在同一个冲突块里：块边界、行号区间、三段内容三者
/// 都能一眼定死，不会被「上下文行算不算块内」这种问题干扰。
fn uu_conflict_repo(tag: &str) -> TempRepo {
    let repo = TempRepo::new(tag);
    repo.init();
    repo.use_diff3();
    repo.write("s.txt", "top\nbase line\nbottom\n");
    repo.commit_all("基线");
    let ours = repo.current_branch();
    repo.branch_off("feature");
    repo.write("s.txt", "top\ntheir line\nbottom\n");
    repo.commit_all("对方改动");
    repo.checkout(&ours);
    repo.write("s.txt", "top\nour line\nbottom\n");
    repo.commit_all("我方改动");
    repo.merge_expect_conflict("feature");
    repo
}

fn read_params(workspace_id: &str, path: &str) -> Value {
    json!({ "workspace_id": workspace_id, "path": path })
}

/// 场景一（UU）：双改同一行。
///
/// 断言口径是**逐字**的：ours / base / theirs 三段必须与 Git 真正写进工作区的标记
/// 内容一字不差，行号区间必须精确覆盖 `<<<<<<<` 到 `>>>>>>>`。只看「解析出了 1 块」
/// 是没有意义的——块里的三段串位（ours 装成 theirs）在那种断言下照样通过。
#[test]
fn conflict_read_reports_uu_sections_verbatim() {
    let (mut core, _data) = boot("git-conflict-uu");
    let repo = uu_conflict_repo("conflict-uu");
    let workspace_id = open_workspace(&mut core, &repo.root);

    assert_eq!(
        repo.unmerged_xy("s.txt").as_deref(),
        Some("UU"),
        "用例本身没造出 UU 冲突，工作区原文：\n{}",
        repo.read("s.txt")
    );

    let read = core.ok("git.conflict_read", read_params(&workspace_id, "s.txt"));
    assert_eq!(read["path"], json!("s.txt"));
    assert_eq!(read["kind"], json!("both_modified"));
    assert_eq!(read["xy"], json!("UU"));
    assert_eq!(read["present"], json!(true));

    let blocks = read["blocks"].as_array().expect("blocks 是数组");
    assert_eq!(blocks.len(), 1, "整份文件落在同一块里：{read}");
    let block = &blocks[0];
    assert_eq!(block["index"], json!(0));

    // 三段逐字核对：内容行不含标记行，空行也照原样保留。
    //
    // 只有第 2 行冲突，首尾的 `top` / `bottom` 是两侧共有的上下文，Git 会把它们留在
    // 标记块**外面**——所以三段各只有一行，块也不是整份文件。这正是要用例逐字断言
    // 的原因：把块当成整份文件去取一侧，会把上下文行也当成冲突内容重复一遍。
    assert_eq!(block["ours"], json!(["our line"]));
    assert_eq!(block["base"], json!(["base line"]));
    assert_eq!(block["theirs"], json!(["their line"]));
    // 标签同样取自 Git 的实际输出：ours 是 HEAD，theirs 是分支名，base 是短 oid。
    assert_eq!(block["ours_label"], json!("HEAD"));
    assert_eq!(block["theirs_label"], json!("feature"));
    assert_eq!(block["base_label"], json!(repo.short_merge_base()));

    // 行号是 1 起、含标记行的闭区间。真实布局（diff3）：
    //   1 top / 2 <<<<<<< HEAD / 3 our line / 4 ||||||| <oid> / 5 base line /
    //   6 ======= / 7 their line / 8 >>>>>>> feature / 9 bottom
    // 共 9 行，块覆盖 2..=8；下面的行数断言与之一致，避免「区间报对但文件另有内容」。
    assert_eq!(block["start_line"], json!(2));
    assert_eq!(block["end_line"], json!(8));
    assert_eq!(
        repo.read("s.txt").lines().count(),
        9,
        "工作区原文行数与断言的行号区间不符：\n{}",
        repo.read("s.txt")
    );
}

/// 场景二（UU + ours）：取我方版本后，文件内容逐字等于我方原文，冲突消失且已落索引。
#[test]
fn conflict_resolve_ours_clears_conflict_and_keeps_ours() {
    let (mut core, _data) = boot("git-conflict-ours");
    let repo = uu_conflict_repo("conflict-ours");
    let workspace_id = open_workspace(&mut core, &repo.root);

    let resolved = core.ok(
        "git.conflict_resolve",
        json!({ "workspace_id": workspace_id, "path": "s.txt", "side": "ours" }),
    );
    assert_eq!(resolved["path"], json!("s.txt"));
    assert_eq!(resolved["kind"], json!("both_modified"));
    assert_eq!(resolved["resolved"], json!(true));
    assert_eq!(resolved["still_conflicted"], json!(false));
    assert_eq!(resolved["resolved_blocks"], json!(1));
    assert_eq!(resolved["remaining"], json!([]), "只剩这一个冲突文件");

    assert_eq!(repo.read("s.txt"), "top\nour line\nbottom\n");
    assert!(
        repo.unmerged_xy("s.txt").is_none(),
        "git status 里不得再有未合并记录"
    );
    assert_eq!(
        String::from_utf8_lossy(&repo.blob(":s.txt")),
        "top\nour line\nbottom\n",
        "解决结果必须已经进索引（stage 0）"
    );
}

/// 场景三（UU + both）：两侧都留，顺序由 `theirs_first` 决定。
///
/// 同一份冲突按两种顺序各解一次，是为了钉住「拼接顺序」这条约定：外壳的按钮文案是
/// 「我方在前 / 对方在前」，顺序反了用户看不出来，但结果就是错的。
#[test]
fn conflict_resolve_both_joins_sides_in_requested_order() {
    for theirs_first in [false, true] {
        let tag = if theirs_first {
            "both-theirs"
        } else {
            "both-ours"
        };
        let (mut core, _data) = boot(&format!("git-conflict-{tag}"));
        let repo = uu_conflict_repo(&format!("conflict-{tag}"));
        let workspace_id = open_workspace(&mut core, &repo.root);

        let resolved = core.ok(
            "git.conflict_resolve",
            json!({
                "workspace_id": workspace_id,
                "path": "s.txt",
                "side": "both",
                "theirs_first": theirs_first,
            }),
        );
        assert_eq!(resolved["resolved"], json!(true));
        assert_eq!(resolved["resolved_blocks"], json!(1));
        assert_eq!(resolved["remaining"], json!([]));

        let expected = if theirs_first {
            // 块外只有共有的 top / bottom，块内两行按顺序拼：top / 对方 / 我方 / bottom
            "top\ntheir line\nour line\nbottom\n"
        } else {
            "top\nour line\ntheir line\nbottom\n"
        };
        assert_eq!(
            repo.read("s.txt"),
            expected,
            "theirs_first={theirs_first} 时拼接顺序不符"
        );
        assert!(repo.unmerged_xy("s.txt").is_none());
    }
}

/// 场景四（AA）：双方各自新增了同名同路径文件，没有共同祖先版本。
///
/// `AA` 与 `UU` 的差别在 base：`AA` 的 base 段**存在但为空**（diff3 下 Git 仍写
/// `|||||||` 行，紧接着就是 `=======`）。这里把 `Some([])` 与 `None` 分开验：
/// 外壳靠这个区分「有祖先但为空」和「根本没有 base 段」。
#[test]
fn conflict_read_reports_aa_with_empty_base_section() {
    let (mut core, _data) = boot("git-conflict-aa");
    let repo = TempRepo::new("conflict-aa");
    repo.init();
    repo.use_diff3();
    repo.write("other.txt", "x\n");
    repo.commit_all("基线");
    let ours = repo.current_branch();
    repo.branch_off("feature");
    repo.write("new.txt", "feature\nsecond\n");
    repo.commit_all("对方新增");
    repo.checkout(&ours);
    repo.write("new.txt", "main\nsecond\n");
    repo.commit_all("我方新增");
    repo.merge_expect_conflict("feature");
    let workspace_id = open_workspace(&mut core, &repo.root);

    assert_eq!(
        repo.unmerged_xy("new.txt").as_deref(),
        Some("AA"),
        "用例本身没造出 AA 冲突：\n{}",
        repo.read("new.txt")
    );

    let read = core.ok("git.conflict_read", read_params(&workspace_id, "new.txt"));
    assert_eq!(read["kind"], json!("both_added"));
    assert_eq!(read["xy"], json!("AA"));
    assert_eq!(read["present"], json!(true));

    let blocks = read["blocks"].as_array().expect("blocks 是数组");
    assert_eq!(blocks.len(), 1, "{read}");
    assert_eq!(blocks[0]["ours"], json!(["main", "second"]));
    assert_eq!(blocks[0]["theirs"], json!(["feature", "second"]));
    assert_eq!(
        blocks[0]["base"],
        json!([]),
        "AA 的 base 段存在但为空，不能折成 null：{read}"
    );
    assert!(blocks[0]["base_label"].is_string(), "AA 同样带 base 标签");

    let resolved = core.ok(
        "git.conflict_resolve",
        json!({ "workspace_id": workspace_id, "path": "new.txt", "side": "both" }),
    );
    assert_eq!(resolved["kind"], json!("both_added"));
    assert_eq!(resolved["resolved"], json!(true));
    assert_eq!(repo.read("new.txt"), "main\nsecond\nfeature\nsecond\n");
    assert!(repo.unmerged_xy("new.txt").is_none());
    // 相对 HEAD 是「修改」而不是「新增」：我方分支本来就已经有这个文件，
    // 解决只是把它在索引里从 stage 2/3 收敛成 stage 0。
    assert_eq!(
        repo.staged_status(),
        "M\tnew.txt",
        "AA 解决后相对 HEAD 是改动"
    );
}

/// 场景五（DU）：我方删除、对方修改。工作区里是对方的版本，没有标记块可切。
///
/// 这类冲突**没有** `<<<<<<<`，`blocks` 必须为空而不是「解析失败」，`kind` 得让外壳
/// 知道「取 ours 等于删文件」。这里取 theirs：文件回来、内容逐字是对方改的那份。
#[test]
fn conflict_resolve_theirs_of_du_restores_their_version() {
    let (mut core, _data) = boot("git-conflict-du");
    let repo = TempRepo::new("conflict-du");
    repo.init();
    repo.write("del.txt", "base\n");
    repo.commit_all("基线");
    let ours = repo.current_branch();
    repo.branch_off("side");
    repo.write("del.txt", "modified by them\n");
    repo.commit_all("对方修改");
    repo.checkout(&ours);
    repo.git(&["rm", "-q", "--", "del.txt"]);
    repo.commit_all("我方删除");
    repo.merge_expect_conflict("side");
    let workspace_id = open_workspace(&mut core, &repo.root);

    assert_eq!(
        repo.unmerged_xy("del.txt").as_deref(),
        Some("DU"),
        "用例本身没造出 DU 冲突"
    );

    let read = core.ok("git.conflict_read", read_params(&workspace_id, "del.txt"));
    assert_eq!(read["kind"], json!("deleted_by_us"));
    assert_eq!(read["xy"], json!("DU"));
    assert_eq!(read["present"], json!(true), "工作区里留着对方的版本");
    assert_eq!(
        read["blocks"],
        json!([]),
        "一侧缺失的冲突没有标记块，不能报成解析失败"
    );

    let resolved = core.ok(
        "git.conflict_resolve",
        json!({ "workspace_id": workspace_id, "path": "del.txt", "side": "theirs" }),
    );
    assert_eq!(resolved["kind"], json!("deleted_by_us"));
    assert_eq!(resolved["resolved"], json!(true));
    assert_eq!(resolved["resolved_blocks"], json!(0), "没有块可替换");
    assert_eq!(resolved["remaining"], json!([]));
    assert_eq!(repo.read("del.txt"), "modified by them\n");
    assert!(repo.unmerged_xy("del.txt").is_none());
}

/// 场景六（UD）：对方删除、我方修改。取 theirs 等于**删除**，必须走 `git rm` 落索引。
///
/// 这一条钉的是 `ConflictKind::missing()` 的方向性：若把 `UD` 认成 `DU`，取 theirs
/// 会去找对方的文件版本，而对方压根没有——`checkout --theirs` 报错还是小事，最怕
/// 写出一个空文件再 `add` 上去，等于把「删除」悄悄变成了「清空」。
#[test]
fn conflict_resolve_theirs_of_ud_deletes_the_file() {
    let (mut core, _data) = boot("git-conflict-ud");
    let repo = TempRepo::new("conflict-ud");
    repo.init();
    repo.write("del.txt", "base\n");
    repo.commit_all("基线");
    let ours = repo.current_branch();
    repo.branch_off("side");
    repo.git(&["rm", "-q", "--", "del.txt"]);
    repo.commit_all("对方删除");
    repo.checkout(&ours);
    repo.write("del.txt", "modified by us\n");
    repo.commit_all("我方修改");
    repo.merge_expect_conflict("side");
    let workspace_id = open_workspace(&mut core, &repo.root);

    assert_eq!(
        repo.unmerged_xy("del.txt").as_deref(),
        Some("UD"),
        "用例本身没造出 UD 冲突"
    );

    let read = core.ok("git.conflict_read", read_params(&workspace_id, "del.txt"));
    assert_eq!(read["kind"], json!("deleted_by_them"));
    assert_eq!(read["xy"], json!("UD"));
    assert_eq!(read["present"], json!(true), "工作区里留着我方的版本");
    assert_eq!(read["blocks"], json!([]));

    let resolved = core.ok(
        "git.conflict_resolve",
        json!({ "workspace_id": workspace_id, "path": "del.txt", "side": "theirs" }),
    );
    assert_eq!(resolved["kind"], json!("deleted_by_them"));
    assert_eq!(resolved["resolved"], json!(true));
    assert_eq!(resolved["remaining"], json!([]));

    assert!(
        !repo.root.join("del.txt").exists(),
        "取已删除的那一侧，工作区文件必须消失"
    );
    assert!(
        repo.unmerged_xy("del.txt").is_none(),
        "git status 里不得再有未合并记录"
    );
    assert_eq!(
        repo.staged_status(),
        "D\tdel.txt",
        "删除必须已经进索引（不是留个工作区删除）"
    );
}

/// 反向用例：非冲突文件调两个新方法必须报 7013，而不是「读出一份没有块的冲突」。
///
/// 7013 / 7004 分开验：前者是「文件不在冲突里，去刷新冲突面板」，后者是「参数写错，
/// 去改调用」。把这类误用折成「成功 + 空 blocks」，外壳会把一个普通文件显示成待解决
/// 冲突，用户点「取 ours」就等于把工作区改动覆盖掉。
#[test]
fn conflict_methods_reject_non_conflict_file_with_7013() {
    let (mut core, _data) = boot("git-conflict-clean");
    let repo = TempRepo::new("conflict-clean");
    repo.init();
    repo.write("a.txt", "line1\n");
    repo.commit_all("基线");
    repo.write("a.txt", "line1 changed\n");
    let workspace_id = open_workspace(&mut core, &repo.root);

    let (code, err) = error_body(
        &mut core,
        "git.conflict_read",
        read_params(&workspace_id, "a.txt"),
    );
    assert_eq!(code, 7013, "非冲突文件读冲突：{err}");
    assert_eq!(err["data"]["path"], json!("a.txt"));

    let (code, err) = error_body(
        &mut core,
        "git.conflict_resolve",
        json!({ "workspace_id": workspace_id, "path": "a.txt", "side": "ours" }),
    );
    assert_eq!(code, 7013, "非冲突文件解冲突：{err}");
    assert_eq!(err["data"]["path"], json!("a.txt"));

    // 参数层：认不出的 side、以及既没 side 也没 content，都不许猜，直接 7004。
    let (code, err) = error_body(
        &mut core,
        "git.conflict_resolve",
        json!({ "workspace_id": workspace_id, "path": "a.txt", "side": "mine" }),
    );
    assert_eq!(code, 7004, "未知 side 必须报参数非法：{err}");
    assert_eq!(err["data"]["side"], json!("mine"));

    let (code, _err) = error_body(
        &mut core,
        "git.conflict_resolve",
        read_params(&workspace_id, "a.txt"),
    );
    assert_eq!(code, 7004, "side 与 content 全空必须报参数非法");
}

/// 反向用例（7014）：调用方记下的块布局已经过期，不能按旧行号切。
#[test]
fn conflict_resolve_reports_7014_when_layout_drifts() {
    let (mut core, _data) = boot("git-conflict-drift");
    let repo = uu_conflict_repo("conflict-drift");
    let workspace_id = open_workspace(&mut core, &repo.root);

    let read = core.ok("git.conflict_read", read_params(&workspace_id, "s.txt"));
    assert_eq!(read["blocks"].as_array().map(|b| b.len()), Some(1));
    let snapshot_start = read["blocks"][0]["start_line"].clone();

    // 外壳以为有 2 块（中途有别人往文件里又插了一块）：现状是 1 块，必须拒绝。
    let (code, err) = error_body(
        &mut core,
        "git.conflict_resolve",
        json!({
            "workspace_id": workspace_id,
            "path": "s.txt",
            "side": "ours",
            "expect_blocks": 2,
        }),
    );
    assert_eq!(code, 7014, "块数不符必须报冲突已变化：{err}");
    assert_eq!(err["data"]["path"], json!("s.txt"));
    assert_eq!(repo.read("s.txt").lines().count(), 9, "报错时不得动过文件");

    // 起始行对不上同样拒绝。
    let (code, _err) = error_body(
        &mut core,
        "git.conflict_resolve",
        json!({
            "workspace_id": workspace_id,
            "path": "s.txt",
            "side": "ours",
            "expect_start_line": 99,
        }),
    );
    assert_eq!(code, 7014);

    // 布局对上了才执行。
    let resolved = core.ok(
        "git.conflict_resolve",
        json!({
            "workspace_id": workspace_id,
            "path": "s.txt",
            "side": "ours",
            "expect_blocks": 1,
            "expect_start_line": snapshot_start,
        }),
    );
    assert_eq!(resolved["resolved"], json!(true));
    assert_eq!(repo.read("s.txt"), "top\nour line\nbottom\n");
}

/// 场景七（防误判）：正文里本来就有像标记的普通文本，不能被当成冲突块。
///
/// 真冲突文件里出现 `<<<<<<<` 是很常见的（这个仓库自己的文档就在讨论标记语法）。
/// 只认「恰好 7 个标记字符 + 至少一个空格的标签」并且配对到底才算块，否则：
/// 把正文当块 → 解决时会把正文整段删掉；把块当正文 → 用户点了「取 ours」什么也没变。
#[test]
fn conflict_scan_ignores_marker_like_text_outside_blocks() {
    let (mut core, _data) = boot("git-conflict-guard");
    let repo = TempRepo::new("conflict-guard");
    repo.init();
    repo.use_diff3();

    // 前 6 行全是「像标记但不是标记」的正文：
    // 缩进过的、只有 6 个字符的、8 个字符的、没有标签的、8 个等号的。
    let decoys = [
        "a <<<<<<< not at column 0",
        "<<<<<<<",
        "<<<<<<",
        "<<<<<<<<",
        "========",
        ">>>>>>>",
    ];
    let mut body = String::new();
    for line in decoys {
        body.push_str(line);
        body.push('\n');
    }
    for index in 1..=10 {
        body.push_str(&format!("filler {index:02}\n"));
    }
    body.push_str("target line\n");
    body.push_str("tail\n");
    repo.write("fp.txt", &body);
    repo.commit_all("基线");

    let ours = repo.current_branch();
    repo.branch_off("feature");
    let theirs = body.replace("target line\n", "target line by them\n");
    repo.write("fp.txt", &theirs);
    repo.commit_all("对方改动");
    repo.checkout(&ours);
    let mine = body.replace("target line\n", "target line by us\n");
    repo.write("fp.txt", &mine);
    repo.commit_all("我方改动");
    repo.merge_expect_conflict("feature");
    let workspace_id = open_workspace(&mut core, &repo.root);

    assert_eq!(repo.unmerged_xy("fp.txt").as_deref(), Some("UU"));

    let read = core.ok("git.conflict_read", read_params(&workspace_id, "fp.txt"));
    let blocks = read["blocks"].as_array().expect("blocks 是数组");
    assert_eq!(blocks.len(), 1, "六行像标记的正文不得被数成冲突块：{read}");
    let block = &blocks[0];
    assert!(
        block["start_line"].as_u64().unwrap_or(0) > 6,
        "真块在第 6 行之后，块起点却报成 {}：{read}",
        block["start_line"]
    );
    for decoy in decoys {
        let in_ours = block["ours"]
            .as_array()
            .is_some_and(|lines| lines.iter().any(|line| line.as_str() == Some(decoy)));
        assert!(!in_ours, "正文行 {decoy:?} 被吸进了 ours 段：{read}");
    }
    assert!(
        block["ours"].as_array().is_some_and(|lines| lines
            .iter()
            .any(|line| line.as_str() == Some("target line by us"))),
        "ours 段必须是我方改后的内容：{read}"
    );
    assert!(
        block["theirs"].as_array().is_some_and(|lines| lines
            .iter()
            .any(|line| line.as_str() == Some("target line by them"))),
        "theirs 段必须是对方改后的内容：{read}"
    );

    // 解决之后，六行正文必须原样还在——这才是「没被误判」的最终证明。
    let resolved = core.ok(
        "git.conflict_resolve",
        json!({ "workspace_id": workspace_id, "path": "fp.txt", "side": "ours" }),
    );
    assert_eq!(resolved["resolved"], json!(true));
    let after = repo.read("fp.txt");
    for decoy in decoys {
        assert!(
            after.lines().any(|line| line == decoy),
            "解决后正文行 {decoy:?} 不见了：\n{after}"
        );
    }
    assert_eq!(after, mine, "取 ours 后文件必须逐字等于我方原文");
}

/// 场景八（多文件）：解决一个之后，`remaining` 必须给出其余仍在冲突的文件。
///
/// 外壳靠它决定「关不关冲突面板」：少报会让面板停在已解决的文件上，多报会让用户
/// 一次次点「已解决」却关不掉。
#[test]
fn conflict_resolve_reports_remaining_conflicts() {
    let (mut core, _data) = boot("git-conflict-remaining");
    let repo = TempRepo::new("conflict-remaining");
    repo.init();
    repo.write("a.txt", "a-base\n");
    repo.write("b.txt", "b-base\n");
    repo.commit_all("基线");
    let ours = repo.current_branch();
    repo.branch_off("feature");
    repo.write("a.txt", "a-theirs\n");
    repo.write("b.txt", "b-theirs\n");
    repo.commit_all("对方改动");
    repo.checkout(&ours);
    repo.write("a.txt", "a-ours\n");
    repo.write("b.txt", "b-ours\n");
    repo.commit_all("我方改动");
    repo.merge_expect_conflict("feature");
    let workspace_id = open_workspace(&mut core, &repo.root);

    assert_eq!(repo.unmerged_xy("a.txt").as_deref(), Some("UU"));
    assert_eq!(repo.unmerged_xy("b.txt").as_deref(), Some("UU"));

    let first = core.ok(
        "git.conflict_resolve",
        json!({ "workspace_id": workspace_id, "path": "a.txt", "side": "ours" }),
    );
    assert_eq!(first["remaining"], json!(["b.txt"]), "另一个文件还在冲突");
    assert_eq!(repo.read("a.txt"), "a-ours\n");

    let second = core.ok(
        "git.conflict_resolve",
        json!({ "workspace_id": workspace_id, "path": "b.txt", "side": "theirs" }),
    );
    assert_eq!(second["remaining"], json!([]), "全部解决后剩余应为空");
    assert_eq!(repo.read("b.txt"), "b-theirs\n");
}
