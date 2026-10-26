//! 仓库句柄与工作区级服务。

use crate::error::{classify_hunk_apply_failure, GitError};
use crate::exec::{normalize_path, Runner};
use crate::model::{
    GitBranches, GitCheckoutOutcome, GitCommitOutcome, GitDiff, GitFetchOutcome, GitFileHunks,
    GitHunkOutcome, GitPullOutcome, GitPushOutcome, GitRefUpdate, GitStageOutcome,
    GitStashApplyOutcome, GitStashDropOutcome, GitStashEntry, GitStashOutcome, GitStatus,
    HunkSelector, MAX_PULL_CHANGED_PATHS,
};
use crate::parse::{hunk_patch, parse_branches, parse_diff, parse_status, ParsedDiffFile};
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
        let normalized = match path {
            Some(raw) => Some(normalize_path(raw)?),
            None => None,
        };

        let text = self.raw_diff(normalized.as_deref(), staged, context_lines)?;
        let (diff, truncated) = truncate_on_boundary(text, DIFF_LIMIT_BYTES);
        Ok(GitDiff {
            path: normalized,
            staged,
            bytes: diff.len() as u64,
            diff,
            truncated,
        })
    }

    /// 未截断的 diff 原文。
    ///
    /// 按块暂存**不能**吃 `diff()` 的结果：那个走的是 [`DIFF_LIMIT_BYTES`] 截断路径，
    /// 大文件的后半段 hunk 会被砍掉，于是「第 7 块找不到」会被误报成定位失效。
    /// 所以 hunk 解析另开一条不截断的通道，与 `diff()` 共用同一套调用参数，
    /// 避免两边对「staged / context / 路径」的解释出现分歧。
    fn raw_diff(
        &self,
        normalized: Option<&str>,
        staged: bool,
        context_lines: Option<u32>,
    ) -> Result<String, GitError> {
        let context = context_lines.unwrap_or(3).min(MAX_CONTEXT_LINES);
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
        if let Some(normalized) = normalized {
            args.push("--".to_string());
            args.push(normalized.to_string());
        }
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let raw = self.runner().bytes(&borrowed)?;
        Ok(String::from_utf8_lossy(&raw).to_string())
    }

    /// 某个文件的 hunk 清单（供外壳渲染「逐块暂存」的勾选粒度）。
    ///
    /// 不走 [`Self::diff`] 的截断：一块被砍一半的 hunk 比没有更糟——外壳会把它当成
    /// 合法的第 N 块交给 `stage_hunk`，然后由 `git apply` 报「corrupt patch」。
    pub fn hunks(
        &self,
        path: &str,
        staged: bool,
        context_lines: Option<u32>,
    ) -> Result<GitFileHunks, GitError> {
        let normalized = normalize_path(path)?;
        match self.parsed_diff_file(&normalized, staged, context_lines)? {
            Some(file) => Ok(file.to_model()),
            // 没有 diff 不等于出错：文件可能已被暂存干净，或者根本没有改动。
            None => Ok(GitFileHunks {
                path: normalized,
                binary: false,
                hunks: Vec::new(),
            }),
        }
    }

    /// 解析单文件的 diff 片段。
    ///
    /// 先按新路径匹配，再退回旧路径：重命名后外壳可能还拿着改名前的名字。
    fn parsed_diff_file(
        &self,
        path: &str,
        staged: bool,
        context_lines: Option<u32>,
    ) -> Result<Option<ParsedDiffFile>, GitError> {
        let text = self.raw_diff(Some(path), staged, context_lines)?;
        let mut files = parse_diff(&text);
        let position = files.iter().position(|file| file.path == path).or_else(|| {
            files
                .iter()
                .position(|file| file.old_path.as_deref() == Some(path))
        });
        Ok(position.map(|index| files.swap_remove(index)))
    }

    /// 按块暂存 / 取消暂存（`git apply --cached [--reverse]`）。
    ///
    /// ## 为什么不是 `git add -p`
    ///
    /// `add -p` 是交互式命令，要靠一串 y / n / s 的应答驱动，且它的分块口径与
    /// `git diff` 输出的 hunk 序号**不保证一致**（上下文合并规则不同）。外壳点的是
    /// diff 里的第 N 块，就必须让 git 应用**同一份** diff 里的第 N 块——所以这里把
    /// 那块原样拼成补丁喂给 `git apply --cached`，定位即证据。
    ///
    /// ## 定位失效为什么必须失败
    ///
    /// 只按序号找块是不够的：用户点完之后文件可能又被改过，此时「第 0 块」已经是
    /// 另一处改动，照序号暂存就会把**用户没选的内容**塞进索引。所以定位是
    /// 「序号 + 四个行号（+ 可选 header）」全等匹配，任何一项对不上都报
    /// [`GitError::HunkDrifted`] 并带上期望与实际位置——宁可让用户刷新重选，
    /// 也不替他猜。
    ///
    /// `staged` 的来源按方向区分：暂存看工作区 diff（`git diff`），取消暂存看索引
    /// diff（`git diff --cached`）——`--reverse` 只负责把补丁反向打进索引。
    pub fn stage_hunk(
        &self,
        selector: &HunkSelector,
        unstage: bool,
        context_lines: Option<u32>,
    ) -> Result<GitHunkOutcome, GitError> {
        let path = normalize_path(&selector.path)?;
        let file = self
            .parsed_diff_file(&path, unstage, context_lines)?
            .ok_or_else(|| GitError::HunkDrifted {
                path: path.clone(),
                index: selector.index,
                expected: selector.expected(),
                actual: None,
            })?;

        if file.binary {
            return Err(GitError::BinaryHunk { path });
        }

        let hunk = file
            .hunks
            .iter()
            .find(|hunk| hunk.index == selector.index)
            .ok_or_else(|| GitError::HunkDrifted {
                path: path.clone(),
                index: selector.index,
                expected: selector.expected(),
                actual: None,
            })?;

        if !selector.matches(hunk) {
            return Err(GitError::HunkDrifted {
                path: path.clone(),
                index: selector.index,
                expected: selector.expected(),
                actual: Some(hunk.location()),
            });
        }

        let patch = hunk_patch(&file, hunk);
        let mut args: Vec<&str> = vec!["apply", "--cached"];
        if unstage {
            args.push("--reverse");
        }
        // `--whitespace=nowarn`：补丁里本来就带着原文件的空白，让 git 再警告一遍
        // 没有意义；真正的冲突该由「patch does not apply」表达。
        args.push("--whitespace=nowarn");

        let output = self.runner().execute(&[], &args, Some(&patch))?;
        if !output.success {
            return Err(classify_hunk_apply_failure(
                &path,
                selector.index,
                selector.expected(),
                Some(hunk.location()),
                &args.join(" "),
                &output.stderr_trimmed(),
                output.code,
            ));
        }

        Ok(GitHunkOutcome {
            hunk: hunk.clone(),
            unstage,
            staged_total: self.staged_count()?,
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
    // 分支切换
    // -----------------------------------------------------------------------

    /// 切换分支（`create` 为真时新建）。
    ///
    /// **脏工作区默认拒绝**：`allow_dirty=false` 时先看已跟踪文件有没有未提交改动，
    /// 有就直接返回 [`GitError::DirtyWorkspace`]（带文件清单）。理由不是「git 做不到」，
    /// 而是 git 能做到的部分最难解释——它会把这些改动带到新分支上，用户切过去看到
    /// 「怎么新分支里也有这些改动」，再切回来又「怎么这边也没了」，于是不敢碰。
    /// 明确拒绝 + 给出文件清单，用户至少知道该先 commit 还是先 stash。
    ///
    /// 未跟踪文件**不算脏**：`checkout` 从不删除未跟踪文件，把它们也算脏会让几乎所有
    /// 真实仓库都切不了分支。真正会挡住 checkout 的是「未跟踪文件将被覆盖」，那种情况
    /// git 会报错，这里把它的报错解析回 [`GitError::DirtyWorkspace`]，UI 的提示口径统一。
    pub fn checkout(
        &self,
        branch: &str,
        create: bool,
        start_point: Option<&str>,
        allow_dirty: bool,
    ) -> Result<GitCheckoutOutcome, GitError> {
        let branch = validate_ref_name(branch)?;
        let start_point = match start_point {
            Some(raw) => Some(validate_ref_name(raw)?),
            None => None,
        };
        if !allow_dirty {
            let dirty = self.tracked_changes()?;
            if !dirty.is_empty() {
                return Err(GitError::DirtyWorkspace { files: dirty });
            }
        }

        let previous = self.symbolic_ref()?;
        let mut args: Vec<String> = vec!["checkout".to_string()];
        if create {
            args.push("-b".to_string());
        }
        args.push(branch.clone());
        if let Some(start) = &start_point {
            args.push(start.clone());
        }
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = self.runner().execute(&[], &borrowed, None)?;
        if !output.success {
            let stderr = output.stderr_trimmed();
            let blocked = parse_blocked_files(&stderr);
            if !blocked.is_empty() {
                return Err(GitError::DirtyWorkspace { files: blocked });
            }
            return Err(GitError::Command {
                args: args.join(" "),
                stderr,
                code: output.code,
            });
        }

        let oid = self.head_oid()?;
        let short_oid = oid.as_deref().map(short_of);
        Ok(GitCheckoutOutcome {
            branch: self.symbolic_ref()?.unwrap_or(branch),
            previous,
            created: create,
            oid,
            short_oid,
        })
    }

    // -----------------------------------------------------------------------
    // 远端
    // -----------------------------------------------------------------------

    /// 抓取远端更新，返回**实际发生变化**的远端跟踪引用。
    ///
    /// `prune` 为 `true` 时才加 `--prune`（`git fetch --prune`）：删掉远端已经消失的
    /// 跟踪引用是破坏性的，用户会看不出「我的 `origin/x` 怎么没了」，所以必须由外壳
    /// 显式要，不能默认打开。
    /// 比较「fetch 前 / 后」两轮 `refs/remotes` 而不是解析 fetch 的进度输出：后者是给
    /// 人看的、随 git 版本变（`-> FETCH_HEAD` 写法改过好几版），前者是稳定契约。
    pub fn fetch(&self, remote: Option<&str>, prune: bool) -> Result<GitFetchOutcome, GitError> {
        let remote = match remote {
            Some(raw) => validate_ref_name(raw)?,
            None => String::new(),
        };
        let before = self.remote_refs()?;

        let mut args: Vec<String> = vec!["fetch".to_string()];
        if prune {
            args.push("--prune".to_string());
        }
        if !remote.is_empty() {
            args.push(remote.clone());
        }
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = self.runner().execute(&[], &borrowed, None)?;
        if !output.success {
            return Err(crate::error::classify_remote_failure(
                &args.join(" "),
                &output.stderr_trimmed(),
                output.code,
                self.symbolic_ref()?.as_deref(),
            ));
        }

        let after = self.remote_refs()?;
        let mut updated = Vec::new();
        for (name, (new_full, new_short)) in &after {
            let old = before.get(name);
            if old.map(|(full, _)| full) == Some(new_full) {
                continue;
            }
            let (old_short, forced) = match old {
                Some((old_full, old_short)) => (
                    Some(old_short.clone()),
                    !self.is_ancestor(old_full, new_full)?,
                ),
                None => (None, false),
            };
            updated.push(GitRefUpdate {
                name: name.clone(),
                old_short,
                new_short: new_short.clone(),
                forced,
            });
        }
        updated.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(GitFetchOutcome {
            remote: if remote.is_empty() {
                match self.symbolic_ref()? {
                    Some(current) => self
                        .upstream_remote_of(&current)?
                        .unwrap_or_else(|| "origin".to_string()),
                    None => "origin".to_string(),
                }
            } else {
                remote
            },
            updated,
        })
    }

    /// `pull --ff-only`：只接受快进，**不**生成合并提交、**不**rebase。
    ///
    /// 分叉时宁可失败（[`GitError::NotFastForward`]）也不替用户决定「合并还是变基」：
    /// 这两条路对历史形状的影响完全不同，是用户的选择题，不是 IDE 的默认值。
    pub fn pull(
        &self,
        remote: Option<&str>,
        branch: Option<&str>,
    ) -> Result<GitPullOutcome, GitError> {
        let current = self.symbolic_ref()?.ok_or_else(|| {
            GitError::Invalid("分离头指针状态下没有上游分支，请先切到某个分支".to_string())
        })?;
        let upstream = self.upstream_of(&current)?;
        // 缺省的 remote / branch 一律从上游推出；上游不存在时**不猜** `origin/<当前分支>`，
        // 而是报 [`GitError::NoUpstream`]（7009）。猜错的后果是「静默拉到另一个分支」或
        // 「弹出与真实原因无关的远端错误」，两者都比直接说「这个分支没有上游」更难查。
        let (remote, branch) = match (remote, branch) {
            (Some(remote), Some(branch)) => {
                (validate_ref_name(remote)?, validate_ref_name(branch)?)
            }
            (Some(remote), None) => {
                let remote = validate_ref_name(remote)?;
                let branch = upstream
                    .as_deref()
                    .and_then(|name| name.rsplit_once('/'))
                    .map(|(_, branch)| branch.to_string())
                    .ok_or_else(|| GitError::NoUpstream {
                        branch: current.clone(),
                    })?;
                (remote, branch)
            }
            (None, Some(branch)) => {
                let branch = validate_ref_name(branch)?;
                let remote = upstream
                    .as_deref()
                    .and_then(|name| name.rsplit_once('/'))
                    .map(|(remote, _)| remote.to_string())
                    .ok_or_else(|| GitError::NoUpstream {
                        branch: current.clone(),
                    })?;
                (remote, branch)
            }
            (None, None) => {
                let upstream = upstream.clone().ok_or_else(|| GitError::NoUpstream {
                    branch: current.clone(),
                })?;
                match upstream.rsplit_once('/') {
                    Some((remote, branch)) => (remote.to_string(), branch.to_string()),
                    // 上游只写了分支名（`branch.main.merge` 配了、`remote` 是 `.`）：
                    // 这种配置下 git 自己会报错，这里退回 origin 只为拼一句可读的上游名。
                    None => ("origin".to_string(), upstream),
                }
            }
        };
        let upstream_name = upstream.unwrap_or_else(|| format!("{remote}/{branch}"));

        let previous_oid = self
            .head_oid()?
            .ok_or_else(|| GitError::Invalid("当前分支还没有任何提交，无从拉取".to_string()))?;

        let mut args: Vec<String> = vec![
            "pull".to_string(),
            "--ff-only".to_string(),
            "--no-edit".to_string(),
        ];
        args.push(remote.clone());
        args.push(branch.clone());
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = self.runner().execute(&[], &borrowed, None)?;
        if !output.success {
            let stderr = output.stderr_trimmed();
            let blocked = parse_blocked_files(&stderr);
            if !blocked.is_empty() {
                return Err(GitError::DirtyWorkspace { files: blocked });
            }
            return Err(crate::error::classify_remote_failure(
                &args.join(" "),
                &stderr,
                output.code,
                Some(&current),
            ));
        }

        let oid = self
            .head_oid()?
            .ok_or_else(|| GitError::Io("pull 之后读不到 HEAD".to_string()))?;
        let short_oid = short_of(&oid);
        let up_to_date = oid == previous_oid;
        let range = format!("{previous_oid}..{oid}");
        let commits = if up_to_date {
            0
        } else {
            self.rev_count(&range)?
        };
        let changed_paths = if up_to_date {
            Vec::new()
        } else {
            self.diff_name_list(&range)?
        };

        Ok(GitPullOutcome {
            branch: current,
            upstream: upstream_name,
            previous_oid,
            oid,
            short_oid,
            commits,
            changed_paths,
            up_to_date,
        })
    }

    /// 推送当前分支。
    ///
    /// `force` 走 `--force-with-lease` 而不是 `--force`：租约检查会在「远端已被别人
    /// 推过」时失败，正好挡住最痛的那种事故（把同事的提交推平）。裸 `--force` 在本
    /// crate 里**没有任何**调用路径。是否允许 `force` 由外壳的二次确认决定，内核只
    /// 如实执行并回报 `forced`。
    pub fn push(
        &self,
        remote: Option<&str>,
        branch: Option<&str>,
        force: bool,
        set_upstream: bool,
    ) -> Result<GitPushOutcome, GitError> {
        let current = self.symbolic_ref()?.ok_or_else(|| {
            GitError::Invalid("分离头指针状态下无法推送，请先切到某个分支".to_string())
        })?;
        let branch = match branch {
            Some(raw) => validate_ref_name(raw)?,
            None => current,
        };
        let remote = match remote {
            Some(raw) => validate_ref_name(raw)?,
            // 没指定远端、分支也还没有上游时：`set_upstream` 说明用户就是要给 origin
            // 建一条上游，可以推；否则报 7009 交回外壳去问「要以 origin 为上游推上去吗」。
            // 替用户默认 origin 看着无害，实际会留下「推成功了，但下次拉取仍然不知道
            // 拉哪」的半截状态。
            None => match self.upstream_remote_of(&branch)? {
                Some(name) => name,
                None if set_upstream => "origin".to_string(),
                None => {
                    return Err(GitError::NoUpstream {
                        branch: branch.clone(),
                    })
                }
            },
        };
        let remote_branch = self
            .upstream_of(&branch)?
            .and_then(|name| name.rsplit_once('/').map(|(_, branch)| branch.to_string()))
            .unwrap_or_else(|| branch.clone());

        let mut args: Vec<String> = vec!["push".to_string(), "--porcelain".to_string()];
        if force {
            args.push("--force-with-lease".to_string());
        }
        if set_upstream {
            args.push("--set-upstream".to_string());
        }
        args.push(remote.clone());
        args.push(format!("{branch}:{remote_branch}"));
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = self.runner().execute(&[], &borrowed, None)?;
        if !output.success {
            return Err(crate::error::classify_remote_failure(
                &args.join(" "),
                &output.stderr_trimmed(),
                output.code,
                Some(&branch),
            ));
        }

        // `--porcelain` 的每个**引用**一行 `<flag>\t<from>:<to>\t<summary>`，`=` 表示
        // 远端已经一致（没有要推的提交）。但同一份 stdout 里还夹着 `To <url>` / `Done`
        // 这类不带 TAB 的横幅行，直接对全部行 `starts_with('=')` 会把它们算成「有更新」，
        // 于是「再推一次」被误报成推了新东西。只认带第二个字段的行。
        let up_to_date = output
            .stdout_trimmed()
            .lines()
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let flag = fields.next()?;
                fields.next()?;
                flag.chars().next()
            })
            .all(|flag| flag == '=');

        let oid = self
            .head_oid()?
            .ok_or_else(|| GitError::Invalid("当前分支还没有提交可推送".to_string()))?;
        Ok(GitPushOutcome {
            branch,
            remote,
            remote_branch,
            forced: force,
            up_to_date,
            short_oid: short_of(&oid),
            oid,
        })
    }

    // -----------------------------------------------------------------------
    // stash
    // -----------------------------------------------------------------------

    /// 保存当前改动到栈顶。
    ///
    /// `include_untracked` 对应 `-u`：默认**不**带上未跟踪文件（git 的默认），因为用户
    /// 常常是想「把这几个改动收起来」，而不是「把刚生成的一堆产物也塞进 stash」。
    pub fn stash_push(
        &self,
        message: Option<&str>,
        include_untracked: bool,
        paths: &[String],
    ) -> Result<GitStashOutcome, GitError> {
        if let Some(text) = message {
            if text.trim().is_empty() {
                return Err(GitError::Invalid("stash 说明不能是空白".to_string()));
            }
        }
        let normalized = normalize_all(paths)?;

        let mut args: Vec<String> = vec!["stash".to_string(), "push".to_string()];
        if include_untracked {
            args.push("--include-untracked".to_string());
        }
        if let Some(text) = message {
            args.push("-m".to_string());
            args.push(text.trim().to_string());
        }
        if !normalized.is_empty() {
            args.push("--".to_string());
            args.extend(normalized);
        }
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = self.runner().execute(&[], &borrowed, None)?;
        if !output.success {
            return Err(GitError::Command {
                args: args.join(" "),
                stderr: crate::error::summarize_stderr(&output.stderr_trimmed()),
                code: output.code,
            });
        }

        let list = self.stash_list()?;
        // 「没有改动可保存」时 git 打印 `No local changes to save` 且**什么都不新建**：
        // 不能因为成功退出就报一条不存在的记录给 UI。
        let saved = !output
            .stdout_trimmed()
            .to_ascii_lowercase()
            .contains("no local changes to save");
        Ok(GitStashOutcome {
            entry: if saved { list.first().cloned() } else { None },
            total: list.len() as u32,
        })
    }

    /// stash 列表（栈顶在最前）。
    pub fn stash_list(&self) -> Result<Vec<GitStashEntry>, GitError> {
        let raw = self
            .runner()
            .text(&["stash", "list", "--format=%gd%x1f%H%x1f%h%x1f%gs"])?;
        let mut entries = Vec::new();
        for line in raw.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let mut parts = line.split('\u{1f}');
            let reference = parts.next().unwrap_or_default();
            let oid = parts.next().unwrap_or_default().to_string();
            let short_oid = parts.next().unwrap_or_default().to_string();
            // `%gs` 里可能含 `\x1f` 之外的一切字符，含分隔符就重新拼回去，别把说明吃掉。
            let message = parts.collect::<Vec<_>>().join("\u{1f}");
            let Some(index) = parse_stash_index(reference) else {
                continue;
            };
            entries.push(GitStashEntry {
                index,
                oid,
                short_oid,
                message,
            });
        }
        Ok(entries)
    }

    /// `stash apply` / `stash pop`。
    ///
    /// 冲突时**不丢弃** stash：`git stash pop` 在遇到冲突时本来就不会删除记录，本层
    /// 据此把 git 的约束显式化——冲突时返回 [`GitError::StashConflict`]（带冲突文件），
    /// 用户解决完冲突可以再 `drop`，或者 `reset` 掉重来。
    pub fn stash_apply(
        &self,
        index: u32,
        drop_after: bool,
    ) -> Result<GitStashApplyOutcome, GitError> {
        let before = self.stash_list()?;
        if !before.iter().any(|entry| entry.index == index) {
            return Err(GitError::Invalid(format!("stash@{{{index}}} 不存在")));
        }
        let reference = format!("stash@{{{index}}}");
        let command = if drop_after { "pop" } else { "apply" };
        let args = ["stash", command, reference.as_str()];
        let output = self.runner().execute(&[], &args, None)?;
        if !output.success {
            // `git stash apply/pop` 的三方合并日志（`Auto-merging` / `CONFLICT`）走的是
            // **stdout**，只有「工作区有改动挡住落盘」那类提示才走 stderr。只看 stderr
            // 会把冲突降级成 7003，用户于是既看不到冲突文件、也拿不到「记录还在」的承诺。
            let merged = format!("{}\n{}", output.stdout_trimmed(), output.stderr_trimmed());
            let conflicts = parse_conflict_files(&merged);
            if !conflicts.is_empty() {
                return Err(GitError::StashConflict { files: conflicts });
            }
            let blocked = parse_blocked_files(&merged);
            if !blocked.is_empty() {
                // 工作区现有改动挡住了 stash 的落盘：stash 原样保留，让用户先处理。
                return Err(GitError::DirtyWorkspace { files: blocked });
            }
            return Err(GitError::Command {
                args: args.join(" "),
                stderr: crate::error::summarize_stderr(&merged),
                code: output.code,
            });
        }

        let total = self.stash_list()?.len() as u32;
        Ok(GitStashApplyOutcome {
            index,
            dropped: drop_after,
            total,
        })
    }

    /// 丢弃一条 stash 记录（**不可逆**，是否已二次确认由外壳负责）。
    pub fn stash_drop(&self, index: u32) -> Result<GitStashDropOutcome, GitError> {
        let list = self.stash_list()?;
        let entry = list
            .iter()
            .find(|entry| entry.index == index)
            .cloned()
            .ok_or_else(|| GitError::Invalid(format!("stash@{{{index}}} 不存在")))?;

        let reference = format!("stash@{{{index}}}");
        let args = ["stash", "drop", reference.as_str()];
        let output = self.runner().execute(&[], &args, None)?;
        if !output.success {
            return Err(GitError::Command {
                args: args.join(" "),
                stderr: crate::error::summarize_stderr(&output.stderr_trimmed()),
                code: output.code,
            });
        }
        Ok(GitStashDropOutcome {
            index,
            oid: entry.oid,
            message: entry.message,
            total: self.stash_list()?.len() as u32,
        })
    }

    // -----------------------------------------------------------------------
    // 内部
    // -----------------------------------------------------------------------

    /// HEAD 指向的提交（空仓库为 `None`）。
    fn head_oid(&self) -> Result<Option<String>, GitError> {
        let value =
            self.runner()
                .text_allow_failure(&["rev-parse", "--verify", "--quiet", "HEAD"])?;
        Ok(value.filter(|oid| !oid.is_empty()))
    }

    /// 已跟踪文件的未提交改动（未跟踪 / 忽略的不算，理由见 [`Self::checkout`]）。
    fn tracked_changes(&self) -> Result<Vec<String>, GitError> {
        Ok(self
            .status()?
            .entries
            .iter()
            .filter(|entry| {
                !matches!(
                    entry.kind,
                    crate::EntryKind::Untracked | crate::EntryKind::Ignored
                )
            })
            .map(|entry| entry.path.clone())
            .collect())
    }

    /// 所有远端跟踪引用：`名字 → (完整 SHA, 短 SHA)`。
    fn remote_refs(&self) -> Result<HashMap<String, (String, String)>, GitError> {
        // 必须一起取 `%(symref)`：`refs/remotes/origin/HEAD` 是**符号引用**，它的
        // `%(refname:short)` 会被 shorten 成 `origin`（而不是 `origin/HEAD`），只按
        // `ends_with("/HEAD")` 过滤根本拦不住它，于是 fetch 会多报出一条名为 `origin`
        // 的幽灵「远端分支」，并在首次 fetch 时把 `up_to_date` 误判成 false。
        let raw = self.runner().text(&[
            "for-each-ref",
            "--format=%(refname:short)%00%(objectname)%00%(objectname:short)%00%(symref)",
            "refs/remotes",
        ])?;
        let mut map = HashMap::new();
        for line in raw.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let mut parts = line.split('\0');
            let name = parts.next().unwrap_or_default();
            let full = parts.next().unwrap_or_default();
            let short = parts.next().unwrap_or_default();
            let symref = parts.next().unwrap_or_default();
            if name.is_empty() || !symref.is_empty() || name.ends_with("/HEAD") {
                continue;
            }
            map.insert(name.to_string(), (full.to_string(), short.to_string()));
        }
        Ok(map)
    }

    /// `branch.<name>.remote`（没配置时为 `None`）。
    fn upstream_remote_of(&self, branch: &str) -> Result<Option<String>, GitError> {
        let key = format!("branch.{branch}.remote");
        Ok(self.config_value(&key)?.filter(|name| name != "."))
    }

    /// 分支的上游全名（`origin/main` 形式）。
    fn upstream_of(&self, branch: &str) -> Result<Option<String>, GitError> {
        let key = format!("branch.{branch}.merge");
        let merge = self.config_value(&key)?;
        let remote = self.config_value(&format!("branch.{branch}.remote"))?;
        Ok(match (remote, merge) {
            (Some(remote), Some(merge)) => {
                let short = merge.strip_prefix("refs/heads/").unwrap_or(&merge);
                if remote == "." {
                    // 本地跟踪分支（`branch.x.remote = .`）：没有远端可拉。
                    Some(short.to_string())
                } else {
                    Some(format!("{remote}/{short}"))
                }
            }
            _ => None,
        })
    }

    /// `is_ancestor`：`old` 是否是 `new` 的祖先（判断这次更新是不是快进）。
    fn is_ancestor(&self, old: &str, new: &str) -> Result<bool, GitError> {
        let output =
            self.runner()
                .execute(&[], &["merge-base", "--is-ancestor", old, new], None)?;
        Ok(output.success)
    }

    /// `rev-list --count <range>`。
    fn rev_count(&self, range: &str) -> Result<u64, GitError> {
        let text = self.runner().text(&["rev-list", "--count", range])?;
        Ok(text.trim().parse::<u64>().unwrap_or(0))
    }

    /// 区间内变更文件路径（按路径升序，最多 [`MAX_PULL_CHANGED_PATHS`] 条）。
    ///
    /// `-z` 保证中文路径不被转义、不被引号包住；排序放在 Rust 侧做，因为
    /// `git diff --name-only` 的顺序既不保证字典序也不保证稳定。
    fn diff_name_list(&self, range: &str) -> Result<Vec<String>, GitError> {
        let raw = self.runner().bytes(&["diff", "--name-only", "-z", range])?;
        let mut paths: Vec<String> = String::from_utf8_lossy(&raw)
            .split('\0')
            .filter(|piece| !piece.trim().is_empty())
            .map(str::to_string)
            .collect();
        paths.sort();
        paths.truncate(MAX_PULL_CHANGED_PATHS);
        Ok(paths)
    }

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

