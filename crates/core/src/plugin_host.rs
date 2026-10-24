//! 宿主能力（插件系统 P3.5 读侧 + P4 切片二写侧）：`host/fs.read` /
//! `host/fs.list` / `host/fs.stat` / `host/search.content` / `host/workspace.info`
//! 与 `host/fs.write` 的真实实现。
//!
//! 这一层是 [`ai_ide_plugin::host_api::HostApi`] 的内核侧实现：`crates/plugin`
//! 只负责把插件进程发出的 `host/*` 帧转过来，**资格判定、路径判定、审批判定、
//! 用量判定全在这里**。遥测走 [`HostApi::notify`] 这条旁路：`host/log` 无条件
//! 收（写日志没有额外信息面），`host/progress` 要 `progress` 能力，转成
//! `plugin.progress` 推给外壳窗口与订阅了它的插件（P5 切片四）。网络
//! （`net.fetch`）、模型（`llm.chat`）明确留给后续切片，一律回 `-32601`。
//!
//! 闸门严格按 docs/14 §8.1 的顺序，且**顺序不可交换**：
//!
//! 1. **能力闸门**（6103）：账本里没有 `fs.read` / `fs.write` / `search` 就直接拒；
//! 2. **路径闸门**（1002 越界 / 1003 敏感）：只收相对路径，`..`、盘符、UNC、
//!    NUL 一律拒；`.git` / `.ssh` / `.aws` / `.kube` / `.env` 任意层级硬拒；
//!    规范化（含符号链接解析）后必须落在规范化的工作区根之内；
//! 3. **审批闸门**（3001）：写侧专属。`scope_key` 是**规范化绝对路径**，
//!    命中「已 always」的键就不问；否则交出 [`WriteApprover`] 裁决，
//!    没答复 / 明确拒绝一律 3001 —— 写盘这件事的默认值只能是「不写」；
//! 4. **配额闸门**（6109）：一次工具调用内 ≤ 200 次，计数由
//!    [`HostApi::begin_call`] 在工具调用边界归零。
//!
//! 能力必须排在配额前面：反过来的话，未授权插件会先撞「次数超了」，
//! 用户看到的提示指错方向，而且白送了一个「本插件有没有被授权」的探测口。
//! 审批排在配额前面同理：被拒的写请求连额度都不该消耗。
//!
//! 写盘用「同目录临时文件 → `fsync` → `rename`」：`rename` 在同卷上是替换语义
//! （Windows 走 `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`），掉电最多丢掉这次
//! 写入，绝不会给用户留半个文件。
//!
//! 每次调用（含被拒、被驳回与超配额）都落一行 `plugin.host_call` 审计。审计是
//! 异步写的：这条路径跑在插件 stdout 的帧泵里，同步 `await` 一次写库会堵住该
//! 插件的所有回包，所以拿不到 Tokio 运行时（纯单测）或没有库时就**跳过**，
//! 绝不 `block_on`。

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};

use ai_ide_plugin::events::{progress_payload, PLUGIN_LOG, PLUGIN_PROGRESS};
use ai_ide_plugin::host::Host;
use ai_ide_plugin::host_api::{
    HostApi, HostApiError, LOG_METHOD, PROGRESS_CAPABILITY, PROGRESS_METHOD,
};
use ai_ide_store::{audit_actor, audit_kind, NewAuditEvent, Store};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Value};

use crate::peer::Peer;

/// `host/fs.read` 的默认读取上限（64 KB）。
pub const DEFAULT_READ_BYTES: u64 = 64 * 1024;
/// `host/search.content` 的默认命中条数上限。
pub const DEFAULT_SEARCH_RESULTS: usize = 50;
/// 单次工具调用允许的宿主调用次数（docs/14 §8.4）。
pub const QUOTA_PER_CALL: usize = 200;
/// `host/search.content` 单文件大小上限：超过就跳过，避免一次搜索把 GB 级日志读进来。
const MAX_SEARCH_FILE_BYTES: u64 = 1024 * 1024;
/// 搜索不进入的目录（构建产物 / 依赖 / 版本库内部）。
const SKIP_DIRS: &[&str] = &["node_modules", "target", ".git", ".hg", ".svn"];
/// 敏感段：任意层级命中即整条路径拒（1003）。
const SENSITIVE_SEGMENTS: &[&str] = &[".git", ".ssh", ".aws", ".kube", ".env"];

/// 审计 `decision` 取值：放行。
pub const DECISION_ALLOW: &str = "allow";
/// 审计 `decision` 取值：被闸门拒（能力 / 路径 / 参数）。
pub const DECISION_DENY: &str = "deny";
/// 审计 `decision` 取值：写侧审批当场获准（一次或总是）。
pub const DECISION_APPROVE: &str = "approve";
/// 审计 `decision` 取值：写侧审批被驳回 / 无人裁决。
pub const DECISION_REJECT: &str = "reject";
/// 审计 `decision` 取值：配额超限。
pub const DECISION_QUOTA_EXCEEDED: &str = "quota_exceeded";
/// 审计 `decision` 取值：闸门放行但执行失败（文件不存在、IO 错）。
pub const DECISION_ERROR: &str = "error";

/// 临时文件前缀：写盘先落到目标**同目录**（跨目录就不是同卷，rename 会失效），
/// 名字带进程号与自增序号，避免两个插件同时写同一目标时互相踩。
const WRITE_TEMP_PREFIX: &str = ".aiide-write";

/// 方法 → 所需能力。返回 `None` 的方法就是「v1 未实现」，回 `-32601`。
///
/// 注意 `workspace.info` 也归 `fs.read`：它只是把已打开工作区的根告诉插件，
/// 读能力本就隐含这件事，单开一个 `workspace` 能力会让清单白多一项。
fn capability_of(method: &str) -> Option<&'static str> {
    match method {
        "host/fs.read" | "host/fs.list" | "host/fs.stat" | "host/workspace.info" => Some("fs.read"),
        "host/search.content" => Some("search"),
        // 写侧单列能力：docs/14 §7.1 里 `fs.write` 是独立一行，授权粒度不同
        // （读可以用一次授权覆盖全部读，写要按文件审批）。
        "host/fs.write" => Some("fs.write"),
        _ => None,
    }
}

/// 写侧审批结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteApproval {
    /// 只放行这一次。
    AllowOnce,
    /// 放行并记住 `scope_key`，同一目标的后续写入不再打扰用户。
    AllowAlways,
    /// 明确拒绝。
    Reject,
}

/// 一次写侧审批请求。
///
/// `scope_key` 是**规范化绝对路径**（正斜杠、Windows 下统一小写），它同时是
/// 「总是允许」的落键粒度（docs/14 §7.1：按文件）与审计里可对账的标识；
/// `path` 只是给用户看的相对路径，不参与任何判定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteApprovalRequest {
    pub plugin_id: String,
    pub method: String,
    pub path: String,
    pub scope_key: String,
}

/// 一次命令审批请求（P5 切片二）。
///
/// 与 [`WriteApprovalRequest`] 分开：命令不是「写一个文件」，它的 `scope_key`
/// 是「哪个插件的哪条命令」，而 `path`（若该命令针对具体文件）只是给人看的
/// 目标说明。混用一张请求表会让「总是允许」的落键粒度从文件漂到命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandApprovalRequest {
    pub plugin_id: String,
    /// 命令名（插件内原始名）。
    pub command: String,
    /// 该命令声明了 `pathField` 时的规范化目标路径（仅用于展示与脱敏）。
    pub path: Option<String>,
    /// 「总是允许」的落键：`plugin.command:<plugin_id>:<command>[:<path>]`。
    pub scope_key: String,
}

/// 写侧审批的裁决方。
///
/// 同步签名是刻意的：判定跑在插件 stdout 的帧泵上，异步等一个 UI 回执会把该
/// 插件的所有回包一起堵住。实现方要接 UI，就自己用带超时的同步等待（超时返回
/// `None` = 不表态 = 拒），绝不能在这里 `block_on` 一个无限期的等待。
///
/// 返回 `None` 按「拒绝」处理：审批链断了的时候，默认值必须是「不写」。
pub trait WriteApprover: Send + Sync {
    fn decide(&self, request: &WriteApprovalRequest) -> Option<WriteApproval>;

    /// 命令审批（P5 切片二）：`risk=write` 的插件命令在执行**前**过这里。
    ///
    /// 默认返回 `None`（= 拒）：没接入审批的实现在这一步就该拦下写命令，
    /// 而不是「没人管就放行」。生产实现见 `crate::plugin_approver`。
    fn decide_command(&self, request: &CommandApprovalRequest) -> Option<WriteApproval> {
        let _ = request;
        None
    }
}

/// 写侧的 `scope_key`：规范化绝对路径。
///
/// 三条归一：反斜杠统一成正斜杠、Windows 下转小写（文件系统大小写不敏感，
/// 不折小写就能用 `A.txt` / `a.txt` 把「按文件审批」变成「每条路径问一次」）、
/// 去掉尾部斜杠。不用 `canonicalize`：它要求路径已存在，而写侧大量是新建文件。
pub fn write_scope_key(abs: &Path) -> String {
    let text = abs.to_string_lossy().replace('\\', "/");
    let trimmed = text.trim_end_matches('/');
    let key = if trimmed.is_empty() {
        text
    } else {
        trimmed.to_string()
    };
    if cfg!(windows) {
        key.to_ascii_lowercase()
    } else {
        key
    }
}

/// 读侧宿主能力的实现。
///
/// 字段全是「可换 + 可并发读」的：工作区根在 `ws.open` / `ws.close` 时换，
/// 授权集在 `plugin.load` 后按账本刷新，库在启动时后装，配额按插件计时累加。
pub struct FsHostApi {
    /// 工作区根（规范化前的原始路径）。`None` = 没打开工作区。
    root: Arc<RwLock<Option<PathBuf>>>,
    /// 缺省授权集（未按插件登记时使用）。
    granted: Arc<RwLock<HashSet<String>>>,
    /// 按插件的授权集：账本口径是「每个插件各有一套」，共享一份会串味。
    by_plugin: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    /// 会话库。用 `RwLock` 而非 `Option<Store>`：api 在 `AppState::new` 就建好
    /// （`Host` 随时可能收到 `host/*`），库是启动稍后才连上的，晚装必须能生效。
    ///
    /// `Arc` 是为了把槽位交给写侧审批方（[`crate::plugin_approver`]）：
    /// 「总是允许」的落盘要写进同一个库，而库可能在审批方建好之后才连上。
    store: Arc<RwLock<Option<Store>>>,
    /// 每插件在当前工具调用内的宿主调用计数。
    quota: Mutex<HashMap<String, usize>>,
    /// 写侧审批裁决方。没装 = 没人能批准 = 写请求一律 3001（fail closed）。
    approver: RwLock<Option<Arc<dyn WriteApprover>>>,
    /// 已「总是允许」的写侧 `scope_key` 集合（规范化绝对路径）。
    ///
    /// 与账本里的免问规则同粒度：一条规则 = 一个文件的绝对路径。装载时用
    /// [`FsHostApi::seed_write_always`] 从库里灌回来，撤销时用同一入口覆盖。
    /// 落库持久化留给审批链路接线（切片三）：当前只在进程内生效，重启即忘，
    /// 也就是「重启后重新问一次」——偏保守的那一侧，不会变成静默放行。
    write_always: RwLock<HashSet<String>>,
    /// 审计 `callId` 的自增序号（进程内唯一即可，不跨进程对齐）。
    calls: AtomicU64,
    /// 临时文件名里的自增序号（不复用 `calls`：临时文件与审计编号没有关系，
    /// 混用会让 callId 出现「跳过」的假象）。
    temps: AtomicU64,
    /// 装载表（P5 遥测转投用）。
    ///
    /// 存 `Weak` 而不是 `Arc`：`Host` 持有本结构，本结构再持 `Arc<Host>` 会成环，
    /// 进程退出时释放不掉。`AppState::new` 建好 `Host` 后立刻回填。
    plugins: RwLock<Option<Weak<Host>>>,
    /// 外壳窗口的事件出口（P5 切片四）。
    ///
    /// 与 `plugins` 分开：`plugin.progress` 的主收件人是**用户看到的窗口**，
    /// 不是插件。一个插件都没订阅时，这条通道仍然要通——所以两者不能合成一个
    /// 「投给订阅者」的动作。`None` = 没有打开的窗口（纯单测 / Core 单独跑）。
    shell: RwLock<Option<Arc<dyn ShellEvents>>>,
}

