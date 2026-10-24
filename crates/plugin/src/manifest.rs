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
pub const HOST_PLUGIN_API_MINOR: u32 = 0;

/// 清单文件名。
pub const MANIFEST_FILE: &str = "plugin.json";

/// 单次工具调用超时默认值（毫秒）。
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// 单次工具调用超时下限（毫秒）。低于 1 s 的插件基本只会在握手阶段就超时。
pub const MIN_TIMEOUT_MS: u64 = 1_000;

/// 单次工具调用超时上限（毫秒），与 docs/14 §6.2 的 120 s 硬墙对齐。
pub const MAX_TIMEOUT_MS: u64 = 120_000;

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
    pub timeout_ms: u64,
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
    let tools = parse_tools(obj, &mut errors);
    let commands = parse_commands(obj, &mut errors);

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
        if let Some(field) = tool.path_field.as_deref() {
            if field.trim().is_empty() {
                errors.push(Violation::manifest(
                    format!("$.tools[{index}].pathField"),
                    "pathField 不能为空字符串",
                ));
            }
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
        if capabilities.is_some() {
            match tool.risk {
                PluginToolRisk::Read => {}
                PluginToolRisk::Write => {
                    if !normalized_caps.iter().any(|cap| cap == "fs.write") {
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
        timeout_ms: timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
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
        // 无法落地，语义校验阶段会把它当矛盾报出来（见 `validate` 的语义段）。
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

// ---------------------------------------------------------------------------
// 语义判断（手写，不引正则：三个小函数比多一个依赖划算）
// ---------------------------------------------------------------------------

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
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("main 不能为空".to_string());
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(format!("main 必须是包内相对路径，不得为绝对路径：{raw}"));
    }

    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(segment) => parts.push(segment.to_string_lossy().into_owned()),
            Component::ParentDir => {
                return Err(format!("main 不得包含 ..（会越出插件包根）：{raw}"));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "main 必须是包内相对路径，不得带盘符 / 根目录前缀：{raw}"
                ));
            }
        }
    }

    if parts.is_empty() {
        return Err(format!("main 归一化后为空：{raw}"));
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
                    "parameters": { "type": "object", "properties": {} }
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
}
