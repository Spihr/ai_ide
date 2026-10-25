//! 文档同步：`didOpen` / `didChange`（full）/ `didClose`。
//!
//! 本切片只做 **full 同步**（`TextDocumentSyncKind.Full`）：每次变更把整篇文本发过去。
//! 增量同步（`range` + `rangeLength`）需要客户端做文本编辑的坐标换算，一旦算错，
//! 服务器侧的文档就与编辑器永久不一致——那是后续切片配 `didChange` 增量的活。
//! 所以这里刻意**不接受 range**，`content_changes` 里只有一个纯 `text` 字段，
//! 服务器按规范理解成「整篇替换」。
//!
//! 版本号语义（规范要求）：同一 uri 的 `version` 必须单调递增，且只在打开期间有效。
//! [`DocumentSync`] 把这张表管起来，避免调用方自己维护版本号时漏加或重复。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::LspError;

/// `textDocument/didOpen`。
pub const METHOD_DID_OPEN: &str = "textDocument/didOpen";
/// `textDocument/didChange`。
pub const METHOD_DID_CHANGE: &str = "textDocument/didChange";
/// `textDocument/didClose`。
pub const METHOD_DID_CLOSE: &str = "textDocument/didClose";

/// 文档标识。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextDocumentIdentifier {
    /// 文档 uri（`file:///...`）。
    pub uri: String,
}

/// 带版本号的文档标识。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionedTextDocumentIdentifier {
    /// 文档 uri。
    pub uri: String,
    /// 版本号（同一 uri 单调递增）。
    pub version: i32,
}

/// 打开文档时的完整内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocumentItem {
    /// 文档 uri。
    pub uri: String,
    /// 语言标识，例如 `rust` / `typescript`。
    pub language_id: String,
    /// 初始版本号（本 crate 从 1 开始）。
    pub version: i32,
    /// 全文。
    pub text: String,
}

/// 一次变更。只有 `text` 表示整篇替换（full 同步）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextDocumentContentChangeEvent {
    /// 替换后的全文。
    pub text: String,
}

/// `textDocument/didOpen` 的参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DidOpenTextDocumentParams {
    /// 被打开的文档。
    pub text_document: TextDocumentItem,
}

/// `textDocument/didChange` 的参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DidChangeTextDocumentParams {
    /// 被修改的文档与它的新版本号。
    pub text_document: VersionedTextDocumentIdentifier,
    /// 变更列表（full 同步下恒为一项）。
    pub content_changes: Vec<TextDocumentContentChangeEvent>,
}

/// `textDocument/didClose` 的参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DidCloseTextDocumentParams {
    /// 被关闭的文档。
    pub text_document: TextDocumentIdentifier,
}

#[derive(Debug, Clone)]
struct OpenDocument {
    language_id: String,
    version: i32,
}

/// 打开文档的版本表。
///
/// 纯内存、纯同步（内部 `Mutex`），可以脱离连接单测：版本号规则一旦错，
/// 服务器侧文档就会错位，这是 LSP 调试里最难查的一类问题。
#[derive(Debug, Default)]
pub struct DocumentSync {
    documents: Mutex<HashMap<String, OpenDocument>>,
}

