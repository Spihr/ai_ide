//! 单元测试用的临时工作区沙箱。
//!
//! 每个 [`Sandbox`] 持有一个临时目录与一个已打开的工作区句柄，析构时清理目录。
//! 集成测试（`tests/`）用不了 `cfg(test)` 里的东西，那边自带一份等价的精简实现。

use ai_ide_workspace::{Workspace, WorkspaceManager};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::error::AgentError;
use crate::registry::{Tool, ToolContext, ToolOutcome};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// 临时目录 + 已打开的工作区。
pub(crate) struct Sandbox {
    dir: PathBuf,
    workspace: Arc<Workspace>,
}

impl Sandbox {
    /// 建一个临时工作区。
    pub(crate) fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ai-ide-agent-{}-{}-{}",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let manager = WorkspaceManager::new();
        let workspace = manager
            .open(&dir.display().to_string())
            .expect("打开临时工作区");
        Self { dir, workspace }
    }

    /// 工具上下文。
    pub(crate) fn ctx(&self) -> ToolContext<'_> {
        ToolContext::new(&self.workspace)
    }

    /// 绝对路径。
    pub(crate) fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    /// 直接落盘（绕开工具，用于铺测试数据）。
    pub(crate) fn write(&self, rel: &str, content: &str) {
        let path = self.path(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("建父目录");
        }
        std::fs::write(path, content).expect("写测试文件");
    }

    /// 直接读盘（用于断言工具真的写进去了）。
    pub(crate) fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path(rel)).expect("读测试文件")
    }

    /// 文件是否存在。
    pub(crate) fn exists(&self, rel: &str) -> bool {
        self.path(rel).exists()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

/// 调一个工具，省掉重复的 ctx 样板。
pub(crate) fn call(
    tool: &dyn Tool,
    sandbox: &Sandbox,
    args: Value,
) -> Result<ToolOutcome, AgentError> {
    tool.call(&sandbox.ctx(), &args)
}
