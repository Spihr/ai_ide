//! 检查点与回滚（M5 切片五）。
//!
//! ## 为什么单独一套存储，而不是塞进 SQLite
//!
//! 会话记「说过什么」，检查点记「磁盘上原本是什么」。后者要的是**逐字节还原**，
//! 内容天然是文件；把文件内容塞进 SQLite 只会让同一份字节存两遍，回滚时还多一层
//! 转义。所以快照内容直接落在 `<data_dir>/checkpoints/<工作区键>/<检查点 id>/files/`
//! 下，元数据是同目录的 `meta.json`。
//!
//! ## 校验怎么算数
//!
//! docs/05 要求「回滚后文件与拍快照时逐字节一致」。这里不靠「写完没报错」自我安慰：
//! 每个文件在快照时登记 SHA-256，回滚写回后**重新读盘再算一遍**，逐个比对，结论
//! 折成 `verified` 返回。比对失败的文件单独标 `failed`，不影响其余文件回滚。
//!
//! ## 三条边界
//!
//! 1. 一切路径先过 [`PathGuard`]：快照只覆盖工作区内、且非敏感的文件；
//! 2. 单文件 8 MiB、单检查点 5000 个文件 / 256 MiB，超限跳过并计数（`skipped`），
//!    绝不因为一个超大文件让整次快照失败；
//! 3. 回滚只动**被登记**的文件，做「覆盖 / 删除 / 重建」三种动作；清单外的新文件
//!    一律不碰——「回滚」不该顺手删掉用户自己后来写的东西，目录级清理的能力在
//!    `workspace.delete`（且只进回收站）。也因此 `verified` 的含义是
//!    「本次回滚触及的每个文件都与快照登记值一致」，不是「整个工作区与当时一样」。
//!    `created` 条目只有 Agent 记录器能产出（它在写新文件前先登记「原本不存在」），
//!    全量手动快照看不到「快照之后才出现的文件」，故不会生成这种条目。

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ai_ide_protocol::dto::{
    CheckpointFileKind, CheckpointFileView, CheckpointOrigin, CheckpointRestoreFileView,
    CheckpointRestoreStatus, CheckpointView,
};
use ai_ide_protocol::ErrorCode;
use ai_ide_workspace::fs_ops::hash_bytes;
use ai_ide_workspace::ignore_rules::IgnoreStack;
use ai_ide_workspace::{PathError, PathGuard};
use serde::{Deserialize, Serialize};

/// 单个检查点最多收录的文件数。
pub const MAX_FILES: usize = 5_000;
/// 单个文件的体积上限：再大就不进快照，如实计入 `skipped`。
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// 单个检查点的总容量上限。
pub const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// 存放快照内容的子目录名。
const FILES_DIR: &str = "files";
/// 元数据文件名。
const META_FILE: &str = "meta.json";
/// 元数据格式版本：将来改结构时靠它决定怎么读旧数据。
const META_SCHEMA: u32 = 1;
/// 环境变量：覆盖检查点根目录（测试与 CI 用）。
pub const DIR_ENV: &str = "AI_IDE_CHECKPOINT_DIR";

/// 检查点相关的失败原因。
#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("检查点不存在：{0}")]
    NotFound(String),
    #[error("检查点元数据损坏：{0}")]
    Corrupt(String),
    #[error("没有可快照的文件：{0}")]
    Empty(String),
    #[error(transparent)]
    Path(#[from] PathError),
    #[error("IO 错误（{path}）：{source}")]
    Io { path: String, source: io::Error },
}

impl CheckpointError {
    /// 包装一次 IO 错误并带上路径。
    pub fn io(path: impl AsRef<Path>, source: io::Error) -> Self {
        Self::Io {
            path: path.as_ref().display().to_string(),
            source,
        }
    }

    /// 映射为协议错误码。
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::NotFound(_) => ErrorCode::CHECKPOINT_NOT_FOUND,
            Self::Corrupt(_) => ErrorCode::CHECKPOINT_CORRUPT,
            Self::Empty(_) => ErrorCode::INVALID_PARAMS,
            Self::Path(err) => err.code(),
            Self::Io { .. } => ErrorCode::IO_ERROR,
        }
    }
}

// ---------------------------------------------------------------------------
// meta.json 的形状
// ---------------------------------------------------------------------------

