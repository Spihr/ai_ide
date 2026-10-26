//! `lsp.*` 的协议处理器（LSP 切片二：内核侧连接管理）。
//!
//! 本模块只做三件事：
//!
//! 1. 解析参数（`parse_params`），把「要什么语言、哪个文件、哪个位置」翻译成
//!    `crates/lsp` 认得的入参；
//! 2. 用 [`LspManager`] 管住**连接**：`workspace_id → server_id → 连接 + 元信息`。
//!    连接本体（分帧、握手、文档同步）在 [`ai_ide_lsp::LspClient`]，这里只负责
//!    「按需起、按 id 复用、按 id 关」；
//! 3. 把 [`LspError`] 折成三段专用错误码（`10001` / `10002` / `10003`），
//!    让外壳能分别给出「去装服务器」「换个语言」「这个句柄没了」三种不同提示。
//!
//! ## 为什么错误码只有三个
//!
//! `10xxx` 段留给语言服务，且只区隔「用户该做什么」：没装（`10001`，出路是装）、
//! 语言不认识（`10002`，出路是显式指定 `language_id`）、句柄不认识（`10003`，
//! 出路是重新 `lsp.start`）。其余失败（进程崩了、协议回包非法、超时）都归
//! `-32603`——它们对用户是同一种处置（重试 / 看日志），多分几个码只是让前端
//! 多写几个分支。
//!
//! ## 连接作用域：以 `root` 为准，退到活动工作区
//!
//! `lsp.*` 的入参没有 `workspace_id`（见 `crates/protocol` 的 DTO），但管理器必须
//! 分作用域——同一份 `typescript-language-server` 不能同时给两个不同工作区当根，
//! 否则索引与跳转都会跨项目串味。这里的做法是：**有 `root` 就用 `root` 的绝对路径
//! 当作用域键**（与活动窗口无关，同一文件重复调用拿到的永远是同一条连接）；
//! 没有 `root` 时才退到 [`AppState::active_id`]。
//!
//! ## 与 dispatch 的关系
//!
//! 五个 handler 已在 `dispatch.rs` 接线，`lsp.*` 五项也已从
//! `DECLARED_NOT_IMPLEMENTED` 搬进 `IMPLEMENTED`（74 → 79），`core.hello`
//! 因此如实把它们报成能力。
//!
//! ## 诊断是推送，不是应答
//!
//! `textDocument/publishDiagnostics` 是服务器**主动推**的通知，没有对应的请求，
//! 因此它不可能是某个 handler 的返回值：`lsp.start` 一成功就为这条连接挂一个
//! 转发任务，把来自 [`ai_ide_lsp::LspClient::subscribe`] 的诊断翻成
//! `event::LSP_DIAGNOSTICS`（`lsp.diagnostics`）推给外壳。
//!
//! 任务的生命周期绑在**广播通道**上而不是绑在句柄上：`lsp.stop` 关掉进程后读线程
//! 结束、发送端随之析构，转发任务收到 `Closed` 自行退出。这样「谁负责收尾」只有
//! 一处答案，不需要再维护一张「任务表」。

use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use ai_ide_lsp::{
    uri_to_path, CommandSpec, LanguageRegistry, LspClient, LspError, ServerOverrides,
    SourceLocation, SourcePosition, SourceRange,
};
use ai_ide_protocol::dto::{
    LspCompletionItem, LspCompletionList, LspDiagnostic, LspDiagnosticSeverity,
    LspDiagnosticsEvent, LspHover, LspLocation, LspPosition, LspRange, LspRequestMethod,
    LspRequestParams, LspRequestResult, LspServerInfo, LspServerStatus, LspServersParams,
    LspServersResult, LspStartParams, LspStartResult, LspStopParams, LspStopResult, LspSyncParams,
    LspSyncResult,
};
use ai_ide_protocol::{methods, ErrorCode, ErrorObject, Request};
use serde_json::Value;
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::dispatch::parse_params;
use crate::peer::Peer;
use crate::state::AppState;

/// 既没有 `root` 也没有活动工作区时的作用域键。
///
/// 用一个固定串而不是空串：空串在日志里像个缺字段的 bug，而这其实是合法状态
/// （用户直接对一个游离文件开语言服务）。
const DEFAULT_WORKSPACE: &str = "default";

// ---------------------------------------------------------------------------
// 错误适配
// ---------------------------------------------------------------------------

