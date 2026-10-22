//! 仓库句柄与工作区级服务。

use crate::error::GitError;
use crate::exec::{normalize_path, Runner};
use crate::model::{GitBranches, GitCommitOutcome, GitDiff, GitStageOutcome, GitStatus};
use crate::parse::{parse_branches, parse_status};
use crate::{DIFF_LIMIT_BYTES, MAX_CONTEXT_LINES};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 仓库未配置身份时使用的临时作者名。
///
/// 只作为**单次命令**的 `-c` 参数传入，不写进 `.git/config`：用户的仓库配置不该被
/// IDE 悄悄改掉，但全新仓库 / CI 容器里没有全局身份时，提交也不该直接失败到没路走。
pub const FALLBACK_AUTHOR_NAME: &str = "AI IDE";
/// 配套的临时作者邮箱。
pub const FALLBACK_AUTHOR_EMAIL: &str = "ai-ide@localhost";

/// 一个工作区根的 Git 仓库句柄。
///
/// 同步 API：所有方法都是「起一个 git 进程 → 读输出 → 解析」。异步留给
/// `crates/core` 用 `spawn_blocking` 包一层——内核的运行时不该被一个可能跑几百毫秒的
/// `git status` 卡住，但 git 这一层也没必要为此引入 async。
#[derive(Debug, Clone)]
pub struct GitRepo {
    root: PathBuf,
    /// 工作区根相对仓库根的路径前缀（`sub/dir/` 形式；工作区就是仓库根时为空串）。
    ///
    /// 必须自己算：`git status` 的 porcelain 输出**永远**是仓库根相对的，`-c
    /// status.relativePaths=true` 与 `--relative` 都不影响它（后者 status 根本不认）。
    /// 所以「只看工作区子树」只能由本层过滤 + 去前缀完成，diff 才会用到 git 自己的
    /// `--relative`。
    prefix: String,
}

impl GitRepo {
    /// 打开工作区根对应的仓库。
    ///
    /// 判据是 `rev-parse --is-inside-work-tree`，不是「目录里有没有 `.git`」：后者
    /// 会漏掉 worktree、`--separate-git-dir`、以及工作区位于仓库子目录的情况。
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, GitError> {
        let root = root.into();
        if !root.is_dir() {
            return Err(GitError::not_a_repository(&root));
        }
        let mut repo = Self {
            root,
            prefix: String::new(),
        };
        let inside = repo
            .runner()
            .execute(&[], &["rev-parse", "--is-inside-work-tree"], None)?;
        if !inside.success || inside.stdout_trimmed() != "true" {
            return Err(GitError::not_a_repository(repo.root()));
        }
        repo.prefix = repo.runner().text(&["rev-parse", "--show-prefix"])?;
        Ok(repo)
    }

    /// 工作区根。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 工作区根相对仓库根的前缀（末尾带 `/`，仓库根时为空串）。
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// 工作区变更清单 + 分支头信息。
    ///
    /// `--untracked-files=all` 而不是默认的 `normal`：UI 要能直接对单个未跟踪文件
    /// 执行「暂存」，折叠成 `dir/` 就没法点了。
    ///
    /// 范围：先拿整个仓库的 porcelain 输出，再按 [`Self::prefix`] 过滤并要求去前缀。
    /// 反过来（指望 git 只报子树）不行——`git status` 不认 `--relative`，而 porcelain
    /// 输出恒为仓库根相对路径。代价是每次状态轮询会把整个仓库扫一遍（git 自身有
    /// 索引缓存，实测开销可接受），换来的是「工作区指向哪个子目录，路径就是哪种」。
    pub fn status(&self) -> Result<GitStatus, GitError> {
        let raw = self.runner().bytes(&[
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=all",
            "-z",
        ])?;
        let mut status = parse_status(&raw);
        status.entries = scope_entries(&self.prefix, status.entries);
        Ok(status)
    }