impl Default for FsHostApi {
    fn default() -> Self {
        Self::new()
    }
}

/// 落一行 `plugin.host_call` 所需的原料。
///
/// 收成结构体而不是七个位置参数：调用处几乎都是 `Option` 字段，位置参数一多
/// 就分不清「这个 `None` 是 target 还是 scope_key」。用 [`Self::new`] 起头，
/// 再用 `target` / `scope_key` 按需补，读起来跟审计字段一一对应。
struct HostCallAudit<'a> {
    plugin_id: &'a str,
    method: &'a str,
    capability: &'a str,
    call_id: &'a str,
    decision: &'a str,
    target: Option<&'a str>,
    scope_key: Option<&'a str>,
}

impl<'a> HostCallAudit<'a> {
    fn new(
        plugin_id: &'a str,
        method: &'a str,
        capability: &'a str,
        call_id: &'a str,
        decision: &'a str,
    ) -> Self {
        Self {
            plugin_id,
            method,
            capability,
            call_id,
            decision,
            target: None,
            scope_key: None,
        }
    }

    fn target(mut self, target: Option<&'a str>) -> Self {
        self.target = target;
        self
    }

    fn scope_key(mut self, scope_key: Option<&'a str>) -> Self {
        self.scope_key = scope_key;
        self
    }
}

impl FsHostApi {
    pub fn new() -> Self {
        Self {
            root: Arc::new(RwLock::new(None)),
            granted: Arc::new(RwLock::new(HashSet::new())),
            by_plugin: Arc::new(RwLock::new(HashMap::new())),
            store: Arc::new(RwLock::new(None)),
            quota: Mutex::new(HashMap::new()),
            approver: RwLock::new(None),
            write_always: RwLock::new(HashSet::new()),
            calls: AtomicU64::new(0),
            temps: AtomicU64::new(0),
            plugins: RwLock::new(None),
            shell: RwLock::new(None),
        }
    }

    /// 回填装载表句柄（`AppState::new` 建好 `Host` 后调用一次）。
    pub fn set_plugins(&self, plugins: &Arc<Host>) {
        if let Ok(mut slot) = self.plugins.write() {
            *slot = Some(Arc::downgrade(plugins));
        }
    }

    /// 装上外壳窗口的事件出口（`AppState::attach_peer` 调一次）。
    ///
    /// 后装覆盖先装：外壳是「当前连着的那个窗口」，旧窗口关了就该失效，留着
    /// 一条指向它的通道只会把事件投进空气。
    pub fn set_shell_events(&self, sink: Arc<dyn ShellEvents>) {
        if let Ok(mut slot) = self.shell.write() {
            *slot = Some(sink);
        }
    }

    /// 直接拿外壳的 stdio 对端接线（`AppState::attach_peer` 的便捷入口）。
    ///
    /// 不在 Tokio 运行时里时返回 `false`（纯单测常见）：外壳通道缺席不影响
    /// 插件订阅投递，所以这里只是「没接上」，不是错误。
    pub fn attach_shell_events(&self, peer: Arc<Peer>) -> bool {
        match StdioShellEvents::new(peer) {
            Some(sink) => {
                self.set_shell_events(Arc::new(sink));
                true
            }
            None => false,
        }
    }

    /// 把一条宿主侧事件投给订阅了它的插件（没回填装载表时静默跳过）。
    fn publish_event(&self, event: &str, payload: Value) -> usize {
        let host = self
            .plugins
            .read()
            .ok()
            .and_then(|slot| slot.as_ref().and_then(Weak::upgrade));
        match host {
            Some(host) => host.publish(event, payload),
            None => 0,
        }
    }

    /// 把一条宿主事件投给外壳窗口（没接线时静默跳过）。
    ///
    /// 与 [`Self::publish_event`] 是两条独立的路：前者是「给订阅了的插件」，
    /// 后者是「给用户眼前的窗口」。`plugin.progress` 两边都走，但出口不同，
    /// 少任何一条都不该拖住另一条。
    fn emit_shell(&self, event: &str, payload: Value) {
        let sink = self.shell.read().ok().and_then(|slot| slot.clone());
        if let Some(sink) = sink {
            sink.emit(event, payload);
        }
    }

    /// 装上写侧审批裁决方（`AppState` 接线 / 单测注入）。
    pub fn set_write_approver(&self, approver: Arc<dyn WriteApprover>) {
        if let Ok(mut slot) = self.approver.write() {
            *slot = Some(approver);
        }
    }

    /// 用一份完整的「总是允许」集合覆盖当前集合（装载 / 撤销后刷新用）。
    ///
    /// 覆盖而不是追加：撤销一条规则后必须能真的忘掉，追加语义做不到这一点。
    pub fn seed_write_always(&self, keys: Vec<String>) {
        if let Ok(mut slot) = self.write_always.write() {
            *slot = keys
                .into_iter()
                .map(|key| write_scope_key(Path::new(&key)))
                .collect();
        }
    }

    /// 记一条「总是允许」的写侧范围。
    pub fn remember_write_always(&self, scope_key: &str) {
        if let Ok(mut slot) = self.write_always.write() {
            slot.insert(scope_key.to_string());
        }
    }

    /// 该 `scope_key` 是否已被「总是允许」。
    pub fn is_write_always(&self, scope_key: &str) -> bool {
        self.write_always
            .read()
            .map(|slot| slot.contains(scope_key))
            .unwrap_or(false)
    }

    fn approver_snapshot(&self) -> Option<Arc<dyn WriteApprover>> {
        self.approver.read().ok().and_then(|slot| slot.clone())
    }

    /// 装上会话库（`AppState::install_store` 调一次）。审计全靠它，没装就静默跳过。
    pub fn set_store(&self, store: Store) {
        if let Ok(mut slot) = self.store.write() {
            *slot = Some(store);
        }
    }

    /// 换工作区根。`ws.open` 传 `Some(root)`，`ws.close` 传 `None`。
    pub fn set_root(&self, root: Option<PathBuf>) {
        if let Ok(mut slot) = self.root.write() {
            *slot = root;
        }
    }

    /// 设置缺省授权集（不区分插件时的兜底）。
    pub fn set_granted(&self, caps: Vec<String>) {
        if let Ok(mut slot) = self.granted.write() {
            *slot = caps.into_iter().collect();
        }
    }

    /// 按插件刷新授权集（`plugin.load` 成功后按账本调一次）。
    pub fn set_granted_for(&self, plugin_id: &str, caps: Vec<String>) {
        // 顺手把配额归零：新装载的插件不该背着上一轮的计数。
        if let Ok(mut map) = self.quota.lock() {
            map.insert(plugin_id.to_string(), 0);
        }
        if let Ok(mut slot) = self.by_plugin.write() {
            slot.insert(plugin_id.to_string(), caps.into_iter().collect());
        }
    }

    /// 当前工作区根（测试与审计用）。
    pub fn root(&self) -> Option<PathBuf> {
        self.root.read().ok().and_then(|slot| slot.clone())
    }

    /// 工作区根的共享槽位。
    ///
    /// 审批方必须拿到槽位而不是快照：`ws.open` / `ws.close` 会随时换根，拷一份
    /// 进来就会把「用户批了 A 区的文件」记到 B 区名下。
    pub fn root_handle(&self) -> Arc<RwLock<Option<PathBuf>>> {
        Arc::clone(&self.root)
    }

    /// 会话库的共享槽位（同上：库可能晚装，审批方必须按需取，不能拷快照）。
    pub fn store_handle(&self) -> Arc<RwLock<Option<Store>>> {
        Arc::clone(&self.store)
    }

    /// 某插件在当前工具调用内已用的宿主调用次数（测试用）。
    pub fn quota_used(&self, plugin_id: &str) -> usize {
        self.quota
            .lock()
            .map(|map| map.get(plugin_id).copied().unwrap_or(0))
            .unwrap_or(0)
    }

    fn store_snapshot(&self) -> Option<Store> {
        self.store.read().ok().and_then(|slot| slot.clone())
    }

    /// 命令侧能力闸门用（P5 切片二）：与 `host/*` 同一份授权表，同一套口径。
    ///
    /// 不复制一份「命令授权集」：命令能做的事都在插件进程里，最终还是要落到
    /// `host/*` 上；两套表只会在「命令放行、host 拒绝」时造出一半成功的怪状态。
    pub fn has_capability(&self, plugin_id: &str, capability: &str) -> bool {
        self.is_granted(plugin_id, capability)
    }

    /// 该插件是否被授予了这项能力。
    ///
    /// 先看按插件登记的授权；没有登记（老调用方 / 单测直连）再退回缺省集。
    fn is_granted(&self, plugin_id: &str, capability: &str) -> bool {
        if let Ok(map) = self.by_plugin.read() {
            if let Some(caps) = map.get(plugin_id) {
                return caps.contains(capability);
            }
        }
        self.granted
            .read()
            .map(|caps| caps.contains(capability))
            .unwrap_or(false)
    }

    /// 配额闸门：还有额度就 +1 并返回 `true`。
    fn take_quota(&self, plugin_id: &str) -> bool {
        let Ok(mut map) = self.quota.lock() else {
            // 锁中毒：宁可放行一次也不要把插件永久卡死；次数上限本身不是安全边界。
            return true;
        };
        let used = map.entry(plugin_id.to_string()).or_insert(0);
        if *used >= QUOTA_PER_CALL {
            return false;
        }
        *used += 1;
        true
    }

