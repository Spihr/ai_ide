//! `workspace.*` / `search.*` 处理器。
//!
//! ## 三条贯穿始终的约定
//!
//! 1. **磁盘 IO 一律走 `spawn_blocking`**。内核是 async 的，一次同步读大文件就能
//!    把 worker 线程堵住，进而卡住所有并发请求；这里不给自己留侥幸。
//! 2. **错误码由下层决定**。`WorkspaceError` / `IndexError` 各自实现了码映射，
//!    这层只做转发，避免两处各写一套 `match` 后逐渐漂移。
//! 3. **不带 `workspace_id` 的方法作用于活动工作区**。`read_file` / `write_file`
//!    是编辑时的高频调用，每次都要前端回传句柄是没必要的负担；越界风险仍由
//!    `PathGuard` 在解析路径时拦下。

use crate::dispatch::parse_params;
use crate::peer::Peer;
use crate::state::AppState;
use ai_ide_indexer::{grep, symbols, FileIndex, IndexError};
use ai_ide_protocol::dto::{
    OkResult, SearchContentParams, SearchFilesParams, SearchFilesResult, SearchSymbolsParams,
    SearchSymbolsResult, WorkspaceChangedEvent, WorkspaceCloseParams, WorkspaceCreateParams,
    WorkspaceDeleteParams, WorkspaceIndexedEvent, WorkspaceOpenParams, WorkspaceOpenResult,
    WorkspaceReadFileParams, WorkspaceRenameParams, WorkspaceRevealParams, WorkspaceStats,
    WorkspaceTreeParams, WorkspaceTreeResult, WorkspaceWriteFileParams,
};
use ai_ide_protocol::{methods, ErrorObject, PathChange, Request};
use ai_ide_workspace::{fs_ops, Workspace, WorkspaceError};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task;
use tokio::time::timeout;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// 错误适配
// ---------------------------------------------------------------------------

/// 下层的错误类型统一折算成协议错误对象。
trait IntoProtocolError {
    fn to_error(&self) -> ErrorObject;
}

impl IntoProtocolError for WorkspaceError {
    fn to_error(&self) -> ErrorObject {
        ErrorObject::new(self.code(), self.to_string())
    }
}

impl IntoProtocolError for IndexError {
    fn to_error(&self) -> ErrorObject {
        ErrorObject::new(self.code(), self.to_string())
    }
}

fn ws_error(err: &WorkspaceError) -> ErrorObject {
    err.to_error()
}

fn join_error(err: task::JoinError) -> ErrorObject {
    ErrorObject::internal(format!("后台任务失败: {err}"))
}

/// 在阻塞线程池里执行一段磁盘 / CPU 活，并把错误折算成协议错误。
async fn blocking<T, E, F>(f: F) -> Result<T, ErrorObject>
where
    F: FnOnce() -> Result<T, E> + Send + 'static,
    E: IntoProtocolError + Send + 'static,
    T: Send + 'static,
{
    match task::spawn_blocking(f).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(err.to_error()),
        Err(err) => Err(join_error(err)),
    }
}

// ---------------------------------------------------------------------------
// workspace.open / close
// ---------------------------------------------------------------------------

