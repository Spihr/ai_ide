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
    /// 本次构建耗时（毫秒），从开始遍历到索引可用。
    #[serde(default)]
    #[ts(type = "number")]
    pub duration_ms: u64,
    /// 是否为增量构建。
    ///
    /// 当前内核每次都是全量重建（`false`）；字段先占位，等增量重建落地后
    /// 前端不必再改协议就能区分「补扫了几个文件」与「重扫了整库」。
    #[serde(default)]
    pub incremental: bool,
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
    /// 索引是否仍在后台构建；`true` 时 `matches` 是降级结果（可能为空）。
    ///
    /// 与 `search.symbols` 同义：区分「真的没这个文件」与「还没扫完」，前端据此
    /// 提示「索引中」而不是把空结果当成结论。
    #[serde(default)]
    pub indexing: bool,
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

/// 符号种类。
///
/// 七种语言共用一套 kind，而不是各自一套：前端呈现的是一张「符号列表」，
/// 分组的维度应该是「这是函数还是类型」，而不是「这是 Rust 的 trait 还是 TS 的 interface」。
/// 因此 `trait` / `interface` / `struct` / `class` 各自独立，`function` / `method`
/// 按「是否定义在类型体内」区分（Rust 的 `impl`、TS/Java 的 `class`、Python 的 `class`、
/// Go 的接收者函数都算方法）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    /// 函数（模块级 / 顶层）。
    Function,
    /// 方法（定义在类型体内）。
    Method,
    /// 结构体（Rust `struct`、Go `type X struct`）。
    Struct,
    /// 类（TS / JS / Python / Java `class`）。
    Class,
    /// 枚举（Rust / TS / Java `enum`）。
    Enum,
    /// 接口（TS `interface`、Java `interface`、Go `type X interface`）。
    Interface,
    /// trait（Rust）。
    Trait,
    /// 常量（`const` / `static` / 模块级全大写赋值 / `static final`）。
    Const,
}

/// 符号在文件内的行范围（1 起算，闭区间）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SymbolRange {
    /// 声明起始行（含）。
    #[ts(type = "number")]
    pub start_line: u32,
    /// 声明结束行（含）。单行声明与 `start_line` 相等。
    #[ts(type = "number")]
    pub end_line: u32,
}

/// 一个符号。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Symbol {
    /// 名字（不含 `fn` / `class` 等关键字）。
    pub name: String,
    pub kind: SymbolKind,
    /// 相对工作区根的路径，`/` 分隔。
    pub path: String,
    pub range: SymbolRange,
}

/// `search.symbols` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SearchSymbolsParams {
    pub workspace_id: String,
    /// 查询串；空串表示「所有符号」。大小写不敏感，先按子串命中，再退化到子序列模糊匹配。
    pub query: String,
    /// 只返回这些种类；缺省（或空数组）表示不过滤。
    #[serde(default)]
    pub kinds: Option<Vec<SymbolKind>>,
}

/// `search.symbols` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SearchSymbolsResult {
    /// 命中上限由内核固定（200 条），按相关度排序。
    pub symbols: Vec<Symbol>,
    /// 索引是否仍在后台构建；`true` 时 `symbols` 是降级结果（可能为空）。
    #[serde(default)]
    pub indexing: bool,
}

// ---------------------------------------------------------------------------
// inline_edit.* —— 选区改写（M3 切片三）
// ---------------------------------------------------------------------------

/// 编辑器选区。**行号 1 起算、闭区间；列号 0 起算、右开区间**（与 LSP 的
/// `Range` 同口径，也与 `workspace.read_file` 回传的行序对得上）。
///
/// 两套基数是刻意的：人读行号从 1 开始，Monaco / VS Code 的列坐标从 0 开始。
/// 把它们统一成同一套基数，只会让其中一侧每次都差一行或差一列。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TextSelection {
    /// 起始行（含，1 起算）。
    #[ts(type = "number")]
    pub start_line: u32,
    /// 起始列（含，0 起算）。
    #[ts(type = "number")]
    pub start_column: u32,
    /// 结束行（含，1 起算）。
    #[ts(type = "number")]
    pub end_line: u32,
    /// 结束列（不含，0 起算）。
    #[ts(type = "number")]
    pub end_column: u32,
}

/// `inline_edit.propose` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InlineEditProposeParams {
    pub workspace_id: String,
    /// 相对工作区根的路径，`/` 分隔。
    pub path: String,
    /// 自然语言指令，例如「改成早退返回」「补上错误处理」。
    pub instruction: String,
    pub selection: TextSelection,
    /// 选区上下各带几行上下文，缺省 40。上限 400——再多也塞不进上下文预算，
    /// 且模型对超长前缀的定位能力会明显下降。
    #[serde(default)]
    pub context_lines: Option<u32>,
    /// 走哪个模型；缺省时用工作区/全局默认模型。与 `llm.chat_stream` 同一个结构。
    #[serde(default)]
    pub model_ref: Option<ModelRef>,
}

/// `inline_edit.propose` 响应结果。**内核不写盘**，只描述「应该改成什么」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InlineEditProposeResult {
    /// 原样回显目标路径（相对工作区根），外壳据此确认补丁打在哪份文件上。
    pub path: String,
    /// 实际被替换的选区。与请求原样一致——越界不会被「体贴地」收拢到合法范围，
    /// 而是直接回 9001：静默改选区会让用户的补丁打在没画到的地方。
    pub range: TextSelection,
    /// 被替换的原文（按 `range` 切出来的精确片段，不含上下文）。
    pub original: String,
    /// 模型给出的替换文本（已去掉代码围栏；无围栏时取整段响应）。
    pub replacement: String,
    /// 统一格式补丁（`--- a/<path>` / `+++ b/<path>`），可直接交给 diff 视图渲染。
    pub diff: String,
    /// 上游用量；provider 没回传时缺省（不是补零——0 与「未上报」含义不同，
    /// 计费面板把两者混起来就会少算）。
    #[serde(default)]
    pub usage: Option<InlineEditUsage>,
}

/// Inline Edit 的用量。字段与 `llm.usage` 事件同口径，但独立成协议结构：
/// `ai-ide-protocol` 不依赖 `ai-ide-llm`（契约层不能反过来依赖实现层），
/// 因此这里只能是镜像，而不是复用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InlineEditUsage {
    #[ts(type = "number")]
    pub prompt_tokens: u64,
    #[ts(type = "number")]
    pub completion_tokens: u64,
    /// 命中提示缓存的 token 数；上游未报为 `None`。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub cached_tokens: Option<u64>,
}

// ---------------------------------------------------------------------------
// provider.* / model.* —— Provider 与模型目录（M2）
// ---------------------------------------------------------------------------

/// 上游协议种类。M2 只落地 `openai_chat_completions`（其余为已定义、待实现的适配器）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    /// OpenAI Chat Completions，覆盖所有兼容网关（DeepSeek / Kimi / 智谱 / OpenRouter …）。
    OpenaiChatCompletions,
    /// OpenAI Responses API。
    OpenaiResponses,
    /// Anthropic Messages API。
    AnthropicMessages,
    /// Google Gemini `generateContent`。
    Gemini,
    /// Ollama 本地服务。
    Ollama,
}

/// `provider.upsert` 请求参数。
///
/// 不带 `id` 即新建；带 `id` 即原地更新（`api_key` 缺省表示沿用已存密钥）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProviderUpsertParams {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub protocol: ProviderProtocol,
    /// 接口根地址，例如 `https://api.deepseek.com/v1`。
    pub base_url: String,
    /// API Key。**只进内存保管**（docs/02 §6.3），内核与磁盘上的自定义配置里都不落明文。
    #[serde(default)]
    pub api_key: Option<String>,
    /// 附加请求头（例如网关要求的 `X-Api-Version`）。
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    /// 是否启用。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 单次请求超时（毫秒）；缺省 60000。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub timeout_ms: Option<u64>,
}

/// `provider.upsert` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProviderUpsertResult {
    pub provider_id: String,
}

/// `provider.list` 请求参数（空对象）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProviderListParams {}

/// Provider 的脱敏视图：**任何字段都不含明文密钥**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    pub protocol: ProviderProtocol,
    pub base_url: String,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    pub enabled: bool,
    /// 是否已配置密钥。
    pub has_key: bool,
    /// 密钥指纹，形如 `sk-a…4f2c`；未配置时为 null。
    #[serde(default)]
    #[ts(optional)]
    pub key_hint: Option<String>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub timeout_ms: Option<u64>,
    /// 最近一次 `provider.test` 是否通过；从未测试过为 null。
    #[serde(default)]
    #[ts(type = "boolean | null")]
    pub last_test_ok: Option<bool>,
    #[ts(type = "number")]
    pub created_at_ms: i64,
    #[ts(type = "number")]
    pub updated_at_ms: i64,
}

/// `provider.list` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProviderListResult {
    pub providers: Vec<ProviderView>,
}

/// `provider.delete` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProviderDeleteParams {
    pub provider_id: String,
}

/// `provider.test` 的内联覆盖项：允许「先试连通、再决定是否保存」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProviderTestOverride {
    pub protocol: ProviderProtocol,
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
}

/// `provider.test` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProviderTestParams {
    #[serde(default)]
    pub provider_id: Option<String>,
    /// 内联覆盖（JS 侧字段名就是 `override`）。
    #[serde(default, rename = "override")]
    pub override_spec: Option<ProviderTestOverride>,
}

/// `provider.test` 响应结果。连通性判定只看「能否拿到模型列表」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProviderTestResult {
    pub ok: bool,
    #[ts(type = "number")]
    pub latency_ms: u64,
    #[ts(type = "number")]
    pub models_found: u64,
    /// 失败原因摘要或附加说明。
    #[serde(default)]
    pub message: Option<String>,
}

/// 模型能力标注。决定 UI 是否显示推理折叠块、是否允许挂工具。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelCaps {
    /// 是否支持流式输出。
    pub streaming: bool,
    /// 是否支持工具调用。
    pub tools: bool,
    /// 是否输出思维链（OpenAI 兼容的 `reasoning_content`）。
    pub reasoning: bool,
    /// 是否支持图片输入。
    pub vision: bool,
    /// 上下文窗口（token）；未知为 null。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub context_window: Option<u64>,
}

impl Default for ModelCaps {
    fn default() -> Self {
        Self {
            streaming: true,
            tools: false,
            reasoning: false,
            vision: false,
            context_window: None,
        }
    }
}

/// 模型级默认采样参数，请求未显式给值时使用。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelDefaultParams {
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub top_p: Option<f64>,
}

/// `model.upsert` 请求参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelUpsertParams {
    pub provider_id: String,
    /// 上游模型名，例如 `deepseek-chat`。
    pub model_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub caps: Option<ModelCaps>,
    #[serde(default)]
    pub params: Option<ModelDefaultParams>,
}

/// `model.upsert` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelUpsertResult {
    /// 模型条目的稳定主键（`provider_id` + `model_id`）。
    pub model_pk: String,
}

/// 模型条目视图。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelView {
    pub model_pk: String,
    pub provider_id: String,
    pub model_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    pub caps: ModelCaps,
    pub params: ModelDefaultParams,
    #[ts(type = "number")]
    pub created_at_ms: i64,
}

/// `model.list` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelListParams {
    /// 只列某个 Provider 下的模型；缺省列出全部。
    #[serde(default)]
    pub provider_id: Option<String>,
}

/// `model.list` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelListResult {
    pub models: Vec<ModelView>,
}

/// `model.delete` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelDeleteParams {
    pub model_pk: String,
}

// ---------------------------------------------------------------------------
// llm.* —— 流式对话（M2）
// ---------------------------------------------------------------------------

/// 模型引用：Provider + 上游模型名。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelRef {
    pub provider_id: String,
    pub model_id: String,
}

/// 对话角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

/// 消息附件（M2 只透传路径，读取与去重由 M3 的 @上下文接手）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ChatAttachment {
    /// 附件类型，当前只有 `file`。
    pub kind: String,
    /// 相对工作区根的路径。
    pub path: String,
}

/// 一次工具调用（助手发起）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ChatToolCall {
    pub id: String,
    pub name: String,
    /// 参数 JSON 原文（不做解析，避免上游半截 JSON 导致丢内容）。
    pub arguments: String,
}

/// 一条对话消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ChatMessage {
    pub role: ChatRole,
    /// 文本内容；纯工具调用消息可能为空串。
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<ChatAttachment>,
    /// 助手消息携带的工具调用。
    #[serde(default)]
    pub tool_calls: Vec<ChatToolCall>,
    /// `role = tool` 时指向被回应的调用 id。
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

/// 可挂载的工具声明。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ToolSpec {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema。
    pub parameters: serde_json::Value,
}

/// 工具选择策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
}

/// 单次请求的采样参数。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ChatParams {
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    /// 停止序列。
    #[serde(default)]
    pub stop: Vec<String>,
    /// 厂商私有参数透传（例如 `{"top_k": 40}`）。
    #[serde(default)]
    pub extra: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// 上下文引用（M3）
// ---------------------------------------------------------------------------

/// `@` 引用的种类。
///
/// 四类对应编辑器里四种被选中的对象：文件、目录、选区、当前文件。
/// `current_file` 与 `file` 的注入格式完全一致，区别只在语义——前者由调用方在
/// 「用户没显式选文件、但编辑器里有活动文件」的场合给出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ContextRefKind {
    /// `@文件`：注入文件内容（超长时按单文件上限裁剪）。
    File,
    /// `@文件夹`：注入目录树（浅层，不递归到底）。
    Folder,
    /// `@选区`：注入一段行区间，或选区原文。
    Selection,
    /// `@当前文件`：注入编辑器里的活动文件。
    CurrentFile,
}

/// 注入内容的压缩策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum CompressStrategy {
    /// 按 token 预算裁剪（M3 落地的策略）。
    #[default]
    Truncate,
    /// 摘要压缩（M3 落地）：放不下的引用保留段头与段尾、中间以标注交代省略行数，
    /// 结构完整。走**本地抽取**，不调用模型、不读密钥、不联网；*模型*摘要属后续切片。
    Summarize,
}

/// 一条上下文引用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ContextRef {
    pub kind: ContextRefKind,
    /// 工作区内的路径。相对路径按工作区根展开；绝对路径只要落在工作区内也接受。
    /// 越界路径会让整个 `llm.chat_stream` 报错，不会被静默丢弃。
    pub path: String,
    /// 行区间起点（1-based，含）。`file` / `current_file` / `selection` 有效。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub start_line: Option<u32>,
    /// 行区间终点（1-based，含）。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub end_line: Option<u32>,
    /// 选区原文。仅 `selection` 未给行号时使用；给了行号时以行号为准，
    /// 免得两处口径不一致。
    #[serde(default)]
    pub text: Option<String>,
    /// 展示名（UI chip 文案）。只用于界面，不注入对话内容。
    #[serde(default)]
    pub label: Option<String>,
}

/// 注入预算与压缩策略。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CompressOptions {
    /// 是否启用预算裁剪；缺省为 `true`。
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub strategy: CompressStrategy,
    /// 注入内容的 token 预算；留空用内核默认值。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub budget_tokens: Option<u64>,
}