    /// 落一行 `plugin.host_call` 审计。
    ///
    /// `session_id` 暂为空：P3.5 的宿主调用发生在插件工具执行内部，这条路径上
    /// 拿不到「当前会话 id」（工具上下文里没有它），编一个假 id 比留空更坏。
    fn audit(&self, entry: HostCallAudit<'_>) {
        let Some(store) = self.store_snapshot() else {
            return;
        };
        let HostCallAudit {
            plugin_id,
            method,
            capability,
            call_id,
            decision,
            target,
            scope_key,
        } = entry;
        let mut detail = json!({
            "pluginId": plugin_id,
            // `action` 与 `method` 同值：P4 切片四把能力变更 / 工具调用 / 生命周期
            // 的明细字段统一成 `pluginId / action / target / decision / callId`，
            // 审计面板一条渲染逻辑就能覆盖全部插件事件。`method` 保留是为了不
            // 打破 P3.5 已有的查询与测试。
            "action": method,
            "method": method,
            "capability": capability,
            "target": target,
            "decision": decision,
            "callId": call_id,
        });
        // 写侧才带 `scopeKey`：读侧没有「按文件授权」这回事，塞一个空字段
        // 只会让审计查询多一个永远为 null 的列。
        if let Some(key) = scope_key {
            detail["scopeKey"] = json!(key);
        }
        let event = NewAuditEvent::by(
            audit_actor::PLUGIN,
            audit_kind::PLUGIN_HOST_CALL,
            format!("插件 {plugin_id} 调用 {method}：{decision}"),
        )
        .with_detail(detail);

        // 异步落库。拿不到运行时（同步单测、非 Tokio 线程）就跳过：这条路径上
        // 绝不能 `block_on`，那会把插件的帧泵连同回包一起卡住。
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            if let Err(err) = store.record_audit(event).await {
                tracing::warn!(error = %err, "写插件宿主调用审计失败");
            }
        });
    }

    /// 路径闸门 + 规范化。返回可用的绝对路径。
    ///
    /// 顺序：形状（相对 / 无 `..` / 无盘符 UNC / 无 NUL）→ 敏感段 → 规范化后
    /// 复核前缀与敏感段。最后一步不能省：`link/.ssh` 这种「先符号链接再进敏感
    /// 目录」的写法，只有规范化之后才看得见。
    fn resolve(&self, raw: &str) -> Result<PathBuf, HostApiError> {
        if raw.contains('\0') {
            return Err(HostApiError::invalid_params("path 含 NUL 字符"));
        }
        let trimmed = raw.trim();
        let relative = Path::new(trimmed);
        if relative.is_absolute() {
            return Err(HostApiError::path_out_of_workspace(trimmed));
        }
        for component in relative.components() {
            match component {
                Component::ParentDir => return Err(HostApiError::path_out_of_workspace(trimmed)),
                Component::RootDir | Component::Prefix(_) => {
                    return Err(HostApiError::path_out_of_workspace(trimmed));
                }
                Component::Normal(segment) => {
                    if is_sensitive(&segment.to_string_lossy()) {
                        return Err(HostApiError::path_denied(trimmed));
                    }
                }
                Component::CurDir => {}
            }
        }

        let root = self.root().ok_or_else(|| {
            HostApiError::new(
                HostApiError::WORKSPACE_NOT_OPENED,
                "尚未打开工作区，宿主读能力不可用",
            )
        })?;
        // 根目录必须能规范化：根都没了还把相对路径拼上去做词法比较，
        // 会把「工作区不在了」错报成「路径越界」。
        let root_abs = root.canonicalize().map_err(|_| {
            HostApiError::new(
                HostApiError::NOT_FOUND,
                format!("工作区根目录不可用：{}", root.display()),
            )
        })?;
        let joined = root_abs.join(trimmed);
        let abs = canonicalize_lenient(&joined);

        if !within(&root_abs, &abs) {
            return Err(HostApiError::path_out_of_workspace(
                &abs.display().to_string(),
            ));
        }
        // 规范化后复核敏感段：符号链接可能把「看起来干净」的路径带进 `.ssh`。
        if let Ok(rel) = abs.strip_prefix(&root_abs) {
            for component in rel.components() {
                if let Component::Normal(segment) = component {
                    if is_sensitive(&segment.to_string_lossy()) {
                        return Err(HostApiError::path_denied(&rel.display().to_string()));
                    }
                }
            }
        }
        Ok(abs)
    }

    /// 相对工作区根的展示路径（统一正斜杠，审计与回包都用它）。
    fn display_path(&self, abs: &Path) -> String {
        let Some(root) = self.root() else {
            return abs.display().to_string().replace('\\', "/");
        };
        let root_abs = root.canonicalize().unwrap_or(root);
        abs.strip_prefix(&root_abs)
            .map(|rel| rel.display().to_string().replace('\\', "/"))
            .unwrap_or_else(|_| abs.display().to_string().replace('\\', "/"))
    }

    fn fs_read(&self, abs: &Path, params: &Value) -> Result<Value, HostApiError> {
        let max_bytes = match params.get("maxBytes") {
            None | Some(Value::Null) => DEFAULT_READ_BYTES,
            Some(value) => value.as_u64().filter(|n| *n > 0).ok_or_else(|| {
                HostApiError::invalid_params("maxBytes 必须是大于 0 的整数（字节）")
            })?,
        };
        let meta = std::fs::metadata(abs).map_err(|err| io_error(abs, &err))?;
        if meta.is_dir() {
            return Err(HostApiError::new(
                HostApiError::IO,
                format!("{} 是目录，不能按文件读取", self.display_path(abs)),
            ));
        }
        let file = File::open(abs).map_err(|err| io_error(abs, &err))?;
        // 多读 1 字节：靠「读到了第 max+1 个字节」判定被截断，比先 stat 再读更准
        // （文件可能在 stat 与 read 之间长大）。
        let mut buffer = Vec::new();
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut buffer)
            .map_err(|err| io_error(abs, &err))?;
        let truncated = buffer.len() as u64 > max_bytes;
        let keep = (buffer.len() as u64).min(max_bytes) as usize;
        buffer.truncate(keep);
        Ok(json!({
            "path": self.display_path(abs),
            "content": String::from_utf8_lossy(&buffer).to_string(),
            "bytes": buffer.len(),
            "truncated": truncated,
        }))
    }

    fn fs_list(&self, abs: &Path) -> Result<Value, HostApiError> {
        let meta = std::fs::metadata(abs).map_err(|err| io_error(abs, &err))?;
        if !meta.is_dir() {
            return Err(HostApiError::new(
                HostApiError::IO,
                format!("{} 不是目录", self.display_path(abs)),
            ));
        }
        let mut entries = Vec::new();
        for item in std::fs::read_dir(abs).map_err(|err| io_error(abs, &err))? {
            let item = item.map_err(|err| io_error(abs, &err))?;
            let name = item.file_name().to_string_lossy().to_string();
            // 敏感项连名字都不给：`.env` / `.ssh` 出现在列表里，等于把「这里有个
            // 密钥文件」当情报送出去，而插件并没有读它的能力。
            if is_sensitive(&name) {
                continue;
            }
            let file_type = item.file_type().map_err(|err| io_error(abs, &err))?;
            let dir = file_type.is_dir();
            let size = if dir {
                0
            } else {
                item.metadata().map(|meta| meta.len()).unwrap_or(0)
            };
            entries.push(json!({ "name": name, "dir": dir, "size": size }));
        }
        // 目录顺序在文件系统里是随机的：不排序的话同一个目录两次列出来不一样，
        // 模型会以为「文件变了」。
        entries.sort_by(|left, right| {
            let left = left["name"].as_str().unwrap_or("").to_lowercase();
            let right = right["name"].as_str().unwrap_or("").to_lowercase();
            left.cmp(&right)
        });
        Ok(json!({ "path": self.display_path(abs), "entries": entries }))
    }

    fn fs_stat(&self, abs: &Path) -> Result<Value, HostApiError> {
        let meta = std::fs::metadata(abs).map_err(|err| io_error(abs, &err))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|gap| gap.as_millis() as i64)
            .unwrap_or(0);
        Ok(json!({
            "path": self.display_path(abs),
            "size": meta.len(),
            "mtime": mtime,
            "dir": meta.is_dir(),
        }))
    }

    /// 命令侧路径闸门（P5 切片二）：把清单 `pathField` 指的那个入参取出来过
    /// [`Self::resolve`]，返回展示路径。
    ///
    /// 与 `host/fs.*` 共用同一个 `resolve`——不能给命令开一条更宽的路径检查，
    /// 否则「用命令绕过 fs.write 的路径闸门」就是现成的漏洞。
    pub fn command_path(
        &self,
        args: &Value,
        path_field: &str,
    ) -> Result<Option<String>, HostApiError> {
        let Some(raw) = args.get(path_field) else {
            return Ok(None);
        };
        match raw {
            Value::Null => Ok(None),
            Value::String(text) => self.resolve(text).map(|abs| Some(self.display_path(&abs))),
            _ => Err(HostApiError::invalid_params(format!(
                "{path_field} 必须是字符串路径"
            ))),
        }
    }

    /// 命令审批闸门（P5 切片二，docs/14 §8.1 ⑤ 在命令上的同一条）。
    ///
    /// 只有 `risk=write` 的命令走这里（读命令不该弹窗，否则命令面板会变成
    /// 「点一次弹一次」的灾难）。沿用写侧的「总是允许」集合，但落键换成
    /// `plugin.command:<plugin>:<command>[:<path>]`，粒度是命令而不是文件。
    pub fn approve_command(
        &self,
        plugin_id: &str,
        method: &str,
        command: &str,
        path: Option<&str>,
        scope_key: &str,
    ) -> Result<(), HostApiError> {
        if self.is_write_always(scope_key) {
            return Ok(());
        }
        let request = CommandApprovalRequest {
            plugin_id: plugin_id.to_string(),
            command: command.to_string(),
            path: path.map(str::to_string),
            scope_key: scope_key.to_string(),
        };
        let decision = self
            .approver_snapshot()
            .and_then(|approver| approver.decide_command(&request));
        let call_id = format!("host-{}", self.calls.fetch_add(1, Ordering::Relaxed) + 1);
        // 审计的 target 取「哪个文件」而不是「哪条命令」：命令名在 summary 里，
        // 而审批的争议点永远是「它想动我哪个文件」。没声明路径时才退回命令名。
        //
        // 与 [`Self::approve_write`] 一样用嵌套 fn 而不是闭包：这里要显式声明
        // 「返回值的生命周期跟着入参走」，闭包写不出这个约束（会报
        // `lifetime may not live long enough`）。
        fn command_audit<'a>(
            plugin_id: &'a str,
            method: &'a str,
            command: &'a str,
            path: Option<&'a str>,
            scope_key: &'a str,
            call_id: &'a str,
            decision: &'a str,
        ) -> HostCallAudit<'a> {
            HostCallAudit::new(plugin_id, method, "fs.write", call_id, decision)
                .target(Some(path.unwrap_or(command)))
                .scope_key(Some(scope_key))
        }
        match decision {
            Some(WriteApproval::AllowAlways) => {
                self.remember_write_always(scope_key);
                self.audit(command_audit(
                    plugin_id,
                    method,
                    command,
                    path,
                    scope_key,
                    &call_id,
                    DECISION_APPROVE,
                ));
                Ok(())
            }
            Some(WriteApproval::AllowOnce) => {
                self.audit(command_audit(
                    plugin_id,
                    method,
                    command,
                    path,
                    scope_key,
                    &call_id,
                    DECISION_APPROVE,
                ));
                Ok(())
            }
            Some(WriteApproval::Reject) | None => {
                self.audit(command_audit(
                    plugin_id,
                    method,
                    command,
                    path,
                    scope_key,
                    &call_id,
                    DECISION_REJECT,
                ));
                Err(HostApiError::tool_not_allowed(method, scope_key))
            }
        }
    }

    /// 写侧审批闸门（docs/14 §8.1 ⑤）。
    ///
    /// 命中「总是允许」不打搅用户；否则交给 [`WriteApprover`] 当场裁决。
    /// 裁决方缺失、没答复、明确拒绝，一律 3001——写盘的默认值只能是「不写」。
    fn approve_write(
        &self,
        plugin_id: &str,
        method: &str,
        path: &str,
        scope_key: &str,
        call_id: &str,
    ) -> Result<(), HostApiError> {
        if self.is_write_always(scope_key) {
            // 之前已经批过这个文件：不再打扰用户，也不重复记 `approve`
            // （审计里那一条 approve 已经存在，重复记会让「批过几次」失真）。
            return Ok(());
        }
        let request = WriteApprovalRequest {
            plugin_id: plugin_id.to_string(),
            method: method.to_string(),
            path: path.to_string(),
            scope_key: scope_key.to_string(),
        };
        let decision = self
            .approver_snapshot()
            .and_then(|approver| approver.decide(&request));
        // 写侧审计行形状固定（capability 恒为 fs.write，target 与 scopeKey 恒有值），
        // 只有 decision 不同；用嵌套 fn 而不是闭包，是因为这里要显式声明
        // 「返回值的生命周期跟着入参走」，闭包写不出这个约束。
        fn approval_audit<'a>(
            plugin_id: &'a str,
            method: &'a str,
            path: &'a str,
            scope_key: &'a str,
            call_id: &'a str,
            decision: &'a str,
        ) -> HostCallAudit<'a> {
            HostCallAudit::new(plugin_id, method, "fs.write", call_id, decision)
                .target(Some(path))
                .scope_key(Some(scope_key))
        }
        match decision {
            Some(WriteApproval::AllowAlways) => {
                self.remember_write_always(scope_key);
                self.audit(approval_audit(
                    plugin_id,
                    method,
                    path,
                    scope_key,
                    call_id,
                    DECISION_APPROVE,
                ));
                Ok(())
            }
            Some(WriteApproval::AllowOnce) => {
                self.audit(approval_audit(
                    plugin_id,
                    method,
                    path,
                    scope_key,
                    call_id,
                    DECISION_APPROVE,
                ));
                Ok(())
            }
            Some(WriteApproval::Reject) | None => {
                self.audit(approval_audit(
                    plugin_id,
                    method,
                    path,
                    scope_key,
                    call_id,
                    DECISION_REJECT,
                ));
                Err(HostApiError::tool_not_allowed(method, scope_key))
            }
        }
    }

    /// 把 `content` + `encoding` 解成待写字节。
    ///
    /// 这一段的错误全是 `-32602`，且它必须在审批之前跑完：否则一个畸形请求也会
    /// 弹出审批框，用户批了才发现参数根本不对——白打扰一次。
    fn write_payload(params: &Value) -> Result<Vec<u8>, HostApiError> {
        let content = match params.get("content") {
            Some(Value::String(text)) => text,
            None | Some(Value::Null) => {
                return Err(HostApiError::invalid_params("缺少 content 参数"))
            }
            Some(_) => return Err(HostApiError::invalid_params("content 必须是字符串")),
        };
        let encoding = match params.get("encoding") {
            None | Some(Value::Null) => "utf8",
            Some(Value::String(text)) => text.as_str(),
            Some(_) => return Err(HostApiError::invalid_params("encoding 必须是字符串")),
        };
        match encoding.to_ascii_lowercase().as_str() {
            "utf8" | "utf-8" => Ok(content.as_bytes().to_vec()),
            // 插件写二进制（图片、导出包）不该被迫做十六进制，给 base64 就够：
            // 帧是 JSON，塞不下裸字节。
            "base64" => BASE64.decode(content).map_err(|err| {
                HostApiError::invalid_params(format!("content 不是合法 base64：{err}"))
            }),
            other => Err(HostApiError::invalid_params(format!(
                "不支持的 encoding：{other}（支持 utf8 / base64）"
            ))),
        }
    }

    /// `host/fs.write`：临时文件 → `fsync` → `rename`。
    ///
    /// `rename` 只在同卷上原子，所以临时文件必须落在**目标同目录**；先 `fsync`
    /// 是为了让「rename 成功」真的等于「内容已经落盘」，否则掉电后可能留下一个
    /// 名字对、内容为空的文件——比半个文件更难查。
    ///
    /// 参数解码与「目标是目录」的检查都在审批之前做完（见 [`Self::write_payload`]
    /// 与 `call` 里的写侧闸门），进到这里的只剩「真的要落盘」这一件事。
    fn fs_write(&self, abs: &Path, bytes: &[u8], existed: bool) -> Result<Value, HostApiError> {
        let display = self.display_path(abs);
        let parent = abs.parent().ok_or_else(|| {
            HostApiError::new(HostApiError::IO, format!("{display} 没有可写入的父目录"))
        })?;
        // 目录不存在就补建：v1 没有 `fs.mkdir`，不补建的话插件连新建目录下的
        // 第一个文件都写不了。范围已经过路径闸门，建目录不会越出工作区。
        std::fs::create_dir_all(parent).map_err(|err| write_error(&display, &err))?;

        let seq = self.temps.fetch_add(1, Ordering::Relaxed) + 1;
        let temp = parent.join(format!(
            "{WRITE_TEMP_PREFIX}-{}-{seq}.tmp",
            std::process::id()
        ));
        let written = (|| -> std::io::Result<()> {
            let mut file = File::create(&temp)?;
            file.write_all(bytes)?;
            file.sync_all()
        })();
        if let Err(err) = written {
            remove_temp(&temp);
            return Err(write_error(&display, &err));
        }
        if let Err(err) = std::fs::rename(&temp, abs) {
            // 失败就别把临时文件留给用户：否则工作区里会慢慢堆出
            // `.aiide-write-*.tmp`，而且 `fs.list` 会把它们当正常文件报出去。
            remove_temp(&temp);
            return Err(write_error(&display, &err));
        }

        // 插件写盘也是「保存」：其它订阅了 `file.saved` 的插件应当同样感知到，
        // 不能只有编辑器那条路径会发事件。载荷只有路径，不带正文。
        self.publish_event(
            ai_ide_plugin::events::FILE_SAVED,
            ai_ide_plugin::events::file_saved_payload(&display),
        );

        Ok(json!({
            "path": display,
            "bytes": bytes.len(),
            "created": !existed,
        }))
    }

    fn workspace_info(&self) -> Result<Value, HostApiError> {
        let root = self.root().ok_or_else(|| {
            HostApiError::new(
                HostApiError::WORKSPACE_NOT_OPENED,
                "尚未打开工作区，宿主读能力不可用",
            )
        })?;
        let name = root
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| root.display().to_string());
        Ok(json!({
            "root": self.display_path(&root),
            "name": name,
        }))
    }

    fn search_content(&self, params: &Value) -> Result<Value, HostApiError> {
        let query = params
            .get("query")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .unwrap_or("");
        if query.is_empty() {
            return Err(HostApiError::invalid_params("query 不能为空"));
        }
        let max_results = match params.get("maxResults") {
            None | Some(Value::Null) => DEFAULT_SEARCH_RESULTS,
            Some(value) => value
                .as_u64()
                .filter(|n| *n > 0)
                .map(|n| n as usize)
                .ok_or_else(|| HostApiError::invalid_params("maxResults 必须是大于 0 的整数"))?,
        };
        let root = self.root().ok_or_else(|| {
            HostApiError::new(
                HostApiError::WORKSPACE_NOT_OPENED,
                "尚未打开工作区，宿主读能力不可用",
            )
        })?;
        let root_abs = root.canonicalize().map_err(|_| {
            HostApiError::new(
                HostApiError::NOT_FOUND,
                format!("工作区根目录不可用：{}", root.display()),
            )
        })?;

        let needle = query.to_lowercase();
        let mut matches: Vec<Value> = Vec::new();
        let mut truncated = false;
        let mut stack = vec![root_abs.clone()];

        'walk: while let Some(dir) = stack.pop() {
            let Ok(items) = std::fs::read_dir(&dir) else {
                // 单个目录读不动（权限）不该让整次搜索失败：跳过它，继续找别的。
                continue;
            };
            for item in items.flatten() {
                let path = item.path();
                let name = item.file_name().to_string_lossy().to_string();
                if is_sensitive(&name) {
                    continue;
                }
                let Ok(file_type) = item.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    if SKIP_DIRS
                        .iter()
                        .any(|skip| skip.eq_ignore_ascii_case(&name))
                    {
                        continue;
                    }
                    stack.push(path);
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                let Some(text) = read_text_file(&path) else {
                    continue;
                };
                let rel = path
                    .strip_prefix(&root_abs)
                    .map(|rel| rel.display().to_string().replace('\\', "/"))
                    .unwrap_or_else(|_| path.display().to_string().replace('\\', "/"));
                for (index, line) in text.lines().enumerate() {
                    if !line.to_lowercase().contains(&needle) {
                        continue;
                    }
                    matches.push(json!({
                        "path": rel,
                        "line": index + 1,
                        "preview": preview(line),
                    }));
                    if matches.len() >= max_results {
                        truncated = true;
                        break 'walk;
                    }
                }
            }
        }

        Ok(json!({ "matches": matches, "truncated": truncated }))
    }

    /// 等待审计落库到「出现满足条件的行」为止（单测用）。
    ///
    /// 审计是 `spawn` 出去异步落的，且一次工具调用会连写很多行——只等「有行」会
    /// 拿到前几行就返回，断言 `quota_exceeded` 这类**靠后**的判定就会假失败。
    #[cfg(test)]
    async fn wait_audit(
        store: &Store,
        want: impl Fn(&ai_ide_store::AuditEvent) -> bool,
    ) -> Vec<ai_ide_store::AuditEvent> {
        let query = |limit: i64| {
            ai_ide_store::AuditQuery::new()
                .kind(audit_kind::PLUGIN_HOST_CALL)
                .with_limit(limit)
        };
        for _ in 0..200 {
            let rows = store.list_audit(query(500)).await.unwrap_or_default();
            if rows.iter().any(&want) {
                return rows;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        store.list_audit(query(500)).await.unwrap_or_default()
    }
}

impl HostApi for FsHostApi {
    fn call(&self, plugin_id: &str, method: &str, params: &Value) -> Result<Value, HostApiError> {
        let Some(capability) = capability_of(method) else {
            // 不做审计：连方法都不存在，谈不上「插件用了什么能力」。
            return Err(HostApiError::new(
                HostApiError::METHOD_NOT_FOUND,
                format!("v1 未实现该宿主能力：{method}"),
            ));
        };
        let call_id = format!("host-{}", self.calls.fetch_add(1, Ordering::Relaxed) + 1);

        // 闸门 ①：能力。
        if !self.is_granted(plugin_id, capability) {
            self.audit(
                HostCallAudit::new(plugin_id, method, capability, &call_id, DECISION_DENY)
                    .target(audit_target(params)),
            );
            return Err(HostApiError::unauthorized(method, capability));
        }

        // 闸门 ②：路径（`workspace.info` / `search.content` 没有入参路径，
        // 搜索的遍历范围就是工作区根，越界问题由遍历本身保证）。
        let path_method = matches!(
            method,
            "host/fs.read" | "host/fs.list" | "host/fs.stat" | "host/fs.write"
        );
        let resolved = if path_method {
            match params.get("path") {
                Some(Value::String(raw)) => self.resolve(raw).map(Some),
                None | Some(Value::Null) => Err(HostApiError::invalid_params("缺少 path 参数")),
                Some(_) => Err(HostApiError::invalid_params("path 必须是字符串")),
            }
        } else {
            Ok(None)
        };
        let abs = match resolved {
            Ok(abs) => abs,
            Err(err) => {
                self.audit(
                    HostCallAudit::new(plugin_id, method, capability, &call_id, DECISION_DENY)
                        .target(audit_target(params)),
                );
                return Err(err);
            }
        };

        // 闸门 ③：审批（写侧专属）。顺序是「参数解码 → 目标形状 → 审批」：
        // 先问用户、再发现参数畸形，等于拿审批弹窗当校验器，白打扰一次；
        // 而路径必须更早过闸门，否则弹窗就成了「这个路径敏不敏感」的探测器。
        let mut scope_key: Option<String> = None;
        let mut write_payload: Option<(Vec<u8>, bool)> = None;
        if method == "host/fs.write" {
            if let Some(path) = abs.as_deref() {
                let display = self.display_path(path);
                // 写侧所有「还没到审批就失败」的分支，审计形状都一样：记 deny +
                // 目标路径，好让审计里能看出「这次为什么没问用户」。
                let deny = |err: HostApiError| -> HostApiError {
                    self.audit(
                        HostCallAudit::new(plugin_id, method, capability, &call_id, DECISION_DENY)
                            .target(Some(&display)),
                    );
                    err
                };
                let bytes = Self::write_payload(params).map_err(deny)?;
                if path.is_dir() {
                    return Err(deny(HostApiError::new(
                        HostApiError::IO,
                        format!("{display} 是目录，不能按文件写入"),
                    )));
                }
                let key = write_scope_key(path);
                self.approve_write(plugin_id, method, &display, &key, &call_id)?;
                scope_key = Some(key);
                write_payload = Some((bytes, path.exists()));
            }
        }

        // 闸门 ④：配额（200 次/工具调用）。
        if !self.take_quota(plugin_id) {
            self.audit(
                HostCallAudit::new(
                    plugin_id,
                    method,
                    capability,
                    &call_id,
                    DECISION_QUOTA_EXCEEDED,
                )
                .target(audit_target(params))
                .scope_key(scope_key.as_deref()),
            );
            return Err(HostApiError::new(
                HostApiError::QUOTA_EXCEEDED,
                format!("单次工具调用的宿主调用次数已达上限（{QUOTA_PER_CALL} 次）"),
            ));
        }

        let target = match abs.as_deref() {
            Some(path) => Some(self.display_path(path)),
            None => audit_target(params).map(str::to_string),
        };
        let outcome = match (abs.as_deref(), method) {
            (Some(path), "host/fs.read") => self.fs_read(path, params),
            (Some(path), "host/fs.list") => self.fs_list(path),
            (Some(path), "host/fs.stat") => self.fs_stat(path),
            (Some(path), "host/fs.write") => match write_payload {
                Some((bytes, existed)) => self.fs_write(path, &bytes, existed),
                // 走不到：写侧的 payload 在上面的闸门里已经备好。
                None => Err(HostApiError::new(
                    HostApiError::METHOD_NOT_FOUND,
                    "写侧参数缺失",
                )),
            },
            (None, "host/workspace.info") => self.workspace_info(),
            (None, "host/search.content") => self.search_content(params),
            (_, other) => Err(HostApiError::new(
                HostApiError::METHOD_NOT_FOUND,
                format!("v1 未实现该宿主能力：{other}"),
            )),
        };
        let decision = if outcome.is_ok() {
            DECISION_ALLOW
        } else {
            DECISION_ERROR
        };
        self.audit(
            HostCallAudit::new(plugin_id, method, capability, &call_id, decision)
                .target(target.as_deref())
                .scope_key(scope_key.as_deref()),
        );
        outcome
    }

    fn begin_call(&self, plugin_id: &str) {
        if let Ok(mut map) = self.quota.lock() {
            map.insert(plugin_id.to_string(), 0);
        }
    }

    /// `host/log` / `host/progress`：旁路遥测。
    ///
    /// 两者都不是「取数据」的通道，闸门却不一样：`host/log` 不要能力（写日志没有
    /// 额外信息面）；`host/progress` 要 `progress` 能力——它会推给**外壳窗口**
    /// （以及订阅 `plugin.progress` 的插件），是一条能让插件在用户眼前说话的
    /// 通道，未授权的插件不该有它，所以照样走 §8.1 ③ 的能力闸门（6103）。
    ///
    /// 载荷在这一层重新拼，不沿用 `params`：进度只允许 `{pluginId,token,message,
    /// percent,truncated}`，超上限截断并标记（见 [`progress_payload`]）。插件往
    /// `params` 里塞的任何别的键（`content` / `fields` / `path`）都到不了事件里——
    /// 「进度是只读遥测、不是文件内容出口」这条约束落在这里，而不是靠插件自觉。
    ///
    /// `pluginId` 一律用这里的 `plugin_id`（来自进程句柄，装载时确定）覆盖插件
    /// 自报的值：载荷里的身份只能是宿主认定的那一个，插件改不了。
    fn notify(&self, plugin_id: &str, method: &str, params: &Value) -> Result<(), HostApiError> {
        match method {
            LOG_METHOD => {
                let level = params
                    .get("level")
                    .and_then(Value::as_str)
                    .unwrap_or("info")
                    .to_string();
                let message = params
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_default();
                let mut payload = json!({
                    "pluginId": plugin_id,
                    "level": level,
                    "message": message,
                });
                // 允许透传结构化字段，但只放行白名单键：`pluginId` 必须最后由
                // 宿主写，不能让 `params` 里同名的键把它顶掉。
                if let (Some(extra), Some(object)) =
                    (params.get("fields").cloned(), payload.as_object_mut())
                {
                    if extra.is_object() {
                        object.insert("fields".to_string(), extra);
                    }
                }
                tracing::debug!(plugin_id, %method, "插件日志");
                self.publish_event(PLUGIN_LOG, payload);
                Ok(())
            }
            PROGRESS_METHOD => {
                if !self.is_granted(plugin_id, PROGRESS_CAPABILITY) {
                    return Err(HostApiError::unauthorized(method, PROGRESS_CAPABILITY));
                }
                let message = params.get("message").and_then(Value::as_str);
                if message.is_none() {
                    // 没有消息的进度条对用户毫无信息量，也会让收件方分不清
                    // 「插件没说话」和「插件报了空」。空串按「没写」处理。
                    return Err(HostApiError::new(
                        HostApiError::INVALID_PARAMS,
                        "host/progress 需要 message",
                    ));
                }
                let payload = progress_payload(
                    plugin_id,
                    params.get("token").and_then(Value::as_str),
                    message,
                    params.get("percent").and_then(Value::as_f64),
                );
                let forwarded = self.publish_event(PLUGIN_PROGRESS, payload.clone());
                // 订阅是插件之间的事，用户看到的进度条不依赖它：外壳窗口是主路径，
                // 一条订阅都没有时也照推（`delivered` 为 0 只是「没插件要」）。
                self.emit_shell(PLUGIN_PROGRESS, payload);
                tracing::debug!(plugin_id, forwarded, "插件进度");
                Ok(())
            }
            _ => Err(HostApiError::method_not_found(method)),
        }
    }
}

/// 外壳窗口的事件出口。
///
/// 插件事件有两个收件人：订阅了的插件（`plugin/event` 帧）与**已打开的外壳
/// 窗口**（`plugin.progress` 这类要给人看的东西）。后者与 `subscriptions` 无关
/// ——一个插件都没订阅时，进度条也得动。
///
/// 实现方必须**立即返回**：`emit` 跑在插件 stdout 的帧泵里，阻塞等于卡死该
/// 插件的所有回包（一条慢的窗口写会把工具调用拖到 120 s 硬墙）。所以异步写出
/// 由实现方负责，本 trait 只保证「投出去」这一件事。
pub trait ShellEvents: Send + Sync {
    fn emit(&self, event: &str, payload: Value);
}

/// 写到外壳 stdio 的 [`ShellEvents`] 实现（`AppState::attach_peer` 装一次）。
///
/// 拿住 `Handle` 而不是在 `emit` 里现找运行时：调用点在插件的 Tokio 任务里没错，
/// 但把「有没有运行时」这件事留到运行期判断，会让失败变成静默丢事件。
pub struct StdioShellEvents {
    peer: Arc<Peer>,
    handle: tokio::runtime::Handle,
}

impl StdioShellEvents {
    /// 当前运行时下建一个；不在 Tokio 运行时里（纯单测）返回 `None`。
    pub fn new(peer: Arc<Peer>) -> Option<Self> {
        tokio::runtime::Handle::try_current()
            .ok()
            .map(|handle| Self { peer, handle })
    }
}

impl ShellEvents for StdioShellEvents {
    fn emit(&self, event: &str, payload: Value) {
        let peer = Arc::clone(&self.peer);
        let event = event.to_string();
        self.handle
            .spawn(async move { peer.notify(&event, &payload).await });
    }
}

/// 审计 `target`：有路径就记路径，搜索记查询词（路径无关）。
fn audit_target(params: &Value) -> Option<&str> {
    params
        .get("path")
        .and_then(|value| value.as_str())
        .or_else(|| params.get("query").and_then(|value| value.as_str()))
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// 敏感段判定：`.git` / `.ssh` / `.aws` / `.kube` 以及任何 `.env` / `.env.*`。
fn is_sensitive(segment: &str) -> bool {
    let lowered = segment.to_ascii_lowercase();
    if SENSITIVE_SEGMENTS.contains(&lowered.as_str()) {
        return true;
    }
    lowered == ".env" || lowered.starts_with(".env.")
}

/// `target` 是否在 `root` 之内（按路径边界比较，避免 `root` 与 `rootx` 混淆）。
fn within(root: &Path, target: &Path) -> bool {
    let root_text = normalize_for_compare(root);
    let target_text = normalize_for_compare(target);
    target_text == root_text || target_text.starts_with(&format!("{root_text}\\"))
}

fn normalize_for_compare(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

/// 词法规范化：只处理 `.` / `..`，不碰文件系统（目标可能还不存在）。
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut parts: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // 只在能回退时才弹栈；弹不动就保留（后续前缀比较会拒掉它）。
                if matches!(parts.last(), Some(Component::Normal(_))) {
                    parts.pop();
                } else {
                    parts.push(component);
                }
            }
            other => parts.push(other),
        }
    }
    let mut out = PathBuf::new();
    for part in parts {
        out.push(part.as_os_str());
    }
    out
}

