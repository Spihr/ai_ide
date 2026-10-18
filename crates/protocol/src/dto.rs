//! 各方法的参数与结果类型（DTO）。
//!
//! M0 阶段只覆盖 `core.*` 四个方法与取消请求；其余域（workspace / llm / agent /
//! session / provider / terminal / git / mcp / permission）在对应里程碑中追加。
//!
//! 约定：所有数值型字段显式标注 `#[ts(type = "number")]`，避免 ts-rs 把 64 位整数
//! 映射成 `bigint` 而与本项目「JSON 里就是 number」的约定冲突。

use crate::version::ProtocolVersion;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// 类型导出目录（相对 crate 根，即 `crates/protocol`）。
const TS_DIR: &str = "../../packages/shared-types/src/generated/";

// ---------------------------------------------------------------------------
// core.hello —— 握手
// ---------------------------------------------------------------------------

/// `core.hello` 请求参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HelloParams {
    /// 客户端（Electron Main）版本。
    pub client_version: String,
    /// 客户端支持的协议版本。
    pub protocol_version: ProtocolVersion,
    /// 平台标识，取自 `os.platform()`，例如 `win32`。
    pub platform: String,
    /// 界面语言，例如 `zh-CN`。
    pub locale: String,
    /// 客户端能力开关，供内核按需降级，例如 `["tcp-debug"]`。
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// `core.hello` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HelloResult {
    /// 内核版本（取自 crate 版本）。
    pub core_version: String,
    /// 内核实现的协议版本。
    pub protocol_version: ProtocolVersion,
    /// 内核已实现的能力清单（方法名），便于客户端按能力开关 UI。
    pub capabilities: Vec<String>,
    /// 内核启动时刻（Unix 毫秒）。
    #[ts(type = "number")]
    pub started_at_ms: i64,
}

// ---------------------------------------------------------------------------
// core.ping —— 心跳
// ---------------------------------------------------------------------------

/// `core.ping` 请求参数（空对象）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PingParams {}

/// `core.ping` 响应结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PingResult {
    pub pong: bool,
    /// 内核已运行时长（毫秒）。
    #[ts(type = "number")]
    pub uptime_ms: u64,
}

// ---------------------------------------------------------------------------
// core.stats —— 资源自检
// ---------------------------------------------------------------------------

/// `core.stats` 请求参数（空对象）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StatsParams {}

/// `core.stats` 响应结果。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StatsResult {
    /// 常驻内存（MB）。
    pub rss_mb: f64,
    /// 已处理请求总数。
    #[ts(type = "number")]
    pub requests_handled: u64,
    /// 当前在途任务数。
    #[ts(type = "number")]
    pub tasks_active: u64,
    /// 内核已运行时长（毫秒）。
    #[ts(type = "number")]
    pub uptime_ms: u64,
}

// ---------------------------------------------------------------------------
// core.shutdown —— 退出
// ---------------------------------------------------------------------------

/// `core.shutdown` 请求参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ShutdownParams {
    /// `true` 表示刷盘后再退出；`false` 表示立即退出。
    #[serde(default)]
    pub graceful: bool,
}

/// `core.shutdown` 响应结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ShutdownResult {
    pub ok: bool,
}

// ---------------------------------------------------------------------------
// $/cancelRequest —— 取消在途请求
// ---------------------------------------------------------------------------

/// `$/cancelRequest` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CancelParams {
    /// 被取消请求的 `id`。
    pub id: String,
}

// ---------------------------------------------------------------------------
// core.log —— 内核日志事件（通知，无 id）
// ---------------------------------------------------------------------------

/// 内核日志级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// `core.log` 通知载荷。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LogEvent {
    pub level: LogLevel,
    /// 来源模块，例如 `core.rpc`。
    pub target: String,
    pub message: String,
    /// 发生时刻（Unix 毫秒）。
    #[ts(type = "number")]
    pub at_ms: i64,
}

// ---------------------------------------------------------------------------
// workspace.* —— 工作区与文件系统
// ---------------------------------------------------------------------------

/// `workspace.open` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceOpenParams {
    /// 工作区根目录，绝对路径。
    pub path: String,
    /// 是否启动文件监听；默认关闭，由 UI 显式开启。
    #[serde(default)]
    pub watch: bool,
}

