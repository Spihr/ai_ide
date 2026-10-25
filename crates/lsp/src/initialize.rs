//! 握手与能力声明：`initialize` / `initialized` / `shutdown` / `exit`。
//!
//! ## 顺序不能换
//!
//! 规范把四步写成严格时序：
//!
//! ```text
//! 客户端 ──initialize(请求)──▶ 服务器        （服务器此时还不能接其他请求）
//! 客户端 ◀────result────────── 服务器
//! 客户端 ──initialized(通知)─▶ 服务器        （通知，不能当请求发，收不到响应）
//! ……
//! 客户端 ──shutdown(请求)────▶ 服务器 ◀─ null
//! 客户端 ──exit(通知)────────▶ 服务器        （进程退出）
//! ```
//!
//! `initialized` 是通知，一旦误写成请求，服务器会回 `-32601`，而某些服务器
//! 还会因此**不开始索引**——表现是「hover 永远空」且没有任何报错。
//!
//! ## 能力声明为什么要显式写全
//!
//! `initialize` 里的 `capabilities` 是告诉服务器「客户端会处理什么」。少声明
//! `publishDiagnostics.versionSupport`，服务器就可能不带 version 发诊断，
//! 客户端便没法丢弃「旧版本的诊断」。这里只声明显式支持的部分，不做全能宣告。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `initialize`。
pub const METHOD_INITIALIZE: &str = "initialize";
/// `initialized`（通知）。
pub const METHOD_INITIALIZED: &str = "initialized";
/// `shutdown`。
pub const METHOD_SHUTDOWN: &str = "shutdown";
/// `exit`（通知）。
pub const METHOD_EXIT: &str = "exit";

/// 客户端名字（进 `clientInfo`，服务器日志与 `--version` 面板会显示）。
pub const CLIENT_NAME: &str = "ai-ide";

/// 文档同步方式（`TextDocumentSyncKind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "i32", from = "i32")]
pub enum TextDocumentSyncKind {
    /// 不做同步。
    None,
    /// 每次变更发全文（本 crate 采用的方式）。
    Full,
    /// 每次变更发增量（本轮未实现，见 `document` 模块说明）。
    Incremental,
}

impl From<TextDocumentSyncKind> for i32 {
    fn from(kind: TextDocumentSyncKind) -> Self {
        match kind {
            TextDocumentSyncKind::None => 0,
            TextDocumentSyncKind::Full => 1,
            TextDocumentSyncKind::Incremental => 2,
        }
    }
}

impl From<i32> for TextDocumentSyncKind {
    fn from(value: i32) -> Self {
        match value {
            0 => Self::None,
            2 => Self::Incremental,
            // 服务器给了未来版本的新数字：按 Full 处理比拒绝握手更实用，
            // 因为 Full 是「整篇替换」，对任何实现都安全。
            _ => Self::Full,
        }
    }
}

/// 文档同步能力：规范允许它是裸数字，也允许是带细节的对象。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TextDocumentSync {
    /// `textDocumentSync: 1`
    Kind(TextDocumentSyncKind),
    /// `textDocumentSync: { openClose, change, save }`
    Options(TextDocumentSyncOptions),
}

impl TextDocumentSync {
    /// 实际生效的同步方式。
    pub fn kind(&self) -> TextDocumentSyncKind {
        match self {
            Self::Kind(kind) => *kind,
            Self::Options(options) => options.change.unwrap_or(TextDocumentSyncKind::None),
        }
    }

    /// 是否支持 `didOpen` / `didClose`。裸数字形态隐含支持。
    pub fn open_close(&self) -> bool {
        match self {
            Self::Kind(kind) => *kind != TextDocumentSyncKind::None,
            Self::Options(options) => options.open_close.unwrap_or(false),
        }
    }
}

/// 对象形态的文档同步能力。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocumentSyncOptions {
    /// 是否处理 `didOpen` / `didClose`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_close: Option<bool>,
    /// 变更方式。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<TextDocumentSyncKind>,
    /// `save` 选项（本层不主动触发保存）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save: Option<Value>,
}

/// 「bool 或对象」形态的能力位（`hoverProvider` 等）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderCapability {
    /// `true` / `false`。
    Flag(bool),
    /// 带选项的对象。
    Options(Value),
}

impl ProviderCapability {
    /// 服务器是否声明支持该能力。
    pub fn is_supported(&self) -> bool {
        match self {
            Self::Flag(flag) => *flag,
            // 对象形态一律视为「支持」（选项细节本层不解释）。
            Self::Options(_) => true,
        }
    }
}