    /// unified diff。
    ///
    /// `path` 为 `None` 时是全量 diff。未跟踪文件不在 diff 里（git 本身如此）——
    /// 它们没有基准版本可对比，UI 应把 `?` 条目渲染成「新文件」而不是空 diff。
    pub fn diff(
        &self,
        path: Option<&str>,
        staged: bool,
        context_lines: Option<u32>,
    ) -> Result<GitDiff, GitError> {
        let context = context_lines.unwrap_or(3).min(MAX_CONTEXT_LINES);
        let normalized = match path {
            Some(raw) => Some(normalize_path(raw)?),
            None => None,
        };

        let mut args: Vec<String> = vec![
            "diff".to_string(),
            "--no-color".to_string(),
            "--no-ext-diff".to_string(),
            "--relative".to_string(),
            format!("--unified={context}"),
        ];
        if staged {
            args.push("--cached".to_string());
        }
        if let Some(normalized) = &normalized {
            args.push("--".to_string());
            args.push(normalized.clone());
        }
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let raw = self.runner().bytes(&borrowed)?;
        let text = String::from_utf8_lossy(&raw).to_string();

        let (diff, truncated) = truncate_on_boundary(text, DIFF_LIMIT_BYTES);
        Ok(GitDiff {
            path: normalized,
            staged,
            bytes: diff.len() as u64,
            diff,
            truncated,
        })
    }

    /// 暂存 / 取消暂存。
    ///
    /// `all` 与 `paths` 都给时按 `all` 处理（`.` 是工作区根，`--relative` 的语义
    /// 在 `add` / `reset` 上同样成立，所以不会波及工作区外的文件）。
    pub fn stage(
        &self,
        paths: &[String],
        all: bool,
        unstage: bool,
    ) -> Result<GitStageOutcome, GitError> {
        let normalized = normalize_all(paths)?;
        if !all && normalized.is_empty() {
            return Err(GitError::Invalid(
                "paths 与 all 至少要给一个：没有要操作的路径".to_string(),
            ));
        }

        let mut args: Vec<String> = if unstage {
            if self.has_head()? {
                vec!["reset".to_string(), "-q".to_string(), "HEAD".to_string()]
            } else {
                // 还没有任何提交时 `reset HEAD` 无从比较，只能从索引里摘掉。
                vec![
                    "rm".to_string(),
                    "-r".to_string(),
                    "--cached".to_string(),
                    "--ignore-unmatch".to_string(),
                    "-q".to_string(),
                ]
            }
        } else {
            vec!["add".to_string(), "-A".to_string()]
        };

        args.push("--".to_string());
        if all {
            args.push(".".to_string());
        } else {
            args.extend(normalized.iter().cloned());
        }

        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        self.runner().execute_checked(&[], &borrowed, None)?;

        Ok(GitStageOutcome {
            paths: if all { Vec::new() } else { normalized },
            all,
            unstage,
            staged_total: self.staged_count()?,
        })
    }

    /// 提交（可选 `--amend` / `--allow-empty` / 限定路径）。
    ///
    /// 提交信息走 stdin（`-F -`）而不是 `-m`：多行信息、超长信息、含引号的信息都
    /// 不需要任何转义，Windows 上也不会碰到命令行长度上限。
    ///
    /// 不加 `--quiet`：它连 "nothing to commit" 一起吞掉，只剩退出码 1，就没法把
    /// 「没东西可提交」与真正的失败区分开（对应错误码 7005 需要这个区分）。输出在
    /// 本层一律不落日志，静音与否用户看不见。
    pub fn commit(
        &self,
        message: &str,
        paths: Option<&[String]>,
        amend: bool,
        allow_empty: bool,
    ) -> Result<GitCommitOutcome, GitError> {
        if message.trim().is_empty() {
            return Err(GitError::Invalid("提交信息不能为空".to_string()));
        }
        let normalized = normalize_all(paths.unwrap_or(&[]))?;

        let name = self
            .config_value("user.name")?
            .unwrap_or_else(|| FALLBACK_AUTHOR_NAME.to_string());
        let email = self
            .config_value("user.email")?
            .unwrap_or_else(|| FALLBACK_AUTHOR_EMAIL.to_string());

        let mut args: Vec<String> = vec!["commit".to_string(), "-F".to_string(), "-".to_string()];
        if amend {
            args.push("--amend".to_string());
        }
        if allow_empty {
            args.push("--allow-empty".to_string());
        }
        if !normalized.is_empty() {
            args.push("--".to_string());
            args.extend(normalized.iter().cloned());
        }
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();

        let configs = [("user.name", name.as_str()), ("user.email", email.as_str())];
        let output = self.runner().execute(&configs, &borrowed, Some(message))?;
        if !output.success {
            // git 把「nothing to commit」打在 **stdout**，失败原因才走 stderr——所以
            // 两路都要看，否则「空提交」会被误判成命令失败。
            let stdout_text = output.stdout_trimmed();
            let stderr_text = output.stderr_trimmed();
            let combined = format!("{stdout_text}\n{stderr_text}").to_ascii_lowercase();
            if combined.contains("nothing to commit")
                || combined.contains("no changes added to commit")
                || combined.contains("nothing added to commit")
            {
                return Err(GitError::NothingToCommit);
            }
            return Err(GitError::Command {
                args: args.join(" "),
                stderr: if stderr_text.is_empty() {
                    stdout_text
                } else {
                    stderr_text
                },
                code: output.code,
            });
        }

        let oid = self.runner().text(&["rev-parse", "HEAD"])?;
        let short_oid = self.runner().text(&["rev-parse", "--short", "HEAD"])?;
        let files = self.commit_file_count()?;
        Ok(GitCommitOutcome {
            short_oid,
            oid,
            summary: message
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string(),
            branch: self.symbolic_ref()?,
            files,
            author: format!("{name} <{email}>"),
        })
    }

