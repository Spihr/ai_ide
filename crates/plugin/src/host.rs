//! 插件宿主（P2）：装载 = 起进程 + 握手 + 一致性校验；卸载 = 关停 + 工具下架。
//!
//! 判定链严格按 docs/14 §6：**授权齐备 → 起进程 → 握手 → 校验 `pluginApi` →
//! 校验工具集合与清单一致**。任一环失败都要把刚起的进程收掉，绝不留半装载的
//! 状态——「进程活着但工具没上架」比彻底失败更难排查。
//!
//! 宿主不认识账本：状态该落 `ready` 还是 `crashed` 由 `crates/core` 决定，
//! 这里只如实回答「进程还在不在」「stderr 最后几行是什么」。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::events::{subscribed, EventSink, PushOutcome, EVENT_METHOD, PLUGIN_STATE_CHANGED};
use crate::host_api::HostApi;
use crate::loader::{PluginProcess, RequestError, SpawnError, SpawnOptions, EXITED_REPLY_CODE};
use crate::manifest::{
    namespaced_tool, Manifest, ManifestCommand, ManifestTool, ValidatedManifest,
    HOST_PLUGIN_API_MAJOR, HOST_PLUGIN_API_MINOR,
};

/// 宿主 → 插件的握手方法名（docs/14 §6.2）。
pub const HANDSHAKE_METHOD: &str = "plugin/handshake";
/// P1 之前 demo 用的旧名。握手时若回 `-32601` 自动退回它，兼容老包。
pub const LEGACY_HANDSHAKE_METHOD: &str = "plugin/initialize";
/// JSON-RPC 的「方法不存在」。
pub const METHOD_NOT_FOUND: i64 = -32601;
/// 握手总预算。插件拉起 + 首次握手都在这一步，给足 15 s。
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// 宿主 → 插件的工具调用方法名（P3 工具注入，docs/14 §6.4）。
pub const TOOL_INVOKE_METHOD: &str = "plugin/tool-invoke";
/// 宿主 → 插件的命令执行方法名（P5 切片二，docs/14 §7「命令与事件」）。
///
/// 与 [`TOOL_INVOKE_METHOD`] 分成两条帧：命令的执行语义是「用户按下去了」，
/// 插件侧可能要弹 UI、可能要长跑，和「模型调一个工具」在插件实现里往往走
/// 不同分支；共用一条帧会逼插件靠参数形状去猜来源。
pub const COMMAND_INVOKE_METHOD: &str = "plugin/command-invoke";
/// 单次工具调用预算。与清单默认值 [`crate::manifest::DEFAULT_TIMEOUT_MS`] 一致；
/// 调用一律走这个常量，避免「插件自己声明的超时」被拿来做无限等待。
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// 同一装载会话内允许的自动重启次数（docs/14 §4.2、验收场景 11）。
///
/// 「会话」从一次成功的 `load` 起算，直到这轮进程彻底停住为止：期间无论重启
/// 成功多少次，累计次数都不会被清零，到顶即停——否则一个每 100 ms 就崩一次的
/// 插件能让内核永远重启下去。
pub const MAX_AUTO_RESTARTS: u32 = 3;

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