/// 落盘的检查点元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    /// 结构版本，见 [`META_SCHEMA`]。
    pub schema_version: u32,
    pub checkpoint_id: String,
    pub label: String,
    /// 拍快照时工作区的绝对根路径。
    pub workspace_root: String,
    pub origin: CheckpointOrigin,
    #[serde(default)]
    pub session_id: Option<String>,
    /// 建立时刻（Unix 毫秒）。
    pub created_at_ms: i64,
    pub total_bytes: u64,
    pub skipped: u64,
    pub files: Vec<MetaFile>,
}

/// 元数据里的单个文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaFile {
    /// 相对工作区根，`/` 分隔。
    pub path: String,
    pub kind: CheckpointFileKind,
    #[serde(default)]
    pub before_hash: Option<String>,
    pub size: u64,
}

impl SnapshotMeta {
    /// 转成协议视图。
    pub fn to_view(&self) -> CheckpointView {
        CheckpointView {
            checkpoint_id: self.checkpoint_id.clone(),
            label: self.label.clone(),
            workspace_root: self.workspace_root.clone(),
            origin: self.origin,
            session_id: self.session_id.clone(),
            created_at_ms: self.created_at_ms,
            files: self
                .files
                .iter()
                .map(|file| CheckpointFileView {
                    path: file.path.clone(),
                    kind: file.kind,
                    before_hash: file.before_hash.clone(),
                    size: file.size,
                })
                .collect(),
            total_bytes: self.total_bytes,
            skipped: self.skipped,
        }
    }
}

// ---------------------------------------------------------------------------
// 一次创建的入参
// ---------------------------------------------------------------------------

/// 创建检查点的入参。字段多于 7 个的写法容易漏，所以收成结构体。
pub struct SnapshotRequest<'a> {
    /// 工作区绝对根路径。
    pub root: &'a Path,
    /// 标题；缺省按来源生成。
    pub label: Option<&'a str>,
    /// 只拍这些相对路径；`None` 表示按忽略规则遍历整个工作区。
    pub paths: Option<&'a [String]>,
    pub origin: CheckpointOrigin,
    pub session_id: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// 存储
// ---------------------------------------------------------------------------

/// 检查点仓库：一个根目录 + 一堆按工作区分子目录的检查点。
#[derive(Debug, Clone)]
pub struct CheckpointStore {
    base: PathBuf,
}

