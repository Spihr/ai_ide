//! 插件宿主骨架（参考实现，**未接入内核**）。
//!
//! 状态：`crates/plugin-host` 尚未出现在 `core/Cargo.toml` 的依赖里，
//! 本文件只用于确定类型签名与闸门顺序，编译通过与否都不影响现有构建。
//! 真正的接线动作见 `protocol_patch.md`，那一步必须和 dispatch 分支同时落地，
//! 否则会出现「方法名在表里、dispatch 没有分支」的能力虚报。
//!
//! 设计约束（不可让步）：
//! 1. 宿主在 Core 侧。插件的一切副作用经宿主代理，才能复用 PathGuard /
//!    PermissionGate / 审计落库——放到 Main 侧就绕过了这三件事。
//! 2. 插件方法名不进 `methods::IMPLEMENTED`。统一走 `plugin.call`，
//!    否则 dispatch 的穷举 match 会被插件数量撑爆。
//! 3. 握手必须双向校验：插件少报（清单骗人）和多报（偷偷扩权）都要拒载。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;

/// 插件 API 版本，与 `protocol/src/version.rs` 里的协商逻辑共用同一套比较规则。
pub const PLUGIN_API: (u32, u32) = (1, 0);

pub const FRAME_LIMIT_BYTES: usize = 1024 * 1024;
pub const TOOL_RESULT_LIMIT_BYTES: usize = 64 * 1024;
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// 状态机：只允许这些迁移，别的一律拒绝。
///
/// ```text
/// Installed ──grant──▶ Granted ──spawn+handshake──▶ Ready
///     │                   │                          │
///     │                   └──missing cap──▶ NeedsGrant
///     └──invalid manifest──▶ Incompatible            │
///                                        crash ──────┴──▶ Crashed ──restart──▶ Ready
///                                                          └─over limit─▶ Disabled
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginState {
    Installed,
    NeedsGrant,
    Ready,
    Crashed,
    Disabled,
}