/// 服务器能力（只保留客户端会用到的部分）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    /// 文档同步方式。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_document_sync: Option<TextDocumentSync>,
    /// 补全。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_provider: Option<ProviderCapability>,
    /// 悬停。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hover_provider: Option<ProviderCapability>,
    /// 跳转定义。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_provider: Option<ProviderCapability>,
    /// 查找引用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references_provider: Option<ProviderCapability>,
    /// 文档符号。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_symbol_provider: Option<ProviderCapability>,
    /// 工作区符号。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_symbol_provider: Option<ProviderCapability>,
    /// 重命名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rename_provider: Option<ProviderCapability>,
    /// 代码动作。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_action_provider: Option<ProviderCapability>,
    /// 格式化。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_formatting_provider: Option<ProviderCapability>,
}

impl ServerCapabilities {
    /// 某能力是否被声明支持。
    pub fn supports(&self, capability: Provider) -> bool {
        let slot = match capability {
            Provider::Completion => &self.completion_provider,
            Provider::Hover => &self.hover_provider,
            Provider::Definition => &self.definition_provider,
            Provider::References => &self.references_provider,
            Provider::DocumentSymbol => &self.document_symbol_provider,
            Provider::WorkspaceSymbol => &self.workspace_symbol_provider,
            Provider::Rename => &self.rename_provider,
            Provider::CodeAction => &self.code_action_provider,
            Provider::Formatting => &self.document_formatting_provider,
        };
        slot.as_ref().is_some_and(ProviderCapability::is_supported)
    }

    /// 实际生效的同步方式。
    pub fn sync_kind(&self) -> TextDocumentSyncKind {
        self.text_document_sync
            .as_ref()
            .map(TextDocumentSync::kind)
            .unwrap_or(TextDocumentSyncKind::None)
    }

    /// 是否支持 `didOpen` / `didClose`。
    pub fn supports_open_close(&self) -> bool {
        self.text_document_sync
            .as_ref()
            .is_some_and(TextDocumentSync::open_close)
    }
}

/// 可查询的能力位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// 补全。
    Completion,
    /// 悬停。
    Hover,
    /// 跳转定义。
    Definition,
    /// 查找引用。
    References,
    /// 文档符号。
    DocumentSymbol,
    /// 工作区符号。
    WorkspaceSymbol,
    /// 重命名。
    Rename,
    /// 代码动作。
    CodeAction,
    /// 格式化。
    Formatting,
}

/// 客户端信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// 客户端名。
    pub name: String,
    /// 客户端版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// 服务器信息（回包里的 `serverInfo`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// 服务器名，例如 `rust-analyzer`。
    pub name: String,
    /// 服务器版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// `initialize` 的结果。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// 服务器能力。
    #[serde(default)]
    pub capabilities: ServerCapabilities,
    /// 服务器信息。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_info: Option<ServerInfo>,
}

/// 工作区目录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceFolder {
    /// 目录 uri。
    pub uri: String,
    /// 展示名。
    pub name: String,
}

/// 工作区相关能力。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceClientCapabilities {
    /// 是否响应 `workspace/configuration`（本层会回等长 null 数组）。
    pub configuration: bool,
    /// 是否支持多根工作区。
    pub workspace_folders: bool,
}

/// 文档同步相关能力。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynchronizationClientCapabilities {
    /// 是否支持动态注册（本层静态声明，故为 false）。
    pub dynamic_registration: bool,
    /// 是否接收 `didSave` 相关通知。
    pub did_save: bool,
}

/// 诊断相关能力。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishDiagnosticsClientCapabilities {
    /// 是否处理诊断里的 `relatedInformation`。
    pub related_information: bool,
    /// 是否处理诊断里的 `version`（用于丢弃过期诊断）。
    pub version_support: bool,
}

/// Markdown 内容能力。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownClientCapabilities {
    /// 支持的内容格式。`markdown` 与 `plaintext` 同时声明，服务器会优先给前者。
    pub content_format: Vec<String>,
}

/// 文本相关能力。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocumentClientCapabilities {
    /// 文档同步。
    pub synchronization: SynchronizationClientCapabilities,
    /// 诊断。
    pub publish_diagnostics: PublishDiagnosticsClientCapabilities,
    /// 悬停内容格式。
    pub hover: MarkdownClientCapabilities,
}

/// 客户端能力总表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    /// 工作区。
    pub workspace: WorkspaceClientCapabilities,
    /// 文本文档。
    pub text_document: TextDocumentClientCapabilities,
    /// 位置编码（LSP 3.17）。显式声明 `utf-16`：编辑器内部用 UTF-16 偏移，
    /// 服务器若默认 `utf-8` 则所有位置都会偏。
    pub general: Value,
}

impl Default for ClientCapabilities {
    fn default() -> Self {
        Self {
            workspace: WorkspaceClientCapabilities {
                configuration: true,
                workspace_folders: false,
            },
            text_document: TextDocumentClientCapabilities {
                synchronization: SynchronizationClientCapabilities {
                    dynamic_registration: false,
                    did_save: false,
                },
                publish_diagnostics: PublishDiagnosticsClientCapabilities {
                    related_information: true,
                    version_support: true,
                },
                hover: MarkdownClientCapabilities {
                    content_format: vec!["markdown".to_string(), "plaintext".to_string()],
                },
            },
            general: serde_json::json!({ "positionEncodings": ["utf-16"] }),
        }
    }
}