impl CheckpointStore {
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    /// 按 `AI_IDE_CHECKPOINT_DIR` → `<data_dir>/checkpoints` 的顺序定根目录。
    pub fn from_env() -> Self {
        let base = std::env::var_os(DIR_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::llm_runtime::default_data_dir().join("checkpoints"));
        Self::new(base)
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    /// 工作区子目录名：根路径的 SHA-256 前 16 位。
    ///
    /// 用哈希而不是目录名，是为了让「两个同名工作区」不至于互相看不见，
    /// 也顺便避开 Windows 路径里的非法字符。
    fn workspace_key(root: &Path) -> String {
        hash_bytes(root.to_string_lossy().as_bytes())[..16].to_string()
    }

    fn workspace_dir(&self, root: &Path) -> PathBuf {
        self.base.join(Self::workspace_key(root))
    }

    /// 某个检查点的目录。
    fn dir_for(&self, root: &Path, checkpoint_id: &str) -> PathBuf {
        self.workspace_dir(root).join(checkpoint_id)
    }

    // -----------------------------------------------------------------------
    // 创建
    // -----------------------------------------------------------------------

    /// 拍一个检查点。
    pub fn create(&self, request: &SnapshotRequest<'_>) -> Result<CheckpointView, CheckpointError> {
        let guard = PathGuard::new(request.root.to_path_buf())?;
        let checkpoint_id = uuid::Uuid::new_v4().to_string();
        let dir = self.dir_for(guard.root(), &checkpoint_id);

        let mut skipped = 0u64;
        let (candidates, extra_skipped) = match request.paths {
            Some(paths) if !paths.is_empty() => self.explicit_paths(&guard, paths),
            _ => collect_workspace_files(guard.root(), &guard),
        };
        skipped += extra_skipped;

        let files_dir = dir.join(FILES_DIR);
        let mut files = Vec::new();
        let mut total_bytes = 0u64;

        for abs in candidates {
            let rel = guard.to_relative(&abs);
            let bytes = match fs::read(&abs) {
                Ok(bytes) => Some(bytes),
                Err(err) if err.kind() == io::ErrorKind::NotFound => None,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            let size = bytes.as_ref().map_or(0, |bytes| bytes.len() as u64);
            if size > MAX_FILE_BYTES || total_bytes.saturating_add(size) > MAX_TOTAL_BYTES {
                skipped += 1;
                continue;
            }

            let mut before_hash = None;
            if let Some(bytes) = &bytes {
                before_hash = Some(hash_bytes(bytes));
                let dest = files_dir.join(&rel);
                if let Some(parent) = dest.parent() {
                    if let Err(err) = fs::create_dir_all(parent) {
                        return Err(CheckpointError::io(parent, err));
                    }
                }
                if let Err(err) = fs::write(&dest, bytes) {
                    return Err(CheckpointError::io(&dest, err));
                }
                total_bytes += size;
            }

            files.push(MetaFile {
                path: rel,
                kind: if bytes.is_some() {
                    CheckpointFileKind::Modified
                } else {
                    CheckpointFileKind::Created
                },
                before_hash,
                size,
            });
        }

        if files.is_empty() {
            // 空检查点没有任何回滚能力，却会占着列表——宁可直接报错。
            let _ = fs::remove_dir_all(&dir);
            return Err(CheckpointError::Empty(format!(
                "{} 下没有可快照的文件",
                guard.root().display()
            )));
        }

        let meta = SnapshotMeta {
            schema_version: META_SCHEMA,
            checkpoint_id: checkpoint_id.clone(),
            label: normalize_label(request.label, request.origin, &checkpoint_id),
            workspace_root: guard.root().display().to_string(),
            origin: request.origin,
            session_id: request.session_id.map(str::to_string),
            created_at_ms: crate::state::now_ms(),
            total_bytes,
            skipped,
            files,
        };
        write_meta(&dir, &meta)?;
        Ok(meta.to_view())
    }

    /// 显式指定的路径：越界或敏感的直接跳过，而不是让整次快照失败。
    fn explicit_paths(&self, guard: &PathGuard, paths: &[String]) -> (Vec<PathBuf>, u64) {
        let mut out = Vec::new();
        let mut skipped = 0u64;
        for rel in paths {
            let abs = match guard.resolve(rel) {
                Ok(abs) => abs,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            // 敏感文件能读也不能写回，拍进快照等于给用户一个「点了回滚却失败」的按钮。
            if guard.is_protected(&abs) || abs.is_dir() {
                skipped += 1;
                continue;
            }
            out.push(abs);
        }
        (out, skipped)
    }

    // -----------------------------------------------------------------------
    // 查询
    // -----------------------------------------------------------------------

    /// 列出某工作区的检查点（创建时间倒序）。
    pub fn list(
        &self,
        root: &Path,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<CheckpointView>, u64), CheckpointError> {
        let dir = self.workspace_dir(root);
        let mut metas = read_all_metas(&dir)?;
        metas.sort_by(|a, b| {
            b.created_at_ms
                .cmp(&a.created_at_ms)
                .then_with(|| b.checkpoint_id.cmp(&a.checkpoint_id))
        });
        let total = metas.len() as u64;
        let page = metas
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .map(|meta| meta.to_view())
            .collect();
        Ok((page, total))
    }

    /// 按 id 全局查找：检查点可能属于任何工作区，这里挨个工作区目录看一眼。
    fn find(&self, checkpoint_id: &str) -> Result<(PathBuf, SnapshotMeta), CheckpointError> {
        if !is_safe_id(checkpoint_id) {
            return Err(CheckpointError::NotFound(checkpoint_id.to_string()));
        }
        let entries = match fs::read_dir(&self.base) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(CheckpointError::NotFound(checkpoint_id.to_string()))
            }
            Err(err) => return Err(CheckpointError::io(&self.base, err)),
        };
        for entry in entries.flatten() {
            let candidate = entry.path().join(checkpoint_id);
            if candidate.is_dir() {
                let meta = read_meta(&candidate)?;
                return Ok((candidate, meta));
            }
        }
        Err(CheckpointError::NotFound(checkpoint_id.to_string()))
    }

    // -----------------------------------------------------------------------
    // 回滚
    // -----------------------------------------------------------------------

    /// 回滚到某个检查点，并逐文件回读校验。
    pub fn restore(&self, checkpoint_id: &str) -> Result<RestoreOutcome, CheckpointError> {
        let (dir, meta) = self.find(checkpoint_id)?;
        let guard = PathGuard::new(PathBuf::from(&meta.workspace_root))?;
        let files_dir = dir.join(FILES_DIR);

        let mut report = Vec::with_capacity(meta.files.len());
        let mut verified = true;
        for file in &meta.files {
            match restore_one(&guard, &files_dir, file) {
                Ok((status, hash)) => {
                    // 期望状态：modified / deleted 回滚后必须等于登记哈希；
                    // created 回滚后必须不存在（hash 为 None，比对自动成立）。
                    if hash.as_deref() != file.before_hash.as_deref() {
                        verified = false;
                    }
                    report.push(CheckpointRestoreFileView {
                        path: file.path.clone(),
                        status,
                        hash,
                        detail: None,
                    });
                }
                Err(err) => {
                    verified = false;
                    report.push(CheckpointRestoreFileView {
                        path: file.path.clone(),
                        status: CheckpointRestoreStatus::Failed,
                        hash: None,
                        detail: Some(err.to_string()),
                    });
                }
            }
        }

        Ok(RestoreOutcome {
            checkpoint: meta.to_view(),
            files: report,
            verified,
        })
    }

    // -----------------------------------------------------------------------
    // 删除
    // -----------------------------------------------------------------------

    /// 删掉一个检查点。不存在时返回 `(false, 0)`，让调用方可以幂等清理。
    pub fn delete(&self, checkpoint_id: &str) -> Result<(bool, u64), CheckpointError> {
        match self.find(checkpoint_id) {
            Ok((dir, meta)) => {
                fs::remove_dir_all(&dir).map_err(|err| CheckpointError::io(&dir, err))?;
                Ok((true, meta.total_bytes))
            }
            Err(CheckpointError::NotFound(_)) => Ok((false, 0)),
            Err(err) => Err(err),
        }
    }
}

/// 回滚结果（协议层据此拼 `CheckpointRestoreResult`）。
#[derive(Debug)]
pub struct RestoreOutcome {
    pub checkpoint: CheckpointView,
    pub files: Vec<CheckpointRestoreFileView>,
    pub verified: bool,
}

/// 回滚单个文件，返回状态与回读哈希。
fn restore_one(
    guard: &PathGuard,
    files_dir: &Path,
    file: &MetaFile,
) -> Result<(CheckpointRestoreStatus, Option<String>), CheckpointError> {
    match file.kind {
        CheckpointFileKind::Created => {
            // 快照时它不存在：回滚就是把它抹掉，且不再回读哈希。
            let abs = guard.resolve(&file.path)?;
            if abs.exists() {
                fs::remove_file(&abs).map_err(|err| CheckpointError::io(&abs, err))?;
            }
            Ok((CheckpointRestoreStatus::Removed, None))
        }
        CheckpointFileKind::Modified | CheckpointFileKind::Deleted => {
            let abs = guard.resolve_for_write(&file.path)?;
            if let Some(parent) = abs.parent() {
                fs::create_dir_all(parent).map_err(|err| CheckpointError::io(parent, err))?;
            }
            let src = files_dir.join(&file.path);
            fs::copy(&src, &abs).map_err(|err| CheckpointError::io(&src, err))?;
            let written = fs::read(&abs).map_err(|err| CheckpointError::io(&abs, err))?;
            let status = if file.kind == CheckpointFileKind::Deleted {
                CheckpointRestoreStatus::Recreated
            } else {
                CheckpointRestoreStatus::Restored
            };
            Ok((status, Some(hash_bytes(&written))))
        }
    }
}

// ---------------------------------------------------------------------------
// Agent 自动快照
// ---------------------------------------------------------------------------

/// Agent 运行期间的自动快照记录器。
///
/// 一次 run 里**同一个文件只快照一次**：Agent 往往对同一文件连改几轮，每轮都拍
/// 一份既费磁盘，又让「回滚」这词变得难以理解——用户想回到的是「Agent 动手之前」。
/// 记录器惰性建目录：一条文件都没碰过就不留下任何东西。
pub struct CheckpointRecorder {
    store: CheckpointStore,
    root: PathBuf,
    guard: PathGuard,
    session_id: Option<String>,
    checkpoint_id: String,
    label: String,
    state: Mutex<RecorderState>,
}

#[derive(Default)]
struct RecorderState {
    dir: Option<PathBuf>,
    files: Vec<MetaFile>,
    seen: HashSet<String>,
    skipped: u64,
    total_bytes: u64,
}

impl CheckpointRecorder {
    /// 为一次 Agent 运行建记录器。`run_tag` 会进标题，便于在列表里对上是哪次运行。
    pub fn new(
        store: CheckpointStore,
        root: PathBuf,
        session_id: Option<String>,
        run_tag: &str,
    ) -> Result<Self, CheckpointError> {
        let guard = PathGuard::new(root.clone())?;
        let checkpoint_id = uuid::Uuid::new_v4().to_string();
        Ok(Self {
            store,
            root,
            guard,
            session_id,
            label: format!("Agent 改动前（run {}）", short_tag(run_tag)),
            checkpoint_id,
            state: Mutex::new(RecorderState::default()),
        })
    }