/// [`LspError`] → 协议错误对象。
///
/// 只把两种「用户能自己解决」的错误翻译成专用码，其余一律 `-32603`：
/// 把 `Spawn` / `Protocol` / `Timeout` 也塞进 `10001` 会让面板提示「去装服务器」，
/// 用户装完发现还是同一个错——那比不给码更糟。
pub fn protocol_error(err: &LspError) -> ErrorObject {
    match err {
        LspError::NotInstalled { .. } => {
            ErrorObject::new(ErrorCode::LSP_SERVER_NOT_INSTALLED, err.to_string())
        }
        LspError::UnsupportedLanguage { .. } => {
            ErrorObject::new(ErrorCode::LSP_LANGUAGE_UNSUPPORTED, err.to_string())
        }
        other => ErrorObject::internal(other.to_string()),
    }
}

fn unknown_server(server_id: &str) -> ErrorObject {
    ErrorObject::new(
        ErrorCode::LSP_SERVER_UNKNOWN,
        format!("未知的 server_id：`{server_id}`（可能从未启动，或已被 lsp.stop 关掉）"),
    )
}

/// 在阻塞线程池里读文件。
///
/// 不走 `tokio::fs`：workspace 的 tokio 没有开 `fs` feature，而且内核里「磁盘 IO
/// 一律 `spawn_blocking`」本来就是既有约定（见 `ws.rs` 的三条约定）。
async fn read_text(path: &Path) -> Result<String, ErrorObject> {
    let target = path.to_path_buf();
    match tokio::task::spawn_blocking(move || std::fs::read_to_string(&target)).await {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(err)) => Err(ErrorObject::internal(format!(
            "读取 `{}` 失败：{err}",
            path.display()
        ))),
        Err(err) => Err(ErrorObject::internal(format!("后台任务失败：{err}"))),
    }
}

// ---------------------------------------------------------------------------
// 连接句柄与元信息
// ---------------------------------------------------------------------------

/// 一条连接的可变元信息。
#[derive(Debug, Clone)]
struct ServerMeta {
    status: LspServerStatus,
    restarts: u32,
    last_error: Option<String>,
}

/// 一条语言服务器连接 + 它的元信息。
///
/// `client` 用 [`Arc`] 包：`lsp.request` 拿到句柄后要跨 `await` 用，而管理器里的
/// 表锁**不能**跨 `await` 持有，否则一个慢请求会把整个 `lsp.*` 面锁死。
pub struct LspServerHandle {
    server_id: String,
    workspace_id: String,
    language_id: String,
    command: String,
    root: Option<PathBuf>,
    client: Arc<LspClient>,
    meta: Mutex<ServerMeta>,
}

impl LspServerHandle {
    fn new(
        server_id: String,
        workspace_id: String,
        language_id: String,
        command: String,
        root: Option<PathBuf>,
        client: Arc<LspClient>,
        restarts: u32,
    ) -> Self {
        Self {
            server_id,
            workspace_id,
            language_id,
            command,
            root,
            client,
            meta: Mutex::new(ServerMeta {
                // `LspClient::start` 里已经跑完 `initialize` + `initialized`，
                // 走到这里就一定握过手了，不存在 `Starting` 的中间态。
                status: LspServerStatus::Ready,
                restarts,
                last_error: None,
            }),
        }
    }

    /// 连接句柄。
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// 所属作用域（工作区根目录路径，或活动工作区 id）。
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// 语言分组（注册表口径，例如 `typescript`）。
    pub fn language_id(&self) -> &str {
        &self.language_id
    }

    /// 实际执行的命令。
    pub fn command(&self) -> &str {
        &self.command
    }

    /// 连接本体。
    pub fn client(&self) -> &Arc<LspClient> {
        &self.client
    }

    /// 当前状态（连接已断但表里还是 `Ready` 时如实报 `Crashed`）。
    pub fn status(&self) -> LspServerStatus {
        let meta = self.lock_meta();
        self.effective_status(&meta)
    }

    /// 关掉之后的落账：状态置 `Stopped`。
    fn mark_stopped(&self) {
        self.lock_meta().status = LspServerStatus::Stopped;
    }

    /// 协议面的摘要。
    pub fn info(&self) -> LspServerInfo {
        let meta = self.lock_meta();
        LspServerInfo {
            server_id: self.server_id.clone(),
            language_id: self.language_id.clone(),
            command: self.command.clone(),
            root: self.root.as_ref().map(|root| root.display().to_string()),
            status: self.effective_status(&meta),
            // `pid` 留空：`StdioTransport` 目前不对外暴露子进程号，编一个假值比
            // 留空更糟（外壳会拿它去 taskkill）。字段缺省是合法的，见 DTO。
            pid: None,
            restarts: meta.restarts,
            last_error: meta.last_error.clone(),
        }
    }

