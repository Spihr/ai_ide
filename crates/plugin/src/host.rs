//! 插件宿主（P2）：装载 = 起进程 + 握手 + 一致性校验；卸载 = 关停 + 工具下架。
//!
//! 判定链严格按 docs/14 §6：**授权齐备 → 起进程 → 握手 → 校验 `pluginApi` →
//! 校验工具集合与清单一致**。任一环失败都要把刚起的进程收掉，绝不留半装载的
//! 状态——「进程活着但工具没上架」比彻底失败更难排查。
//!
//! 宿主不认识账本：状态该落 `ready` 还是 `crashed` 由 `crates/core` 决定，
//! 这里只如实回答「进程还在不在」「stderr 最后几行是什么」。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::loader::{PluginProcess, RequestError, SpawnError, SpawnOptions};
use crate::manifest::{
    namespaced_tool, Manifest, ManifestTool, ValidatedManifest, HOST_PLUGIN_API_MAJOR,
    HOST_PLUGIN_API_MINOR,
};

/// 宿主 → 插件的握手方法名（docs/14 §6.2）。
pub const HANDSHAKE_METHOD: &str = "plugin/handshake";
/// P1 之前 demo 用的旧名。握手时若回 `-32601` 自动退回它，兼容老包。
pub const LEGACY_HANDSHAKE_METHOD: &str = "plugin/initialize";
/// JSON-RPC 的「方法不存在」。
pub const METHOD_NOT_FOUND: i64 = -32601;
/// 握手总预算。插件拉起 + 首次握手都在这一步，给足 15 s。
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// 插件声明的 API 版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiVersion {
    pub major: u32,
    #[serde(default)]
    pub minor: u32,
}

impl std::fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// 握手回包里的工具声明。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub risk: Option<ai_ide_protocol::PluginToolRisk>,
    /// JSON Schema；兼容 `inputSchema` 这种常见别名。
    #[serde(default, alias = "inputSchema", alias = "input_schema")]
    pub parameters: Option<Value>,
}

/// 握手回包里的命令声明。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeCommand {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
}

/// 插件对握手的应答。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Handshake {
    /// 兼容两种写法：对象 `{major,minor}` 或字符串 `"1.0"`。
    #[serde(default)]
    pub plugin_api: Option<Value>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub tools: Vec<HandshakeTool>,
    #[serde(default)]
    pub commands: Vec<HandshakeCommand>,
    #[serde(default)]
    pub subscriptions: Vec<String>,
}

impl Handshake {
    /// 解析出的 API 版本；写错格式时为 `None`。
    pub fn api_version(&self) -> Option<ApiVersion> {
        let value = self.plugin_api.as_ref()?;
        if let Some(object) = value.as_object() {
            let major = object.get("major")?.as_u64()? as u32;
            let minor = object.get("minor").and_then(Value::as_u64).unwrap_or(0) as u32;
            return Some(ApiVersion { major, minor });
        }
        let text = value.as_str()?;
        let mut parts = text.split('.');
        let major = parts.next()?.trim().parse::<u32>().ok()?;
        let minor = parts
            .next()
            .and_then(|part| part.trim().parse::<u32>().ok())
            .unwrap_or(0);
        Some(ApiVersion { major, minor })
    }

    /// 握手声明的工具名（保持回包顺序）。
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.iter().map(|tool| tool.name.clone()).collect()
    }
}

/// 装载失败的全部情形。`crates/core` 按它选 61xx。
#[derive(Debug, Clone)]
pub enum LoadError {
    /// 安装目录不见了（账本有记录、磁盘没有）。
    NotInstalled(String),
    /// 清单要的能力还没授权（6103）。
    MissingCapabilities { missing: Vec<String> },
    /// 拉不起进程（解释器缺失 / 入口丢失）。
    Spawn(SpawnError),
    /// 握手超时（6105）。
    Timeout { stage: String, timeout: Duration },
    /// 进程在握手期间挂了（6102）。
    Crashed {
        code: Option<i32>,
        tail: Vec<String>,
    },
    /// 协议违规 / 工具集合对不上（6106）。
    Protocol { message: String },
    /// `pluginApi` 不兼容（6104）。
    ApiMismatch { plugin: String, host: String },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled(dir) => write!(f, "插件安装目录不存在：{dir}"),
            Self::MissingCapabilities { missing } => {
                write!(f, "能力尚未授权：{}", missing.join(", "))
            }
            Self::Spawn(error) => write!(f, "{error}"),
            Self::Timeout { stage, timeout } => {
                write!(f, "{stage} 超时（{} ms）", timeout.as_millis())
            }
            Self::Crashed { code, .. } => match code {
                Some(code) => write!(f, "插件进程在握手期间退出（退出码 {code}）"),
                None => write!(f, "插件进程在握手期间退出"),
            },
            Self::Protocol { message } => write!(f, "插件协议错误：{message}"),
            Self::ApiMismatch { plugin, host } => {
                write!(f, "pluginApi {plugin} 与宿主 {host} 不兼容")
            }
        }
    }
}

