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

/// 布尔字段的 serde 默认值：`true`。
fn default_true() -> bool {
    true
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
}