    /// 工具写文件**之前**调用：把当前内容收进快照。
    ///
    /// 这里永不返回错误——快照失败不该拦下一次正常编辑，日志里留痕即可。
    pub fn before_write(&self, abs: &Path) {
        if !self.guard.contains(abs) {
            return;
        }
        let rel = self.guard.to_relative(abs);
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(err) => {
                tracing::warn!(error = %err, "检查点记录器锁已中毒，跳过本次快照");
                return;
            }
        };
        if !state.seen.insert(rel.clone()) {
            return;
        }

        let bytes = match fs::read(abs) {
            Ok(bytes) => Some(bytes),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => {
                tracing::warn!(path = %abs.display(), error = %err, "读取待快照文件失败");
                return;
            }
        };
        let size = bytes.as_ref().map_or(0, |bytes| bytes.len() as u64);
        if size > MAX_FILE_BYTES || state.total_bytes.saturating_add(size) > MAX_TOTAL_BYTES {
            state.skipped += 1;
            tracing::warn!(path = %abs.display(), size, "文件超出快照上限，已跳过");
            return;
        }

        let mut before_hash = None;
        if let Some(bytes) = &bytes {
            let dir = self.prepare_dir(&mut state);
            let dest = dir.join(FILES_DIR).join(&rel);
            if let Some(parent) = dest.parent() {
                if let Err(err) = fs::create_dir_all(parent) {
                    state.skipped += 1;
                    tracing::warn!(error = %err, "创建快照目录失败");
                    return;
                }
            }
            if let Err(err) = fs::write(&dest, bytes) {
                state.skipped += 1;
                tracing::warn!(error = %err, "写入快照内容失败");
                return;
            }
            before_hash = Some(hash_bytes(bytes));
            state.total_bytes += size;
        }