/// 装载成功的结果。
#[derive(Debug, Clone)]
pub struct Loaded {
    pub handshake: Handshake,
    /// 合并后的工具声明（清单顺序，描述/风险/schema 以握手为准、缺省回落清单）。
    pub tools: Vec<HandshakeTool>,
    /// 带命名空间的工具全名，用于注册与下架。
    pub namespaced_tools: Vec<String>,
    pub pid: Option<u32>,
    /// 本次是复用已在跑的进程（`plugin.load` 幂等）。
    pub reused: bool,
}

/// 一次装载所需的输入。
pub struct LoadSpec<'a> {
    pub plugin_id: &'a str,
    /// `<data_dir>/plugins/<id>`。
    pub install_dir: &'a Path,
    pub validated: &'a ValidatedManifest,
    /// 清单声明要的能力。
    pub requested: Vec<String>,
    /// 账本里已授权的能力。
    pub granted: Vec<String>,
    /// 握手参数里的 `workspace`（未打开工作区时为 `null`）。
    pub workspace: Value,
}

/// 一个被判死的插件（供上层落 `crashed` + `last_error`）。
#[derive(Debug, Clone)]
pub struct Crashed {
    pub plugin_id: String,
    /// 它原本占用的工具全名——上层拿它做下架。
    pub namespaced_tools: Vec<String>,
    pub exit_code: Option<i32>,
    pub tail: Vec<String>,
    pub violation: Option<String>,
}

struct Entry {
    handshake: Handshake,
    tools: Vec<HandshakeTool>,
    namespaced: Vec<String>,
    process: Arc<PluginProcess>,
}

/// 已装载插件表。
#[derive(Default)]
pub struct Host {
    loaded: Mutex<HashMap<String, Entry>>,
}

impl Host {
    pub fn new() -> Self {
        Self::default()
    }

    /// 装载（幂等）：已装载且进程活着时直接复用，不重复起进程。
    pub async fn load(&self, spec: LoadSpec<'_>) -> Result<Loaded, LoadError> {
        let plugin_id = spec.plugin_id.to_string();

        if let Some(existing) = self.alive(&plugin_id) {
            return Ok(Loaded {
                reused: true,
                ..existing
            });
        }
        // 残壳（进程已死但还没被清走）：先关停再重来，避免两个进程抢同一份数据。
        if let Some(stale) = self.take(&plugin_id) {
            stale.process.shutdown().await;
        }

        let missing: Vec<String> = spec
            .requested
            .iter()
            .filter(|capability| !spec.granted.contains(capability))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(LoadError::MissingCapabilities { missing });
        }

        let manifest = &spec.validated.manifest;
        let entry = spec.install_dir.join(&spec.validated.entry);
        if !entry.is_file() {
            return Err(LoadError::NotInstalled(entry.display().to_string()));
        }

        let process = PluginProcess::spawn(SpawnOptions {
            plugin_id: &plugin_id,
            install_dir: spec.install_dir,
            entry: &entry,
        })
        .await
        .map_err(LoadError::Spawn)?;

        let params = handshake_params(manifest, spec.install_dir, spec.workspace);
        let handshake = match handshake(&process, params).await {
            Ok(handshake) => handshake,
            Err(error) => {
                process.shutdown().await;
                return Err(error);
            }
        };

        // 版本与工具集合都对齐后才允许入表：入表就意味着「工具可路由」。
        if let Err(error) = verify(manifest, &handshake) {
            process.shutdown().await;
            return Err(error);
        }

