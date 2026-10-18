//! # ai-ide-workspace
//!
//! AI IDE 的**工作区能力层**：路径安全、文件读写、目录树、文件监听。
//!
//! 与 `ai-ide-indexer` 的分工：本 crate 只管「单个文件/目录怎么访问」，
//! 「在一堆文件里找东西」交给 indexer。
//!
//! ## 安全边界（对应 `docs/02-系统架构设计.md` §8）
//!
//! - 一切路径都要过 [`PathGuard`]：越界即拒，写操作额外拒敏感路径与系统目录；
//! - 删除只允许移入回收站（`trash`），永久删除不在能力范围内；
//! - 敏感路径检查是**内核侧**的最后一道闸门，不依赖上层是否自觉。

pub mod fs_ops;
pub mod ignore_rules;
pub mod manager;
pub mod path;
pub mod tree;
pub mod watcher;

pub use manager::{Workspace, WorkspaceManager};
pub use path::{PathError, PathGuard};

use std::path::Path;

/// 工作区操作的统一错误。
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error(transparent)]
    Path(#[from] PathError),

    #[error("文件或目录不存在：{0}")]
    NotFound(String),

    #[error("IO 错误（{path}）：{source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("内容冲突：磁盘内容与预期不一致")]
    HashMismatch { expected: String, actual: String },

    #[error("参数非法：{0}")]
    InvalidArgs(String),

    #[error("工作区未打开：{0}")]
    NotOpened(String),

    #[error("{0}")]
    Other(String),
}

impl WorkspaceError {
    /// 包装一次 IO 错误并带上路径。
    pub fn io(path: impl AsRef<Path>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.as_ref().display().to_string(),
            source,
        }
    }

    /// 映射为协议错误码。
    pub fn code(&self) -> ai_ide_protocol::ErrorCode {
        use ai_ide_protocol::ErrorCode;
        match self {
            Self::Path(err) => err.code(),
            Self::NotFound(_) => ErrorCode::FILE_NOT_FOUND,
            Self::Io { .. } => ErrorCode::IO_ERROR,
            // 乐观锁失败：03 文档规定写入冲突复用 3003。
            Self::HashMismatch { .. } => ErrorCode::DIFF_APPLY_CONFLICT,
            Self::InvalidArgs(_) => ErrorCode::INVALID_PARAMS,
            Self::NotOpened(_) => ErrorCode::WORKSPACE_NOT_OPENED,
            Self::Other(_) => ErrorCode::INTERNAL_ERROR,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_mapped() {
        use ai_ide_protocol::ErrorCode;

        assert_eq!(
            WorkspaceError::NotFound("x".into()).code(),
            ErrorCode::FILE_NOT_FOUND
        );
        assert_eq!(
            WorkspaceError::HashMismatch {
                expected: "a".into(),
                actual: "b".into()
            }
            .code(),
            ErrorCode::DIFF_APPLY_CONFLICT
        );
        assert_eq!(
            WorkspaceError::InvalidArgs("bad".into()).code(),
            ErrorCode::INVALID_PARAMS
        );
    }

    #[test]
    fn io_error_keeps_path_in_message() {
        let err = WorkspaceError::io("C:\\tmp\\a.txt", std::io::Error::other("boom"));
        assert!(err.to_string().contains("a.txt"));
        assert!(err.to_string().contains("boom"));
    }
}