impl Default for CompressOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            strategy: CompressStrategy::Truncate,
            budget_tokens: None,
        }
    }
}

/// `llm.chat_stream` 的上下文引用参数。
///
/// 缺省（`null`）时内核不注入任何内容——M2 的行为原样保留，老调用方无需改动。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ChatContext {
    /// 引用所属工作区；留空回落到活动工作区。
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// 引用列表。**注入顺序即数组顺序**。
    #[serde(default)]
    pub refs: Vec<ContextRef>,
    /// 注入预算；留空用默认值。
    #[serde(default)]
    pub compress: Option<CompressOptions>,
}

/// `llm.chat_stream` 请求参数。
///
/// 方法立即返回 `{stream_id}`，内容通过 `llm.*` 事件族推送——这样 UI 的
/// 「发送」按钮不会被网络往返阻塞，取消也能作用在真正在途的那条流上。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LlmChatStreamParams {
    /// 调用方生成的流 id（同时作为 `$/cancelRequest` 之外的取消句柄）。
    pub stream_id: String,
    /// 会话 id（M3 会话持久化用，M2 仅原样回落到事件里）。
    #[serde(default)]
    pub session_id: Option<String>,
    pub model_ref: ModelRef,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    #[serde(default)]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default)]
    pub params: Option<ChatParams>,
    /// 上下文引用（M3）。内核把引用内容渲染成一条 `system` 消息，插在 `messages` 之前。
    #[serde(default)]
    pub context: Option<ChatContext>,
}

/// `llm.chat_stream` 响应结果（立即返回）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LlmChatStreamResult {
    pub stream_id: String,
}

/// `llm.cancel` 请求参数。幂等：不存在的 `stream_id` 也回成功。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LlmCancelParams {
    pub stream_id: String,
}

/// `llm.list_models` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LlmListModelsParams {
    pub provider_id: String,
}

/// 上游模型条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub owned_by: Option<String>,
}

/// `llm.list_models` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LlmListModelsResult {
    pub models: Vec<ModelInfo>,
}

/// `llm.count_tokens` 请求参数。
///
/// 两个口径可以同时给出，结果相加：
/// - `messages`：消息口径，含每条消息的固定结构开销；
/// - `texts`：纯文本口径，用于输入框里还没组装成消息的引用片段。
///
/// 两者都缺省时结果为 0（不报错）：UI 每敲一个字都会调它，为空格子报错只会刷屏。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LlmCountTokensParams {
    pub model_ref: ModelRef,
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub texts: Vec<String>,
}

/// `llm.count_tokens` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LlmCountTokensResult {
    #[ts(type = "number")]
    pub tokens: u64,
    /// 是否为精确值。模型族已接真实分词器（`o200k_base` / `cl100k_base`）时为 true，
    /// 未识别模型族回落估算为 false；两种口径都在 `note` 里说明。
    pub exact: bool,
    /// 计数口径说明。
    #[serde(default)]
    pub note: Option<String>,
}

/// 流结束原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    /// 上游给了未知值时的兜底（原始值见 `llm.error` 之外的日志）。
    Unknown,
}

/// `llm.started` 事件载荷。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LlmStartedEvent {
    pub stream_id: String,
    pub model_ref: ModelRef,
    #[serde(default)]
    pub session_id: Option<String>,
    #[ts(type = "number")]
    pub started_at_ms: i64,
}

/// `llm.delta` 事件载荷：正文增量。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LlmDeltaEvent {
    pub stream_id: String,
    pub text: String,
}

/// `llm.reasoning` 事件载荷：思维链增量。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LlmReasoningEvent {
    pub stream_id: String,
    pub text: String,
}

/// `llm.tool_call` 事件载荷：按片段拼接的工具调用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LlmToolCallEvent {
    pub stream_id: String,
    /// 调用序号（同一条流内从 0 开始）。
    #[ts(type = "number")]
    pub index: u32,
    /// 首帧带 id，后续帧为 null。
    #[serde(default)]
    pub id: Option<String>,
    /// 首帧带函数名，后续帧为 null。
    #[serde(default)]
    pub name: Option<String>,
    /// 参数 JSON 的增量片段。
    pub arguments_delta: String,
}

/// `llm.usage` 事件载荷。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LlmUsageEvent {
    pub stream_id: String,
    #[ts(type = "number")]
    pub prompt_tokens: u64,
    #[ts(type = "number")]
    pub completion_tokens: u64,
    /// 命中提示缓存的 token 数；上游未报为 null。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub cached_tokens: Option<u64>,
}

/// `llm.finished` 事件载荷。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LlmFinishedEvent {
    pub stream_id: String,
    pub finish_reason: FinishReason,
    /// 上游响应 id（若有）。
    #[serde(default)]
    pub message_id: Option<String>,
}

/// `llm.error` 事件载荷：流异常终止。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LlmErrorEvent {
    pub stream_id: String,
    pub code: crate::error::ErrorCode,
    pub message: String,
    /// 上游原始错误体（脱敏后），便于用户自查配额与模型名。
    #[serde(default)]
    pub upstream: Option<serde_json::Value>,
    /// 上游建议的重试等待（毫秒，来自 `Retry-After`）。
    ///
    /// 有它 UI 才能显示「N 秒后重试」；只报一句「限流了」等于把最有用的一半信息丢掉。
    #[serde(default)]
    pub retry_after_ms: Option<u64>,
}

/// `provider.changed` 事件载荷。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProviderChangedEvent {
    /// 变更类型：`upserted` / `deleted`。
    pub kind: String,
    pub provider_id: String,
}

// ---------------------------------------------------------------------------
// agent.* —— Agent 运行时（M4）
// ---------------------------------------------------------------------------
//
// M4 第三切片先定协议面：`agent.start` / `agent.stop` / `agent.decide` 三个请求，
// 以及 `agent.approval_required` / `agent.finished` 两条通知；内核接线在下一轮。
//
// 裁决动作只有一处定义（[`ApprovalDecision`]）：`crates/agent` 的权限裁决点直接
// 复用它，不另立一套枚举，免得两侧漂移成「协议说 allow_once、内核说 allow」。

/// Agent 运行模式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode {
    /// 纯对话，不跑工具。
    Chat,
    /// 闭环执行：模型 → 工具 → 模型。
    #[default]
    Agent,
    /// 行内小改写。
    Inline,
}

/// 一次 Agent 运行的终态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum AgentRunStatus {
    /// 模型正常收手。
    Completed,
    /// 撞上限，或模型 / 工具故障。
    Failed,
    /// 被 `agent.stop` 或 `$/cancelRequest` 打断。
    Cancelled,
}

/// 用户对一次工具调用的裁决动作。
///
/// - `allow_once`：只放行本次。
/// - `allow_always`：同一工具同一范围在**本进程内**不再重复询问。
/// - `deny` / `deny_with_reason`：拒绝；后者额外带上 `reason`，供回灌模型自纠。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    AllowOnce,
    AllowAlways,
    Deny,
    DenyWithReason,
}

/// 待裁决调用的风险等级，供 UI 决定要不要标红。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalRisk {
    Low,
    Medium,
    High,
}

/// `agent.start` 的权限覆盖（只收窄，不放大）。
///
/// 内核的安全红线（越界路径 `1002`、敏感路径 `1003`、高危命令 `3001`）是硬拒，
/// 这里的 `allow_tools` 翻不动它们。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentPermissionOverride {
    /// 直接放行的工具名（精确匹配）。
    #[serde(default)]
    pub allow_tools: Vec<String>,
    /// 直接拒绝的工具名（精确匹配，优先于 `allow_tools`）。
    #[serde(default)]
    pub deny_tools: Vec<String>,
}

/// `agent.start` 请求参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentStartParams {
    /// 会话 id；留空表示本次运行不落会话。
    #[serde(default)]
    pub session_id: Option<String>,
    /// 本次运行使用的模型。
    pub model_ref: ModelRef,
    /// 目标描述（自然语言）。
    pub goal: String,
    /// 运行模式；缺省 `agent`。
    #[serde(default)]
    pub mode: AgentMode,
    /// 首轮注入的上下文引用，口径与 `llm.chat_stream` 的 `context.refs` 一致。
    #[serde(default)]
    pub context_refs: Vec<ContextRef>,
    /// 步数上限；留空用内核默认值。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub max_steps: Option<u32>,
    /// 权限覆盖；留空即内核默认策略。
    #[serde(default)]
    pub permissions_override: Option<AgentPermissionOverride>,
}

/// `agent.start` 响应结果（立即返回，不等跑完）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentStartResult {
    /// 本次运行的句柄；`agent.stop` / `agent.decide` 与 `agent.*` 事件族都用它。
    pub run_id: String,
}

/// `agent.stop` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentStopParams {
    pub run_id: String,
}

/// `agent.stop` 响应结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentStopResult {
    pub ok: bool,
    /// 停止前已完成的步数，便于 UI 交代「走到哪停的」。
    #[ts(type = "number")]
    pub steps_done: u32,
}

/// `agent.decide` 请求参数：对一次权限询问的应答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentDecideParams {
    pub run_id: String,
    /// 被裁决的询问 id（来自 `agent.approval_required`）。
    pub approval_id: String,
    pub decision: ApprovalDecision,
    /// 拒绝理由；`deny_with_reason` 时可带上，供回灌模型自纠。
    #[serde(default)]
    pub reason: Option<String>,
}

/// `agent.decide` 响应结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentDecideResult {
    pub ok: bool,
}

/// `agent.approval_required` 事件载荷：这一步需要用户裁决。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentApprovalRequiredEvent {
    pub run_id: String,
    pub approval_id: String,
    /// 工具名（`read` / `edit` / `run_command` …）。
    pub tool: String,
    /// 工具参数原文，直接给 UI 展示「它打算干什么」。
    pub args: serde_json::Value,
    pub risk: ApprovalRisk,
    /// 为什么问这一声（人话，不是错误码）。
    pub reason: String,
}

/// `agent.finished` 事件载荷：一次运行结束。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentFinishedEvent {
    pub run_id: String,
    pub status: AgentRunStatus,
    /// 实际走了多少步。
    #[ts(type = "number")]
    pub steps: u32,
    /// 给用户看的一句话收尾（模型总结或失败原因）。
    #[serde(default)]
    pub summary: Option<String>,
}

// ---------------------------------------------------------------------------
// session.* —— 会话与消息（M5）
// ---------------------------------------------------------------------------

/// 一条会话的对外视图。
///
/// 只回 `workspace_path` 而不回 `workspace_id`：工作区 id 是运行期句柄，重启后
/// 由 Main 重新打开才存在；落盘的是绝对路径，它才是跨进程稳定的身份。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionView {
    pub session_id: String,
    pub title: String,
    /// 会话绑定的工作区绝对路径；跨工作区会话为空。
    #[serde(default)]
    pub workspace_path: Option<String>,
    /// 会话默认模型。
    #[serde(default)]
    pub model_ref: Option<ModelRef>,
    /// 创建时刻（Unix 毫秒）。
    #[ts(type = "number")]
    pub created_at_ms: i64,
    /// 最后活动时刻（Unix 毫秒）：追加消息与改名都会推进。
    #[ts(type = "number")]
    pub updated_at_ms: i64,
}

/// 一条消息的对外视图：在 [`ChatMessage`] 之上补消息 id、所属会话与时间。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MessageView {
    pub message_id: String,
    pub session_id: String,
    /// 父消息 id（编辑重发 / 分支链）。
    #[serde(default)]
    pub parent_id: Option<String>,
    pub role: ChatRole,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<ChatAttachment>,
    #[serde(default)]
    pub tool_calls: Vec<ChatToolCall>,
    /// `role = tool` 时指向被回应的调用 id。
    #[serde(default)]
    pub tool_call_id: Option<String>,
    /// 产生该消息时使用的模型。
    #[serde(default)]
    pub model_ref: Option<ModelRef>,
    /// 上游用量（`llm.usage` 事件体，原样保存）。
    #[serde(default)]
    pub usage: Option<serde_json::Value>,
    /// 创建时刻（Unix 毫秒）。
    #[ts(type = "number")]
    pub created_at_ms: i64,
}

/// `session.create` 请求参数。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCreateParams {
    /// 会话标题；缺省由内核生成（`新会话`）。
    #[serde(default)]
    pub title: Option<String>,
    /// 绑定到已打开的工作区；不给则是跨工作区会话。
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// 会话默认模型。
    #[serde(default)]
    pub model_ref: Option<ModelRef>,
}

/// `session.create` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCreateResult {
    pub session_id: String,
    pub session: SessionView,
}

/// `session.list` 请求参数。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionListParams {
    /// 只看某个工作区的会话。
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// 标题关键词（大小写不敏感的子串匹配）。
    #[serde(default)]
    pub query: Option<String>,
    /// 返回条数上限，缺省 50，内部夹到 1..=500。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub limit: Option<u32>,
    /// 偏移量，缺省 0。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub offset: Option<u32>,
}

/// `session.list` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionListResult {
    /// 按 `updated_at_ms` 倒序。
    pub sessions: Vec<SessionView>,
    /// 过滤条件下的命中总数（不受 `limit` / `offset` 影响）。
    #[ts(type = "number")]
    pub total: u64,
}

/// `session.get` 请求参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionGetParams {
    pub session_id: String,
    /// 是否一并返回消息；缺省 `true`（新会话只有零条消息，代价可忽略）。
    #[serde(default = "default_true")]
    pub include_messages: bool,
}

/// `session.get` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionGetResult {
    pub session: SessionView,
    /// `include_messages=false` 时为空数组。
    #[serde(default)]
    pub messages: Vec<MessageView>,
}

/// `session.update` 请求参数：改名 / 换默认模型，两者至少给一个。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionUpdateParams {
    pub session_id: String,
    /// 新标题。
    #[serde(default)]
    pub title: Option<String>,
    /// 新的会话默认模型。
    #[serde(default)]
    pub model_ref: Option<ModelRef>,
}

/// `session.delete` 请求参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionDeleteParams {
    /// 待删除的会话 id；不存在的 id 会被忽略（幂等）。
    pub session_ids: Vec<String>,
}

/// `session.delete` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionDeleteResult {
    pub ok: bool,
    /// 实际删掉的会话数（消息随会话级联删除）。
    #[ts(type = "number")]
    pub deleted: u64,
}

/// `session.append_message` 请求参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAppendMessageParams {
    pub session_id: String,
    /// 消息本体（角色、文本、附件、工具调用）。
    pub message: ChatMessage,
    /// 父消息 id（编辑重发 / 分支链）。
    #[serde(default)]
    pub parent_id: Option<String>,
    /// 产生该消息时使用的模型。
    #[serde(default)]
    pub model_ref: Option<ModelRef>,
    /// 上游用量 JSON。
    #[serde(default)]
    pub usage: Option<serde_json::Value>,
}

/// `session.append_message` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAppendMessageResult {
    pub message: MessageView,
}

/// `session.messages` 请求参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionMessagesParams {
    pub session_id: String,
    /// 返回条数上限，缺省 200，内部夹到 1..=10000。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub limit: Option<u32>,
    /// 偏移量，缺省 0。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub offset: Option<u32>,
}