impl LoadError {
    /// 确定性失败：清单、协议、版本这类错误与「这一次跑得巧不巧」无关，
    /// 同样的包再来一次必然还是同一个结果，自动重启只会把同一个错误刷三遍。
    ///
    /// 反过来，`Crashed`（进程自己在跑的过程中退出）与 `Timeout`（握手期间
    /// 没回话）都当作非确定性，值得重试。
    pub fn is_deterministic(&self) -> bool {
        matches!(
            self,
            Self::Protocol { .. }
                | Self::ApiMismatch { .. }
                | Self::NotInstalled(_)
                | Self::MissingCapabilities { .. }
                | Self::Spawn(_)
        )
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

/// 一个**当下可路由**的插件工具，供上层注入 agent 工具表（P3）。
///
/// 只在插件活着时出现在 [`Host::tool_catalog`] 里：进程一死，工具就该从模型
/// 视野里消失，否则模型会继续调用一个没有接收方的名字。
#[derive(Debug, Clone, PartialEq)]
pub struct RunningTool {
    pub plugin_id: String,
    /// 插件内 `tools[].name`，调用时原样回填给插件。
    pub name: String,
    /// 喂给模型/注册表的名字：`plugin__<plugin_id>__<name>`。
    pub namespaced_name: String,
    pub description: String,
    pub risk: ai_ide_protocol::PluginToolRisk,
    /// JSON Schema；插件没声明时给一个空对象 schema，避免注入出半残定义。
    pub parameters: Value,
}

/// 一条**当下可触发**的插件命令（P5 切片二）。
///
/// 与 [`RunningTool`] 分开：命令不进 agent 工具表，只进命令面板；两者的
/// 风险档位与 schema 语义相同（都来自清单），但「谁能触发」完全不同——
/// 工具是模型，命令是用户。
#[derive(Debug, Clone, PartialEq)]
pub struct RunningCommand {
    pub plugin_id: String,
    /// 插件内 `commands[].name`，执行时原样回填给插件。
    pub name: String,
    pub title: String,
    pub description: Option<String>,
    pub risk: ai_ide_protocol::PluginToolRisk,
    /// 路径闸门用的入参字段名（见 [`crate::manifest::ManifestCommand::path_field`]）。
    pub path_field: Option<String>,
    /// 入参 JSON Schema；`None` 表示该命令不吃入参。
    pub args: Option<Value>,
}

impl From<(&str, &ManifestCommand)> for RunningCommand {
    fn from((plugin_id, command): (&str, &ManifestCommand)) -> Self {
        Self {
            plugin_id: plugin_id.to_string(),
            name: command.name.clone(),
            title: command.title.clone(),
            description: command.description.clone(),
            risk: command.risk,
            path_field: command.path_field.clone(),
            args: command.args.clone(),
        }
    }
}

/// 一次工具调用的失败原因。`crates/core` 按它选 61xx。
#[derive(Debug, Clone)]
pub enum CallError {
    /// 插件没装载，或进程已经退出（工具已不可路由）。
    NotRunning(String),
    /// 插件在跑，但没声明过这个工具。
    UnknownTool(String),
    /// 插件在跑，但清单里没有这条命令（P5 切片二：归属 6106 的运行时兜底）。
    UnknownCommand(String),
    /// 等回包超时（6105）。
    Timeout { tool: String, timeout: Duration },
    /// 请求途中进程没了（6102）。
    Exited {
        code: Option<i32>,
        tail: Vec<String>,
    },
    /// 插件明确回了错误帧。
    Plugin { code: i64, message: String },
    /// 帧不合协议 / 装载表不可用（6106）。
    Protocol { message: String },
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRunning(plugin_id) => write!(f, "插件未装载或已退出：{plugin_id}"),
            Self::UnknownTool(tool) => write!(f, "插件没有声明工具：{tool}"),
            Self::UnknownCommand(command) => write!(f, "插件没有声明命令：{command}"),
            Self::Timeout { tool, timeout } => {
                write!(f, "调用工具 {tool} 超时（{} ms）", timeout.as_millis())
            }
            Self::Exited { code, .. } => match code {
                Some(code) => write!(f, "插件进程在调用期间退出（退出码 {code}）"),
                None => write!(f, "插件进程在调用期间退出"),
            },
            Self::Plugin { code, message } => write!(f, "插件回错误 {code}：{message}"),
            Self::Protocol { message } => write!(f, "插件协议错误：{message}"),
        }
    }
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

/// 一次装载的全部入参（宿主不查账本，自动重启时只能靠自己留的这份底稿）。
///
/// 不实现 `Debug`：`host` 是 `dyn HostApi`，钉不出有意义的调试串，且调试输出
/// 里也不该出现宿主实现。
#[derive(Clone)]
struct Resume {
    plugin_id: String,
    install_dir: PathBuf,
    /// 入口文件的绝对路径（`install_dir` + 清单里的 `main`）。
    entry: PathBuf,
    manifest: Manifest,
    requested: Vec<String>,
    granted: Vec<String>,
    workspace: Value,
    /// 宿主能力出口；自动重启沿用它，重启后 `host/*` 才不会突然变成 -32601。
    host: Option<Arc<dyn HostApi>>,
}

/// 起进程 + 握手 + 校验之后的成品。
struct Start {
    handshake: Handshake,
    tools: Vec<HandshakeTool>,
    namespaced: Vec<String>,
    process: Arc<PluginProcess>,
}

struct Entry {
    start: Start,
    resume: Resume,
    /// 本装载会话内已经用掉的自动重启次数（重启成功也不清零）。
    restart_attempts: u32,
    /// 装载时冻结的订阅清单（取自**清单**的 `subscriptions`，不是握手回包——
    /// 清单是用户在安装时看到并同意的那一份，运行期改不了）。
    subscriptions: Vec<String>,
    /// 该插件的事件出口。一个插件一条队列，互不影响：慢插件只堵自己。
    events: Arc<EventSink>,
}

impl Entry {
    fn new(start: Start, resume: Resume, restart_attempts: u32) -> Self {
        let sink = Arc::new(EventSink::new(resume.plugin_id.clone()));
        let subscriptions = resume.manifest.subscriptions.clone();
        Self {
            start,
            resume,
            restart_attempts,
            subscriptions,
            events: sink,
        }
    }
}

/// 把一条事件队列抽干，逐条写进插件进程。
///
/// 只在队列里已有事件时被拉起，抽干即退出（见 [`EventSink`]）。写失败说明
/// 进程已经不可用：停止投递并清空后续，插件的死活由装载表的状态机去判定，
/// 事件通道不做二次裁决，也不重试。
fn flush_events(sink: &Arc<EventSink>, process: Arc<PluginProcess>) {
    if !sink.begin_drain() {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        sink.end_drain();
        tracing::debug!(
            plugin_id = %sink.plugin_id(),
            "没有异步运行时，事件留在队列里等下次"
        );
        return;
    };
    let queue = Arc::clone(sink);
    runtime.spawn(async move {
        loop {
            let Some(item) = queue.pop() else {
                queue.end_drain();
                // 收尾窗口：刚判空又有事件进来时接着干，避免事件卡在队里等到
                // 下一次 publish 才动（那可能就是几秒之后）。
                if !queue.is_empty() && queue.begin_drain() {
                    continue;
                }
                break;
            };
            let params = json!({ "event": item.event, "payload": item.payload });
            if let Err(error) = process.notify(EVENT_METHOD, &params).await {
                tracing::debug!(
                    plugin_id = %queue.plugin_id(),
                    event = %item.event,
                    %error,
                    "事件写不进插件进程，停止投递"
                );
                queue.end_drain();
                break;
            }
        }
    });
}

/// 一次自动重启的结局。
#[derive(Debug)]
pub enum Restart {
    /// 重启成功，工具重新上架；`attempts` 是该会话累计用掉的重启次数。
    Recovered { loaded: Loaded, attempts: u32 },
    /// 撞上确定性失败（协议/清单/版本/目录），不再重试。
    Deterministic { error: LoadError, attempts: u32 },
    /// 连续重启到顶，插件停在 `crashed`；`message` 直接写进 `last_error`。
    Exhausted {
        attempts: u32,
        exit_code: Option<i32>,
        tail: Vec<String>,
        message: String,
    },
    /// 装载表里没有它（从没 `load` 过，或已被收走）。
    NotLoaded,
}

/// 已装载插件表。
#[derive(Default)]
pub struct Host {
    loaded: Mutex<HashMap<String, Entry>>,
    /// 宿主能力实现方（P3.5）。`None` 时插件反向请求一律 `-32601`。
    ///
    /// 不加锁读也不是问题——它只在启动装配时写一次；用 `Mutex` 只是为了让
    /// `Host` 保持 `Sync`（`Arc<Host>` 要跨线程被 `AppState` 持有）。
    api: Mutex<Option<Arc<dyn HostApi>>>,
}

impl Host {
    pub fn new() -> Self {
        Self::default()
    }

    /// 装上宿主能力实现（内核装配时调用一次）。
    ///
    /// 已装载的插件仍持有装载时的快照：本方法约定在 `plugin.load` 之前调用，
    /// 这也符合实际时序（内核先装配 `FsHostApi`，再等用户点装载）。
    pub fn set_api(&self, api: Arc<dyn HostApi>) {
        if let Ok(mut guard) = self.api.lock() {
            *guard = Some(api);
        }
    }

    /// 取当前宿主能力实现（未接线为 `None`）。
    pub fn api(&self) -> Option<Arc<dyn HostApi>> {
        self.api.lock().ok().and_then(|guard| guard.clone())
    }

    /// 装载（幂等）：已装载且进程活着时直接复用，不重复起进程。
    ///
    /// 每次成功装载都会开启一个新的「重启会话」：`restart_attempts` 归零。
    /// 也就是说「最多 3 次」是**每次装载**的预算，用户手动 `load` 相当于重新
    /// 给一次预算——他按下按钮这个动作本身就是在说「我知道它崩过，再试一次」。
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
            stale.start.process.shutdown().await;
        }