    /// 当前分支与分支列表。
    pub fn branches(&self) -> Result<GitBranches, GitError> {
        let current = self.symbolic_ref()?;
        let raw = self.runner().text(&[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(objectname:short)%00%(upstream:short)%00%(HEAD)",
            "refs/heads",
            "refs/remotes",
        ])?;
        let branches = parse_branches(&raw);
        Ok(GitBranches {
            detached: current.is_none(),
            current,
            branches,
        })
    }

    // -----------------------------------------------------------------------
    // 内部
    // -----------------------------------------------------------------------

    fn runner(&self) -> Runner {
        Runner::new(self.root.clone())
    }

    /// 当前分支名；分离头指针时为 `None`（`symbolic-ref` 退出码 1）。
    fn symbolic_ref(&self) -> Result<Option<String>, GitError> {
        let value =
            self.runner()
                .text_allow_failure(&["symbolic-ref", "--quiet", "--short", "HEAD"])?;
        Ok(value.filter(|name| !name.is_empty()))
    }

    /// HEAD 是否已指向某个提交（空仓库为 `false`）。
    fn has_head(&self) -> Result<bool, GitError> {
        let value =
            self.runner()
                .text_allow_failure(&["rev-parse", "--verify", "--quiet", "HEAD"])?;
        Ok(value.map(|oid| !oid.is_empty()).unwrap_or(false))
    }

    fn config_value(&self, key: &str) -> Result<Option<String>, GitError> {
        let value = self
            .runner()
            .text_allow_failure(&["config", "--get", key])?;
        Ok(value.filter(|value| !value.trim().is_empty()))
    }

    /// 暂存区条目数（冲突条目不算：它还没法直接提交）。
    fn staged_count(&self) -> Result<u64, GitError> {
        Ok(self
            .status()?
            .entries
            .iter()
            .filter(|entry| entry.index != "." && entry.index != "U")
            .count() as u64)
    }

    /// 刚生成的这次提交改了哪些文件。
    fn commit_file_count(&self) -> Result<u64, GitError> {
        let raw = self.runner().bytes(&[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "-z",
            "--root",
            "HEAD",
        ])?;
        let text = String::from_utf8_lossy(&raw);
        Ok(text
            .split('\0')
            .filter(|piece| !piece.trim_matches('\n').is_empty())
            .count() as u64)
    }
}

/// 工作区级 Git 服务：按工作区根缓存仓库句柄。
///
/// 缓存的是「这个目录确实是仓库」这个结论：`git.status` 会被 UI 高频轮询
/// （焦点变化、文件保存、面板展开），每次都重跑 `rev-parse` 是白花钱。失败的打开
/// **不**缓存——用户可能在 IDE 里刚 `git init`，下一次调用就该成功。
#[derive(Debug, Default)]
pub struct GitService {
    repos: Mutex<HashMap<PathBuf, Arc<GitRepo>>>,
}