/// `session.messages` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionMessagesResult {
    /// 按 `created_at_ms` 正序（同毫秒按写入顺序）。
    pub messages: Vec<MessageView>,
    /// 该会话的消息总数（不受 `limit` / `offset` 影响）。
    #[ts(type = "number")]
    pub total: u64,
}

// ---------------------------------------------------------------------------
// session.export / session.fork（M5 切片三）
// ---------------------------------------------------------------------------

/// 会话导出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum SessionExportFormat {
    /// 人读的 Markdown（默认）：一张会话摘要表 + 逐条消息。
    #[default]
    Markdown,
    /// 机器读的 JSON：`SessionExportDocument`，字段与协议 DTO 完全一致。
    Json,
}

impl SessionExportFormat {
    /// 文件扩展名（`out_path` 没带扩展名时补上）。
    pub fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Json => "json",
        }
    }
}

/// `session.export` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionExportParams {
    pub session_id: String,
    /// 导出格式，缺省 `markdown`。
    #[serde(default)]
    pub format: SessionExportFormat,
    /// 落盘路径：必须是绝对路径。指向已存在的目录时，内核在里面按会话名生成文件；
    /// 指向文件时直接写入（上层若已存在同名文件，由调用方决定是否覆盖）。
    pub out_path: String,
}

/// `session.export` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionExportResult {
    pub ok: bool,
    /// 实际写入的绝对路径（目录入参已折算成具体文件名）。
    pub path: String,
    /// 写入字节数。
    #[ts(type = "number")]
    pub bytes: u64,
    /// 导出的消息条数。
    #[ts(type = "number")]
    pub messages: u64,
}

/// JSON 导出的顶层文档。
///
/// `format_version` 固定为 1：将来字段演进时客户端据此判断兼容性，
/// 不靠「猜字段在不在」。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionExportDocument {
    pub format_version: u32,
    #[ts(type = "number")]
    pub exported_at_ms: i64,
    pub session: SessionView,
    pub messages: Vec<MessageView>,
}

/// `session.fork` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionForkParams {
    /// 源会话 id。
    pub session_id: String,
    /// 截断点：给出时只复制到该消息（含）为止；缺省复制全部消息。
    #[serde(default)]
    pub from_message_id: Option<String>,
    /// 新会话标题；缺省在原标题后加「（分支）」。
    #[serde(default)]
    pub title: Option<String>,
}

/// `session.fork` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionForkResult {
    pub new_session_id: String,
    /// 新会话快照，客户端可直接插进列表，不必再 `session.get`。
    pub session: SessionView,
    /// 复制过来的消息条数。
    #[ts(type = "number")]
    pub messages: u64,
}

/// 布尔字段的 serde 默认值：`true`。
fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// audit.*（M5 切片四）
// ---------------------------------------------------------------------------

/// 一条审计事件的对外视图。
///
/// `summary` 与 `detail` 在存储层写入与读取时各脱敏一次，这里拿到的已经是安全值：
/// 客户端不必（也无法）自己做脱敏。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AuditEventView {
    /// 自增主键，也是稳定的翻页锚点。
    #[ts(type = "number")]
    pub event_id: i64,
    /// 事件种类，形如 `session.created`；用字符串而非枚举，新增种类不破坏老客户端。
    pub kind: String,
    /// 关联会话；跨会话事件为空。
    #[serde(default)]
    pub session_id: Option<String>,
    /// 发起方：`core` / `user` / `agent`。
    pub actor: String,
    /// 人可读摘要（已脱敏）。
    pub summary: String,
    /// 结构化补充信息（已脱敏）。
    #[serde(default)]
    pub detail: serde_json::Value,
    /// 事件时刻（Unix 毫秒）。
    #[ts(type = "number")]
    pub created_at_ms: i64,
}

/// `audit.list` 请求参数。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AuditListParams {
    /// 只看某个会话的事件。
    #[serde(default)]
    pub session_id: Option<String>,
    /// 只看某种事件（如 `session.deleted`）。
    #[serde(default)]
    pub kind: Option<String>,
    /// 起始时刻（含，Unix 毫秒）。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub since_ms: Option<i64>,
    /// 结束时刻（含，Unix 毫秒）。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub until_ms: Option<i64>,
    /// 返回条数上限，缺省 50，内部夹到 1..=500。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub limit: Option<u32>,
    /// 偏移量，缺省 0。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub offset: Option<u32>,
}

/// `audit.list` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AuditListResult {
    /// 按 `created_at_ms` 倒序（同刻按 `event_id` 倒序）。
    pub events: Vec<AuditEventView>,
    /// 过滤条件下的命中总数（不受 `limit` / `offset` 影响）。
    #[ts(type = "number")]
    pub total: u64,
}

// ---------------------------------------------------------------------------
// checkpoint.*（M5 切片五：检查点与回滚）
// ---------------------------------------------------------------------------

/// 检查点是怎么来的。
///
/// 区分来源不是为了好看：UI 里「Agent 自动拍的」和「我手动拍的」要能分开列，
/// 排查「谁把这个文件改了」时第一眼看的就是这两种。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointOrigin {
    /// 用户手动创建。
    Manual,
    /// Agent 改动文件前自动创建。
    Agent,
}

/// 一个文件相对检查点时刻的处置方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointFileKind {
    /// 拍快照时存在且有内容：回滚要用快照覆盖回去。
    Modified,
    /// 拍快照时不存在：回滚要把它删掉。
    Created,
    /// 拍快照时存在但已被删除：回滚要把它恢复出来。
    Deleted,
}

/// 检查点里的一个文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CheckpointFileView {
    /// 相对工作区根的路径，统一用 `/` 分隔。
    pub path: String,
    pub kind: CheckpointFileKind,
    /// 快照时刻内容的 SHA-256（十六进制）。`created` 下为空。
    #[serde(default)]
    #[ts(optional)]
    pub before_hash: Option<String>,
    /// 快照内容的字节数。
    #[ts(type = "number")]
    pub size: u64,
}

/// 一个检查点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CheckpointView {
    pub checkpoint_id: String,
    /// 人可读标题，例如「Agent 改动前（run 8f2a）」。
    pub label: String,
    /// 建立快照时工作区的绝对根路径。回滚按它定位，不依赖工作区还开着。
    pub workspace_root: String,
    pub origin: CheckpointOrigin,
    /// Agent 自动检查点时关联的会话。
    #[serde(default)]
    #[ts(optional)]
    pub session_id: Option<String>,
    /// 建立时刻（Unix 毫秒）。
    #[ts(type = "number")]
    pub created_at_ms: i64,
    /// 快照覆盖的文件清单。
    pub files: Vec<CheckpointFileView>,
    /// 快照内容占用的字节数。
    #[ts(type = "number")]
    pub total_bytes: u64,
    /// 因为单文件体积 / 文件数 / 总容量上限被跳过的文件数。
    #[ts(type = "number")]
    pub skipped: u64,
}

/// `checkpoint.create` 请求参数。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CheckpointCreateParams {
    /// 目标工作区；缺省取当前活动工作区。
    #[serde(default)]
    #[ts(optional)]
    pub workspace_id: Option<String>,
    /// 标题；缺省时内核按来源生成。
    #[serde(default)]
    #[ts(optional)]
    pub label: Option<String>,
    /// 只给这些相对路径拍快照；缺省时按忽略规则遍历整个工作区。
    #[serde(default)]
    #[ts(optional)]
    pub paths: Option<Vec<String>>,
    /// 关联会话（Agent 自动检查点会带）。
    #[serde(default)]
    #[ts(optional)]
    pub session_id: Option<String>,
}

/// `checkpoint.create` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CheckpointCreateResult {
    pub checkpoint: CheckpointView,
}

/// `checkpoint.list` 请求参数。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CheckpointListParams {
    /// 只看某个工作区的检查点；缺省取当前活动工作区。
    #[serde(default)]
    #[ts(optional)]
    pub workspace_id: Option<String>,
    /// 返回条数上限，缺省 50，内部夹到 1..=200。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub limit: Option<u32>,
    /// 偏移量，缺省 0。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub offset: Option<u32>,
}

/// `checkpoint.list` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CheckpointListResult {
    /// 按创建时间倒序。
    pub checkpoints: Vec<CheckpointView>,
    /// 该工作区下的检查点总数（不受 `limit` / `offset` 影响）。
    #[ts(type = "number")]
    pub total: u64,
}

/// `checkpoint.restore` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CheckpointRestoreParams {
    pub checkpoint_id: String,
}

/// 单个文件的回滚结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointRestoreStatus {
    /// 用快照内容覆盖了现有文件。
    Restored,
    /// 文件是快照之后才出现的，已按快照状态删除。
    Removed,
    /// 快照之后被删掉的，已恢复。
    Recreated,
    /// 该文件回滚失败，`detail` 里是原因。
    Failed,
}

/// 回滚报告里的单个文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CheckpointRestoreFileView {
    pub path: String,
    pub status: CheckpointRestoreStatus,
    /// 回滚后回读算出的 SHA-256；`removed` / `failed` 为空。
    #[serde(default)]
    #[ts(optional)]
    pub hash: Option<String>,
    /// 失败原因（`status = failed` 时非空）。
    #[serde(default)]
    #[ts(optional)]
    pub detail: Option<String>,
}

/// `checkpoint.restore` 响应结果。
///
/// `verified` 是**回读校验**的结论：把文件写回后重新算一遍 SHA-256，与快照登记值
/// 逐个比对。docs/05 的「回滚后文件逐字节一致」靠这个布尔值兑现，而不是靠
/// 「写完没报错」。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CheckpointRestoreResult {
    pub checkpoint_id: String,
    pub verified: bool,
    pub files: Vec<CheckpointRestoreFileView>,
}

/// `checkpoint.delete` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CheckpointDeleteParams {
    pub checkpoint_id: String,
}

/// `checkpoint.delete` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CheckpointDeleteResult {
    pub checkpoint_id: String,
    /// 是否真的删掉了（不存在时返回 `false` 而不是报错，便于 UI 幂等清理）。
    pub removed: bool,
    /// 释放的字节数。
    #[ts(type = "number")]
    pub freed_bytes: u64,
}

/// 一条免问规则（`permission_rule` 表的一行）。
///
/// `id` 是**范围键**本身（`command:cargo` / `path:edit:src` / `tool:foo`），
/// 撤销时就按它删——UI 不需要另记一个内部主键。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PermissionRuleView {
    /// 范围键。
    pub id: String,
    /// 工具名。
    pub tool_pattern: String,
    /// 生效范围：工作区绝对路径，或 `global`。
    pub scope: String,
    /// 裁决结果，当前只有 `allow`。
    pub policy: String,
    /// 记下这条规则的时刻（Unix 毫秒）。
    #[ts(type = "number")]
    pub created_at_ms: i64,
}

/// `permission.list` 请求参数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PermissionListParams {
    /// 只看某个工作区（同时包含 `global` 规则）；缺省取当前活动工作区。
    #[serde(default)]
    #[ts(optional)]
    pub workspace_id: Option<String>,
    /// 返回条数上限，缺省 200，内部夹到 1..=200。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub limit: Option<u32>,
}

/// `permission.list` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PermissionListResult {
    /// 按记录时间倒序。
    pub rules: Vec<PermissionRuleView>,
    /// 该范围内的规则总数（不受 `limit` 影响）。
    #[ts(type = "number")]
    pub total: u64,
}

/// `permission.revoke` 请求参数。
///
/// 两种撤销方式二选一：给 `id` 撤销单条，给 `tool` 撤销该工具名下全部。
/// 两者都给时以 `id` 为准（更精确的意图优先），都不给则报参数错误。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PermissionRevokeParams {
    /// 要撤销的范围键。
    #[serde(default)]
    #[ts(optional)]
    pub id: Option<String>,
    /// 要整批撤销的工具名。
    #[serde(default)]
    #[ts(optional)]
    pub tool: Option<String>,
}

/// `permission.revoke` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PermissionRevokeResult {
    /// 实际删除的条数（按 `id` 撤销且不存在时为 0，不报错）。
    #[ts(type = "number")]
    pub removed: u64,
    /// 撤销后仍在生效的免问范围条数，便于 UI 直接回显。
    #[ts(type = "number")]
    pub remaining: u64,
}

/// 供 TS 侧引用的导出目录常量说明（不参与序列化，仅为文档服务）。
pub const _TS_EXPORT_DIR: &str = TS_DIR;

// ---------------------------------------------------------------------------
// terminal.*（M6 切片一：终端会话）
// ---------------------------------------------------------------------------

/// 一个终端会话的只读视图。
///
/// `cwd` 用字符串而非路径类型：客户端只做展示与「复制路径」，不需要内核替它
/// 校验路径合法性。`pid` 在平台拿不到时为 `null`（例如某些沙箱里）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalView {
    /// 会话 id（UUID v4）。
    pub id: String,
    /// 实际启动的可执行文件路径。
    pub shell: String,
    /// 起始工作目录；缺省表示继承内核进程的当前目录。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cwd: Option<String>,
    /// 子进程 pid。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pid: Option<u32>,
    /// 当前列数（`terminal.resize` 会更新这里）。
    pub cols: u16,
    /// 当前行数。
    pub rows: u16,
    /// 建会话时刻（Unix 毫秒）。
    #[ts(type = "number")]
    pub created_at_ms: i64,
}

/// `terminal.create` 请求参数。
///
/// 全部可选：`{}` 就是「按平台缺省 shell、缺省尺寸、继承内核 cwd」开一个终端。
/// 尺寸会在内核侧夹到合法区间，客户端量到 0 不会建出 1×1 的废终端。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalCreateParams {
    /// shell 可执行文件；缺省取 `%COMSPEC%`（Windows）/ `$SHELL`（Unix）。
    #[serde(default)]
    #[ts(optional)]
    pub shell: Option<String>,
    /// 传给 shell 的参数。
    #[serde(default)]
    pub args: Vec<String>,
    /// 起始工作目录。
    #[serde(default)]
    #[ts(optional)]
    pub cwd: Option<String>,
    /// 追加 / 覆盖的环境变量（在继承来的环境之上）。
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// 初始列数，缺省 80。
    #[serde(default)]
    #[ts(optional)]
    pub cols: Option<u16>,
    /// 初始行数，缺省 24。
    #[serde(default)]
    #[ts(optional)]
    pub rows: Option<u16>,
}

/// `terminal.create` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalCreateResult {
    /// 新建的会话。
    pub terminal: TerminalView,
}

/// `terminal.write` 请求参数。
///
/// `data` 是**原始字节的 base64**：终端里要发的不只是正文，还有 `\r`、
/// `Ctrl-C`（`0x03`）、方向键（`ESC [ A`）这类任意字节。用字符串传会让
/// 控制字符在 JSON 转义里丢一层。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalWriteParams {
    /// 目标会话 id。
    pub terminal_id: String,
    /// 原始字节的 base64（标准字母表，带 `=` 填充）。
    pub data: String,
}

/// `terminal.write` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalWriteResult {
    /// 实际写入的字节数（base64 解码之后）。
    #[ts(type = "number")]
    pub bytes: u64,
}

