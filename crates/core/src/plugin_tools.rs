//! 插件工具注入（P3）。
//!
//! 把「本次运行前已装载且进程还活着」的插件工具包成 [`Tool`]，续在 MCP 工具
//! 后面挂到 agent 的注册表上。三条硬约束：
//!
//! 1. **命名空间**：注入名一律用宿主算好的 `plugin__<id>__<tool>`，不用插件
//!    声明的裸名。注册表按名线性查找、重名静默失效，前缀是唯一防撞车手段。
//! 2. **声明原样透传**：`parameters` 直接用宿主 `tool_catalog()` 给的 JSON Schema，
//!    不在这里「补齐」或「精简」——模型看到的世界必须等于插件真实校验的世界。
//! 3. **闸门复用**：本桥不做任何权限判断。工具进注册表后与核心八工具走同一条
//!    `PermissionGate` 路：`risk=read` 由上层登记进只读直通集合，`risk=write`
//!    落到既有「未登记 → 审批」分支，没有旁路。

use ai_ide_agent::{AgentError, Tool, ToolContext, ToolOutcome, ToolRegistry};
use ai_ide_plugin::host::Host;
use ai_ide_plugin::host::{CallError, RunningTool};
use ai_ide_protocol::PluginToolRisk;
use serde_json::Value;
use std::sync::Arc;

/// 插件没写说明时的兜底文案。
const FALLBACK_DESCRIPTION: &str = "插件工具（插件未提供说明）";

/// 说明前缀：模型据此知道这条能力来自外部插件而不是内置工具。
fn describe(plugin: &str, description: &str) -> String {
    let text = description.trim();
    if text.is_empty() {
        format!("来源：插件 {plugin}；{FALLBACK_DESCRIPTION}")
    } else {
        format!("来源：插件 {plugin}；{text}")
    }
}

/// 一个插件工具在 agent 注册表里的代表。
///
/// 只持 [`Arc<Host>`] 与一份声明快照：插件被卸载或崩溃后，快照仍在，但真正调用
/// 会在宿主侧拿到 `NotRunning`——注册表不需要为「插件没了」维护失效逻辑。
pub struct PluginToolBridge {
    host: Arc<Host>,
    info: RunningTool,
    description: String,
}

impl PluginToolBridge {
    /// 用宿主给出的工具声明构造。
    pub fn new(host: Arc<Host>, info: RunningTool) -> Self {
        let description = describe(&info.plugin_id, &info.description);
        Self {
            host,
            info,
            description,
        }
    }

    /// 工具声明（测试与外壳展示用）。
    pub fn info(&self) -> &RunningTool {
        &self.info
    }

    /// 该工具是否声明为只读（决定是否进权限闸门的直通集合）。
    pub fn is_read(&self) -> bool {
        self.info.risk == PluginToolRisk::Read
    }
}

impl Tool for PluginToolBridge {
    // 注册表的键是命名空间名，与字段 `info.name`（插件内原名）刻意不同。
    #[allow(clippy::misnamed_getters)]
    fn name(&self) -> &str {
        &self.info.namespaced_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.info.parameters.clone()
    }

    fn call(&self, _ctx: &ToolContext<'_>, args: &Value) -> Result<ToolOutcome, AgentError> {
        let host = Arc::clone(&self.host);
        let plugin_id = self.info.plugin_id.clone();
        let tool = self.info.name.clone();
        let arguments = args.clone();
        let name = self.info.namespaced_name.clone();

        // `Tool::call` 是同步的，而 `Host::call_tool` 要等插件回包（异步）。这里与
        // MCP 桥同口径：把等待搬进独立线程 + 独立 current_thread 运行时。**不能**
        // 用 `Handle::block_on`——调用方就在 tokio worker 上，那样会把调度器一起堵死。
        let joined = std::thread::Builder::new()
            .name(format!("plugin-call-{plugin_id}"))
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                Ok::<_, std::io::Error>(
                    runtime.block_on(host.call_tool(&plugin_id, &tool, arguments)),
                )
            })
            .map_err(|err| AgentError::ToolFailed {
                tool: name.clone(),
                message: format!("创建插件调用线程失败：{err}"),
            })?;

        let reply = match joined.join() {
            Ok(Ok(Ok(reply))) => reply,
            Ok(Ok(Err(err))) => return Err(call_error(&name, err)),
            Ok(Err(err)) => {
                return Err(AgentError::ToolFailed {
                    tool: name,
                    message: format!("创建插件调用运行时失败：{err}"),
                })
            }
            // 线程 panic 说明本 crate 有 bug：按可回灌的工具错误上报，别把循环带走。
            Err(_) => {
                return Err(AgentError::ToolFailed {
                    tool: name,
                    message: "插件调用线程异常退出".to_string(),
                })
            }
        };

        Ok(ToolOutcome::new(result_text(&reply)))
    }
}