pub async fn open(peer: &Peer, state: &AppState, req: &Request) {
    let params: WorkspaceOpenParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let workspace = match state.workspaces().open(&params.path) {
        Ok(workspace) => workspace,
        Err(err) => return peer.respond_error(&req.id, &ws_error(&err)).await,
    };

    // 索引不在请求路径上同步建：首次打开先回一份「索引中」快照（空统计 + indexing），
    // 遍历交给后台任务，就绪后由 `workspace.indexed` 推送真实规模。大库首次打开
    // 因此不再被整库遍历阻塞。缓存命中则照旧直接复用。
    let (stats, indexing) = match state.index(&workspace.id) {
        Some(index) => (index.stats(), false),
        None => {
            spawn_index_build(state, &workspace);
            (WorkspaceStats::default(), true)
        }
    };

    state.set_active(&workspace.id);

    // 插件的读侧宿主能力跟着活动工作区走：`host/fs.read` 的相对路径都是相对它解析的。
    // 多工作区下后开的那个成为默认靶子，与 `workspace.tree` 的「不带 id 就取活动区」一致。
    state.plugin_host().set_root(Some(workspace.root.clone()));

    // 写侧（P4 切片三）：先把裁决方接到 agent 审批通道，再把本工作区已批过的
    // 「总是允许」灌回宿主。顺序不能反——先灌集合再装裁决方，中间那一刻的写请求
    // 会拿到 3001（没人能批）；先装后灌只会多问一次，不会误拒。
    crate::plugin_approver::install(state);
    let seeded = crate::plugin_approver::seed_write_always(state, Some(&workspace.root)).await;
    if seeded > 0 {
        tracing::debug!(seeded, root = %workspace.root.display(), "已灌入写侧免问规则");
    }

    if params.watch {
        start_watch(state, &workspace);
    }

    info!(
        workspace_id = %workspace.id,
        root = %workspace.root.display(),
        files = stats.files,
        "工作区已打开"
    );

    let result = WorkspaceOpenResult {
        workspace_id: workspace.id.clone(),
        root: workspace.root.display().to_string(),
        name: workspace.name.clone(),
        stats,
        // 首次打开时索引在后台建，这里为真；缓存命中时为假。
        indexing,
    };
    peer.respond(&req.id, &result).await;
}

pub async fn close(peer: &Peer, state: &AppState, req: &Request) {
    let params: WorkspaceCloseParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    // 关之前先记下根：Git 缓存按根路径存，关完就拿不到 `Workspace` 了。
    let root = state
        .workspaces()
        .get(&params.workspace_id)
        .ok()
        .map(|workspace| workspace.root.clone());
    if let Err(err) = state.workspaces().close(&params.workspace_id) {
        return peer.respond_error(&req.id, &ws_error(&err)).await;
    }
    // 索引是「工作区的派生物」，工作区没了就没有留存意义——留着只会占内存。
    state.drop_index(&params.workspace_id);
    // Git 缓存同理：同一路径关掉后可能被 `git init` 成另一个仓库，
    // 留着旧句柄会让 `git.status` 回报上一个仓库的状态。
    if let Some(root) = root {
        state.git().forget(&root);
    }
    state.clear_active(&params.workspace_id);

    // 关掉活动工作区后，插件不能继续读它：要么换到还开着的那个，要么彻底关闸。
    // 这里不保留旧根——「已关闭的工作区仍然可读」是最难解释的一种泄漏。
    let next_root = state
        .active_id()
        .and_then(|id| state.workspaces().get(&id).ok())
        .map(|workspace| workspace.root.clone());
    state.plugin_host().set_root(next_root.clone());

    // 写侧免问范围跟着活动工作区换：留着上一个区的集合，等于「关掉的工作区
    // 仍然免问写盘」。灌 0 条就是清空。
    crate::plugin_approver::seed_write_always(state, next_root.as_deref()).await;

    info!(workspace_id = %params.workspace_id, "工作区已关闭");
    peer.respond(&req.id, &OkResult { ok: true }).await;
}

/// 启动文件监听，并把去抖后的批次推给主进程。
///
/// 监听线程是 std 线程（`notify` 的要求），事件通过 tokio channel 过桥到 async 侧。
fn start_watch(state: &AppState, workspace: &Arc<Workspace>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<PathChange>>();
    if let Err(err) = workspace.start_watch(tx) {
        warn!(workspace_id = %workspace.id, error = %err, "启动文件监听失败");
        return;
    }

    let workspace_id = workspace.id.clone();
    let peer = state.peer();
    let plugins = state.plugins().clone();
    tokio::spawn(async move {
        while let Some(mut changes) = rx.recv().await {
            // 合并窗口：一次保存常常连着触发好几条路径变化（临时文件写入、
            // rename、目录 mtime），逐批投递会让插件被同一件事唤醒多次。
            // 这里在源头上再收一次，与 `EventQueue` 的同类型事件合并一起兜高频。
            // 退出条件有两个：`Ok(None)` = 通道关了（工作区已关闭），这批发完就退出；
            // `Err(_)` = 窗口内没有新批次，合并结束。
            while let Ok(Some(more)) = timeout(WORKSPACE_CHANGE_MERGE_WINDOW, rx.recv()).await {
                changes.extend(more);
            }

            // 插件侧载荷只有路径：`workspace.changed` 的语义是「这些文件可能变了」，
            // 正文由插件自己按需通过 `host/fs.read` 去取，宿主不主动推送内容。
            plugins.publish(
                ai_ide_plugin::events::WORKSPACE_CHANGED,
                ai_ide_plugin::events::workspace_changed_payload(
                    changes.iter().map(|change| change.path.as_str()),
                ),
            );

            let Some(peer) = peer.as_ref() else {
                // 单测里没有对端句柄，事件无处可去，直接丢弃。
                continue;
            };
            peer.notify(
                methods::event::WORKSPACE_CHANGED,
                &WorkspaceChangedEvent {
                    workspace_id: workspace_id.clone(),
                    changes,
                },
            )
            .await;
        }
    });
}