impl GitService {
    /// 新建空服务。
    pub fn new() -> Self {
        Self::default()
    }

    /// 取工作区根对应的仓库句柄（首次调用会校验并缓存）。
    pub fn repo_for(&self, root: &Path) -> Result<Arc<GitRepo>, GitError> {
        let key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        if let Some(repo) = self.cached(&key) {
            return Ok(repo);
        }
        let repo = Arc::new(GitRepo::open(key.clone())?);
        if let Ok(mut map) = self.repos.lock() {
            map.insert(key, Arc::clone(&repo));
        }
        Ok(repo)
    }

    /// 丢掉某个工作区的缓存（关闭工作区时调用）。
    pub fn forget(&self, root: &Path) {
        let key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        if let Ok(mut map) = self.repos.lock() {
            map.remove(&key);
        }
    }

    /// 当前缓存条目数（测试用）。
    pub fn cached_len(&self) -> usize {
        self.repos.lock().map(|map| map.len()).unwrap_or(0)
    }

    fn cached(&self, key: &Path) -> Option<Arc<GitRepo>> {
        self.repos.lock().ok().and_then(|map| map.get(key).cloned())
    }
}

fn normalize_all(paths: &[String]) -> Result<Vec<String>, GitError> {
    paths.iter().map(|path| normalize_path(path)).collect()
}

/// 把仓库根相对的条目收敛到工作区子树，并去掉前缀。
///
/// 工作区就是仓库根时（`prefix` 为空）原样返回。跨前缀的条目（同一次提交里同时改了
/// 工作区内外的文件）里，重命名可能只有一侧在子树内：此时原路径置 `None`，而不是
/// 编一条子树内不存在的路径给 UI。
fn scope_entries(
    prefix: &str,
    entries: Vec<crate::model::GitStatusEntry>,
) -> Vec<crate::model::GitStatusEntry> {
    if prefix.is_empty() {
        return entries;
    }
    entries
        .into_iter()
        .filter_map(|mut entry| {
            let path = entry.path.strip_prefix(prefix)?.to_string();
            if path.is_empty() {
                return None;
            }
            entry.path = path;
            entry.original_path = entry
                .original_path
                .as_deref()
                .and_then(|original| original.strip_prefix(prefix))
                .map(|original| original.to_string());
            Some(entry)
        })
        .collect()
}

