//! 文件监听：把 `notify` 的原始事件去抖成 [`PathChange`] 批次。
//!
//! 三个关键处理：
//!
//! 1. **去抖 200ms**。一次保存往往触发多个事件（写入、改名、属性变更），
//!    逐个上报会把 UI 刷爆，也会让「外部改动提示」重复弹。
//! 2. **净效果合并**。[`Create`] + [`Modify`] 合成一次「新建」；
//!    [`Create`] + [`Delete`] 相互抵消——临时文件的生命周期不该惊动用户。
//! 3. **噪音过滤**。`.git` 与默认忽略目录（`node_modules` / `target` …）
//!    的事件全部丢弃，否则 `git status` 就能引发一次事件风暴。

use crate::ignore_rules::is_default_ignored_path;
use crate::WorkspaceError;
use ai_ide_protocol::{PathChange, PathChangeKind};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

/// 去抖窗口。
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// 批次接收端：每个元素是一次去抖后的变更批次。
pub type ChangeSender = UnboundedSender<Vec<PathChange>>;

/// 监听句柄；drop 即停止监听。
pub struct WatcherHandle {
    /// 只为持有监听器的生命周期而存在：字段一 drop，监听立即停止。
    /// 因此它永远不需要被读取，`dead_code` 在这里是预期行为而非疏漏。
    #[allow(dead_code)]
    inner: RecommendedWatcher,
}

impl std::fmt::Debug for WatcherHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatcherHandle").finish_non_exhaustive()
    }
}

impl WatcherHandle {
    /// 停止监听（drop 时也会停，这里给个显式出口便于语义清晰）。
    pub fn stop(self) {
        drop(self);
    }
}

/// 在独立线程里启动去抖循环，事件以相对路径批次推入 `tx`。
///
/// `guard_root` 是工作区根，用于把绝对路径转成相对路径。
pub fn start(
    root: &Path,
    guard_root: PathBuf,
    tx: ChangeSender,
) -> Result<WatcherHandle, WorkspaceError> {
    let (raw_tx, raw_rx) = std_mpsc::channel::<notify::Result<Event>>();

    let mut watcher = notify::recommended_watcher(move |res| {
        // 接收端已关闭时发送失败是正常退出路径，忽略即可。
        let _ = raw_tx.send(res);
    })
    .map_err(|e| WorkspaceError::Other(format!("创建文件监听器失败：{e}")))?;

    watcher
        .watch(root, RecursiveMode::Recursive)
        .map_err(|e| WorkspaceError::Other(format!("监听目录失败（{}）：{e}", root.display())))?;

    std::thread::Builder::new()
        .name("ai-ide-watch-debounce".into())
        .spawn(move || debounce_loop(raw_rx, tx, guard_root))
        .map_err(|e| WorkspaceError::Other(format!("启动监听线程失败：{e}")))?;

    Ok(WatcherHandle { inner: watcher })
}

/// 去抖主循环：攒够一个窗口的静默期就 flush 一批。
fn debounce_loop(rx: std_mpsc::Receiver<notify::Result<Event>>, tx: ChangeSender, root: PathBuf) {
    let mut pending: BTreeMap<String, PathChange> = BTreeMap::new();

    loop {
        // 没攒到东西就阻塞等；攒到了就只等到窗口结束。
        let received = if pending.is_empty() {
            match rx.recv() {
                Ok(value) => Some(value),
                Err(_) => break,
            }
        } else {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(value) => Some(value),
                Err(std_mpsc::RecvTimeoutError::Timeout) => None,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                    flush(&mut pending, &tx, &root);
                    break;
                }
            }
        };

        match received {
            Some(Ok(event)) => absorb(event, &mut pending, &root),
            Some(Err(err)) => tracing::debug!(error = %err, "文件监听事件错误"),
            None => flush(&mut pending, &tx, &root),
        }
    }
}

/// 把一批变更推给上层。
fn flush(pending: &mut BTreeMap<String, PathChange>, tx: &ChangeSender, root: &Path) {
    if pending.is_empty() {
        return;
    }
    let changes: Vec<PathChange> = std::mem::take(pending).into_values().collect();
    let rel_root = root.to_string_lossy().replace('\\', "/");
    tracing::trace!(count = changes.len(), root = %rel_root, "推送文件变更批次");
    if tx.send(changes).is_err() {
        tracing::debug!("工作区变更接收端已关闭");
    }
}

/// 吸收一个原始事件。
fn absorb(event: Event, pending: &mut BTreeMap<String, PathChange>, root: &Path) {
    let Some((kind, from, to)) = classify(&event) else {
        return;
    };

    let mut push = |path: &Path, kind: PathChangeKind, old: Option<String>| {
        let Some(rel) = to_relative(root, path) else {
            return;
        };
        if should_skip(&rel) {
            return;
        }
        merge(pending, rel, kind, old);
    };

    match kind {
        PathChangeKind::Renamed => {
            if let Some(old) = from.as_ref() {
                push(old, PathChangeKind::Deleted, None);
            }
            if let Some(new) = to.as_ref() {
                push(
                    new,
                    PathChangeKind::Created,
                    from.as_ref().map(|p| relative_string(root, p)),
                );
            }
        }
        other => {
            if let Some(path) = to.or(from) {
                push(&path, other, None);
            }
        }
    }
}

