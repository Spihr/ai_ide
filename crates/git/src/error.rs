//! Git 层错误。
//!
//! 这一层不认识协议错误码（`crates/git` 不依赖 `ai-ide-protocol`，与 `crates/terminal`
//! 同一取舍）：码表映射留在 `crates/core/src/git_api.rs`。因此每个变体都带着足够
//! 结构化的信息，让上层能拼 `data`（路径、stderr、参数），而不是只剩一句人话。

use std::path::Path;

/// Git 能力失败的原因。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GitError {
    /// 机器上没有可用的 git：`Command::spawn` 报 `NotFound`，或 `AI_IDE_GIT` 不可执行。
    #[error("未找到可用的 git 可执行文件：{0}")]
    NotAvailable(String),

    /// 工作区根不是 Git 工作树（未 `init`、或路径已不存在）。
    #[error("不是 Git 仓库：{0}")]
    NotARepository(String),

    /// 参数不可用：路径越界 / 绝对路径、上下文行数越界、提交信息为空。
    #[error("git 参数非法：{0}")]
    Invalid(String),

    /// 暂存区没有可提交的内容。
    #[error("没有可提交的变更")]
    NothingToCommit,

    /// 工作区有未提交改动，会被这次分支切换带走或挡住。
    ///
    /// 带 `files` 而不是只带一句话：外壳要把它们渲染成可点击清单，让用户就地决定
    /// 「提交这些」还是「先 stash」，而不是自己再跑一次 `status` 去猜是哪些文件。
    #[error("工作区有 {} 个未提交改动，已拒绝切换分支", .files.len())]
    DirtyWorkspace {
        /// 脏文件路径（相对工作区根）。
        files: Vec<String>,
    },

    /// 远端拒绝：非快进（本地落后或分叉）。
    #[error("非快进：{detail}")]
    NotFastForward {
        /// git 原文摘要（已压低成一句）。
        detail: String,
    },

    /// 远端认证失败。
    #[error("远端认证失败：{detail}")]
    AuthFailed {
        /// git 原文摘要。
        detail: String,
    },

    /// 当前分支没有上游分支。
    #[error("分支 {branch} 没有上游分支")]
    NoUpstream {
        /// 分支名。
        branch: String,
    },

    /// `stash pop` / `apply` 撞冲突：改动已落盘但未合并，**stash 未被丢弃**。
    #[error("stash 应用冲突：{} 个文件待解决", .files.len())]
    StashConflict {
        /// 冲突文件路径。
        files: Vec<String>,
    },

    /// 二进制文件没有可寻址的 hunk：`git diff` 只给出 `Binary files ... differ`。
    ///
    /// 为什么是拒绝而不是「当成整文件那块」：二进制没有「第几块」这回事，把整文件
    /// 塞进索引会让调用方以为自己只挑了一片改动，实际整份文件进了暂存区——
    /// 这正是「按块暂存」最不能出的错。
    #[error("二进制文件不支持按 hunk 暂存：{path}")]
    BinaryHunk {
        /// 相对工作区根的路径。
        path: String,
    },

    /// hunk 定位失效：文件已被改、行号漂移，或该序号在当前 diff 里已不存在。
    ///
    /// 带 `expected` / `actual` 两段可读描述：用户看到的不是「失败了」，而是
    /// 「你要的那块在 A，现在在 B」——刷新后重选即可，不用猜。
    #[error("hunk 定位失效：{path} 第 {index} 块（期望 {expected}）")]
    HunkDrifted {
        /// 相对工作区根的路径。
        path: String,
        /// 调用方给的序号。
        index: u32,
        /// 调用方期望的位置。
        expected: String,
        /// 当前同序号 hunk 的**实际**位置；该序号已不存在时为 `None`。
        actual: Option<String>,
    },

    /// git 命令非零退出。`args` 是去掉全局参数后的子命令与实参，`stderr` 是原文。
    #[error("git {args} 失败（退出码 {code:?}）：{stderr}")]
    Command {
        /// 子命令与实参，空格拼接（仅供展示，不含 `-C` 等全局参数）。
        args: String,
        /// stderr 原文（已 trim）。
        stderr: String,
        /// 退出码；被信号杀死时为 `None`。
        code: Option<i32>,
    },

    /// 进程层 IO 失败（写 stdin、读输出等）。
    #[error("git IO 失败：{0}")]
    Io(String),
}

impl GitError {
    /// 不是仓库：把路径转成字符串以便上层放进 `data.path`。
    pub fn not_a_repository(path: &Path) -> Self {
        Self::NotARepository(path.display().to_string())
    }

    /// 是否属于「环境不满足」类错误（未装 git / 不是仓库）。
    ///
    /// 上层据此决定是否值得提示用户去装 git，而不是当成一次失败的写操作。
    pub fn is_environment(&self) -> bool {
        matches!(self, Self::NotAvailable(_) | Self::NotARepository(_))
    }
}

