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

/// 一次分支切换的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCheckoutOutcome {
    /// 切换后的分支名。
    pub branch: String,
    /// 切换前的分支名（原先是分离头指针时 `None`）。
    pub previous: Option<String>,
    /// 是否本次新建了分支。
    pub created: bool,
    /// 切换后 HEAD 指向的提交（空仓库新建分支时为 `None`）。
    pub oid: Option<String>,
    /// 短 SHA。
    pub short_oid: Option<String>,
}

/// `fetch` 里一个被更新的引用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRefUpdate {
    /// 引用短名（如 `origin/main`）。
    pub name: String,
    /// 更新前短 SHA（新建引用时为 `None`）。
    pub old_short: Option<String>,
    /// 更新后短 SHA。
    pub new_short: String,
    /// 是否是强制更新（远端被改写过）。
    pub forced: bool,
}

/// 一次 `fetch` 的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitFetchOutcome {
    /// 远端名。
    pub remote: String,
    /// 实际发生变化的引用（无变化时为空）。
    pub updated: Vec<GitRefUpdate>,
}

/// `pull` 回带的变更路径上限。
///
/// 上限存在的理由与 `diff` 的截断一致：一次快进可能带进上万个文件，把整张表塞进
/// 一条 JSON-RPC 响应只会把外壳的列表控件撑死。200 条足够回答「这次拉进来改了什么」，
/// 真正的全景用户会去看提交历史。
pub const MAX_PULL_CHANGED_PATHS: usize = 200;

/// 一次 `pull` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPullOutcome {
    /// 当前分支名。
    pub branch: String,
    /// 上游分支名（如 `origin/main`）。
    pub upstream: String,
    /// 拉取前 HEAD。
    pub previous_oid: String,
    /// 拉取后 HEAD。
    pub oid: String,
    /// 短 SHA。
    pub short_oid: String,
    /// 本次并入的提交数（已是最新时为 0）。
    pub commits: u64,
    /// 本次并入的变更文件路径（按路径升序，最多 [`MAX_PULL_CHANGED_PATHS`] 条）。
    pub changed_paths: Vec<String>,
    /// 是否本来就已是最新。
    pub up_to_date: bool,
}

/// 一次 `push` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPushOutcome {
    /// 推送的本地分支名。
    pub branch: String,
    /// 远端名。
    pub remote: String,
    /// 远端分支名。
    pub remote_branch: String,
    /// 是否强制推送（调用方必须已显式确认）。
    pub forced: bool,
    /// 是否本来就没有需要推的提交。
    pub up_to_date: bool,
    /// 推送后远端分支指向的提交。
    pub oid: String,
    /// 短 SHA。
    pub short_oid: String,
}

/// 一条 stash 记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStashEntry {
    /// `stash@{N}` 里的 N（0 是最近一条）。
    pub index: u32,
    /// 完整 SHA。
    pub oid: String,
    /// 短 SHA。
    pub short_oid: String,
    /// 用户给的说明（`git stash push -m` 的内容），没有时是 git 生成的默认描述。
    pub message: String,
}

/// 一次 `stash push` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStashOutcome {
    /// 新生成的记录；工作区本来就干净时为 `None`（git 不会新建记录）。
    pub entry: Option<GitStashEntry>,
    /// 保存后栈中总条数。
    pub total: u32,
}

/// 一次 `stash apply` / `pop` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStashApplyOutcome {
    /// 被应用的记录序号（应用前）。
    pub index: u32,
    /// 是否从栈里删掉了该记录（`pop` 为 `true`，`apply` 为 `false`）。
    pub dropped: bool,
    /// 应用后栈中剩余条数。
    pub total: u32,
}

/// 一次 `stash drop` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStashDropOutcome {
    /// 被丢弃的记录序号。
    pub index: u32,
    /// 被丢弃记录的 SHA（丢完就查不到了，必须在这里回传给 UI 以便事后追溯）。
    pub oid: String,
    /// 被丢弃记录的说明。
    pub message: String,
    /// 丢弃后栈中剩余条数。
    pub total: u32,
}