/// 规范化一个「可能还不存在」的路径。
///
/// `canonicalize` 只认已存在的路径，而写侧大量是新建文件，所以分两段：目标
/// 存在就直接规范化；不存在就把**最深的已存在祖先**规范化，再把剩余段拼回去。
/// 只对整串做词法规范化是不够的——父目录是符号链接、指向工作区外时，
/// 词法路径看着干净，实际写出去就出界了。
fn canonicalize_lenient(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let mut probe = path.to_path_buf();
    let mut rest: Vec<OsString> = Vec::new();
    while let Some(name) = probe.file_name().map(|name| name.to_os_string()) {
        rest.push(name);
        if !probe.pop() {
            break;
        }
        if probe.exists() {
            let mut base = probe.canonicalize().unwrap_or_else(|_| probe.clone());
            for part in rest.iter().rev() {
                base.push(part);
            }
            return normalize_lexically(&base);
        }
    }
    normalize_lexically(path)
}

/// 写盘失败的错误码：父目录不存在给 1004，其余（权限、占用、跨卷）给 1005。
fn write_error(display: &str, err: &std::io::Error) -> HostApiError {
    if err.kind() == std::io::ErrorKind::NotFound {
        return HostApiError::new(HostApiError::NOT_FOUND, format!("路径不存在：{display}"));
    }
    HostApiError::new(HostApiError::IO, format!("写入 {display} 失败：{err}"))
}