/// 远端失败的判别式：**只**靠 git 的英文 stderr（`LC_ALL=C` 保证稳定）。
///
/// 为什么要在这一层判别、而不是把 stderr 原样甩给 UI：非快进、认证失败、无上游这三种
/// 情形下用户的**下一步动作完全不同**（去拉取 / 去登录 / 去设上游）。只给一句
/// 「git push 失败」，用户只能自己去搜 stderr。
///
/// 判别顺序是「越具体越先」：无上游 → 认证 → 非快进。反过来会把 `git push` 在没有
/// 上游时输出的提示（里面含 `--set-upstream`）误判成别的问题。
pub(crate) fn classify_remote_failure(
    args: &str,
    stderr: &str,
    code: Option<i32>,
    branch: Option<&str>,
) -> GitError {
    let lowered = stderr.to_ascii_lowercase();
    let detail = summarize_stderr(stderr);

    const NO_UPSTREAM: [&str; 4] = [
        "no upstream configured",
        "no tracking information for the current branch",
        "has no upstream branch",
        "there is no tracking information",
    ];
    if NO_UPSTREAM.iter().any(|needle| lowered.contains(needle)) {
        return GitError::NoUpstream {
            branch: branch.unwrap_or("HEAD").to_string(),
        };
    }

    const AUTH: [&str; 10] = [
        "authentication failed",
        "could not read username",
        "could not read password",
        "permission denied (publickey",
        "permission denied, please try again",
        "terminal prompts disabled",
        "invalid username or password",
        "host key verification failed",
        "access denied",
        "repository not found",
    ];
    if AUTH.iter().any(|needle| lowered.contains(needle)) {
        return GitError::AuthFailed { detail };
    }

    const NOT_FAST_FORWARD: [&str; 6] = [
        "non-fast-forward",
        "not possible to fast-forward",
        "failed to push some refs",
        "[rejected]",
        "fetch first",
        "diverged",
    ];
    if NOT_FAST_FORWARD
        .iter()
        .any(|needle| lowered.contains(needle))
    {
        return GitError::NotFastForward { detail };
    }

    GitError::Command {
        args: args.to_string(),
        stderr: detail,
        code,
    }
}

/// 把多行 stderr 压成一句给用户看的摘要。
///
/// 优先取第一条 `fatal:` / `error:` / `remote:` —— git 的第一句通常就是结论，后面
/// 跟着的往往是一大段 `hint:` 教怎么修，塞进 toast 只会糊成一团。
pub(crate) fn summarize_stderr(stderr: &str) -> String {
    const MAX: usize = 300;
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|line| {
            let lowered = line.to_ascii_lowercase();
            lowered.starts_with("fatal:")
                || lowered.starts_with("error:")
                || lowered.starts_with("remote:")
        })
        .or_else(|| stderr.lines().map(str::trim).find(|line| !line.is_empty()))
        .unwrap_or("git 未给出错误信息");
    if line.len() <= MAX {
        return line.to_string();
    }
    // 按字符边界截断：stderr 里可能是远端返回的中文/日文提示。
    let mut cut = MAX;
    while cut > 0 && !line.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &line[..cut])
}

