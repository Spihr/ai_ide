//! 读侧宿主能力（插件系统 P3.5）：`host/fs.read` / `host/fs.list` /
//! `host/fs.stat` / `host/search.content` / `host/workspace.info` 的真实实现。
//!
//! 这一层是 [`ai_ide_plugin::host_api::HostApi`] 的内核侧实现：`crates/plugin`
//! 只负责把插件进程发出的 `host/*` 帧转过来，**资格判定、路径判定、用量判定
//! 全在这里**。写侧（`fs.write`）、网络（`net.fetch`）、模型（`llm.chat`）、
//! 进度（`progress`）明确留给 P4，一律回 `-32601`。
//!
//! 三道闸门严格按 docs/14 §8.1 的顺序，且**顺序不可交换**：
//!
//! 1. **能力闸门**（6103）：账本里没有 `fs.read` / `search` 就直接拒；
//! 2. **路径闸门**（1002 越界 / 1003 敏感）：只收相对路径，`..`、盘符、UNC、
//!    NUL 一律拒；`.git` / `.ssh` / `.aws` / `.kube` / `.env` 任意层级硬拒；
//!    规范化（含符号链接解析）后必须落在规范化的工作区根之内；
//! 3. **配额闸门**（6109）：一次工具调用内 ≤ 200 次，计数由
//!    [`HostApi::begin_call`] 在工具调用边界归零。
//!
//! 能力必须排在配额前面：反过来的话，未授权插件会先撞「次数超了」，
//! 用户看到的提示指错方向，而且白送了一个「本插件有没有被授权」的探测口。
//!
//! 每次调用（含被拒与超配额）都落一行 `plugin.host_call` 审计。审计是异步写的：
//! 这条路径跑在插件 stdout 的帧泵里，同步 `await` 一次写库会堵住该插件的所有
//! 回包，所以拿不到 Tokio 运行时（纯单测）或没有库时就**跳过**，绝不 `block_on`。

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use ai_ide_plugin::host_api::{HostApi, HostApiError};
use ai_ide_store::{audit_actor, audit_kind, NewAuditEvent, Store};
use serde_json::{json, Value};

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
/// 审计 `decision` 取值：配额超限。
pub const DECISION_QUOTA_EXCEEDED: &str = "quota_exceeded";
/// 审计 `decision` 取值：闸门放行但执行失败（文件不存在、IO 错）。
pub const DECISION_ERROR: &str = "error";

/// 方法 → 所需能力。返回 `None` 的方法就是「v1 未实现」，回 `-32601`。
///
/// 注意 `workspace.info` 也归 `fs.read`：它只是把已打开工作区的根告诉插件，
/// 读能力本就隐含这件事，单开一个 `workspace` 能力会让清单白多一项。
fn capability_of(method: &str) -> Option<&'static str> {
    match method {
        "host/fs.read" | "host/fs.list" | "host/fs.stat" | "host/workspace.info" => Some("fs.read"),
        "host/search.content" => Some("search"),
        _ => None,
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
    store: RwLock<Option<Store>>,
    /// 每插件在当前工具调用内的宿主调用计数。
    quota: Mutex<HashMap<String, usize>>,
    /// 审计 `callId` 的自增序号（进程内唯一即可，不跨进程对齐）。
    calls: AtomicU64,
}

impl Default for FsHostApi {
    fn default() -> Self {
        Self::new()
    }
}

impl FsHostApi {
    pub fn new() -> Self {
        Self {
            root: Arc::new(RwLock::new(None)),
            granted: Arc::new(RwLock::new(HashSet::new())),
            by_plugin: Arc::new(RwLock::new(HashMap::new())),
            store: RwLock::new(None),
            quota: Mutex::new(HashMap::new()),
            calls: AtomicU64::new(0),
        }
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
    fn audit(
        &self,
        plugin_id: &str,
        method: &str,
        capability: &str,
        target: Option<&str>,
        decision: &str,
        call_id: &str,
    ) {
        let Some(store) = self.store_snapshot() else {
            return;
        };
        let event = NewAuditEvent::by(
            audit_actor::PLUGIN,
            audit_kind::PLUGIN_HOST_CALL,
            format!("插件 {plugin_id} 调用 {method}：{decision}"),
        )
        .with_detail(json!({
            "pluginId": plugin_id,
            "method": method,
            "capability": capability,
            "target": target,
            "decision": decision,
            "callId": call_id,
        }));

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
        let abs = if joined.exists() {
            joined
                .canonicalize()
                .unwrap_or_else(|_| normalize_lexically(&joined))
        } else {
            normalize_lexically(&joined)
        };

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
                plugin_id,
                method,
                capability,
                audit_target(params),
                DECISION_DENY,
                &call_id,
            );
            return Err(HostApiError::unauthorized(method, capability));
        }

        // 闸门 ②：路径（`workspace.info` / `search.content` 没有入参路径，
        // 搜索的遍历范围就是工作区根，越界问题由遍历本身保证）。
        let resolved = if matches!(method, "host/fs.read" | "host/fs.list" | "host/fs.stat") {
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
                    plugin_id,
                    method,
                    capability,
                    audit_target(params),
                    DECISION_DENY,
                    &call_id,
                );
                return Err(err);
            }
        };

        // 闸门 ③：配额（200 次/工具调用）。
        if !self.take_quota(plugin_id) {
            self.audit(
                plugin_id,
                method,
                capability,
                audit_target(params),
                DECISION_QUOTA_EXCEEDED,
                &call_id,
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
            plugin_id,
            method,
            capability,
            target.as_deref(),
            decision,
            &call_id,
        );
        outcome
    }

    fn begin_call(&self, plugin_id: &str) {
        if let Ok(mut map) = self.quota.lock() {
            map.insert(plugin_id.to_string(), 0);
        }
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

    /// 未实现的方法回 -32601，且不去碰闸门（不消耗配额、不落审计）。
    #[tokio::test]
    async fn unimplemented_method_answers_method_not_found() {
        let root = sandbox("unimpl");
        let (api, store) = api_with_store(&root, &["fs.read"], "unimpl").await;

        for method in [
            "host/fs.write",
            "host/net.fetch",
            "host/llm.chat",
            "host/progress",
        ] {
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
}
