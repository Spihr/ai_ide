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
}
