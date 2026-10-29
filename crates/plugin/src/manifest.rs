//! 插件清单：结构定义、宿主常量与「形状 → 语义 → 交叉」三段校验。
//!
//! 三段顺序不是排版偏好：后一段依赖前一段的产物。形状不过的清单（`tools` 不是数组）
//! 根本不该进入「工具名是否重复」的讨论，否则一个类型错误会被级联成十条无关报错，
//! 插件作者反而更难看懂。
//!
//! 与之配套的另一条规则是**一次报全**：三段都跑完才返回，不在第一条错误处短路。
//! 清单校验的成本就是一次 JSON 解析，逐条返回只会让作者来回改十轮。
//!
//! 校验码分工（docs/14 §5.3、§6.6）：
//!
//! - `6107` `PLUGIN_MANIFEST_INVALID`：形状/语义问题，`details[]` 里逐条给 `code/path/message`；
//! - `6104` `PLUGIN_API_MISMATCH`：`pluginApi` major 与宿主不等——单独成码，
//!   因为它的处置方式完全不同（换插件版本，而不是改清单字段）；
//! - `6108` `PLUGIN_TOOL_CONFLICT`：工具名与核心工具撞名。
//!
//! 顶层错误码统一用 `6107`（「这个包不合法」），细分码放在每条 `details` 里：
//! 外壳只需要一个分支就能把全部问题列出来，而不是按首个错误码决定文案、把剩下的藏起来。

use std::path::{Component, Path};

use ai_ide_protocol::dto::PluginToolRisk;
use ai_ide_protocol::ErrorCode;
use serde::Serialize;
use serde_json::{json, Map, Value};

/// 宿主实现的插件 API 大版本。清单的 major 必须与它相等（docs/14 §6.7）。
pub const HOST_PLUGIN_API_MAJOR: u32 = 1;

/// 宿主实现的插件 API 小版本。清单 minor 高于它只降级、不拒载。
///
/// 1.1 = 声明式视图（docs/24）：`views[]` 解析、`plugin.status` 原样下发、
/// `plugin:view` 刷新事件、外部视口 spawn。这一整套已经在这里落地，宿主就该
/// 如实报 1 而不是继续报 0——否则消费方（asset3d）声明 `1.1` 时会被打上
/// 「按宿主演进能力集降级运行」的告警，用户看到的是一条并不存在的兼容问题。
pub const HOST_PLUGIN_API_MINOR: u32 = 1;

/// 清单文件名。
pub const MANIFEST_FILE: &str = "plugin.json";

/// 单次工具调用超时默认值（毫秒）。
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// 单次工具调用超时下限（毫秒）。低于 1 s 的插件基本只会在握手阶段就超时。
pub const MIN_TIMEOUT_MS: u64 = 1_000;

/// 单次工具调用超时上限（毫秒），与 docs/14 §6.2 的 120 s 硬墙对齐。
pub const MAX_TIMEOUT_MS: u64 = 120_000;

/// 清单能声明的网络域名数量上限。
///
/// 32 是「够用且能一眼看完」的量级：清单是给用户审的东西，域名一多就没人看了，
/// 而 `allowedDomains` 恰恰是出站网络唯一的边界，看不完等于没边界。
pub const MAX_ALLOWED_DOMAINS: usize = 32;

/// 单个域名的字符长度上限（DNS 全名上限 253）。
pub const MAX_DOMAIN_LEN: usize = 253;

/// 模型白名单最多几条（`allowedModels`）。
///
/// 与域名同一个理由：清单是给用户审的，条目一多就没人逐条看了，而这份名单
/// 恰恰是插件能烧掉多少 token 的边界。
pub const MAX_ALLOWED_MODELS: usize = 16;
/// 单条模型条目的字符长度上限（`provider/model` 两段加起来够用）。
pub const MAX_MODEL_ENTRY_LEN: usize = 160;
/// 清单 `maxTokens` 的上限（与 `plugin_llm::MAX_TOKENS_CEILING` 同值）。
///
/// 两处各写一份常量是刻意的：清单校验在 `crates/plugin`，运行期夹取在
/// `crates/core`，后者不依赖前者的「信任」——校验通过不等于运行期不再看。
pub const MAX_TOKENS_CEILING: u64 = 8192;

/// 一份清单最多声明几个视图（`views`，docs/24 §4.1）。
///
/// 视图声明随 `plugin.status` 下发（ADR-15），条数就是 `status` 的固定开销：
/// 不设上限等于让一个插件把状态响应撑成正比于自己的野心。8 个够表达「主视图 +
/// 若干次级视图」，再多读者也翻不过来。
pub const MAX_VIEWS: usize = 8;

/// 视图标题的字符数上限（`views[].title`）。
pub const MAX_VIEW_TITLE_LEN: usize = 48;

/// 首版唯一允许的挂载位置（docs/24 §4.1 取舍 2）。
///
/// 渲染端侧栏是固定枚举，动态视图只能挂到 `PluginPanel` 详情区做二级 tab；
/// 放开第二个值就等于要改外壳枚举，所以这里写成单元素白名单而不是宽松校验。
pub const VIEW_LOCATIONS: &[&str] = &["pluginPanel.detail"];

/// 视图刷新事件的白名单（`views[].refresh`）。
///
/// 与 `subscriptions` 不同：这里不订阅事件载荷，只表达「这些事件发生时请重拉一次
/// `entry`」。所以名单必须是宿主确实会广播的那几个，写错一个就是永不刷新。
pub const VIEW_REFRESH_EVENTS: &[&str] = &["workspace.changed", "file.saved", "session.ended"];

/// 外部视口协议的宿主白名单（`views[].external.protocol`，ADR-17）。
///
/// 协议版本号由插件与视口进程约定，但**由宿主认可**：不在表里的协议说明这版 IDE
/// 不认识它的会话格式，装上了也只会连不上。首版仅 asset3d。
pub const VIEW_EXTERNAL_PROTOCOLS: &[&str] = &["asset3d.session/1"];

/// 外部视口可执行文件必须的扩展名（`views[].external.program`）。
pub const VIEW_EXTERNAL_SUFFIX: &str = ".exe";

/// 能力白名单——**这张表就是权限模型本身**（docs/14 §7.1）。
///
/// 不在表里的能力一律拒载。理由不是「还没实现所以先拦着」，而是「未知能力无法被
/// 闸门覆盖」：能力名拼错一个字母就静默变成另一个语义，比直接拒载危险得多。
pub const ALLOWED_CAPABILITIES: &[&str] = &[
    "fs.read",
    "fs.write",
    "search",
    "workspace.info",
    "net.fetch",
    "llm.chat",
    // 进度上报（P5 切片四）：`host/progress` 的能力名，沿用「方法名去掉 `host/`」
    // 的既有口径。它不读不写，只影响「插件能不能往进度条上说话」。
    "progress",
];

/// 拒绝清单：出现在清单里即拒载，不是「暂不支持」。
///
/// `shell.exec` 意味着插件能绕开工具闸门自己起进程——一旦给了它，
/// `risk`/审批/配额这三层全部形同虚设，所以它链路上不存在，而不是默认关闭。
pub const DENIED_CAPABILITIES: &[&str] = &["shell.exec"];

/// 核心工具名（与 `crates/agent/src/tools/` 的 8 个内置工具逐字一致）。
///
/// 插件工具注册时本来会加 `plugin__<id>__` 前缀，但清单里仍不允许撞名：
/// 撞名说明作者对宿主能力的认知有偏差，早拒比运行期歧义好。
pub const CORE_TOOL_NAMES: &[&str] = &[
    "apply_diff",
    "create",
    "edit",
    "grep",
    "list",
    "read",
    "run_command",
    "write",
];

// ---------------------------------------------------------------------------
// 清单结构
// ---------------------------------------------------------------------------

/// 一份通过校验的清单（规范化后的形态）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub plugin_api: String,
    pub main: String,
    pub capabilities: Vec<String>,
    pub tools: Vec<ManifestTool>,
    pub commands: Vec<ManifestCommand>,
    pub subscriptions: Vec<String>,
    /// 出站网络域名单（`host/net.fetch` 用，P5 切片五）。
    ///
    /// 与 `capabilities` 里的 `net.fetch` 是一对：能力回答「有没有资格联网」，
    /// 这里回答「能连谁」。两个都过才发得出去（见 [`crate::host_api::NET_FETCH_METHOD`]）。
    /// 空表示一个域名都不放行——`net.fetch` 已申请却没写清单的包在校验阶段就被拒，
    /// 运行期再兜一次「空表全拒」。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed_domains: Vec<String>,
    /// 模型白名单（`host/llm.chat` 用，P5 切片六）：`provider/model` 形式。
    ///
    /// 与 `capabilities` 里的 `llm.chat` 是一对：能力回答「能不能调模型」，这里
    /// 回答「能调哪些」。空表示一个模型都不放行——申请了 `llm.chat` 却没写这份
    /// 名单的包在校验阶段就被拒，运行期再兜一次「空名单全拒」。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed_models: Vec<String>,
    /// 单次模型调用允许的最大输出 token（清单 `maxTokens`）。
    ///
    /// `0` 表示没声明：没申请 `llm.chat` 的包不写它是正常的；申请了就会被校验
    /// 拦下。运行期按它夹，插件要更多一律 `-32602`。
    #[serde(skip_serializing_if = "is_zero")]
    pub max_tokens: u64,
    pub timeout_ms: u64,
    /// 声明式视图（docs/24 §4.1）。
    ///
    /// 空数组是绝大多数包常态，所以序列化时整个字段省略：落库 JSON 与改造前
    /// 逐字一致，老包的 `manifest_json` 不用迁移。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub views: Vec<ManifestView>,
}

/// `skip_serializing_if` 用：`maxTokens` 不声明时不出现在序列化结果里。
fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// 清单里的一个工具声明。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestTool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub risk: PluginToolRisk,
    /// 审批 scope 用的路径参数名；缺省表示该工具不针对具体文件。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_field: Option<String>,
    /// JSON Schema 子集，原样透传给模型。
    pub parameters: Value,
}

/// 清单里的一个命令声明（命令面板入口）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestCommand {
    pub name: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 审批档位：`write` 的命令执行前弹审批（P5 切片二）。
    ///
    /// 命令没有 `exec` 档位可用（同工具：宿主不提供 `shell.exec`），解析到 `exec`
    /// 按清单错误拒装——「写了 exec 却按 read 放行」是最坏的一种静默降级。
    pub risk: PluginToolRisk,
    /// 路径闸门用的入参字段名；缺省表示该命令不针对具体文件。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_field: Option<String>,
    /// 入参 JSON Schema（清单里的 `args`），缺省等于「不吃入参」。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
}

/// 清单里的一个视图声明（docs/24 §4.1）。
///
/// 视图只描述「挂在哪、标题叫什么、用哪个工具取内容」，内容本身不进清单：
/// 清单是安装期的静态文件，往里塞数据等于绕开插件进程与闸门（ADR-15）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestView {
    pub id: String,
    pub title: String,
    /// 挂载位置，首版仅 `pluginPanel.detail`（见 [`VIEW_LOCATIONS`]）。
    pub location: String,
    /// 取内容的工具名，必须是本包 `tools[]` 里 `risk=read` 的那一个。
    pub entry: String,
    /// 触发重拉的事件名（见 [`VIEW_REFRESH_EVENTS`]），空表示只在打开时拉一次。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub refresh: Vec<String>,
    /// 外部视口可执行文件；不声明就只渲染只读描述树（ADR-16）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external: Option<ManifestExternal>,
}

