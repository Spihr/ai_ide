//! 工作区注册表。
//!
//! 协议里每个方法都带 `workspace_id`，所以内核天然要支持**同时打开多个**工作区。
//! UI 的 M1 只会开一个，但把注册表做成多例的代价几乎为零，反而省掉了
//! 「未来支持多根工作区时推倒重来」的风险。

use crate::path::PathGuard;
use crate::watcher::{self, ChangeSender, WatcherHandle};
use crate::WorkspaceError;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

/// 一个已打开的工作区。
pub struct Workspace {
    /// 协议句柄。
    pub id: String,
    /// 规范化后的根目录。
    pub root: PathBuf,
    /// 根目录名（UI 标题用）。
    pub name: String,
    /// 路径守卫。
    pub guard: PathGuard,
    watcher: Mutex<Option<WatcherHandle>>,
}

impl std::fmt::Debug for Workspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspace")
            .field("id", &self.id)
            .field("root", &self.root)
            .field("watching", &self.is_watching())
            .finish()
    }
}

impl Workspace {
    /// 开启文件监听；重复调用是幂等的。
    pub fn start_watch(&self, tx: ChangeSender) -> Result<(), WorkspaceError> {
        let mut slot = self
            .watcher
            .lock()
            .map_err(|_| WorkspaceError::Other("监听器锁已中毒".into()))?;
        if slot.is_some() {
            return Ok(());
        }
        let handle = watcher::start(&self.root, self.root.clone(), tx)?;
        *slot = Some(handle);
        Ok(())
    }

    /// 停止文件监听。
    pub fn stop_watch(&self) {
        if let Ok(mut slot) = self.watcher.lock() {
            *slot = None;
        }
    }

    /// 当前是否在监听。
    pub fn is_watching(&self) -> bool {
        self.watcher.lock().map(|s| s.is_some()).unwrap_or(false)
    }

    /// 绝对路径 → 相对路径。
    pub fn relative(&self, abs: &Path) -> String {
        self.guard.to_relative(abs)
    }
}

/// 工作区注册表。
#[derive(Debug, Default)]
pub struct WorkspaceManager {
    items: RwLock<HashMap<String, Arc<Workspace>>>,
}

impl WorkspaceManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 打开工作区。
    ///
    /// 同一个根目录重复打开时返回既有句柄，避免出现两个 `workspace_id`
    /// 指向同一目录、各自跑一套监听器。
    pub fn open(&self, path: &str) -> Result<Arc<Workspace>, WorkspaceError> {
        let guard = PathGuard::new(PathBuf::from(path))?;
        let root = guard.root().to_path_buf();

        if let Some(existing) = self.find_by_root(&root) {
            return Ok(existing);
        }

        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());

        let workspace = Arc::new(Workspace {
            id: uuid::Uuid::new_v4().to_string(),
            root,
            name,
            guard,
            watcher: Mutex::new(None),
        });

        let mut items = self
            .items
            .write()
            .map_err(|_| WorkspaceError::Other("工作区注册表锁已中毒".into()))?;
        // 双检：并发 open 时后到的那个复用先到的句柄。
        if let Some(existing) = items.values().find(|w| w.root == workspace.root) {
            return Ok(Arc::clone(existing));
        }
        items.insert(workspace.id.clone(), Arc::clone(&workspace));
        Ok(workspace)
    }

    /// 按句柄取工作区。
    pub fn get(&self, id: &str) -> Result<Arc<Workspace>, WorkspaceError> {
        self.items
            .read()
            .map_err(|_| WorkspaceError::Other("工作区注册表锁已中毒".into()))?
            .get(id)
            .map(Arc::clone)
            .ok_or_else(|| WorkspaceError::NotOpened(id.to_string()))
    }

    /// 关闭工作区；顺带停掉监听器。
    pub fn close(&self, id: &str) -> Result<(), WorkspaceError> {
        let mut items = self
            .items
            .write()
            .map_err(|_| WorkspaceError::Other("工作区注册表锁已中毒".into()))?;
        if let Some(workspace) = items.remove(id) {
            workspace.stop_watch();
        }
        Ok(())
    }

    /// 关闭全部工作区，返回关闭数量。
    pub fn close_all(&self) -> usize {
        let mut items = match self.items.write() {
            Ok(items) => items,
            Err(_) => return 0,
        };
        let count = items.len();
        for (_, workspace) in items.drain() {
            workspace.stop_watch();
        }
        count
    }

    /// 已打开的工作区数量。
    pub fn count(&self) -> usize {
        self.items.read().map(|i| i.len()).unwrap_or(0)
    }

    /// 所有句柄（用于 `core.stats`）。
    pub fn ids(&self) -> Vec<String> {
        self.items
            .read()
            .map(|i| i.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn find_by_root(&self, root: &Path) -> Option<Arc<Workspace>> {
        let items = self.items.read().ok()?;
        items.values().find(|w| w.root == root).map(Arc::clone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ai-ide-mgr-{}-{}-{}",
            tag,
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    #[test]
    fn open_then_get() {
        let root = temp_dir("open");
        let manager = WorkspaceManager::new();
        let ws = manager.open(root.to_str().unwrap()).unwrap();

        assert_eq!(manager.count(), 1);
        assert_eq!(ws.name, root.file_name().unwrap().to_string_lossy());
        assert!(manager.get(&ws.id).is_ok());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn opening_same_root_twice_reuses_handle() {
        let root = temp_dir("reuse");
        let manager = WorkspaceManager::new();
        let a = manager.open(root.to_str().unwrap()).unwrap();
        let b = manager.open(root.to_str().unwrap()).unwrap();

        assert_eq!(a.id, b.id, "同一目录不应产生两个工作区");
        assert_eq!(manager.count(), 1);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn close_removes_workspace() {
        let root = temp_dir("close");
        let manager = WorkspaceManager::new();
        let ws = manager.open(root.to_str().unwrap()).unwrap();
        manager.close(&ws.id).unwrap();

        assert_eq!(manager.count(), 0);
        assert!(manager.get(&ws.id).is_err());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unknown_id_is_an_error() {
        let manager = WorkspaceManager::new();
        assert!(manager.get("nope").is_err());
    }

    #[test]
    fn cannot_open_missing_directory() {
        let manager = WorkspaceManager::new();
        let err = manager
            .open(r"C:\definitely\not\here\ai-ide")
            .expect_err("不存在的目录应被拒绝");
        assert!(matches!(err, WorkspaceError::Path(_)));
    }

    #[test]
    fn close_all_clears_registry() {
        let a = temp_dir("all-a");
        let b = temp_dir("all-b");
        let manager = WorkspaceManager::new();
        manager.open(a.to_str().unwrap()).unwrap();
        manager.open(b.to_str().unwrap()).unwrap();

        assert_eq!(manager.count(), 2);
        assert_eq!(manager.close_all(), 2);
        assert_eq!(manager.count(), 0);

        fs::remove_dir_all(&a).ok();
        fs::remove_dir_all(&b).ok();
    }
}
