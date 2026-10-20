//! Agent 层的错误类型。
//!
//! 错误分成两类，分界线在 [`AgentError::is_fatal`]：
//!
//! - **终止循环**：路径闸门拒绝（越界 `1002`、敏感路径 `1003`、非法路径 `1006`）与模型
//!   调用失败。前者与 M3 的上下文注入同口径——路径是安全边界，不参与容错；后者意味着
//!   循环已经无法继续。
//! - **回灌模型**：文件不存在、`edit` 目标文本不唯一、参数 JSON 坏了……这些会变成一条
//!   `role = tool` 的消息回到模型手里，让它自己纠正后重试，而不是让整条对话失败。
//!   M4 验收里「非唯一匹配自动重试」正是建立在这条通道上。
//!
//! 注意 `WorkspaceError::Path` 也要判为致命：工具直接调 `fs_ops` / `tree` 时，路径错误是
//! 包在 `WorkspaceError` 里冒出来的，只认 `AgentError::Path` 会漏掉这半边闸门。

use ai_ide_protocol::{ErrorCode, ErrorObject};
use ai_ide_workspace::{PathError, WorkspaceError};

/// Agent 循环与工具执行错误。
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// 路径闸门拒绝（终止循环）。
    #[error(transparent)]
    Path(#[from] PathError),

    /// 工作区能力层错误；其 `Path` 分支同样终止循环。
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),

    /// 检索层错误（坏正则、坏 glob 等，回灌模型）。
    #[error(transparent)]
    Index(#[from] ai_ide_indexer::IndexError),

    /// 模型请求了未注册的工具（回灌）。
    #[error("未注册的工具：{0}")]
    UnknownTool(String),

    /// 工具参数不合法（回灌）。
    #[error("工具参数不合法（{tool}）：{reason}")]
    BadArgs { tool: String, reason: String },

    /// 工具执行失败但可恢复（回灌）。
    #[error("{tool} 执行失败：{message}")]
    ToolFailed { tool: String, message: String },

    /// 目标内容对不上：`old_text` 无法唯一定位（回灌，复用协议 `3003`）。
    ///
    /// 与 `write` / `edit` 的乐观锁失配同族——都是「你以为的磁盘内容 ≠ 实际内容」。
    /// 乐观锁那条走 `fs_ops` 的 `WorkspaceError::HashMismatch`，本身已映射 `3003`；
    /// 这里补上工具自己判定的那一半（`old_text` 命中 0 处或 ≥2 处），两处口径一致。
    #[error("{tool} 冲突：{message}")]
    Conflict { tool: String, message: String },

    /// 模型调用失败（终止循环）。
    #[error("模型调用失败：{0}")]
    Model(String),
}

impl AgentError {
    /// 是否终止整个循环（而不是把失败回灌给模型）。
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::Path(_) | Self::Model(_) | Self::Workspace(WorkspaceError::Path(_))
        )
    }

    /// 映射为协议错误码（见 `docs/03-内核接口协议规范.md` §1.3）。
    ///
    /// 工具级失败不占用 `3xxx` 语义码：它走 `role = tool` 消息回灌，不产生 JSON-RPC 错误。
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Path(err) => err.code(),
            Self::Workspace(err) => err.code(),
            Self::Index(err) => err.code(),
            Self::UnknownTool(_) | Self::BadArgs { .. } => ErrorCode::INVALID_PARAMS,
            Self::ToolFailed { .. } => ErrorCode::INTERNAL_ERROR,
            Self::Conflict { .. } => ErrorCode::DIFF_APPLY_CONFLICT,
            Self::Model(_) => ErrorCode::PROVIDER_UNAVAILABLE,
        }
    }

    /// 转成 JSON-RPC 错误对象，供后续 `agent.*` 方法接线直接复用。
    pub fn to_error_object(&self) -> ErrorObject {
        let object = ErrorObject::new(self.code(), self.to_string());
        match self {
            Self::Path(PathError::OutOfWorkspace { path }) => {
                object.with_data(serde_json::json!({ "path": path }))
            }
            _ => object,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_gate_failures_are_fatal_with_protocol_codes() {
        let out = AgentError::Path(PathError::OutOfWorkspace {
            path: "..\\x".into(),
        });
        assert!(out.is_fatal());
        assert_eq!(out.code(), ErrorCode::PATH_OUT_OF_WORKSPACE);

        let denied = AgentError::Path(PathError::Denied {
            path: ".env".into(),
        });
        assert!(denied.is_fatal());
        assert_eq!(denied.code(), ErrorCode::PATH_DENIED);
    }

    #[test]
    fn workspace_path_failure_is_fatal_too() {
        let err = AgentError::Workspace(WorkspaceError::Path(PathError::OutOfWorkspace {
            path: "..".into(),
        }));
        assert!(err.is_fatal(), "包在 WorkspaceError 里的越界同样要终止循环");
        assert_eq!(err.code(), ErrorCode::PATH_OUT_OF_WORKSPACE);
    }

    #[test]
    fn recoverable_failures_are_not_fatal() {
        assert!(!AgentError::UnknownTool("nope".into()).is_fatal());
        assert_eq!(
            AgentError::UnknownTool("nope".into()).code(),
            ErrorCode::INVALID_PARAMS
        );

        let failed = AgentError::ToolFailed {
            tool: "edit".into(),
            message: "非唯一匹配".into(),
        };
        assert!(!failed.is_fatal());

        let conflict = AgentError::Conflict {
            tool: "edit".into(),
            message: "old_text 出现 2 次".into(),
        };
        assert!(!conflict.is_fatal(), "定位冲突要回灌给模型，让它补锚点");
        assert_eq!(conflict.code(), ErrorCode::DIFF_APPLY_CONFLICT);

        let missing = AgentError::Workspace(WorkspaceError::NotFound("a.txt".into()));
        assert!(!missing.is_fatal(), "读不到文件应回灌给模型重试");
        assert_eq!(missing.code(), ErrorCode::FILE_NOT_FOUND);
    }

    #[test]
    fn model_failure_is_fatal() {
        assert!(AgentError::Model("上游挂了".into()).is_fatal());
    }

    #[test]
    fn out_of_workspace_error_object_carries_path() {
        let object = AgentError::Path(PathError::OutOfWorkspace {
            path: "C:\\Windows".into(),
        })
        .to_error_object();
        assert_eq!(object.code, ErrorCode::PATH_OUT_OF_WORKSPACE);
        assert_eq!(object.data.unwrap()["path"], "C:\\Windows");
    }
}