/// `terminal.resize` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalResizeParams {
    /// 目标会话 id。
    pub terminal_id: String,
    /// 新列数（内核夹到 2..=500）。
    pub cols: u16,
    /// 新行数（内核夹到 1..=200）。
    pub rows: u16,
}

/// `terminal.resize` 响应结果：回夹取后的真实尺寸，客户端据此校正本地视图。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalResizeResult {
    /// 调整后的会话视图。
    pub terminal: TerminalView,
}

/// `terminal.kill` 请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalKillParams {
    /// 目标会话 id。
    pub terminal_id: String,
}

/// `terminal.list` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalListResult {
    /// 存活的会话，按创建时间升序（与前端标签页顺序一致）。
    pub terminals: Vec<TerminalView>,
    /// 会话总数（就是 `terminals.len()`，回给 UI 免得它自己数）。
    #[ts(type = "number")]
    pub total: u64,
}

/// `terminal.data` 事件：一段输出。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalDataEvent {
    /// 会话 id。
    pub terminal_id: String,
    /// 原始字节的 base64。
    pub data: String,
    /// 解码后的字节数，便于客户端做背压统计而不必先解码。
    #[ts(type = "number")]
    pub bytes: u64,
}

/// `terminal.exit` 事件：子进程退出。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalExitEvent {
    /// 会话 id。
    pub terminal_id: String,
    /// 退出码；拿不到时为 `null`。
    pub code: Option<i32>,
    /// 终止信号名（Unix）；Windows 恒为 `null`。
    pub signal: Option<String>,
    /// 退出时刻（Unix 毫秒）。
    #[ts(type = "number")]
    pub at_ms: i64,
}

// ---------------------------------------------------------------------------
// git.*（M6 切片三：工作区 Git 能力）
// ---------------------------------------------------------------------------

/// `git.status` 请求参数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStatusParams {
    /// 工作区 id；缺省作用于活动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
}

/// 工作区里的一个变更条目。
///
/// `index` / `worktree` 是 git 的两列状态字母，未变更的一侧为 `"."`；直接透出字母
/// 而不翻译成布尔量，是因为 UI 要区分「已暂存的新增（`A`）」与「未暂存的新增（`?`）」，
/// 这两者合并成一个 `is_new` 就再也分不开了。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStatusEntry {
    /// 相对工作区根的文件路径（正斜杠）。
    pub path: String,
    /// 重命名 / 复制前的原路径；其它情况为 `null`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub original_path: Option<String>,
    /// 索引区（暂存区）状态：`M` / `A` / `D` / `R` / `C` / `T` / `U`，无变更为 `.`。
    pub index: String,
    /// 工作区状态，无变更为 `.`。
    pub worktree: String,
    /// 条目类别：`changed` / `renamed` / `unmerged` / `untracked` / `ignored`。
    pub kind: String,
}

/// `git.status` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStatusResult {
    /// 工作区 id。
    pub workspace_id: String,
    /// 当前分支名；分离头指针时为 `null`。
    pub branch: Option<String>,
    /// 是否处于分离头指针状态。
    pub detached: bool,
    /// HEAD 指向的提交；仓库还没有任何提交时为 `null`。
    pub head_oid: Option<String>,
    /// 上游分支（`origin/main` 这类）；未设置时为 `null`。
    pub upstream: Option<String>,
    /// 相对上游领先的提交数；无上游时为 0。
    #[ts(type = "number")]
    pub ahead: u64,
    /// 相对上游落后的提交数；无上游时为 0。
    #[ts(type = "number")]
    pub behind: u64,
    /// 工作区是否干净（`entries` 为空）。
    pub clean: bool,
    /// 变更清单，按 git 输出顺序（路径升序）。
    pub entries: Vec<GitStatusEntry>,
}

/// `git.diff` 请求参数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitDiffParams {
    /// 工作区 id；缺省作用于活动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
    /// 只取该路径的 diff（相对工作区根）；缺省为全量 diff。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    /// 是否看暂存区（`git diff --cached`）。`false` 为「工作区相对暂存区」。
    #[serde(default)]
    pub staged: bool,
    /// 上下文行数；缺省 3（与 git 缺省一致），上限 100。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number")]
    pub context_lines: Option<u32>,
}

/// `git.diff` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitDiffResult {
    /// 请求里的路径；全量 diff 时为 `null`。
    pub path: Option<String>,
    /// 是否是暂存区 diff（回显请求参数）。
    pub staged: bool,
    /// unified diff 文本（未着色，`\n` 换行）。无差异时为空串。
    pub diff: String,
    /// diff 文本的字节数（UTF-8 编码后）。
    #[ts(type = "number")]
    pub bytes: u64,
    /// 是否因超过 1 MiB 上限被截断。截断只切尾部，头部始终完整。
    pub truncated: bool,
}

/// `git.stage` 请求参数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStageParams {
    /// 工作区 id；缺省作用于活动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
    /// 要暂存 / 取消暂存的路径；与 `all` 二选一（都给了按 `all` 处理）。
    #[serde(default)]
    pub paths: Vec<String>,
    /// 暂存全部变更（含删除），等价 `git add -A`。
    #[serde(default)]
    pub all: bool,
    /// `true` 为取消暂存（`git reset`），`false` 为暂存（`git add`）。
    #[serde(default)]
    pub unstage: bool,
}

/// `git.stage` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStageResult {
    /// 实际操作的路径（回显，已归一化为正斜杠）；全量操作为空表。
    pub paths: Vec<String>,
    /// 是否为全量操作。
    pub all: bool,
    /// 是否为取消暂存。
    pub unstage: bool,
    /// 操作后仍在暂存区的条目数，便于 UI 立刻刷新角标而不必再发一次 `git.status`。
    #[ts(type = "number")]
    pub staged_total: u64,
}

/// 一块可寻址的 hunk。
///
/// 定位靠**四个行号 + `index`**，不靠 hunk 序号单独说话：文件在用户点之前又被改过时，
/// 「第 0 块」可能已经漂到别的位置，只按序号暂存会把没选的内容塞进索引。
/// `header` 是 `@@` 之后那截上下文（git 写在 `@@` 后的函数名 / 类名），便于 UI 展示。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitHunk {
    /// 文件路径（相对工作区根，正斜杠）。
    pub path: String,
    /// 该 hunk 在文件内的序号（0 起，按 diff 输出顺序）。
    #[ts(type = "number")]
    pub index: u32,
    /// 旧侧起始行（1 起；纯新增的块为 0）。
    #[ts(type = "number")]
    pub old_start: u32,
    /// 旧侧行数。
    #[ts(type = "number")]
    pub old_lines: u32,
    /// 新侧起始行（1 起）。
    #[ts(type = "number")]
    pub new_start: u32,
    /// 新侧行数。
    #[ts(type = "number")]
    pub new_lines: u32,
    /// `@@` 之后的上下文（原样，含前导空格；无上下文时为空串）。
    pub header: String,
    /// 正文行，每行带 git 的前缀字符（` ` / `+` / `-`）。
    ///
    /// 末行可能是一条 `\ No newline at end of file`——它不是内容，而是「上一行没有
    /// 换行符结尾」的标记，外壳展示时应单独渲染，不要当成文本内容。
    pub lines: Vec<String>,
}

/// 单个文件的 hunk 清单。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitFileHunks {
    /// 文件路径（相对工作区根，正斜杠）。
    pub path: String,
    /// 是否是二进制文件。二进制没有 hunk，`git.stage_hunk` 会以 7012 明确拒绝。
    pub binary: bool,
    /// hunk 清单；无改动时为空表。
    pub hunks: Vec<GitHunk>,
}

/// `git.stage_hunk` 请求参数。
///
/// `old_start` / `old_lines` / `new_start` / `new_lines` 四项与 `index` 一起构成定位：
/// 只要有一项与当前 diff 不符，就报 7011 让外壳刷新，**绝不**按序号猜着暂存。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStageHunkParams {
    /// 工作区 id；缺省作用于活动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
    /// 文件路径（相对工作区根）。
    pub path: String,
    /// hunk 序号（0 起）。
    #[ts(type = "number")]
    pub index: u32,
    /// 旧侧起始行。
    #[ts(type = "number")]
    pub old_start: u32,
    /// 旧侧行数。
    #[ts(type = "number")]
    pub old_lines: u32,
    /// 新侧起始行。
    #[ts(type = "number")]
    pub new_start: u32,
    /// 新侧行数。
    #[ts(type = "number")]
    pub new_lines: u32,
}

/// `git.stage_hunk` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStageHunkResult {
    /// 实际暂存的那块（回显解析出的完整结构，便于外壳直接标成已暂存）。
    pub hunk: GitHunk,
    /// 操作后仍在暂存区的条目数。
    #[ts(type = "number")]
    pub staged_total: u64,
}

/// `git.unstage_hunk` 请求参数。定位字段语义同 [`GitStageHunkParams`]。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitUnstageHunkParams {
    /// 工作区 id；缺省作用于活动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
    /// 文件路径（相对工作区根）。
    pub path: String,
    /// hunk 序号（0 起）。
    #[ts(type = "number")]
    pub index: u32,
    /// 旧侧起始行。
    #[ts(type = "number")]
    pub old_start: u32,
    /// 旧侧行数。
    #[ts(type = "number")]
    pub old_lines: u32,
    /// 新侧起始行。
    #[ts(type = "number")]
    pub new_start: u32,
    /// 新侧行数。
    #[ts(type = "number")]
    pub new_lines: u32,
}

/// `git.unstage_hunk` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitUnstageHunkResult {
    /// 实际取消暂存的那块。
    pub hunk: GitHunk,
    /// 操作后仍在暂存区的条目数。
    #[ts(type = "number")]
    pub staged_total: u64,
}

/// 文件里的一个冲突块（一对 `<<<<<<<` … `>>>>>>>`）。
///
/// 行号是 1 起的闭区间、**含标记行**：外壳据此把冲突区高亮出来，所以 `end_line` 是
/// `>>>>>>>` 那一行，而不是最后一个内容行。`ours` / `base` / `theirs` 只放内容行，
/// 标记行本身不在里面（否则外壳想把块替换成一看就得自己认出哪几行是标记）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitConflictBlock {
    /// 块序号（0 起，按文件内出现顺序）。
    #[ts(type = "number")]
    pub index: u32,
    /// `<<<<<<<` 所在行号（1 起）。
    #[ts(type = "number")]
    pub start_line: u32,
    /// `>>>>>>>` 所在行号（1 起）。
    #[ts(type = "number")]
    pub end_line: u32,
    /// `<<<<<<<` 后的 ours 标签（合并时是分支名）。
    pub ours_label: String,
    /// `|||||||` 后的 base 标签；只有 `merge.conflictStyle=diff3` 才有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base_label: Option<String>,
    /// `>>>>>>>` 后的 theirs 标签。
    pub theirs_label: String,
    /// ours 段内容行（保留空行，不含标记行）。
    pub ours: Vec<String>,
    /// base 段内容行；非 `diff3` 风格为 `null`——空数组表示「有这一段但没有内容」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base: Option<Vec<String>>,
    /// theirs 段内容行。
    pub theirs: Vec<String>,
}

/// `git.conflict_read` 请求参数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitConflictReadParams {
    /// 工作区 id；缺省作用于活动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
    /// 文件路径（相对工作区根）。
    pub path: String,
}

/// `git.conflict_read` 响应结果。
///
/// `blocks` 为空不代表「没有冲突」：二进制冲突、一侧缺失（`DU` / `UD`）、两侧都删
/// 都存在「有冲突但没有标记可解析」的情况。外壳应据 `kind` 决定是渲染逐块按钮，
/// 还是只给「整份取一侧 / 删除」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitConflictReadResult {
    /// 文件路径（相对工作区根）。
    pub path: String,
    /// 冲突类型：`both_modified` / `both_added` / `both_deleted` / `deleted_by_us` /
    /// `deleted_by_them` / `added_by_us` / `added_by_them` / `other`。
    pub kind: String,
    /// `git status` 的两位 XY 原文（`UU` / `AA` / `DU` …），`kind` 为 `other` 时唯一依据。
    pub xy: String,
    /// 工作区里是否真的有这个文件。
    pub present: bool,
    /// 冲突块清单。
    pub blocks: Vec<GitConflictBlock>,
}

/// `git.conflict_resolve` 请求参数。
///
/// 三种解决方式，优先级：`content` > `side`。
///
/// - `side = "ours"` / `"theirs"`：逐块取其一段；没有标记可切时整份取那一侧
///   （那一侧本来没有文件，则删除该文件，而不是留一个空文件）；
/// - `side = "both"`：逐块把两侧都留下，`theirs_first` 决定拼接顺序；
/// - `content`：调用方手里的整份全文，原样覆盖写盘。
///
/// `expect_*` 是**乐观锁**：与现状不符即 7014，绝不按过期布局硬切。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitConflictResolveParams {
    /// 工作区 id；缺省作用于活动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
    /// 文件路径（相对工作区根）。
    pub path: String,
    /// 取哪一侧：`ours` / `theirs` / `both`；写了 `content` 时忽略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub side: Option<String>,
    /// `side = "both"` 时 theirs 是否排在前面；缺省 `false`（ours 在前）。
    #[serde(default)]
    pub theirs_first: bool,
    /// 自定义全文：`Some` 时整份覆盖写盘。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub content: Option<String>,
    /// 调用方看到的冲突块数量；不一致即 7014。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number")]
    pub expect_blocks: Option<u32>,
    /// 调用方看到的首个冲突块起始行；不一致即 7014。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number")]
    pub expect_start_line: Option<u32>,
}

/// `git.conflict_resolve` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitConflictResolveResult {
    /// 被解决的文件。
    pub path: String,
    /// 冲突类型。
    pub kind: String,
    /// 文件是否已离开未合并状态（即确实 `git add` 成功）。
    pub resolved: bool,
    /// 写盘后是否仍带冲突标记；`true` 时**没有** `git add`，索引保持未合并。
    pub still_conflicted: bool,
    /// 本次替换掉的冲突块数量。
    #[ts(type = "number")]
    pub resolved_blocks: u32,
    /// 解决后仓库里剩下的冲突文件（已排序），外壳可直接重新渲染冲突面板。
    pub remaining: Vec<String>,
}

/// `git.commit` 请求参数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitCommitParams {
    /// 工作区 id；缺省作用于活动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
    /// 提交信息（首行为摘要）。
    pub message: String,
    /// 只提交这些路径；缺省提交整个暂存区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub paths: Option<Vec<String>>,
    /// 追加到上一次提交（`--amend`）。
    #[serde(default)]
    pub amend: bool,
    /// 允许空提交（`--allow-empty`）。
    #[serde(default)]
    pub allow_empty: bool,
}

/// `git.commit` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitCommitResult {
    /// 新提交的完整 SHA-1。
    pub oid: String,
    /// 短哈希（至少 7 位）。
    pub short_oid: String,
    /// 提交信息首行。
    pub summary: String,
    /// 提交后的分支名；分离头指针时为 `null`。
    pub branch: Option<String>,
    /// 本次提交涉及的文件数。
    #[ts(type = "number")]
    pub files: u64,
    /// 作者（`Name <email>`）。仓库未配置身份时由内核填入临时身份，见协议文档。
    pub author: String,
}

