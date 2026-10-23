//! MCP 工具注入（M7 切片二）。
//!
//! 把「已连接 Server 发现到的工具」包成 [`Tool`]，增量挂到 agent 的工具注册表上。
//! 三条硬约束：
//!
//! 1. **命名空间**：注入名必须是 `mcp__<server>__<tool>`。裸名会跟核心八工具撞车
//!    （没准哪天某个 MCP Server 也提供一个 `read`），而撞车在注册表里是静默失效，
//!    排查成本极高。
//! 2. **原样透传**：`description` 与 `inputSchema` 一个字都不改。它们在两侧都是
//!    JSON Schema，任何「规整」都会让模型看到的世界与 Server 的真实校验错位。
//! 3. **闸门复用**：本桥**不自己做任何权限判断**。工具一旦进注册表，就与核心八工具
//!    走同一条 `PermissionGate` → 审批 → `dispatch` 的路，没有旁路。

use ai_ide_agent::{AgentError, Tool, ToolContext, ToolOutcome, ToolRegistry};
use ai_ide_protocol::dto::McpToolInfo;
use serde_json::Value;
use std::sync::Arc;

use crate::mcp_manager::{result_text, McpManager};

/// Server 未提供说明时的兜底文案。
const FALLBACK_DESCRIPTION: &str = "MCP 工具（Server 未提供说明）";

/// 一个 MCP 工具在 agent 注册表里的代表。
///
/// 无状态约束在这里由「持 [`Arc<McpManager>`] + 只读字段」满足：连接由管理器统一
/// 持有，工具实例自己不存连接，因此断连后旧实例只会拿到「未连接」错误。
pub struct McpToolBridge {
    manager: Arc<McpManager>,
    info: McpToolInfo,
}

impl McpToolBridge {
    /// 用工具声明构造。`manager` 用来在调用时取当前连接（断连后自然失效）。
    pub fn new(manager: Arc<McpManager>, info: McpToolInfo) -> Self {
        Self { manager, info }
    }

    /// 工具声明（测试与外壳展示用）。
    pub fn info(&self) -> &McpToolInfo {
        &self.info
    }
}

impl Tool for McpToolBridge {
    // `Tool::name` 是注册表的键，必须是命名空间名而不是裸名——这里刻意与字段
    // `info.name`（Server 侧原名）不同，故关掉这条 getter 命名检查。
    #[allow(clippy::misnamed_getters)]
    fn name(&self) -> &str {
        &self.info.namespaced_name
    }

    fn description(&self) -> &str {
        self.info
            .description
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .unwrap_or(FALLBACK_DESCRIPTION)
    }

    fn parameters(&self) -> Value {
        self.info.input_schema.clone()
    }

    fn call(&self, _ctx: &ToolContext<'_>, args: &Value) -> Result<ToolOutcome, AgentError> {
        match self
            .manager
            .call_tool_blocking(&self.info.server, &self.info.name, args.clone())
        {
            Ok(result) => Ok(ToolOutcome::new(result_text(&result))),
            // MCP 层的失败一律折成**可回灌**的工具错误：模型可以换个参数、换个工具，
            // 或者自己向用户报「Server 没连上」。唯一例外是路径越界——但 MCP 工具
            // 的路径由 Server 自己管，内核这边没有路径可判，所以这里不会出现 fatal。
            Err(err) => Err(AgentError::ToolFailed {
                tool: self.info.namespaced_name.clone(),
                message: err.to_string(),
            }),
        }
    }
}

impl std::fmt::Debug for McpToolBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolBridge")
            .field("name", &self.info.namespaced_name)
            .field("server", &self.info.server)
            .finish()
    }
}

/// 把当前所有已连接 Server 的工具折成待注入的工具实例。
///
/// 顺序与管理器一致（Server 名 → 工具名升序），便于日志与测试断言稳定。
pub fn bridges(manager: &Arc<McpManager>) -> Vec<Box<dyn Tool>> {
    manager
        .tool_infos(None)
        .into_iter()
        .map(|info| Box::new(McpToolBridge::new(Arc::clone(manager), info)) as Box<dyn Tool>)
        .collect()
}

