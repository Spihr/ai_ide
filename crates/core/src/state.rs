//! 内核运行期状态。
//!
//! M0 只保存两样东西：进程启动时刻（用于上报 uptime）与取消登记表。
//! 后续里程碑会把工作区句柄、索引、会话、Provider 配置等挂到这里。

use crate::agent_api::AgentHub;
use crate::llm_api::StreamHub;
use crate::llm_runtime::{default_store_path, LlmRuntime};
use crate::mcp_manager::McpManager;
use crate::peer::Peer;
use ai_ide_git::GitService;
use ai_ide_indexer::FileIndex;
use ai_ide_store::Store;
use ai_ide_terminal::TerminalManager;
use ai_ide_workspace::{Workspace, WorkspaceError, WorkspaceManager};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// 全局状态。
///
/// M0 只保存进程启动时刻与取消登记表；M1 起挂上工作区注册表、文件索引缓存
/// 与对端句柄（用于主动推送 `workspace.changed` / `workspace.indexed` 事件）。
pub struct AppState {
    started_at: Instant,
    /// 已请求取消、但尚未被对应处理器消费的请求 id。
    cancelled: Mutex<HashSet<String>>,
    /// 已打开的工作区。协议里每个工作区都有独立 id，注册表天然支持多例。
    workspaces: WorkspaceManager,
    /// 各工作区的文件路径索引，键为 `workspace_id`。
    indices: Mutex<HashMap<String, Arc<FileIndex>>>,
    /// 当前活动工作区；协议中不带 `workspace_id` 的方法默认作用于它。
    active: Mutex<Option<String>>,
    /// 对端句柄，用于主动推送事件。单测里不注入，事件静默丢弃。
    peer: Mutex<Option<Arc<Peer>>>,
    /// 模型接入层状态（M2：Provider 目录 / 注册表 / 内存密钥）。
    llm: LlmRuntime,
    /// 在途流式对话的取消句柄表（M2）。放在状态里是因为「取消」是一条独立报文，
    /// 需要按 `stream_id` 找到那条流。
    streams: Arc<StreamHub>,
    /// 在途 agent 运行的登记表（M4）。与 `streams` 同理：`agent.stop` / `agent.decide`
    /// 都是独立报文，需要按 `run_id` / `approval_id` 找到那条运行或那条待决询问。
    agents: Arc<AgentHub>,
    /// 终端会话表（M6 切片一）。PTY 子进程与读线程都在 `crates/terminal` 里，
    /// 状态里只留管理器句柄：`terminal.*` 是独立报文，和 agent / stream 一样要按
    /// 会话 id 找到对应实例。
    terminals: Arc<TerminalManager>,
    /// Git 仓库句柄缓存（M6 切片三）。按工作区根缓存 `GitRepo`：每次 `git.status`
    /// 都重新 `rev-parse` 一遍仓库根是白花钱，而工作区关闭时缓存必须失效
    /// （同一个路径可能被关掉后重新 `init` 成另一个仓库），见 `ws::close`。
    git: Arc<GitService>,
    /// 会话与消息库（M5）。进程启动时连一次，此后只读句柄。
    ///
    /// 用 `OnceLock` 而不是 `Mutex<Option<..>>`：它只写一次，写入发生在开始读 stdin
    /// 之前；读侧因此不必上锁，`session.*` 也就不会因为拿锁而排在同一条队列里。
    store: OnceLock<Store>,
    /// MCP 连接管理器（M7 切片二）。同样是「只装一次」的进程级资源：管理器内部
    /// 已用 `RwLock` 管多 Server 的活连接，这里只保证句柄唯一。
    ///
    /// 用 `OnceLock` 而非字段直挂，是为了让「内核没装载 MCP」（单测、精简构建）
    /// 成为一个**显式状态**：`mcp.*` 会如实回空表 / 8001，而不是 panic 或假装成功。
    mcp: OnceLock<Arc<McpManager>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            cancelled: Mutex::new(HashSet::new()),
            workspaces: WorkspaceManager::new(),
            indices: Mutex::new(HashMap::new()),
            active: Mutex::new(None),
            peer: Mutex::new(None),
            // 用默认落盘路径起步；单测用 `with_llm_runtime` 换成临时目录 + 替身传输。
            llm: LlmRuntime::new(default_store_path()),
            streams: Arc::new(StreamHub::new()),
            agents: Arc::new(AgentHub::new()),
            terminals: Arc::new(TerminalManager::new()),
            git: Arc::new(GitService::new()),
            store: OnceLock::new(),
            mcp: OnceLock::new(),
        }
    }

    /// 进程已运行时长（毫秒）。
    pub fn uptime_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    /// 模型接入层状态：Provider 目录、注册表与内存密钥（M2）。
    pub fn llm(&self) -> &LlmRuntime {
        &self.llm
    }

    /// 在途流式对话的取消句柄表（M2）。
    pub fn streams(&self) -> Arc<StreamHub> {
        Arc::clone(&self.streams)
    }

    /// 在途 agent 运行的登记表（M4）。
    pub fn agent(&self) -> Arc<AgentHub> {
        Arc::clone(&self.agents)
    }

    /// 终端会话管理器（M6 切片一）。事件通道由 `run()` 取走后统一转发。
    pub fn terminal(&self) -> Arc<TerminalManager> {
        Arc::clone(&self.terminals)
    }

    /// Git 仓库句柄缓存（M6 切片三）。
    pub fn git(&self) -> Arc<GitService> {
        Arc::clone(&self.git)
    }

    /// 装上会话库（M5）。重复调用保留第一次装上的那份——库句柄是进程级资源，
    /// 换掉它只会让已建立的连接泄漏。
    pub fn install_store(&self, store: Store) {
        let _ = self.store.set(store);
    }

    /// 会话库；启动时未能连接（目录不可写 / 迁移失败）则为 `None`，
    /// 此时 `session.*` 如实返回 5002 而不是假装成功。
    pub fn store(&self) -> Option<&Store> {
        self.store.get()
    }

    /// 装上 MCP 连接管理器（M7 切片二）。重复调用保留第一次装上的那份——
    /// 换掉它会让已建立的 Server 连接失去归属（子进程变成孤儿）。
    pub fn install_mcp(&self, manager: Arc<McpManager>) {
        let _ = self.mcp.set(manager);
    }

    /// MCP 连接管理器；未装载时为 `None`，此时 `mcp.*` 如实回空表或 8001。
    pub fn mcp(&self) -> Option<&Arc<McpManager>> {
        self.mcp.get()
    }

    /// 换上一个现成的模型接入运行时。
    ///
    /// 落盘目录与 HTTP 传输都在 [`LlmRuntime`] 构造时确定，所以单测用它把
    /// 存储挪到临时目录、把传输换成测试替身，整条 `provider.*` 链路即可离线验收。
    pub fn with_llm_runtime(mut self, llm: LlmRuntime) -> Self {
        self.llm = llm;
        self
    }

    /// 登记一条取消请求。
    pub fn mark_cancelled(&self, id: &str) {
        if let Ok(mut set) = self.cancelled.lock() {
            set.insert(id.to_string());
        }
    }

    /// 该请求是否已被取消。
    pub fn is_cancelled(&self, id: &str) -> bool {
        self.cancelled
            .lock()
            .map(|set| set.contains(id))
            .unwrap_or(false)
    }

    /// 消费掉取消登记（请求处理结束时调用，避免登记表无限增长）。
    pub fn clear_cancellation(&self, id: &str) {
        if let Ok(mut set) = self.cancelled.lock() {
            set.remove(id);
        }
    }

    /// 当前待消费的取消登记数量（测试用）。
    pub fn pending_cancellations(&self) -> usize {
        self.cancelled.lock().map(|set| set.len()).unwrap_or(0)
    }

    // -----------------------------------------------------------------------
    // 工作区与索引（M1）
    // -----------------------------------------------------------------------

    /// 工作区注册表。
    pub fn workspaces(&self) -> &WorkspaceManager {
        &self.workspaces
    }

    /// 注入对端句柄；注入后事件才能推给主进程。
    pub fn attach_peer(&self, peer: Arc<Peer>) {
        if let Ok(mut slot) = self.peer.lock() {
            *slot = Some(peer);
        }
    }

    /// 对端句柄；未注入时为 `None`。
    pub fn peer(&self) -> Option<Arc<Peer>> {
        self.peer.lock().ok().and_then(|slot| slot.clone())
    }

    /// 记为当前活动工作区。
    pub fn set_active(&self, workspace_id: &str) {
        if let Ok(mut slot) = self.active.lock() {
            *slot = Some(workspace_id.to_string());
        }
    }

    /// 清掉活动标记（仅当它正是给定的工作区）。
    pub fn clear_active(&self, workspace_id: &str) {
        if let Ok(mut slot) = self.active.lock() {
            if slot.as_deref() == Some(workspace_id) {
                *slot = None;
            }
        }
    }

    /// 当前活动工作区 id。
    pub fn active_id(&self) -> Option<String> {
        self.active.lock().ok().and_then(|slot| slot.clone())
    }

    /// 取工作区：给了 id 就按 id 取，没给就退回活动工作区。
    ///
    /// 协议里 `workspace.tree` 带 `workspace_id`，而 `read_file` / `write_file`
    /// 这类高频方法不带——它们默认作用于活动工作区，路径越界仍由 `PathGuard` 拦。
    pub fn workspace(&self, workspace_id: Option<&str>) -> Result<Arc<Workspace>, WorkspaceError> {
        let id = match workspace_id {
            Some(id) if !id.trim().is_empty() => id.to_string(),
            _ => self
                .active_id()
                .ok_or_else(|| WorkspaceError::NotOpened("尚未打开任何工作区".to_string()))?,
        };
        self.workspaces.get(&id)
    }

    /// 取已建好的索引。
    pub fn index(&self, workspace_id: &str) -> Option<Arc<FileIndex>> {
        self.indices.lock().ok()?.get(workspace_id).map(Arc::clone)
    }

    /// 缓存索引。
    pub fn put_index(&self, workspace_id: &str, index: Arc<FileIndex>) {
        if let Ok(mut map) = self.indices.lock() {
            map.insert(workspace_id.to_string(), index);
        }
    }

    /// 丢弃索引（关闭工作区时调用）。
    pub fn drop_index(&self, workspace_id: &str) {
        if let Ok(mut map) = self.indices.lock() {
            map.remove(workspace_id);
        }
    }

    /// 已打开的工作区数量。
    pub fn workspace_count(&self) -> usize {
        self.workspaces.count()
    }
}