/// `git.branches` 请求参数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitBranchesParams {
    /// 工作区 id；缺省作用于活动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
}

/// 一个分支（本地或远端）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitBranch {
    /// 分支名；远端的形如 `origin/main`。
    pub name: String,
    /// 分支指向的完整提交哈希。
    pub oid: String,
    /// 短哈希。
    pub short_oid: String,
    /// 是否为当前分支。
    pub is_current: bool,
    /// 是否为远端跟踪分支。
    pub is_remote: bool,
    /// 上游分支名；未设置为 `null`。
    pub upstream: Option<String>,
}

/// `git.branches` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitBranchesResult {
    /// 工作区 id。
    pub workspace_id: String,
    /// 当前分支名；分离头指针时为 `null`。
    pub current: Option<String>,
    /// 是否处于分离头指针状态。
    pub detached: bool,
    /// 本地分支在前、远端分支在后，各自按名字升序。
    pub branches: Vec<GitBranch>,
}

// ---------------------------------------------------------------------------
// Git 深化（缺口第二批：分支切换 / 远端 / stash）
// ---------------------------------------------------------------------------
//
// 这一组 DTO 与上面 `git.*` 五点同域，但**每一个都带着「可能丢数据或触网」的语义**，
// 所以字段设计上刻意多写一件事：把「接下来会发生什么」提前放进请求、把「实际发生了
// 什么」完整放进响应。外壳据此决定要不要二次确认，而不是靠猜。

/// `git.checkout` 请求参数。
///
/// 脏工作区时内核**拒绝切换**并报 `7006`：`git checkout` 在两边都有改动时会直接失败，
/// 但在「改动可被静默带过去」的场景下它会真的把改动搬走——那种「成功」最危险，
/// 因为用户看不见分支边界上发生了什么。这里选择显式拒绝，把决定权交回用户
/// （先提交、先 `git.stash`，或明确要求带改动切换）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitCheckoutParams {
    /// 工作区 id；缺省作用于活动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
    /// 目标分支名。`create` 为真时是**新分支名**，为假时必须是已存在的本地或远端分支。
    pub branch: String,
    /// 是否新建分支（等价 `git checkout -b <branch> <start_point>`）。
    #[serde(default)]
    pub create: bool,
    /// 新建分支的起点（分支名 / 提交哈希）；仅 `create` 为真时生效，缺省为当前 HEAD。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub start_point: Option<String>,
}

/// `git.checkout` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitCheckoutResult {
    /// 工作区 id。
    pub workspace_id: String,
    /// 切换后的分支名（新建时为新分支名）。
    pub branch: String,
    /// 切换前的分支名；原先是分离头指针时为 `null`。
    pub previous: Option<String>,
    /// 是否为本次新建的分支。
    pub created: bool,
    /// 切换后 HEAD 指向的完整哈希。
    pub oid: String,
    /// 短哈希。
    pub short_oid: String,
}

/// `git.fetch` 请求参数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitFetchParams {
    /// 工作区 id；缺省作用于活动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
    /// 远端名；缺省用当前分支的上游远端，再缺省用 `origin`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub remote: Option<String>,
    /// 是否顺带清理远端已删除的跟踪分支（`--prune`）。
    #[serde(default)]
    pub prune: bool,
}

/// 一次远端引用更新。
///
/// `old_short` 为空表示「新建引用」；`forced` 为真表示远端历史被改写（强推过）。
/// 这三种情形在 UI 上语气完全不同，所以内核拆开报，不让外壳从字符串里猜。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitRefUpdate {
    /// 引用名，如 `origin/main` 或 `refs/tags/v1`。
    pub ref_name: String,
    /// 更新前的短哈希；新建引用时为 `null`。
    pub old_short: Option<String>,
    /// 更新后的短哈希。
    pub new_short: String,
    /// 是否为强制更新（远端历史被改写）。
    pub forced: bool,
}

/// `git.fetch` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitFetchResult {
    /// 工作区 id。
    pub workspace_id: String,
    /// 实际使用的远端名。
    pub remote: String,
    /// 本次被更新的引用；没有更新时为空表（`up_to_date` 为真）。
    pub updated: Vec<GitRefUpdate>,
    /// 是否没有任何引用变化。
    pub up_to_date: bool,
}

/// `git.pull` 请求参数。
///
/// 只做**快进**（`--ff-only`）。内核不提供 `--rebase` / 合并式 pull：合并会在工作区
/// 制造一个用户没要求的 merge commit，冲突则把仓库留在半途状态——这两件事都该在
/// 用户知道自己在做什么时做，而不是被一个「拉一下」按钮顺手做掉。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitPullParams {
    /// 工作区 id；缺省作用于活动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
    /// 远端名；缺省用当前分支的上游远端。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub remote: Option<String>,
    /// 远端分支名；缺省用当前分支的上游分支名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub branch: Option<String>,
}

/// `git.pull` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitPullResult {
    /// 工作区 id。
    pub workspace_id: String,
    /// 当前分支名。
    pub branch: String,
    /// 上游分支名（`origin/main` 形式）。
    pub upstream: String,
    /// 拉取前的 HEAD 哈希。
    pub previous_oid: String,
    /// 拉取后的 HEAD 哈希。
    pub oid: String,
    /// 短哈希。
    pub short_oid: String,
    /// 是否真的前进了（已是最新时为 `false`）。
    pub updated: bool,
    /// 本次带入的提交数；未更新时为 0。
    #[ts(type = "number")]
    pub commits: u64,
    /// 本次变更的文件数；未更新时为 0。
    #[ts(type = "number")]
    pub files: u64,
    /// 变更文件清单（最多 200 条，按路径升序），供外壳展示「拉进来改了什么」。
    pub changed_paths: Vec<String>,
}

/// `git.push` 请求参数。
///
/// **没有** `force` 字段：强制推送只允许 `force_with_lease`，即「我确认远端仍停在
/// 我看到的位置」。裸 `--force` 会覆盖别人刚推上去的提交，且无法撤销——这种能力
/// 不该被一个布尔字段顺手打开。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitPushParams {
    /// 工作区 id；缺省作用于活动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
    /// 远端名；缺省用当前分支的上游远端，再缺省用 `origin`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub remote: Option<String>,
    /// 远端分支名；缺省与本地分支同名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub branch: Option<String>,
    /// 是否顺手把上游设为本次推送的远端分支（`-u`）。
    #[serde(default)]
    pub set_upstream: bool,
    /// 是否用 `--force-with-lease` 强推。外壳必须先二次确认。
    #[serde(default)]
    pub force_with_lease: bool,
}

/// `git.push` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitPushResult {
    /// 工作区 id。
    pub workspace_id: String,
    /// 推送的本地分支名。
    pub branch: String,
    /// 远端名。
    pub remote: String,
    /// 远端分支名。
    pub remote_branch: String,
    /// 是否使用了 `--force-with-lease`。
    pub forced: bool,
    /// 是否本来就同步（`Everything up-to-date`）。
    pub up_to_date: bool,
    /// 推送后本地 HEAD 的完整哈希。
    pub oid: String,
    /// 短哈希。
    pub short_oid: String,
}

/// 一条 stash（`stash@{index}`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStashEntry {
    /// 序号，对应 `stash@{n}` 里的 `n`；`0` 是最新一条。
    pub index: u32,
    /// 完整哈希。
    pub oid: String,
    /// 短哈希。
    pub short_oid: String,
    /// stash 信息（`git stash save -m` 写入的 message，或默认的 `WIP on <branch>`）。
    pub message: String,
}

/// `git.stash` 请求参数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStashParams {
    /// 工作区 id；缺省作用于活动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
    /// 说明文字；缺省由 git 生成 `WIP on <branch>: <short> <subject>`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message: Option<String>,
    /// 是否连未跟踪文件一起收进 stash（`--include-untracked`）。
    #[serde(default)]
    pub include_untracked: bool,
}

/// `git.stash` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStashResult {
    /// 工作区 id。
    pub workspace_id: String,
    /// 是否真的产生了一条 stash（工作区本来就干净时为 `false`）。
    pub stashed: bool,
    /// 新产生的 stash；`stashed` 为假时为 `null`。
    pub entry: Option<GitStashEntry>,
    /// 操作后 stash 总数。
    pub total: u32,
}

/// `git.stash_list` 请求参数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStashListParams {
    /// 工作区 id；缺省作用于活动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
}

/// `git.stash_list` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStashListResult {
    /// 工作区 id。
    pub workspace_id: String,
    /// 按序号升序（`0` 最新）。
    pub entries: Vec<GitStashEntry>,
}

/// `git.stash_pop` / `git.stash_apply` 请求参数。
///
/// 两个方法共用一套参数与结果：差别只在「成功之后删不删这条 stash」。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStashApplyParams {
    /// 工作区 id；缺省作用于活动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
    /// 目标 stash 序号（`stash@{index}`）；缺省 `0`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub index: Option<u32>,
}

/// `git.stash_pop` / `git.stash_apply` 响应结果。
///
/// 冲突**不走**这条结果：冲突一律是 `7010`（`data.files` 带冲突文件、`data.stash_kept`
/// 恒真）。这样「取出成功」与「取出但撞冲突」在外壳上是两套不同的话术，不会混。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStashApplyResult {
    /// 工作区 id。
    pub workspace_id: String,
    /// 动作名：`pop` 或 `apply`。
    pub action: String,
    /// 被取出的 stash 序号。
    pub index: u32,
    /// 该 stash 是否已被删除（`pop` 成功为真，`apply` 恒为假）。
    pub dropped: bool,
    /// 操作后剩余的 stash 数。
    pub total: u32,
}

/// `git.stash_drop` 请求参数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStashDropParams {
    /// 工作区 id；缺省作用于活动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<String>,
    /// 目标 stash 序号；缺省 `0`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub index: Option<u32>,
}

/// `git.stash_drop` 响应结果。
///
/// 回带被删条目的 `oid` / `message`：这是用户**唯一**能事后确认「到底删了哪一条」的
/// 证据（已删的 stash 不在 `stash_list` 里，界面上会直接消失）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStashDropResult {
    /// 工作区 id。
    pub workspace_id: String,
    /// 被删除的 stash 序号。
    pub index: u32,
    /// 被删除条目的完整哈希。
    pub oid: String,
    /// 被删除条目的说明。
    pub message: String,
    /// 操作后剩余的 stash 数。
    pub total: u32,
}

// ---------------------------------------------------------------------------
// MCP（M7 切片二：`mcp.*`）
// ---------------------------------------------------------------------------

/// 一个 MCP Server 的运行期视图。
///
/// `configured` 与 `connected` 是两件事：`mcp.json` 里写着（可连），不等于连上了
/// （可用）。外壳据此区分「点一下连接」和「点一下断开」。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpServerInfo {
    /// Server 名（`mcp.json` 里的 key）。
    pub name: String,
    /// 传输类型：`stdio` / `http` / `sse`。
    pub transport: String,
    /// 是否在配置里（`false` 只可能出现在「配置被改掉但连接还在」的短暂状态）。
    pub configured: bool,
    /// 是否已连接且可用。
    pub connected: bool,
    /// 连接状态原样透传：`configured` / `connecting` / `ready` / `stale` / `closed`。
    pub state: String,
    /// 已发现的工具数量。
    pub tool_count: u32,
    /// 最近一次失败原因（成功则为 `null`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_error: Option<String>,
}

/// `mcp.servers` 参数：空对象即可，保留占位以便后续加过滤条件。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpServersParams {}

/// `mcp.servers` 响应结果。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpServersResult {
    /// 已配置 ∪ 已连接的 Server，按名字升序。
    pub servers: Vec<McpServerInfo>,
}

/// `mcp.connect` 参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpConnectParams {
    /// 要连接的 Server 名（必须是 `mcp.json` 里的 key）。
    pub server: String,
}

/// 一个可注入 agent 的 MCP 工具。
///
/// `input_schema` 是 Server 的 `inputSchema` **原样**透传：它在两侧都是 JSON Schema，
/// 任何「规整」都会让模型的入参校验与 Server 的校验错位。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpToolInfo {
    /// 工具在 Server 内的原始名。
    pub name: String,
    /// 注入 agent 注册表时用的全名：`mcp__<server>__<tool>`。
    pub namespaced_name: String,
    /// 所属 Server。
    pub server: String,
    /// 给模型看的说明（来自 Server，可能为空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    /// 入参 JSON Schema，原样透传。
    pub input_schema: serde_json::Value,
}

/// `mcp.connect` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpConnectResult {
    /// 连上的 Server。
    pub server: String,
    /// 实际使用的传输类型。
    pub transport: String,
    /// 握手协商出的协议版本（Server 未回报时为 `null`）。
    pub protocol_version: Option<String>,
    /// 发现到的工具数量。
    pub tool_count: u32,
    /// 发现到的工具清单（已带命名空间）。
    pub tools: Vec<McpToolInfo>,
}

/// `mcp.disconnect` 参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpDisconnectParams {
    /// 要断开的 Server 名。
    pub server: String,
}

/// `mcp.disconnect` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpDisconnectResult {
    /// 被断开（或本就未连接）的 Server。
    pub server: String,
    /// 本次调用是否真的关掉了一条连接。
    ///
    /// `false` 表示「本来就是断开状态」——幂等，不当错误。
    pub closed: bool,
    /// 断开后从注册表下架的工具数量。
    pub removed_tools: u32,
}

/// `mcp.tools` 参数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpToolsParams {
    /// 只看某个 Server；缺省返回全部已连接 Server 的工具。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub server: Option<String>,
}

/// `mcp.tools` 响应结果。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpToolsResult {
    /// 工具清单，按 Server 名、工具名升序。
    pub tools: Vec<McpToolInfo>,
}

/// `mcp.call` 参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpCallParams {
    /// 目标 Server 名；`tool` 若已是 `mcp__<server>__<tool>` 全名则可省略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub server: Option<String>,
    /// 工具名：原始名或带命名空间的全名都接受。
    pub tool: String,
    /// 入参对象；缺省为空对象。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub arguments: Option<serde_json::Value>,
}

/// MCP 工具返回的一块内容。
///
/// 不照搬 MCP 的带标签联合体：内核协议用「`kind` + 按需字段」的平铺形态，
/// 外壳与 TS 侧都不必为了一个图片块去写判别式。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpContentBlock {
    /// 块类型：`text` / `image` / `audio` / `resource` / `resource_link`。
    pub kind: String,
    /// 文本内容（`kind = text` 时）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub text: Option<String>,
    /// base64 数据（图片 / 音频）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub data: Option<String>,
    /// MIME 类型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mime_type: Option<String>,
    /// 资源 URI（资源链接 / 内嵌资源）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub uri: Option<String>,
    /// 展示名（资源链接）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    /// 内嵌资源原文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resource: Option<serde_json::Value>,
}

/// `mcp.call` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpCallResult {
    /// 实际执行调用的 Server。
    pub server: String,
    /// 工具原始名。
    pub tool: String,
    /// 带命名空间的工具全名。
    pub namespaced_name: String,
    /// 内容块，原样透传（文本 / 图片 / 资源）。
    pub content: Vec<McpContentBlock>,
    /// 文本块拼成的正文：与注入 agent 时回灌给模型的内容一致。
    pub text: String,
    /// 工具**执行**失败（不是协议失败）。这种情况仍是成功响应。
    pub is_error: bool,
    /// 结构化输出（Server 支持时）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub structured_content: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// 插件宿主（P0 协议骨架）