/// 视图配套的外部视口（`views[].external`）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestExternal {
    /// 包内相对路径的 `.exe`，安装时校验真实存在。
    pub program: String,
    /// 会话协议版本，须在 [`VIEW_EXTERNAL_PROTOCOLS`] 内。
    pub protocol: String,
}

/// 校验通过的结果：规范化清单 + 归一化入口 + 非阻断告警。
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedManifest {
    pub manifest: Manifest,
    /// 归一化后的包内相对入口（`/` 分隔），安装时用它定位入口文件。
    pub entry: String,
    /// 不阻断安装的告警（如 `pluginApi` minor 高于宿主、工具缺 description）。
    pub warnings: Vec<String>,
}

impl ValidatedManifest {
    /// 规范化清单的 JSON 表示（落库用，字段顺序固定，便于 diff）。
    pub fn to_manifest_json(&self) -> String {
        serde_json::to_string(&self.manifest).unwrap_or_else(|_| "{}".to_string())
    }

    /// 带命名空间的工具全名列表，注册/冲突检测共用一套拼法。
    pub fn namespaced_tool_names(&self) -> Vec<String> {
        self.manifest
            .tools
            .iter()
            .map(|tool| namespaced_tool(&self.manifest.id, &tool.name))
            .collect()
    }
}

/// 插件工具的命名空间拼法：`plugin__<plugin_id>__<name>`（docs/14 §6.4）。
pub fn namespaced_tool(plugin_id: &str, name: &str) -> String {
    format!("plugin__{plugin_id}__{name}")
}

// ---------------------------------------------------------------------------
// 错误条目
// ---------------------------------------------------------------------------

/// 一条清单问题。`code` 是本条自己的细分码，顶层错误统一包成 6107。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub code: ErrorCode,
    /// 指向清单内的位置，如 `$.tools[1].name`，前端可以直接高亮。
    pub path: String,
    pub message: String,
}

impl Violation {
    /// 形状/语义违规（6107）。
    pub fn manifest(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::PLUGIN_MANIFEST_INVALID,
            path: path.into(),
            message: message.into(),
        }
    }

    /// `pluginApi` 与宿主不兼容（6104）。
    pub fn api_mismatch(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::PLUGIN_API_MISMATCH,
            path: path.into(),
            message: message.into(),
        }
    }

    /// 工具名与核心工具冲突（6108）。
    pub fn tool_conflict(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::PLUGIN_TOOL_CONFLICT,
            path: path.into(),
            message: message.into(),
        }
    }

    /// 包内相对路径越界（1002）。
    ///
    /// 与 `main` 的处理刻意不同：`main` 越界报 6107，因为它总归是「清单写坏了」；
    /// 而 `external.program` 是**要被主进程 spawn 的路径**，越界意味着清单能把执行
    /// 导向包外的任意可执行文件，性质是路径越狱而不是字段格式错，所以单独成码。
    pub fn path_escape(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::PATH_OUT_OF_WORKSPACE,
            path: path.into(),
            message: message.into(),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "code": self.code.as_i32(),
            "path": self.path,
            "message": self.message,
        })
    }
}