        state.files.push(MetaFile {
            path: rel,
            kind: if bytes.is_some() {
                CheckpointFileKind::Modified
            } else {
                CheckpointFileKind::Created
            },
            before_hash,
            size,
        });
    }

    /// 运行结束：有记录就落盘 `meta.json` 并返回检查点 id。
    pub fn finish(&self) -> Option<String> {
        let mut state = self.state.lock().ok()?;
        if state.files.is_empty() {
            // 没碰过文件：把可能的空目录清掉，别在列表里留空检查点。
            if let Some(dir) = state.dir.take() {
                let _ = fs::remove_dir_all(dir);
            }
            return None;
        }
        let meta = SnapshotMeta {
            schema_version: META_SCHEMA,
            checkpoint_id: self.checkpoint_id.clone(),
            label: self.label.clone(),
            workspace_root: self.root.display().to_string(),
            origin: CheckpointOrigin::Agent,
            session_id: self.session_id.clone(),
            created_at_ms: crate::state::now_ms(),
            total_bytes: state.total_bytes,
            skipped: state.skipped,
            files: std::mem::take(&mut state.files),
        };
        let dir = state.dir.clone()?;
        match write_meta(&dir, &meta) {
            Ok(()) => Some(meta.checkpoint_id),
            Err(err) => {
                tracing::warn!(error = %err, "写入检查点元数据失败");
                None
            }
        }
    }

    /// 当前是否已经收过文件（供调用方决定要不要报 id）。
    pub fn has_files(&self) -> bool {
        self.state
            .lock()
            .map(|state| !state.files.is_empty())
            .unwrap_or(false)
    }

    fn prepare_dir(&self, state: &mut RecorderState) -> PathBuf {
        if let Some(dir) = &state.dir {
            return dir.clone();
        }
        let dir = self.store.dir_for(self.guard.root(), &self.checkpoint_id);
        if let Err(err) = fs::create_dir_all(dir.join(FILES_DIR)) {
            tracing::warn!(error = %err, "创建检查点目录失败");
        }
        state.dir = Some(dir.clone());
        dir
    }
}

