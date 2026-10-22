//! 工作区 Git 的协议处理器（M6 切片三）。
//!
//! git 能力本体在 `crates/git`（[`ai_ide_git::GitRepo`] / [`ai_ide_git::GitService`]），
//! 这里只做三件事：
//!
//! 1. 把 `git.*` 的 DTO 参数翻成仓库方法调用，把领域结构翻回 `Git*Result`；
//! 2. 把 [`GitError`] 翻成带域错误码的 [`ErrorObject`]（7001~7005）；
//! 3. 把同步的 git 子进程调用搬进阻塞线程池。
//!
//! ## 为什么必须 `spawn_blocking`
//!
//! `GitRepo` 的每个方法都是一次同步子进程调用：`git status` 在大仓库上要几百毫秒，
//! `git commit` 还要跑 `pre-commit` 钩子——钩子里跑一遍 lint + 测试是常态。这类
//! 耗时直接写在 async 函数里会占住 tokio worker，把并发的 `workspace.read_file`、
//! `llm.chat_stream` 一起卡住。与 `ws.rs` 的磁盘 IO 同一条纪律，不给侥幸。
//!
//! ## 为什么没有 `git.*` 事件
//!
//! 索引变化已经由 `workspace.changed` 推给外壳了，Git 状态是该变化的一个视图。
//! 再加一条 `git.changed` 只会让外壳收到两份事实、并在两次事件之间做无谓的去抖。
//! 外壳据 `workspace.changed` 重新发 `git.status` 即可。
//!
//! ## 工作区解析
//!
//! 五个方法都带可选 `workspace_id`，缺省作用于活动工作区——与 `read_file` 一致。
//! 工作区没打开时报 3001，不是 7002：「不是仓库」与「没有工作区」是两回事。
//!
//! ## 错误码
//!
//! | 场景 | 码 |
//! |---|---|
//! | 找不到 git / 不可执行 | 7001 |
//! | 工作区不是 Git 仓库 | 7002 |
//! | git 非零退出（`data.stderr` 带原文） | 7003 |
//! | 参数非法（路径越界、空提交信息等） | 7004 |
//! | 暂存区没有可提交的内容 | 7005 |

use crate::dispatch::parse_params;
use crate::peer::Peer;
use crate::state::AppState;
use ai_ide_git::{GitBranchInfo, GitError, GitRepo, GitStatus, GitStatusEntry};
use ai_ide_protocol::dto::{
    GitBranch, GitBranchesParams, GitBranchesResult, GitCommitParams, GitCommitResult,
    GitDiffParams, GitDiffResult, GitStageParams, GitStageResult, GitStatusParams, GitStatusResult,
};
use ai_ide_protocol::{ErrorCode, ErrorObject, Request};
use std::sync::Arc;
use tokio::task;

// ---------------------------------------------------------------------------
// 错误适配
// ---------------------------------------------------------------------------

/// 把 git 层错误折算成协议错误对象。
///
/// `data` 不是装饰：`7003` 带 `stderr`（用户要看的 fatal 原文），`7004` 与 `7002`
/// 带路径与参数，外壳据此决定是「弹提示」还是「跳到出错的文件」。
fn error_of(err: &GitError) -> ErrorObject {
    match err {
        GitError::NotAvailable(detail) => {
            ErrorObject::new(ErrorCode::GIT_UNAVAILABLE, err.to_string())
                .with_data(serde_json::json!({ "detail": detail }))
        }
        GitError::NotARepository(path) => {
            ErrorObject::new(ErrorCode::GIT_NOT_A_REPOSITORY, err.to_string())
                .with_data(serde_json::json!({ "path": path }))
        }
        GitError::Invalid(detail) => {
            ErrorObject::new(ErrorCode::GIT_INVALID_PARAMS, err.to_string())
                .with_data(serde_json::json!({ "detail": detail }))
        }
        GitError::NothingToCommit => {
            ErrorObject::new(ErrorCode::GIT_NOTHING_TO_COMMIT, err.to_string())
        }
        GitError::Command { args, stderr, code } => {
            ErrorObject::new(ErrorCode::GIT_COMMAND_FAILED, err.to_string())
                .with_data(serde_json::json!({ "args": args, "stderr": stderr, "exit_code": code }))
        }
        // 进程层 IO（写 stdin / 读输出失败）说明内核与 git 之间的管道坏了：
        // 这不是「git 命令失败」，标成内部错误，避免外壳把它当成可重试的仓库问题。
        GitError::Io(message) => ErrorObject::internal(format!("git 进程通信失败：{message}")),
    }
}

