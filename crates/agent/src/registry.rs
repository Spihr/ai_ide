//! 工具注册表：声明（给模型看）、派发（按名调用）、结果回传（拼成消息）。
//!
//! 与既有 JSON-RPC 协议保持同一套风格：工具名与文档一致、参数是 JSON Schema、
//! 失败按 [`AgentError`] 分类映射协议错误码。注册表本身**无状态**——工作区通过
//! [`ToolContext`] 传入，因此可以被多个会话共享。

use crate::error::AgentError;
use crate::message;
use crate::tools;
use ai_ide_protocol::dto::{ChatMessage, ToolSpec};
use ai_ide_workspace::{PathGuard, Workspace};
use serde_json::Value;
use std::path::Path;

/// 工具执行时可访问的工作区上下文。
#[derive(Debug, Clone, Copy)]
pub struct ToolContext<'a> {
    /// 路径闸门：所有路径都必须先过它。
    pub guard: &'a PathGuard,
    /// 工作区根目录（需要遍历时用）。
    pub root: &'a Path,
    /// 工作区句柄，用于回填协议 DTO。
    pub workspace_id: &'a str,
}

impl<'a> ToolContext<'a> {
    /// 从已打开的工作区构造。
    pub fn new(workspace: &'a Workspace) -> Self {
        Self {
            guard: &workspace.guard,
            root: &workspace.root,
            workspace_id: &workspace.id,
        }
    }

    /// 把绝对路径折成「相对工作区根」的展示路径。
    ///
    /// 统一用 `/` 分隔：回灌给模型的路径不该因为宿主是 Windows 就变成 `src\\a.ts`，
    /// 否则模型复制这条路径再传给 `edit` 时很容易在 JSON 里忘记转义反斜杠。
    pub fn display_path(&self, abs: &Path) -> String {
        let relative = abs.strip_prefix(self.root).unwrap_or(abs);
        let text = relative.to_string_lossy().replace('\\', "/");
        if text.is_empty() {
            ".".to_string()
        } else {
            text
        }
    }
}

/// 工具一次成功执行的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    /// 回灌给模型的正文。已是最终形态，拼装时不再加工。
    pub content: String,
}

impl ToolOutcome {
    /// 包装一段结果正文。
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
        }
    }

    /// 拼成 `role = tool` 消息。
    pub fn into_message(self, call_id: &str) -> ChatMessage {
        message::tool_message(call_id, self.content)
    }
}

/// 一个可注册的工具。
///
/// 实现者**无状态**：工作区经 [`ToolContext`] 传入，所以工具实例可以常驻注册表。
pub trait Tool: Send + Sync {
    /// 工具名，需与协议/文档里的名字一致。
    fn name(&self) -> &str;

    /// 给模型看的一句话说明。
    ///
    /// 返回 `&str` 而不是 `&'static str`：M7 起注册表里会有**运行期才出现**的工具
    /// （MCP Server 发现出来的），它们的说明存在实例字段里，没有 `'static` 生命周期。
    fn description(&self) -> &str;

    /// 参数 JSON Schema。
    fn parameters(&self) -> Value;

    /// 执行。
    fn call(&self, ctx: &ToolContext<'_>, args: &Value) -> Result<ToolOutcome, AgentError>;

    /// 协议声明。
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: Some(self.description().to_string()),
            parameters: self.parameters(),
        }
    }
}

/// 工具注册表。
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    /// M4 的八个核心工具：`read` / `list` / `grep` / `write` / `edit` / `create`
    /// 六个读写工具，加切片二的 `run_command` 与 `apply_diff`。
    ///
    /// 权限规则表与裁决点属后续切片：这里注册的是「能力」，不是「许可」——
    /// `run_command` 自身只做命令黑名单与路径闸门（安全底线），审批由上层裁决。
    pub fn core() -> Self {
        Self {
            tools: vec![
                Box::new(tools::read::ReadTool),
                Box::new(tools::list::ListTool),
                Box::new(tools::grep::GrepTool),
                Box::new(tools::write::WriteTool),
                Box::new(tools::edit::EditTool),
                Box::new(tools::create::CreateTool),
                Box::new(tools::run_command::RunCommandTool),
                Box::new(tools::apply_diff::ApplyDiffTool),
            ],
        }
    }

    /// 追加一个工具（后续切片与测试用）。
    #[must_use]
    pub fn with_tool(mut self, tool: Box<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// 批量追加工具（M7：一次注入一个 Server 的全部 MCP 工具）。
    ///
    /// 重名不覆盖也不报错：注册表按名线性查找，重名只会让后注册的永远取不到。
    /// 所以注入方必须保证名字带命名空间（`mcp__<server>__<tool>`），这里只如实全收。
    #[must_use]
    pub fn with_tools(mut self, tools: Vec<Box<dyn Tool>>) -> Self {
        self.tools.extend(tools);
        self
    }

    /// 全部工具声明，按注册顺序。
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|tool| tool.spec()).collect()
    }

    /// 工具名清单。
    pub fn names(&self) -> Vec<&str> {
        self.tools.iter().map(|tool| tool.name()).collect()
    }

    /// 工具数量。
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// 是否为空注册表。
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// 按名取工具。
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(|tool| &**tool as &dyn Tool)
    }

    /// 按名派发：解析参数 JSON → 调工具。
    ///
    /// 未注册的工具与坏参数都是**可回灌**的（见 [`AgentError::is_fatal`]），
    /// 模型改一版参数就能继续。
    pub fn dispatch(
        &self,
        ctx: &ToolContext<'_>,
        name: &str,
        arguments: &str,
    ) -> Result<ToolOutcome, AgentError> {
        let tool = self
            .get(name)
            .ok_or_else(|| AgentError::UnknownTool(name.to_string()))?;
        let args = parse_arguments(name, arguments)?;
        tool.call(ctx, &args)
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::core()
    }
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools", &self.names())
            .finish()
    }
}

