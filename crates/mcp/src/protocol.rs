//! MCP 协议数据模型。
//!
//! 这里只放「线上契约」——版本常量、`initialize` 与工具相关的结果结构、
//! 以及工具命名空间规则。传输与状态机在别处。
//!
//! ## 协议版本协商
//!
//! 客户端在 `initialize` 里发自己最高的版本；Server 回它选定的版本。规范要求
//! 客户端**只接受自己支持的版本**，否则应当断开——静默接受会让字段语义错位
//! （不同版本的 `tools/list` 返回结构变过）。[`negotiate_version`] 就是这个判断。
//!
//! ## 工具命名空间
//!
//! MCP 工具名只在单个 Server 内唯一，多个 Server 拼进同一张工具表会撞名，
//! 也可能撞到内置工具。docs/04 §8.2 的约定是 `mcp__<server>__<tool>`，
//! [`tool_namespace`] / [`split_namespaced_tool`] 负责双向转换。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 本客户端支持的协议版本，**从新到旧**。
///
/// 顺序即优先级：握手时发第一个，Server 回其中任意一个都接受。
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// 握手时声明的版本。
pub const LATEST_PROTOCOL_VERSION: &str = SUPPORTED_PROTOCOL_VERSIONS[0];

/// 工具命名空间前缀。
pub const TOOL_NAMESPACE_PREFIX: &str = "mcp__";

/// 命名空间分隔符。
pub const TOOL_NAMESPACE_SEPARATOR: &str = "__";

/// 本客户端名（进 `clientInfo`）。
pub const CLIENT_NAME: &str = "ai-ide";

/// 客户端版本。
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 判断 Server 回的版本是否受支持；受支持时返回规范写法。
pub fn negotiate_version(server_version: &str) -> Option<&'static str> {
    let wanted = server_version.trim();
    SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .copied()
        .find(|version| *version == wanted)
}

/// 拼出对外暴露的工具名：`mcp__<server>__<tool>`。
pub fn tool_namespace(server: &str, tool: &str) -> String {
    format!("{TOOL_NAMESPACE_PREFIX}{server}{TOOL_NAMESPACE_SEPARATOR}{tool}")
}

/// 拆回 `(server, tool)`。
///
/// 只按第一个 `__` 切分：工具名自己可能带 `__`（如 `read__file`），
/// 而 Server 名在配置解析阶段已被禁止包含 `__`（见 [`crate::config`]）。
pub fn split_namespaced_tool(full_name: &str) -> Option<(&str, &str)> {
    let rest = full_name.strip_prefix(TOOL_NAMESPACE_PREFIX)?;
    let (server, tool) = rest.split_once(TOOL_NAMESPACE_SEPARATOR)?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool))
}

fn default_input_schema() -> Value {
    json!({ "type": "object" })
}

/// `initialize` 的返回。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// Server 选定的协议版本。
    pub protocol_version: String,
    /// Server 能力声明（原样保留，上层按需读）。
    #[serde(default)]
    pub capabilities: Value,
    /// Server 自述。
    pub server_info: ServerInfo,
    /// 使用说明（部分 Server 会写一段提示词给模型）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// Server 自述信息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// Server 名。
    #[serde(default)]
    pub name: String,
    /// Server 版本。
    #[serde(default)]
    pub version: String,
    /// 展示用的标题。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// 一个 MCP 工具的描述。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
    /// 工具名（Server 内唯一）。
    pub name: String,
    /// 展示标题。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 给模型看的说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 入参 JSON Schema。缺省按空对象 schema 处理，避免上层每次判空。
    #[serde(default = "default_input_schema")]
    pub input_schema: Value,
    /// 出参 schema（新版本才有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    /// 行为标注（`readOnlyHint` / `destructiveHint` 等），原样保留。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
}

/// `tools/list` 的单页结果。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsPage {
    /// 本页工具。
    #[serde(default)]
    pub tools: Vec<McpTool>,
    /// 下一页游标；空/缺失表示结束。
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// 工具返回的一个内容块。
///
/// 用内部 tag（`type`）枚举：MCP 的内容块就是「按 type 分支」的结构。
/// 未知 type 落到 [`McpContent::Unknown`] 而不是解析失败——Server 加了新块类型时，
/// 调用方应该看到「有一个没识别的内容块」，而不是整次调用报协议错。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum McpContent {
    /// 纯文本。
    #[serde(rename = "text")]
    Text {
        /// 文本内容。
        text: String,
    },
    /// 内联图片（base64）。
    #[serde(rename = "image")]
    Image {
        /// base64 数据。
        #[serde(default)]
        data: String,
        /// MIME 类型。
        #[serde(rename = "mimeType", default)]
        mime_type: String,
    },
    /// 内联音频（base64）。
    #[serde(rename = "audio")]
    Audio {
        /// base64 数据。
        #[serde(default)]
        data: String,
        /// MIME 类型。
        #[serde(rename = "mimeType", default)]
        mime_type: String,
    },
    /// 内嵌资源。
    #[serde(rename = "resource")]
    Resource {
        /// 资源对象（`uri` / `mimeType` / `text` 或 `blob`）。
        #[serde(default)]
        resource: Value,
    },
    /// 资源链接。
    #[serde(rename = "resource_link")]
    ResourceLink {
        /// 资源 URI。
        #[serde(default)]
        uri: String,
        /// 展示名。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// MIME 类型。
        #[serde(rename = "mimeType", default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
    /// 未识别的内容块（新版本新增类型时走这里）。
    #[serde(other)]
    Unknown,
}