/// 工作区规模统计。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceStats {
    /// 纳入索引的文件数。
    #[ts(type = "number")]
    pub files: u64,
    /// 目录数。
    #[ts(type = "number")]
    pub dirs: u64,
    /// 被忽略规则排除的条目数。
    #[ts(type = "number")]
    pub ignored: u64,
}

/// `workspace.open` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceOpenResult {
    /// 工作区句柄，后续调用凭此定位。
    pub workspace_id: String,
    /// 规范化后的根目录绝对路径。
    pub root: String,
    /// 根目录名，用于标题展示。
    pub name: String,
    /// 规模统计；`indexing` 为 `true` 时该值为增量快照。
    pub stats: WorkspaceStats,
    /// 是否正在后台建立全量索引。
    #[serde(default)]
    pub indexing: bool,
}

/// `workspace.indexed` 事件载荷：后台索引完成。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceIndexedEvent {
    pub workspace_id: String,
    /// 最终规模统计。
    pub stats: WorkspaceStats,
}

/// `workspace.close` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceCloseParams {
    pub workspace_id: String,
}

/// `workspace.close` / `workspace.create` / `workspace.rename` /
/// `workspace.delete` / `workspace.reveal` 的通用结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct OkResult {
    pub ok: bool,
}

/// 目录条目类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// 目录树中的一个条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TreeEntry {
    /// 条目名（不含父路径）。
    pub name: String,
    /// 绝对路径。
    pub path: String,
    pub kind: EntryKind,
    /// 字节数；目录为 0。
    #[ts(type = "number")]
    pub size: u64,
    /// 最后修改时刻（Unix 毫秒）；不可得时为 0。
    #[ts(type = "number")]
    pub mtime: i64,
    /// 是否命中忽略规则（UI 灰显，但仍返回）。
    pub ignored: bool,
}

/// `workspace.tree` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceTreeParams {
    pub workspace_id: String,
    /// 相对工作区根的路径；缺省表示根目录。
    #[serde(default)]
    pub path: Option<String>,
    /// 递归深度，1 表示只列一层；缺省为 1。
    #[serde(default)]
    pub depth: Option<u32>,
    /// 是否把忽略的条目一并返回（UI 未开启「显示忽略项」时应为 false）。
    #[serde(default)]
    pub include_ignored: bool,
}

/// `workspace.tree` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceTreeResult {
    pub entries: Vec<TreeEntry>,
}

/// 文本换行风格。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum Eol {
    Lf,
    Crlf,
    Cr,
}

/// `workspace.read_file` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceReadFileParams {
    /// 绝对路径，或相对工作区根的路径。
    pub path: String,
    /// 指定编码则强制按该编码解码；缺省为自动探测。
    #[serde(default)]
    pub encoding: Option<String>,
    /// 最大读取字节数，超出即截断；缺省 8 MiB。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub max_bytes: Option<u64>,
}

/// `workspace.read_file` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceReadFileResult {
    /// 解码后的文本，换行统一为 `\n`（原始风格见 `eol`，保存时内核会还原）。
    pub content: String,
    /// 实际使用的编码名，例如 `utf-8` / `utf-16le` / `gbk`。
    pub encoding: String,
    /// 原始换行风格。
    pub eol: Eol,
    /// 文件字节数（截断前）。
    #[ts(type = "number")]
    pub size: u64,
    /// 是否因 `max_bytes` 被截断。
    pub truncated: bool,
    /// 磁盘内容的 SHA-256（十六进制），用于乐观锁。
    pub hash: String,
}

/// `workspace.write_file` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceWriteFileParams {
    /// 绝对路径，或相对工作区根的路径。
    pub path: String,
    /// 新内容，换行按 `\n` 传入。
    pub content: String,
    /// 乐观锁：与磁盘当前内容不符时拒绝写入（错误码 3003）。
    #[serde(default)]
    pub expected_hash: Option<String>,
    /// 换行风格；缺省沿用文件原有风格（新文件为 `lf`）。
    #[serde(default)]
    pub eol: Option<Eol>,
}

/// `workspace.write_file` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceWriteFileResult {
    pub ok: bool,
    /// 写入后磁盘内容的 SHA-256（十六进制）。
    pub hash: String,
}

/// `workspace.create` 的条目类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum CreateKind {
    File,
    Dir,
}

/// `workspace.create` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceCreateParams {
    pub path: String,
    pub kind: CreateKind,
}