/// 解析工具参数。
///
/// 空串按空对象处理：上游在「无参调用」时常给空串，这不是错误。
fn parse_arguments(tool: &str, arguments: &str) -> Result<Value, AgentError> {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let value: Value = serde_json::from_str(trimmed).map_err(|err| AgentError::BadArgs {
        tool: tool.to_string(),
        reason: format!("参数不是合法 JSON：{err}"),
    })?;
    if !value.is_object() {
        return Err(AgentError::BadArgs {
            tool: tool.to_string(),
            reason: "参数应为 JSON 对象".into(),
        });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Sandbox;

    #[test]
    fn core_registry_declares_the_eight_tools() {
        let registry = ToolRegistry::core();
        let names = registry.names();
        assert_eq!(names.len(), 8);
        for expected in [
            "read",
            "list",
            "grep",
            "write",
            "edit",
            "create",
            "run_command",
            "apply_diff",
        ] {
            assert!(names.contains(&expected), "缺少工具 {expected}");
        }
        // 权限裁决、审批 UI、diff 视图都还没落地，注册表里不该混进别的东西。
        assert_eq!(
            names,
            vec![
                "read",
                "list",
                "grep",
                "write",
                "edit",
                "create",
                "run_command",
                "apply_diff"
            ],
            "注册顺序即声明顺序，后续切片要追加也只能追加在末尾"
        );
    }

    #[test]
    fn specs_are_json_schema_objects() {
        for spec in ToolRegistry::core().specs() {
            assert!(!spec.name.is_empty());
            assert!(spec.description.is_some(), "{} 缺少说明", spec.name);
            assert_eq!(
                spec.parameters["type"], "object",
                "{} 的参数应是 JSON Schema 对象",
                spec.name
            );
            assert!(spec.parameters["properties"].is_object());
        }
    }

    #[test]
    fn dispatch_runs_a_registered_tool() {
        let sandbox = Sandbox::new("registry-dispatch");
        sandbox.write("a.txt", "hello\n");

        let outcome = ToolRegistry::core()
            .dispatch(&sandbox.ctx(), "read", r#"{"path":"a.txt"}"#)
            .expect("read 应成功");
        assert!(outcome.content.contains("hello"));
    }

    #[test]
    fn unknown_tool_is_recoverable() {
        let sandbox = Sandbox::new("registry-unknown");
        let err = ToolRegistry::core()
            .dispatch(&sandbox.ctx(), "web_search", "{}")
            .unwrap_err();
        assert!(matches!(err, AgentError::UnknownTool(_)));
        assert!(!err.is_fatal(), "模型点错工具名，应回灌让它改");
    }

    #[test]
    fn bad_arguments_are_recoverable() {
        let sandbox = Sandbox::new("registry-badargs");
        let registry = ToolRegistry::core();

        let err = registry
            .dispatch(&sandbox.ctx(), "read", "{not json")
            .unwrap_err();
        assert!(matches!(err, AgentError::BadArgs { .. }));
        assert!(!err.is_fatal());

        let err = registry
            .dispatch(&sandbox.ctx(), "read", "[1,2]")
            .unwrap_err();
        assert!(matches!(err, AgentError::BadArgs { .. }));
    }

    #[test]
    fn empty_arguments_mean_empty_object() {
        let sandbox = Sandbox::new("registry-emptyargs");
        sandbox.write("src/a.ts", "export {}\n");
        let outcome = ToolRegistry::core()
            .dispatch(&sandbox.ctx(), "list", "   ")
            .expect("无参调用应被当成空对象");
        assert!(outcome.content.contains("<tree path=\".\""));
        assert!(outcome.content.contains("dir\tsrc/"));
    }

    #[test]
    fn get_returns_none_for_unknown_name() {
        let registry = ToolRegistry::core();
        assert!(registry.get("edit").is_some());
        assert!(registry.get("nope").is_none());
        assert_eq!(registry.len(), 8);
        assert!(!registry.is_empty());
    }
}