/// `tools/call` 的返回。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    /// 内容块列表。
    #[serde(default)]
    pub content: Vec<McpContent>,
    /// 工具**执行**失败（不是协议失败）。
    ///
    /// 这是 MCP 的刻意设计：工具跑挂了要作为正常结果回给模型看，让它自己重试或换路，
    /// 所以调用方拿到 `is_error = true` 时一般是 `Ok`，不是 `Err`。
    #[serde(default)]
    pub is_error: bool,
    /// 结构化输出（Server 支持时）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
}

impl CallToolResult {
    /// 把文本块拼成一个字符串（换行连接），非文本块忽略。
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                McpContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_version_is_first_supported() {
        assert_eq!(LATEST_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS[0]);
    }

    #[test]
    fn negotiates_supported_versions_only() {
        for version in SUPPORTED_PROTOCOL_VERSIONS {
            assert_eq!(negotiate_version(version), Some(version));
        }
        assert_eq!(negotiate_version("2023-01-01"), None);
        assert_eq!(negotiate_version(""), None);
    }

    #[test]
    fn negotiates_after_trimming_whitespace() {
        assert_eq!(negotiate_version(" 2024-11-05\n"), Some("2024-11-05"));
    }

    #[test]
    fn namespace_roundtrip() {
        let full = tool_namespace("github", "create_issue");
        assert_eq!(full, "mcp__github__create_issue");
        assert_eq!(
            split_namespaced_tool(&full),
            Some(("github", "create_issue"))
        );
    }

    #[test]
    fn namespace_keeps_separator_inside_tool_name() {
        let full = tool_namespace("fs", "read__file");
        assert_eq!(split_namespaced_tool(&full), Some(("fs", "read__file")));
    }

    #[test]
    fn namespace_rejects_foreign_names() {
        assert_eq!(split_namespaced_tool("read_file"), None);
        assert_eq!(split_namespaced_tool("mcp__github"), None);
        assert_eq!(split_namespaced_tool("mcp____tool"), None);
        assert_eq!(split_namespaced_tool("mcp__server__"), None);
    }

    #[test]
    fn initialize_result_parses_camel_case() {
        let value = json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {"listChanged": true}},
            "serverInfo": {"name": "filesystem", "version": "1.2.3"},
            "instructions": "用于读写本地文件"
        });
        let result: InitializeResult = serde_json::from_value(value).expect("parse");
        assert_eq!(result.protocol_version, "2025-06-18");
        assert_eq!(result.server_info.name, "filesystem");
        assert_eq!(result.instructions.as_deref(), Some("用于读写本地文件"));
    }

    #[test]
    fn initialize_result_tolerates_missing_optional_fields() {
        let result: InitializeResult = serde_json::from_value(json!({
            "protocolVersion": "2024-11-05",
            "serverInfo": {"name": "x"}
        }))
        .expect("parse");
        assert_eq!(result.capabilities, Value::Null);
        assert_eq!(result.server_info.version, "");
        assert!(result.instructions.is_none());
    }

    #[test]
    fn tool_defaults_input_schema_to_object() {
        let tool: McpTool = serde_json::from_value(json!({"name": "echo"})).expect("parse");
        assert_eq!(tool.input_schema, json!({"type": "object"}));
        assert!(tool.description.is_none());
    }

    #[test]
    fn tools_page_parses_cursor() {
        let page: ToolsPage = serde_json::from_value(json!({
            "tools": [{"name": "a"}, {"name": "b"}],
            "nextCursor": "page-2"
        }))
        .expect("parse");
        assert_eq!(page.tools.len(), 2);
        assert_eq!(page.next_cursor.as_deref(), Some("page-2"));
    }

    #[test]
    fn call_result_collects_text_blocks() {
        let result: CallToolResult = serde_json::from_value(json!({
            "content": [
                {"type": "text", "text": "第一行"},
                {"type": "image", "data": "AAA", "mimeType": "image/png"},
                {"type": "text", "text": "第二行"}
            ],
            "isError": false
        }))
        .expect("parse");
        assert_eq!(result.text(), "第一行\n第二行");
        assert!(!result.is_error);
        assert_eq!(result.content.len(), 3);
    }

    #[test]
    fn call_result_keeps_unknown_content_type() {
        let result: CallToolResult = serde_json::from_value(json!({
            "content": [{"type": "video", "uri": "file:///a.mp4"}]
        }))
        .expect("未知内容块不应让整次解析失败");
        assert_eq!(result.content, vec![McpContent::Unknown]);
        assert_eq!(result.text(), "");
    }

    #[test]
    fn call_result_parses_resource_link() {
        let result: CallToolResult = serde_json::from_value(json!({
            "content": [{"type": "resource_link", "uri": "file:///a.txt", "name": "a.txt", "mimeType": "text/plain"}]
        }))
        .expect("parse");
        match &result.content[0] {
            McpContent::ResourceLink {
                uri,
                name,
                mime_type,
            } => {
                assert_eq!(uri, "file:///a.txt");
                assert_eq!(name.as_deref(), Some("a.txt"));
                assert_eq!(mime_type.as_deref(), Some("text/plain"));
            }
            other => panic!("期望 resource_link，得到 {other:?}"),
        }
    }

    #[test]
    fn empty_call_result_is_valid() {
        let result: CallToolResult = serde_json::from_value(json!({})).expect("parse");
        assert!(result.content.is_empty());
        assert!(!result.is_error);
        assert_eq!(result.text(), "");
    }
}
