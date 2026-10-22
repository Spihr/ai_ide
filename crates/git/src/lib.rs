//! # ai-ide-git
//!
//! AI IDE 内核的工作区 Git 能力（M6 切片三）：`git.status` / `git.diff` /
//! `git.stage` / `git.commit` / `git.branches` 五个协议方法的数据来源。
//!
//! ## 为什么驱动系统 `git` 而不是链接 `git2`
//!
//! `git2`（libgit2）是纯库、无外部依赖，但它在三件事上会与用户看到的结果打架：
//!
//! 1. **行为一致性**：`status` 的忽略规则（`.gitignore` 的取反、嵌套仓库、
//!    `core.excludesFile`）由 git 自己实现，libgit2 只是近似复刻；用户觉得
//!    「命令行看不到这个文件，IDE 却列出来了」就是这类偏差。
//! 2. **配置与 hooks**：`commit` 要跑用户的 `pre-commit` / `commit-msg` 钩子，
//!    并尊重 `commit.gpgsign`、`core.autocrlf`、`includeIf` 等配置。libgit2 需要
//!    逐项手工对齐。
//! 3. **体积**：libgit2 + OpenSSL 静态链进内核会让二进制显著变胖，而安装 AI IDE
//!    的机器上几乎必然已装 git（用户本来就在用它）。
//!
//! 代价是要处理「git 不存在 / 版本过旧 / 输出编码」这些进程层问题。本 crate 把它们
//! 收敛在 [`exec`] 一层：统一 `LC_ALL=C`（错误信息稳定为英文，便于识别
//! 「nothing to commit」这类可映射错误）、`GIT_TERMINAL_PROMPT=0`（绝不在后台
//! 弹凭据输入）、`GIT_OPTIONAL_LOCKS=0`（状态轮询不抢 `index.lock`）、
//! `core.quotepath=false`（中文路径不被转义）。可用 `AI_IDE_GIT` 环境变量指定
//! git 可执行文件。
//!
//! ## 作用域
//!
//! 所有命令都在 `git -C <工作区根>` 下执行，并带 `--relative`：即使工作区只是某个
//! 大仓库的子目录，`status` / `diff` 也只报告该子树内的变更，路径一律相对工作区根、
//! 正斜杠分隔。协议层收到的 `path` 会先归一化（`exec::normalize_path`），绝对路径与
//! `..` 越界一律拒绝（`7004`），避免借 `git` 的 `-C` 跳出工作区。
//!
//! ## 分层
//!
//! - [`GitRepo`]：单个仓库句柄，纯同步，一个方法对应一条 git 命令；
//! - [`GitService`]：按工作区根缓存 [`GitRepo`]，避免每次 `git.status` 轮询都重跑
//!   `rev-parse`；
//! - 本 crate 不认识协议 DTO：`crates/core/src/git_api.rs` 负责把这里的领域结构
//!   翻成 `crates/protocol` 的 `Git*` 结构并发错误码。

mod exec;
mod parse;

pub mod error;
pub mod model;
pub mod repo;

pub use error::GitError;
pub use model::{
    EntryKind, GitBranchInfo, GitBranches, GitCommitOutcome, GitDiff, GitStageOutcome, GitStatus,
    GitStatusEntry,
};
pub use repo::{GitRepo, GitService};

/// diff 文本的字节上限。超过就截断（只切尾部），避免一次全量 diff 把协议帧撑爆。
pub const DIFF_LIMIT_BYTES: usize = 1024 * 1024;

/// `git.diff` 的上下文行数上限。
pub const MAX_CONTEXT_LINES: u32 = 100;