    fn effective_status(&self, meta: &ServerMeta) -> LspServerStatus {
        if meta.status == LspServerStatus::Ready && self.client.is_closed() {
            LspServerStatus::Crashed
        } else {
            meta.status
        }
    }

    fn lock_meta(&self) -> std::sync::MutexGuard<'_, ServerMeta> {
        self.meta
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl std::fmt::Debug for LspServerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspServerHandle")
            .field("server_id", &self.server_id)
            .field("workspace_id", &self.workspace_id)
            .field("language_id", &self.language_id)
            .field("command", &self.command)
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// 管理器
// ---------------------------------------------------------------------------

/// 一次启动解析的结果（还没起进程）。
#[derive(Debug, Clone)]
pub struct StartPlan {
    /// 生效的语言分组。
    pub language_id: String,
    /// 实际会执行的命令（可能来自备选）。
    pub command: CommandSpec,
    /// 工作区根目录（同时作为 `initialize` 的 `rootUri`）。
    pub root: Option<PathBuf>,
    /// 可以直接交给 [`LspClient::start`] 的配置。
    pub config: ai_ide_lsp::ServerConfig,
}

/// 语言服务器连接管理器：`workspace_id → server_id → 连接 + 元信息`。
///
/// 两层 `BTreeMap` 而不是一张扁平表：`BTreeMap` 的迭代顺序天然有序，`lsp.servers`
/// 直接按 `server_id` 升序返回，前端 diff 时不会因为顺序抖动而全量重绘；
/// 同时「某个工作区全关掉」是一次 `remove`，不用扫全表。
pub struct LspManager {
    registry: LanguageRegistry,
    workspaces: Mutex<BTreeMap<String, BTreeMap<String, Arc<LspServerHandle>>>>,
}

impl Default for LspManager {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for LspManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspManager")
            .field("workspaces", &self.lock_workspaces().len())
            .finish_non_exhaustive()
    }
}

impl LspManager {
    /// 用内置注册表造一个空管理器。
    pub fn new() -> Self {
        Self::with_registry(LanguageRegistry::builtin())
    }

    /// 用指定注册表造一个空管理器（测试里塞自定义条目用）。
    pub fn with_registry(registry: LanguageRegistry) -> Self {
        Self {
            registry,
            workspaces: Mutex::new(BTreeMap::new()),
        }
    }

    /// 注册表（配置解析与命令探测都在它里面，这里只读）。
    pub fn registry(&self) -> &LanguageRegistry {
        &self.registry
    }

    /// 连接句柄的稳定 id。
    ///
    /// 同一 `(workspace_id, language_id, root)` 必须拿到同一个 id，否则「重复
    /// `lsp.start` 复用连接」会失效——每次调用都起一个新进程，编辑器里点十次
    /// 定义就起十个 `rust-analyzer`。`root` 参与哈希是因为同一个语言在不同项目
    /// 根下是两条不同的连接。
    pub fn server_id_for(workspace_id: &str, language_id: &str, root: Option<&Path>) -> String {
        match root {
            Some(root) => format!("{language_id}@{workspace_id}#{:08x}", stable_hash(root)),
            None => format!("{language_id}@{workspace_id}"),
        }
    }

    /// 把一个连接登记进来（同 id 覆盖）。
    pub fn insert(&self, handle: Arc<LspServerHandle>) {
        let mut workspaces = self.lock_workspaces();
        workspaces
            .entry(handle.workspace_id.clone())
            .or_default()
            .insert(handle.server_id.clone(), handle);
    }

    /// 按 id 找连接（跨全部作用域）。
    pub fn lookup(&self, server_id: &str) -> Option<Arc<LspServerHandle>> {
        let workspaces = self.lock_workspaces();
        workspaces
            .values()
            .find_map(|servers| servers.get(server_id).cloned())
    }

    /// 按 id 找连接，找不到就是 `10003`。
    pub fn require(&self, server_id: &str) -> Result<Arc<LspServerHandle>, ErrorObject> {
        self.lookup(server_id)
            .ok_or_else(|| unknown_server(server_id))
    }

