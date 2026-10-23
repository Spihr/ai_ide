//! 把插件工具适配成内核 `Tool`，让 Agent 的 ToolRegistry 无感收录。
//!
//! 关键点：`Tool` 的签名不该因为插件而改动。插件工具与核心工具在
//! ToolRegistry 眼里必须长得一样，否则 `agent/registry.rs` 会分叉成两条路径，
//! 而分叉出来的那条一定缺少审批或审计。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::tools::{Tool, ToolCall, ToolOutcome};

pub struct PluginTool {
    host: Arc<crate::plugin_host::PluginHost>,
    plugin_id: String,
    /// 已带前缀：`plugin.<id>.<name>`
    full_name: String,
    description: String,
    parameters: Value,
    risk: String,
    path_field: Option<String>,
}

#[async_trait]
impl Tool for PluginTool {
    fn name(&self) -> &str {
        &self.full_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        // 原样透传插件 schema，宿主不重写、不裁剪。
        // 重写 schema 会让模型看到的参数与实际接受的不一致。
        self.parameters.clone()
    }

    /// 审批信息：把插件声明的 risk 翻译成内核的审批需求。
    ///
    /// 这一步不能省。若插件工具一率 `None`，PermissionGate 就完全看不见它，
    /// 插件的写操作会在「无审批」状态下滑过去——这是最容易漏的一条。
    fn approval(&self, call: &ToolCall) -> Option<crate::permission::Request> {
        let scope = self
            .path_field
            .as_ref()
            .and_then(|f| call.args.get(f))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        Some(crate::permission::Request {
            tool: self.full_name.clone(),
            risk: self.risk.clone(),
            scope_key: scope,
        })
    }

    async fn call(&self, call: &ToolCall) -> Result<ToolOutcome, crate::tools::ToolError> {
        let result = self
            .host
            .call(&self.plugin_id, "tool/invoke", serde_json::json!({
                "tool": self.full_name.rsplit('.').next().unwrap_or_default(),
                "arguments": call.args,
            }))
            .await
            .map_err(|e| crate::tools::ToolError::new(e.code(), e.to_string()))?;

        Ok(ToolOutcome {
            content: result
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            truncated: result.get("truncated").and_then(Value::as_bool).unwrap_or(false),
            data: result.get("data").cloned(),
        })
    }
}

/// 注册入口。命名强制 `plugin.<id>.<name>` 的理由：
/// 1. 与核心工具天然不冲突，`ToolRegistry::with_tool` 的重复检查才有效；
/// 2. 模型从名字就能看出这是第三方能力，出错时更容易归因；
/// 3. 卸载时按前缀批量摘除，不用维护额外的映射表。
pub fn register_plugin_tools(registry: &mut crate::tools::ToolRegistry, host: Arc<crate::plugin_host::PluginHost>) {
    for (full_name, reference) in host.tool_index_snapshot() {
        let tool = PluginTool {
            host: host.clone(),
            plugin_id: reference.plugin_id.clone(),
            full_name: full_name.clone(),
            description: reference.tool.description.clone(),
            parameters: reference.tool.parameters.clone(),
            risk: reference.tool.risk.clone(),
            path_field: reference.tool.path_field.clone(),
        };
        // TODO(plugin-p0)：冲突时不要 panic——插件崩溃不该带走整个 Core。
        // 正确做法是拒载该插件并回报 6008。
        registry.with_tool(Box::new(tool));
    }
}