/// `ErrorObject.data` 的载荷：`{ "details": [{code,path,message}, ...] }`。
pub fn details_json(errors: &[Violation]) -> Value {
    json!({
        "details": errors.iter().map(Violation::to_json).collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------------------
// 校验入口
// ---------------------------------------------------------------------------

/// 对已解析的清单 JSON 做三段校验。
///
/// `root` 是包目录：只用于判断 `main` 指向的文件是否存在（目录不存在时跳过，
/// 让「包不存在」由上层报更准确的错误）。除读元数据外不碰磁盘。
pub fn validate(root: &Path, raw: &Value) -> Result<ValidatedManifest, Vec<Violation>> {
    let Some(obj) = raw.as_object() else {
        // 连对象都不是，后续两段无从谈起，直接返回单条。
        return Err(vec![Violation::manifest("$", "清单必须是 JSON 对象")]);
    };

    let mut errors: Vec<Violation> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // ---------- 第一段：形状 ----------
    let id = required_str(obj, "id", &mut errors);
    let name = required_str(obj, "name", &mut errors);
    let version = required_str(obj, "version", &mut errors);
    let plugin_api = required_str(obj, "pluginApi", &mut errors);
    let main = required_str(obj, "main", &mut errors);
    let description = optional_str(obj, "description", &mut errors);
    let capabilities = string_array(obj, "capabilities", true, &mut errors);
    let subscriptions = string_array(obj, "subscriptions", false, &mut errors).unwrap_or_default();
    let timeout_ms = optional_timeout(obj, &mut errors);
    let allowed_domains = string_array(obj, "allowedDomains", false, &mut errors);
    let allowed_models = string_array(obj, "allowedModels", false, &mut errors);
    let max_tokens = optional_max_tokens(obj, &mut errors);
    let tools = parse_tools(obj, &mut errors);
    let commands = parse_commands(obj, &mut errors);
    let views = parse_views(obj, &mut errors);

    // ---------- 第二段：语义 ----------
    if let Some(id) = id.as_deref() {
        if !is_valid_id(id) {
            errors.push(Violation::manifest(
                "$.id",
                "id 必须匹配 ^[a-z0-9][a-z0-9-]{1,63}$（小写字母/数字/连字符，2–64 字符）",
            ));
        }
    }
    if let Some(name) = name.as_deref() {
        if name.trim().is_empty() {
            errors.push(Violation::manifest("$.name", "name 不能为空"));
        }
    }
    if let Some(version) = version.as_deref() {
        if !is_semver(version) {
            errors.push(Violation::manifest(
                "$.version",
                "version 必须是 x.y.z 形式的语义化版本（三段数字，不允许前导零）",
            ));
        }
    }
    if let Some(api) = plugin_api.as_deref() {
        match parse_plugin_api(api) {
            None => errors.push(Violation::manifest(
                "$.pluginApi",
                "pluginApi 必须形如 \"1.0\"（major.minor）",
            )),
            Some((major, minor)) => {
                if major != HOST_PLUGIN_API_MAJOR {
                    errors.push(Violation::api_mismatch(
                        "$.pluginApi",
                        format!(
                            "pluginApi {api} 与宿主 {HOST_PLUGIN_API_MAJOR}.{HOST_PLUGIN_API_MINOR} 不兼容：major 必须相等"
                        ),
                    ));
                } else if minor > HOST_PLUGIN_API_MINOR {
                    warnings.push(format!(
                        "pluginApi {api} 的 minor 高于宿主 {HOST_PLUGIN_API_MAJOR}.{HOST_PLUGIN_API_MINOR}，按宿主演进能力集降级运行"
                    ));
                }
            }
        }
    }

    // allowedDomains：归一化（小写、去尾点）后逐条判形状。
    // `None` 表示形状阶段已报「不是字符串数组」，不再级联。
    let mut normalized_domains: Vec<String> = Vec::new();
    if let Some(domains) = &allowed_domains {
        if domains.len() > MAX_ALLOWED_DOMAINS {
            errors.push(Violation::manifest(
                "$.allowedDomains",
                format!(
                    "allowedDomains 最多 {MAX_ALLOWED_DOMAINS} 条，收到 {} 条",
                    domains.len()
                ),
            ));
        }
        for (index, domain) in domains.iter().enumerate() {
            let path = format!("$.allowedDomains[{index}]");
            let normalized = domain.trim().trim_end_matches('.').to_ascii_lowercase();
            if !is_valid_domain(&normalized) {
                errors.push(Violation::manifest(
                    path,
                    format!(
                        "域名 {domain} 非法：只写主机名（可带 *. 前缀），不带 scheme / 端口 / 路径"
                    ),
                ));
            } else if !normalized_domains.contains(&normalized) {
                normalized_domains.push(normalized);
            }
        }
    }

    // allowedModels：`provider/model`，归一化（全小写）后逐条判形状。
    // 不做「斜杠切一刀就完事」的松散校验：`a/b/c`、空段、带空白的条目一律拒，
    // 否则白名单比对会出现「两个不同清单写法等价」的灰区。
    let mut normalized_models: Vec<String> = Vec::new();
    if let Some(models) = &allowed_models {
        if models.len() > MAX_ALLOWED_MODELS {
            errors.push(Violation::manifest(
                "$.allowedModels",
                format!(
                    "allowedModels 最多 {MAX_ALLOWED_MODELS} 条，收到 {} 条",
                    models.len()
                ),
            ));
        }
        for (index, entry) in models.iter().enumerate() {
            let path = format!("$.allowedModels[{index}]");
            let normalized = entry.trim().to_ascii_lowercase();
            if normalized.len() > MAX_MODEL_ENTRY_LEN {
                errors.push(Violation::manifest(
                    path,
                    format!("模型条目超过 {MAX_MODEL_ENTRY_LEN} 字符"),
                ));
            } else if !is_valid_model_entry(&normalized) {
                errors.push(Violation::manifest(
                    path,
                    format!("模型条目 {entry} 非法：应为 provider/model（各段非空，只含字母数字与 . _ -）"),
                ));
            } else if !normalized_models.contains(&normalized) {
                normalized_models.push(normalized);
            }
        }
    }

    // maxTokens：范围（0 视为「没声明」，由交叉校验决定算不算错）。
    if let Some(tokens) = max_tokens {
        if tokens == 0 || tokens > MAX_TOKENS_CEILING {
            errors.push(Violation::manifest(
                "$.maxTokens",
                format!("maxTokens 必须在 1–{MAX_TOKENS_CEILING} 之间，收到 {tokens}"),
            ));
        }
    }

    // main：先归一化，再判存在。归一化失败时不再做存在性判断——否则
    // `../../../evil.mjs` 会额外报一条「文件不存在」，掩盖真正的越界原因。
    let mut entry = None;
    if let Some(main) = main.as_deref() {
        match normalize_relative(main) {
            Err(reason) => errors.push(Violation::manifest("$.main", reason)),
            Ok(normalized) => {
                if root.is_dir() && !root.join(&normalized).is_file() {
                    errors.push(Violation::manifest(
                        "$.main",
                        format!("main 指向的文件不存在：{}", normalized),
                    ));
                }
                entry = Some(normalized);
            }
        }
    }

    // capabilities：白名单 + 去重。`None` 表示形状阶段已报错，不再级联。
    // 白名单不通过**不**影响第三段交叉校验：否则清单里混进一个 `shell.exec`，
    // 就能把「risk=write 却没申请 fs.write」这条矛盾一起藏掉，作者修完能力名
    // 再装一次才发现，等于把「一次报全」退回成挤牙膏。形状不合法（不是字符串数组）
    // 时仍跳过，避免一条类型错误生出两条交叉错误。
    let mut normalized_caps: Vec<String> = Vec::new();
    if let Some(caps) = &capabilities {
        for (index, cap) in caps.iter().enumerate() {
            let path = format!("$.capabilities[{index}]");
            if DENIED_CAPABILITIES.contains(&cap.as_str()) {
                errors.push(Violation::manifest(
                    path,
                    format!("能力 {cap} 属宿主拒绝清单：插件不得自行起进程"),
                ));
            } else if !ALLOWED_CAPABILITIES.contains(&cap.as_str()) {
                errors.push(Violation::manifest(
                    path,
                    format!(
                        "未知能力 {cap}；host 只提供 {}",
                        ALLOWED_CAPABILITIES.join(" / ")
                    ),
                ));
            } else if !normalized_caps.contains(cap) {
                normalized_caps.push(cap.clone());
            }
        }
    }

    for (index, tool) in tools.iter().enumerate() {
        if !is_valid_tool_name(&tool.name) {
            errors.push(Violation::manifest(
                format!("$.tools[{index}].name"),
                format!("工具名 {} 非法：须匹配 ^[a-z][a-z0-9_]{{1,40}}$", tool.name),
            ));
        }
        if tool.description.as_deref().unwrap_or("").trim().is_empty() {
            warnings.push(format!(
                "工具 {} 缺 description，模型无法判断何时调用",
                tool.name
            ));
        }
        if tool.path_field.is_some() {
            check_path_field(
                &mut errors,
                &format!("$.tools[{index}]"),
                tool.path_field.as_deref(),
                Some(&tool.parameters),
                "parameters",
            );
        }
    }

    for (index, command) in commands.iter().enumerate() {
        if !is_valid_tool_name(&command.name) {
            errors.push(Violation::manifest(
                format!("$.commands[{index}].name"),
                format!(
                    "命令名 {} 非法：须匹配 ^[a-z][a-z0-9_]{{1,40}}$",
                    command.name
                ),
            ));
        }
        if command.title.trim().is_empty() {
            errors.push(Violation::manifest(
                format!("$.commands[{index}].title"),
                "title 不能为空",
            ));
        }
        // 命令与工具同口径：pathField 必须对得上 args 里的 string 字段。
        if command.path_field.is_some() {
            check_path_field(
                &mut errors,
                &format!("$.commands[{index}]"),
                command.path_field.as_deref(),
                command.args.as_ref(),
                "args",
            );
        }
    }

    // views：形状已保证四个必填字段是「非空字符串」，这里判各自的取值域。
    // `entry` 与工具的对应关系放在第三段——它要读 `tools`，属于交叉。
    if views.len() > MAX_VIEWS {
        errors.push(Violation::manifest(
            "$.views",
            format!("views 最多 {MAX_VIEWS} 条，收到 {} 条", views.len()),
        ));
    }
    for (index, view) in views.iter().enumerate() {
        let base = format!("$.views[{index}]");
        if !is_valid_view_id(&view.id) {
            errors.push(Violation::manifest(
                format!("{base}.id"),
                format!(
                    "视图 id {} 非法：须匹配 ^[a-z][a-z0-9_-]{{0,31}}$（小写字母开头，1–32 字符）",
                    view.id
                ),
            ));
        }
        // 标题是给用户看的，长度按字符算而不是字节：中文标题 48 字节只有 16 个字。
        // 控制字符单独拒——它们会让面板 tab 标题串行、错位。
        let title_len = view.title.chars().count();
        if title_len > MAX_VIEW_TITLE_LEN {
            errors.push(Violation::manifest(
                format!("{base}.title"),
                format!("title 最多 {MAX_VIEW_TITLE_LEN} 字符，收到 {title_len} 字符"),
            ));
        }
        if view.title.chars().any(|ch| ch.is_control()) {
            errors.push(Violation::manifest(
                format!("{base}.title"),
                "title 不得包含控制字符",
            ));
        }
        if !VIEW_LOCATIONS.contains(&view.location.as_str()) {
            errors.push(Violation::manifest(
                format!("{base}.location"),
                format!(
                    "未知挂载位置 {}；首版只支持 {}",
                    view.location,
                    VIEW_LOCATIONS.join(" / ")
                ),
            ));
        }
        for (slot, event) in view.refresh.iter().enumerate() {
            if !VIEW_REFRESH_EVENTS.contains(&event.as_str()) {
                errors.push(Violation::manifest(
                    format!("{base}.refresh[{slot}]"),
                    format!(
                        "未知刷新事件 {event}；可选 {}",
                        VIEW_REFRESH_EVENTS.join(" / ")
                    ),
                ));
            }
        }

        let Some(external) = &view.external else {
            continue;
        };
        let external_base = format!("{base}.external");
        // program 是要被主进程 spawn 的路径，先过包根沙箱：越界直接 1002，
        // 与 `main` 一样不在归一化失败后再叠一条「文件不存在」。
        match normalize_package_path(&external.program, "external.program") {
            Err(reason) => errors.push(Violation::path_escape(
                format!("{external_base}.program"),
                reason,
            )),
            Ok(normalized) => {
                if !normalized
                    .to_ascii_lowercase()
                    .ends_with(VIEW_EXTERNAL_SUFFIX)
                {
                    errors.push(Violation::manifest(
                        format!("{external_base}.program"),
                        format!("可执行文件必须带 {VIEW_EXTERNAL_SUFFIX} 扩展名：{normalized}"),
                    ));
                } else if root.is_dir() && !root.join(&normalized).is_file() {
                    errors.push(Violation::manifest(
                        format!("{external_base}.program"),
                        format!("external.program 指向的文件不存在：{normalized}"),
                    ));
                }
            }
        }
        if !VIEW_EXTERNAL_PROTOCOLS.contains(&external.protocol.as_str()) {
            errors.push(Violation::manifest(
                format!("{external_base}.protocol"),
                format!(
                    "未知视口协议 {}；宿主只认 {}",
                    external.protocol,
                    VIEW_EXTERNAL_PROTOCOLS.join(" / ")
                ),
            ));
        }
    }

    // ---------- 第三段：交叉 ----------
    // 工具重名、与核心工具撞名、risk 与 capabilities 自相矛盾。
    let mut seen_tools: Vec<&str> = Vec::new();
    for (index, tool) in tools.iter().enumerate() {
        let path = format!("$.tools[{index}].name");
        if seen_tools.contains(&tool.name.as_str()) {
            errors.push(Violation::manifest(
                path.clone(),
                format!("工具名在包内重复：{}", tool.name),
            ));
        } else {
            seen_tools.push(tool.name.as_str());
        }
        if CORE_TOOL_NAMES.contains(&tool.name.as_str()) {
            errors.push(Violation::tool_conflict(
                path.clone(),
                format!(
                    "工具名 {} 与核心工具冲突；插件工具须以 {} 命名空间注册",
                    tool.name,
                    namespaced_tool(id.as_deref().unwrap_or("<id>"), &tool.name)
                ),
            ));
        }
        // 能力缺失只在 capabilities 形状合法时判，避免一条类型错误生出两条交叉错误；
        // 白名单不通过仍要判（见上），否则错一条能力就能把这条矛盾一起藏掉。
        // `exec` 例外：它压根没有落地档位，跟能力申请写得对不对无关，必须无条件拒。
        match tool.risk {
            PluginToolRisk::Read => {}
            PluginToolRisk::Write => {
                if capabilities.is_some()
                    && !normalized_caps.iter().any(|cap| cap == "fs.write")
                {
                    errors.push(Violation::manifest(
                        format!("$.tools[{index}].risk"),
                        format!(
                            "工具 {} 声明 risk=write 但未申请 fs.write——清单自相矛盾",
                            tool.name
                        ),
                    ));
                }
            }
            PluginToolRisk::Exec => {
                errors.push(Violation::manifest(
                    format!("$.tools[{index}].risk"),
                    format!(
                        "工具 {} 声明 risk=exec，而宿主不提供 shell.exec：该档位无法落地",
                        tool.name
                    ),
                ));
            }
        }
    }

    let mut seen_commands: Vec<&str> = Vec::new();
    for (index, command) in commands.iter().enumerate() {
        if seen_commands.contains(&command.name.as_str()) {
            errors.push(Violation::manifest(
                format!("$.commands[{index}].name"),
                format!("命令名在包内重复：{}", command.name),
            ));
        } else {
            seen_commands.push(command.name.as_str());
        }
        // 命令的 risk 与能力也必须对得上：宿主按清单自报的档位决定是否弹审批，
        // 若允许「声明 write 却不申请 fs.write」，命令就能靠回退档位绕过闸门。
        match command.risk {
            PluginToolRisk::Read => {}
            PluginToolRisk::Write => {
                if capabilities.is_some()
                    && !normalized_caps.iter().any(|cap| cap == "fs.write")
                {
                    errors.push(Violation::manifest(
                        format!("$.commands[{index}].risk"),
                        format!(
                            "命令 {} 声明 risk=write 但未申请 fs.write——清单自相矛盾",
                            command.name
                        ),
                    ));
                }
            }
            PluginToolRisk::Exec => {
                errors.push(Violation::manifest(
                    format!("$.commands[{index}].risk"),
                    format!(
                        "命令 {} 声明 risk=exec，而宿主不提供 shell.exec：该档位无法落地",
                        command.name
                    ),
                ));
            }
        }
    }

    // 视图 ↔ 工具：`entry` 必须落在本包 `tools[]` 里，而且是 `risk=read` 的那一个。
    //
    // 「未声明」用 6108 而不是 6107：对作者来说这等价于「引用了一个不存在的工具」，
    // 与工具撞名同属「指向了不该指的东西」；「指向写工具」才是清单自相矛盾（6107）。
    // `tools` 形状不合法时不判——parse_tools 此时返回空表，逐条判会生出一串
    // 「entry 不在 tools[] 中」的假错，把真正的类型错误埋掉。
    let tools_shaped = obj.get("tools").map_or(true, Value::is_array);
    let mut seen_views: Vec<&str> = Vec::new();
    for (index, view) in views.iter().enumerate() {
        let base = format!("$.views[{index}]");
        if seen_views.contains(&view.id.as_str()) {
            errors.push(Violation::manifest(
                format!("{base}.id"),
                format!("视图 id 在包内重复：{}", view.id),
            ));
        } else {
            seen_views.push(view.id.as_str());
        }
        if !tools_shaped {
            continue;
        }
        match tools.iter().find(|tool| tool.name == view.entry) {
            None => errors.push(Violation::tool_conflict(
                format!("{base}.entry"),
                format!(
                    "视图 {} 的 entry 工具 {} 未在本包 tools[] 中声明",
                    view.id, view.entry
                ),
            )),
            Some(tool) if !matches!(tool.risk, PluginToolRisk::Read) => {
                errors.push(Violation::manifest(
                    format!("{base}.entry"),
                    format!(
                        "视图 {} 的 entry 工具 {} 不是 risk=read：视图加载不得触发写盘或审批",
                        view.id, view.entry
                    ),
                ));
            }
            Some(_) => {}
        }
    }

    // 网络：能力与域名单必须成对出现。单写一边的清单一定是作者理解偏了——
    // 只申请 net.fetch 不写域名，运行期等于「永远拒」，装了也没用；
    // 只写域名不申请能力，则是「以为能联网」。两种都在装载前说清楚，别留到调用时报。
    if capabilities.is_some() {
        let wants_net = normalized_caps.iter().any(|cap| cap == "net.fetch");
        if wants_net && normalized_domains.is_empty() {
            errors.push(Violation::manifest(
                "$.allowedDomains",
                "申请了 net.fetch 却没声明 allowedDomains：出站网络没有可连的目标",
            ));
        } else if !wants_net && !normalized_domains.is_empty() {
            errors.push(Violation::manifest(
                "$.capabilities",
                "声明了 allowedDomains 但未申请 net.fetch：域名清单不会生效",
            ));
        }
    }

    // 模型：能力 / 模型名单 / maxTokens 三者必须成套。只申请 llm.chat 不写名单，
    // 运行期等于「永远拒」——装了也用不了，不如装载前说清楚；只写名单不申请能力，
    // 则是作者以为能调，实际一步都出不去。`maxTokens` 缺省同样意味着用不了。
    if capabilities.is_some() {
        let wants_llm = normalized_caps.iter().any(|cap| cap == "llm.chat");
        if wants_llm && normalized_models.is_empty() {
            errors.push(Violation::manifest(
                "$.allowedModels",
                "申请了 llm.chat 却没声明 allowedModels：没有任何模型被授权",
            ));
        } else if !wants_llm && !normalized_models.is_empty() {
            errors.push(Violation::manifest(
                "$.capabilities",
                "声明了 allowedModels 但未申请 llm.chat：模型名单不会生效",
            ));
        }
        if wants_llm && !matches!(max_tokens, Some(tokens) if tokens > 0) {
            errors.push(Violation::manifest(
                "$.maxTokens",
                "申请了 llm.chat 却没声明 maxTokens：输出长度没有上限",
            ));
        } else if !wants_llm && max_tokens.is_some() {
            errors.push(Violation::manifest(
                "$.capabilities",
                "声明了 maxTokens 但未申请 llm.chat：该上限不会生效",
            ));
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let manifest = Manifest {
        id: id.unwrap_or_default(),
        name: name.unwrap_or_default(),
        version: version.unwrap_or_default(),
        description,
        plugin_api: plugin_api.unwrap_or_default(),
        main: entry.clone().unwrap_or_default(),
        capabilities: normalized_caps,
        tools,
        commands,
        subscriptions,
        allowed_domains: normalized_domains,
        allowed_models: normalized_models,
        max_tokens: max_tokens.unwrap_or(0),
        timeout_ms: timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
        views,
    };

    Ok(ValidatedManifest {
        manifest,
        entry: entry.unwrap_or_default(),
        warnings,
    })
}

// ---------------------------------------------------------------------------
// 形状解析辅助
// ---------------------------------------------------------------------------

fn required_str(
    obj: &Map<String, Value>,
    field: &str,
    errors: &mut Vec<Violation>,
) -> Option<String> {
    let path = format!("$.{field}");
    match obj.get(field) {
        None | Some(Value::Null) => {
            errors.push(Violation::manifest(path, format!("缺少必填字段 {field}")));
            None
        }
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.clone()),
        Some(Value::String(_)) => {
            errors.push(Violation::manifest(path, format!("{field} 不能为空")));
            None
        }
        Some(_) => {
            errors.push(Violation::manifest(path, format!("{field} 必须是字符串")));
            None
        }
    }
}

fn optional_str(
    obj: &Map<String, Value>,
    field: &str,
    errors: &mut Vec<Violation>,
) -> Option<String> {
    match obj.get(field) {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => {
            errors.push(Violation::manifest(
                format!("$.{field}"),
                format!("{field} 必须是字符串"),
            ));
            None
        }
    }
}

/// 字符串数组字段。`required` 为真时缺失算错（`capabilities` 必填）。
fn string_array(
    obj: &Map<String, Value>,
    field: &str,
    required: bool,
    errors: &mut Vec<Violation>,
) -> Option<Vec<String>> {
    let path = format!("$.{field}");
    match obj.get(field) {
        None | Some(Value::Null) => {
            if required {
                errors.push(Violation::manifest(
                    path,
                    format!("缺少必填字段 {field}（应为字符串数组）"),
                ));
            }
            None
        }
        Some(Value::Array(items)) => {
            let mut values = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                match item {
                    Value::String(value) => values.push(value.clone()),
                    _ => errors.push(Violation::manifest(
                        format!("{path}[{index}]"),
                        format!("{field} 的每一项都必须是字符串"),
                    )),
                }
            }
            Some(values)
        }
        Some(_) => {
            errors.push(Violation::manifest(
                path,
                format!("{field} 必须是字符串数组"),
            ));
            None
        }
    }
}

/// 域名形状校验：允许 `example.com` / `api.example.com` / `*.example.com`。
///
/// 只做形状，不做「是不是公网」——那要解析 DNS，属于运行期的活（`plugin_net`）。
/// 这里挡住的是「写错格式」这一类：`https://a.com/x`、`a.com:8443`、`a_b.com`。
fn is_valid_domain(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_DOMAIN_LEN {
        return false;
    }
    let body = value.strip_prefix("*.").unwrap_or(value);
    if body.is_empty() || !body.contains('.') {
        return false;
    }
    let labels: Vec<&str> = body.split('.').collect();
    let Some(last) = labels.last() else {
        return false;
    };
    // 顶级标签必须带字母：`1.2.3.4` 这种 IP 字面量留给运行期地址闸门判，
    // 清单里不鼓励写 IP（写了也放行公网 IP，但不做特殊处理）。
    if !last.chars().any(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    labels.iter().all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// `maxTokens` 的形状解析：只认正整数。范围判定留给语义段（那边才有
/// `MAX_TOKENS_CEILING` 的上下文），这里只保证「不是数字」能报准。
fn optional_max_tokens(obj: &Map<String, Value>, errors: &mut Vec<Violation>) -> Option<u64> {
    match obj.get("maxTokens") {
        None | Some(Value::Null) => None,
        Some(Value::Number(number)) => match number.as_u64() {
            Some(value) => Some(value),
            None => {
                errors.push(Violation::manifest(
                    "$.maxTokens",
                    "maxTokens 必须是非负整数",
                ));
                None
            }
        },
        Some(_) => {
            errors.push(Violation::manifest(
                "$.maxTokens",
                "maxTokens 必须是整数（不是字符串或小数）",
            ));
            None
        }
    }
}

/// 模型条目形状：`provider/model`，两段都非空，只允许 `[a-z0-9._-]`。
///
/// 斜杠恰好一个：多个斜杠意味着作者把 `vendor/provider/model` 当条目写，
/// 那种条目运行期永远匹配不上白名单，装载前就该拦。
fn is_valid_model_entry(entry: &str) -> bool {
    let mut parts = entry.split('/');
    let (Some(provider), Some(model), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let ok = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    };
    ok(provider) && ok(model)
}

fn optional_timeout(obj: &Map<String, Value>, errors: &mut Vec<Violation>) -> Option<u64> {
    match obj.get("timeoutMs") {
        None | Some(Value::Null) => None,
        Some(Value::Number(number)) => match number.as_u64() {
            Some(value) if (MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&value) => Some(value),
            Some(value) => {
                errors.push(Violation::manifest(
                    "$.timeoutMs",
                    format!(
                        "timeoutMs={value} 越界，允许范围 {MIN_TIMEOUT_MS}–{MAX_TIMEOUT_MS} 毫秒"
                    ),
                ));
                None
            }
            None => {
                errors.push(Violation::manifest(
                    "$.timeoutMs",
                    "timeoutMs 必须是非负整数（毫秒）",
                ));
                None
            }
        },
        Some(_) => {
            errors.push(Violation::manifest(
                "$.timeoutMs",
                "timeoutMs 必须是整数（毫秒）",
            ));
            None
        }
    }
}

/// 解析 `risk` 字段（工具与命令共用一套口径）。
///
/// 返回 `None` 表示已记一条清单错误、调用方应当跳过该项——工具与命令对非法
/// `risk` 的处理必须一致，否则同一个包里的两种声明会给出不同的宽容度。
fn parse_risk(
    item: &Map<String, Value>,
    base: &str,
    errors: &mut Vec<Violation>,
) -> Option<PluginToolRisk> {
    match item.get("risk") {
        None | Some(Value::Null) => Some(PluginToolRisk::Read),
        Some(Value::String(raw)) => match raw.as_str() {
            "read" => Some(PluginToolRisk::Read),
            "write" => Some(PluginToolRisk::Write),
            "exec" => Some(PluginToolRisk::Exec),
            other => {
                errors.push(Violation::manifest(
                    format!("{base}.risk"),
                    format!("risk 只能是 read / write / exec，收到 {other}"),
                ));
                None
            }
        },
        Some(_) => {
            errors.push(Violation::manifest(
                format!("{base}.risk"),
                "risk 必须是字符串",
            ));
            None
        }
    }
}

fn parse_tools(obj: &Map<String, Value>, errors: &mut Vec<Violation>) -> Vec<ManifestTool> {
    let Some(value) = obj.get("tools") else {
        return Vec::new();
    };
    let Value::Array(items) = value else {
        errors.push(Violation::manifest("$.tools", "tools 必须是数组"));
        return Vec::new();
    };

    let mut tools = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let base = format!("$.tools[{index}]");
        let Some(tool) = item.as_object() else {
            errors.push(Violation::manifest(base, "tools[] 每项必须是对象"));
            continue;
        };
        let name = match tool.get("name") {
            Some(Value::String(name)) if !name.trim().is_empty() => name.clone(),
            Some(Value::String(_)) => {
                errors.push(Violation::manifest(format!("{base}.name"), "name 不能为空"));
                continue;
            }
            Some(_) => {
                errors.push(Violation::manifest(
                    format!("{base}.name"),
                    "name 必须是字符串",
                ));
                continue;
            }
            None => {
                errors.push(Violation::manifest(
                    format!("{base}.name"),
                    "缺少必填字段 name",
                ));
                continue;
            }
        };

        let Some(risk) = parse_risk(tool, &base, errors) else {
            continue;
        };

        // parameters 必须是 `{ "type": "object", ... }`：模型侧只接受对象入参，
        // 一个 `type: "string"` 的 schema 会让工具调用在参数拼装上就崩。
        let parameters = match tool.get("parameters") {
            Some(Value::Object(schema)) if schema.get("type") == Some(&json!("object")) => {
                Value::Object(schema.clone())
            }
            _ => {
                errors.push(Violation::manifest(
                    format!("{base}.parameters"),
                    "parameters 必须是合法 JSON Schema 子集，且 type 为 \"object\"",
                ));
                continue;
            }
        };

        let description = match tool.get("description") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(text.clone()),
            Some(_) => {
                errors.push(Violation::manifest(
                    format!("{base}.description"),
                    "description 必须是字符串",
                ));
                None
            }
        };

        let path_field = match tool.get("pathField") {
            None | Some(Value::Null) => None,
            Some(Value::String(field)) => Some(field.clone()),
            Some(_) => {
                errors.push(Violation::manifest(
                    format!("{base}.pathField"),
                    "pathField 必须是字符串",
                ));
                None
            }
        };

        tools.push(ManifestTool {
            name,
            description,
            risk,
            path_field,
            parameters,
        });
    }
    tools
}

/// 命令声明。`name` 与 demo 的 `id` 都接受：demo 包用的是 `id`，
/// 而协议 DTO 暴露的是 `name`——两者指向同一个东西，不兼容只会让作者困惑。
fn parse_commands(obj: &Map<String, Value>, errors: &mut Vec<Violation>) -> Vec<ManifestCommand> {
    let Some(value) = obj.get("commands") else {
        return Vec::new();
    };
    let Value::Array(items) = value else {
        errors.push(Violation::manifest("$.commands", "commands 必须是数组"));
        return Vec::new();
    };

    let mut commands = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let base = format!("$.commands[{index}]");
        let Some(command) = item.as_object() else {
            errors.push(Violation::manifest(base, "commands[] 每项必须是对象"));
            continue;
        };
        let name = ["name", "id"]
            .iter()
            .find_map(|key| match command.get(*key) {
                Some(Value::String(value)) if !value.trim().is_empty() => Some(value.clone()),
                _ => None,
            });
        let Some(name) = name else {
            errors.push(Violation::manifest(
                format!("{base}.name"),
                "命令缺少非空 name（或 id）",
            ));
            continue;
        };

        let title = match command.get("title") {
            Some(Value::String(title)) => title.clone(),
            None | Some(Value::Null) => name.clone(),
            Some(_) => {
                errors.push(Violation::manifest(
                    format!("{base}.title"),
                    "title 必须是字符串",
                ));
                name.clone()
            }
        };
        let description = match command.get("description") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(text.clone()),
            Some(_) => {
                errors.push(Violation::manifest(
                    format!("{base}.description"),
                    "description 必须是字符串",
                ));
                None
            }
        };

        // 命令与工具同一套 risk 口径：`write` 会在执行前弹审批（3001）。
        // 命令没有 `exec` 档位——宿主不提供 `shell.exec`，写 `exec` 的命令
        // 无法落地，交叉校验阶段会无条件把它当清单错误报出来（见 `validate` 第三段）。
        let Some(risk) = parse_risk(command, &base, errors) else {
            continue;
        };

        // `args` 是**可选**的：命令不带参数是最常见形态（"重载配置"、"清缓存"）。
        // 声明了就必须是 `type: "object"` 的 schema——与工具 side 一致，因为
        // 校验器只吃对象入参；给了别的形状，命令会在执行时永远 -32602。
        let args = match command.get("args") {
            None | Some(Value::Null) => None,
            Some(Value::Object(schema)) if schema.get("type") == Some(&json!("object")) => {
                Some(Value::Object(schema.clone()))
            }
            Some(_) => {
                errors.push(Violation::manifest(
                    format!("{base}.args"),
                    "args 必须是合法 JSON Schema 子集，且 type 为 \"object\"",
                ));
                continue;
            }
        };

        let path_field = match command.get("pathField") {
            None | Some(Value::Null) => None,
            Some(Value::String(field)) if !field.trim().is_empty() => Some(field.clone()),
            Some(_) => {
                errors.push(Violation::manifest(
                    format!("{base}.pathField"),
                    "pathField 必须是非空字符串",
                ));
                None
            }
        };

        commands.push(ManifestCommand {
            name,
            title,
            description,
            risk,
            path_field,
            args,
        });
    }
    commands
}

/// `pathField` 与入参 schema 的对应关系（工具与命令同一套口径）。
///
/// 路径闸门在宿主里靠「按这个名字从入参里取值」定位目标文件：名字写错，
/// 或声明了却是个 number，闸门既取不到路径也无法规范化，只能退化成
/// 「看不见路径的写」——这正是 §8 要堵的静默降级，所以必须在安装期就拒。
///
/// `schema_label` 只影响报错文案：工具侧是 `parameters`，命令侧是 `args`。
fn check_path_field(
    errors: &mut Vec<Violation>,
    base: &str,
    declared: Option<&str>,
    schema: Option<&Value>,
    schema_label: &str,
) {
    let Some(field) = declared else {
        return;
    };
    let path = format!("{base}.pathField");
    if field.trim().is_empty() {
        errors.push(Violation::manifest(path, "pathField 不能为空字符串"));
        return;
    }
    // 命令的 `args` 是可选的：声明了 pathField 却没给入参 schema、
    // 或给的 schema 被形状段判掉，都意味着闸门取不到这个字段。
    let Some(Value::Object(schema)) = schema else {
        errors.push(Violation::manifest(
            path,
            format!(
                "声明了 pathField = {field}，但缺少 {schema_label}.properties —— 闸门取不到该路径参数"
            ),
        ));
        return;
    };
    let Some(property) = schema
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|properties| properties.get(field))
    else {
        errors.push(Violation::manifest(
            path,
            format!("pathField = {field} 不在 {schema_label}.properties 中 —— 闸门取不到该路径参数"),
        ));
        return;
    };
    let Value::Object(property) = property else {
        errors.push(Violation::manifest(
            path,
            format!("{schema_label}.properties.{field} 必须是对象（JSON Schema 子集）"),
        ));
        return;
    };
    // 只认 `type: "string"`：`type` 缺省或写成 `["string","null"]` 都不放行，
    // 宁可让作者补一个明确的 string，也不要在闸门里做隐式兼容。
    if property.get("type").and_then(Value::as_str) != Some("string") {
        errors.push(Violation::manifest(
            path,
            format!(
                "pathField = {field} 的入参类型必须是 string，收到 {}",
                property
                    .get("type")
                    .map_or_else(|| "未声明 type".to_string(), |ty| format!("{ty}"))
            ),
        ));
    }
}