    /// 摘掉一条连接（返回被摘掉的那条）。
    pub fn remove(&self, server_id: &str) -> Option<Arc<LspServerHandle>> {
        let mut workspaces = self.lock_workspaces();
        let mut removed = None;
        for servers in workspaces.values_mut() {
            if let Some(handle) = servers.remove(server_id) {
                removed = Some(handle);
                break;
            }
        }
        // 空作用域不留在表里：否则「有没有连接」的判断会把空桶当成活连接。
        workspaces.retain(|_, servers| !servers.is_empty());
        removed
    }

    /// 全部连接摘要（按 `server_id` 升序）。
    pub fn infos(&self) -> Vec<LspServerInfo> {
        let workspaces = self.lock_workspaces();
        let mut infos: Vec<LspServerInfo> = workspaces
            .values()
            .flat_map(|servers| servers.values())
            .map(|handle| handle.info())
            .collect();
        infos.sort_by(|left, right| left.server_id.cmp(&right.server_id));
        infos
    }

    /// 某个作用域下的连接摘要（按 `server_id` 升序）。
    pub fn infos_in(&self, workspace_id: &str) -> Vec<LspServerInfo> {
        let workspaces = self.lock_workspaces();
        workspaces
            .get(workspace_id)
            .map(|servers| servers.values().map(|handle| handle.info()).collect())
            .unwrap_or_default()
    }

    /// 连接总数。
    pub fn len(&self) -> usize {
        self.lock_workspaces()
            .values()
            .map(|servers| servers.len())
            .sum()
    }

    /// 是否一条连接都没有。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 解析 `lsp.start` 入参：定语言、定根、定命令。
    ///
    /// 配置解析与命令探测**全部复用** [`LanguageRegistry`]：这里不自己拼
    /// `typescript-language-server --stdio`，也不自己扫 `PATH`，否则「注册表说装了、
    /// 内核说没装」这类不一致会立刻出现。
    pub fn resolve(&self, params: &LspStartParams) -> Result<StartPlan, ErrorObject> {
        let inferred = params
            .path
            .as_deref()
            .and_then(|path| self.registry.language_id_for_path(Path::new(path)))
            .map(str::to_string);
        let language_id = match (&params.language_id, inferred) {
            // 显式指定优先：用户说「这个 `.ts` 用 `deno`」，扩展名不该翻盘。
            (Some(explicit), _) => explicit.clone(),
            (None, Some(inferred)) => inferred,
            (None, None) => {
                let target = params.path.as_deref().unwrap_or("<未给出 path>");
                return Err(ErrorObject::new(
                    ErrorCode::LSP_LANGUAGE_UNSUPPORTED,
                    format!(
                        "无法从 `{target}` 推断语言：注册表里没有这个扩展名，请显式指定 language_id"
                    ),
                ));
            }
        };

        let root = params.root.as_deref().map(PathBuf::from);
        let mut overrides = ServerOverrides::new().language_id(language_id.clone());
        if let Some(command) = &params.command {
            overrides = overrides.command(command.clone());
        }
        if !params.args.is_empty() {
            overrides = overrides.args(params.args.clone());
        }
        if let Some(root) = &root {
            overrides = overrides.root(root.clone());
        }

        let resolved = match params.path.as_deref() {
            Some(path) => self.registry.resolve_for_path(Path::new(path), &overrides),
            None => self.registry.resolve_for_language(&language_id, &overrides),
        }
        .map_err(|err| protocol_error(&err))?;

        Ok(StartPlan {
            language_id: resolved.language_id,
            root: resolved.config.cwd.clone(),
            command: resolved.command,
            config: resolved.config,
        })
    }

    fn lock_workspaces(
        &self,
    ) -> std::sync::MutexGuard<'_, BTreeMap<String, BTreeMap<String, Arc<LspServerHandle>>>> {
        // 锁被毒化（某个 handler panic 在临界区里）时继续用里面的表：
        // 一条连接崩了不该让整个 `lsp.*` 面永久不可用。
        self.workspaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 稳定的路径哈希：同一条路径在同一进程里必须每次都算出同一个值。
///
/// 不用 `RandomState`（`HashMap` 的默认哈希器带进程级随机种子），否则重启后
/// `server_id` 会变，前端缓存的句柄全部失效。
fn stable_hash(path: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.to_string_lossy().to_lowercase().hash(&mut hasher);
    hasher.finish()
}

/// 进程级单例。
///
/// 本切片不动 `state.rs`（那会把 diff 从「一个文件」扩到「状态结构 + 启动流程」），
/// 所以管理器挂在这里。接线 `dispatch.rs` 时把它搬进 `AppState` 与 `mcp` 同构即可，
/// 五个 handler 的签名不需要跟着改。
static MANAGER: OnceLock<LspManager> = OnceLock::new();

/// 取进程级管理器。
pub fn manager() -> &'static LspManager {
    MANAGER.get_or_init(LspManager::new)
}