/// 在阻塞线程池里跑一段仓库操作，返回 `(workspace_id, 结果)`。
///
/// 仓库句柄按工作区根缓存（`GitService`），缓存未命中时的 `rev-parse` 也一并落在
/// 阻塞线程里——那同样是一次子进程调用。
async fn run<T, F>(
    state: &AppState,
    workspace_id: Option<&str>,
    operation: F,
) -> Result<(String, T), ErrorObject>
where
    F: FnOnce(Arc<GitRepo>) -> Result<T, GitError> + Send + 'static,
    T: Send + 'static,
{
    let workspace = state
        .workspace(workspace_id)
        .map_err(|err| ErrorObject::new(err.code(), err.to_string()))?;
    let service = state.git();
    let root = workspace.root.clone();
    let workspace_id = workspace.id.clone();

    let value = task::spawn_blocking(move || {
        let repo = service.repo_for(&root)?;
        operation(repo)
    })
    .await
    .map_err(|err| ErrorObject::internal(format!("后台任务失败: {err}")))?
    .map_err(|err| error_of(&err))?;

    Ok((workspace_id, value))
}

// ---------------------------------------------------------------------------
// 领域结构 → 协议 DTO
// ---------------------------------------------------------------------------

fn entry_of(entry: GitStatusEntry) -> ai_ide_protocol::dto::GitStatusEntry {
    ai_ide_protocol::dto::GitStatusEntry {
        path: entry.path,
        original_path: entry.original_path,
        index: entry.index,
        worktree: entry.worktree,
        kind: entry.kind.as_str().to_string(),
    }
}

fn status_of(workspace_id: String, status: GitStatus) -> GitStatusResult {
    let clean = status.is_clean();
    GitStatusResult {
        workspace_id,
        branch: status.branch,
        detached: status.detached,
        head_oid: status.head_oid,
        upstream: status.upstream,
        ahead: status.ahead,
        behind: status.behind,
        clean,
        entries: status.entries.into_iter().map(entry_of).collect(),
    }
}

fn branch_of(branch: GitBranchInfo) -> GitBranch {
    GitBranch {
        name: branch.name,
        oid: branch.oid,
        short_oid: branch.short_oid,
        is_current: branch.is_current,
        is_remote: branch.is_remote,
        upstream: branch.upstream,
    }
}

// ---------------------------------------------------------------------------
// git.status
// ---------------------------------------------------------------------------

