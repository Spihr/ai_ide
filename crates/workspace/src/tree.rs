//! 目录树：按层懒加载。
//!
//! UI 展开一个节点才请求一层，所以这里**不建全量树**——那是 indexer 的活。
//! 被忽略的条目默认直接剔除（不返回、也不下探），开启 `include_ignored`
//! 时返回并标记，由 UI 灰显。

use crate::ignore_rules::IgnoreStack;
use crate::path::PathGuard;
use crate::WorkspaceError;
use ai_ide_protocol::{EntryKind, TreeEntry};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// 单次请求的最大递归深度，防止误传 `depth=100` 把内核拖死。
pub const MAX_DEPTH: u32 = 8;

/// 列出目录条目。
///
/// - `input` 为 `None` 或空白时表示工作区根目录；
/// - `depth` 为 1 表示只列一层（UI 默认）；
/// - 返回按「目录优先、再按名称不区分大小写」排序。
pub fn list(
    guard: &PathGuard,
    input: Option<&str>,
    depth: u32,
    include_ignored: bool,
) -> Result<Vec<TreeEntry>, WorkspaceError> {
    let dir = match input {
        Some(p) if !p.trim().is_empty() => guard.resolve(p)?,
        _ => guard.root().to_path_buf(),
    };
    if !dir.is_dir() {
        return Err(WorkspaceError::NotFound(dir.display().to_string()));
    }

    let depth = depth.clamp(1, MAX_DEPTH);
    let mut out: Vec<TreeEntry> = Vec::new();
    // 广度优先：外层先入列，保证 UI 收到的顺序是稳定的层级序。
    let mut frontier: Vec<(PathBuf, u32)> = vec![(dir, 1)];

    while let Some((current, level)) = frontier.pop() {
        let stack = IgnoreStack::build(guard.root(), &current);
        let mut level_entries = read_dir_sorted(&current)?;

        for (path, kind, size, mtime, ignored) in level_entries.drain(..) {
            let is_dir = matches!(kind, EntryKind::Dir);
            if ignored && !include_ignored {
                continue;
            }
            if is_dir && level < depth {
                frontier.push((path.clone(), level + 1));
            }
            out.push(TreeEntry {
                name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path: path.display().to_string(),
                kind,
                size,
                mtime,
                ignored: stack.is_ignored(&path, is_dir),
            });
        }
    }

    Ok(out)
}

/// 读取一层目录并排序，返回 `(路径, 类型, 大小, mtime, 是否忽略)`。
#[allow(clippy::type_complexity)]
fn read_dir_sorted(
    dir: &Path,
) -> Result<Vec<(PathBuf, EntryKind, u64, i64, bool)>, WorkspaceError> {
    let stack = IgnoreStack::build(dir, dir);
    let read = std::fs::read_dir(dir).map_err(|e| WorkspaceError::io(dir, e))?;

    let mut items: Vec<(PathBuf, EntryKind, u64, i64, bool)> = Vec::new();
    for entry in read {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::debug!(dir = %dir.display(), error = %err, "跳过无法读取的目录项");
                continue;
            }
        };
        let path = entry.path();
        // 用 symlink_metadata：链接本身不该被当作目标，避免跟随到工作区外。
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(err) => {
                tracing::debug!(path = %path.display(), error = %err, "跳过无法 stat 的目录项");
                continue;
            }
        };
        let ft = meta.file_type();
        let kind = if ft.is_symlink() {
            EntryKind::Symlink
        } else if ft.is_dir() {
            EntryKind::Dir
        } else if ft.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        let size = if matches!(kind, EntryKind::File) {
            meta.len()
        } else {
            0
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let ignored = stack.is_ignored(&path, matches!(kind, EntryKind::Dir));
        items.push((path, kind, size, mtime, ignored));
    }

    items.sort_by(|a, b| {
        let a_dir = matches!(a.1, EntryKind::Dir);
        let b_dir = matches!(b.1, EntryKind::Dir);
        b_dir.cmp(&a_dir).then_with(|| {
            a.0.file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .cmp(&b.0.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        })
    });
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ai-ide-tree-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    #[test]
    fn lists_directories_before_files() {
        let root = temp_workspace("order");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("b.txt"), "b").unwrap();
        fs::write(root.join("a.txt"), "a").unwrap();

        let guard = PathGuard::new(root.clone()).unwrap();
        let entries = list(&guard, None, 1, false).unwrap();

        assert_eq!(entries.len(), 3);
        assert!(matches!(entries[0].kind, EntryKind::Dir));
        assert_eq!(entries[0].name, "src");
        assert_eq!(entries[1].name, "a.txt");
        assert_eq!(entries[2].name, "b.txt");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn ignored_entries_hidden_by_default() {
        let root = temp_workspace("ignore");
        fs::create_dir_all(root.join("node_modules")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();

        let guard = PathGuard::new(root.clone()).unwrap();
        let hidden = list(&guard, None, 1, false).unwrap();
        assert_eq!(hidden.len(), 1);

        let shown = list(&guard, None, 1, true).unwrap();
        assert_eq!(shown.len(), 2);
        assert!(shown.iter().any(|e| e.ignored && e.name == "node_modules"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn gitignore_is_honoured() {
        let root = temp_workspace("gitignore");
        fs::create_dir_all(root.join("logs")).unwrap();
        fs::write(root.join(".gitignore"), "logs/\n").unwrap();
        fs::write(root.join("keep.txt"), "x").unwrap();

        let guard = PathGuard::new(root.clone()).unwrap();
        let visible = list(&guard, None, 1, false).unwrap();
        assert!(visible.iter().all(|e| e.name != "logs"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn recursion_respects_depth() {
        let root = temp_workspace("depth");
        fs::create_dir_all(root.join("a").join("b").join("c")).unwrap();

        let guard = PathGuard::new(root.clone()).unwrap();
        let one = list(&guard, None, 1, false).unwrap();
        assert_eq!(one.len(), 1, "深度 1 只有 a");

        let three = list(&guard, None, 3, false).unwrap();
        assert_eq!(three.len(), 3, "深度 3 应含 a/b/c");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_directory_is_not_found() {
        let root = temp_workspace("missing");
        let guard = PathGuard::new(root.clone()).unwrap();
        let err = list(&guard, Some("nope"), 1, false).unwrap_err();
        assert!(matches!(err, WorkspaceError::NotFound(_)));
        fs::remove_dir_all(&root).ok();
    }
}