impl std::fmt::Debug for PluginToolBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginToolBridge")
            .field("name", &self.info.namespaced_name)
            .field("plugin", &self.info.plugin_id)
            .field("risk", &self.info.risk)
            .finish()
    }
}

/// 宿主失败一律折成**可回灌**的工具错误。
///
/// 与 MCP 同口径：插件崩了不该终止整次运行——模型可以换工具、换参数，或者如实告诉
/// 用户「这个插件没了」。唯一的例外本可以留给「路径越界」，但插件工具的路径由插件
/// 自己管，内核没有路径可判，所以这里不会产出 fatal。
fn call_error(tool: &str, err: CallError) -> AgentError {
    let message = match &err {
        CallError::Timeout { timeout, .. } => format!("{}（超时 {}ms）", err, timeout.as_millis()),
        _ => err.to_string(),
    };
    AgentError::ToolFailed {
        tool: tool.to_string(),
        message,
    }
}

/// 插件回包转文本：字符串原样，其余序列化成 JSON。
///
/// 不在这里做二次包装（不加「插件返回：」之类的前缀）——回灌给模型的内容越接近
/// 插件原意越好，前缀会被模型当成数据的一部分引用。
pub(crate) fn result_text(reply: &Value) -> String {
    match reply {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

/// 把当前所有运行中插件的工具折成待注入的工具实例。
///
/// 顺序由 [`Host::tool_catalog`] 保证（按 `namespaced_name` 升序），这里不再排序，
/// 免得两处排序口径不一致。
pub fn bridges(host: &Arc<Host>) -> Vec<Box<dyn Tool>> {
    host.tool_catalog()
        .into_iter()
        .map(|info| Box::new(PluginToolBridge::new(Arc::clone(host), info)) as Box<dyn Tool>)
        .collect()
}

/// 运行中插件的**只读**工具全名：权限闸门要用它把 `risk=read` 划进直通集合。
///
/// 只挑 `Read`：`risk=write` 必须留在「未登记 → 审批」那条路上，漏一个就是
/// 绕过审批的写盘口子。
pub fn read_tool_names(host: &Arc<Host>) -> Vec<String> {
    host.tool_catalog()
        .into_iter()
        .filter(|tool| tool.risk == PluginToolRisk::Read)
        .map(|tool| tool.namespaced_name)
        .collect()
}

/// 在既有注册表上追加插件工具。
///
/// `host` 为 `None` 时原样返回：内核精简构建 / 单测里没有宿主，不该凭空造空壳工具。
#[must_use]
pub fn inject(base: ToolRegistry, host: Option<&Arc<Host>>) -> ToolRegistry {
    match host {
        Some(host) => base.with_tools(bridges(host)),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_ide_plugin::host::LoadSpec;
    use ai_ide_plugin::install::inspect;
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;

    /// 最小的插件替身：任何 `plugin/tool-invoke` 都回显工具名与参数。
    const ECHO_TOOL: &str = r#"
        const readline = require('node:readline')
        const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
        rl.on('line', (line) => {
          if (!line.trim()) return
          const frame = JSON.parse(line)
          const reply = frame.method === 'plugin/handshake'
            ? { pluginApi: '1.0', name: 'demo', version: '0.1.0',
                capabilities: ['fs.write'],
                tools: [
                  { name: 'peek', description: '看一眼（只读）', risk: 'read',
                    parameters: { type: 'object', properties: {} } },
                  { name: 'poke', description: '动一下（会写盘）', risk: 'write',
                    parameters: { type: 'object', properties: {} } }
                ] }
            : { method: frame.method, tool: (frame.params || {}).tool,
                arguments: (frame.params || {}).arguments }
          process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: frame.id, result: reply }) + '\n')
        })
    "#;

    const MANIFEST: &str = r#"{
      "id": "demo",
      "name": "Demo",
      "version": "0.1.0",
      "pluginApi": "1.0",
      "main": "index.cjs",
      "capabilities": ["fs.write"],
      "tools": [
        { "name": "peek", "description": "看一眼（只读）", "risk": "read",
          "parameters": { "type": "object", "properties": {} } },
        { "name": "poke", "description": "动一下（会写盘）", "risk": "write",
          "parameters": { "type": "object", "properties": {} } }
      ],
      "commands": [],
      "subscriptions": [],
      "timeoutMs": 30000
    }"#;

    fn node_ready() -> bool {
        std::process::Command::new(
            std::env::var(ai_ide_plugin::loader::NODE_ENV).unwrap_or_else(|_| "node".to_string()),
        )
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
    }

    fn scratch(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|gap| gap.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "aiide-plugin-tools-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    /// 装一份最小插件并装载它，返回宿主句柄。
    async fn loaded_host(dir: &Path) -> Arc<Host> {
        std::fs::write(dir.join("plugin.json"), MANIFEST).expect("写清单");
        let entry = dir.join("index.cjs");
        std::fs::write(&entry, ECHO_TOOL).expect("写脚本");
        let validated = inspect(dir).expect("清单应合法");
        let host = Arc::new(Host::new());
        host.load(LoadSpec {
            plugin_id: "demo",
            install_dir: dir,
            validated: &validated,
            requested: Vec::new(),
            granted: Vec::new(),
            workspace: Value::Null,
        })
        .await
        .expect("装载应成功");
        host
    }

    /// 名字、说明前缀、schema 三样必须对：名字带命名空间、说明标明来源。
    #[test]
    fn declaration_uses_namespaced_name_and_source_prefix() {
        let info = RunningTool {
            plugin_id: "demo".to_string(),
            name: "peek".to_string(),
            namespaced_name: "plugin__demo__peek".to_string(),
            description: "看一眼".to_string(),
            risk: PluginToolRisk::Read,
            parameters: json!({ "type": "object", "properties": { "q": { "type": "string" } } }),
        };
        let bridge = PluginToolBridge::new(Arc::new(Host::new()), info.clone());
        assert_eq!(bridge.name(), "plugin__demo__peek");
        assert_eq!(bridge.description(), "来源：插件 demo；看一眼");
        assert!(bridge.is_read());
        assert_eq!(bridge.parameters(), info.parameters);
        assert_eq!(bridge.spec().name, "plugin__demo__peek");
    }

    /// 插件没写说明时也要给一句可读文案，绝不吐空串。
    #[test]
    fn missing_description_falls_back_but_keeps_source() {
        let info = RunningTool {
            plugin_id: "demo".to_string(),
            name: "peek".to_string(),
            namespaced_name: "plugin__demo__peek".to_string(),
            description: "   ".to_string(),
            risk: PluginToolRisk::Read,
            parameters: json!({ "type": "object" }),
        };
        let bridge = PluginToolBridge::new(Arc::new(Host::new()), info);
        let text = bridge.description();
        assert!(text.contains("来源：插件 demo"), "{text}");
        assert!(text.contains(FALLBACK_DESCRIPTION), "{text}");
    }

    /// 没给宿主时注入是空操作（内核精简构建路径）。
    #[test]
    fn inject_without_host_keeps_registry() {
        let registry = inject(ToolRegistry::core(), None);
        assert_eq!(registry.len(), ToolRegistry::core().len());
    }

    /// 注入后注册表能查到 `plugin__<id>__<tool>`；卸载后目录与注册表里都消失。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn inject_after_load_and_gone_after_unload() {
        if !node_ready() {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let dir = scratch("catalog");
        let host = loaded_host(&dir).await;

        let registry = inject(ToolRegistry::core(), Some(&host));
        assert_eq!(
            registry.len(),
            ToolRegistry::core().len() + 2,
            "应注入 2 个插件工具"
        );
        assert!(registry.get("plugin__demo__peek").is_some());
        assert!(registry.get("plugin__demo__poke").is_some());
        assert_eq!(
            read_tool_names(&host),
            vec!["plugin__demo__peek".to_string()],
            "只有 risk=read 的工具进直通集合"
        );

        let names = host.unload("demo").await.expect("卸载应成功");
        assert_eq!(names.len(), 2);
        assert!(host.tool_catalog().is_empty(), "卸载后目录应为空");
        // 每次运行现建注册表：下一轮运行自然拿不到已卸载的工具。
        let registry = inject(ToolRegistry::core(), Some(&host));
        assert!(registry.get("plugin__demo__peek").is_none());
        assert!(registry.get("plugin__demo__poke").is_none());

        host.shutdown_all().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 走一遍完整调用：回包原样成为工具正文；插件不在时回可回灌错误。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn call_round_trip_and_unloaded_is_recoverable() {
        if !node_ready() {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let dir = scratch("call");
        let host = loaded_host(&dir).await;
        let registry = inject(ToolRegistry::core(), Some(&host));
        let workspaces = ai_ide_workspace::WorkspaceManager::new();
        let workspace = workspaces
            .open(&dir.display().to_string())
            .expect("临时目录应能作为工作区");
        let ctx = ToolContext::new(&workspace);

        let out = registry
            .dispatch(&ctx, "plugin__demo__peek", r#"{"q":"你好"}"#)
            .expect("调用应成功");
        assert!(out.content.contains("\"tool\":\"peek\""), "{}", out.content);
        assert!(out.content.contains("你好"), "{}", out.content);

        host.unload("demo").await.expect("卸载应成功");
        let err = registry
            .dispatch(&ctx, "plugin__demo__peek", "{}")
            .expect_err("卸载后调用应失败");
        assert!(!err.is_fatal(), "插件缺失不该终止循环：{err}");
        assert!(matches!(err, AgentError::ToolFailed { .. }), "{err}");
        assert!(err.to_string().contains("plugin__demo__peek"), "{err}");

        host.shutdown_all().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
