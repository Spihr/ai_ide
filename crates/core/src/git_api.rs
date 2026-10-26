//! 工作区 Git 的协议处理器（M6 切片三 + 缺口第二批 Git 深化）。
//!
//! git 能力本体在 `crates/git`（[`ai_ide_git::GitRepo`] / [`ai_ide_git::GitService`]），
//! 这里只做三件事：
//!
//! 1. 把 `git.*` 的 DTO 参数翻成仓库方法调用，把领域结构翻回 `Git*Result`；
//! 2. 把 [`GitError`] 翻成带域错误码的 [`ErrorObject`]（7001~7010）；
//! 3. 把同步的 git 子进程调用搬进阻塞线程池。
//!
//! ## 为什么必须 `spawn_blocking`
//!
//! `GitRepo` 的每个方法都是一次同步子进程调用：`git status` 在大仓库上要几百毫秒，
//! `git commit` 还要跑 `pre-commit` 钩子——钩子里跑一遍 lint + 测试是常态。这类
//! 耗时直接写在 async 函数里会占住 tokio worker，把并发的 `workspace.read_file`、
//! `llm.chat_stream` 一起卡住。与 `ws.rs` 的磁盘 IO 同一条纪律，不给侥幸。
//! `git fetch` / `push` 更甚：一次半分钟的远端往返，绝不能在 worker 线程上等。
//!
//! ## 为什么没有 `git.*` 事件
//!
//! 索引变化已经由 `workspace.changed` 推给外壳了，Git 状态是该变化的一个视图。
//! 再加一条 `git.changed` 只会让外壳收到两份事实、并在两次事件之间做无谓的去抖。
//! 外壳据 `workspace.changed` 重新发 `git.status` 即可。
//!
//! ## 工作区解析
//!
//! 所有方法都带可选 `workspace_id`，缺省作用于活动工作区——与 `read_file` 一致。
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
//! | 工作区有未提交改动，拒绝切换分支（`data.files`） | 7006 |
//! | 非快进 push / pull（`data.detail`） | 7007 |
//! | 远端认证失败（`data.detail`） | 7008 |
//! | 当前分支没有上游分支（`data.branch`） | 7009 |
//! | stash pop/apply 冲突，stash 未被丢弃（`data.files` + `data.stash_kept`） | 7010 |
//! | 单块暂存定位失效（文件被改、行号漂移；`data.expected` / `data.actual`） | 7011 |
//! | 二进制文件不支持按 hunk 暂存（`data.path`） | 7012 |
//!
//! 7006~7010 是缺口第二批补的**专用**码。它们各自的用户下一步动作都不同（先提交 /
//! 先 stash、先拉取、去登录、去设上游、去解冲突），所以不能退化成一个 7003——外壳
//! 拿 7003 只能提示「git 命令失败了」，用户还得自己读 stderr。
//!
//! 7011 / 7012 是第四批（单 hunk 暂存）补的。`7011` 的动作是「刷新后重选」，
//! `7012` 的动作是「这条路本来就不通，改走整文件暂存」——把两者并进 `7003`，
//! 外壳就只能笼统地说「操作失败」，用户会反复点同一个按钮。