/// 视图声明的形状解析（docs/24 §4.1）。
///
/// 四个必填字段缺任何一个就**整条丢弃**，不推入半成品：语义段与交叉段都会再看
/// 这些字段，留一条 id 为空的残项只会派生出「id 非法」「entry 不在 tools」这类
/// 由同一个根因引起的假错。这条取舍与 `parse_commands` 里 `risk` 缺失即 `continue`
/// 同源。
fn parse_views(obj: &Map<String, Value>, errors: &mut Vec<Violation>) -> Vec<ManifestView> {
    let Some(value) = obj.get("views") else {
        return Vec::new();
    };
    let Value::Array(items) = value else {
        errors.push(Violation::manifest("$.views", "views 必须是数组"));
        return Vec::new();
    };

    let mut views = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let base = format!("$.views[{index}]");
        let Some(view) = item.as_object() else {
            errors.push(Violation::manifest(base, "views[] 每项必须是对象"));
            continue;
        };

        let id = required_view_str(view, &base, "id", errors);
        let title = required_view_str(view, &base, "title", errors);
        let location = required_view_str(view, &base, "location", errors);
        let entry = required_view_str(view, &base, "entry", errors);
        // 缺字段时 `required_view_str` 已经报过，这里只是不继续装配。
        let (Some(id), Some(title), Some(location), Some(entry)) = (id, title, location, entry)
        else {
            continue;
        };

        // `refresh` 可选；写了的每一项都必须是字符串，非字符串逐条报而不是整段丢弃，
        // 否则一个笔误会让「事件名写错」这条更重要的信息一起消失。
        let refresh = match view.get("refresh") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(events)) => {
                let mut collected = Vec::with_capacity(events.len());
                for (slot, event) in events.iter().enumerate() {
                    match event {
                        Value::String(name) => collected.push(name.clone()),
                        _ => errors.push(Violation::manifest(
                            format!("{base}.refresh[{slot}]"),
                            "refresh 的每一项都必须是字符串",
                        )),
                    }
                }
                collected
            }
            Some(_) => {
                errors.push(Violation::manifest(
                    format!("{base}.refresh"),
                    "refresh 必须是字符串数组",
                ));
                Vec::new()
            }
        };

        // `external` 整体可选，但出现即要求 `program` 与 `protocol` 都在：
        // 只给其中一个的配置无论缺哪边都跑不通（没 program 无处 spawn，
        // 没 protocol 连上也不知道怎么说话），早点说清楚比运行期连不上强。
        let external_base = format!("{base}.external");
        let external = match view.get("external") {
            None | Some(Value::Null) => None,
            Some(Value::Object(section)) => {
                let program = required_view_str(section, &external_base, "program", errors);
                let protocol = required_view_str(section, &external_base, "protocol", errors);
                match (program, protocol) {
                    (Some(program), Some(protocol)) => Some(ManifestExternal { program, protocol }),
                    _ => None,
                }
            }
            Some(_) => {
                errors.push(Violation::manifest(
                    external_base,
                    "external 必须是对象（含 program 与 protocol）",
                ));
                None
            }
        };

        views.push(ManifestView {
            id,
            title,
            location,
            entry,
            refresh,
            external,
        });
    }
    views
}