//
// P0 只定协议，不装载进程：这些类型是空分支将来要填的**形状**，现在就被 ts-rs
// 导出成 TS，外壳可以先按它写面板骨架，不必等内核实现完再返工。
//
// 字段一律 snake_case（与 terminal.* / mcp.* 一致），由 serde 默认命名负责，
// 不额外加 `rename_all`——混用两种风格是前端最容易踩的坑。
// ---------------------------------------------------------------------------

/// 插件在宿主侧的状态（docs/14 §5 状态机）。
///
/// `needs_grant` 单独成态而不是"未加载"：它意味着清单合法、只差用户点授权，
/// UI 要给出「授权」按钮而不是「加载」按钮——两者的下一步完全不同。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum PluginState {
    /// 已安装但未加载（用户没点，或 `autoload=false`）。
    #[default]
    Disabled,
    /// 清单申请的能力中有未授予项。
    NeedsGrant,
    /// 正在起进程 / 握手。
    Loading,
    /// 已握手，工具可调用。
    Ready,
    /// 进程崩了，工具已下架。
    Crashed,
}

/// 插件工具的审批档位。与 Agent 的 [`ApprovalRisk`] 分开定义，是因为插件工具
/// 的风险由**清单声明**（插件自己能填），而 `ApprovalRisk` 由内核判定。
/// 两者在注入 agent 时映射：`read → read`、`write` / `exec → write`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum PluginToolRisk {
    /// 只读，免审批。
    #[default]
    Read,
    /// 写盘，走审批。
    Write,
    /// 起进程 / 执行命令，走审批且默认拒绝。
    Exec,
}

/// 插件摘要视图。
///
/// 不回传清单原文：清单里有 `entry`（可执行文件路径）与 `permissions` 细节，
/// 渲染端不需要，回传只会把「结构契约」变成「谁都能读的内部表示」。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginSummary {
    /// 插件 id（清单里的 `id`，全局唯一，也是 `plugin.*` 的寻址键）。
    pub id: String,
    /// 展示名。
    pub name: String,
    /// 语义化版本。
    pub version: String,
    /// 一句话说明（清单可缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    /// 当前状态。
    pub state: PluginState,
    /// 清单**申请**的能力（如 `fs.read`、`net.fetch`）。
    pub capabilities: Vec<String>,
    /// 用户**已授予**的能力；`needs_grant` 时是本集合的子集关系。
    pub granted_capabilities: Vec<String>,
    /// 已注册工具数（未加载时为 0）。
    pub tool_count: u32,
    /// 崩溃重启次数，供面板提示「这个插件不稳定」。
    ///
    /// 只记**崩溃后重新装载**的次数：用户手动 `unload` / `reload` 不加。停在 0 表示
    /// 装好之后一次都没崩过，不是「没重启过」。
    pub restarts: u32,
    /// 最近一次失败原因，供面板直接展示，不必让用户翻日志。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_error: Option<String>,
}

/// 一个插件工具的对外描述。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginToolSummary {
    /// 插件内原始工具名（插件自己认识的那个名字）。
    pub name: String,
    /// 带命名空间的工具全名：`plugin__<plugin_id>__<name>`。
    ///
    /// 命名空间是硬约束：插件工具与核心工具、MCP 工具共处一个注册表，
    /// 不加前缀就等着撞名（[`crate::ErrorCode::PLUGIN_TOOL_CONFLICT`] 就是为此留的）。
    pub namespaced_name: String,
    /// 给模型看的说明。
    pub description: String,
    /// 审批档位（见 [`PluginToolRisk`]）。
    pub risk: PluginToolRisk,
    /// JSON Schema 形式的入参定义，原样透传给模型。
    pub input_schema: serde_json::Value,
}

/// 一个插件命令的对外描述（P5 进命令面板，P0 仅占位）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginCommandSummary {
    /// 插件内原始命令名。
    pub name: String,
    /// 命令面板标题。
    pub title: String,
    /// 一句话说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    /// 审批档位：`write` 的命令在执行**前**弹一次审批（3001），
    /// `read` 只走状态 / 归属 / 路径 / 配额四关。
    #[serde(default)]
    pub risk: PluginToolRisk,
    /// 路径闸门用的入参字段名（如 `path`）。缺省表示该命令不针对具体文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path_field: Option<String>,
    /// 入参 JSON Schema（清单里的 `args`）。缺省表示该命令不吃入参。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub args: Option<serde_json::Value>,
}

/// `plugin.list` 参数：无（保留空对象，便于将来加过滤条件而不破坏兼容）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginListParams {}

/// `plugin.list` 响应结果。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginListResult {
    /// 已安装插件，按 id 升序排列，保证面板顺序稳定。
    pub plugins: Vec<PluginSummary>,
}

/// `plugin.status` 参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginStatusParams {
    /// 插件 id。
    pub plugin_id: String,
}

/// `plugin.status` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginStatusResult {
    /// 插件摘要。
    pub plugin: PluginSummary,
    /// 已注册工具（未加载时为空数组）。
    pub tools: Vec<PluginToolSummary>,
    /// 声明的命令（未加载时为空数组）。
    pub commands: Vec<PluginCommandSummary>,
}

/// `plugin.install` 参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginInstallParams {
    /// 插件包路径（目录或 `.zip`，内核自己判类型）。
    pub path: String,
}

/// `plugin.install` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginInstallResult {
    /// 装好的插件摘要（状态必为 `disabled` 或 `needs_grant`，安装不启动进程）。
    pub plugin: PluginSummary,
    /// 安装时覆盖了同 id 的旧版本。
    pub replaced: bool,
}

/// `plugin.uninstall` 参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginUninstallParams {
    /// 插件 id。
    pub plugin_id: String,
}

/// `plugin.uninstall` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginUninstallResult {
    /// 被卸载的插件 id。
    pub plugin_id: String,
    /// 连带下架的工具数（卸载运行中插件时非 0）。
    pub removed_tools: u32,
    /// 插件目录是否已删除（目录被占用时会留残，需如实告知）。
    pub removed_dir: bool,
}

/// `plugin.load` 参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginLoadParams {
    /// 插件 id。
    pub plugin_id: String,
}

/// `plugin.load` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginLoadResult {
    /// 加载后的插件摘要（状态 `ready`）。
    pub plugin: PluginSummary,
    /// 握手后注册的工具。
    pub tools: Vec<PluginToolSummary>,
}

/// `plugin.load_many` 参数（P5 切片八：批量并发装载）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginLoadManyParams {
    /// 要装载的插件 id 列表。
    ///
    /// 顺序即结果顺序；重复 id 只装一次（去重保留首次出现的位置）。空数组是
    /// 合法输入，返回空 `items`——批量接口不该为空集合报错。
    pub plugin_ids: Vec<String>,
    /// 同时在装载的插件数上限；`None` 取 [`Self::DEFAULT_MAX_CONCURRENT`]。
    ///
    /// 传 `0` 也按默认值处理（不拒绝也不串行化），超过
    /// [`Self::MAX_CONCURRENT_LIMIT`] 时按上限截断——上限存在的意义是别让
    /// 外壳一次拉起几十个子进程把机器压死，而不是拿参数错误去折腾调用方。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub max_concurrent: Option<u32>,
}

impl PluginLoadManyParams {
    /// 缺省并发度：同时最多装载 4 个插件。
    pub const DEFAULT_MAX_CONCURRENT: u32 = 4;
    /// 并发度硬上限：再多也没意义（每个插件都要起一个子进程并等握手）。
    pub const MAX_CONCURRENT_LIMIT: u32 = 16;
    /// 单次批量装载的插件数上限，防止一次请求把队列塞爆。
    pub const MAX_PLUGINS: usize = 32;
}

/// 批量装载中单个插件的结局。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginLoadManyItem {
    /// 插件 id（与入参一致）。
    pub plugin_id: String,
    /// 装载成功时的结果；失败为 `None`。
    pub result: Option<PluginLoadResult>,
    /// 装载失败时的原因；成功为 `None`。
    ///
    /// 失败**不**让整个批量请求变成 JSON-RPC 错误：一次装 5 个挂掉 1 个，
    /// 调用方要的是另外 4 个的结果 + 这 1 个的原因，而不是一片空白。
    pub error: Option<PluginLoadFailure>,
}

/// 单个插件装载失败的原因（与 JSON-RPC 错误体同源，便于外壳统一呈现）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginLoadFailure {
    /// 错误码（`ErrorCode` 的数值，如 6101 插件不存在）。
    pub code: i32,
    /// 错误说明。
    pub message: String,
}

/// `plugin.load_many` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginLoadManyResult {
    /// 每个插件的结局，顺序与入参一致（重复 id 只保留首次出现的一条）。
    pub items: Vec<PluginLoadManyItem>,
    /// 本次实际采用的并发度（已按 [`PluginLoadManyParams`] 的规则归一）。
    pub max_concurrent: u32,
}

/// `plugin.unload` 参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginUnloadParams {
    /// 插件 id。
    pub plugin_id: String,
}

/// `plugin.unload` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginUnloadResult {
    /// 插件 id。
    pub plugin_id: String,
    /// 从 agent 注册表下架的工具数。
    pub removed_tools: u32,
}

/// `plugin.reload` 参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginReloadParams {
    /// 插件 id。
    pub plugin_id: String,
}

/// `plugin.reload` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginReloadResult {
    /// 重载后的插件摘要。
    pub plugin: PluginSummary,
    /// 重载后注册的工具。
    pub tools: Vec<PluginToolSummary>,
}

/// `plugin.command` 参数（P5 切片二：命令面板触发）。
///
/// 与 [`PluginCallParams`] 刻意的差别：命令只有 `name` + `args` 两个业务字段，
/// 没有 `method`——「执行哪个插件、哪条命令」在协议层是一等公民，因为闸门要按
/// **归属**（这命令是不是这个插件声明的）逐条判，不能靠一个自由字符串承载。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginCommandParams {
    /// 插件 id。
    pub plugin_id: String,
    /// 命令名（**只能是本插件 `commands[]` 里声明的**，越权回 6106）。
    pub command: String,
    /// 入参，缺省视为 `{}`；非空时按清单 `commands[].args` 校验，非法回 `-32602`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub args: Option<serde_json::Value>,
}

/// `plugin.command` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginCommandResult {
    /// 插件 id（回显，便于一次并发多命令时对号）。
    pub plugin_id: String,
    /// 实际执行的命令名。
    pub command: String,
    /// 结果正文（文本化后回灌调用方）。
    pub content: String,
    /// 正文是否被截断（超过 64 KB）。
    pub truncated: bool,
}

/// `plugin.call` 参数（docs/14 §6.3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginCallParams {
    /// 插件 id。
    pub plugin_id: String,
    /// 工具名或命令名（**只能是本插件声明的**，越权回 6106）。
    pub method: String,
    /// 入参，缺省视为 `{}`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub params: Option<serde_json::Value>,
}

/// `plugin.call` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginCallResult {
    /// 插件 id（回显，便于一次并发多调用时对号）。
    pub plugin_id: String,
    /// 实际执行的方法名。
    pub method: String,
    /// 结果正文（文本化后回灌模型）。
    pub content: String,
    /// 正文是否被截断（超过 64 KB）。
    pub truncated: bool,
}

/// `plugin.grant` 参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginGrantParams {
    /// 插件 id。
    pub plugin_id: String,
    /// 要授予的能力；空数组表示「授予清单申请的全部能力」。
    pub capabilities: Vec<String>,
}

/// `plugin.grant` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginGrantResult {
    /// 授权后的插件摘要（`granted_capabilities` 已更新）。
    pub plugin: PluginSummary,
}

/// `plugin.revoke` 参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginRevokeParams {
    /// 插件 id。
    pub plugin_id: String,
    /// 要撤销的能力。
    pub capabilities: Vec<String>,
}

/// `plugin.revoke` 响应结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginRevokeResult {
    /// 撤销后的插件摘要。
    pub plugin: PluginSummary,
    /// 是否因撤销关键能力而连带卸载（撤销后工具已下架）。
    pub unloaded: bool,
}

// ---------------------------------------------------------------------------
// lsp.* —— 语言服务（切片一：协议面先行，dispatch 待接线）
// ---------------------------------------------------------------------------
//
// 坐标口径：**行列都从 1 起算**。LSP 线格式（`textDocument/position`）是 0-based，
// 但那是内核与服务器之间的私事：`crates/lsp::features` 已经把入参换算成 0-based、
// 出参统一回 1-based。协议面对外壳只暴露一套基数，避免 UI 每处都要记得「行 +1、
// 列 +0」，也避免 `range` 与 `position` 用两套口径而互相错位。
//
// 本文件不复刻 LSP 的 `textDocument/*` 线格式：外壳不该知道 `LocationLink`、
// `MarkupContent`、`CompletionItemKind` 这些方言细节，只该知道「跳哪去、显示什么」。

/// 编辑器位置，**行列均从 1 起算**（列是 UTF-16 码元计数，与 Monaco 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspPosition {
    /// 行号，从 1 开始。
    #[ts(type = "number")]
    pub line: u32,
    /// 行内偏移，从 1 开始。
    #[ts(type = "number")]
    pub character: u32,
}

/// 编辑器区间，**半开**：含 `start`、不含 `end`（与 LSP `Range` 同构，仍用 1-based）。
///
/// 半开是刻意的：空区间（`start == end`）能表达「光标插入点」，闭区间表达不了；
/// 跨行选中时 `end.character` 也不会多算一个字符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspRange {
    /// 起点（含）。
    pub start: LspPosition,
    /// 终点（不含）。
    pub end: LspPosition,
}

/// 一条语言服务器连接的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum LspServerStatus {
    /// 进程已起、正在 `initialize` 握手（此时发查询会排队或失败）。
    Starting,
    /// 握手完成，可以收查询与文档同步。
    Ready,
    /// 已正常关闭（`stop` 之后，或用户手动关掉）。
    Stopped,
    /// 进程异常退出（崩溃 / 握手被拒）。`last_error` 有原因。
    Crashed,
}

/// 一条语言服务器的摘要（`lsp.start` / `lsp.servers` 回传）。
///
/// 字段刻意只放「外壳要显示、要据此决策」的东西：`server_id` 是后续每次调用的
/// 句柄，`status` 决定面板按钮是「启动」还是「停止」，`restarts` 与 `last_error`
/// 回答「它为什么一直在重连」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspServerInfo {
    /// 连接句柄，由内核分配（同一 `(language_id, root)` 复用同一条）。
    pub server_id: String,
    /// 语言 id（`typescript` / `python` / `rust` …，取自注册表）。
    pub language_id: String,
    /// 实际执行的命令（可能是备选实现，例如 `pyright-langserver` 缺失时退到 `pylsp`）。
    pub command: String,
    /// 工作区根目录绝对路径；服务器不要求根时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub root: Option<String>,
    /// 连接状态。
    pub status: LspServerStatus,
    /// 进程号（`starting` / `ready` 时给出，其余为空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pid: Option<u32>,
    /// 本会话内自动重启次数（到上限就停在 `crashed`）。
    #[ts(type = "number")]
    pub restarts: u32,
    /// 最近一次失败原因（只保留文本，不含栈）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_error: Option<String>,
}