/// 让记录器直接当循环的留痕钩子用。
///
/// 循环给的已经是绝对路径，记录器自己再过一遍 [`PathGuard`]（工作区外的一律不理），
/// 所以这里只是把两个同名方法对上，不加任何语义。
impl ai_ide_agent::WriteHook for CheckpointRecorder {
    fn before_write(&self, abs: &Path) {
        CheckpointRecorder::before_write(self, abs);
    }
}

// ---------------------------------------------------------------------------
// 目录遍历与元数据读写
// ---------------------------------------------------------------------------

/// 遍历整个工作区收集候选文件（受忽略规则与保护路径约束）。
fn collect_workspace_files(root: &Path, guard: &PathGuard) -> (Vec<PathBuf>, u64) {
    let mut out = Vec::new();
    let mut skipped = 0u64;
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let rules = IgnoreStack::build(root, &dir);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                skipped += 1;
                continue;
            };
            // 符号链接不跟随：可能指向工作区之外，也可能成环。
            if meta.file_type().is_symlink() {
                skipped += 1;
                continue;
            }
            let is_dir = meta.is_dir();
            if rules.is_ignored(&path, is_dir) || guard.is_protected(&path) {
                continue;
            }
            if is_dir {
                stack.push(path);
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            if out.len() >= MAX_FILES {
                skipped += 1;
                continue;
            }
            out.push(path);
        }
    }

    out.sort();
    (out, skipped)
}

fn read_meta(dir: &Path) -> Result<SnapshotMeta, CheckpointError> {
    let path = dir.join(META_FILE);
    let text = fs::read_to_string(&path).map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            CheckpointError::NotFound(dir.display().to_string())
        } else {
            CheckpointError::io(&path, err)
        }
    })?;
    let meta: SnapshotMeta = serde_json::from_str(&text)
        .map_err(|err| CheckpointError::Corrupt(format!("{}: {err}", path.display())))?;
    if meta.schema_version > META_SCHEMA {
        return Err(CheckpointError::Corrupt(format!(
            "{} 的结构版本 {} 比内核认识的 {META_SCHEMA} 更新",
            path.display(),
            meta.schema_version
        )));
    }
    Ok(meta)
}

fn read_all_metas(dir: &Path) -> Result<Vec<SnapshotMeta>, CheckpointError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(CheckpointError::io(dir, err)),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // 单个检查点损坏不该让整个列表打不开：跳过并留日志。
        match read_meta(&path) {
            Ok(meta) => out.push(meta),
            Err(err) => tracing::warn!(dir = %path.display(), error = %err, "跳过损坏的检查点"),
        }
    }
    Ok(out)
}

fn write_meta(dir: &Path, meta: &SnapshotMeta) -> Result<(), CheckpointError> {
    fs::create_dir_all(dir).map_err(|err| CheckpointError::io(dir, err))?;
    let path = dir.join(META_FILE);
    let text = serde_json::to_string_pretty(meta)
        .map_err(|err| CheckpointError::Corrupt(format!("序列化失败：{err}")))?;
    fs::write(&path, text).map_err(|err| CheckpointError::io(&path, err))
}

/// 检查点 id 只允许字母数字与连字符：它会被拼进路径。
fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}

fn normalize_label(label: Option<&str>, origin: CheckpointOrigin, id: &str) -> String {
    if let Some(label) = label.map(str::trim).filter(|label| !label.is_empty()) {
        return label.to_string();
    }
    match origin {
        CheckpointOrigin::Manual => format!("手动检查点 {}", &id[..8]),
        CheckpointOrigin::Agent => format!("Agent 改动前（{}）", &id[..8]),
    }
}