/// 视图段内的必填字符串字段：`required_str` 只处理顶层字段，这里要带下标路径。
fn required_view_str(
    section: &Map<String, Value>,
    base: &str,
    key: &str,
    errors: &mut Vec<Violation>,
) -> Option<String> {
    let path = format!("{base}.{key}");
    match section.get(key) {
        None | Some(Value::Null) => {
            errors.push(Violation::manifest(path, format!("缺少必填字段 {key}")));
            None
        }
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.clone()),
        Some(Value::String(_)) => {
            errors.push(Violation::manifest(path, format!("{key} 不能为空")));
            None
        }
        Some(_) => {
            errors.push(Violation::manifest(path, format!("{key} 必须是字符串")));
            None
        }
    }
}

// ---------------------------------------------------------------------------
// 语义判断（手写，不引正则：三个小函数比多一个依赖划算）
// ---------------------------------------------------------------------------

/// 视图 id：`^[a-z][a-z0-9_-]{0,31}$`（docs/24 §4.1 字段表）。
///
/// 不复用 [`is_valid_id`]：插件 id 允许首字符是数字、不允许下划线，视图 id 反过来。
/// 两者形状相近但契约不同，共用一套宽松判定只会让其中一边的约束名存实亡。
pub fn is_valid_view_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if !(1..=32).contains(&bytes.len()) {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes.iter().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_' || *byte == b'-'
    })
}