/// `initialize` 的参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    /// 客户端进程 id（服务器用它判断客户端是否还活着）。
    pub process_id: Option<u32>,
    /// 客户端信息。
    pub client_info: ClientInfo,
    /// 根目录 uri（旧字段，多根工作区走 `workspaceFolders`）。
    pub root_uri: Option<String>,
    /// 工作区目录列表。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_folders: Option<Vec<WorkspaceFolder>>,
    /// 客户端能力。
    pub capabilities: ClientCapabilities,
}

impl InitializeParams {
    /// 按根目录构造；`root_uri` 为 `None` 表示只开单文件、没有工作区。
    pub fn new(root_uri: Option<String>) -> Self {
        Self {
            process_id: Some(std::process::id()),
            client_info: ClientInfo {
                name: CLIENT_NAME.to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            },
            workspace_folders: root_uri.as_ref().map(|uri| {
                vec![WorkspaceFolder {
                    uri: uri.clone(),
                    name: "workspace".to_string(),
                }]
            }),
            root_uri,
            capabilities: ClientCapabilities::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_kind_accepts_bare_number_and_object() {
        let bare: ServerCapabilities =
            serde_json::from_value(serde_json::json!({"textDocumentSync": 1})).expect("ok");
        assert_eq!(bare.sync_kind(), TextDocumentSyncKind::Full);
        assert!(bare.supports_open_close());

        let object: ServerCapabilities = serde_json::from_value(serde_json::json!({
            "textDocumentSync": {"openClose": true, "change": 2}
        }))
        .expect("ok");
        assert_eq!(object.sync_kind(), TextDocumentSyncKind::Incremental);
        assert!(object.supports_open_close());
    }

    #[test]
    fn missing_sync_capability_means_none() {
        let caps: ServerCapabilities = serde_json::from_value(serde_json::json!({})).expect("ok");
        assert_eq!(caps.sync_kind(), TextDocumentSyncKind::None);
        assert!(!caps.supports_open_close());
        assert!(!caps.supports(Provider::Hover));
    }

    #[test]
    fn unknown_sync_kind_falls_back_to_full() {
        assert_eq!(
            TextDocumentSyncKind::from(99),
            TextDocumentSyncKind::Full,
            "未来版本的新数字按全文同步处理"
        );
    }

    #[test]
    fn providers_accept_bool_and_object_shapes() {
        let caps: ServerCapabilities = serde_json::from_value(serde_json::json!({
            "hoverProvider": true,
            "definitionProvider": {"workDoneProgress": true},
            "renameProvider": false
        }))
        .expect("ok");
        assert!(caps.supports(Provider::Hover));
        assert!(caps.supports(Provider::Definition));
        assert!(!caps.supports(Provider::Rename));
    }

    #[test]
    fn initialize_params_carry_workspace_folder_and_utf16_encoding() {
        let params = InitializeParams::new(Some("file:///E:/ai_ide".into()));
        let value = serde_json::to_value(&params).expect("serialize");
        assert_eq!(value["clientInfo"]["name"], serde_json::json!("ai-ide"));
        assert_eq!(value["rootUri"], serde_json::json!("file:///E:/ai_ide"));
        assert_eq!(
            value["workspaceFolders"][0]["uri"],
            serde_json::json!("file:///E:/ai_ide")
        );
        assert_eq!(
            value["capabilities"]["general"]["positionEncodings"][0],
            serde_json::json!("utf-16")
        );
        assert_eq!(
            value["capabilities"]["textDocument"]["publishDiagnostics"]["versionSupport"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn no_root_uri_means_no_workspace_folders() {
        let params = InitializeParams::new(None);
        let value = serde_json::to_value(&params).expect("serialize");
        assert!(value["rootUri"].is_null());
        assert!(value.get("workspaceFolders").is_none());
    }

    #[test]
    fn initialize_result_parses_server_info() {
        let result: InitializeResult = serde_json::from_value(serde_json::json!({
            "capabilities": {"textDocumentSync": 1, "hoverProvider": true},
            "serverInfo": {"name": "rust-analyzer", "version": "0.3.2000"}
        }))
        .expect("ok");
        assert_eq!(result.capabilities.sync_kind(), TextDocumentSyncKind::Full);
        assert_eq!(
            result.server_info.as_ref().map(|info| info.name.as_str()),
            Some("rust-analyzer")
        );
    }

    #[test]
    fn handshake_method_names_are_protocol_exact() {
        assert_eq!(METHOD_INITIALIZE, "initialize");
        assert_eq!(METHOD_INITIALIZED, "initialized");
        assert_eq!(METHOD_SHUTDOWN, "shutdown");
        assert_eq!(METHOD_EXIT, "exit");
    }
}