/// 校验分支名 / 远端名 / 起点引用。
///
/// 这里挡的是**两类**问题：一是命令行注入（`-f`、`--orphan` 这类以 `-` 开头的名字会
/// 被 git 当成选项），二是明显不合法的引用（`..`、`~`、`^`、`:`、空格……）。
/// 完整的合法性判定仍交给 git——它认 `check-ref-format`，本层只做「不安全的先拦掉」。
fn validate_ref_name(raw: &str) -> Result<String, GitError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(GitError::Invalid("分支名不能为空".to_string()));
    }
    if trimmed.starts_with('-') {
        return Err(GitError::Invalid(format!(
            "引用名不能以 - 开头（会被当成命令选项）：{raw}"
        )));
    }
    if trimmed
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return Err(GitError::Invalid(format!("引用名不得含空白字符：{raw}")));
    }
    const FORBIDDEN: [&str; 8] = ["..", "~", "^", ":", "?", "*", "[", "\\"];
    if FORBIDDEN.iter().any(|needle| trimmed.contains(needle))
        || trimmed.starts_with('/')
        || trimmed.ends_with('/')
        || trimmed.contains("//")
        || trimmed.ends_with(".lock")
        || trimmed == "@"
    {
        return Err(GitError::Invalid(format!("不是合法的引用名：{raw}")));
    }
    Ok(trimmed.to_string())
}