/// `lsp.start` 参数。
///
/// `path` 与 `language_id` 至少给一个：给 `path` 时由注册表按扩展名推语言并决定
/// 命令，给 `language_id` 时按语言直接取注册表条目。两者都给时 `language_id`
/// 优先（用户显式指定的方言不该被扩展名覆盖）。
///
/// `command` / `args` 是逃生口：注册表里没有的语言、或想用 `vtsls` 替
/// `typescript-language-server` 时用。参数走数组，**不拼 shell 字符串**——
/// 拼字符串等于把命令注入的口子开给渲染进程。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspStartParams {
    /// 目标文件绝对路径（据此推语言、定根）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    /// 目标语言 id；与 `path` 至少给一个。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub language_id: Option<String>,
    /// 工作区根目录；缺省取 `path` 所在项目的根（找不到则用文件所在目录）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub root: Option<String>,
    /// 覆盖注册表里的命令（可执行文件路径或 PATH 上的名字）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub command: Option<String>,
    /// 覆盖启动参数（配合 `command` 用）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

/// `lsp.start` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspStartResult {
    /// 起好（或复用）的连接摘要。
    pub server: LspServerInfo,
    /// 是否是复用已有连接（`true` 时 `restarts` 与 `pid` 保持原值）。
    pub reused: bool,
}

/// `lsp.stop` 参数。
///
/// 只认 `server_id`：进程归内核管，外壳没有「指定 pid / 强制杀」的入参。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspStopParams {
    /// 要关掉的连接句柄（来自 `lsp.start` / `lsp.servers`）。
    pub server_id: String,
}

/// `lsp.stop` 响应结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspStopResult {
    /// 被关闭的连接句柄（回显）。
    pub server_id: String,
    /// 是否真的关掉了一条活着的连接（`false` 表示本来就已关闭，幂等不报错）。
    pub stopped: bool,
}

/// `lsp.servers` 参数（无入参，留空对象占位）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspServersParams {}

/// `lsp.servers` 响应结果。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspServersResult {
    /// 当前活着的连接（按 `server_id` 升序，顺序稳定便于前端 diff）。
    pub servers: Vec<LspServerInfo>,
}

/// `lsp.request` 支持的方法。
///
/// **枚举而非自由字符串**：语言服务器的线格式方法名（`textDocument/hover` 等）
/// 不许从渲染进程透传——否则外壳能借这条通道问任意东西（包括 `workspace/executeCommand`
/// 这类有副作用的）。查询面只开四个只读动作，与 `crates/lsp::features` 一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum LspRequestMethod {
    /// 跳转定义。
    Definition,
    /// 查找引用。
    References,
    /// 悬停信息。
    Hover,
    /// 补全候选。
    Completion,
}

/// `lsp.request` 参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspRequestParams {
    /// 连接句柄。
    pub server_id: String,
    /// 查询类型。
    pub method: LspRequestMethod,
    /// 目标文件绝对路径。
    pub path: String,
    /// 查询位置（**1-based**）。`completion` 用它定补全上下文，`references` 用它定符号。
    pub position: LspPosition,
    /// 仅 `references` 有效：是否把符号声明本身算进结果。
    ///
    /// 缺省交给服务器默认值——各服务器不一致（TypeScript 默认含、Pyright 默认不含），
    /// 所以想要确定结果时显式给。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub include_declaration: Option<bool>,
}

/// 一个可跳转位置（`definition` / `references` 结果）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspLocation {
    /// 原始 uri（服务器给的，可能是 `git:` / `untitled:` 等非文件 scheme）。
    pub uri: String,
    /// 能落回磁盘时给出绝对路径；非文件 scheme 时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    /// 1-based 半开区间。
    pub range: LspRange,
}

/// 悬停内容（`hover` 结果）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspHover {
    /// 内容正文（服务器给 Markdown 时原样保留，交给前端渲染）。
    pub contents: String,
    /// 悬停实际作用的区间；服务器可省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub range: Option<LspRange>,
}

/// 一个补全候选（只保留上层会展示 / 插入的字段）。
///
/// 不复刻 LSP 的 `textEdit` / `additionalTextEdits`：跨区改写由编辑器侧
/// 自己决定要不要采纳，协议面先只给「插入什么文本」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspCompletionItem {
    /// 展示文本，也是唯一的必填字段。
    pub label: String,
    /// LSP `CompletionItemKind`（1 文本、3 函数、6 变量…），原样透传供前端选图标。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number")]
    pub kind: Option<u32>,
    /// 右侧灰字说明（类型签名等）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub detail: Option<String>,
    /// 文档说明（Markdown 或纯文本）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub documentation: Option<String>,
    /// 插入文本（服务器要求替换 `label` 时给）；为空时插 `label` 本身。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub insert_text: Option<String>,
    /// 排序键（服务器指定优先级时给）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sort_text: Option<String>,
}

/// 补全列表（`completion` 结果）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspCompletionList {
    /// 服务器是否认为结果不完整（`true` 时编辑器应继续按前缀本地过滤）。
    pub is_incomplete: bool,
    /// 候选项。
    pub items: Vec<LspCompletionItem>,
}

/// `lsp.request` 响应结果。
///
/// 四个方法共用一个信封：`method` 回显决定哪个字段非空。不用
/// `serde(untagged)` 的四选一——那会让「空结果」与「查错字段」无法区分，
/// 前端还要靠猜来判断是该画跳转列表还是补全面板。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspRequestResult {
    /// 连接句柄（回显）。
    pub server_id: String,
    /// 查询类型（回显；前端据此选渲染分支）。
    pub method: LspRequestMethod,
    /// 目标文件绝对路径（回显）。
    pub path: String,
    /// `definition` / `references` 的结果；其他方法为空数组。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<LspLocation>,
    /// `hover` 的结果；没有可展示内容时为空（不是错误）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub hover: Option<LspHover>,
    /// `completion` 的结果；其他方法为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub completions: Option<LspCompletionList>,
}

/// `lsp.sync` 参数。
///
/// `content` 缺省表示「从磁盘读」：改了没保存的文件要把编辑器缓冲传上来，
/// 否则服务器看到的还是旧文本，定义跳转会跳到已经删掉的行上。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspSyncParams {
    /// 连接句柄。
    pub server_id: String,
    /// 目标文件绝对路径。
    pub path: String,
    /// 编辑器里的当前文本；为空则读磁盘。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub content: Option<String>,
}

/// `lsp.sync` 响应结果。
///
/// 幂等：同一份文本重复同步不会推版本号、也不会重发 `didChange`，所以
/// 外壳不必自己维护「上次发的是哪版」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspSyncResult {
    /// 连接句柄（回显）。
    pub server_id: String,
    /// 目标文件绝对路径（回显）。
    pub path: String,
    /// 文档 uri（同步用的键，后续查询按它定位）。
    pub uri: String,
    /// 同步后的版本号（`didOpen` 为 1，之后每次真变更 +1）。
    #[ts(type = "number")]
    pub version: i32,
    /// 本次是否真的发了 `didOpen`。
    pub opened: bool,
    /// 本次是否真的发了 `didChange`。
    pub changed: bool,
}

/// 诊断严重度。
///
/// LSP 线格式是 1..4 的数字，这里换成枚举：数字 `1` 与 `Error` 谁看都得查表，
/// 而 UI 分支写 `severity === 'error'` 一眼就懂。`0`／缺省／越界一律归一成
/// [`LspDiagnosticSeverity::Unknown`]——不猜成 `error`，免得把提示画成红点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum LspDiagnosticSeverity {
    /// 错误（LSP `1`）。
    Error,
    /// 警告（LSP `2`）。
    Warning,
    /// 信息（LSP `3`）。
    Information,
    /// 提示（LSP `4`）。
    Hint,
    /// 服务器没给严重度，或给了越界值。
    Unknown,
}

/// 一条诊断。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspDiagnostic {
    /// 出问题的区间（1-based 半开）。
    pub range: LspRange,
    /// 严重度。
    pub severity: LspDiagnosticSeverity,
    /// 服务器给的错误码（`TS2304` 这类）；可直接展示，但不要拿来分支。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code: Option<String>,
    /// 来源（`ts` / `pyright` / `rust-analyzer` …）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source: Option<String>,
    /// 文案。
    pub message: String,
}