pub async fn status(peer: &Peer, state: &AppState, req: &Request) {
    let params: GitStatusParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    match run(state, params.workspace_id.as_deref(), |repo| repo.status()).await {
        Ok((workspace_id, status)) => {
            peer.respond(&req.id, &status_of(workspace_id, status))
                .await
        }
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

// ---------------------------------------------------------------------------
// git.diff
// ---------------------------------------------------------------------------

pub async fn diff(peer: &Peer, state: &AppState, req: &Request) {
    let params: GitDiffParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let path = params.path.clone();
    let staged = params.staged;
    let context_lines = params.context_lines;

    let outcome = run(state, params.workspace_id.as_deref(), move |repo| {
        repo.diff(path.as_deref(), staged, context_lines)
    })
    .await;

    match outcome {
        Ok((_, diff)) => {
            let result = GitDiffResult {
                path: diff.path,
                staged: diff.staged,
                diff: diff.diff,
                bytes: diff.bytes,
                truncated: diff.truncated,
            };
            peer.respond(&req.id, &result).await
        }
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

// ---------------------------------------------------------------------------
// git.stage
// ---------------------------------------------------------------------------

pub async fn stage(peer: &Peer, state: &AppState, req: &Request) {
    let params: GitStageParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let paths = params.paths.clone();
    let all = params.all;
    let unstage = params.unstage;

    let outcome = run(state, params.workspace_id.as_deref(), move |repo| {
        repo.stage(&paths, all, unstage)
    })
    .await;

    match outcome {
        Ok((_, staged)) => {
            let result = GitStageResult {
                paths: staged.paths,
                all: staged.all,
                unstage: staged.unstage,
                staged_total: staged.staged_total,
            };
            peer.respond(&req.id, &result).await
        }
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

// ---------------------------------------------------------------------------
// git.commit
// ---------------------------------------------------------------------------

pub async fn commit(peer: &Peer, state: &AppState, req: &Request) {
    let params: GitCommitParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let message = params.message.clone();
    let paths = params.paths.clone();
    let amend = params.amend;
    let allow_empty = params.allow_empty;

    let outcome = run(state, params.workspace_id.as_deref(), move |repo| {
        repo.commit(&message, paths.as_deref(), amend, allow_empty)
    })
    .await;

    match outcome {
        Ok((_, commit)) => {
            let result = GitCommitResult {
                oid: commit.oid,
                short_oid: commit.short_oid,
                summary: commit.summary,
                branch: commit.branch,
                files: commit.files,
                author: commit.author,
            };
            peer.respond(&req.id, &result).await
        }
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

// ---------------------------------------------------------------------------
// git.branches
// ---------------------------------------------------------------------------

pub async fn branches(peer: &Peer, state: &AppState, req: &Request) {
    let params: GitBranchesParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    match run(state, params.workspace_id.as_deref(), |repo| {
        repo.branches()
    })
    .await
    {
        Ok((workspace_id, branches)) => {
            let result = GitBranchesResult {
                workspace_id,
                current: branches.current,
                detached: branches.detached,
                branches: branches.branches.into_iter().map(branch_of).collect(),
            };
            peer.respond(&req.id, &result).await
        }
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_errors_map_to_dedicated_codes() {
        let unavailable = error_of(&GitError::NotAvailable("git".into()));
        assert_eq!(unavailable.code, ErrorCode::GIT_UNAVAILABLE);

        let not_repo = error_of(&GitError::not_a_repository(std::path::Path::new("E:\\tmp")));
        assert_eq!(not_repo.code, ErrorCode::GIT_NOT_A_REPOSITORY);
        assert_eq!(not_repo.data.as_ref().unwrap()["path"], "E:\\tmp");
    }

    #[test]
    fn command_error_carries_stderr_and_exit_code() {
        let err = error_of(&GitError::Command {
            args: "commit -F -".into(),
            stderr: "fatal: unable to write".into(),
            code: Some(128),
        });
        assert_eq!(err.code, ErrorCode::GIT_COMMAND_FAILED);
        let data = err.data.as_ref().expect("7003 必须带 data");
        assert_eq!(data["stderr"], "fatal: unable to write");
        assert_eq!(data["exit_code"], 128);
    }

    #[test]
    fn parameter_and_index_errors_map_separately() {
        assert_eq!(
            error_of(&GitError::Invalid("../x".into())).code,
            ErrorCode::GIT_INVALID_PARAMS
        );
        assert_eq!(
            error_of(&GitError::NothingToCommit).code,
            ErrorCode::GIT_NOTHING_TO_COMMIT
        );
        // 进程管道问题不是「git 命令失败」，不能混进 7003：外壳会据此提示用户
        // 去检查仓库，而实际该重试的是内核。
        assert_eq!(
            error_of(&GitError::Io("broken pipe".into())).code,
            ErrorCode::INTERNAL_ERROR
        );
    }

    #[test]
    fn status_conversion_keeps_kind_and_cleanness() {
        let status = GitStatus {
            branch: Some("main".into()),
            detached: false,
            head_oid: Some("a".repeat(40)),
            upstream: None,
            ahead: 0,
            behind: 0,
            entries: vec![GitStatusEntry {
                path: "src/main.rs".into(),
                original_path: None,
                index: ".".into(),
                worktree: "M".into(),
                kind: ai_ide_git::EntryKind::Changed,
            }],
        };
        let result = status_of("ws-1".into(), status);
        assert_eq!(result.workspace_id, "ws-1");
        assert!(!result.clean);
        assert_eq!(result.entries[0].kind, "changed");
        assert_eq!(result.entries[0].worktree, "M");
    }
}