/// 作用域键：有 `root` 用 `root` 的绝对路径，否则退到活动工作区。
fn workspace_id_of(state: &AppState, root: Option<&Path>) -> String {
    match root {
        Some(root) => root.display().to_string(),
        None => state
            .active_id()
            .unwrap_or_else(|| DEFAULT_WORKSPACE.to_string()),
    }
}

fn lsp_position(position: LspPosition) -> ai_ide_lsp::Position {
    // 协议面 1-based，LSP 线格式 0-based。下溢（前端传 0）钳到 0 而不是报错：
    // 报错会让「光标在第一行第一列」这种合法场景因为一次 `-1` 归零而失败。
    ai_ide_lsp::Position::new(
        position.line.saturating_sub(1),
        position.character.saturating_sub(1),
    )
}

fn lsp_range(range: SourceRange) -> LspRange {
    LspRange {
        start: protocol_position(range.start),
        end: protocol_position(range.end),
    }
}

fn protocol_position(position: SourcePosition) -> LspPosition {
    LspPosition {
        line: position.line,
        character: position.character,
    }
}

fn lsp_location(location: &SourceLocation) -> LspLocation {
    LspLocation {
        uri: location.uri.clone(),
        path: location
            .path
            .as_ref()
            .map(|path| path.display().to_string()),
        range: lsp_range(location.range),
    }
}

// ---------------------------------------------------------------------------
// 诊断转发
// ---------------------------------------------------------------------------

/// 上游的诊断方法名。
///
/// 只在这里出现一次：`crates/lsp` 不导出这个常量（它把通知当纯数据广播），
/// 而协议面的 `event::LSP_DIAGNOSTICS` 是**下行**改名的那个，两者不是同一个名字。
const PUBLISH_DIAGNOSTICS: &str = "textDocument/publishDiagnostics";

/// LSP 的 0-based 坐标 → 协议面的 1-based。
///
/// 与 [`lsp_position`] 的 `saturating_sub` 相反：这里是**上游**给的坐标，
/// 加一不可能溢出（`u32::MAX` 行的文档不存在），但仍用 `saturating_add` 兜住
/// 服务器发疯的情况，不 panic。
fn wire_position(value: &Value) -> Option<LspPosition> {
    let line = u32::try_from(value.get("line")?.as_i64()?).ok()?;
    let character = u32::try_from(value.get("character")?.as_i64()?).ok()?;
    Some(LspPosition {
        line: line.saturating_add(1),
        character: character.saturating_add(1),
    })
}

/// LSP `DiagnosticSeverity`（1..4 数字）→ 协议枚举。
///
/// 缺省 / `0` / 越界一律 `Unknown`，不猜成 `error`：把提示画成红点比不画更糟。
fn wire_severity(value: Option<&Value>) -> LspDiagnosticSeverity {
    match value.and_then(Value::as_u64) {
        Some(1) => LspDiagnosticSeverity::Error,
        Some(2) => LspDiagnosticSeverity::Warning,
        Some(3) => LspDiagnosticSeverity::Information,
        Some(4) => LspDiagnosticSeverity::Hint,
        _ => LspDiagnosticSeverity::Unknown,
    }
}