impl DocumentSync {
    /// 空表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一次打开，返回要给服务器的参数（版本号从 1 开始）。
    ///
    /// 重复打开同一 uri 时按「重新打开」处理：版本号继续递增，
    /// 这样即使调用方漏发了 `didClose`，服务器也不会看到版本回退。
    pub fn open(
        &self,
        uri: impl Into<String>,
        language_id: impl Into<String>,
        text: impl Into<String>,
    ) -> DidOpenTextDocumentParams {
        let uri = uri.into();
        let language_id = language_id.into();
        let mut documents = self.lock();
        let version = documents
            .get(&uri)
            .map(|document| document.version + 1)
            .unwrap_or(1);
        documents.insert(
            uri.clone(),
            OpenDocument {
                language_id: language_id.clone(),
                version,
            },
        );
        DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri,
                language_id,
                version,
                text: text.into(),
            },
        }
    }

    /// 登记一次全文变更；文档未打开时返回 [`LspError::Document`]。
    pub fn change_full(
        &self,
        uri: &str,
        text: impl Into<String>,
    ) -> Result<DidChangeTextDocumentParams, LspError> {
        let mut documents = self.lock();
        let Some(document) = documents.get_mut(uri) else {
            return Err(LspError::Document(format!(
                "对未打开的文档 `{uri}` 发送 didChange"
            )));
        };
        document.version += 1;
        Ok(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.to_string(),
                version: document.version,
            },
            content_changes: vec![TextDocumentContentChangeEvent { text: text.into() }],
        })
    }

    /// 登记一次关闭；文档未打开时返回 [`LspError::Document`]。
    pub fn close(&self, uri: &str) -> Result<DidCloseTextDocumentParams, LspError> {
        let mut documents = self.lock();
        if documents.remove(uri).is_none() {
            return Err(LspError::Document(format!(
                "对未打开的文档 `{uri}` 发送 didClose"
            )));
        }
        Ok(DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier {
                uri: uri.to_string(),
            },
        })
    }

    /// 当前版本号；未打开时为 `None`。
    pub fn version(&self, uri: &str) -> Option<i32> {
        self.lock().get(uri).map(|document| document.version)
    }

    /// 语言标识；未打开时为 `None`。
    pub fn language_id(&self, uri: &str) -> Option<String> {
        self.lock()
            .get(uri)
            .map(|document| document.language_id.clone())
    }

    /// 是否已打开。
    pub fn is_open(&self, uri: &str) -> bool {
        self.lock().contains_key(uri)
    }

    /// 已打开的文档数。
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// 是否没有任何打开的文档。
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, OpenDocument>> {
        // 这里没有跨字段不变量，锁中毒后继续用内部数据比 panic 更合适。
        self.documents
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 本地路径 → `file://` uri。
///
/// Windows 盘符路径要变成 `file:///E:/dir/a.ts`（三个斜杠），并且把空格、中文等
/// 非 ASCII 字节百分号编码；否则服务器解析 uri 时会拿到错路径，最后表现为
/// 「诊断全空」而不是报错——所以要在这里一次做对。
pub fn path_to_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::with_capacity(text.len() + 8);
    uri.push_str("file://");
    if !text.starts_with('/') {
        uri.push('/');
    }
    for byte in text.as_bytes() {
        match byte {
            // 未保留字符 + 路径分隔符 + 盘符冒号，保持可读。
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => uri.push(*byte as char),
            b'-' | b'.' | b'_' | b'~' | b'/' | b':' => uri.push(*byte as char),
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    uri
}

/// `file://` uri → 本地路径。
///
/// 只处理 `file:` scheme；其他协议（`untitled:` / `git:`）返回 `None`，
/// 因为那些 uri 不对应磁盘路径，硬转会误导调用方。
pub fn uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri
        .strip_prefix("file://")
        .or_else(|| uri.strip_prefix("file:"))?;
    let decoded = percent_decode(rest)?;
    // Windows：`/E:/dir/a.ts` → `E:/dir/a.ts`。
    let trimmed = if decoded.len() > 2 && decoded.as_bytes()[2] == b':' {
        decoded[1..].to_string()
    } else {
        decoded
    };
    Some(std::path::PathBuf::from(trimmed))
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// 按扩展名猜 LSP 语言标识（`didOpen` 必填）。
///
/// 猜不出时返回 `plaintext`：宁可让服务器退化成无语法分析，也不要发空字符串
/// ——部分服务器会因此直接拒绝对话。
pub fn language_id_for_path(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "py" | "pyi" => "python",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "json" => "json",
        "jsonc" => "jsonc",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "sh" | "bash" => "shellscript",
        "ps1" => "powershell",
        "lua" => "lua",
        _ => "plaintext",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_start_at_one_and_increase_monotonically() {
        let sync = DocumentSync::new();
        let opened = sync.open("file:///a.ts", "typescript", "const a = 1;");
        assert_eq!(opened.text_document.version, 1);

        let first = sync
            .change_full("file:///a.ts", "const a = 2;")
            .expect("ok");
        assert_eq!(first.text_document.version, 2);
        assert_eq!(first.content_changes.len(), 1);
        assert_eq!(first.content_changes[0].text, "const a = 2;");

        let second = sync
            .change_full("file:///a.ts", "const a = 3;")
            .expect("ok");
        assert_eq!(second.text_document.version, 3);

        sync.close("file:///a.ts").expect("ok");
        assert!(!sync.is_open("file:///a.ts"));
        assert!(sync.is_empty());
    }

    #[test]
    fn change_and_close_require_open_document() {
        let sync = DocumentSync::new();
        let change = sync
            .change_full("file:///ghost.ts", "x")
            .expect_err("必须失败");
        assert!(matches!(change, LspError::Document(_)), "{change}");
        assert_eq!(change.class(), crate::error::ErrorClass::State);

        let close = sync.close("file:///ghost.ts").expect_err("必须失败");
        assert!(matches!(close, LspError::Document(_)), "{close}");
    }

    #[test]
    fn reopening_keeps_version_increasing() {
        let sync = DocumentSync::new();
        sync.open("file:///a.rs", "rust", "fn main() {}");
        let reopened = sync.open("file:///a.rs", "rust", "fn main() { }");
        assert_eq!(reopened.text_document.version, 2);
        assert_eq!(sync.version("file:///a.rs"), Some(2));
        assert_eq!(sync.language_id("file:///a.rs").as_deref(), Some("rust"));
    }

    #[test]
    fn full_sync_change_has_no_range_field() {
        let sync = DocumentSync::new();
        sync.open("file:///a.py", "python", "print(1)");
        let change = sync.change_full("file:///a.py", "print(2)").expect("ok");
        let value = serde_json::to_value(&change).expect("serialize");
        let event = &value["contentChanges"][0];
        assert!(event.get("range").is_none(), "{value}");
        assert_eq!(event["text"], serde_json::json!("print(2)"));
        assert_eq!(
            value["textDocument"]["uri"],
            serde_json::json!("file:///a.py")
        );
        assert_eq!(value["textDocument"]["version"], serde_json::json!(2));
    }

    #[test]
    fn open_params_use_camel_case_language_id() {
        let sync = DocumentSync::new();
        let opened = sync.open("file:///a.tsx", "typescriptreact", "x");
        let value = serde_json::to_value(&opened).expect("serialize");
        assert_eq!(
            value["textDocument"]["languageId"],
            serde_json::json!("typescriptreact")
        );
        assert_eq!(value["textDocument"]["version"], serde_json::json!(1));
    }

    #[test]
    fn windows_paths_become_file_uris_with_three_slashes() {
        let uri = path_to_uri(Path::new(r"E:\ai_ide\crates\lsp\src\main.rs"));
        assert_eq!(uri, "file:///E:/ai_ide/crates/lsp/src/main.rs");
        assert_eq!(
            uri_to_path(&uri),
            Some(std::path::PathBuf::from("E:/ai_ide/crates/lsp/src/main.rs"))
        );
    }

    #[test]
    fn spaces_and_non_ascii_are_percent_encoded() {
        let uri = path_to_uri(Path::new(r"E:\项目 目录\a b.ts"));
        assert!(uri.starts_with("file:///E:/"), "{uri}");
        assert!(uri.contains("%20"), "{uri}");
        assert!(!uri.contains(' '), "{uri}");
        assert_eq!(
            uri_to_path(&uri)
                .as_deref()
                .map(|path| path.to_string_lossy().into_owned()),
            Some("E:/项目 目录/a b.ts".to_string())
        );
    }

    #[test]
    fn non_file_uris_have_no_path() {
        assert_eq!(uri_to_path("untitled:Untitled-1"), None);
        assert_eq!(uri_to_path("git://x"), None);
    }

    #[test]
    fn language_ids_cover_common_extensions() {
        let cases = [
            ("a.rs", "rust"),
            ("a.ts", "typescript"),
            ("a.tsx", "typescriptreact"),
            ("a.jsx", "javascriptreact"),
            ("a.py", "python"),
            ("a.toml", "toml"),
            ("a.unknownext", "plaintext"),
            ("noext", "plaintext"),
        ];
        for (name, expected) in cases {
            assert_eq!(language_id_for_path(Path::new(name)), expected, "{name}");
        }
    }
}