/// 完整 SHA 的短形式（7 位，与 git 的默认长度一致）。
fn short_of(oid: &str) -> String {
    oid.chars().take(7).collect()
}

/// 从 `stash@{3}` 里取 `3`。
fn parse_stash_index(reference: &str) -> Option<u32> {
    let inside = reference.strip_prefix("stash@{")?.strip_suffix('}')?;
    inside.parse::<u32>().ok()
}

/// 解析 git 报出的「这些文件会被覆盖 / 会被删除」清单。
///
/// 三种场景的文案都长这样（列表每行以制表符缩进）：
/// 切分支时 `Your local changes to the following files would be overwritten by checkout`、
/// 拉取时 `... by merge`、以及未跟踪文件 `The following untracked working tree files
/// would be overwritten by checkout`。解析成路径清单后统一映射到 7006，UI 的提示口径
/// 就与「本层自己判定的脏工作区」一致。
fn parse_blocked_files(stderr: &str) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    let mut in_list = false;
    for line in stderr.lines() {
        let without_break = line.trim_end();
        if without_break.starts_with('\t') || without_break.starts_with("    ") {
            let path = without_break.trim();
            if in_list && !path.is_empty() && !files.iter().any(|seen| seen == path) {
                files.push(path.to_string());
            }
            continue;
        }
        let lowered = without_break.to_ascii_lowercase();
        in_list = lowered.contains("would be overwritten")
            || lowered.contains("would be removed")
            || lowered.contains("untracked working tree files");
    }
    files
}

/// 解析 stash / pull 冲突的 `CONFLICT` 行。
///
/// 两种形态都要覆盖：
/// - `CONFLICT (content): Merge conflict in src/a.rs`
/// - `CONFLICT (modify/delete): src/a.rs deleted in HEAD and modified in stash ...`
fn parse_conflict_files(stderr: &str) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    for line in stderr.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("CONFLICT") else {
            continue;
        };
        let path = if let Some((_, after)) = rest.split_once("Merge conflict in ") {
            Some(after.trim().to_string())
        } else if let Some((_, after)) = rest.split_once("): ") {
            after.split_whitespace().next().map(str::to_string)
        } else {
            None
        };
        if let Some(path) = path {
            if !path.is_empty() && !files.iter().any(|seen| seen == &path) {
                files.push(path);
            }
        }
    }
    files
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