use crate::dispatch::parse_params;
use crate::peer::Peer;
use crate::state::AppState;
use ai_ide_git::{GitBranchInfo, GitError, GitRepo, GitStatus, GitStatusEntry, HunkSelector};
use ai_ide_protocol::dto::{
    GitBranch, GitBranchesParams, GitBranchesResult, GitCheckoutParams, GitCheckoutResult,
    GitCommitParams, GitCommitResult, GitDiffParams, GitDiffResult, GitFetchParams, GitFetchResult,
    GitPullParams, GitPullResult, GitPushParams, GitPushResult, GitStageHunkParams,
    GitStageHunkResult, GitStageParams, GitStageResult, GitStashApplyParams, GitStashApplyResult,
    GitStashDropParams, GitStashDropResult, GitStashListParams, GitStashListResult, GitStashParams,
    GitStashResult, GitStatusParams, GitStatusResult, GitUnstageHunkParams, GitUnstageHunkResult,
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
        // 7006：脏工作区拒绝切换。`files` 是外壳渲染「提交这些 / 先 stash」清单的依据。
        GitError::DirtyWorkspace { files } => {
            ErrorObject::new(ErrorCode::GIT_WORKSPACE_DIRTY, err.to_string())
                .with_data(serde_json::json!({ "files": files }))
        }
        // 7007：非快进。用户的动作是「先拉取再推」，不是「重试」。
        GitError::NotFastForward { detail } => {
            ErrorObject::new(ErrorCode::GIT_NOT_FAST_FORWARD, err.to_string())
                .with_data(serde_json::json!({ "detail": detail }))
        }
        // 7008：认证失败。用户的动作是「配凭据」，重试一万次也一样。
        GitError::AuthFailed { detail } => {
            ErrorObject::new(ErrorCode::GIT_AUTH_FAILED, err.to_string())
                .with_data(serde_json::json!({ "detail": detail }))
        }
        // 7009：没有上游。用户的动作是「设上游」，`branch` 让外壳能直接填进确认框。
        GitError::NoUpstream { branch } => {
            ErrorObject::new(ErrorCode::GIT_NO_UPSTREAM, err.to_string())
                .with_data(serde_json::json!({ "branch": branch }))
        }
        // 7010：stash 冲突。`stash_kept` 恒真——冲突时 git 与我们都不删那条记录，
        // 外壳据此把「取出失败」和「取出了但记录没了」讲成两件不同的事。
        GitError::StashConflict { files } => {
            ErrorObject::new(ErrorCode::GIT_STASH_CONFLICT, err.to_string())
                .with_data(serde_json::json!({ "files": files, "stash_kept": true }))
        }
        // 7011：定位漂移。硬按序号暂存会把用户没选的改动塞进索引，所以这里必须是
        // 硬失败。`expected` / `actual` 让外壳能画出「你要的 @@ -8,6 → 现在的 @@ -12,7」，
        // 用户刷新重选即可，不必猜发生了什么。
        GitError::HunkDrifted {
            path,
            index,
            expected,
            actual,
        } => ErrorObject::new(ErrorCode::GIT_HUNK_DRIFTED, err.to_string()).with_data(
            serde_json::json!({ "path": path, "index": index, "expected": expected, "actual": actual }),
        ),
        // 7012：二进制文件。与 7011 分开——这不是「刷新再试」，而是「压根没有 hunk」。
        GitError::BinaryHunk { path } => {
            ErrorObject::new(ErrorCode::GIT_BINARY_HUNK, err.to_string())
                .with_data(serde_json::json!({ "path": path }))
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

/// 领域侧一条 stash → 协议 DTO。
///
/// 两条同名结构都得手写一遍：`crates/git` 不依赖协议（它要能独立测试），所以
/// 这里就是唯一的翻译点，字段逐个点名而不是 `From` 实现——漏掉新字段时编译器
/// 会在这里报错，而不是悄悄少传一个字段给外壳。
fn stash_entry_of(entry: ai_ide_git::GitStashEntry) -> ai_ide_protocol::dto::GitStashEntry {
    ai_ide_protocol::dto::GitStashEntry {
        index: entry.index,
        oid: entry.oid,
        short_oid: entry.short_oid,
        message: entry.message,
    }
}

fn ref_update_of(update: ai_ide_git::GitRefUpdate) -> ai_ide_protocol::dto::GitRefUpdate {
    ai_ide_protocol::dto::GitRefUpdate {
        ref_name: update.name,
        old_short: update.old_short,
        new_short: update.new_short,
        forced: update.forced,
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
// git.stage_hunk / git.unstage_hunk
// ---------------------------------------------------------------------------

fn hunk_of(hunk: ai_ide_git::GitHunk) -> ai_ide_protocol::dto::GitHunk {
    ai_ide_protocol::dto::GitHunk {
        path: hunk.path,
        index: hunk.index,
        old_start: hunk.old_start,
        old_lines: hunk.old_lines,
        new_start: hunk.new_start,
        new_lines: hunk.new_lines,
        header: hunk.header,
        lines: hunk.lines,
    }
}

/// 按块暂存：只把外壳选中的那一块写进索引。
///
/// `context_lines` 不对外暴露：hunk 的切分口径必须与外壳拿到 diff 时**完全一致**，
/// 否则「用户在 `-U3` 的视图里点的第 2 块」与内核按另一个上下文行数解析出的第 2 块
/// 就是不同的东西。固定用 `git diff` 的默认 3 行，两边对齐才有意义。
pub async fn stage_hunk(peer: &Peer, state: &AppState, req: &Request) {
    let params: GitStageHunkParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let selector = HunkSelector {
        path: params.path.clone(),
        index: params.index,
        old_start: params.old_start,
        old_lines: params.old_lines,
        new_start: params.new_start,
        new_lines: params.new_lines,
        header: None,
    };

    let outcome = run(state, params.workspace_id.as_deref(), move |repo| {
        repo.stage_hunk(&selector, false, None).map(hunk_outcome_of)
    })
    .await;

    match outcome {
        Ok((_, result)) => peer.respond(&req.id, &result).await,
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

/// 按块取消暂存：把该块从索引里反向撤掉，工作区保持不动。
pub async fn unstage_hunk(peer: &Peer, state: &AppState, req: &Request) {
    let params: GitUnstageHunkParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let selector = HunkSelector {
        path: params.path.clone(),
        index: params.index,
        old_start: params.old_start,
        old_lines: params.old_lines,
        new_start: params.new_start,
        new_lines: params.new_lines,
        header: None,
    };

    let outcome = run(state, params.workspace_id.as_deref(), move |repo| {
        repo.stage_hunk(&selector, true, None).map(|outcome| {
            let result = hunk_outcome_of(outcome);
            GitUnstageHunkResult {
                hunk: result.hunk,
                staged_total: result.staged_total,
            }
        })
    })
    .await;

    match outcome {
        Ok((_, result)) => peer.respond(&req.id, &result).await,
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

fn hunk_outcome_of(outcome: ai_ide_git::GitHunkOutcome) -> GitStageHunkResult {
    GitStageHunkResult {
        hunk: hunk_of(outcome.hunk),
        staged_total: outcome.staged_total,
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

// ---------------------------------------------------------------------------
// 缺口第二批：分支切换 / 远端 / stash
// ---------------------------------------------------------------------------

/// `git.checkout`：切换或新建分支。
pub async fn checkout(peer: &Peer, state: &AppState, req: &Request) {
    let params: GitCheckoutParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    // 参数要在进阻塞线程前搬进闭包（`run` 的闭包是 `'static`，`req` 活不到那里）。
    let workspace_id = params.workspace_id;
    let branch = params.branch;
    let create = params.create;
    let start_point = params.start_point;

    // `allow_dirty` **恒为 `false`**：脏工作区一律 7006 退回给用户。
    // 这不是「git 做不到」，而是 git 做到的那一半最难解释——改动会被静默带到新分支。
    let outcome = run(state, workspace_id.as_deref(), move |repo| {
        repo.checkout(&branch, create, start_point.as_deref(), false)
    })
    .await;

    match outcome {
        Ok((workspace_id, outcome)) => {
            let result = GitCheckoutResult {
                workspace_id,
                branch: outcome.branch,
                previous: outcome.previous,
                created: outcome.created,
                // 空仓库里 `checkout -b` 之后还没有提交：领域侧是 `None`，协议侧用空串
                // 表达「尚无提交」，外壳据 `created` 区分「新建分支」与「切换分支」。
                oid: outcome.oid.unwrap_or_default(),
                short_oid: outcome.short_oid.unwrap_or_default(),
            };
            peer.respond(&req.id, &result).await
        }
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

/// `git.fetch`：拉取远端引用，不动工作区。
pub async fn fetch(peer: &Peer, state: &AppState, req: &Request) {
    let params: GitFetchParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let workspace_id = params.workspace_id;
    let remote = params.remote;
    let prune = params.prune;

    let outcome = run(state, workspace_id.as_deref(), move |repo| {
        repo.fetch(remote.as_deref(), prune)
    })
    .await;

    match outcome {
        Ok((workspace_id, outcome)) => {
            // 「没有变化」由内核自己算，不让外壳从空表里猜：空表也可能意味着
            // 「解析失败但 git 退出码是 0」，两者在 UI 上的语气不同。
            let up_to_date = outcome.updated.is_empty();
            let result = GitFetchResult {
                workspace_id,
                remote: outcome.remote,
                updated: outcome.updated.into_iter().map(ref_update_of).collect(),
                up_to_date,
            };
            peer.respond(&req.id, &result).await
        }
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

/// `git.pull`：只做快进（`--ff-only`），非快进报 7007。
pub async fn pull(peer: &Peer, state: &AppState, req: &Request) {
    let params: GitPullParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let workspace_id = params.workspace_id;
    let remote = params.remote;
    let branch = params.branch;

    let outcome = run(state, workspace_id.as_deref(), move |repo| {
        repo.pull(remote.as_deref(), branch.as_deref())
    })
    .await;

    match outcome {
        Ok((workspace_id, outcome)) => {
            let files = outcome.changed_paths.len() as u64;
            let result = GitPullResult {
                workspace_id,
                branch: outcome.branch,
                upstream: outcome.upstream,
                previous_oid: outcome.previous_oid,
                oid: outcome.oid,
                short_oid: outcome.short_oid,
                updated: !outcome.up_to_date,
                commits: outcome.commits,
                files,
                changed_paths: outcome.changed_paths,
            };
            peer.respond(&req.id, &result).await
        }
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

/// `git.push`：推当前分支；强制推送只走 `--force-with-lease`。
pub async fn push(peer: &Peer, state: &AppState, req: &Request) {
    let params: GitPushParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let workspace_id = params.workspace_id;
    let remote = params.remote;
    let branch = params.branch;
    let force = params.force_with_lease;
    let set_upstream = params.set_upstream;

    // `force` 走的是 `--force-with-lease`，不是 `--force`：前者带「我看到的远端仍是
    // 那个位置」的租约校验，别人刚推上去时会被拒；后者会直接覆盖，且无法撤销。
    let outcome = run(state, workspace_id.as_deref(), move |repo| {
        repo.push(remote.as_deref(), branch.as_deref(), force, set_upstream)
    })
    .await;

    match outcome {
        Ok((workspace_id, outcome)) => {
            let result = GitPushResult {
                workspace_id,
                branch: outcome.branch,
                remote: outcome.remote,
                remote_branch: outcome.remote_branch,
                forced: outcome.forced,
                up_to_date: outcome.up_to_date,
                oid: outcome.oid,
                short_oid: outcome.short_oid,
            };
            peer.respond(&req.id, &result).await
        }
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

/// `git.stash`：把当前改动收进 stash。
pub async fn stash(peer: &Peer, state: &AppState, req: &Request) {
    let params: GitStashParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let workspace_id = params.workspace_id;
    let message = params.message;
    let include_untracked = params.include_untracked;

    // 协议层暂不开放「只 stash 指定路径」：那需要外壳先把路径解析成工作区相对路径，
    // 而当前的 UI 入口是「整个工作区」，多一个空切片参数只是给未来留位。
    let outcome = run(state, workspace_id.as_deref(), move |repo| {
        repo.stash_push(message.as_deref(), include_untracked, &[])
    })
    .await;

    match outcome {
        Ok((workspace_id, outcome)) => {
            let stashed = outcome.entry.is_some();
            let result = GitStashResult {
                workspace_id,
                stashed,
                entry: outcome.entry.map(stash_entry_of),
                total: outcome.total,
            };
            peer.respond(&req.id, &result).await
        }
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

/// `git.stash_list`：列 stash（`0` 最新）。
pub async fn stash_list(peer: &Peer, state: &AppState, req: &Request) {
    let params: GitStashListParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let outcome = run(state, params.workspace_id.as_deref(), move |repo| {
        repo.stash_list()
    })
    .await;

    match outcome {
        Ok((workspace_id, entries)) => {
            let result = GitStashListResult {
                workspace_id,
                entries: entries.into_iter().map(stash_entry_of).collect(),
            };
            peer.respond(&req.id, &result).await
        }
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

/// `git.stash_pop` 与 `git.stash_apply` 的公共实现：差别只在成功之后删不删条目。
async fn stash_take(peer: &Peer, state: &AppState, req: &Request, drop_after: bool) {
    let params: GitStashApplyParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let workspace_id = params.workspace_id;
    let index = params.index.unwrap_or(0);

    // 冲突路径不走成功结果：`repo.stash_apply` 会把 git 的冲突 stderr 解析成
    // `GitError::StashConflict`，这里经 `error_of` 变 7010 + `stash_kept: true`。
    let outcome = run(state, workspace_id.as_deref(), move |repo| {
        repo.stash_apply(index, drop_after)
    })
    .await;

    match outcome {
        Ok((workspace_id, outcome)) => {
            let result = GitStashApplyResult {
                workspace_id,
                action: if drop_after { "pop" } else { "apply" }.to_string(),
                index: outcome.index,
                dropped: outcome.dropped,
                total: outcome.total,
            };
            peer.respond(&req.id, &result).await
        }
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

/// `git.stash_pop`：取出并删除该条目。
pub async fn stash_pop(peer: &Peer, state: &AppState, req: &Request) {
    stash_take(peer, state, req, true).await
}

/// `git.stash_apply`：取出但保留该条目。
pub async fn stash_apply(peer: &Peer, state: &AppState, req: &Request) {
    stash_take(peer, state, req, false).await
}

/// `git.stash_drop`：丢弃一条 stash（**不可逆**，二次确认由外壳负责）。
pub async fn stash_drop(peer: &Peer, state: &AppState, req: &Request) {
    let params: GitStashDropParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let workspace_id = params.workspace_id;
    let index = params.index.unwrap_or(0);

    let outcome = run(state, workspace_id.as_deref(), move |repo| {
        repo.stash_drop(index)
    })
    .await;

    match outcome {
        Ok((workspace_id, outcome)) => {
            // 回带 `oid` / `message`：删掉的条目从 `stash_list` 里彻底消失，这是用户
            // 事后唯一能确认「到底删了哪一条」的证据。
            let result = GitStashDropResult {
                workspace_id,
                index: outcome.index,
                oid: outcome.oid,
                message: outcome.message,
                total: outcome.total,
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
