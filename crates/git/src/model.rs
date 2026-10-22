//! Git 领域的值类型。
//!
//! 与 `crates/terminal` 的做法一致：crate 内部只表达「git 事实」，不掺协议字段
//! （没有 `#[serde]`、没有 `ts-rs`）。协议 DTO 在 `crates/protocol`，翻译在
//! `crates/core/src/git_api.rs`——这样 git 层可以独立测试，协议演进也不必改这里。

use std::path::PathBuf;

/// 工作区变更清单的条目类别。
///
/// 只区分「git 语义上不同」的几类，不把 `M` / `T` / `D` 拆成枚举：那些字母已经在
/// [`GitStatusEntry::index`] / [`GitStatusEntry::worktree`] 里逐字透出，再拆一遍
/// 只会产生两份可能不一致的真相。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// 已有文件的改动（含类型变化、删除）。
    Changed,
    /// 重命名或复制。
    Renamed,
    /// 冲突未解决。
    Unmerged,
    /// 未跟踪。
    Untracked,
    /// 被忽略（默认不返回，保留枚举位以便将来 `include_ignored`）。
    Ignored,
}

impl EntryKind {
    /// 协议里 `kind` 字段的取值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::Renamed => "renamed",
            Self::Unmerged => "unmerged",
            Self::Untracked => "untracked",
            Self::Ignored => "ignored",
        }
    }
}

/// 一个变更条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatusEntry {
    /// 相对工作区根的路径（正斜杠）。
    pub path: String,
    /// 重命名 / 复制前的原路径。
    pub original_path: Option<String>,
    /// 索引区状态字母，未变更为 `.`。
    pub index: String,
    /// 工作区状态字母，未变更为 `.`。
    pub worktree: String,
    /// 条目类别。
    pub kind: EntryKind,
}

/// 工作区状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitStatus {
    /// 当前分支名；分离头指针时 `None`。
    pub branch: Option<String>,
    /// 是否分离头指针。
    pub detached: bool,
    /// HEAD 提交；仓库尚无提交（unborn）时 `None`。
    pub head_oid: Option<String>,
    /// 上游分支名。
    pub upstream: Option<String>,
    /// 领先上游的提交数。
    pub ahead: u64,
    /// 落后上游的提交数。
    pub behind: u64,
    /// 变更清单。
    pub entries: Vec<GitStatusEntry>,
}

impl GitStatus {
    /// 工作区是否干净。
    pub fn is_clean(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 一次 diff 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDiff {
    /// 请求的路径；全量 diff 为 `None`。
    pub path: Option<String>,
    /// 是否暂存区 diff。
    pub staged: bool,
    /// unified diff 文本。
    pub diff: String,
    /// 文本字节数。
    pub bytes: u64,
    /// 是否被 [`crate::DIFF_LIMIT_BYTES`] 截断。
    pub truncated: bool,
}

/// 一次暂存 / 取消暂存的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStageOutcome {
    /// 实际操作的路径；全量操作为空表。
    pub paths: Vec<String>,
    /// 是否全量操作。
    pub all: bool,
    /// 是否取消暂存。
    pub unstage: bool,
    /// 操作后暂存区条目数。
    pub staged_total: u64,
}

/// 一次提交的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommitOutcome {
    /// 完整 SHA。
    pub oid: String,
    /// 短 SHA。
    pub short_oid: String,
    /// 提交信息首行。
    pub summary: String,
    /// 提交后的分支名；分离头指针时 `None`。
    pub branch: Option<String>,
    /// 本次提交的文件数。
    pub files: u64,
    /// 作者 `Name <email>`。
    pub author: String,
}

/// 一个分支。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitBranchInfo {
    /// 分支名（远端形如 `origin/main`）。
    pub name: String,
    /// 完整 SHA。
    pub oid: String,
    /// 短 SHA。
    pub short_oid: String,
    /// 是否当前分支。
    pub is_current: bool,
    /// 是否远端跟踪分支。
    pub is_remote: bool,
    /// 上游分支名。
    pub upstream: Option<String>,
}

/// 分支列表。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitBranches {
    /// 当前分支名；分离头指针时 `None`。
    pub current: Option<String>,
    /// 是否分离头指针。
    pub detached: bool,
    /// 本地在前、远端在后。
    pub branches: Vec<GitBranchInfo>,
}

/// 用于 `GitService` 缓存的键：仓库工作树根。
pub type RepoKey = PathBuf;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_kind_strings_are_stable() {
        assert_eq!(EntryKind::Changed.as_str(), "changed");
        assert_eq!(EntryKind::Renamed.as_str(), "renamed");
        assert_eq!(EntryKind::Unmerged.as_str(), "unmerged");
        assert_eq!(EntryKind::Untracked.as_str(), "untracked");
        assert_eq!(EntryKind::Ignored.as_str(), "ignored");
    }

    #[test]
    fn status_cleanness_follows_entries() {
        let mut status = GitStatus::default();
        assert!(status.is_clean());
        status.entries.push(GitStatusEntry {
            path: "a.txt".into(),
            original_path: None,
            index: ".".into(),
            worktree: "M".into(),
            kind: EntryKind::Changed,
        });
        assert!(!status.is_clean());
    }
}