/// 收拾临时文件。删不掉也不算错：这次写入本来就要报错了，把临时文件的
/// 错误盖到写入错误上，用户看到的原因会跑偏。
fn remove_temp(temp: &Path) {
    let _ = std::fs::remove_file(temp);
}

/// 把 IO 错误翻译成协议错误码：不存在给 1004，其余（权限 / 设备）给 1005。
fn io_error(path: &Path, err: &std::io::Error) -> HostApiError {
    let display = path.display().to_string().replace('\\', "/");
    if err.kind() == std::io::ErrorKind::NotFound {
        return HostApiError::new(HostApiError::NOT_FOUND, format!("路径不存在：{display}"));
    }
    HostApiError::new(HostApiError::IO, format!("读取 {display} 失败：{err}"))
}

/// 读一个「可能是文本」的文件；二进制（前 8 KB 含 NUL）或超大文件返回 `None`。
fn read_text_file(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_SEARCH_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.iter().take(8192).any(|byte| *byte == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).to_string())
}

/// 命中行的预览：去首尾空白、压到 200 字符以内。
fn preview(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.chars().count() <= 200 {
        return trimmed.to_string();
    }
    trimmed.chars().take(200).collect()
}

// ---------------------------------------------------------------------------
// 单测
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ai_ide_plugin::events::{MAX_PROGRESS_MESSAGE_CHARS, MAX_PROGRESS_TOKEN_CHARS};

    fn sandbox(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|gap| gap.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "aiide-plugin-host-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("建临时目录");
        root
    }

    fn api_with(root: &Path, caps: &[&str]) -> FsHostApi {
        let api = FsHostApi::new();
        api.set_root(Some(root.to_path_buf()));
        api.set_granted(caps.iter().map(|cap| cap.to_string()).collect());
        api
    }

    /// 路径闸门：三种形状各拒一种，且都归到对的错误码上。
    #[test]
    fn path_gate_rejects_absolute_parent_and_sensitive() {
        let root = sandbox("path");
        let api = api_with(&root, &["fs.read"]);

        let absolute = api
            .call("demo", "host/fs.list", &json!({ "path": "C:/Windows" }))
            .expect_err("绝对路径必须拒");
        assert_eq!(absolute.code, HostApiError::PATH_OUT_OF_WORKSPACE);

        let parent = api
            .call("demo", "host/fs.list", &json!({ "path": "../etc" }))
            .expect_err(".. 必须拒");
        assert_eq!(parent.code, HostApiError::PATH_OUT_OF_WORKSPACE);

        let sensitive = api
            .call("demo", "host/fs.read", &json!({ "path": ".git/config" }))
            .expect_err("敏感段必须拒");
        assert_eq!(sensitive.code, HostApiError::PATH_DENIED);

        std::fs::remove_dir_all(&root).ok();
    }

    /// `fs.read` 超上限要如实标 `truncated`，并把 `bytes` 报成实际读到的字节数。
    #[test]
    fn fs_read_marks_truncation() {
        let root = sandbox("read");
        std::fs::write(root.join("big.txt"), "a".repeat(200)).expect("写文件");
        let api = api_with(&root, &["fs.read"]);

        let full = api
            .call("demo", "host/fs.read", &json!({ "path": "big.txt" }))
            .expect("默认上限下应读全");
        assert_eq!(full["bytes"], 200);
        assert_eq!(full["truncated"], false);

        let cut = api
            .call(
                "demo",
                "host/fs.read",
                &json!({ "path": "big.txt", "maxBytes": 10 }),
            )
            .expect("截断不是错误");
        assert_eq!(cut["bytes"], 10);
        assert_eq!(cut["truncated"], true);
        assert_eq!(cut["content"].as_str().unwrap_or("").len(), 10);

        std::fs::remove_dir_all(&root).ok();
    }

    /// `fs.list` 只给一层：子目录里有什么，得再问一次才知道。
    #[test]
    fn fs_list_is_single_level() {
        let root = sandbox("list");
        std::fs::create_dir_all(root.join("src")).expect("建子目录");
        std::fs::write(root.join("src/deep.txt"), "deep").expect("写深文件");
        std::fs::write(root.join("top.txt"), "top").expect("写顶层文件");
        let api = api_with(&root, &["fs.read"]);

        let listing = api
            .call("demo", "host/fs.list", &json!({ "path": "" }))
            .expect("列根目录");
        let entries = listing["entries"].as_array().expect("entries 数组");
        let names: Vec<&str> = entries
            .iter()
            .filter_map(|entry| entry["name"].as_str())
            .collect();
        assert_eq!(names, vec!["src", "top.txt"], "按名字升序，且只有一层");
        assert!(
            entries
                .iter()
                .any(|entry| entry["name"] == "src" && entry["dir"] == true),
            "子目录要带 dir 标记"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// `search.content` 能命中，但绝不进 `node_modules`。
    #[test]
    fn search_content_hits_and_skips_node_modules() {
        let root = sandbox("search");
        std::fs::create_dir_all(root.join("node_modules/pkg")).expect("建 node_modules");
        std::fs::write(root.join("note.md"), "hello HOST capability\nsecond line\n")
            .expect("写文件");
        std::fs::write(root.join("node_modules/pkg/index.js"), "hello HOST\n").expect("写依赖");
        let api = api_with(&root, &["search"]);

        let found = api
            .call("demo", "host/search.content", &json!({ "query": "host" }))
            .expect("搜索应成功");
        let matches = found["matches"].as_array().expect("matches 数组");
        assert_eq!(matches.len(), 1, "只应命中工作区里的那一处：{found}");
        assert_eq!(matches[0]["path"], "note.md");
        assert_eq!(matches[0]["line"], 1);
        assert_eq!(found["truncated"], false);

        std::fs::remove_dir_all(&root).ok();
    }

    /// 未授权 → 6103，且先于配额：连撞配额的机会都不给。
    #[test]
    fn unauthorized_plugin_is_rejected_before_quota() {
        let root = sandbox("deny");
        let api = api_with(&root, &[]);

        for _ in 0..(QUOTA_PER_CALL + 5) {
            let err = api
                .call("demo", "host/fs.list", &json!({ "path": "" }))
                .expect_err("未授权必须拒");
            assert_eq!(err.code, HostApiError::UNAUTHORIZED);
        }
        assert_eq!(
            api.quota_used("demo"),
            0,
            "被能力闸门拦下的调用不该消耗配额"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// 建一个带库的 api，并返回库句柄（审计断言要靠它）。
    async fn api_with_store(root: &Path, caps: &[&str], tag: &str) -> (FsHostApi, Store) {
        let db = root.join(format!("audit-{tag}.db"));
        let store = Store::connect(&db).await.expect("建库");
        let api = api_with(root, caps);
        api.set_store(store.clone());
        (api, store)
    }

    /// 超 200 次 → 6109，且审计里必须留下一行 `quota_exceeded`。
    #[tokio::test]
    async fn quota_is_enforced_and_audited() {
        let root = sandbox("quota");
        std::fs::write(root.join("a.txt"), "x").expect("写文件");
        let (api, store) = api_with_store(&root, &["fs.read"], "quota").await;
        api.begin_call("demo");

        for index in 0..QUOTA_PER_CALL {
            api.call("demo", "host/fs.read", &json!({ "path": "a.txt" }))
                .unwrap_or_else(|err| panic!("第 {index} 次不该失败：{err:?}"));
        }
        let denied = api
            .call("demo", "host/fs.read", &json!({ "path": "a.txt" }))
            .expect_err("第 201 次必须超配额");
        assert_eq!(denied.code, HostApiError::QUOTA_EXCEEDED);

        // 换一次工具调用就该归零。
        api.begin_call("demo");
        assert_eq!(api.quota_used("demo"), 0);

        let rows = FsHostApi::wait_audit(&store, |row| {
            row.detail["decision"] == DECISION_QUOTA_EXCEEDED
        })
        .await;
        assert!(
            rows.iter()
                .any(|row| row.detail["decision"] == DECISION_QUOTA_EXCEEDED),
            "审计里应有 quota_exceeded 行"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// 正常调用的审计行字段齐全，actor 是 plugin，session 留空。
    #[tokio::test]
    async fn successful_call_is_audited_with_all_fields() {
        let root = sandbox("audit");
        std::fs::write(root.join("a.txt"), "hello").expect("写文件");
        let (api, store) = api_with_store(&root, &["fs.read"], "ok").await;

        api.call("demo", "host/fs.read", &json!({ "path": "a.txt" }))
            .expect("应读到文件");

        let rows = FsHostApi::wait_audit(&store, |_| true).await;
        let row = rows.first().expect("应有一行审计");
        assert_eq!(row.actor, audit_actor::PLUGIN);
        assert_eq!(row.kind, audit_kind::PLUGIN_HOST_CALL);
        assert!(row.session_id.is_none(), "P3.5 里 session_id 暂空");
        let detail = &row.detail;
        assert_eq!(detail["pluginId"], "demo");
        assert_eq!(detail["method"], "host/fs.read");
        assert_eq!(detail["capability"], "fs.read");
        assert_eq!(detail["target"], "a.txt");
        assert_eq!(detail["decision"], DECISION_ALLOW);
        assert!(
            detail["callId"].as_str().unwrap_or("").starts_with("host-"),
            "callId 应带上序号：{detail}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// 假审批器：答复固定结论，并记下问过几次、问了什么。
    struct StubApprover {
        reply: Option<WriteApproval>,
        seen: Mutex<Vec<WriteApprovalRequest>>,
    }

    impl StubApprover {
        fn answering(reply: Option<WriteApproval>) -> Arc<Self> {
            Arc::new(Self {
                reply,
                seen: Mutex::new(Vec::new()),
            })
        }

        fn asked(&self) -> Vec<WriteApprovalRequest> {
            self.seen
                .lock()
                .map(|rows| rows.clone())
                .unwrap_or_default()
        }
    }

    impl WriteApprover for StubApprover {
        fn decide(&self, request: &WriteApprovalRequest) -> Option<WriteApproval> {
            if let Ok(mut rows) = self.seen.lock() {
                rows.push(request.clone());
            }
            self.reply
        }
    }

    /// 工作区根目录里的临时文件（`.aiide-write-*.tmp`），用来断言「没留垃圾」。
    fn temp_files(root: &Path) -> Vec<String> {
        std::fs::read_dir(root)
            .map(|items| {
                items
                    .flatten()
                    .map(|item| item.file_name().to_string_lossy().to_string())
                    .filter(|name| name.starts_with(WRITE_TEMP_PREFIX))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 拒绝审批时必须 3001，且**文件一个都不能出现**（连临时文件都不许留）。
    #[test]
    fn fs_write_rejection_leaves_no_file() {
        let root = sandbox("write-reject");
        let api = api_with(&root, &["fs.write"]);
        let approver = StubApprover::answering(Some(WriteApproval::Reject));
        api.set_write_approver(approver.clone());

        let err = api
            .call(
                "demo",
                "host/fs.write",
                &json!({ "path": "note.txt", "content": "hello" }),
            )
            .expect_err("被驳回的写必须失败");
        assert_eq!(err.code, HostApiError::TOOL_NOT_ALLOWED);
        assert!(
            !root.join("note.txt").exists(),
            "审批拒绝后文件不能产生：{:?}",
            std::fs::read_dir(&root)
                .map(|it| it.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
        );
        assert!(temp_files(&root).is_empty(), "临时文件也必须收拾干净");
        assert_eq!(approver.asked().len(), 1, "应该只问一次");
        assert_eq!(
            api.quota_used("demo"),
            0,
            "审批排在配额前面：被驳回的写不该消耗额度"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// 没有装裁决方 = 无人能批准 = 拒（fail closed），文件同样不许出现。
    #[test]
    fn fs_write_without_approver_is_rejected() {
        let root = sandbox("write-noapprover");
        let api = api_with(&root, &["fs.write"]);

        let err = api
            .call(
                "demo",
                "host/fs.write",
                &json!({ "path": "orphan.txt", "content": "x" }),
            )
            .expect_err("没人批准就写不了");
        assert_eq!(err.code, HostApiError::TOOL_NOT_ALLOWED);
        assert!(!root.join("orphan.txt").exists(), "不能落盘");

        std::fs::remove_dir_all(&root).ok();
    }

    /// 一次授权只管一次：第二次写还得再问。
    #[test]
    fn fs_write_allow_once_asks_every_time() {
        let root = sandbox("write-once");
        let api = api_with(&root, &["fs.write"]);
        let approver = StubApprover::answering(Some(WriteApproval::AllowOnce));
        api.set_write_approver(approver.clone());

        for content in ["first", "second"] {
            api.call(
                "demo",
                "host/fs.write",
                &json!({ "path": "note.txt", "content": content }),
            )
            .expect("一次授权放行一次");
        }
        assert_eq!(approver.asked().len(), 2, "AllowOnce 不该被记住");
        assert_eq!(
            std::fs::read_to_string(root.join("note.txt")).expect("读回"),
            "second",
            "后写覆盖前写"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// `always` 按规范化绝对路径记账：同一文件不再问，换文件、换插件都重新问。
    #[test]
    fn fs_write_allow_always_is_remembered_per_file() {
        let root = sandbox("write-always");
        std::fs::create_dir_all(root.join("sub")).expect("建子目录");
        let api = api_with(&root, &["fs.write"]);
        let approver = StubApprover::answering(Some(WriteApproval::AllowAlways));
        api.set_write_approver(approver.clone());

        // 同一个文件的另一种写法（`./` 前缀、反斜杠分隔），都是同一个 scope_key。
        api.call(
            "demo",
            "host/fs.write",
            &json!({ "path": "note.txt", "content": "a" }),
        )
        .expect("首次写入");
        api.call(
            "demo",
            "host/fs.write",
            &json!({ "path": "./note.txt", "content": "b" }),
        )
        .expect("同路径换写法也该免问");
        assert_eq!(approver.asked().len(), 1, "同一文件只问一次");

        api.call(
            "demo",
            "host/fs.write",
            &json!({ "path": "sub/other.txt", "content": "c" }),
        )
        .expect("换文件");
        assert_eq!(approver.asked().len(), 2, "换了文件必须重新问");

        // 大小写不同也算同一个文件：Windows 文件系统不区分大小写，
        // 不折小写就等于把「按文件审批」变成「每条路径问一次」。
        api.call(
            "demo",
            "host/fs.write",
            &json!({ "path": "NOTE.TXT", "content": "d" }),
        )
        .expect("同文件换大小写");
        assert_eq!(
            approver.asked().len(),
            2,
            "Windows 下大小写不构成新文件：{:?}",
            approver
                .asked()
                .iter()
                .map(|row| row.scope_key.clone())
                .collect::<Vec<_>>()
        );

        // 账本里存的键是规范化绝对路径（正斜杠 + 小写），撤销时才能对上。
        let key = write_scope_key(&root.canonicalize().expect("规范化根").join("note.txt"));
        assert!(api.is_write_always(&key), "always 应落在规范化绝对路径上");
        assert!(!key.contains('\\'), "scope_key 必须是正斜杠：{key}");
        assert_eq!(key, key.to_ascii_lowercase(), "Windows 下应折小写：{key}");

        // 覆盖式刷新能把它真的忘掉（撤销规则后必须立刻生效）。
        api.seed_write_always(Vec::new());
        assert!(!api.is_write_always(&key), "撤销后应忘掉");
        api.call(
            "demo",
            "host/fs.write",
            &json!({ "path": "note.txt", "content": "e" }),
        )
        .expect("重新批准");
        assert_eq!(approver.asked().len(), 3, "撤销后必须重新问");

        std::fs::remove_dir_all(&root).ok();
    }

    /// 写盘是原子的：覆盖后内容整体替换，且不留下临时文件。
    #[test]
    fn fs_write_replaces_atomically_without_temp_leftovers() {
        let root = sandbox("write-atomic");
        std::fs::write(root.join("note.txt"), "old-content").expect("预置文件");
        let api = api_with(&root, &["fs.write"]);
        api.set_write_approver(StubApprover::answering(Some(WriteApproval::AllowOnce)));

        let written = api
            .call(
                "demo",
                "host/fs.write",
                &json!({ "path": "note.txt", "content": "brand new" }),
            )
            .expect("写入应成功");
        assert_eq!(written["path"], "note.txt");
        assert_eq!(written["bytes"], 9);
        assert_eq!(written["created"], false, "已存在的文件不算新建");
        assert_eq!(
            std::fs::read_to_string(root.join("note.txt")).expect("读回"),
            "brand new"
        );
        assert!(temp_files(&root).is_empty(), "rename 之后不该留住临时文件");

        // 多层新目录 + base64 内容：目录要能自动补建，字节要按解码后算。
        let nested = api
            .call(
                "demo",
                "host/fs.write",
                &json!({
                    "path": "a/b/c.bin",
                    "content": BASE64.encode([0u8, 1, 2, 3]),
                    "encoding": "base64",
                }),
            )
            .expect("建目录 + 写二进制");
        assert_eq!(nested["created"], true, "新文件要报 created");
        assert_eq!(nested["bytes"], 4, "bytes 按解码后的字节数算");
        assert_eq!(
            std::fs::read(root.join("a/b/c.bin")).expect("读回字节"),
            vec![0u8, 1, 2, 3],
            "base64 必须如实解码落盘"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// 参数不对就别写：缺 content、encoding 不认识、目标是目录。
    #[test]
    fn fs_write_validates_params_before_touching_disk() {
        let root = sandbox("write-params");
        std::fs::create_dir_all(root.join("adir")).expect("建目录");
        let api = api_with(&root, &["fs.write"]);
        let approver = StubApprover::answering(Some(WriteApproval::AllowOnce));
        api.set_write_approver(approver.clone());

        let missing = api
            .call("demo", "host/fs.write", &json!({ "path": "x.txt" }))
            .expect_err("缺 content");
        assert_eq!(missing.code, HostApiError::INVALID_PARAMS);

        let bad_encoding = api
            .call(
                "demo",
                "host/fs.write",
                &json!({ "path": "x.txt", "content": "x", "encoding": "gbk" }),
            )
            .expect_err("不认识的 encoding");
        assert_eq!(bad_encoding.code, HostApiError::INVALID_PARAMS);

        let dir_target = api
            .call(
                "demo",
                "host/fs.write",
                &json!({ "path": "adir", "content": "x" }),
            )
            .expect_err("目录不能当文件写");
        assert_eq!(dir_target.code, HostApiError::IO);
        assert!(!root.join("x.txt").exists(), "参数错误不该留下文件");
        assert!(
            approver.asked().is_empty(),
            "参数 / 目标形状没过关就不该问用户"
        );

        // 反证：参数对了才会走到审批，也只问这一次。
        api.call(
            "demo",
            "host/fs.write",
            &json!({ "path": "x.txt", "content": "ok" }),
        )
        .expect("合法请求应写入");
        assert_eq!(approver.asked().len(), 1, "只有通过校验的那次才该去审批");

        std::fs::remove_dir_all(&root).ok();
    }

    /// 写侧的路径闸门：越界 / 敏感一律在审批之前拒掉，别让弹窗变成探测器。
    #[test]
    fn fs_write_path_gate_rejects_before_approval() {
        let root = sandbox("write-path");
        let api = api_with(&root, &["fs.write"]);
        let approver = StubApprover::answering(Some(WriteApproval::AllowAlways));
        api.set_write_approver(approver.clone());

        let cases = [
            ("../escape.txt", HostApiError::PATH_OUT_OF_WORKSPACE),
            ("C:/Windows/evil.txt", HostApiError::PATH_OUT_OF_WORKSPACE),
            (".env", HostApiError::PATH_DENIED),
            (".git/config", HostApiError::PATH_DENIED),
        ];
        for (path, code) in cases {
            let err = api
                .call(
                    "demo",
                    "host/fs.write",
                    &json!({ "path": path, "content": "x" }),
                )
                .expect_err("必须拒");
            assert_eq!(err.code, code, "{path}");
        }
        assert!(approver.asked().is_empty(), "路径没过闸门就不该问用户");
        assert!(
            !root.join("escape.txt").exists(),
            "越界路径不能在工作区里留下东西"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// 写侧审批被驳回要落 `reject` 行；成功则先 `approve` 再 `allow`，且都带 scopeKey。
    #[tokio::test]
    async fn fs_write_audits_approve_reject_and_allow() {
        let root = sandbox("write-audit");
        let (api, store) = api_with_store(&root, &["fs.write"], "write").await;

        let rejecter = StubApprover::answering(Some(WriteApproval::Reject));
        api.set_write_approver(rejecter);
        api.call(
            "demo",
            "host/fs.write",
            &json!({ "path": "denied.txt", "content": "x" }),
        )
        .expect_err("被驳回");

        let rows =
            FsHostApi::wait_audit(&store, |row| row.detail["decision"] == DECISION_REJECT).await;
        let rejected = rows
            .iter()
            .find(|row| row.detail["decision"] == DECISION_REJECT)
            .expect("应有一行 reject");
        assert_eq!(rejected.detail["capability"], "fs.write");
        assert_eq!(rejected.detail["target"], "denied.txt");
        assert_eq!(rejected.detail["pluginId"], "demo");
        assert!(
            rejected.detail["scopeKey"]
                .as_str()
                .unwrap_or("")
                .ends_with("/denied.txt"),
            "审计要带规范化绝对路径：{}",
            rejected.detail
        );

        // 换成放行，再写一次：应出现 approve 与 allow 两行，且共用同一个 callId。
        api.set_write_approver(StubApprover::answering(Some(WriteApproval::AllowOnce)));
        api.call(
            "demo",
            "host/fs.write",
            &json!({ "path": "ok.txt", "content": "y" }),
        )
        .expect("放行后写入成功");

        let rows =
            FsHostApi::wait_audit(&store, |row| row.detail["decision"] == DECISION_ALLOW).await;
        let allowed = rows
            .iter()
            .find(|row| row.detail["decision"] == DECISION_ALLOW)
            .expect("应有一行 allow");
        assert_eq!(allowed.detail["method"], "host/fs.write");
        assert_eq!(allowed.detail["target"], "ok.txt");
        let call_id = allowed.detail["callId"].as_str().unwrap_or("").to_string();
        let approve = rows
            .iter()
            .find(|row| row.detail["decision"] == DECISION_APPROVE)
            .expect("应有一行 approve");
        assert_eq!(approve.detail["callId"], call_id, "同一调用要能对账");
        assert_eq!(approve.detail["target"], "ok.txt");

        std::fs::remove_dir_all(&root).ok();
    }

    /// 写侧也吃同一份配额：第 201 次写先被审批挡，再被配额挡（审批在前）。
    #[tokio::test]
    async fn fs_write_shares_the_quota_and_audits_overflow() {
        let root = sandbox("write-quota");
        let (api, store) = api_with_store(&root, &["fs.write"], "write-quota").await;
        api.set_write_approver(StubApprover::answering(Some(WriteApproval::AllowAlways)));
        api.begin_call("demo");

        for index in 0..QUOTA_PER_CALL {
            api.call(
                "demo",
                "host/fs.write",
                &json!({ "path": "loop.txt", "content": format!("{index}") }),
            )
            .unwrap_or_else(|err| panic!("第 {index} 次不该失败：{err:?}"));
        }
        let denied = api
            .call(
                "demo",
                "host/fs.write",
                &json!({ "path": "loop.txt", "content": "overflow" }),
            )
            .expect_err("第 201 次必须超配额");
        assert_eq!(
            denied.code,
            HostApiError::QUOTA_EXCEEDED,
            "审批过了才轮到配额"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("loop.txt")).expect("读回"),
            format!("{}", QUOTA_PER_CALL - 1),
            "超配额的写不能落盘"
        );

        let rows = FsHostApi::wait_audit(&store, |row| {
            row.detail["decision"] == DECISION_QUOTA_EXCEEDED
        })
        .await;
        let overflow = rows
            .iter()
            .find(|row| row.detail["decision"] == DECISION_QUOTA_EXCEEDED)
            .expect("应有 quota_exceeded 行");
        assert_eq!(overflow.detail["capability"], "fs.write");
        assert!(
            overflow.detail["scopeKey"]
                .as_str()
                .unwrap_or("")
                .ends_with("/loop.txt"),
            "超配额的行也要带 scopeKey：{}",
            overflow.detail
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// 没授权 `fs.write` 一律 6103，且不会被问审批。
    #[test]
    fn fs_write_needs_the_capability() {
        let root = sandbox("write-unauth");
        let api = api_with(&root, &["fs.read"]);
        let approver = StubApprover::answering(Some(WriteApproval::AllowAlways));
        api.set_write_approver(approver.clone());

        let err = api
            .call(
                "demo",
                "host/fs.write",
                &json!({ "path": "x.txt", "content": "x" }),
            )
            .expect_err("没有 fs.write 能力");
        assert_eq!(err.code, HostApiError::UNAUTHORIZED);
        assert!(approver.asked().is_empty(), "没能力就不该惊动审批");
        assert!(!root.join("x.txt").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    /// 未实现的方法回 -32601，且不去碰闸门（不消耗配额、不落审计）。
    #[tokio::test]
    async fn unimplemented_method_answers_method_not_found() {
        let root = sandbox("unimpl");
        let (api, store) = api_with_store(&root, &["fs.read"], "unimpl").await;

        for method in ["host/net.fetch", "host/llm.chat", "host/progress"] {
            let err = api
                .call("demo", method, &json!({ "path": "a.txt" }))
                .expect_err("v1 未实现");
            assert_eq!(err.code, HostApiError::METHOD_NOT_FOUND, "{method}");
            assert!(err.message.contains("v1 未实现"), "{}", err.message);
        }
        assert_eq!(api.quota_used("demo"), 0);
        // 方法不存在 = 连「插件用了什么能力」都谈不上，审计表必须干净。
        let rows = store
            .list_audit(ai_ide_store::AuditQuery::new().kind(audit_kind::PLUGIN_HOST_CALL))
            .await
            .unwrap_or_default();
        assert!(rows.is_empty(), "不该落审计");

        std::fs::remove_dir_all(&root).ok();
    }

    // ---------------------------------------------------------------
    // P5 切片四：`host/progress` 进度上报通道
    // ---------------------------------------------------------------

    /// 外壳窗口替身：把投到窗口的事件按顺序记下来。
    #[derive(Default)]
    struct RecordingShell {
        events: Mutex<Vec<(String, Value)>>,
    }

    impl RecordingShell {
        fn taken(&self) -> Vec<(String, Value)> {
            self.events
                .lock()
                .map(|list| list.clone())
                .unwrap_or_default()
        }
    }

    impl ShellEvents for RecordingShell {
        fn emit(&self, event: &str, payload: Value) {
            if let Ok(mut list) = self.events.lock() {
                list.push((event.to_string(), payload));
            }
        }
    }

    /// 有 `progress` 能力 → 转成 `plugin.progress`，并且**只**带白名单键。
    ///
    /// 插件塞进来的 `content` / `path` 不能出现在载荷里：进度是只读遥测，不是
    /// 往外搬文件内容的旁路，这条靠宿主重拼载荷来保证，不靠插件自觉。
    #[test]
    fn progress_is_forwarded_as_plugin_progress() {
        let root = sandbox("progress-fwd");
        let api = api_with(&root, &["progress"]);
        let shell = Arc::new(RecordingShell::default());
        api.set_shell_events(shell.clone());

        api.notify(
            "demo",
            "host/progress",
            &json!({
                "token": "scan",
                "message": "正在扫描 src/components",
                "percent": 42,
                "content": "秘密文件内容",
                "path": "src/secret.ts",
                "pluginId": "forged",
            }),
        )
        .expect("有 progress 能力就该放行");

        let seen = shell.taken();
        assert_eq!(seen.len(), 1, "外壳窗口应收到一条：{seen:?}");
        assert_eq!(seen[0].0, "plugin.progress");
        let payload = &seen[0].1;
        assert_eq!(payload["pluginId"], json!("demo"), "身份由宿主认定");
        assert_eq!(payload["token"], json!("scan"));
        assert_eq!(payload["message"], json!("正在扫描 src/components"));
        assert_eq!(payload["percent"], json!(42.0));
        assert_eq!(payload["truncated"], json!(false));
        assert!(
            payload.get("content").is_none() && payload.get("path").is_none(),
            "进度不得携带文件内容：{payload}"
        );
        assert_eq!(
            payload.as_object().map(|object| object.len()),
            Some(5),
            "载荷只有白名单的五个键：{payload}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// 没 `progress` 能力 → 6103，且一个字都不投（外壳不收、插件也不收）。
    #[test]
    fn progress_needs_the_capability() {
        let root = sandbox("progress-unauth");
        let api = api_with(&root, &["fs.read"]);
        let shell = Arc::new(RecordingShell::default());
        api.set_shell_events(shell.clone());

        let err = api
            .notify(
                "demo",
                "host/progress",
                &json!({ "message": "偷偷报个进度", "percent": 10 }),
            )
            .expect_err("没有 progress 能力");
        assert_eq!(err.code, HostApiError::UNAUTHORIZED);
        assert!(err.message.contains(PROGRESS_CAPABILITY), "{}", err.message);
        assert!(shell.taken().is_empty(), "被拒的上报不得外泄");

        std::fs::remove_dir_all(&root).ok();
    }

    /// 超上限截断并打标，`percent` 归一到 0–100。
    #[test]
    fn progress_truncates_over_long_message_and_clamps_percent() {
        let root = sandbox("progress-trunc");
        let api = api_with(&root, &["progress"]);
        let shell = Arc::new(RecordingShell::default());
        api.set_shell_events(shell.clone());

        api.notify(
            "demo",
            "host/progress",
            &json!({
                "token": "t".repeat(MAX_PROGRESS_TOKEN_CHARS + 10),
                "message": "长".repeat(MAX_PROGRESS_MESSAGE_CHARS + 50),
                "percent": 250,
            }),
        )
        .expect("截断不是错误");

        let seen = shell.taken();
        assert_eq!(seen.len(), 1);
        let payload = &seen[0].1;
        assert_eq!(payload["truncated"], json!(true), "截断要打标");
        assert_eq!(
            payload["message"].as_str().unwrap_or("").chars().count(),
            MAX_PROGRESS_MESSAGE_CHARS,
            "消息按字符截到上限，不留半个汉字"
        );
        assert_eq!(
            payload["token"].as_str().unwrap_or("").chars().count(),
            MAX_PROGRESS_TOKEN_CHARS
        );
        assert_eq!(payload["percent"], json!(100.0), "超过 100 归到 100");

        std::fs::remove_dir_all(&root).ok();
    }

    /// 没有 `message` 的进度是空帧，直接按参数错误拒，不往窗口推空条。
    #[test]
    fn progress_without_message_is_rejected() {
        let root = sandbox("progress-empty");
        let api = api_with(&root, &["progress"]);
        let shell = Arc::new(RecordingShell::default());
        api.set_shell_events(shell.clone());

        let err = api
            .notify("demo", "host/progress", &json!({ "percent": 5 }))
            .expect_err("缺 message");
        assert_eq!(err.code, HostApiError::INVALID_PARAMS);
        assert!(shell.taken().is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    /// `host/log` 仍然不要能力：同一份授权表下，日志能过、进度不能过。
    #[test]
    fn log_stays_free_while_progress_is_gated() {
        let root = sandbox("progress-vs-log");
        let api = api_with(&root, &[]);
        let shell = Arc::new(RecordingShell::default());
        api.set_shell_events(shell.clone());

        api.notify("demo", "host/log", &json!({ "message": "hi" }))
            .expect("日志不需要能力");
        let denied = api
            .notify("demo", "host/progress", &json!({ "message": "hi" }))
            .expect_err("进度需要能力");
        assert_eq!(denied.code, HostApiError::UNAUTHORIZED);
        assert!(
            shell.taken().is_empty(),
            "日志走插件订阅通道，不进外壳的进度口"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