/// 一条 LSP 诊断 → 协议 DTO。
///
/// 缺 `range` 的坏条目返回 `None`：坐标是渲染的唯一依据，编一个 `0:0` 会让红点
/// 落在文件开头，用户点过去发现什么都没有。
fn wire_diagnostic(value: &Value) -> Option<LspDiagnostic> {
    let range = value.get("range")?;
    Some(LspDiagnostic {
        range: LspRange {
            start: wire_position(range.get("start")?)?,
            end: wire_position(range.get("end")?)?,
        },
        severity: wire_severity(value.get("severity")),
        // `code` 在 LSP 里既可能是字符串（`"TS2304"`）也可能是数字（`2322`），
        // 统一成字符串：面板要原样展示，数字类型对 UI 没有额外价值。
        code: value.get("code").and_then(|code| match code {
            Value::String(code) => Some(code.clone()),
            Value::Number(code) => Some(code.to_string()),
            _ => None,
        }),
        source: value
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_string),
        message: value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

/// 一条 `publishDiagnostics` 通知 → `lsp.diagnostics` 事件载荷。
///
/// 缺 `uri` 时返回 `None`（无处安放诊断）；其余字段都能降级，不因此丢整批。
fn diagnostics_event(server_id: &str, params: &Value) -> Option<LspDiagnosticsEvent> {
    let uri = params.get("uri").and_then(Value::as_str)?;
    Some(LspDiagnosticsEvent {
        server_id: server_id.to_string(),
        // uri → 本地路径：外壳按路径给编辑器挂标记。解不出来（非 `file:` 协议）
        // 就原样回 uri——总比丢事件好，至少面板能显示「有个未知来源的文件」。
        path: uri_to_path(uri)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| uri.to_string()),
        version: params
            .get("version")
            .and_then(Value::as_i64)
            .and_then(|version| i32::try_from(version).ok()),
        diagnostics: params
            .get("diagnostics")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(wire_diagnostic).collect())
            .unwrap_or_default(),
    })
}