        let resume = Resume {
            plugin_id: plugin_id.clone(),
            install_dir: spec.install_dir.to_path_buf(),
            entry: spec.install_dir.join(&spec.validated.entry),
            manifest: spec.validated.manifest.clone(),
            requested: spec.requested.clone(),
            granted: spec.granted.clone(),
            workspace: spec.workspace.clone(),
            host: self.api(),
        };
        let start = start(&resume).await?;
        let loaded = loaded_of(&start, false);
        if let Ok(mut guard) = self.loaded.lock() {
            guard.insert(plugin_id, Entry::new(start, resume, 0));
        }
        Ok(loaded)
    }

    /// 崩溃后的自动重启：非确定性退出才重启，同会话最多 [`MAX_AUTO_RESTARTS`] 次。
    ///
    /// 调用前插件已经死透了（`has_exited`），这里把它从装载表摘掉再按原样重起：
    /// 进程已经没了，工具也就不再可路由，「先下架、再重启」是天然顺序，不需要
    /// 额外的中间态。三种结局都如实返回，账本由 `crates/core` 落。
    pub async fn auto_restart(&self, plugin_id: &str) -> Restart {
        let Some(entry) = self.take(plugin_id) else {
            return Restart::NotLoaded;
        };
        let attempts = entry.restart_attempts;
        let exit_code = entry
            .start
            .process
            .wait_exit(Duration::from_millis(0))
            .await
            .flatten();
        let tail = entry.start.process.stderr_tail();

        // 进程活着时吐了不合协议的东西：这是代码问题，重启只会复现同一个违规。
        if let Some(violation) = entry.start.process.violation() {
            return Restart::Deterministic {
                error: LoadError::Protocol { message: violation },
                attempts,
            };
        }

        let mut attempts = attempts;
        let mut last_error: Option<LoadError> = None;
        let mut last_exit = exit_code;
        let mut last_tail = tail;
        while attempts < MAX_AUTO_RESTARTS {
            attempts += 1;
            match start(&entry.resume).await {
                Ok(started) => {
                    let loaded = loaded_of(&started, false);
                    tracing::info!(
                        plugin_id = %plugin_id,
                        attempt = attempts,
                        pid = ?started.process.pid(),
                        "插件已自动重启并重新握手"
                    );
                    if let Ok(mut guard) = self.loaded.lock() {
                        guard.insert(
                            plugin_id.to_string(),
                            Entry::new(started, entry.resume, attempts),
                        );
                    }
                    return Restart::Recovered { loaded, attempts };
                }
                Err(error) if error.is_deterministic() => {
                    tracing::warn!(
                        plugin_id = %plugin_id,
                        attempt = attempts,
                        %error,
                        "插件重启撞上确定性失败，不再重试"
                    );
                    return Restart::Deterministic { error, attempts };
                }
                Err(error) => {
                    if let LoadError::Crashed { code, tail } = &error {
                        last_exit = *code;
                        last_tail = tail.clone();
                    }
                    tracing::warn!(
                        plugin_id = %plugin_id,
                        attempt = attempts,
                        %error,
                        "插件自动重启失败，准备重试"
                    );
                    last_error = Some(error);
                }
            }
        }

        let message = match last_error {
            Some(error) => format!("已达自动重启上限（{MAX_AUTO_RESTARTS} 次），不再重启：{error}"),
            None => format!("已达自动重启上限（{MAX_AUTO_RESTARTS} 次），不再重启"),
        };
        Restart::Exhausted {
            attempts,
            exit_code: last_exit,
            tail: last_tail,
            message,
        }
    }

    /// 已崩溃（进程已退出）但还留在装载表里的插件 id。
    ///
    /// 与 [`Host::take_crashed`] 的区别：这里只点名、不摘表，好让上层接着调用
    /// [`Host::auto_restart`]——重启所需的入参还挂在装载表的条目上。
    pub fn crashed_ids(&self) -> Vec<String> {
        let Ok(guard) = self.loaded.lock() else {
            return Vec::new();
        };
        guard
            .iter()
            .filter(|(_, entry)| entry.start.process.has_exited())
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// 卸载：关停进程并返回被下架的工具全名。未装载时返回 `None`。
    pub async fn unload(&self, plugin_id: &str) -> Option<Vec<String>> {
        let entry = self.take(plugin_id)?;
        entry.start.process.shutdown().await;
        Some(entry.start.namespaced)
    }

    /// 该插件是否已装载且进程还活着。
    pub fn is_running(&self, plugin_id: &str) -> bool {
        self.alive(plugin_id).is_some()
    }

    /// 宿主 → 插件的单向事件投递（P5，docs/14 §6.5）。
    ///
    /// 过滤发生在**入队之前**：没声明订阅的插件连队列都不进，更不会有帧写进
    /// 它的 stdin——「不投递」因此在物理上等于「不唤醒」（进程收不到 stdin
    /// 数据就不会被调度去读）。已退出的插件同样跳过：它的工具已下架，再往里
    /// 写帧只会撞管道错误。
    ///
    /// 返回本次实际入队的插件数（合并进已有队列也算），供调用方观测。
    pub fn publish(&self, event: &str, payload: Value) -> usize {
        let targets: Vec<(Arc<EventSink>, Arc<PluginProcess>)> = {
            let Ok(guard) = self.loaded.lock() else {
                return 0;
            };
            guard
                .values()
                .filter(|entry| !entry.start.process.has_exited())
                .filter(|entry| subscribed(&entry.subscriptions, event))
                .map(|entry| (entry.events.clone(), entry.start.process.clone()))
                .collect()
        };
        if targets.is_empty() {
            tracing::trace!(event, "没有插件订阅该事件，不投递");
            return 0;
        }
        let mut delivered = 0;
        for (sink, process) in targets {
            if sink.push(event, payload.clone()) == PushOutcome::Dropped {
                tracing::warn!(
                    plugin_id = %sink.plugin_id(),
                    %event,
                    dropped = sink.dropped(),
                    "事件队列已满，丢弃最旧的一条"
                );
            }
            delivered += 1;
            flush_events(&sink, process);
        }
        delivered
    }

    /// 广播一次插件状态迁移（`plugin.state_changed`）。
    ///
    /// `pluginId` 由这里填：插件发不出这个事件（它只能发 `host/log` /
    /// `host/progress`，见 `loader::handle_notification`），所以载荷里的身份
    /// 一定是宿主认定的那一个，伪造没有入口。
    pub fn announce_state(&self, plugin_id: &str, state: &str) -> usize {
        self.publish(
            PLUGIN_STATE_CHANGED,
            crate::events::state_changed_payload(plugin_id, state),
        )
    }

    /// 某插件当前还在队列里、尚未写出去的事件数（诊断与单测用）。
    pub fn pending_events(&self, plugin_id: &str) -> usize {
        self.loaded
            .lock()
            .ok()
            .and_then(|guard| guard.get(plugin_id).map(|entry| entry.events.len()))
            .unwrap_or(0)
    }

    /// 某插件装载时冻结的订阅清单（诊断用）。
    pub fn subscriptions(&self, plugin_id: &str) -> Vec<String> {
        self.loaded
            .lock()
            .ok()
            .and_then(|guard| {
                guard
                    .get(plugin_id)
                    .map(|entry| entry.subscriptions.clone())
            })
            .unwrap_or_default()
    }

    /// 已在跑的工具全名（未装载为空）。
    pub fn running_tools(&self, plugin_id: &str) -> Vec<String> {
        self.alive(plugin_id)
            .map(|loaded| loaded.namespaced_tools)
            .unwrap_or_default()
    }

    /// 当前所有可路由的插件工具（P3 工具注入的唯一来源）。
    ///
    /// 每次调用都现场扫装载表，不缓存：账本里插件状态随时会变（崩溃、卸载、
    /// 自动重启后重新握手），缓存一份就要额外维护失效逻辑，反而更容易注出
    /// 一个「工具在表里、插件已经不在」的幽灵工具。
    ///
    /// 只收进程还活着的条目；顺序按 `namespaced_name` 排定，保证同一状态下列表
    /// 逐字稳定（注入 prompt 时不会每组轮次都抖动）。
    pub fn tool_catalog(&self) -> Vec<RunningTool> {
        let Ok(guard) = self.loaded.lock() else {
            return Vec::new();
        };
        let mut catalog = Vec::new();
        for (plugin_id, entry) in guard.iter() {
            if entry.start.process.has_exited() {
                continue;
            }
            for (index, tool) in entry.start.tools.iter().enumerate() {
                let namespaced_name = match entry.start.namespaced.get(index) {
                    Some(name) => name.clone(),
                    None => namespaced_tool(plugin_id, &tool.name),
                };
                catalog.push(RunningTool {
                    plugin_id: plugin_id.clone(),
                    name: tool.name.clone(),
                    namespaced_name,
                    description: tool.description.clone().unwrap_or_default(),
                    risk: tool.risk.unwrap_or_default(),
                    parameters: tool
                        .parameters
                        .clone()
                        .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                });
            }
        }
        catalog.sort_by(|left, right| left.namespaced_name.cmp(&right.namespaced_name));
        catalog
    }

    /// 当前所有可触发的插件命令（P5 切片二：命令面板的数据源）。
    ///
    /// 与 [`Self::tool_catalog`] 同一口径：现场扫装载表、只收进程还活着的、
    /// 按 `(plugin_id, name)` 排定，保证面板顺序逐字稳定。命令取自**清单**
    /// （`resume.manifest.commands`）而不是握手：命令是用户在安装时看到的那份
    /// 承诺，插件运行期不能偷偷加一条命令出来让用户按。
    pub fn command_catalog(&self) -> Vec<RunningCommand> {
        let Ok(guard) = self.loaded.lock() else {
            return Vec::new();
        };
        let mut catalog = Vec::new();
        for (plugin_id, entry) in guard.iter() {
            if entry.start.process.has_exited() {
                continue;
            }
            for command in entry.resume.manifest.commands.iter() {
                catalog.push(RunningCommand::from((plugin_id.as_str(), command)));
            }
        }
        catalog.sort_by(|left, right| {
            (left.plugin_id.as_str(), left.name.as_str())
                .cmp(&(right.plugin_id.as_str(), right.name.as_str()))
        });
        catalog
    }

    /// 某个插件当前可触发的命令（装好了但没在跑时为空：命令要打到进程上）。
    pub fn commands_of(&self, plugin_id: &str) -> Vec<RunningCommand> {
        self.command_catalog()
            .into_iter()
            .filter(|command| command.plugin_id == plugin_id)
            .collect()
    }

    /// 调一次插件工具：`plugin/tool-invoke`，参数 `{tool, arguments}`。
    ///
    /// 锁只用来取一个 `Arc<PluginProcess>` 快照，取到就立刻释放——`await` 期间
    /// 绝不能持锁，否则一个卡住的插件会把整个装载表（含所有插件的 `load` /
    /// `unload` / 心跳）一起冻住，等于插件故障升级为内核故障。
    pub async fn call_tool(
        &self,
        plugin_id: &str,
        tool: &str,
        args: Value,
    ) -> Result<Value, CallError> {
        let process = {
            let guard = self.loaded.lock().map_err(|_| CallError::Protocol {
                message: "插件装载表已不可用（锁中毒）".to_string(),
            })?;
            let Some(entry) = guard.get(plugin_id) else {
                return Err(CallError::NotRunning(plugin_id.to_string()));
            };
            if entry.start.process.has_exited() {
                return Err(CallError::NotRunning(plugin_id.to_string()));
            }
            if !entry.start.tools.iter().any(|item| item.name == tool) {
                return Err(CallError::UnknownTool(tool.to_string()));
            }
            Arc::clone(&entry.start.process)
        };

        let params = json!({ "tool": tool, "arguments": args });
        // 配额按「单次工具调用」计（docs/14 §8.4）：调用边界处归零，插件在一次
        // 工具执行里反复问宿主才能被卡在 200 次。
        if let Some(api) = process.host() {
            api.begin_call(plugin_id);
        }
        match process
            .request(TOOL_INVOKE_METHOD, params, CALL_TIMEOUT)
            .await
        {
            Ok(value) => Ok(value),
            // 宿主自己的「进程没了」哨兵：按崩溃上报，而不是当成插件拒绝。
            Err(RequestError::Plugin { code, .. }) if code == EXITED_REPLY_CODE => {
                Err(CallError::Exited {
                    code: exit_code(&process).await,
                    tail: process.stderr_tail(),
                })
            }
            Err(RequestError::Plugin { code, message }) => Err(CallError::Plugin { code, message }),
            Err(RequestError::Timeout { timeout, .. }) => Err(CallError::Timeout {
                tool: tool.to_string(),
                timeout,
            }),
            Err(RequestError::Exited { tail }) => Err(CallError::Exited {
                code: exit_code(&process).await,
                tail,
            }),
            Err(RequestError::Closed) => Err(CallError::Exited {
                code: exit_code(&process).await,
                tail: process.stderr_tail(),
            }),
            Err(RequestError::Protocol(message)) => Err(CallError::Protocol { message }),
        }
    }

    /// 执行一次插件命令：`plugin/command-invoke`，参数 `{command, args}`。
    ///
    /// 与 [`Self::call_tool`] 同构，差异只有三处，且都在「命令是用户按的」这条
    /// 语义上：① 归属校验查的是**清单里的 `commands[]`**（不是握手里的工具表）；
    /// ② 配额同样在调用边界归零——命令里插件一样会反复问宿主，不归零就等于
    /// 把整场会话的 `host/*` 额度算成一次；③ 错误里的名目叫命令，不是工具。
    pub async fn call_command(
        &self,
        plugin_id: &str,
        command: &str,
        args: Value,
    ) -> Result<Value, CallError> {
        let process = {
            let guard = self.loaded.lock().map_err(|_| CallError::Protocol {
                message: "插件装载表已不可用（锁中毒）".to_string(),
            })?;
            let Some(entry) = guard.get(plugin_id) else {
                return Err(CallError::NotRunning(plugin_id.to_string()));
            };
            if entry.start.process.has_exited() {
                return Err(CallError::NotRunning(plugin_id.to_string()));
            }
            // 运行时再校验一次归属：上层的快照是几十微秒前的，这一条才是真正
            // 「此刻这份装载表里有没有它」的答案。两道检查不重复——上层那道是
            // 为了给出带 `declared` 清单的可诊断错误，这道是防竞态。
            if !entry
                .resume
                .manifest
                .commands
                .iter()
                .any(|item| item.name == command)
            {
                return Err(CallError::UnknownCommand(command.to_string()));
            }
            Arc::clone(&entry.start.process)
        };

        let params = json!({ "command": command, "args": args });
        if let Some(api) = process.host() {
            api.begin_call(plugin_id);
        }
        match process
            .request(COMMAND_INVOKE_METHOD, params, CALL_TIMEOUT)
            .await
        {
            Ok(value) => Ok(value),
            Err(RequestError::Plugin { code, .. }) if code == EXITED_REPLY_CODE => {
                Err(CallError::Exited {
                    code: exit_code(&process).await,
                    tail: process.stderr_tail(),
                })
            }
            Err(RequestError::Plugin { code, message }) => Err(CallError::Plugin { code, message }),
            Err(RequestError::Timeout { timeout, .. }) => Err(CallError::Timeout {
                // 命令超时沿用同一个 `tool` 字段：上层文案统一按「调用」措辞，
                // 多造一个字段只会让错误映射分叉。
                tool: command.to_string(),
                timeout,
            }),
            Err(RequestError::Exited { tail }) => Err(CallError::Exited {
                code: exit_code(&process).await,
                tail,
            }),
            Err(RequestError::Closed) => Err(CallError::Exited {
                code: exit_code(&process).await,
                tail: process.stderr_tail(),
            }),
            Err(RequestError::Protocol(message)) => Err(CallError::Protocol { message }),
        }
    }

    /// 当前所有在跑的插件 id。
    pub fn running_ids(&self) -> Vec<String> {
        self.loaded
            .lock()
            .map(|guard| {
                guard
                    .iter()
                    .filter(|(_, entry)| !entry.start.process.has_exited())
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
            .filter(|(_, entry)| entry.start.process.has_exited())
            .map(|(id, _)| id.clone())
            .collect();
        for plugin_id in dead {
            if let Some(entry) = guard.remove(&plugin_id) {
                crashed.push(Crashed {
                    plugin_id,
                    namespaced_tools: entry.start.namespaced,
                    exit_code: None,
                    tail: entry.start.process.stderr_tail(),
                    violation: entry.start.process.violation(),
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
            entry.start.process.shutdown().await;
        }
    }

    fn alive(&self, plugin_id: &str) -> Option<Loaded> {
        let guard = self.loaded.lock().ok()?;
        let entry = guard.get(plugin_id)?;
        if entry.start.process.has_exited() {
            return None;
        }
        Some(loaded_of(&entry.start, true))
    }

    fn take(&self, plugin_id: &str) -> Option<Entry> {
        self.loaded.lock().ok()?.remove(plugin_id)
    }
}

/// 起进程 + 握手 + 校验：装载与自动重启走同一条路径，保证两边判定不会跑偏。
///
/// 顺序固定为**授权 → 入口存在 → 起进程 → 握手 → 校验**：授权不齐时连进程都不该
/// 起（否则用户会在任务管理器里看到一个本该被拦住的插件）。
async fn start(resume: &Resume) -> Result<Start, LoadError> {
    let missing: Vec<String> = resume
        .requested
        .iter()
        .filter(|capability| !resume.granted.contains(capability))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(LoadError::MissingCapabilities { missing });
    }

    if !resume.entry.is_file() {
        return Err(LoadError::NotInstalled(resume.entry.display().to_string()));
    }

    let process = PluginProcess::spawn(SpawnOptions {
        plugin_id: &resume.plugin_id,
        install_dir: &resume.install_dir,
        entry: &resume.entry,
        host: resume.host.clone(),
    })
    .await
    .map_err(LoadError::Spawn)?;

    let params = handshake_params(
        &resume.manifest,
        &resume.install_dir,
        resume.workspace.clone(),
    );
    let handshake = match handshake(&process, params).await {
        Ok(handshake) => handshake,
        Err(error) => {
            process.shutdown().await;
            return Err(error);
        }
    };

    // 版本与工具集合都对齐后才允许入表：入表就意味着「工具可路由」。
    if let Err(error) = verify(&resume.manifest, &handshake) {
        process.shutdown().await;
        return Err(error);
    }

    let namespaced = resume
        .manifest
        .tools
        .iter()
        .map(|tool| namespaced_tool(&resume.manifest.id, &tool.name))
        .collect::<Vec<_>>();
    let tools = merge_tools(&resume.manifest, &handshake);
    Ok(Start {
        handshake,
        tools,
        namespaced,
        process,
    })
}

fn loaded_of(start: &Start, reused: bool) -> Loaded {
    Loaded {
        handshake: start.handshake.clone(),
        tools: start.tools.clone(),
        namespaced_tools: start.namespaced.clone(),
        pid: start.process.pid(),
        reused,
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
        // 宿主自己的「进程没了」哨兵：这是崩溃，不是插件拒绝——按崩溃上报，
        // 上层才会把它算作非确定性失败并允许自动重启。
        RequestError::Plugin { code, .. } if code == EXITED_REPLY_CODE => LoadError::Crashed {
            code: exit_code(process).await,
            tail: process.stderr_tail(),
        },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{
        file_saved_payload, progress_payload, session_payload, workspace_changed_payload,
        FILE_SAVED, PLUGIN_PROGRESS, SESSION_STARTED, WORKSPACE_CHANGED,
    };
    use crate::loader::{SpawnOptions, NODE_ENV};
    use crate::manifest::ManifestTool;
    use ai_ide_protocol::PluginToolRisk;
    use std::process::Stdio;

    /// 最小的工具调用替身：把 `params` 原样回显，并带上收到的方法名。
    const ECHO_TOOL: &str = r#"
        const readline = require('node:readline')
        const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
        rl.on('line', (line) => {
          if (!line.trim()) return
          const frame = JSON.parse(line)
          const params = frame.params || {}
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0',
            id: frame.id,
            result: { method: frame.method, tool: params.tool, echo: params.arguments }
          }) + '\n')
        })
    "#;

    fn node_ready() -> bool {
        std::process::Command::new(std::env::var(NODE_ENV).unwrap_or_else(|_| "node".to_string()))
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
            "aiide-host-call-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn demo_manifest() -> Manifest {
        Manifest {
            id: "demo".to_string(),
            name: "示例插件".to_string(),
            version: "0.1.0".to_string(),
            description: None,
            plugin_api: "1.0".to_string(),
            main: "index.cjs".to_string(),
            capabilities: Vec::new(),
            tools: vec![ManifestTool {
                name: "echo".to_string(),
                description: Some("回显入参".to_string()),
                risk: PluginToolRisk::Read,
                path_field: None,
                parameters: json!({ "type": "object", "properties": {} }),
            }],
            commands: Vec::new(),
            subscriptions: Vec::new(),
            allowed_domains: Vec::new(),
            allowed_models: Vec::new(),
            max_tokens: 0,
            timeout_ms: 30_000,
        }
    }

    /// 直接按「装载成功」的成品形态塞进装载表：这里要验的是工具路径，
    /// 不是握手判定，走一遍 `load` 只会把测试绑到清单校验的细节上。
    async fn host_with_echo(dir: &Path, entry: &Path) -> Host {
        let process = PluginProcess::spawn(SpawnOptions {
            plugin_id: "demo",
            install_dir: dir,
            entry,
            host: None,
        })
        .await
        .expect("起进程");
        let tools = vec![HandshakeTool {
            name: "echo".to_string(),
            description: Some("回显入参".to_string()),
            risk: Some(PluginToolRisk::Read),
            parameters: Some(json!({ "type": "object", "properties": {} })),
        }];
        let start = Start {
            handshake: Handshake {
                plugin_api: Some(json!({ "major": 1, "minor": 0 })),
                name: Some("demo".to_string()),
                version: Some("0.1.0".to_string()),
                tools: tools.clone(),
                commands: Vec::new(),
                subscriptions: Vec::new(),
            },
            tools,
            namespaced: vec![namespaced_tool("demo", "echo")],
            process,
        };
        let host = Host::new();
        host.loaded.lock().expect("装载表").insert(
            "demo".to_string(),
            Entry::new(
                start,
                Resume {
                    plugin_id: "demo".to_string(),
                    install_dir: dir.to_path_buf(),
                    entry: entry.to_path_buf(),
                    manifest: demo_manifest(),
                    requested: Vec::new(),
                    granted: Vec::new(),
                    workspace: Value::Null,
                    host: None,
                },
                0,
            ),
        );
        host
    }

    #[tokio::test]
    async fn call_tool_on_unloaded_plugin_reports_not_running() {
        let host = Host::new();
        match host.call_tool("ghost", "echo", json!({})).await {
            Err(CallError::NotRunning(plugin_id)) => {
                assert_eq!(plugin_id, "ghost");
                assert!(CallError::NotRunning("ghost".to_string())
                    .to_string()
                    .contains("ghost"));
            }
            other => panic!("未装载应报 NotRunning，实际 {other:?}"),
        }
        assert!(host.tool_catalog().is_empty(), "未装载时目录应为空");
    }

    #[tokio::test]
    async fn call_tool_on_unknown_tool_reports_unknown_tool() {
        if !node_ready() {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let dir = scratch("unknown");
        let entry = dir.join("index.cjs");
        std::fs::write(&entry, ECHO_TOOL).expect("写脚本");
        let host = host_with_echo(&dir, &entry).await;

        match host.call_tool("demo", "no_such_tool", json!({})).await {
            Err(CallError::UnknownTool(tool)) => {
                assert_eq!(tool, "no_such_tool");
                assert!(CallError::UnknownTool("no_such_tool".to_string())
                    .to_string()
                    .contains("no_such_tool"));
            }
            other => panic!("工具不存在应报 UnknownTool，实际 {other:?}"),
        }

        host.shutdown_all().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn call_tool_round_trip_returns_plugin_reply() {
        if !node_ready() {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let dir = scratch("round-trip");
        let entry = dir.join("index.cjs");
        std::fs::write(&entry, ECHO_TOOL).expect("写脚本");
        let host = host_with_echo(&dir, &entry).await;

        let catalog = host.tool_catalog();
        assert_eq!(catalog.len(), 1, "应注入 1 个工具");
        assert_eq!(catalog[0].namespaced_name, "plugin__demo__echo");
        assert_eq!(catalog[0].plugin_id, "demo");
        assert_eq!(catalog[0].risk, PluginToolRisk::Read);

        let reply = host
            .call_tool("demo", "echo", json!({ "text": "你好" }))
            .await
            .expect("调用应成功");
        assert_eq!(reply["method"], json!(TOOL_INVOKE_METHOD));
        assert_eq!(reply["tool"], json!("echo"));
        assert_eq!(reply["echo"], json!({ "text": "你好" }));

        host.shutdown_all().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------------------------------------------------------
    // P5 切片一：事件订阅投递
    // ---------------------------------------------------------------

    /// 事件接收替身：把收到的 `plugin/event` 的 `params` 原样追加到
    /// `<dir>/events.jsonl`，其余带 `id` 的帧回空 `result`。
    ///
    /// 关键点：**只有真收到帧才写文件**。因此「文件不存在」就是「一个字节都
    /// 没进过该进程的 stdin」的物理证据——未订阅的插件不投递、不被唤醒，
    /// 不是靠读代码相信，而是靠落盘证据断言。
    const EVENT_RECORDER: &str = r#"
        const fs = require('node:fs')
        const path = require('node:path')
        const readline = require('node:readline')
        const out = path.join(__dirname, 'events.jsonl')
        const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
        rl.on('line', (line) => {
          if (!line.trim()) return
          const frame = JSON.parse(line)
          if (frame.method === 'plugin/event') {
            fs.appendFileSync(out, JSON.stringify(frame.params) + '\n')
          }
          if (frame.id !== undefined) {
            process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: frame.id, result: {} }) + '\n')
          }
        })
    "#;

    /// 越界替身：一上来就冒充宿主发 `plugin/state_changed`。
    const EVENT_SPOOFER: &str = r#"
        process.stdout.write(JSON.stringify({
          jsonrpc: '2.0',
          method: 'plugin/state_changed',
          params: { pluginId: 'victim', state: 'crashed' }
        }) + '\n')
        const readline = require('node:readline')
        const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
        rl.on('line', () => {})
    "#;

    /// 遥测替身：发一条 `host/progress`，参数里故意伪造 `pluginId`。
    const PROGRESS_REPORTER: &str = r#"
        process.stdout.write(JSON.stringify({
          jsonrpc: '2.0',
          method: 'host/progress',
          params: { token: 'scan', message: '半程', percent: 50, pluginId: 'forged' }
        }) + '\n')
        const readline = require('node:readline')
        const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
        rl.on('line', () => {})
    "#;

    /// 记录 `notify` 的假宿主（只关心遥测，不实现任何 `host/*` 能力）。
    #[derive(Default)]
    struct RecordingHost {
        seen: Mutex<Vec<(String, String, Value)>>,
    }

    impl HostApi for RecordingHost {
        fn call(
            &self,
            _plugin_id: &str,
            method: &str,
            _params: &Value,
        ) -> Result<Value, crate::host_api::HostApiError> {
            Err(crate::host_api::HostApiError::method_not_found(method))
        }

        fn notify(
            &self,
            plugin_id: &str,
            method: &str,
            params: &Value,
        ) -> Result<(), crate::host_api::HostApiError> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push((plugin_id.to_string(), method.to_string(), params.clone()));
            }
            Ok(())
        }
    }

    /// 起一个装载表条目：`subscriptions` 只写进**清单**（投递只看它）。
    async fn event_entry(
        tag: &str,
        plugin_id: &str,
        subscriptions: &[&str],
        script: &str,
        host_api: Option<Arc<dyn HostApi>>,
    ) -> (PathBuf, Entry) {
        let dir = scratch(tag);
        let entry_path = dir.join("index.cjs");
        std::fs::write(&entry_path, script).expect("写脚本");
        let process = PluginProcess::spawn(SpawnOptions {
            plugin_id,
            install_dir: &dir,
            entry: &entry_path,
            host: host_api,
        })
        .await
        .expect("起进程");
        let mut manifest = demo_manifest();
        manifest.id = plugin_id.to_string();
        manifest.tools = Vec::new();
        manifest.subscriptions = subscriptions.iter().map(|item| item.to_string()).collect();
        let start = Start {
            handshake: Handshake {
                plugin_api: Some(json!({ "major": 1, "minor": 0 })),
                name: Some(plugin_id.to_string()),
                version: Some("0.1.0".to_string()),
                tools: Vec::new(),
                commands: Vec::new(),
                subscriptions: Vec::new(),
            },
            tools: Vec::new(),
            namespaced: Vec::new(),
            process,
        };
        let resume = Resume {
            plugin_id: plugin_id.to_string(),
            install_dir: dir.clone(),
            entry: entry_path,
            manifest,
            requested: Vec::new(),
            granted: Vec::new(),
            workspace: Value::Null,
            host: None,
        };
        (dir, Entry::new(start, resume, 0))
    }

    /// 轮询等一个文件出现且非空（投递是异步的：publish → 队 → 任务 → stdin）。
    async fn wait_for_lines(path: &Path) -> Option<String> {
        for _ in 0..120 {
            if let Ok(text) = std::fs::read_to_string(path) {
                if !text.trim().is_empty() {
                    return Some(text);
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        None
    }

    #[tokio::test]
    async fn publish_reaches_subscriber_only() {
        if !node_ready() {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let (alpha_dir, alpha) = event_entry(
            "pub-alpha",
            "alpha",
            &["workspace.changed"],
            EVENT_RECORDER,
            None,
        )
        .await;
        let (beta_dir, beta) = event_entry("pub-beta", "beta", &[], EVENT_RECORDER, None).await;
        let host = Host::new();
        {
            let mut guard = host.loaded.lock().expect("装载表");
            guard.insert("alpha".to_string(), alpha);
            guard.insert("beta".to_string(), beta);
        }

        let delivered = host.publish(
            WORKSPACE_CHANGED,
            workspace_changed_payload(["src/a.ts", "src/b.ts"]),
        );
        assert_eq!(delivered, 1, "只有 alpha 声明了 workspace.changed");

        let text = wait_for_lines(&alpha_dir.join("events.jsonl"))
            .await
            .expect("alpha 应收到事件帧");
        let frames: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("事件行是 JSON"))
            .collect();
        assert_eq!(frames.len(), 1, "一条事件只写一帧");
        // 载荷最小化：只有路径，没有任何文件内容。
        assert_eq!(
            frames[0],
            json!({
                "event": WORKSPACE_CHANGED,
                "payload": { "files": ["src/a.ts", "src/b.ts"] }
            })
        );
        let payload = frames[0]["payload"].as_object().expect("载荷是对象");
        assert_eq!(
            payload.keys().collect::<Vec<_>>(),
            vec!["files"],
            "workspace.changed 只允许带 files"
        );

        // 未订阅的 beta：一个字节都没写进去（也就没被唤醒）。
        assert!(
            !beta_dir.join("events.jsonl").exists(),
            "未订阅的插件不该收到任何帧"
        );
        assert_eq!(host.pending_events("beta"), 0, "未订阅的插件不该有队列残留");
        assert!(host.is_running("beta"), "不投递 ≠ 杀进程：beta 仍在跑");

        host.shutdown_all().await;
        let _ = std::fs::remove_dir_all(&alpha_dir);
        let _ = std::fs::remove_dir_all(&beta_dir);
    }

    #[tokio::test]
    async fn wildcard_subscription_gets_session_events() {
        if !node_ready() {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let (dir, entry) =
            event_entry("pub-session", "alpha", &["session.*"], EVENT_RECORDER, None).await;
        let host = Host::new();
        host.loaded
            .lock()
            .expect("装载表")
            .insert("alpha".to_string(), entry);

        assert_eq!(
            host.publish(SESSION_STARTED, session_payload("s-1")),
            1,
            "session.* 应覆盖 session.started"
        );
        assert_eq!(
            host.publish(FILE_SAVED, file_saved_payload("a.ts")),
            0,
            "没订阅 file.saved 就不该投"
        );

        let text = wait_for_lines(&dir.join("events.jsonl"))
            .await
            .expect("应收到会话事件");
        assert_eq!(
            text.trim(),
            json!({ "event": SESSION_STARTED, "payload": { "sessionId": "s-1" } }).to_string()
        );

        host.shutdown_all().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn announce_state_uses_host_side_plugin_id() {
        if !node_ready() {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let (dir, entry) = event_entry(
            "pub-state",
            "alpha",
            &[PLUGIN_STATE_CHANGED],
            EVENT_RECORDER,
            None,
        )
        .await;
        let host = Host::new();
        host.loaded
            .lock()
            .expect("装载表")
            .insert("alpha".to_string(), entry);

        host.announce_state("alpha", "ready");
        let text = wait_for_lines(&dir.join("events.jsonl"))
            .await
            .expect("应收到状态迁移");
        assert_eq!(
            text.trim(),
            json!({
                "event": PLUGIN_STATE_CHANGED,
                "payload": { "pluginId": "alpha", "state": "ready" }
            })
            .to_string()
        );

        host.shutdown_all().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn plugin_cannot_emit_plugin_events() {
        if !node_ready() {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let (dir, entry) = event_entry("spoof", "alpha", &[], EVENT_SPOOFER, None).await;
        let host = Host::new();
        let process = entry.start.process.clone();
        host.loaded
            .lock()
            .expect("装载表")
            .insert("alpha".to_string(), entry);

        let mut violation = None;
        for _ in 0..120 {
            if let Some(reason) = process.violation() {
                violation = Some(reason);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let violation = violation.expect("插件发 plugin/* 应被判违规");
        assert!(
            violation.contains("plugin/state_changed"),
            "违规原因要指出被拒的方法：{violation}"
        );

        host.shutdown_all().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn host_progress_is_forwarded_with_stamped_plugin_id() {
        if !node_ready() {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let recorder = Arc::new(RecordingHost::default());
        let (dir, entry) = event_entry(
            "progress",
            "alpha",
            &[],
            PROGRESS_REPORTER,
            Some(recorder.clone() as Arc<dyn HostApi>),
        )
        .await;
        let host = Host::new();
        host.loaded
            .lock()
            .expect("装载表")
            .insert("alpha".to_string(), entry);

        let mut seen = Vec::new();
        for _ in 0..120 {
            if let Ok(list) = recorder.seen.lock() {
                if !list.is_empty() {
                    seen = list.clone();
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(seen.len(), 1, "应转出一条遥测");
        assert_eq!(seen[0].0, "alpha");
        assert_eq!(seen[0].1, "host/progress");
        assert_eq!(
            seen[0].2["pluginId"],
            json!("alpha"),
            "pluginId 由宿主覆盖，插件伪造无效"
        );
        // 帧要过一遍 `JSON.stringify`，整数值的浮点在线上会掉成 `50`，
        // 所以按数值比而不是按 `json!` 字面量比。
        assert_eq!(seen[0].2["percent"].as_f64(), Some(50.0));
        assert_eq!(seen[0].2["token"], json!("scan"));

        host.shutdown_all().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `plugin.progress` 走切片一的同一套投递：订阅了的收，没订阅的一个字节都
    /// 收不到（也就没被唤醒）。
    ///
    /// 与 `workspace.changed` 分开断言的理由：进度是宿主高频产出的遥测，一旦
    /// 退化成「谁都推」，用户没订阅的插件会被每一条进度戳醒——这条测试就是
    /// 「未订阅不得收、不得唤醒」在 progress 通道上的落点。
    #[tokio::test]
    async fn progress_event_reaches_subscriber_only() {
        if !node_ready() {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let (alpha_dir, alpha) = event_entry(
            "progress-alpha",
            "alpha",
            &[PLUGIN_PROGRESS],
            EVENT_RECORDER,
            None,
        )
        .await;
        let (beta_dir, beta) =
            event_entry("progress-beta", "beta", &[], EVENT_RECORDER, None).await;
        let host = Host::new();
        {
            let mut guard = host.loaded.lock().expect("装载表");
            guard.insert("alpha".to_string(), alpha);
            guard.insert("beta".to_string(), beta);
        }

        let payload = progress_payload("alpha", Some("scan"), Some("正在扫描"), Some(30.0));
        let delivered = host.publish(PLUGIN_PROGRESS, payload.clone());
        assert_eq!(delivered, 1, "只有 alpha 声明了 plugin.progress");

        let text = wait_for_lines(&alpha_dir.join("events.jsonl"))
            .await
            .expect("alpha 应收到进度帧");
        let frames: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("事件行是 JSON"))
            .collect();
        assert_eq!(frames.len(), 1, "一条进度只写一帧");
        assert_eq!(frames[0]["event"], json!(PLUGIN_PROGRESS));
        assert_eq!(frames[0]["payload"]["token"], json!("scan"));
        assert_eq!(frames[0]["payload"]["message"], json!("正在扫描"));
        assert_eq!(frames[0]["payload"]["pluginId"], json!("alpha"));
        assert_eq!(frames[0]["payload"]["truncated"], json!(false));
        // 帧过 `JSON.stringify`，整数值浮点会掉成 `30`，按数值比。
        assert_eq!(frames[0]["payload"]["percent"].as_f64(), Some(30.0));
        assert_eq!(
            frames[0]["payload"]
                .as_object()
                .expect("载荷是对象")
                .keys()
                .collect::<Vec<_>>(),
            vec!["message", "percent", "pluginId", "token", "truncated"],
            "载荷只有进度字段，没有文件内容的位置"
        );

        assert!(
            !beta_dir.join("events.jsonl").exists(),
            "未订阅的插件不该收到进度帧"
        );
        assert_eq!(host.pending_events("beta"), 0);
        assert!(host.is_running("beta"), "不投递 ≠ 杀进程：beta 仍在跑");

        host.shutdown_all().await;
        let _ = std::fs::remove_dir_all(&alpha_dir);
        let _ = std::fs::remove_dir_all(&beta_dir);
    }
}