fn short_tag(tag: &str) -> String {
    tag.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// 在系统临时目录下建一对「工作区 + 检查点库」，用完删掉。
    struct Fixture {
        root: PathBuf,
        base: PathBuf,
        store: CheckpointStore,
    }

    impl Fixture {
        fn new() -> Self {
            let tag = format!(
                "{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            );
            let root = std::env::temp_dir().join(format!("aiide-ckpt-ws-{tag}"));
            let base = std::env::temp_dir().join(format!("aiide-ckpt-store-{tag}"));
            fs::create_dir_all(&root).expect("建工作区");
            fs::create_dir_all(&base).expect("建检查点库");
            let store = CheckpointStore::new(base.clone());
            Self { root, base, store }
        }

        fn write(&self, rel: &str, content: &str) {
            let path = self.root.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("建目录");
            }
            fs::write(&path, content).expect("写文件");
        }

        fn read(&self, rel: &str) -> String {
            fs::read_to_string(self.root.join(rel)).expect("读文件")
        }

        fn create_manual(&self, label: &str) -> CheckpointView {
            self.store
                .create(&SnapshotRequest {
                    root: &self.root,
                    label: Some(label),
                    paths: None,
                    origin: CheckpointOrigin::Manual,
                    session_id: None,
                })
                .expect("创建检查点")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn create_then_restore_restores_bytes_exactly() {
        let fixture = Fixture::new();
        fixture.write("src/main.rs", "fn main() { println!(\"v1\"); }\n");
        fixture.write("README.md", "# 项目\n");

        let snapshot = fixture.create_manual("基线");
        assert_eq!(snapshot.files.len(), 2);
        assert_eq!(snapshot.origin, CheckpointOrigin::Manual);

        // 改两个文件、删一个文件、再新增一个。
        fixture.write("src/main.rs", "fn main() { println!(\"v2\"); }\n");
        fs::remove_file(fixture.root.join("README.md")).expect("删文件");
        fixture.write("src/extra.rs", "pub fn extra() {}\n");

        let outcome = fixture
            .store
            .restore(&snapshot.checkpoint_id)
            .expect("回滚");
        assert!(outcome.verified, "回滚后必须逐字节一致");
        assert_eq!(
            fixture.read("src/main.rs"),
            "fn main() { println!(\"v1\"); }\n"
        );
        assert_eq!(fixture.read("README.md"), "# 项目\n");
        // 全量快照只登记「当时存在的文件」，没能力识别「之后才出现的文件」；
        // 而回滚不该顺手删掉用户自己新写的东西，所以这个文件必须仍在。
        assert!(fixture.root.join("src/extra.rs").exists());
    }

    #[test]
    fn deleted_file_is_recreated_on_restore() {
        let fixture = Fixture::new();
        fixture.write("docs/a.md", "alpha");
        let snapshot = fixture.create_manual("含删除");
        fs::remove_file(fixture.root.join("docs/a.md")).expect("先删掉");
        // 删完之后再拍一次：这次 a.md 应被登记为 created（快照时不存在）。
        let gone = fixture
            .store
            .create(&SnapshotRequest {
                root: &fixture.root,
                label: Some("删后"),
                paths: Some(&["docs/a.md".to_string()]),
                origin: CheckpointOrigin::Manual,
                session_id: None,
            })
            .expect("拍第二张");
        assert_eq!(gone.files[0].kind, CheckpointFileKind::Created);

        // 回到第一张：文件应被重建。
        let outcome = fixture
            .store
            .restore(&snapshot.checkpoint_id)
            .expect("回滚");
        assert!(outcome.verified);
        assert_eq!(fixture.read("docs/a.md"), "alpha");
    }

    #[test]
    fn restore_reports_failed_file_without_aborting_others() {
        let fixture = Fixture::new();
        fixture.write("ok.txt", "ok");
        let snapshot = fixture.create_manual("坏一张");

        // 把快照内容毁掉，模拟「快照文件被误删」。
        let stored = fixture
            .store
            .dir_for(&fixture.root, &snapshot.checkpoint_id)
            .join(FILES_DIR)
            .join("ok.txt");
        fs::remove_file(&stored).expect("删快照内容");

        fixture.write("ok.txt", "changed");
        let outcome = fixture
            .store
            .restore(&snapshot.checkpoint_id)
            .expect("回滚");
        assert!(!outcome.verified, "快照内容缺失时不能报 verified");
        assert_eq!(outcome.files.len(), 1);
        assert_eq!(outcome.files[0].status, CheckpointRestoreStatus::Failed);
        assert!(outcome.files[0].detail.is_some());
    }

    #[test]
    fn ignored_and_protected_paths_stay_out_of_snapshots() {
        let fixture = Fixture::new();
        fixture.write("src/lib.rs", "pub fn a() {}");
        fixture.write("node_modules/pkg/index.js", "module.exports = 1");
        fixture.write(".env", "SECRET=1");

        let snapshot = fixture.create_manual("过滤");
        let paths: Vec<&str> = snapshot
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert!(paths.contains(&"src/lib.rs"));
        assert!(!paths.iter().any(|path| path.contains("node_modules")));
        assert!(!paths.contains(&".env"));
    }

    #[test]
    fn explicit_sensitive_path_is_skipped_not_fatal() {
        let fixture = Fixture::new();
        fixture.write(".env", "SECRET=1");
        let snapshot = fixture.store.create(&SnapshotRequest {
            root: &fixture.root,
            label: Some("指定敏感"),
            paths: Some(&[".env".to_string()]),
            origin: CheckpointOrigin::Manual,
            session_id: None,
        });
        match snapshot {
            Err(CheckpointError::Empty(_)) => {}
            other => panic!("敏感路径应全被跳过，实际：{other:?}"),
        }
    }

    #[test]
    fn list_is_newest_first_and_paginates() {
        let fixture = Fixture::new();
        fixture.write("a.txt", "1");
        let first = fixture.create_manual("第一");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = fixture.create_manual("第二");

        let (page, total) = fixture.store.list(&fixture.root, 1, 0).expect("列表");
        assert_eq!(total, 2);
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].checkpoint_id, second.checkpoint_id);
        let (page2, _) = fixture.store.list(&fixture.root, 1, 1).expect("第二页");
        assert_eq!(page2[0].checkpoint_id, first.checkpoint_id);
    }

    #[test]
    fn delete_reports_freed_bytes_and_is_idempotent() {
        let fixture = Fixture::new();
        fixture.write("a.txt", "0123456789");
        let snapshot = fixture.create_manual("删");
        let (removed, freed) = fixture.store.delete(&snapshot.checkpoint_id).expect("删");
        assert!(removed);
        assert_eq!(freed, 10);
        let (again, freed_again) = fixture.store.delete(&snapshot.checkpoint_id).expect("再删");
        assert!(!again);
        assert_eq!(freed_again, 0);
    }

    #[test]
    fn unknown_checkpoint_is_reported_as_not_found() {
        let fixture = Fixture::new();
        let err = fixture.store.restore("no-such-id").expect_err("应报不存在");
        assert_eq!(err.code(), ErrorCode::CHECKPOINT_NOT_FOUND);
        // 带路径分隔符的 id 不得拼进路径去探测目录。
        let err = fixture.store.restore("../../etc").expect_err("应拒绝");
        assert_eq!(err.code(), ErrorCode::CHECKPOINT_NOT_FOUND);
    }

    #[test]
    fn recorder_snapshots_each_file_once_and_skips_empty_runs() {
        let fixture = Fixture::new();
        fixture.write("src/a.rs", "v1");

        let recorder = CheckpointRecorder::new(
            fixture.store.clone(),
            fixture.root.clone(),
            Some("session-1".into()),
            "run-abcdef123456",
        )
        .expect("建记录器");
        recorder.before_write(&fixture.root.join("src/a.rs"));
        // 同一个文件第二次改动不该再拍一份。
        recorder.before_write(&fixture.root.join("src/a.rs"));
        recorder.before_write(&fixture.root.join("src/new.rs"));

        let id = recorder.finish().expect("应产出检查点");
        let (list, total) = fixture.store.list(&fixture.root, 50, 0).expect("列表");
        assert_eq!(total, 1);
        assert_eq!(list[0].checkpoint_id, id);
        assert_eq!(list[0].origin, CheckpointOrigin::Agent);
        assert_eq!(list[0].session_id.as_deref(), Some("session-1"));
        assert_eq!(list[0].files.len(), 2);
        assert!(list[0].label.contains("run"));

        // 恢复后 a.rs 回到 v1，new.rs 被删掉。
        fixture.write("src/a.rs", "v2");
        fixture.write("src/new.rs", "created by agent");
        let outcome = fixture.store.restore(&id).expect("回滚");
        assert!(outcome.verified);
        assert_eq!(fixture.read("src/a.rs"), "v1");
        assert!(!fixture.root.join("src/new.rs").exists());

        // 空 run 不产生检查点。
        let empty = CheckpointRecorder::new(
            fixture.store.clone(),
            fixture.root.clone(),
            None,
            "run-empty",
        )
        .expect("建记录器");
        assert!(empty.finish().is_none());
        let (_, total_after) = fixture.store.list(&fixture.root, 50, 0).expect("列表");
        assert_eq!(total_after, 1);
    }

    #[test]
    fn recorder_ignores_paths_outside_workspace() {
        let fixture = Fixture::new();
        let outside = std::env::temp_dir().join("aiide-ckpt-outside.txt");
        fs::write(&outside, "x").expect("写外部文件");
        let recorder = CheckpointRecorder::new(
            fixture.store.clone(),
            fixture.root.clone(),
            None,
            "run-outside",
        )
        .expect("建记录器");
        recorder.before_write(&outside);
        assert!(recorder.finish().is_none(), "工作区外的路径不该进快照");
        let _ = fs::remove_file(outside);
    }
}