/// 为一条连接挂上诊断转发任务（`lsp.start` 成功后调用）。
///
/// 没有 `peer`（进程内单测直接调 handler）时什么都不做：推送需要一个 stdout 出口，
/// 没有出口就没有推送对象。
fn spawn_diagnostics_forwarder(state: &AppState, server_id: &str, client: &Arc<LspClient>) {
    let Some(peer) = state.peer() else {
        return;
    };
    let mut notifications = client.subscribe();
    let server_id = server_id.to_string();
    tokio::spawn(async move {
        loop {
            match notifications.recv().await {
                Ok(notification) => {
                    if notification.method != PUBLISH_DIAGNOSTICS {
                        continue;
                    }
                    let Some(params) = notification.params.as_ref() else {
                        continue;
                    };
                    let Some(event) = diagnostics_event(&server_id, params) else {
                        continue;
                    };
                    peer.notify(methods::event::LSP_DIAGNOSTICS, &event).await;
                }
                // 慢消费者被挤掉几条：诊断是「整篇替换」语义，丢中间批次不影响
                // 最终态（下一次推送会覆盖），所以续读而不是终止。
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(
                        server_id = %server_id,
                        skipped,
                        "诊断通知积压，已跳过过期批次"
                    );
                }
                // 通道关闭＝连接没了（`lsp.stop` 或服务器崩了）：本任务随之结束。
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

// ---------------------------------------------------------------------------
// 处理器
// ---------------------------------------------------------------------------

/// `lsp.start`：起一条连接（已存在就复用）。
pub async fn start(peer: &Peer, state: &AppState, req: &Request) {
    let params: LspStartParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let manager = manager();
    let plan = match manager.resolve(&params) {
        Ok(plan) => plan,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let workspace_id = workspace_id_of(state, plan.root.as_deref());
    let server_id =
        LspManager::server_id_for(&workspace_id, &plan.language_id, plan.root.as_deref());

    // 1) 复用：同 id 且连接没断，直接把现有摘要回给外壳（`reused: true`）。
    let mut restarts = 0;
    if let Some(existing) = manager.lookup(&server_id) {
        if !existing.client().is_closed() {
            info!(server_id = %server_id, "复用已就绪的语言服务器连接");
            return peer
                .respond(
                    &req.id,
                    &LspStartResult {
                        server: existing.info(),
                        reused: true,
                    },
                )
                .await;
        }
        // 2) 断过的旧句柄：摘掉重起，并把重启次数累加（面板要回答「它为什么一直在重连」）。
        restarts = existing.info().restarts.saturating_add(1);
        manager.remove(&server_id);
        warn!(server_id = %server_id, "旧连接已关闭，重新拉起语言服务器");
    }

    match LspClient::start(&plan.config).await {
        Ok(client) => {
            let handle = Arc::new(LspServerHandle::new(
                server_id.clone(),
                workspace_id.clone(),
                plan.language_id.clone(),
                plan.command.command.clone(),
                plan.root.clone(),
                Arc::new(client),
                restarts,
            ));
            let info = handle.info();
            let client = handle.client().clone();
            manager.insert(handle);
            // 先挂转发任务再回包：`initialize` 之后服务器可能立刻推一批诊断，
            // 晚挂就会漏掉首屏那一批（面板要等下一次编辑才有红点）。
            spawn_diagnostics_forwarder(state, &server_id, &client);
            info!(
                server_id = %server_id,
                language = %plan.language_id,
                command = %plan.command.command,
                "语言服务器已就绪"
            );
            peer.respond(
                &req.id,
                &LspStartResult {
                    server: info,
                    reused: false,
                },
            )
            .await;
        }
        Err(err) => peer.respond_error(&req.id, &protocol_error(&err)).await,
    }
}

/// `lsp.stop`：关掉一条连接（幂等：已关掉的再关不报错）。
pub async fn stop(peer: &Peer, _state: &AppState, req: &Request) {
    let params: LspStopParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    // 先摘表再关进程：`shutdown` 要等 `exit`（最长几百毫秒），期间不能让新请求
    // 拿到一个正在关闭的句柄。
    let Some(handle) = manager().remove(&params.server_id) else {
        return peer
            .respond_error(&req.id, &unknown_server(&params.server_id))
            .await;
    };

    let was_alive = !handle.client().is_closed();
    if was_alive {
        if let Err(err) = handle.client().shutdown().await {
            // 关不掉也不改变结论：表里已经没有它了，外壳再调就是 10003。
            warn!(server_id = %params.server_id, error = %err, "语言服务器关闭时出错");
        }
    }
    handle.mark_stopped();

    peer.respond(
        &req.id,
        &LspStopResult {
            server_id: params.server_id,
            stopped: was_alive,
        },
    )
    .await;
}

/// `lsp.servers`：当前活着的连接清单。
pub async fn servers(peer: &Peer, _state: &AppState, req: &Request) {
    if let Err(err) = parse_params::<LspServersParams>(req.params.as_ref()) {
        return peer.respond_error(&req.id, &err).await;
    }
    peer.respond(
        &req.id,
        &LspServersResult {
            servers: manager().infos(),
        },
    )
    .await;
}

/// `lsp.request`：定义 / 引用 / 悬停 / 补全。
pub async fn request(peer: &Peer, _state: &AppState, req: &Request) {
    let params: LspRequestParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let handle = match manager().require(&params.server_id) {
        Ok(handle) => handle,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let path = PathBuf::from(&params.path);
    // 协议面没有 `content` 字段：查询一律以磁盘上的文本为准。编辑器里未保存的
    // 缓冲要参与查询，必须先走 `lsp.sync`（那是唯一带 `content` 的入参）。
    let text = match read_text(&path).await {
        Ok(text) => text,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let position = lsp_position(params.position);
    let client = handle.client().clone();
    let echo = |locations: Vec<LspLocation>,
                hover: Option<LspHover>,
                completions: Option<LspCompletionList>| LspRequestResult {
        server_id: params.server_id.clone(),
        method: params.method,
        path: params.path.clone(),
        locations,
        hover,
        completions,
    };

    let outcome: Result<LspRequestResult, LspError> = match params.method {
        LspRequestMethod::Definition => client
            .definition(&path, &text, position)
            .await
            .map(|locations| echo(locations.iter().map(lsp_location).collect(), None, None)),
        LspRequestMethod::References => {
            // 缺省由服务器决定（TypeScript 默认含声明、Pyright 默认不含），
            // 所以只有调用方显式给了才写进 `context`。
            let include_declaration = params.include_declaration.unwrap_or(true);
            client
                .references(&path, &text, position, include_declaration)
                .await
                .map(|locations| echo(locations.iter().map(lsp_location).collect(), None, None))
        }
        LspRequestMethod::Hover => client.hover(&path, &text, position).await.map(|hover| {
            echo(
                Vec::new(),
                hover.map(|hover| LspHover {
                    contents: hover.contents,
                    range: hover.range.map(lsp_range),
                }),
                None,
            )
        }),
        LspRequestMethod::Completion => {
            client.completion(&path, &text, position).await.map(|list| {
                echo(
                    Vec::new(),
                    None,
                    Some(LspCompletionList {
                        is_incomplete: list.is_incomplete,
                        items: list
                            .items
                            .iter()
                            .map(|item| LspCompletionItem {
                                label: item.label.clone(),
                                kind: item.kind,
                                detail: item.detail.clone(),
                                documentation: item.documentation.clone(),
                                insert_text: item.insert_text.clone(),
                                sort_text: item.sort_text.clone(),
                            })
                            .collect(),
                    }),
                )
            })
        }
    };

    match outcome {
        Ok(result) => peer.respond(&req.id, &result).await,
        Err(err) => peer.respond_error(&req.id, &protocol_error(&err)).await,
    }
}

/// `lsp.sync`：把文件当前文本推给服务器（没开就 `didOpen`，变了就 `didChange`）。
pub async fn sync(peer: &Peer, _state: &AppState, req: &Request) {
    let params: LspSyncParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let handle = match manager().require(&params.server_id) {
        Ok(handle) => handle,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let path = PathBuf::from(&params.path);
    let text = match &params.content {
        Some(content) => content.clone(),
        None => match read_text(&path).await {
            Ok(text) => text,
            Err(err) => return peer.respond_error(&req.id, &err).await,
        },
    };

    match handle.client().sync_document(&path, &text).await {
        Ok(synced) => {
            peer.respond(
                &req.id,
                &LspSyncResult {
                    server_id: params.server_id,
                    path: params.path,
                    uri: synced.uri,
                    version: synced.version,
                    opened: synced.opened,
                    changed: synced.changed,
                },
            )
            .await;
        }
        Err(err) => peer.respond_error(&req.id, &protocol_error(&err)).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current_exe() -> String {
        std::env::current_exe()
            .expect("测试进程自身路径")
            .display()
            .to_string()
    }

    #[test]
    fn fresh_manager_lists_no_servers() {
        let manager = LspManager::new();
        assert!(manager.is_empty());
        assert_eq!(manager.len(), 0);
        // `lsp.servers` 的取数路径：空表必须回空数组，而不是报错。
        assert!(manager.infos().is_empty());
        assert!(manager.infos_in("any-workspace").is_empty());
    }

    #[test]
    fn unknown_server_id_is_reported_as_10003() {
        let manager = LspManager::new();
        let error = manager.require("no-such-server").expect_err("必须报错");
        assert_eq!(error.code, ErrorCode::LSP_SERVER_UNKNOWN);
        assert!(
            error.message.contains("no-such-server"),
            "{}",
            error.message
        );
        // 摘一个不存在的句柄要回 None（`lsp.stop` 靠它区分 10003 与幂等关闭）。
        assert!(manager.remove("no-such-server").is_none());
    }

    #[test]
    fn unsupported_extension_is_reported_as_10002() {
        let manager = LspManager::new();
        let params = LspStartParams {
            path: Some("E:/ai-ide-tmp/a.unknownext".to_string()),
            ..LspStartParams::default()
        };
        let error = manager.resolve(&params).expect_err("未登记扩展名必须报错");
        assert_eq!(error.code, ErrorCode::LSP_LANGUAGE_UNSUPPORTED);
    }

    #[test]
    fn explicit_language_wins_over_extension() {
        let manager = LspManager::new();
        // 命令覆盖成测试进程自身：既不依赖机器上装没装 ts 服务器，也顺带验证
        // 「覆盖命令后不再走备选」这条注册表语义。
        let params = LspStartParams {
            path: Some("E:/ai-ide-tmp/a.unknownext".to_string()),
            language_id: Some("typescript".to_string()),
            command: Some(current_exe()),
            ..LspStartParams::default()
        };
        let plan = manager.resolve(&params).expect("显式语言应可解析");
        assert_eq!(plan.language_id, "typescript");
        assert_eq!(plan.command.command, current_exe());
    }

    #[test]
    fn missing_command_is_reported_as_10001() {
        let manager = LspManager::new();
        let params = LspStartParams {
            path: Some("E:/ai-ide-tmp/a.ts".to_string()),
            command: Some("ai-ide-missing-lsp-binary-xyz".to_string()),
            ..LspStartParams::default()
        };
        let error = manager.resolve(&params).expect_err("命令不存在必须报错");
        assert_eq!(error.code, ErrorCode::LSP_SERVER_NOT_INSTALLED);
    }

    #[test]
    fn server_id_is_stable_per_root_and_language() {
        let root = Path::new("E:/ai_ide");
        let first = LspManager::server_id_for("E:/ai_ide", "typescript", Some(root));
        let second = LspManager::server_id_for("E:/ai_ide", "typescript", Some(root));
        assert_eq!(first, second);
        // 换根 / 换语言都是另一条连接。
        assert_ne!(
            first,
            LspManager::server_id_for("E:/ai_ide", "typescript", Some(Path::new("E:/other")))
        );
        assert_ne!(
            first,
            LspManager::server_id_for("E:/ai_ide", "python", Some(root))
        );
    }
}