/// 按字符边界截断，避免把多字节 UTF-8 切坏。
fn truncate_on_boundary(text: String, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let mut cut = limit;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    (text[..cut].to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempRepo {
        dir: PathBuf,
    }

    impl TempRepo {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "ai-ide-git-{}-{}-{}",
                std::process::id(),
                nanos,
                tag
            ));
            fs::create_dir_all(&dir).expect("建临时目录");
            let repo = Self { dir };
            repo.git(&["init", "-q"]);
            // 与机器上的 git 版本解耦：不依赖 `init -b`。
            repo.git(&["symbolic-ref", "HEAD", "refs/heads/main"]);
            repo.git(&["config", "user.name", "Tester"]);
            repo.git(&["config", "user.email", "tester@example.com"]);
            repo
        }

        fn path(&self) -> &Path {
            &self.dir
        }

        fn git(&self, args: &[&str]) -> String {
            let output = Command::new("git")
                .arg("-C")
                .arg(&self.dir)
                .args(args)
                .output()
                .expect("执行 git");
            assert!(
                output.status.success(),
                "git {args:?} 失败：{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }

        fn write(&self, name: &str, content: &str) {
            let path = self.dir.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("建子目录");
            }
            fs::write(path, content).expect("写文件");
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    macro_rules! require_git {
        () => {
            if !git_available() {
                eprintln!("跳过：本机没有 git");
                return;
            }
        };
    }

    #[test]
    fn open_rejects_a_plain_directory() {
        let dir = std::env::temp_dir().join(format!(
            "ai-ide-git-plain-{}-{:?}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
        ));
        fs::create_dir_all(&dir).expect("建目录");
        let err = GitRepo::open(&dir).expect_err("普通目录不该被当成仓库");
        assert!(matches!(err, GitError::NotARepository(_)), "{err:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_rejects_missing_directory() {
        let err = GitRepo::open(std::env::temp_dir().join("ai-ide-git-does-not-exist-xyz"))
            .expect_err("不存在的目录");
        assert!(err.is_environment());
    }

    #[test]
    fn status_is_clean_in_fresh_repository() {
        require_git!();
        let temp = TempRepo::new("clean");
        let repo = GitRepo::open(temp.path()).expect("打开仓库");
        let status = repo.status().expect("status");
        assert!(status.is_clean(), "{status:?}");
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert!(status.head_oid.is_none(), "空仓库没有 HEAD 提交");
    }

    #[test]
    fn untracked_file_shows_up_then_stage_moves_it_to_index() {
        require_git!();
        let temp = TempRepo::new("stage");
        temp.write("src/main.rs", "fn main() {}\n");
        let repo = GitRepo::open(temp.path()).expect("打开仓库");

        let status = repo.status().expect("status");
        assert_eq!(status.entries.len(), 1);
        assert_eq!(status.entries[0].kind, crate::EntryKind::Untracked);

        let outcome = repo
            .stage(&["src/main.rs".to_string()], false, false)
            .expect("暂存");
        assert_eq!(outcome.staged_total, 1);
        assert!(!outcome.unstage);

        let status = repo.status().expect("status");
        assert_eq!(status.entries[0].index, "A");
        assert_eq!(status.entries[0].worktree, ".");

        // 取消暂存后回到未跟踪。
        let outcome = repo
            .stage(&["src/main.rs".to_string()], false, true)
            .expect("取消暂存");
        assert_eq!(outcome.staged_total, 0);
        let status = repo.status().expect("status");
        assert_eq!(status.entries[0].kind, crate::EntryKind::Untracked);
    }

    #[test]
    fn stage_all_in_unborn_repository_uses_pathspec_dot() {
        require_git!();
        let temp = TempRepo::new("stage-all");
        temp.write("a.txt", "a\n");
        temp.write("nested/b.txt", "b\n");
        let repo = GitRepo::open(temp.path()).expect("打开仓库");
        let outcome = repo.stage(&[], true, false).expect("全量暂存");
        assert_eq!(outcome.staged_total, 2);
        assert!(outcome.paths.is_empty());
    }

    #[test]
    fn commit_returns_hash_and_cleans_the_workspace() {
        require_git!();
        let temp = TempRepo::new("commit");
        temp.write("README.md", "# demo\n");
        let repo = GitRepo::open(temp.path()).expect("打开仓库");
        repo.stage(&[], true, false).expect("暂存");

        let commit = repo
            .commit("feat: 初次提交\n\n带一段多行正文", None, false, false)
            .expect("提交");
        assert_eq!(commit.oid.len(), 40, "{}", commit.oid);
        assert!(commit.oid.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_eq!(commit.short_oid.len().min(7), 7);
        assert_eq!(commit.summary, "feat: 初次提交");
        assert_eq!(commit.branch.as_deref(), Some("main"));
        assert_eq!(commit.files, 1);
        assert_eq!(commit.author, "Tester <tester@example.com>");
        assert!(repo.status().expect("status").is_clean());
    }

    #[test]
    fn commit_without_changes_reports_nothing_to_commit() {
        require_git!();
        let temp = TempRepo::new("nothing");
        temp.write("a.txt", "a\n");
        let repo = GitRepo::open(temp.path()).expect("打开仓库");
        repo.stage(&[], true, false).expect("暂存");
        repo.commit("first", None, false, false).expect("首次提交");

        let err = repo
            .commit("second", None, false, false)
            .expect_err("没有变更时应当报错");
        assert_eq!(err, GitError::NothingToCommit);

        // `allow_empty` 要能覆盖它。
        let commit = repo.commit("second", None, false, true).expect("空提交");
        assert_eq!(commit.files, 0);
    }

    #[test]
    fn empty_commit_message_is_rejected_before_running_git() {
        require_git!();
        let temp = TempRepo::new("empty-message");
        let repo = GitRepo::open(temp.path()).expect("打开仓库");
        let err = repo
            .commit("   \n\t ", None, false, true)
            .expect_err("空提交信息应被拒绝");
        assert!(matches!(err, GitError::Invalid(_)), "{err:?}");
    }

    #[test]
    fn amend_replaces_the_previous_commit() {
        require_git!();
        let temp = TempRepo::new("amend");
        temp.write("a.txt", "a\n");
        let repo = GitRepo::open(temp.path()).expect("打开仓库");
        repo.stage(&[], true, false).expect("暂存");
        let first = repo.commit("first", None, false, false).expect("首次提交");

        temp.write("b.txt", "b\n");
        repo.stage(&[], true, false).expect("暂存");
        let amended = repo
            .commit("first (amended)", None, true, false)
            .expect("amend");
        assert_ne!(amended.oid, first.oid);
        assert_eq!(amended.summary, "first (amended)");
        assert_eq!(amended.files, 2, "amend 后这一提交应含两个文件");
        assert_eq!(repo.branches().expect("branches").branches.len(), 1);
    }

    #[test]
    fn diff_covers_worktree_and_index() {
        require_git!();
        let temp = TempRepo::new("diff");
        temp.write("a.txt", "old\n");
        let repo = GitRepo::open(temp.path()).expect("打开仓库");
        repo.stage(&[], true, false).expect("暂存");
        repo.commit("base", None, false, false).expect("提交");

        temp.write("a.txt", "new\n");
        let worktree = repo.diff(None, false, None).expect("工作区 diff");
        assert!(worktree.diff.contains("-old"), "{}", worktree.diff);
        assert!(worktree.diff.contains("+new"), "{}", worktree.diff);
        assert!(!worktree.truncated);
        assert_eq!(worktree.bytes as usize, worktree.diff.len());
        assert_eq!(worktree.path, None);

        let staged = repo.diff(Some("a.txt"), true, None).expect("暂存区 diff");
        assert!(staged.diff.is_empty(), "未暂存时暂存区 diff 应为空");

        repo.stage(&[], true, false).expect("暂存");
        let staged = repo
            .diff(Some("a.txt"), true, Some(1))
            .expect("暂存区 diff");
        assert!(staged.diff.contains("+new"), "{}", staged.diff);
        assert_eq!(staged.path.as_deref(), Some("a.txt"));
        assert!(repo
            .diff(None, false, None)
            .expect("工作区 diff")
            .diff
            .is_empty());
    }

    #[test]
    fn diff_rejects_paths_outside_the_workspace() {
        require_git!();
        let temp = TempRepo::new("diff-guard");
        let repo = GitRepo::open(temp.path()).expect("打开仓库");
        for raw in ["../outside.txt", "C:\\Windows\\win.ini", "/etc/passwd"] {
            let err = repo
                .diff(Some(raw), false, None)
                .expect_err("越界路径应被拒绝");
            assert!(matches!(err, GitError::Invalid(_)), "{raw} → {err:?}");
        }
    }

    #[test]
    fn stage_rejects_empty_selection_and_bad_paths() {
        require_git!();
        let temp = TempRepo::new("stage-guard");
        let repo = GitRepo::open(temp.path()).expect("打开仓库");
        assert!(repo.stage(&[], false, false).is_err());
        assert!(repo
            .stage(&["../escape.txt".to_string()], false, false)
            .is_err());
    }

    #[test]
    fn workspace_subdirectory_only_sees_its_own_changes() {
        require_git!();
        let temp = TempRepo::new("subdir");
        temp.write("outside.txt", "outside\n");
        temp.write("sub/inside.txt", "inside\n");
        let repo = GitRepo::open(temp.path().join("sub")).expect("子目录也是仓库内");
        let status = repo.status().expect("status");
        let paths: Vec<&str> = status.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["inside.txt"], "--relative 应裁掉工作区外的路径");
    }

    #[test]
    fn branches_lists_local_and_marks_current() {
        require_git!();
        let temp = TempRepo::new("branches");
        temp.write("a.txt", "a\n");
        let repo = GitRepo::open(temp.path()).expect("打开仓库");
        repo.stage(&[], true, false).expect("暂存");
        repo.commit("base", None, false, false).expect("提交");
        temp.git(&["branch", "feature/x"]);

        let branches = repo.branches().expect("branches");
        assert_eq!(branches.current.as_deref(), Some("main"));
        assert!(!branches.detached);
        let names: Vec<&str> = branches.branches.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, vec!["feature/x", "main"]);
        let main = branches
            .branches
            .iter()
            .find(|b| b.name == "main")
            .expect("main");
        assert!(main.is_current);
        assert!(!main.is_remote);
        assert_eq!(main.oid.len(), 40);
    }

    #[test]
    fn service_caches_repositories_and_can_forget() {
        require_git!();
        let temp = TempRepo::new("service");
        let service = GitService::new();
        let first = service.repo_for(temp.path()).expect("打开仓库");
        let second = service.repo_for(temp.path()).expect("命中缓存");
        assert_eq!(service.cached_len(), 1);
        assert!(Arc::ptr_eq(&first, &second));

        service.forget(temp.path());
        assert_eq!(service.cached_len(), 0);
    }

    #[test]
    fn service_does_not_cache_failures() {
        let dir = std::env::temp_dir().join(format!(
            "ai-ide-git-cachefail-{}-{:?}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
        ));
        fs::create_dir_all(&dir).expect("建目录");
        let service = GitService::new();
        assert!(service.repo_for(&dir).is_err());
        assert_eq!(service.cached_len(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scoping_keeps_only_workspace_entries_and_strips_prefix() {
        use crate::model::{EntryKind, GitStatusEntry};
        let entries = vec![
            GitStatusEntry {
                path: "sub/inside.txt".into(),
                original_path: None,
                index: ".".into(),
                worktree: "M".into(),
                kind: EntryKind::Changed,
            },
            GitStatusEntry {
                path: "sub/old.txt".into(),
                original_path: Some("sub/older.txt".into()),
                index: "R".into(),
                worktree: ".".into(),
                kind: EntryKind::Renamed,
            },
            GitStatusEntry {
                path: "sub/from-outside.txt".into(),
                original_path: Some("outside.txt".into()),
                index: "R".into(),
                worktree: ".".into(),
                kind: EntryKind::Renamed,
            },
            GitStatusEntry {
                path: "outside.txt".into(),
                original_path: None,
                index: ".".into(),
                worktree: "M".into(),
                kind: EntryKind::Changed,
            },
            GitStatusEntry {
                path: "submarine/x.txt".into(),
                original_path: None,
                index: ".".into(),
                worktree: "?".into(),
                kind: EntryKind::Untracked,
            },
        ];
        let scoped = scope_entries("sub/", entries);
        let paths: Vec<&str> = scoped.iter().map(|e| e.path.as_str()).collect();
        // `submarine/` 不能被 `sub/` 的前缀匹配误收（按 `/` 结尾比较）。
        assert_eq!(paths, vec!["inside.txt", "old.txt", "from-outside.txt"]);
        assert_eq!(scoped[1].original_path.as_deref(), Some("older.txt"));
        assert_eq!(scoped[2].original_path, None, "原路径落在子树外时置 None");

        // 仓库根作为工作区时不做任何过滤。
        let root_scoped = scope_entries(
            "",
            vec![GitStatusEntry {
                path: "a.txt".into(),
                original_path: None,
                index: ".".into(),
                worktree: "M".into(),
                kind: EntryKind::Changed,
            }],
        );
        assert_eq!(root_scoped.len(), 1);
    }

    #[test]
    fn truncation_keeps_char_boundaries() {
        let text = "中文".repeat(600_000);
        let (truncated, flag) = truncate_on_boundary(text, DIFF_LIMIT_BYTES);
        assert!(flag);
        assert!(truncated.len() <= DIFF_LIMIT_BYTES);
        // 截断后仍是合法 UTF-8（能 to_string 就说明边界对）。
        assert!(!truncated.is_empty());
        let untouched = "短文本".to_string();
        let (same, flag) = truncate_on_boundary(untouched.clone(), DIFF_LIMIT_BYTES);
        assert!(!flag);
        assert_eq!(same, untouched);
    }
}