/// `git apply --cached` 失败的判别式。
///
/// 与 [`classify_remote_failure`] 同一条理由：`apply` 的非零退出里绝大多数是
/// **定位失效**（补丁与当前索引对不上），少数是补丁本身坏了或环境问题。前者用户
/// 的动作是「刷新后重选那一块」，后者是「报 bug / 查环境」——不能混成一句 7003。
///
/// 判别只看 git 固定的英文短语（`LC_ALL=C` 保证稳定），命中即判定位失效；
/// 未命中的一律退回 [`GitError::Command`]，不猜。
pub(crate) fn classify_hunk_apply_failure(
    path: &str,
    index: u32,
    expected: String,
    actual: Option<String>,
    args: &str,
    stderr: &str,
    code: Option<i32>,
) -> GitError {
    const DRIFT: [&str; 6] = [
        "patch does not apply",
        "does not match index",
        "patch failed",
        "while searching for",
        "corrupt patch",
        "unrecognized input",
    ];
    let lowered = stderr.to_ascii_lowercase();
    if DRIFT.iter().any(|needle| lowered.contains(needle)) {
        return GitError::HunkDrifted {
            path: path.to_string(),
            index,
            expected,
            actual,
        };
    }
    GitError::Command {
        args: args.to_string(),
        stderr: stderr.trim().to_string(),
        code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_errors_are_recognized() {
        assert!(GitError::NotAvailable("git".into()).is_environment());
        assert!(GitError::NotARepository("E:\\tmp".into()).is_environment());
        assert!(!GitError::NothingToCommit.is_environment());
        assert!(!GitError::Invalid("x".into()).is_environment());
    }

    #[test]
    fn command_error_message_keeps_stderr_and_code() {
        let err = GitError::Command {
            args: "status --porcelain=v2".into(),
            stderr: "fatal: not a git repository".into(),
            code: Some(128),
        };
        let text = err.to_string();
        assert!(text.contains("status --porcelain=v2"), "{text}");
        assert!(text.contains("not a git repository"), "{text}");
        assert!(text.contains("128"), "{text}");
    }

    #[test]
    fn not_a_repository_uses_display_path() {
        let err = GitError::not_a_repository(Path::new("E:\\work\\demo"));
        match err {
            GitError::NotARepository(path) => assert_eq!(path, "E:\\work\\demo"),
            other => panic!("意外变体：{other:?}"),
        }
    }

    // 下面这些 stderr 全部取自真实 git 输出（`LC_ALL=C` 下），不是凭空编的样例：
    // 判别式一旦与真实文案脱节，错误码就会静默退化回 7003，而这几个码存在的意义
    // 正是让 UI 给出不同的下一步。

    #[test]
    fn push_without_upstream_is_classified_as_no_upstream() {
        let stderr = "fatal: The current branch feature/login has no upstream branch.\n\
To push the current branch and set the remote as upstream, use\n\n\
    git push --set-upstream origin feature/login\n";
        let err = classify_remote_failure(
            "push origin feature/login",
            stderr,
            Some(128),
            Some("feature/login"),
        );
        match err {
            GitError::NoUpstream { branch } => assert_eq!(branch, "feature/login"),
            other => panic!("应判为无上游：{other:?}"),
        }
    }

    #[test]
    fn pull_without_tracking_information_is_no_upstream() {
        let stderr = "There is no tracking information for the current branch.\n\
Please specify which branch you want to merge with.\n";
        let err = classify_remote_failure("pull", stderr, Some(1), Some("topic"));
        assert!(matches!(err, GitError::NoUpstream { .. }), "{err:?}");
    }

    #[test]
    fn https_credential_prompt_failure_is_auth() {
        // `GIT_TERMINAL_PROMPT=0` 让 git 在需要凭据时直接失败，文案就是这两句之一。
        let stderr =
            "fatal: could not read Username for 'https://example.com': terminal prompts disabled";
        let err = classify_remote_failure("fetch origin", stderr, Some(128), Some("main"));
        match err {
            GitError::AuthFailed { detail } => {
                assert!(detail.contains("could not read Username"), "{detail}");
            }
            other => panic!("应判为认证失败：{other:?}"),
        }
    }

    #[test]
    fn ssh_publickey_rejection_is_auth() {
        let stderr = "git@github.com: Permission denied (publickey).\n\
fatal: Could not read from remote repository.";
        let err = classify_remote_failure("fetch origin", stderr, Some(128), Some("main"));
        assert!(matches!(err, GitError::AuthFailed { .. }), "{err:?}");
    }

    #[test]
    fn push_behind_remote_is_not_fast_forward() {
        let stderr = "To https://example.com/demo.git\n \
! [rejected]        main -> main (non-fast-forward)\n\
error: failed to push some refs to 'https://example.com/demo.git'\n";
        let err = classify_remote_failure("push origin main", stderr, Some(1), Some("main"));
        match err {
            // 摘要取第一条 `fatal:` / `error:` / `remote:` 开头的行，即 `error:` 那句。
            GitError::NotFastForward { detail } => {
                assert!(detail.contains("failed to push some refs"), "{detail}");
            }
            other => panic!("应判为非快进：{other:?}"),
        }
    }

    #[test]
    fn non_ff_only_pull_is_not_fast_forward() {
        let stderr = "fatal: Not possible to fast-forward, aborting.";
        let err = classify_remote_failure(
            "pull --ff-only origin main",
            stderr,
            Some(128),
            Some("main"),
        );
        match err {
            GitError::NotFastForward { detail } => {
                assert!(detail.contains("fast-forward"), "{detail}");
            }
            other => panic!("应判为非快进：{other:?}"),
        }
    }

    #[test]
    fn unrecognised_stderr_stays_a_plain_command_error() {
        let err = classify_remote_failure(
            "fetch origin",
            "fatal: unable to access 'https://example.com': Could not resolve host: example.com",
            Some(128),
            None,
        );
        match err {
            GitError::Command { args, stderr, code } => {
                assert_eq!(args, "fetch origin");
                assert!(stderr.starts_with("fatal:"), "{stderr}");
                assert_eq!(code, Some(128));
            }
            other => panic!("未识别原因应保留 7003：{other:?}"),
        }
    }

    #[test]
    fn upstream_wins_over_other_signals() {
        // 真实 `push` 无上游时的提示里含 `--set-upstream`，绝不能被当成认证问题：
        // 用户会白跑一趟去查凭据。
        let stderr = "fatal: The current branch main has no upstream branch.\n\
To push the current branch and set the remote as upstream, use\n\
    git push --set-upstream origin main";
        let err = classify_remote_failure("push", stderr, Some(128), Some("main"));
        assert!(matches!(err, GitError::NoUpstream { .. }), "{err:?}");
    }

    #[test]
    fn long_stderr_is_summarized_on_a_char_boundary() {
        let long = format!("fatal: {}", "远端提示".repeat(200));
        let summary = summarize_stderr(&long);
        assert!(summary.len() <= 303, "摘要应被截断：{}", summary.len());
        assert!(summary.ends_with('…'), "{summary}");
        assert!(summary.starts_with("fatal: "));
    }
}