        let tools = merge_tools(manifest, &handshake);
        let namespaced = manifest
            .tools
            .iter()
            .map(|tool| namespaced_tool(&manifest.id, &tool.name))
            .collect::<Vec<_>>();
        let pid = process.pid();
        let loaded = Loaded {
            handshake: handshake.clone(),
            tools: tools.clone(),
            namespaced_tools: namespaced.clone(),
            pid,
            reused: false,
        };
        if let Ok(mut guard) = self.loaded.lock() {
            guard.insert(
                plugin_id,
                Entry {
                    handshake,
                    tools,
                    namespaced,
                    process,
                },
            );
        }
        Ok(loaded)
    }

    /// 卸载：关停进程并返回被下架的工具全名。未装载时返回 `None`。
    pub async fn unload(&self, plugin_id: &str) -> Option<Vec<String>> {
        let entry = self.take(plugin_id)?;
        entry.process.shutdown().await;
        Some(entry.namespaced)
    }

    /// 该插件是否已装载且进程还活着。
    pub fn is_running(&self, plugin_id: &str) -> bool {
        self.alive(plugin_id).is_some()
    }

    /// 已在跑的工具全名（未装载为空）。
    pub fn running_tools(&self, plugin_id: &str) -> Vec<String> {
        self.alive(plugin_id)
            .map(|loaded| loaded.namespaced_tools)
            .unwrap_or_default()
    }

    /// 当前所有在跑的插件 id。
    pub fn running_ids(&self) -> Vec<String> {
        self.loaded
            .lock()
            .map(|guard| {
                guard
                    .iter()
                    .filter(|(_, entry)| !entry.process.has_exited())
                    .map(|(id, _)| id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 取走所有已崩溃的插件（崩溃即下架）。上层据返回值落 `crashed`。
    pub fn take_crashed(&self) -> Vec<Crashed> {
        let mut crashed = Vec::new();
        let Ok(mut guard) = self.loaded.lock() else {
            return crashed;
        };
        let dead: Vec<String> = guard
            .iter()
            .filter(|(_, entry)| entry.process.has_exited())
            .map(|(id, _)| id.clone())
            .collect();
        for plugin_id in dead {
            if let Some(entry) = guard.remove(&plugin_id) {
                crashed.push(Crashed {
                    plugin_id,
                    namespaced_tools: entry.namespaced,
                    exit_code: None,
                    tail: entry.process.stderr_tail(),
                    violation: entry.process.violation(),
                });
            }
        }
        crashed
    }

    /// 关停所有插件（内核退出前调用；即使这里漏了，作业对象也会兜住）。
    pub async fn shutdown_all(&self) {
        let entries: Vec<Entry> = match self.loaded.lock() {
            Ok(mut guard) => guard.drain().map(|(_, entry)| entry).collect(),
            Err(_) => Vec::new(),
        };
        for entry in entries {
            entry.process.shutdown().await;
        }
    }

    fn alive(&self, plugin_id: &str) -> Option<Loaded> {
        let guard = self.loaded.lock().ok()?;
        let entry = guard.get(plugin_id)?;
        if entry.process.has_exited() {
            return None;
        }
        Some(Loaded {
            handshake: entry.handshake.clone(),
            tools: entry.tools.clone(),
            namespaced_tools: entry.namespaced.clone(),
            pid: entry.process.pid(),
            reused: true,
        })
    }

    fn take(&self, plugin_id: &str) -> Option<Entry> {
        self.loaded.lock().ok()?.remove(plugin_id)
    }
}

/// 组装握手参数。
fn handshake_params(manifest: &Manifest, install_dir: &Path, workspace: Value) -> Value {
    json!({
        "pluginApi": { "major": HOST_PLUGIN_API_MAJOR, "minor": HOST_PLUGIN_API_MINOR },
        "host": {
            "name": "ai-ide-core",
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
        },
        "plugin": {
            "id": manifest.id,
            "version": manifest.version,
            "installDir": install_dir.display().to_string(),
        },
        "workspace": workspace,
    })
}

/// 发握手请求；`plugin/handshake` 不存在时退回 `plugin/initialize`。
async fn handshake(process: &PluginProcess, params: Value) -> Result<Handshake, LoadError> {
    let reply = match process
        .request(HANDSHAKE_METHOD, params.clone(), HANDSHAKE_TIMEOUT)
        .await
    {
        Ok(value) => value,
        Err(RequestError::Plugin { code, .. }) if code == METHOD_NOT_FOUND => {
            tracing::debug!(
                plugin_id = %process.id(),
                "插件未实现 plugin/handshake，退回 plugin/initialize"
            );
            match process
                .request(LEGACY_HANDSHAKE_METHOD, params, HANDSHAKE_TIMEOUT)
                .await
            {
                Ok(value) => value,
                Err(error) => {
                    return Err(map_request_error(LEGACY_HANDSHAKE_METHOD, process, error).await)
                }
            }
        }
        Err(error) => return Err(map_request_error(HANDSHAKE_METHOD, process, error).await),
    };

    serde_json::from_value(reply).map_err(|error| LoadError::Protocol {
        message: format!("握手回包不是合法的握手结构：{error}"),
    })
}

async fn map_request_error(stage: &str, process: &PluginProcess, error: RequestError) -> LoadError {
    match error {
        RequestError::Plugin { code, message } => LoadError::Protocol {
            message: format!("{stage} 被插件拒绝：{code} {message}"),
        },
        RequestError::Timeout { timeout, .. } => LoadError::Timeout {
            stage: stage.to_string(),
            timeout,
        },
        RequestError::Exited { tail } => LoadError::Crashed {
            code: exit_code(process).await,
            tail,
        },
        RequestError::Protocol(message) => LoadError::Protocol { message },
        RequestError::Closed => LoadError::Crashed {
            code: exit_code(process).await,
            tail: process.stderr_tail(),
        },
    }
}

/// 取退出码：进程已退出时 `try_wait` 立刻返回，轮询一次即可。
async fn exit_code(process: &PluginProcess) -> Option<i32> {
    if !process.has_exited() {
        return None;
    }
    process.wait_exit(Duration::from_millis(0)).await.flatten()
}

/// 校验 `pluginApi` 与工具集合。
fn verify(manifest: &Manifest, handshake: &Handshake) -> Result<(), LoadError> {
    let host = format!("{HOST_PLUGIN_API_MAJOR}.{HOST_PLUGIN_API_MINOR}");
    let Some(api) = handshake.api_version() else {
        return Err(LoadError::Protocol {
            message: "握手回包缺少 pluginApi（或格式不合法）".to_string(),
        });
    };
    if api.major != HOST_PLUGIN_API_MAJOR {
        return Err(LoadError::ApiMismatch {
            plugin: api.to_string(),
            host,
        });
    }

    let declared: HashSet<&str> = manifest
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut extra: Vec<String> = Vec::new();
    let mut duplicates: Vec<String> = Vec::new();
    for tool in &handshake.tools {
        if tool.name.trim().is_empty() {
            return Err(LoadError::Protocol {
                message: "握手工具集合里有空名字".to_string(),
            });
        }
        if !seen.insert(tool.name.as_str()) {
            duplicates.push(tool.name.clone());
            continue;
        }
        if !declared.contains(tool.name.as_str()) {
            extra.push(tool.name.clone());
        }
    }
    if !duplicates.is_empty() {
        return Err(LoadError::Protocol {
            message: format!("握手工具集合有重复项：{}", duplicates.join(", ")),
        });
    }
    let missing: Vec<String> = manifest
        .tools
        .iter()
        .filter(|tool| !seen.contains(tool.name.as_str()))
        .map(|tool| tool.name.clone())
        .collect();
    if missing.is_empty() && extra.is_empty() {
        return Ok(());
    }
    Err(LoadError::Protocol {
        message: format!(
            "握手工具集合与清单不一致：清单有而未声明 [{}]；声明了但清单没有 [{}]",
            missing.join(", "),
            extra.join(", ")
        ),
    })
}

/// 以清单顺序合并工具声明：握手补描述/风险/schema，缺省回落清单。
fn merge_tools(manifest: &Manifest, handshake: &Handshake) -> Vec<HandshakeTool> {
    let by_name: HashMap<&str, &HandshakeTool> = handshake
        .tools
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect();
    manifest
        .tools
        .iter()
        .map(|declared: &ManifestTool| {
            let announced = by_name.get(declared.name.as_str());
            HandshakeTool {
                name: declared.name.clone(),
                description: announced
                    .and_then(|tool| tool.description.clone())
                    .or_else(|| declared.description.clone()),
                risk: announced.and_then(|tool| tool.risk).or(Some(declared.risk)),
                parameters: announced
                    .and_then(|tool| tool.parameters.clone())
                    .or_else(|| Some(declared.parameters.clone())),
            }
        })
        .collect()
}