/// unified diff 里的**单个 hunk**。
///
/// 这是「按块暂存」的寻址单位：一个 hunk 就是 diff 里从 `@@` 到下一个 `@@`（或文件头）
/// 之间的那段。字段刻意与 git 的 `@@ -old_start,old_lines +new_start,new_lines @@`
/// 一一对应——外壳把点击的那一行 `@@` 原样回传即可，不需要自己做减法。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHunk {
    /// 相对工作区根的路径（正斜杠）。重命名时取新路径（`b/` 侧）。
    pub path: String,
    /// 文件内序号（0 起）：同一文件的第几块。
    pub index: u32,
    /// 旧文件起始行（1 起；新增文件的第一块是 0）。
    pub old_start: u32,
    /// 旧文件行数。
    pub old_lines: u32,
    /// 新文件起始行。
    pub new_start: u32,
    /// 新文件行数。
    pub new_lines: u32,
    /// `@@` 行里第二组 `@@` 之后的内容（`git diff -U0` 的「函数上下文」），原样保留。
    pub header: String,
    /// 正文行：每个元素含行首标记（` ` / `+` / `-`），
    /// 以及 git 输出的 `\ No newline at end of file` 行——它必须原样留在正文里，
    /// 否则回喂给 `git apply` 的补丁会把「末尾无换行」写成「末尾有换行」。
    pub lines: Vec<String>,
}

impl GitHunk {
    /// 一行可读的定位描述，用于错误信息里的 `expected` / `actual`。
    pub fn location(&self) -> String {
        format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_lines, self.new_start, self.new_lines
        )
    }
}

/// 一个文件的 hunk 清单（`hunks` 方法的返回值）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitFileHunks {
    /// 相对工作区根的路径。
    pub path: String,
    /// 是否是二进制文件：二进制没有 hunk 可寻址，`hunks` 恒为空。
    pub binary: bool,
    /// 按出现顺序排列，`index` 与下标一致。
    pub hunks: Vec<GitHunk>,
}

impl GitFileHunks {
    /// 按序号取一块。
    pub fn hunk(&self, index: u32) -> Option<&GitHunk> {
        self.hunks.iter().find(|hunk| hunk.index == index)
    }
}

/// 调用方（外壳）用来指定「要暂存哪一块」的定位信息。
///
/// 不只带 `index`：文件一旦被改过，第 0 块可能变成别的内容，**只按序号暂存会误提交
/// 用户没选的改动**。所以定位是「序号 + 四个行号（+ 可选 header）」，任何一项对不上
/// 都判定位失效（[`crate::GitError::HunkDrifted`]），宁可失败也不猜。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkSelector {
    /// 相对工作区根的路径。
    pub path: String,
    /// 文件内序号（0 起）。
    pub index: u32,
    /// 期望的旧文件起始行。
    pub old_start: u32,
    /// 期望的旧文件行数。
    pub old_lines: u32,
    /// 期望的新文件起始行。
    pub new_start: u32,
    /// 期望的新文件行数。
    pub new_lines: u32,
    /// 期望的 `@@` 行尾部上下文；`None` 表示不校验（老客户端）。
    pub header: Option<String>,
}

impl HunkSelector {
    /// 期望位置的可读描述，用于 `expected`。
    pub fn expected(&self) -> String {
        format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_lines, self.new_start, self.new_lines
        )
    }

    /// 与一块实际 hunk 是否精确对齐。
    pub fn matches(&self, hunk: &GitHunk) -> bool {
        self.index == hunk.index
            && self.old_start == hunk.old_start
            && self.old_lines == hunk.old_lines
            && self.new_start == hunk.new_start
            && self.new_lines == hunk.new_lines
            && match self.header.as_deref() {
                // 空串等于「调用方没给」，不参与校验（与 DTO 的 Option 语义一致）。
                Some(header) if !header.trim().is_empty() => {
                    header.trim_end() == hunk.header.trim_end()
                }
                _ => true,
            }
    }
}

/// 一次 `stage_hunk` / `unstage_hunk` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHunkOutcome {
    /// 实际被暂存 / 取消暂存的那一块（回传给外壳做「确实只动了这块」的渲染）。
    pub hunk: GitHunk,
    /// 是否是取消暂存。
    pub unstage: bool,
    /// 操作后暂存区文件数。
    pub staged_total: u64,
}

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