/// `workspace.changed` 投给插件前的合并窗口。
const WORKSPACE_CHANGE_MERGE_WINDOW: Duration = Duration::from_millis(120);

// ---------------------------------------------------------------------------
// workspace.tree
// ---------------------------------------------------------------------------

pub async fn tree(peer: &Peer, state: &AppState, req: &Request) {
    let params: WorkspaceTreeParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let workspace = match state.workspace(Some(&params.workspace_id)) {
        Ok(workspace) => workspace,
        Err(err) => return peer.respond_error(&req.id, &ws_error(&err)).await,
    };

    let path = params.path.clone();
    let depth = params.depth.unwrap_or(1);
    let include_ignored = params.include_ignored;

    match blocking(move || {
        ai_ide_workspace::tree::list(&workspace.guard, path.as_deref(), depth, include_ignored)
    })
    .await
    {
        Ok(entries) => {
            peer.respond(&req.id, &WorkspaceTreeResult { entries })
                .await
        }
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

// ---------------------------------------------------------------------------
// workspace.read_file / write_file
// ---------------------------------------------------------------------------

pub async fn read_file(peer: &Peer, state: &AppState, req: &Request) {
    let params: WorkspaceReadFileParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let workspace = match state.workspace(None) {
        Ok(workspace) => workspace,
        Err(err) => return peer.respond_error(&req.id, &ws_error(&err)).await,
    };

    let path = params.path.clone();
    let encoding = params.encoding.clone();
    let max_bytes = params.max_bytes;

    match blocking(move || {
        fs_ops::read_file(&workspace.guard, &path, encoding.as_deref(), max_bytes)
    })
    .await
    {
        Ok(result) => peer.respond(&req.id, &result).await,
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

pub async fn write_file(peer: &Peer, state: &AppState, req: &Request) {
    let params: WorkspaceWriteFileParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let workspace = match state.workspace(None) {
        Ok(workspace) => workspace,
        Err(err) => return peer.respond_error(&req.id, &ws_error(&err)).await,
    };

    let path = params.path.clone();
    let saved = path.clone();
    let content = params.content.clone();
    let expected_hash = params.expected_hash.clone();
    let eol = params.eol;

    match blocking(move || {
        fs_ops::write_file(
            &workspace.guard,
            &path,
            &content,
            expected_hash.as_deref(),
            eol,
        )
    })
    .await
    {
        Ok(result) => {
            // 写盘成功才算「保存」：失败的分支连内容都没落下去，投事件只会让插件
            // 去读一份旧文件。载荷只有路径，`file.saved` 与 `workspace.changed` 一样
            // 不带正文。
            state.plugins().publish(
                ai_ide_plugin::events::FILE_SAVED,
                ai_ide_plugin::events::file_saved_payload(&saved),
            );
            peer.respond(&req.id, &result).await
        }
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

// ---------------------------------------------------------------------------
// workspace.create / rename / delete / reveal
// ---------------------------------------------------------------------------

pub async fn create(peer: &Peer, state: &AppState, req: &Request) {
    let params: WorkspaceCreateParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let workspace = match state.workspace(None) {
        Ok(workspace) => workspace,
        Err(err) => return peer.respond_error(&req.id, &ws_error(&err)).await,
    };

    let path = params.path.clone();
    let kind = params.kind;

    match blocking(move || fs_ops::create(&workspace.guard, &path, kind)).await {
        Ok(()) => peer.respond(&req.id, &OkResult { ok: true }).await,
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

pub async fn rename(peer: &Peer, state: &AppState, req: &Request) {
    let params: WorkspaceRenameParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let workspace = match state.workspace(None) {
        Ok(workspace) => workspace,
        Err(err) => return peer.respond_error(&req.id, &ws_error(&err)).await,
    };

    let from = params.from.clone();
    let to = params.to.clone();
    let overwrite = params.overwrite;

    match blocking(move || fs_ops::rename(&workspace.guard, &from, &to, overwrite)).await {
        Ok(()) => peer.respond(&req.id, &OkResult { ok: true }).await,
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

pub async fn delete(peer: &Peer, state: &AppState, req: &Request) {
    let params: WorkspaceDeleteParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let workspace = match state.workspace(None) {
        Ok(workspace) => workspace,
        Err(err) => return peer.respond_error(&req.id, &ws_error(&err)).await,
    };

    let paths = params.paths.clone();
    let to_trash = params.to_trash;

    match blocking(move || fs_ops::delete(&workspace.guard, &paths, to_trash)).await {
        Ok(result) => peer.respond(&req.id, &result).await,
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

pub async fn reveal(peer: &Peer, state: &AppState, req: &Request) {
    let params: WorkspaceRevealParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let workspace = match state.workspace(None) {
        Ok(workspace) => workspace,
        Err(err) => return peer.respond_error(&req.id, &ws_error(&err)).await,
    };

    let path = params.path.clone();
    match blocking(move || {
        let abs = workspace.guard.resolve(&path)?;
        reveal_in_explorer(&abs)
    })
    .await
    {
        Ok(()) => peer.respond(&req.id, &OkResult { ok: true }).await,
        Err(err) => peer.respond_error(&req.id, &err).await,
    }
}

/// 在系统文件管理器里定位到该路径（只选中，不打开文件）。
fn reveal_in_explorer(abs: &Path) -> Result<(), WorkspaceError> {
    #[cfg(windows)]
    {
        // `explorer.exe` 无论成功与否都可能返回非零退出码，所以只关心能否拉起进程。
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", abs.display()))
            .spawn()
            .map_err(|err| WorkspaceError::io(abs, err))?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        Err(WorkspaceError::Other(format!(
            "暂不支持在当前平台定位文件：{}",
            abs.display()
        )))
    }
}

// ---------------------------------------------------------------------------
// search.files / search.content
// ---------------------------------------------------------------------------

/// 起一次后台索引构建（同一工作区同一时刻最多一个）。
///
/// 构建放阻塞池（遍历 + `ignore` 规则是纯磁盘活），完成后写回缓存并向对端推
/// `workspace.indexed`。工作区在途中被关掉时结果会被 [`IndexRegistry::store_if_current`]
/// 判为作废：既不留缓存，也不推事件——「关掉的工作区还在推送」是最难解释的一种泄漏。
fn spawn_index_build(state: &AppState, workspace: &Arc<Workspace>) {
    let registry = state.indices_handle();
    let Some(build) = registry.begin_build(&workspace.id) else {
        // 已经在建：重复 open、或打开后立刻搜索，都会走到这里。
        return;
    };
    let root = workspace.root.clone();
    let workspace_id = workspace.id.clone();
    let root_label = root.display().to_string();
    let peer = state.peer();
    tokio::spawn(async move {
        let started = Instant::now();
        let built = task::spawn_blocking(move || FileIndex::build(&root)).await;
        match built {
            Ok(Ok(index)) => {
                let duration_ms = started.elapsed().as_millis() as u64;
                let stats = index.stats();
                if !registry.store_if_current(&build, Arc::new(index)) {
                    debug!(
                        workspace_id = %workspace_id,
                        "索引构建完成但工作区已关闭，结果丢弃"
                    );
                    return;
                }
                info!(
                    workspace_id = %workspace_id,
                    root = %root_label,
                    files = stats.files,
                    dirs = stats.dirs,
                    duration_ms,
                    "后台索引已就绪"
                );
                if let Some(peer) = peer {
                    // 全量重建：`incremental` 恒为假，字段先占位（见协议 DTO 注释）。
                    peer.notify(
                        methods::event::WORKSPACE_INDEXED,
                        &WorkspaceIndexedEvent {
                            workspace_id: workspace_id.clone(),
                            stats,
                            duration_ms,
                            incremental: false,
                        },
                    )
                    .await;
                }
            }
            Ok(Err(err)) => {
                registry.abandon(&build);
                warn!(
                    workspace_id = %workspace_id,
                    root = %root_label,
                    error = %err,
                    "后台索引失败，检索保持降级"
                );
            }
            Err(err) => {
                registry.abandon(&build);
                warn!(
                    workspace_id = %workspace_id,
                    error = %err,
                    "后台索引任务异常退出"
                );
            }
        }
    });
}

/// 取**已就绪**的索引；未就绪则启动后台构建并返回 `Ok(None)`。
///
/// `Err` 只表示工作区不存在（调用方要报 1001）；「还没建好」不是错误，而是降级：
/// 调用方回空结果 + `indexing: true`，让前端提示「索引中」而不是「没有这个文件」。
fn ready_index(
    state: &AppState,
    workspace_id: &str,
) -> Result<Option<Arc<FileIndex>>, WorkspaceError> {
    if let Some(index) = state.index(workspace_id) {
        return Ok(Some(index));
    }
    // 兜底「先搜索后打开」：不是当场建，而是把构建踢到后台，本次请求先降级。
    let workspace = state.workspace(Some(workspace_id))?;
    spawn_index_build(state, &workspace);
    Ok(None)
}

pub async fn search_files(peer: &Peer, state: &AppState, req: &Request) {
    let params: SearchFilesParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let index = match ready_index(state, &params.workspace_id) {
        Ok(Some(index)) => index,
        Ok(None) => {
            // 索引还在后台建：给降级形状（空表 + 就绪标记），请求不为此等待整库遍历。
            let result = SearchFilesResult {
                matches: Vec::new(),
                indexing: true,
            };
            return peer.respond(&req.id, &result).await;
        }
        Err(err) => return peer.respond_error(&req.id, &ws_error(&err)).await,
    };

    // 模糊匹配要在整个索引上跑一遍，放阻塞池，别占 async worker。
    let query = params.clone();
    match task::spawn_blocking(move || index.search(&query)).await {
        Ok(result) => peer.respond(&req.id, &result).await,
        Err(err) => peer.respond_error(&req.id, &join_error(err)).await,
    }
}

pub async fn search_content(peer: &Peer, state: &AppState, req: &Request) {
    let params: SearchContentParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let workspace = match state.workspace(Some(&params.workspace_id)) {
        Ok(workspace) => workspace,
        Err(err) => return peer.respond_error(&req.id, &ws_error(&err)).await,
    };

    let root = workspace.root.clone();
    let query = params.clone();

    match task::spawn_blocking(move || grep::search(&root, &query)).await {
        Ok(Ok(result)) => peer.respond(&req.id, &result).await,
        Ok(Err(err)) => peer.respond_error(&req.id, &err.to_error()).await,
        Err(err) => peer.respond_error(&req.id, &join_error(err)).await,
    }
}

// ---------------------------------------------------------------------------
// search.symbols
// ---------------------------------------------------------------------------

/// 符号检索：在索引到的源码文件里按名字找声明。
///
/// 与 [`search_content`] 的分工：内容检索回「哪一行的文本里有这个词」，符号检索回
/// 「这个声明定义在哪、覆盖到哪几行」——前者用于看上下文，后者用于跳转定义。
pub async fn search_symbols(peer: &Peer, state: &AppState, req: &Request) {
    let params: SearchSymbolsParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let index = match ready_index(state, &params.workspace_id) {
        Ok(Some(index)) => index,
        Ok(None) => {
            // 与 `search.files` 同一套降级：索引未就绪时回空表 + 就绪标记。
            let result = SearchSymbolsResult {
                symbols: Vec::new(),
                indexing: true,
            };
            return peer.respond(&req.id, &result).await;
        }
        Err(err) => return peer.respond_error(&req.id, &ws_error(&err)).await,
    };

    // 逐文件读盘 + 正则匹配是纯 CPU / IO 活，放阻塞池，别占 async worker。
    let root = index.root().to_path_buf();
    let query = params.clone();
    match task::spawn_blocking(move || symbols::search(&root, &index, &query)).await {
        Ok(result) => peer.respond(&req.id, &result).await,
        Err(err) => peer.respond_error(&req.id, &join_error(err)).await,
    }
}