/// `workspace.rename` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceRenameParams {
    pub from: String,
    pub to: String,
    /// 目标已存在时是否覆盖；默认 false。
    #[serde(default)]
    pub overwrite: bool,
}

/// `workspace.delete` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceDeleteParams {
    pub paths: Vec<String>,
    /// 是否移入回收站；协议固定为 `true`，永久删除不在能力范围内。
    pub to_trash: bool,
}

/// `workspace.delete` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceDeleteResult {
    /// 全部成功时为 true；部分失败见 `failed`。
    pub ok: bool,
    /// 删除失败的路径。
    pub failed: Vec<String>,
}

/// `workspace.reveal` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceRevealParams {
    pub path: String,
}

/// 外部改动的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum PathChangeKind {
    Created,
    Modified,
    Deleted,
    Renamed,
}

/// 单条路径变更。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PathChange {
    pub path: String,
    pub kind: PathChangeKind,
    /// 仅 `renamed` 时给出原路径。
    #[serde(default)]
    pub old_path: Option<String>,
}

/// `workspace.changed` 事件载荷。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceChangedEvent {
    pub workspace_id: String,
    pub changes: Vec<PathChange>,
}

// ---------------------------------------------------------------------------
// search.* —— 检索
// ---------------------------------------------------------------------------

/// `search.files` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SearchFilesParams {
    pub workspace_id: String,
    /// 查询串，按子序列模糊匹配（`abc` 可匹配 `a/b/c.ts`）。
    pub query: String,
    /// glob 过滤，例如 `*.ts`。
    #[serde(default)]
    pub glob: Option<String>,
    /// 最大返回条数；缺省 200。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub limit: Option<u32>,
}

/// 文件名匹配结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileMatch {
    /// 相对工作区根的路径。
    pub path: String,
    /// 匹配得分，越大越相关。
    pub score: f64,
}

/// `search.files` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SearchFilesResult {
    pub matches: Vec<FileMatch>,
}

/// `search.content` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SearchContentParams {
    pub workspace_id: String,
    /// 查询串。
    pub query: String,
    /// 是否按正则解释 `query`；默认 false（字面量）。
    #[serde(default)]
    pub is_regex: bool,
    /// 是否大小写敏感；默认 false。
    #[serde(default)]
    pub case_sensitive: bool,
    /// 包含的 glob 列表，例如 `["src/**"]`。
    #[serde(default)]
    pub includes: Vec<String>,
    /// 排除的 glob 列表。
    #[serde(default)]
    pub excludes: Vec<String>,
    /// 最大命中数；缺省 500。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub max_results: Option<u32>,
}

/// 内容命中。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ContentMatch {
    /// 相对工作区根的路径。
    pub path: String,
    /// 行号，从 1 开始。
    #[ts(type = "number")]
    pub line: u32,
    /// 列号，从 1 开始（按字符计）。
    #[ts(type = "number")]
    pub column: u32,
    /// 命中所在行的文本（已去除换行，超长会截断）。
    pub preview: String,
}

/// `search.content` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SearchContentResult {
    pub matches: Vec<ContentMatch>,
    /// 是否因达到上限而提前停止。
    pub truncated: bool,
}

/// 供 TS 侧引用的导出目录常量说明（不参与序列化，仅为文档服务）。
pub const _TS_EXPORT_DIR: &str = TS_DIR;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_result_roundtrip() {
        let value = HelloResult {
            core_version: "0.1.0".into(),
            protocol_version: ProtocolVersion::CURRENT,
            capabilities: vec!["core.ping".into()],
            started_at_ms: 1_700_000_000_000,
        };
        let json = serde_json::to_value(&value).unwrap();
        assert_eq!(json["protocol_version"]["major"], 1);
        assert_eq!(json["protocol_version"]["minor"], 0);
        let back: HelloResult = serde_json::from_value(json).unwrap();
        assert_eq!(back, value);
    }

    #[test]
    fn ping_params_deserialize_from_empty_object() {
        let params: PingParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(params, PingParams {});
    }

    #[test]
    fn shutdown_graceful_defaults_false() {
        let params: ShutdownParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!params.graceful);
    }

    #[test]
    fn log_level_is_lowercase() {
        let json = serde_json::to_string(&LogLevel::Warn).unwrap();
        assert_eq!(json, "\"warn\"");
    }
}