/// `event::LSP_DIAGNOSTICS` 通知载荷。
///
/// **整篇替换**：每次推送都是某文件的完整诊断清单，前端拿到就直接覆盖该文件
/// 的标记集合。空数组是有效载荷（表示问题已全部消失），不能被当成「没内容」丢掉
/// ——那样旧的红点会永久残留。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LspDiagnosticsEvent {
    /// 连接句柄。
    pub server_id: String,
    /// 目标文件绝对路径。
    pub path: String,
    /// 这批诊断对应的文档版本；服务器没给时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number")]
    pub version: Option<i32>,
    /// 该文件的全部诊断（可能为空）。
    pub diagnostics: Vec<LspDiagnostic>,
}

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

    /// 未知压缩策略必须在协议层就被拒：内核只认 `truncate` / `summarize`，
    /// 把拼错的值静默当成 `truncate` 会让调用方以为自己拿到了摘要。
    #[test]
    fn compress_strategy_rejects_unknown_value() {
        let err = serde_json::from_value::<CompressOptions>(serde_json::json!({
            "strategy": "summarise"
        }))
        .expect_err("未知策略不该被解析成功");
        assert!(
            err.to_string().contains("unknown variant"),
            "报错要指明取值非法：{err}"
        );
    }

    #[test]
    fn compress_options_default_to_truncate() {
        let options: CompressOptions = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(options.enabled, "缺省开启裁剪");
        assert_eq!(options.strategy, CompressStrategy::Truncate);
        assert_eq!(options.budget_tokens, None);

        let summarize: CompressOptions =
            serde_json::from_value(serde_json::json!({ "strategy": "summarize" })).unwrap();
        assert_eq!(summarize.strategy, CompressStrategy::Summarize);
    }

    #[test]
    fn approval_decision_serializes_to_the_four_wire_values() {
        for (decision, expected) in [
            (ApprovalDecision::AllowOnce, "\"allow_once\""),
            (ApprovalDecision::AllowAlways, "\"allow_always\""),
            (ApprovalDecision::Deny, "\"deny\""),
            (ApprovalDecision::DenyWithReason, "\"deny_with_reason\""),
        ] {
            assert_eq!(serde_json::to_string(&decision).unwrap(), expected);
        }
    }

    /// 拼错的裁决值必须在协议层就被拒。静默降级成 `deny` 会让「随时放行」变成
    /// 「随时拒绝」，而用户以为自己点的是允许。
    #[test]
    fn approval_decision_rejects_unknown_value() {
        let err = serde_json::from_value::<ApprovalDecision>(serde_json::json!("yes"))
            .expect_err("未知裁决值不该被解析成功");
        assert!(
            err.to_string().contains("unknown variant"),
            "报错要指明取值非法：{err}"
        );
    }

    #[test]
    fn agent_start_params_default_to_agent_mode() {
        let params: AgentStartParams = serde_json::from_value(serde_json::json!({
            "model_ref": { "provider_id": "p1", "model_id": "m1" },
            "goal": "给 src/api.ts 的请求加超时"
        }))
        .unwrap();
        assert_eq!(params.mode, AgentMode::Agent, "缺省即闭环执行");
        assert!(params.session_id.is_none());
        assert!(params.context_refs.is_empty());
        assert_eq!(params.max_steps, None);
        assert!(params.permissions_override.is_none());

        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["mode"], "agent");
        let back: AgentStartParams = serde_json::from_value(json).unwrap();
        assert_eq!(back, params);
    }

    #[test]
    fn agent_decide_params_carry_reason_for_deny_with_reason() {
        let decide: AgentDecideParams = serde_json::from_value(serde_json::json!({
            "run_id": "r1",
            "approval_id": "a1",
            "decision": "deny_with_reason",
            "reason": "改的是密钥文件"
        }))
        .unwrap();
        assert_eq!(decide.decision, ApprovalDecision::DenyWithReason);
        assert_eq!(decide.reason.as_deref(), Some("改的是密钥文件"));

        let plain: AgentDecideParams = serde_json::from_value(serde_json::json!({
            "run_id": "r1", "approval_id": "a1", "decision": "allow_once"
        }))
        .unwrap();
        assert_eq!(plain.reason, None);
    }

    #[test]
    fn agent_approval_required_event_carries_call_detail() {
        let event: AgentApprovalRequiredEvent = serde_json::from_value(serde_json::json!({
            "run_id": "r1",
            "approval_id": "a1",
            "tool": "run_command",
            "args": { "cmd": "pnpm test" },
            "risk": "medium",
            "reason": "会执行外部命令"
        }))
        .unwrap();
        assert_eq!(event.tool, "run_command");
        assert_eq!(event.args["cmd"], "pnpm test");
        assert_eq!(event.risk, ApprovalRisk::Medium);
    }

    #[test]
    fn agent_finished_event_roundtrips_all_three_statuses() {
        for (status, expected) in [
            (AgentRunStatus::Completed, "completed"),
            (AgentRunStatus::Failed, "failed"),
            (AgentRunStatus::Cancelled, "cancelled"),
        ] {
            let event = AgentFinishedEvent {
                run_id: "r1".into(),
                status,
                steps: 3,
                summary: None,
            };
            let json = serde_json::to_value(&event).unwrap();
            assert_eq!(json["status"], expected);
            assert_eq!(json["steps"], 3);
            let back: AgentFinishedEvent = serde_json::from_value(json).unwrap();
            assert_eq!(back, event);
        }
    }

    /// 启动 / 停止的返回值必须带着 `run_id`，否则外壳攒不出「谁在跑」的列表。
    #[test]
    fn agent_start_and_stop_results_keep_run_id() {
        let started = AgentStartResult {
            run_id: "run-7".into(),
        };
        assert_eq!(
            serde_json::to_value(&started).unwrap()["run_id"],
            serde_json::json!("run-7")
        );

        let stopped = AgentStopResult {
            ok: true,
            steps_done: 2,
        };
        let json = serde_json::to_value(stopped).unwrap();
        assert_eq!(json["ok"], true);
        assert_eq!(json["steps_done"], 2);
        assert_eq!(
            serde_json::from_value::<AgentStopResult>(json).unwrap(),
            stopped
        );
    }

    // -----------------------------------------------------------------------
    // session.*（M5）
    // -----------------------------------------------------------------------

    /// `session.create` 参数全部可选：`{}` 就该能建一个默认标题的会话。
    #[test]
    fn session_create_params_are_all_optional() {
        let params: SessionCreateParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(params, SessionCreateParams::default());
        assert!(params.title.is_none());
        assert!(params.workspace_id.is_none());
    }

    /// `include_messages` 缺省为 `true`——客户端只给 `session_id` 也应拿到历史。
    #[test]
    fn session_get_defaults_to_include_messages() {
        let params: SessionGetParams =
            serde_json::from_value(serde_json::json!({ "session_id": "s1" })).unwrap();
        assert!(params.include_messages);
    }

    /// 追加消息的参数要能直接吃下 [`ChatMessage`] 的形状。
    #[test]
    fn session_append_message_accepts_chat_message_shape() {
        let params: SessionAppendMessageParams = serde_json::from_value(serde_json::json!({
            "session_id": "s1",
            "message": { "role": "user", "content": "读取 dispatch.rs" },
            "parent_id": null,
        }))
        .unwrap();
        assert_eq!(params.session_id, "s1");
        assert_eq!(params.message.role, ChatRole::User);
        assert_eq!(params.message.content, "读取 dispatch.rs");
        assert!(params.usage.is_none());
    }

    /// 毫秒时间戳必须以 number 出网，不能被当成字符串。
    #[test]
    fn session_view_keeps_timestamps_as_numbers() {
        let view = SessionView {
            session_id: "s1".into(),
            title: "轮次一".into(),
            workspace_path: Some("E:/ai_ide".into()),
            model_ref: None,
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_000_500,
        };
        let json = serde_json::to_value(&view).unwrap();
        assert!(json["created_at_ms"].is_number());
        assert!(json["updated_at_ms"].is_number());
        assert_eq!(serde_json::from_value::<SessionView>(json).unwrap(), view);
    }

    /// 导出格式缺省 markdown；给 json 时按小写出网，不能写成 `Json`。
    #[test]
    fn export_format_defaults_to_markdown_and_serializes_lowercase() {
        let params: SessionExportParams = serde_json::from_value(serde_json::json!({
            "session_id": "s1",
            "out_path": "E:/tmp/会话.md",
        }))
        .unwrap();
        assert_eq!(params.format, SessionExportFormat::Markdown);
        assert_eq!(params.format.extension(), "md");

        let json = serde_json::to_value(SessionExportFormat::Json).unwrap();
        assert_eq!(json, serde_json::json!("json"));
        let back: SessionExportFormat = serde_json::from_value(json).unwrap();
        assert_eq!(back, SessionExportFormat::Json);
    }

    /// fork 的两个可选字段都能缺省；`from_message_id` 是「可截断」而不是必填。
    #[test]
    fn fork_params_optional_fields_default_to_none() {
        let params: SessionForkParams =
            serde_json::from_value(serde_json::json!({ "session_id": "s1" })).unwrap();
        assert!(params.from_message_id.is_none());
        assert!(params.title.is_none());
    }

    // -----------------------------------------------------------------------
    // audit.*（M5 切片四）
    // -----------------------------------------------------------------------

    /// 审计查询参数全部可选：`{}` 就是「最近 50 条」。
    #[test]
    fn audit_list_params_are_all_optional() {
        let params: AuditListParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(params, AuditListParams::default());
        assert!(params.session_id.is_none());
        assert!(params.kind.is_none());
        assert!(params.limit.is_none());
    }

    /// 时间戳必须出成数字而不是字符串：前端直接比较大小、算时间窗。
    #[test]
    fn audit_event_view_keeps_timestamps_numeric() {
        let view = AuditEventView {
            event_id: 7,
            kind: "session.deleted".into(),
            session_id: Some("s1".into()),
            actor: "core".into(),
            summary: "删除会话「旧的」".into(),
            detail: serde_json::json!({ "title": "旧的" }),
            created_at_ms: 1_700_000_000_000,
        };
        let json = serde_json::to_value(&view).unwrap();
        assert!(json["created_at_ms"].is_number());
        assert!(json["event_id"].is_number());
        assert_eq!(json["detail"]["title"], serde_json::json!("旧的"));
        assert_eq!(
            serde_json::from_value::<AuditEventView>(json).unwrap(),
            view
        );
    }

    /// `detail` 与 `session_id` 缺省时要能反序列化：老客户端 / 手写载荷都得容忍。
    #[test]
    fn audit_event_view_tolerates_missing_optional_fields() {
        let view: AuditEventView = serde_json::from_value(serde_json::json!({
            "event_id": 1,
            "kind": "session.created",
            "actor": "core",
            "summary": "创建会话",
            "created_at_ms": 1,
        }))
        .unwrap();
        assert!(view.session_id.is_none());
        assert_eq!(view.detail, serde_json::Value::Null);
    }

    // -----------------------------------------------------------------------
    // terminal.*（M6 切片一）
    // -----------------------------------------------------------------------

    /// `{}` 就是一个可用的建会话请求：缺省 shell、缺省 80×24、继承 cwd。
    #[test]
    fn terminal_create_params_are_all_optional() {
        let params: TerminalCreateParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(params, TerminalCreateParams::default());
        assert!(params.shell.is_none() && params.cwd.is_none());
        assert!(params.cols.is_none() && params.rows.is_none());
        assert!(params.args.is_empty() && params.env.is_empty());
    }

    /// 会话视图的 `pid` 可缺省；`cols` / `rows` 必须是数字（前端要做算术）。
    #[test]
    fn terminal_view_keeps_size_numeric_and_pid_optional() {
        let view = TerminalView {
            id: "t1".into(),
            shell: r"C:\Windows\System32\cmd.exe".into(),
            cwd: Some(r"E:\ai_ide".into()),
            pid: None,
            cols: 120,
            rows: 30,
            created_at_ms: 1_700_000_000_000,
        };
        let json = serde_json::to_value(&view).unwrap();
        assert!(json["cols"].is_number());
        assert!(json["rows"].is_number());
        assert!(json["created_at_ms"].is_number());
        assert!(json.get("pid").is_none(), "None 的 pid 不该出现：{json}");
        assert_eq!(
            serde_json::from_value::<TerminalView>(serde_json::json!({
                "id": "t1",
                "shell": "cmd.exe",
                "cols": 80,
                "rows": 24,
                "created_at_ms": 1,
            }))
            .unwrap()
            .pid,
            None
        );
    }

    /// 退出事件的 `code` / `signal` 可以是 `null`（被信号终止 / Windows 无信号），
    /// 但字段必须在——前端按字段存在与否判断「是否已退出」会踩坑。
    #[test]
    fn terminal_exit_event_keeps_null_fields() {
        let event = TerminalExitEvent {
            terminal_id: "t1".into(),
            code: None,
            signal: Some("SIGKILL".into()),
            at_ms: 1,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert!(json.get("code").is_some(), "code 字段必须在：{json}");
        assert!(json["code"].is_null());
        assert_eq!(json["signal"], serde_json::json!("SIGKILL"));
        assert_eq!(
            serde_json::from_value::<TerminalExitEvent>(json).unwrap(),
            event
        );
    }

    /// 写操作按字节算：`bytes` 是解码后的长度，不是 base64 字符串长度。
    #[test]
    fn terminal_write_dto_carries_base64_payload() {
        let params: TerminalWriteParams = serde_json::from_value(
            serde_json::json!({ "terminal_id": "t1", "data": "ZWNobyBoaQ0K" }),
        )
        .unwrap();
        assert_eq!(params.data, "ZWNobyBoaQ0K");
        let result = TerminalWriteResult { bytes: 11 };
        assert_eq!(
            serde_json::to_value(&result).unwrap()["bytes"],
            serde_json::json!(11)
        );
    }

    /// 插件状态与工具风险都必须序列化成小写字面量：TS 侧靠字面量做穷尽 switch，
    /// 一旦变成 `NeedsGrant` 这种 PascalCase，前端所有分支都静默落空。
    #[test]
    fn plugin_state_and_risk_serialize_as_lower_snake() {
        assert_eq!(
            serde_json::to_value(PluginState::NeedsGrant).unwrap(),
            serde_json::json!("needs_grant")
        );
        assert_eq!(
            serde_json::to_value(PluginState::Ready).unwrap(),
            serde_json::json!("ready")
        );
        assert_eq!(PluginState::default(), PluginState::Disabled);
        assert_eq!(
            serde_json::to_value(PluginToolRisk::Exec).unwrap(),
            serde_json::json!("exec")
        );
        assert_eq!(PluginToolRisk::default(), PluginToolRisk::Read);
        assert_eq!(
            serde_json::from_value::<PluginState>(serde_json::json!("crashed")).unwrap(),
            PluginState::Crashed
        );
    }

    /// `plugin.call` 的 `params` 允许缺省（docs/14 §6.3 第 1 步），缺省即 `{}`；
    /// 而 `plugin_id` / `method` 缺失必须报错——不能让空字符串混进寻址键。
    #[test]
    fn plugin_call_params_default_to_empty_and_require_ids() {
        let params: PluginCallParams = serde_json::from_value(
            serde_json::json!({ "plugin_id": "demo-hello", "method": "repo_stats" }),
        )
        .unwrap();
        assert_eq!(params.plugin_id, "demo-hello");
        assert_eq!(
            params.params, None,
            "缺省的 params 应是 None，由宿主补 {{}}"
        );
        assert!(serde_json::from_value::<PluginCallParams>(
            serde_json::json!({ "method": "repo_stats" })
        )
        .is_err());
        assert!(serde_json::from_value::<PluginCallParams>(
            serde_json::json!({ "plugin_id": "demo-hello" })
        )
        .is_err());
    }

    /// 摘要视图的两个 `Option` 字段必须真的「缺省即不出现」：面板按字段存在与否
    /// 渲染角标，序列化出一个 `"last_error": null` 会让健康的插件也显示错误点。
    #[test]
    fn plugin_summary_omits_optional_fields() {
        let summary = PluginSummary {
            id: "demo-hello".into(),
            name: "Demo Hello".into(),
            version: "0.1.0".into(),
            description: None,
            state: PluginState::Disabled,
            capabilities: vec!["fs.read".into()],
            granted_capabilities: vec![],
            tool_count: 0,
            restarts: 0,
            last_error: None,
        };
        let json = serde_json::to_value(&summary).unwrap();
        assert!(json.get("description").is_none(), "{json}");
        assert!(json.get("last_error").is_none(), "{json}");
        assert_eq!(json["state"], serde_json::json!("disabled"));
        assert_eq!(
            serde_json::from_value::<PluginSummary>(json).unwrap(),
            summary
        );
    }

    // -----------------------------------------------------------------------
    // lsp.*（切片一）
    // -----------------------------------------------------------------------

    /// `method` 是枚举而不是自由字符串：透传线格式方法名等于把
    /// `workspace/executeCommand` 这类有副作用的方法也开给渲染进程。
    #[test]
    fn lsp_request_method_rejects_unknown_string() {
        assert_eq!(
            serde_json::to_string(&LspRequestMethod::Hover).unwrap(),
            "\"hover\""
        );
        assert_eq!(
            serde_json::to_string(&LspRequestMethod::Definition).unwrap(),
            "\"definition\""
        );
        for bad in [
            "textDocument/hover",
            "workspace/executeCommand",
            "HOVER",
            "",
        ] {
            assert!(
                serde_json::from_value::<LspRequestMethod>(serde_json::json!(bad)).is_err(),
                "非法 method 应被拒: {bad}"
            );
        }
    }

    /// 入参位置原样透传 1-based，不做任何 ±1 —— 换算只在内核侧做一次。
    /// 这里钉住的是「协议层不偷偷改坐标」，改了会让外壳每次差一行。
    #[test]
    fn lsp_request_params_keep_one_based_position() {
        let params: LspRequestParams = serde_json::from_value(serde_json::json!({
            "server_id": "lsp-1",
            "method": "references",
            "path": "E:/ai_ide/crates/protocol/src/dto.rs",
            "position": { "line": 1, "character": 1 },
            "include_declaration": true
        }))
        .unwrap();
        assert_eq!(params.position.line, 1);
        assert_eq!(params.position.character, 1);
        assert_eq!(params.method, LspRequestMethod::References);
        assert_eq!(params.include_declaration, Some(true));

        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["position"]["line"], 1);
        assert_eq!(json["position"]["character"], 1);

        // `include_declaration` 缺省时不出现：让服务器默认值生效，
        // 而不是被我们悄悄写成 `false`。
        let json = serde_json::to_value(LspRequestParams {
            server_id: "lsp-1".into(),
            method: LspRequestMethod::Hover,
            path: "E:/a.ts".into(),
            position: LspPosition {
                line: 3,
                character: 7,
            },
            include_declaration: None,
        })
        .unwrap();
        assert!(json.get("include_declaration").is_none(), "{json}");
    }

    /// 空诊断清单是有效载荷（问题已消失），必须能被反序列化并保留「空」这个事实。
    #[test]
    fn lsp_diagnostics_event_allows_empty_list() {
        let event: LspDiagnosticsEvent = serde_json::from_value(serde_json::json!({
            "server_id": "lsp-1",
            "path": "E:/a.ts",
            "diagnostics": []
        }))
        .unwrap();
        assert!(event.diagnostics.is_empty());
        assert_eq!(event.version, None);

        // 空列表序列化后字段仍在：丢了 `diagnostics` 前端就无法清标记。
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["diagnostics"], serde_json::json!([]));
        assert!(json.get("version").is_none(), "{json}");
    }

    #[test]
    fn lsp_diagnostic_severity_is_lowercase_and_roundtrips() {
        let dto = LspDiagnostic {
            range: LspRange {
                start: LspPosition {
                    line: 10,
                    character: 5,
                },
                end: LspPosition {
                    line: 10,
                    character: 12,
                },
            },
            severity: LspDiagnosticSeverity::Warning,
            code: Some("TS2304".into()),
            source: Some("ts".into()),
            message: "找不到名称 'foo'".into(),
        };
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["severity"], serde_json::json!("warning"));
        assert_eq!(serde_json::from_value::<LspDiagnostic>(json).unwrap(), dto);
    }

    /// `lsp.servers` 无入参，但要能从 `{}` 读出来（宿主可能补 `{}` 也可能不发 params）。
    #[test]
    fn lsp_servers_params_deserialize_from_empty_object() {
        let params: LspServersParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(params, LspServersParams {});
    }

    /// `lsp.start` 一次只给 `path` 也要能跑：可选字段不出现时不能被当成缺参。
    #[test]
    fn lsp_start_params_defaults_and_optional_fields() {
        let params: LspStartParams = serde_json::from_value(serde_json::json!({
            "path": "E:/a.py"
        }))
        .unwrap();
        assert_eq!(params.path.as_deref(), Some("E:/a.py"));
        assert_eq!(params.language_id, None);
        assert!(params.args.is_empty());

        let json = serde_json::to_value(&params).unwrap();
        for absent in ["language_id", "root", "command", "args"] {
            assert!(json.get(absent).is_none(), "{absent} 不应出现: {json}");
        }
    }

    /// `lsp.stop` 只认 `server_id`：多余入参该被拒，避免「顺手带个 pid 强杀」的口子。
    #[test]
    fn lsp_stop_params_require_server_id_only() {
        let params: LspStopParams =
            serde_json::from_value(serde_json::json!({ "server_id": "lsp-1" })).unwrap();
        assert_eq!(params.server_id, "lsp-1");
        assert!(serde_json::from_value::<LspStopParams>(serde_json::json!({})).is_err());
    }
}