impl PluginState {
    /// 只有 Ready 的插件才能受理调用。这个判断必须集中在一处，
    /// 散在各调用点早晚会漏掉一个，漏掉的那个就是越权入口。
    pub fn accepts_calls(self) -> bool {
        matches!(self, PluginState::Ready)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(rename = "pluginApi")]
    pub plugin_api: String,
    pub main: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub tools: Vec<ToolDecl>,
    #[serde(default)]
    pub commands: Vec<CommandDecl>,
    #[serde(default)]
    pub subscriptions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolDecl {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// `read` / `write` / `exec`，决定审批策略
    #[serde(default = "default_risk")]
    pub risk: String,
    /// 审批 scope 用的参数名（通常是 `path`）。缺失时 scope 退化为整个插件。
    #[serde(default, rename = "pathField")]
    pub path_field: Option<String>,
    /// 原样透传给模型：插件自己定义 schema，宿主不解释、只校验是 object
    pub parameters: Value,
}

fn default_risk() -> String {
    "read".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct CommandDecl {
    pub id: String,
    #[serde(default)]
    pub title: String,
}

/// 宿主依赖的能力接口，全部以 trait 注入，便于单测时替换成假实现。
pub trait HostPorts: Send + Sync {
    fn path_guard(&self) -> &crate::workspace::PathGuard;
    fn permission(&self) -> &Arc<dyn crate::permission::PermissionGate>;
    fn audit(&self, row: AuditRow);
    fn emit_event(&self, event: &str, payload: Value);
}

#[derive(Debug, Serialize)]
pub struct AuditRow {
    pub plugin_id: String,
    pub action: String,
    pub detail: Value,
}

pub struct PluginEntry {
    pub manifest: Manifest,
    pub state: RwLock<PluginState>,
    pub dir: PathBuf,
    pub process: Option<PluginProcess>,
    /// 握手确认后的工具，定义以清单为准（插件不能运行时扩权）
    pub tools: Vec<ToolDecl>,
    pub restarts: u32,
    pub last_error: Option<String>,
}

pub struct PluginHost {
    workspace_root: PathBuf,
    entries: RwLock<HashMap<String, Arc<PluginEntry>>>,
    /// 全名 `plugin.<id>.<tool>` → 归属
    tool_index: RwLock<HashMap<String, ToolRef>>,
    ports: Arc<dyn HostPorts>,
    grants: RwLock<HashMap<String, Vec<String>>>,
}

#[derive(Clone)]
pub struct ToolRef {
    pub plugin_id: String,
    pub tool: ToolDecl,
}

impl PluginHost {
    // ── 生命周期 ─────────────────────────────────────────────────────────

    /// 装载：静态校验 → 能力校验 → spawn → 握手 → 注册工具。
    ///
    /// 注意能力校验在 spawn **之前**：未授权就不给进程，
    /// 避免插件先跑起来再谈条件（那条路上一定会有人写出「先连上再说」的代码）。
    pub async fn load(&self, dir: &Path) -> Result<(), PluginError> {
        let manifest = self.read_manifest(dir).await?;
        let missing: Vec<_> = manifest
            .capabilities
            .iter()
            .filter(|cap| !self.has_grant(&manifest.id, cap))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(PluginError::capability_denied(missing));
        }

        let entry = Arc::new(PluginEntry {
            manifest: manifest.clone(),
            state: RwLock::new(PluginState::Installed),
            dir: dir.to_path_buf(),
            process: None,
            tools: Vec::new(),
            restarts: 0,
            last_error: None,
        });

        let mut process = PluginProcess::spawn(&entry, self.ports.clone()).await?;
        let hello: HelloResult = process
            .request("plugin/initialize", Self::init_params(&manifest), DEFAULT_CALL_TIMEOUT)
            .await?;

        Self::assert_handshake(&manifest, &hello)?;
        // 定义以清单为准：插件上报仅用于确认「实现了」，不允许追加。
        let tools = manifest.tools.clone();
        self.register_tools(&manifest.id, &tools).await?;

        *entry.state.write().await = PluginState::Ready;
        self.entries.write().await.insert(manifest.id.clone(), entry);
        self.ports.emit_event("plugin.state_changed", serde_json::json!({"state": "ready"}));
        Ok(())
    }

    fn assert_handshake(manifest: &Manifest, hello: &HelloResult) -> Result<(), PluginError> {
        if !crate::protocol::version::compatible(hello.plugin_api, PLUGIN_API) {
            return Err(PluginError::api_mismatch(hello.plugin_api));
        }
        let declared: Vec<&str> = manifest.tools.iter().map(|t| t.name.as_str()).collect();
        let reported: Vec<&str> = hello.tools.iter().map(|t| t.name.as_str()).collect();
        // 多报 = 运行时偷偷扩权；少报 = 清单骗人。两者都拒载。
        if let Some(extra) = reported.iter().find(|n| !declared.contains(n)) {
            return Err(PluginError::protocol_violation(format!("未声明的工具：{extra}")));
        }
        if let Some(missing) = declared.iter().find(|n| !reported.contains(n)) {
            return Err(PluginError::protocol_violation(format!("未实现的工具：{missing}")));
        }
        Ok(())
    }

    // ── 调用网关 ─────────────────────────────────────────────────────────

    /// `plugin.call` 的唯一实现。所有插件方法都从这里过，不存在第二条路径。
    pub async fn call(&self, plugin_id: &str, method: &str, params: Value) -> Result<Value, PluginError> {
        let entry = self
            .entries
            .read()
            .await
            .get(plugin_id)
            .cloned()
            .ok_or_else(|| PluginError::not_found(plugin_id))?;

        // ① 状态
        let state = *entry.state.read().await;
        if !state.accepts_calls() {
            return Err(PluginError::crashed(plugin_id, state));
        }

        // ② 方法归属：`tool/` 前缀的方法必须对得上清单里的工具名，
        //    防止插件借 `plugin.call` 调到它没声明的内部方法。
        if method.starts_with("tool/") {
            let tool_name = method.trim_start_matches("tool/");
            let decl = entry
                .tools
                .iter()
                .find(|t| t.name == tool_name)
                .ok_or_else(|| PluginError::not_found(method))?;

            // ③ 审批：risk 决定要不要问人。scope 取 pathField 的值，
            //    一次批准只覆盖那一个文件，不是整个插件。
            let scope = decl
                .path_field
                .as_ref()
                .and_then(|field| params.get("arguments").and_then(|a| a.get(field)))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            self.ports
                .permission()
                .ensure(crate::permission::Request {
                    tool: format!("plugin.{plugin_id}.{tool_name}"),
                    risk: decl.risk.clone(),
                    scope_key: scope,
                })
                .await?;
        }

        // ④⑤ 由子进程层负责：帧上限、超时、结果截断
        let process = entry.process.as_ref().ok_or_else(|| PluginError::crashed(plugin_id, state))?;
        let result = process
            .request(method, params, DEFAULT_CALL_TIMEOUT)
            .await
            .map_err(|e| PluginError::from_process(plugin_id, e))?;

        Ok(truncate_tool_result(result, TOOL_RESULT_LIMIT_BYTES))
    }

    /// 插件 → 宿主的请求入口。这是**唯一**能产生副作用的地方。
    pub async fn on_plugin_request(
        &self,
        plugin_id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, PluginError> {
        // 方法 → 能力的映射写成常量表，缺一行就等于开一个后门。
        let capability = match method {
            "host/fs.read" | "host/fs.list" | "host/fs.stat" | "host/workspace/info" => "fs.read",
            "host/fs.write" => "fs.write",
            "host/search/content" => "search",
            "host/net.fetch" => "net.fetch",
            "host/llm.chat" => "llm.invoke",
            // 日志与进度是纯上报，不产生副作用，不需要能力
            "host/log" | "host/progress" => return self.handle_report(plugin_id, method, params),
            other => return Err(PluginError::method_not_found(other)),
        };

        if !self.has_grant(plugin_id, capability) {
            self.ports.audit(AuditRow {
                plugin_id: plugin_id.to_string(),
                action: "deny".to_string(),
                detail: serde_json::json!({"method": method, "capability": capability}),
            });
            return Err(PluginError::capability_denied(vec![capability.to_string()]));
        }

        // 路径闸门：插件只给相对路径，绝对路径一律拒（它没有知道的必要）
        let rel = params
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(".")
            .to_string();
        let abs = self.ports.path_guard().resolve(&rel, method == "host/fs.write")?;

        let result = self.dispatch_fs(method, abs, params).await?;
        self.ports.audit(AuditRow {
            plugin_id: plugin_id.to_string(),
            action: method.to_string(),
            detail: serde_json::json!({"path": rel}),
        });
        Ok(result)
    }

    async fn dispatch_fs(&self, _method: &str, _abs: PathBuf, _params: Value) -> Result<Value, PluginError> {
        // TODO(plugin-p0)：复用 `crates/workspace` 读写实现，
        // 不要在这里重写一遍——重写就等于多一套行为，早晚与核心工具不一致。
        unimplemented!()
    }

    fn handle_report(&self, plugin_id: &str, _method: &str, _params: Value) -> Result<Value, PluginError> {
        // TODO(plugin-p0)：host/log → `plugin.log` 事件；host/progress → 进度回灌，
        // 需要按 plugin_id 做节流（每秒 N 条），否则一个死循环日志能把 UI 打满。
        let _ = plugin_id;
        Ok(Value::Null)
    }

    async fn register_tools(&self, plugin_id: &str, tools: &[ToolDecl]) -> Result<(), PluginError> {
        let mut index = self.tool_index.write().await;
        for tool in tools {
            let full = format!("plugin.{plugin_id}.{}", tool.name);
            if index.contains_key(&full) {
                return Err(PluginError::tool_conflict(full));
            }
            index.insert(
                full,
                ToolRef {
                    plugin_id: plugin_id.to_string(),
                    tool: tool.clone(),
                },
            );
        }
        Ok(())
    }

    fn has_grant(&self, plugin_id: &str, capability: &str) -> bool {
        // TODO(plugin-p0)：读 `RwLock` 需要 async，签名改为 async 或在启动时快照。
        let _ = (plugin_id, capability);
        false
    }

    fn init_params(manifest: &Manifest) -> Value {
        // 刻意不带 rootPath：插件不需要知道工作区在哪，越界由 PathGuard 兜住。
        serde_json::json!({
            "pluginApi": {"major": PLUGIN_API.0, "minor": PLUGIN_API.1},
            "plugin": {"id": manifest.id, "version": manifest.version},
            "limits": {"maxFrameBytes": FRAME_LIMIT_BYTES, "requestTimeoutMs": 30000}
        })
    }

    async fn read_manifest(&self, _dir: &Path) -> Result<Manifest, PluginError> {
        // TODO(plugin-p0)：读 plugin.json + 校验（id/version/main/capabilities/risk×cap 交叉）
        unimplemented!()
    }
}

/// 工具结果截断：与 `run_command` 的 64 KiB 口径一致，
/// 保证模型看到的结果长度与核心工具同量级。
fn truncate_tool_result(value: Value, limit: usize) -> Value {
    let Some(text) = value.get("content").and_then(Value::as_str) else {
        return value;
    };
    if text.len() <= limit {
        return value;
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    serde_json::json!({
        "content": format!("{}\n\n[输出已截断：原始 {} 字节，上限 {} 字节]", &text[..end], text.len(), limit),
        "truncated": true
    })
}

#[derive(Debug, Deserialize)]
pub struct HelloResult {
    #[serde(rename = "pluginApi")]
    pub plugin_api: (u32, u32),
    pub tools: Vec<HelloTool>,
}

#[derive(Debug, Deserialize)]
pub struct HelloTool {
    pub name: String,
}

/// 占位：真实实现见 `process.rs`（tokio::process + NDJSON 分帧 + 在途请求表）。
pub struct PluginProcess;

impl PluginProcess {
    pub async fn spawn(_entry: &PluginEntry, _ports: Arc<dyn HostPorts>) -> Result<Self, PluginError> {
        unimplemented!()
    }

    pub async fn request(&mut self, _method: &str, _params: Value, _timeout: Duration) -> Result<Value, PluginError> {
        unimplemented!()
    }
}

#[derive(Debug)]
pub struct PluginError;

impl PluginError {
    pub fn not_found(_: &str) -> Self { Self }
    pub fn crashed(_: &str, _: PluginState) -> Self { Self }
    pub fn capability_denied(_: Vec<String>) -> Self { Self }
    pub fn api_mismatch(_: (u32, u32)) -> Self { Self }
    pub fn protocol_violation(_: String) -> Self { Self }
    pub fn tool_conflict(_: String) -> Self { Self }
    pub fn method_not_found(_: &str) -> Self { Self }
    pub fn from_process(_: &str, _: impl std::fmt::Debug) -> Self { Self }
}