/// 在既有注册表上追加 MCP 工具。
///
/// `manager` 为 `None`（内核尚未装载 MCP 管理器）时原样返回，不产生任何空壳工具。
#[must_use]
pub fn inject(base: ToolRegistry, manager: Option<&Arc<McpManager>>) -> ToolRegistry {
    match manager {
        Some(manager) => base.with_tools(bridges(manager)),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_ide_mcp::{McpConfig, McpServerConfig, StdioServerConfig};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn info(server: &str, tool: &str) -> McpToolInfo {
        McpToolInfo {
            name: tool.to_string(),
            namespaced_name: format!("mcp__{server}__{tool}"),
            server: server.to_string(),
            description: Some("列出目录".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        }
    }

    fn manager_with_fake_server() -> Arc<McpManager> {
        let mut config = McpConfig::default();
        config.servers.insert(
            "fs".to_string(),
            McpServerConfig::Stdio(StdioServerConfig {
                name: "fs".to_string(),
                command: "definitely-not-a-real-binary".to_string(),
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: None,
            }),
        );
        Arc::new(McpManager::new(config))
    }

    /// 名字、说明、schema 三样必须原样透传：模型看到的世界等于 Server 声明的世界。
    #[test]
    fn declaration_is_passed_through_verbatim() {
        let manager = manager_with_fake_server();
        let expected = info("fs", "list_dir");
        let bridge = McpToolBridge::new(Arc::clone(&manager), expected.clone());
        assert_eq!(bridge.name(), "mcp__fs__list_dir");
        assert_eq!(bridge.description(), "列出目录");
        assert_eq!(bridge.parameters(), expected.input_schema);
        let spec = bridge.spec();
        assert_eq!(spec.name, "mcp__fs__list_dir");
        assert_eq!(spec.parameters["required"], json!(["path"]));
    }

    /// Server 没写说明时给一句兜底，绝不吐空串——空说明会让模型靠名字瞎猜。
    #[test]
    fn missing_description_falls_back() {
        let manager = manager_with_fake_server();
        let mut info = info("fs", "list_dir");
        info.description = None;
        let bridge = McpToolBridge::new(manager, info);
        assert_eq!(bridge.description(), FALLBACK_DESCRIPTION);
    }

    /// 断连状态下调用：必须回**可回灌**的 `ToolFailed`，而不是致命错误——
    /// 一次没调通不该把整个 agent 循环带走。
    #[test]
    fn call_without_connection_is_recoverable() {
        let manager = manager_with_fake_server();
        let bridge = McpToolBridge::new(Arc::clone(&manager), info("fs", "list_dir"));
        let dir = std::env::temp_dir();
        let workspaces = ai_ide_workspace::WorkspaceManager::new();
        let workspace = workspaces
            .open(&dir.display().to_string())
            .expect("临时目录应当能作为工作区");
        let ctx = ToolContext::new(&workspace);
        let err = bridge.call(&ctx, &json!({ "path": "." })).unwrap_err();
        assert!(!err.is_fatal(), "MCP 调用失败不该终止循环：{err}");
        assert!(matches!(err, AgentError::ToolFailed { .. }), "{err}");
        let text = err.to_string();
        assert!(text.contains("mcp__fs__list_dir"), "{text}");
        assert!(text.contains("尚未连接"), "{text}");
    }

    /// 没连任何 Server 时注入是空操作：注册表一个工具都不多。
    #[test]
    fn inject_with_nothing_connected_is_a_noop() {
        let manager = manager_with_fake_server();
        let registry = inject(ToolRegistry::core(), Some(&manager));
        assert_eq!(registry.len(), ToolRegistry::core().len());
        assert!(registry.get("mcp__fs__list_dir").is_none());
    }

    /// 没给管理器时同样原样返回（内核没装载 MCP 的路径）。
    #[test]
    fn inject_without_manager_keeps_registry() {
        let registry = inject(ToolRegistry::core(), None);
        assert_eq!(registry.len(), ToolRegistry::core().len());
    }
}