/// 把事件映射为「语义变更」。
fn classify(event: &Event) -> Option<(PathChangeKind, Option<PathBuf>, Option<PathBuf>)> {
    let paths = &event.paths;
    match &event.kind {
        EventKind::Create(_) => Some((PathChangeKind::Created, None, paths.first().cloned())),
        EventKind::Remove(_) => Some((PathChangeKind::Deleted, paths.first().cloned(), None)),
        EventKind::Modify(notify::event::ModifyKind::Name(mode)) => {
            use notify::event::RenameMode;
            match mode {
                RenameMode::Both => Some((
                    PathChangeKind::Renamed,
                    paths.first().cloned(),
                    paths.get(1).cloned(),
                )),
                RenameMode::From => Some((PathChangeKind::Deleted, paths.first().cloned(), None)),
                RenameMode::To => Some((PathChangeKind::Created, None, paths.first().cloned())),
                _ => Some((PathChangeKind::Modified, None, paths.first().cloned())),
            }
        }
        EventKind::Modify(_) => Some((PathChangeKind::Modified, None, paths.first().cloned())),
        // 访问与其它事件对编辑器无意义，直接丢弃。
        _ => None,
    }
}

/// 合并同一路径的多次事件，取「净效果」。
fn merge(
    pending: &mut BTreeMap<String, PathChange>,
    path: String,
    kind: PathChangeKind,
    old_path: Option<String>,
) {
    use PathChangeKind::*;
    let next = match pending.get(&path).map(|c| c.kind) {
        None => kind,
        Some(prev) => match (prev, kind) {
            // 刚建又改 → 还是一次「新建」
            (Created, Modified) => Created,
            // 建了又删 → 净效果为零
            (Created, Deleted) => {
                pending.remove(&path);
                return;
            }
            // 删了又建 → 等同改写
            (Deleted, Created) => Modified,
            // 改名后又被改 → 仍是改名
            (Renamed, Modified) => Renamed,
            (_, next) => next,
        },
    };
    pending.insert(
        path.clone(),
        PathChange {
            path,
            kind: next,
            old_path,
        },
    );
}

/// 噪音过滤：`.git` 与默认忽略目录不打扰用户。
fn should_skip(rel: &str) -> bool {
    let as_path = Path::new(rel);
    if as_path
        .components()
        .any(|c| matches!(c, Component::Normal(n) if n.to_string_lossy() == ".git"))
    {
        return true;
    }
    if is_default_ignored_path(as_path) {
        return true;
    }
    // 编辑器自己落下的临时文件不上报。
    rel.contains(".ai-ide-tmp-")
}

/// 绝对路径 → 相对工作区根（`/` 分隔）；越界返回 `None`。
fn to_relative(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
}

fn relative_string(root: &Path, path: &Path) -> String {
    to_relative(root, path).unwrap_or_else(|| path.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_folds_create_then_modify() {
        let mut pending = BTreeMap::new();
        merge(&mut pending, "a.txt".into(), PathChangeKind::Created, None);
        merge(&mut pending, "a.txt".into(), PathChangeKind::Modified, None);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending["a.txt"].kind, PathChangeKind::Created);
    }

    #[test]
    fn merge_cancels_transient_file() {
        let mut pending = BTreeMap::new();
        merge(
            &mut pending,
            "tmp.txt".into(),
            PathChangeKind::Created,
            None,
        );
        merge(
            &mut pending,
            "tmp.txt".into(),
            PathChangeKind::Deleted,
            None,
        );
        assert!(pending.is_empty(), "临时文件不该惊动用户");
    }

    #[test]
    fn merge_treats_delete_then_create_as_modify() {
        let mut pending = BTreeMap::new();
        merge(&mut pending, "a.txt".into(), PathChangeKind::Deleted, None);
        merge(&mut pending, "a.txt".into(), PathChangeKind::Created, None);
        assert_eq!(pending["a.txt"].kind, PathChangeKind::Modified);
    }

    #[test]
    fn noise_paths_are_filtered() {
        assert!(should_skip(".git/HEAD"));
        assert!(should_skip("node_modules/react/index.js"));
        assert!(should_skip("src/.a.txt.ai-ide-tmp-1234"));
        assert!(!should_skip("src/main.rs"));
    }

    #[test]
    fn relative_conversion_strips_root() {
        let root = Path::new(r"C:\proj");
        assert_eq!(
            to_relative(root, Path::new(r"C:\proj\src\main.rs")),
            Some("src/main.rs".to_string())
        );
        assert_eq!(to_relative(root, Path::new(r"C:\other\main.rs")), None);
    }

    #[test]
    fn classify_maps_create_and_remove() {
        let create = Event::new(EventKind::Create(notify::event::CreateKind::File))
            .add_path(PathBuf::from(r"C:\p\a.txt"));
        let (kind, _, to) = classify(&create).unwrap();
        assert_eq!(kind, PathChangeKind::Created);
        assert_eq!(to.unwrap(), PathBuf::from(r"C:\p\a.txt"));

        let remove = Event::new(EventKind::Remove(notify::event::RemoveKind::File))
            .add_path(PathBuf::from(r"C:\p\a.txt"));
        let (kind, from, _) = classify(&remove).unwrap();
        assert_eq!(kind, PathChangeKind::Deleted);
        assert_eq!(from.unwrap(), PathBuf::from(r"C:\p\a.txt"));
    }
}