/// 当前 Unix 毫秒时间戳。系统时钟异常时回退为 `0`。
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_is_monotonic() {
        let state = AppState::new();
        let first = state.uptime_ms();
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(state.uptime_ms() >= first);
    }

    #[test]
    fn cancellation_roundtrip() {
        let state = AppState::new();
        assert!(!state.is_cancelled("abc"));
        state.mark_cancelled("abc");
        assert!(state.is_cancelled("abc"));
        assert_eq!(state.pending_cancellations(), 1);
        state.clear_cancellation("abc");
        assert!(!state.is_cancelled("abc"));
        assert_eq!(state.pending_cancellations(), 0);
    }

    #[test]
    fn now_ms_is_plausible() {
        // 2020-01-01 之后、2100 之前
        let ts = now_ms();
        assert!(ts > 1_577_836_800_000, "时间戳看起来不对: {ts}");
        assert!(ts < 4_102_444_800_000, "时间戳看起来不对: {ts}");
    }

    /// 终端管理器随状态一起建好，且初始没有任何会话：
    /// `terminal.list` 在没人建过会话时必须回空表，而不是报「管理器缺失」。
    #[test]
    fn terminal_manager_starts_empty() {
        let state = AppState::new();
        assert_eq!(state.terminal().count(), 0);
        assert!(state.terminal().list().is_empty());
        // 两个句柄指向同一份会话表。
        let first = state.terminal();
        let second = state.terminal();
        assert!(std::sync::Arc::ptr_eq(&first, &second));
    }

    /// 没装库的状态必须自曝其短：`session.*` 靠这个 `None` 报 5002。
    #[test]
    fn store_is_absent_until_installed() {
        let state = AppState::new();
        assert!(state.store().is_none());
    }
}