/// `^[a-z0-9][a-z0-9-]{1,63}$`。
pub fn is_valid_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if !(2..=64).contains(&bytes.len()) {
        return false;
    }
    let first = bytes[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

/// `^[a-z][a-z0-9_]{1,40}$`：工具名与命令名共用。
pub fn is_valid_tool_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    if !(2..=41).contains(&bytes.len()) {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

/// 严格 `x.y.z`：三段十进制、不允许前导零。
pub fn is_semver(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts.iter().all(|part| {
        !part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && (part == &"0" || !part.starts_with('0'))
    })
}

/// 解析 `major.minor`；多余段或非数字一律 `None`。
pub fn parse_plugin_api(value: &str) -> Option<(u32, u32)> {
    let mut parts = value.split('.');
    let major = parse_api_part(parts.next()?)?;
    let minor = parse_api_part(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor))
}

fn parse_api_part(part: &str) -> Option<u32> {
    if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if part.len() > 1 && part.starts_with('0') {
        return None;
    }
    part.parse().ok()
}

/// 归一化 `main`：拒绝绝对路径、盘符、UNC 与 `..`，返回 `/` 分隔的相对路径。
///
/// 这里是「包根沙箱」的第一道闸门：安装时还要用 canonicalize 复核一次，
/// 两道都过才认为入口不会逃出插件目录（防的是软链接把 `main` 指到包外）。
pub fn normalize_relative(raw: &str) -> Result<String, String> {
    normalize_package_path(raw, "main")
}

/// `main` 与 `external.program` 共用的包内相对路径归一化；`field` 只影响错误文案。
///
/// 两条路径的沙箱规则必须**逐字相同**——`external.program` 会被主进程 spawn，
/// 漏一条就等于给清单留了一个「指向包外任意 exe」的口子。
fn normalize_package_path(raw: &str, field: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("{field} 不能为空"));
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(format!("{field} 必须是包内相对路径，不得为绝对路径：{raw}"));
    }

    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(segment) => parts.push(segment.to_string_lossy().into_owned()),
            Component::ParentDir => {
                return Err(format!("{field} 不得包含 ..（会越出插件包根）：{raw}"));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "{field} 必须是包内相对路径，不得带盘符 / 根目录前缀：{raw}"
                ));
            }
        }
    }

    if parts.is_empty() {
        return Err(format!("{field} 归一化后为空：{raw}"));
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// 建一个只含 `index.mjs` 的临时包目录（`main` 存在性检查要用）。
    fn temp_package(tag: &str) -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ai-ide-plugin-manifest-{tag}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时包目录");
        std::fs::write(dir.join("index.mjs"), "// demo\n").expect("写入口文件");
        dir
    }

    fn legal_manifest() -> Value {
        json!({
            "id": "demo-hello",
            "name": "Demo Hello",
            "version": "1.0.0",
            "description": "演示插件",
            "pluginApi": "1.0",
            "main": "index.mjs",
            "capabilities": ["fs.read", "fs.write", "search"],
            "tools": [
                {
                    "name": "repo_stats",
                    "description": "统计工作区",
                    "risk": "read",
                    "parameters": { "type": "object", "properties": {} }
                },
                {
                    "name": "write_note",
                    "description": "写笔记",
                    "risk": "write",
                    "pathField": "path",
                    "parameters": {
                        "type": "object",
                        "properties": { "path": { "type": "string" } }
                    }
                }
            ],
            "commands": [{ "id": "greet", "title": "打个招呼" }],
            "subscriptions": ["workspace.changed"]
        })
    }

    /// 合法清单：全部字段都被接受，且规范化结果可序列化回 JSON。
    #[test]
    fn accepts_legal_manifest() {
        let root = temp_package("legal");
        let validated = validate(&root, &legal_manifest()).expect("合法清单不应报错");
        assert_eq!(validated.manifest.id, "demo-hello");
        assert_eq!(validated.manifest.timeout_ms, DEFAULT_TIMEOUT_MS);
        assert_eq!(validated.entry, "index.mjs");
        assert_eq!(
            validated.manifest.tools[1].risk,
            PluginToolRisk::Write,
            "write 工具的 risk 应被解析成枚举"
        );
        assert_eq!(validated.manifest.commands[0].name, "greet");
        assert!(validated.warnings.is_empty(), "合法清单不该有告警");

        let json = validated.to_manifest_json();
        assert!(
            json.contains("\"pluginApi\":\"1.0\""),
            "落库 JSON 用 camelCase"
        );
        assert_eq!(
            validated.namespaced_tool_names(),
            vec![
                "plugin__demo-hello__repo_stats".to_string(),
                "plugin__demo-hello__write_note".to_string()
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `progress` 在能力白名单里（`host/progress` 的闸门靠这条才能过）。
    #[test]
    fn progress_capability_is_allowlisted() {
        let root = temp_package("progress");
        let mut raw = legal_manifest();
        raw["capabilities"] = json!(["fs.read", "fs.write", "progress"]);
        let validated = validate(&root, &raw).expect("progress 应在白名单内");
        assert!(validated
            .manifest
            .capabilities
            .contains(&"progress".to_string()));
        // 拼错一个字母仍然是拒载，而不是「差不多就行」。
        raw["capabilities"] = json!(["progress.report"]);
        let errors = validate(&root, &raw).expect_err("未登记的能力名必须拒载");
        assert_eq!(errors[0].path, "$.capabilities[0]");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 六类非法清单必须**一次报全**，而不是逐条挤牙膏。
    #[test]
    fn reports_all_six_classes_of_invalid_manifest_in_one_pass() {
        let root = temp_package("six");
        let broken = json!({
            // ① id 含大写与下划线
            "id": "Demo_Bad",
            "name": "",
            // ② semver 只有两段
            "version": "1.0",
            "pluginApi": "1.0",
            // ③ main 逃出包根
            "main": "../../../evil.mjs",
            "capabilities": ["fs.read", "shell.exec"],
            "tools": [
                {
                    // ④ risk=write 却没申请 fs.write
                    "name": "evil_write",
                    "description": "越权写盘",
                    "risk": "write",
                    "parameters": { "type": "object", "properties": {} }
                }
            ],
            "commands": []
        });
        let errors = validate(&root, &broken).expect_err("非法清单必须被拒");
        let paths: Vec<&str> = errors.iter().map(|error| error.path.as_str()).collect();

        assert!(paths.contains(&"$.id"), "id 格式错误未报出：{paths:?}");
        assert!(paths.contains(&"$.name"), "空 name 未报出：{paths:?}");
        assert!(paths.contains(&"$.version"), "semver 错误未报出：{paths:?}");
        assert!(paths.contains(&"$.main"), "main 越界未报出：{paths:?}");
        assert!(
            paths.contains(&"$.capabilities[1]"),
            "shell.exec 未报出：{paths:?}"
        );
        assert!(
            paths.contains(&"$.tools[0].risk"),
            "risk 与能力矛盾未报出：{paths:?}"
        );
        assert_eq!(errors.len(), 6, "六类错误应各报一条：{errors:?}");
        // 每一条都自带细分码，6107 之外只有 6104 会出现在语义段。
        assert!(errors
            .iter()
            .all(|error| error.code == ErrorCode::PLUGIN_MANIFEST_INVALID));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 命令没有 `exec` 档位：声明了就在安装期硬拒，而不是回退成 read 静默放行。
    #[test]
    fn command_exec_risk_is_rejected_at_install() {
        let root = temp_package("cmd-exec");
        let mut raw = legal_manifest();
        raw["commands"] = json!([
            { "name": "greet", "title": "打个招呼" },
            { "name": "run_shell", "title": "起进程", "risk": "exec" }
        ]);
        let errors = validate(&root, &raw).expect_err("exec 命令必须拒装");
        let paths: Vec<&str> = errors.iter().map(|error| error.path.as_str()).collect();
        assert!(
            paths.contains(&"$.commands[1].risk"),
            "exec 命令未报出：{paths:?}"
        );
        assert!(
            errors.iter().any(|error| {
                error.path == "$.commands[1].risk" && error.message.contains("shell.exec")
            }),
            "报错文案应点明宿主不提供 shell.exec：{errors:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 命令的 `risk=write` 必须与 `fs.write` 成对：缺了就是清单自相矛盾。
    #[test]
    fn command_write_requires_fs_write_capability() {
        let root = temp_package("cmd-write");
        let mut raw = legal_manifest();
        raw["capabilities"] = json!(["fs.read"]);
        raw["commands"] = json!([
            { "name": "greet", "title": "打个招呼" },
            { "name": "save_all", "title": "全部保存", "risk": "write" }
        ]);
        let errors = validate(&root, &raw).expect_err("write 命令缺 fs.write 必须拒装");
        let paths: Vec<&str> = errors.iter().map(|error| error.path.as_str()).collect();
        assert!(
            paths.contains(&"$.commands[1].risk"),
            "命令 risk 与能力矛盾未报出：{paths:?}"
        );

        // 补上 fs.write 就该过——同一条命令不该因为别的原因再红。
        raw["capabilities"] = json!(["fs.read", "fs.write"]);
        validate(&root, &raw).expect("申请 fs.write 后 write 命令应通过");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `pathField` 必须真的指向 `args.properties` 里的 string 字段。
    #[test]
    fn command_path_field_must_match_args_schema() {
        let root = temp_package("cmd-path");

        // ① 字段不在 properties 里：闸门取不到路径。
        let mut missing = legal_manifest();
        missing["commands"] = json!([{
            "name": "open", "title": "打开", "risk": "read", "pathField": "path",
            "args": { "type": "object", "properties": { "target": { "type": "string" } } }
        }]);
        let errors = validate(&root, &missing).expect_err("pathField 指向不存在的字段必须拒装");
        assert!(
            errors.iter().any(|error| error.path == "$.commands[0].pathField"),
            "字段不存在未报出：{errors:?}"
        );

        // ② 字段在，但类型不是 string。
        let mut wrong_type = legal_manifest();
        wrong_type["commands"] = json!([{
            "name": "open", "title": "打开", "risk": "read", "pathField": "path",
            "args": { "type": "object", "properties": { "path": { "type": "integer" } } }
        }]);
        let errors = validate(&root, &wrong_type).expect_err("pathField 类型不是 string 必须拒装");
        assert!(
            errors.iter().any(|error| {
                error.path == "$.commands[0].pathField" && error.message.contains("string")
            }),
            "类型错误未报出：{errors:?}"
        );

        // ③ 声明了 pathField 却完全没给 args。
        let mut no_args = legal_manifest();
        no_args["commands"] = json!([{
            "name": "open", "title": "打开", "risk": "read", "pathField": "path"
        }]);
        let errors = validate(&root, &no_args).expect_err("有 pathField 无 args 必须拒装");
        assert!(
            errors
                .iter()
                .any(|error| error.path == "$.commands[0].pathField"),
            "缺 args 未报出：{errors:?}"
        );

        // ④ 对得上就放行，且解析结果能原样带出。
        let mut ok = legal_manifest();
        ok["commands"] = json!([{
            "name": "open", "title": "打开", "risk": "read", "pathField": "path",
            "args": { "type": "object", "properties": { "path": { "type": "string" } } }
        }]);
        let validated = validate(&root, &ok).expect("pathField 合法应通过");
        assert_eq!(
            validated.manifest.commands[0].path_field.as_deref(),
            Some("path")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 工具侧同一条口径：`pathField` 也必须落在 `parameters.properties` 且为 string。
    #[test]
    fn tool_path_field_must_match_parameters_schema() {
        let root = temp_package("tool-path");
        let mut raw = legal_manifest();
        raw["tools"][1]["parameters"] = json!({
            "type": "object",
            "properties": { "path": { "type": "integer" } }
        });
        let errors = validate(&root, &raw).expect_err("工具 pathField 类型不符必须拒装");
        assert!(
            errors
                .iter()
                .any(|error| error.path == "$.tools[1].pathField"),
            "工具侧 pathField 未校验：{errors:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `pluginApi` major 不等必须单独用 6104，且顶层仍是一次报全的 6107。
    #[test]
    fn plugin_api_major_mismatch_uses_6104() {
        let root = temp_package("api");
        let mut raw = legal_manifest();
        raw["pluginApi"] = json!("2.0");
        let errors = validate(&root, &raw).expect_err("major 不等必须拒载");
        assert_eq!(errors.len(), 1, "只有一条问题：{errors:?}");
        assert_eq!(errors[0].code, ErrorCode::PLUGIN_API_MISMATCH);
        assert_eq!(errors[0].code.as_i32(), 6104);
        assert_eq!(errors[0].path, "$.pluginApi");
        let details = details_json(&errors);
        assert_eq!(details["details"][0]["code"], json!(6104));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 工具名与核心工具撞名用 6108，包内重名用 6107——两者处置方式不同。
    #[test]
    fn duplicate_and_core_tool_names_are_distinguished() {
        let root = temp_package("tools");
        let mut raw = legal_manifest();
        raw["tools"] = json!([
            {
                "name": "read",
                "description": "撞核心工具",
                "risk": "read",
                "parameters": { "type": "object", "properties": {} }
            },
            {
                "name": "same_name",
                "description": "第一份",
                "risk": "read",
                "parameters": { "type": "object", "properties": {} }
            },
            {
                "name": "same_name",
                "description": "第二份",
                "risk": "read",
                "parameters": { "type": "object", "properties": {} }
            }
        ]);
        let errors = validate(&root, &raw).expect_err("撞名必须拒载");
        assert!(errors.iter().any(|error| {
            error.code == ErrorCode::PLUGIN_TOOL_CONFLICT && error.path == "$.tools[0].name"
        }));
        assert!(errors.iter().any(|error| {
            error.code == ErrorCode::PLUGIN_MANIFEST_INVALID && error.path == "$.tools[2].name"
        }));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `main` 的四种越界写法都要被拒，且理由指向越界本身。
    #[test]
    fn main_rejects_absolute_drive_unc_and_parent_escape() {
        for raw in [
            "../../../evil.mjs",
            "sub/../../evil.mjs",
            "C:/Windows/evil.mjs",
            "C:evil.mjs",
            "/etc/evil.mjs",
            "\\\\server\\share\\evil.mjs",
            "//server/share/evil.mjs",
        ] {
            assert!(
                normalize_relative(raw).is_err(),
                "{raw} 应被判定为越界或绝对路径"
            );
        }
        assert_eq!(
            normalize_relative("./sub/./index.mjs").unwrap(),
            "sub/index.mjs",
            "CurDir 应被归一化掉而不是原样保留"
        );
    }

    /// `main` 指向的文件不存在时要单独报错：这属于「包不完整」，与路径越界不同。
    #[test]
    fn main_must_point_to_existing_file() {
        let root = temp_package("missing-main");
        let mut raw = legal_manifest();
        raw["main"] = json!("dist/index.mjs");
        let errors = validate(&root, &raw).expect_err("入口缺失必须拒载");
        assert!(errors
            .iter()
            .any(|error| error.path == "$.main" && error.message.contains("不存在")));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 形状阶段报错时不级联出交叉错误（`capabilities` 类型错不该再报 risk 矛盾）。
    #[test]
    fn shape_errors_do_not_cascade_into_cross_checks() {
        let root = temp_package("cascade");
        let mut raw = legal_manifest();
        raw["capabilities"] = json!("fs.write");
        let errors = validate(&root, &raw).expect_err("capabilities 必须是数组");
        assert_eq!(errors.len(), 1, "只应有一条形状错误：{errors:?}");
        assert_eq!(errors[0].path, "$.capabilities");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `timeoutMs` 越界与工具名格式错误都要落到具体路径上。
    #[test]
    fn timeout_and_tool_name_bounds_are_enforced() {
        let root = temp_package("bounds");
        let mut raw = legal_manifest();
        raw["timeoutMs"] = json!(999);
        let errors = validate(&root, &raw).expect_err("timeout 越界必须拒载");
        assert!(errors.iter().any(|error| error.path == "$.timeoutMs"));

        let mut raw = legal_manifest();
        raw["timeoutMs"] = json!(MAX_TIMEOUT_MS);
        assert!(validate(&root, &raw).is_ok(), "上限值本身应被接受");

        let mut raw = legal_manifest();
        raw["tools"][0]["name"] = json!("Repo-Stats");
        let errors = validate(&root, &raw).expect_err("工具名大写必须拒载");
        assert!(errors.iter().any(|error| error.path == "$.tools[0].name"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 非对象清单只报一条：没有字段可谈，硬凑多条只会误导。
    #[test]
    fn non_object_manifest_reports_single_error() {
        let root = temp_package("shape");
        let errors = validate(&root, &json!([])).expect_err("数组不是清单");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path, "$");
        let _ = std::fs::remove_dir_all(&root);
    }
    /// `allowedDomains` 归一化：大小写与结尾的点都折成同一份，重复项只留一条。
    ///
    /// 不归一化的话，清单里同时写 `API.Example.com.` 与 `api.example.com`，
    /// 运行期就是「两个域名」但实际是同一台主机——审查的人会以为授权面更大。
    #[test]
    fn allowed_domains_are_normalized_and_deduped() {
        let root = temp_package("domains");
        let mut raw = legal_manifest();
        raw["capabilities"] = json!(["fs.read", "fs.write", "net.fetch"]);
        raw["allowedDomains"] = json!(["  API.Example.com.  ", "api.example.com", "*.other.test"]);
        let validated = validate(&root, &raw).expect("合法域名清单应被接受");
        assert_eq!(
            validated.manifest.allowed_domains,
            vec!["api.example.com".to_string(), "*.other.test".to_string()]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 能力与域名清单必须成对：只写一边的清单在装载前就说清楚。
    #[test]
    fn net_fetch_and_allowed_domains_must_pair_up() {
        let root = temp_package("pair");

        let mut raw = legal_manifest();
        raw["capabilities"] = json!(["fs.read", "net.fetch"]);
        let errors = validate(&root, &raw).expect_err("申请了 net.fetch 却没写域名必须拒载");
        assert!(errors.iter().any(|error| error.path == "$.allowedDomains"));

        let mut raw = legal_manifest();
        raw["capabilities"] = json!(["fs.read"]);
        raw["allowedDomains"] = json!(["api.example.com"]);
        let errors = validate(&root, &raw).expect_err("写了域名却没申请能力必须拒载");
        assert!(errors.iter().any(|error| error.path == "$.capabilities"));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 域名形状与条数上限：`scheme` / 端口 / 下划线 / 裸主机名都不是域名。
    #[test]
    fn allowed_domains_shape_and_count_are_bounded() {
        let root = temp_package("domain-shape");
        let mut raw = legal_manifest();
        raw["capabilities"] = json!(["net.fetch"]);
        raw["allowedDomains"] = json!([
            "https://a.example",
            "a.example:8443",
            "a_b.example",
            "localhost",
            "*",
            "*."
        ]);
        let errors = validate(&root, &raw).expect_err("非法域名必须逐条报错");
        for index in 0..6 {
            let path = format!("$.allowedDomains[{index}]");
            assert!(
                errors.iter().any(|error| error.path == path),
                "第 {index} 条应被拒：{path}"
            );
        }

        let mut raw = legal_manifest();
        raw["capabilities"] = json!(["net.fetch"]);
        let domains: Vec<String> = (0..=MAX_ALLOWED_DOMAINS)
            .map(|index| format!("h{index}.example"))
            .collect();
        raw["allowedDomains"] = json!(domains);
        let errors = validate(&root, &raw).expect_err("超过条数上限必须拒载");
        assert!(errors.iter().any(|error| error.path == "$.allowedDomains"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 模型三件套（能力 / 名单 / 上限）必须成套，缺哪一件都要在装载前说清楚。
    ///
    /// 只申请能力不写名单 = 运行期永远 `6103`，装了也用不了；只写名单不申请能力 =
    /// 作者以为能调、实际一步都出不去；不写 `maxTokens` = 输出没有上限。三种都是
    /// 「装上去才发现」的故障，所以一律前移到校验阶段。
    #[test]
    fn llm_chat_requires_models_and_max_tokens_together() {
        let root = temp_package("llm-pair");

        let mut raw = legal_manifest();
        raw["capabilities"] = json!(["fs.read", "llm.chat"]);
        let errors = validate(&root, &raw).expect_err("申请了 llm.chat 却没写名单必须拒载");
        assert!(errors.iter().any(|error| error.path == "$.allowedModels"));
        assert!(errors.iter().any(|error| error.path == "$.maxTokens"));

        let mut raw = legal_manifest();
        raw["capabilities"] = json!(["fs.read"]);
        raw["allowedModels"] = json!(["openai/gpt-4o"]);
        raw["maxTokens"] = json!(512);
        let errors = validate(&root, &raw).expect_err("写了名单却没申请能力必须拒载");
        assert_eq!(
            errors
                .iter()
                .filter(|error| error.path == "$.capabilities")
                .count(),
            2,
            "名单与上限各报一条"
        );

        // 三件齐了才过：这是 `llm.chat` 唯一的合法形态。
        let mut raw = legal_manifest();
        raw["capabilities"] = json!(["fs.read", "fs.write", "llm.chat"]);
        raw["allowedModels"] = json!(["openai/gpt-4o"]);
        raw["maxTokens"] = json!(512);
        let validated = validate(&root, &raw).expect("成套声明应被接受");
        assert_eq!(validated.manifest.max_tokens, 512);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 模型条目归一化：大小写与首尾空白折成同一份，重复项只留一条。
    ///
    /// 白名单是**按字符串比对**的，不是按「语义上同一个模型」比对的。不归一化就会
    /// 出现 `OpenAI/GPT-4o` 与 `openai/gpt-4o` 两条并列——审查的人会以为授权了
    /// 两个模型，实际是同一个。
    #[test]
    fn allowed_models_are_normalized_and_deduped() {
        let root = temp_package("models");
        let mut raw = legal_manifest();
        raw["capabilities"] = json!(["fs.read", "fs.write", "llm.chat"]);
        raw["allowedModels"] = json!(["  OpenAI/GPT-4o  ", "openai/gpt-4o", "Local/qwen2.5-7b"]);
        raw["maxTokens"] = json!(1024);
        let validated = validate(&root, &raw).expect("合法模型名单应被接受");
        assert_eq!(
            validated.manifest.allowed_models,
            vec!["openai/gpt-4o".to_string(), "local/qwen2.5-7b".to_string()]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 模型条目形状与条数上限：`a/b/c`、空段、带空格、没斜杠都不是 `provider/model`。
    ///
    /// 松一点就会留下「两种写法等价」的灰区：插件写 `a/b/c` 能过校验，运行期却因为
    /// 比对不上而永远被拒，作者只能靠猜。
    #[test]
    fn allowed_models_shape_and_count_are_bounded() {
        let root = temp_package("model-shape");
        let mut raw = legal_manifest();
        raw["capabilities"] = json!(["fs.write", "llm.chat"]);
        raw["allowedModels"] = json!(["gpt-4o", "a/b/c", "/headless", "tail/", "a b/c"]);
        raw["maxTokens"] = json!(512);
        let errors = validate(&root, &raw).expect_err("非法模型条目必须逐条报错");
        for index in 0..5 {
            let path = format!("$.allowedModels[{index}]");
            assert!(
                errors.iter().any(|error| error.path == path),
                "第 {index} 条应被拒：{path}"
            );
        }

        let mut raw = legal_manifest();
        raw["capabilities"] = json!(["fs.write", "llm.chat"]);
        let models: Vec<String> = (0..=MAX_ALLOWED_MODELS)
            .map(|index| format!("p/m{index}"))
            .collect();
        raw["allowedModels"] = json!(models);
        raw["maxTokens"] = json!(512);
        let errors = validate(&root, &raw).expect_err("超过条数上限必须拒载");
        assert!(errors.iter().any(|error| error.path == "$.allowedModels"));

        // 上限本身有天花板：清单能批 8192，运行期也按这个数夹。
        let mut raw = legal_manifest();
        raw["capabilities"] = json!(["fs.write", "llm.chat"]);
        raw["allowedModels"] = json!(["openai/gpt-4o"]);
        raw["maxTokens"] = json!(MAX_TOKENS_CEILING + 1);
        let errors = validate(&root, &raw).expect_err("超过 maxTokens 天花板必须拒载");
        assert!(errors.iter().any(|error| error.path == "$.maxTokens"));

        let _ = std::fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------------
    // views（docs/24 §4.1、§8 V-1/V-2/V-3）
    // -----------------------------------------------------------------------

    /// 带一个完整视图的合法清单：`entry` 指向 `risk=read` 的 `repo_stats`。
    fn view_manifest() -> Value {
        let mut raw = legal_manifest();
        raw["views"] = json!([{
            "id": "main",
            "title": "3D 资产",
            "location": "pluginPanel.detail",
            "entry": "repo_stats",
            "refresh": ["workspace.changed", "file.saved"],
            "external": { "program": "asset3d-gui.exe", "protocol": "asset3d.session/1" }
        }]);
        raw
    }

    /// V-1：没写 `views` 的老清单照旧装载，规范化结果是空表，且落库 JSON 一字不多。
    ///
    /// 「一字不多」是双向兼容的另一半：老包的 `manifest_json` 不该因为新字段而
    /// 在下次 `plugin.status` 里突然多出一个 `"views":[]`。
    #[test]
    fn manifest_without_views_loads_with_empty_views() {
        let root = temp_package("views-absent");
        let validated = validate(&root, &legal_manifest()).expect("缺 views 不应报错");
        assert!(validated.manifest.views.is_empty());
        let json = validated.to_manifest_json();
        assert!(
            !json.contains("views"),
            "空 views 不应出现在落库 JSON 里：{json}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 合法视图：四个必填字段 + refresh + external 全部落位。
    #[test]
    fn legal_view_is_parsed() {
        let root = temp_package("views-legal");
        std::fs::write(root.join("asset3d-gui.exe"), b"MZ").expect("写视口可执行文件");
        let validated = validate(&root, &view_manifest()).expect("合法视图不应报错");
        let view = &validated.manifest.views[0];
        assert_eq!(view.id, "main");
        assert_eq!(view.title, "3D 资产");
        assert_eq!(view.location, "pluginPanel.detail");
        assert_eq!(view.entry, "repo_stats");
        assert_eq!(view.refresh, vec!["workspace.changed", "file.saved"]);
        let external = view.external.as_ref().expect("external 应被解析");
        assert_eq!(external.program, "asset3d-gui.exe");
        assert_eq!(external.protocol, "asset3d.session/1");

        let json = validated.to_manifest_json();
        assert!(
            json.contains("\"views\":[{\"id\":\"main\",\"title\":\"3D 资产\""),
            "落库 JSON 用 camelCase 且保持字段顺序：{json}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// V-2：`entry` 指向未声明的工具 → 6108；指向 `risk=write` 的工具 → 6107。
    #[test]
    fn view_entry_must_be_a_declared_read_tool() {
        let root = temp_package("views-entry");

        let mut raw = view_manifest();
        raw["views"][0]["entry"] = json!("view_render");
        let errors = validate(&root, &raw).expect_err("entry 指向未声明工具必须拒载");
        let violation = errors
            .iter()
            .find(|error| error.path == "$.views[0].entry")
            .expect("应指向 entry 字段");
        assert_eq!(
            violation.code,
            ErrorCode::PLUGIN_TOOL_CONFLICT,
            "未声明的 entry 用 6108"
        );

        // 指向本包的写工具：工具是声明了的，但视图加载会触发审批，属清单自相矛盾。
        let mut raw = view_manifest();
        raw["views"][0]["entry"] = json!("write_note");
        let errors = validate(&root, &raw).expect_err("entry 指向写工具必须拒载");
        let violation = errors
            .iter()
            .find(|error| error.path == "$.views[0].entry")
            .expect("应指向 entry 字段");
        assert_eq!(
            violation.code,
            ErrorCode::PLUGIN_MANIFEST_INVALID,
            "写工具当 entry 用 6107"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// V-3：`external.program` 越界 → 1002；非 `.exe` → 6107；文件不存在 → 6107。
    #[test]
    fn external_program_is_sandboxed() {
        let root = temp_package("views-program");

        // 越界：含 `..`。这是唯一按路径越狱（1002）处理的一档——它会被 spawn。
        for program in ["../evil.exe", "..\\evil.exe", "C:/Windows/system32/cmd.exe"] {
            let mut raw = view_manifest();
            raw["views"][0]["external"]["program"] = json!(program);
            let errors = validate(&root, &raw).expect_err("越界 program 必须拒载");
            let violation = errors
                .iter()
                .find(|error| error.path == "$.views[0].external.program")
                .expect("应指向 program 字段");
            assert_eq!(
                violation.code,
                ErrorCode::PATH_OUT_OF_WORKSPACE,
                "{program} 应按 1002 报"
            );
        }

        // 扩展名：`.exe` 之外一律拒，包括不带扩展名。
        for program in ["asset3d-gui.com", "asset3d-gui", "asset3d-gui.exe.bak"] {
            let mut raw = view_manifest();
            raw["views"][0]["external"]["program"] = json!(program);
            let errors = validate(&root, &raw).expect_err("非 exe 必须拒载");
            let violation = errors
                .iter()
                .find(|error| error.path == "$.views[0].external.program")
                .expect("应指向 program 字段");
            assert_eq!(
                violation.code,
                ErrorCode::PLUGIN_MANIFEST_INVALID,
                "{program} 应按 6107 报"
            );
        }

        // 存在性：合法相对路径但文件不在包里——「包不完整」，6107。
        let mut raw = view_manifest();
        raw["views"][0]["external"]["program"] = json!("bin/asset3d-gui.exe");
        let errors = validate(&root, &raw).expect_err("缺失的 exe 必须拒载");
        let violation = errors
            .iter()
            .find(|error| error.path == "$.views[0].external.program")
            .expect("应指向 program 字段");
        assert!(
            violation.message.contains("不存在"),
            "缺失与格式应能区分：{}",
            violation.message
        );

        // 子目录里的 exe 要给过：只看最后一段扩展名，不限制层级。
        std::fs::create_dir_all(root.join("bin")).expect("建子目录");
        std::fs::write(root.join("bin/asset3d-gui.exe"), b"MZ").expect("写子目录 exe");
        let mut raw = view_manifest();
        raw["views"][0]["external"]["program"] = json!("bin/asset3d-gui.exe");
        let validated = validate(&root, &raw).expect("子目录 exe 应通过");
        assert_eq!(
            validated.manifest.views[0]
                .external
                .as_ref()
                .expect("external 应在")
                .program,
            "bin/asset3d-gui.exe"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 视图语义：id 形状 / title 长度与控制字符 / location / refresh / protocol / 条数。
    #[test]
    fn view_semantics_are_enforced() {
        let root = temp_package("views-semantics");
        std::fs::write(root.join("asset3d-gui.exe"), b"MZ").expect("写视口可执行文件");

        let mut raw = view_manifest();
        raw["views"][0]["id"] = json!("Main_View");
        let errors = validate(&root, &raw).expect_err("id 首字符大写且含下划线必须拒");
        assert!(errors.iter().any(|error| error.path == "$.views[0].id"));

        // 48 字符是上限，按字符算：48 个汉字要能过，49 个不能过。
        let mut raw = view_manifest();
        raw["views"][0]["title"] = json!("视".repeat(MAX_VIEW_TITLE_LEN));
        validate(&root, &raw).expect("48 个汉字应在上限内");
        raw["views"][0]["title"] = json!("视".repeat(MAX_VIEW_TITLE_LEN + 1));
        let errors = validate(&root, &raw).expect_err("49 个汉字必须拒");
        assert!(errors.iter().any(|error| error.path == "$.views[0].title"));

        let mut raw = view_manifest();
        raw["views"][0]["title"] = json!("3D\u{7}资产");
        let errors = validate(&root, &raw).expect_err("控制字符必须拒");
        assert!(errors.iter().any(|error| error.path == "$.views[0].title"));

        // location 首版只开一个值：侧栏枚举是固定的，多写的挂不上去。
        let mut raw = view_manifest();
        raw["views"][0]["location"] = json!("sidebar");
        let errors = validate(&root, &raw).expect_err("未知挂载位置必须拒");
        assert!(errors
            .iter()
            .any(|error| error.path == "$.views[0].location"));

        let mut raw = view_manifest();
        raw["views"][0]["refresh"] = json!(["workspace.changed", "file.deleted"]);
        let errors = validate(&root, &raw).expect_err("未知刷新事件必须拒");
        assert!(errors
            .iter()
            .any(|error| error.path == "$.views[0].refresh[1]"));

        let mut raw = view_manifest();
        raw["views"][0]["external"]["protocol"] = json!("asset3d.session/2");
        let errors = validate(&root, &raw).expect_err("未知协议必须拒");
        assert!(errors
            .iter()
            .any(|error| error.path == "$.views[0].external.protocol"));

        // 条数上限：status 的固定开销要有界。
        let mut raw = view_manifest();
        let views: Vec<Value> = (0..=MAX_VIEWS)
            .map(|index| {
                json!({
                    "id": format!("view{index}"),
                    "title": "视图",
                    "location": "pluginPanel.detail",
                    "entry": "repo_stats"
                })
            })
            .collect();
        raw["views"] = json!(views);
        let errors = validate(&root, &raw).expect_err("超过条数上限必须拒");
        assert!(errors.iter().any(|error| error.path == "$.views"));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 视图 id 包内唯一；`external` 出现即要求 `program` 与 `protocol` 成对。
    #[test]
    fn view_ids_are_unique_and_external_is_complete() {
        let root = temp_package("views-cross");

        let mut raw = view_manifest();
        let second = raw["views"][0].clone();
        raw["views"].as_array_mut().expect("数组").push(second);
        let errors = validate(&root, &raw).expect_err("视图 id 重复必须拒");
        assert!(errors.iter().any(|error| error.path == "$.views[1].id"));

        let mut raw = view_manifest();
        raw["views"][0]["external"] = json!({ "program": "asset3d-gui.exe" });
        let errors = validate(&root, &raw).expect_err("缺 protocol 必须拒");
        assert!(errors
            .iter()
            .any(|error| error.path == "$.views[0].external.protocol"));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 视图段的形状错误不级联：`tools` 不是数组时不再逐条喊「entry 未声明」。
    #[test]
    fn view_shape_errors_do_not_cascade() {
        let root = temp_package("views-shape");
        std::fs::write(root.join("asset3d-gui.exe"), b"MZ").expect("写视口可执行文件");

        let mut raw = view_manifest();
        raw["views"] = json!({ "id": "main" });
        let errors = validate(&root, &raw).expect_err("views 必须是数组");
        assert_eq!(errors.len(), 1, "只应有一条形状错误：{errors:?}");
        assert_eq!(errors[0].path, "$.views");

        // tools 整体不是数组时，不该再逐条派生「entry 未声明工具」。
        // 视图本身保持完整——这一条要排除的正是级联，而非视图形状错误。
        let mut raw = view_manifest();
        raw["tools"] = json!({ "name": "repo_stats" });
        let errors = validate(&root, &raw).expect_err("tools 形状错必须拒");
        assert!(
            errors
                .iter()
                .all(|error| !error.path.starts_with("$.views")),
            "tools 形状错时不应判 entry 交叉：{errors:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
