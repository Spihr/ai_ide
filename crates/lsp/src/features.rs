//! 高层请求封装：`definition` / `references` / `hover` / `completion`。
//!
//! ## 这一层解决什么
//!
//! 直接拿 [`crate::LspClient::request`] 发 `textDocument/hover` 有三个坑，每个都
//! 足以让上层写出一屏 bug：
//!
//! 1. **文档没打开**。规范要求先 `didOpen`，否则服务器要么忽略请求、要么回
//!    空结果，看起来像「这个符号没有定义」——最难查的假阴性。这里每次请求前
//!    自动同步：没开就 `didOpen`（版本 1），内容变了就 `didChange`（full，版本 +1），
//!    内容没变则**不重复发**（否则版本号乱涨，`rust-analyzer` 会当成真变更重算）。
//! 2. **坐标是 0-based**。LSP 的 `line` / `character` 从 0 开始，而所有给用户看
//!    的位置（「第 12 行第 5 列」）从 1 开始。这里把**入参**收成
//!    [`Position`]（0-based，对齐协议），把**出参**统一成 [`SourcePosition`]
//!    （1-based，对齐 UI），转换只在一个地方做。
//! 3. **回包形态有四五种**。`definition` 可能是 `Location` / `Location[]` /
//!    `LocationLink[]` / `null`；`hover.contents` 可能是字符串 / MarkupContent /
//!    MarkedString 数组；`completion` 可能是数组或 `{items}` 列表。这里全部归一，
//!    上层拿到的永远是同一种结构。
//!
//! ## 1-based 的精度说明
//!
//! `character` 在 LSP 里是 UTF-16 码元偏移，加一后得到的是「第几个 UTF-16 单元」
//! 而不是「第几个字符」。含 emoji / 生僻字（代理对）的行上两者会差一，本层不
//! 做码点换算——要精确到字形簇得先拿到整行文本，那属于编辑器侧的活儿。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::client::LspClient;
use crate::document::{path_to_uri, uri_to_path};
use crate::error::LspError;

/// `textDocument/definition`。
pub const METHOD_DEFINITION: &str = "textDocument/definition";
/// `textDocument/references`。
pub const METHOD_REFERENCES: &str = "textDocument/references";
/// `textDocument/hover`。
pub const METHOD_HOVER: &str = "textDocument/hover";
/// `textDocument/completion`。
pub const METHOD_COMPLETION: &str = "textDocument/completion";

/// 请求用的位置，**0-based**（与 LSP 线格式一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    /// 行号，从 0 开始。
    pub line: u32,
    /// 行内偏移，从 0 开始（UTF-16 码元）。
    pub character: u32,
}

impl Position {
    /// 构造一个 0-based 位置。
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }

    /// 文件开头。
    pub const fn start() -> Self {
        Self {
            line: 0,
            character: 0,
        }
    }

    fn to_lsp(self) -> Value {
        json!({ "line": self.line, "character": self.character })
    }
}

/// 结果里的位置，**1-based**（直接给 UI / 日志用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourcePosition {
    /// 行号，从 1 开始。
    pub line: u32,
    /// 行内偏移，从 1 开始。
    pub character: u32,
}

impl SourcePosition {
    /// 构造一个 1-based 位置。
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }

    /// 从 LSP 的 0-based 位置换算（`+1`，下溢时钳到 1）。
    pub fn from_lsp(position: &Value) -> Option<Self> {
        let line = position.get("line")?.as_u64()?;
        let character = position.get("character")?.as_u64()?;
        let line = u32::try_from(line).ok()?.saturating_add(1);
        let character = u32::try_from(character).ok()?.saturating_add(1);
        Some(Self { line, character })
    }
}

/// 1-based 的区间（半开：含 `start`，不含 `end`，与 LSP 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRange {
    /// 起点。
    pub start: SourcePosition,
    /// 终点（不含）。
    pub end: SourcePosition,
}

impl SourceRange {
    /// 从 LSP 的 `range` 对象换算。
    pub fn from_lsp(value: &Value) -> Option<Self> {
        Some(Self {
            start: SourcePosition::from_lsp(value.get("start")?)?,
            end: SourcePosition::from_lsp(value.get("end")?)?,
        })
    }
}

/// 归一后的位置结果（`definition` / `references` 用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLocation {
    /// 原始 uri（服务器给的，可能不是 `file:`）。
    pub uri: String,
    /// 可转换出磁盘路径时给出（`untitled:` / `git:` 这类为 `None`）。
    pub path: Option<PathBuf>,
    /// 1-based 区间。
    pub range: SourceRange,
}

impl SourceLocation {
    /// 从 LSP 的 `Location` / `LocationLink` 归一。
    fn from_lsp(value: &Value) -> Option<Self> {
        // `LocationLink` 用 `targetUri` + `targetSelectionRange`（选中标识符本身），
        // 没有选中区间时退回 `targetRange`（整个符号体）。
        let uri = value
            .get("uri")
            .or_else(|| value.get("targetUri"))
            .and_then(Value::as_str)?;
        let range = value
            .get("range")
            .or_else(|| value.get("targetSelectionRange"))
            .or_else(|| value.get("targetRange"))
            .and_then(SourceRange::from_lsp)?;
        Some(Self {
            uri: uri.to_string(),
            path: uri_to_path(uri),
            range,
        })
    }
}

/// 归一后的悬停信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverInfo {
    /// 内容（Markdown 或纯文本，服务器 `kind` 字段未区分，原样拼接）。
    pub contents: String,
    /// 悬停实际作用的区间（服务器可省）。
    pub range: Option<SourceRange>,
}

/// 一个补全项（只保留上层会展示/插入的字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    /// 展示文本，也是唯一的必填字段。
    pub label: String,
    /// LSP `CompletionItemKind`（1 文本、3 函数、6 变量…）。
    pub kind: Option<u32>,
    /// 右侧灰字说明（类型签名等）。
    pub detail: Option<String>,
    /// 文档说明（Markdown 或纯文本）。
    pub documentation: Option<String>,
    /// 插入文本（服务器要求替换 `label` 时给）。
    pub insert_text: Option<String>,
    /// 排序键。
    pub sort_text: Option<String>,
}

impl CompletionItem {
    fn from_lsp(value: &Value) -> Option<Self> {
        let label = value.get("label").and_then(Value::as_str)?;
        Some(Self {
            label: label.to_string(),
            kind: value
                .get("kind")
                .and_then(Value::as_u64)
                .and_then(|kind| u32::try_from(kind).ok()),
            detail: string_field(value, "detail"),
            documentation: value.get("documentation").and_then(markup_text),
            insert_text: string_field(value, "insertText"),
            sort_text: string_field(value, "sortText"),
        })
    }
}

/// 归一后的补全列表。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompletionList {
    /// 服务器是否认为结果不完整（`true` 时编辑器应继续按前缀过滤）。
    pub is_incomplete: bool,
    /// 候选项。
    pub items: Vec<CompletionItem>,
}

impl CompletionList {
    /// 候选数量。
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// 是否没有候选。
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// 一次自动同步的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncedDocument {
    /// 文档 uri。
    pub uri: String,
    /// 同步后的版本号（`didOpen` 为 1，之后每次真变更 +1）。
    pub version: i32,
    /// 本次是否真的发了 `didOpen`。
    pub opened: bool,
    /// 本次是否真的发了 `didChange`。
    pub changed: bool,
}

impl LspClient {
    /// 把磁盘上的文本同步给服务器（需要时自动 `didOpen` / `didChange`）。
    ///
    /// 幂等：同一份文本重复调用不会重复发变更、不会推版本号。
    pub async fn sync_document(&self, path: &Path, text: &str) -> Result<SyncedDocument, LspError> {
        let uri = path_to_uri(path);
        if !self.documents().is_open(&uri) {
            self.open_document(path, text).await?;
            return Ok(SyncedDocument {
                version: self.documents().version(&uri).unwrap_or(1),
                uri,
                opened: true,
                changed: false,
            });
        }

        let unchanged = self.documents().text(&uri).as_deref() == Some(text);
        if unchanged {
            return Ok(SyncedDocument {
                version: self.documents().version(&uri).unwrap_or(0),
                uri,
                opened: false,
                changed: false,
            });
        }

        let version = self.change_document(&uri, text).await?;
        Ok(SyncedDocument {
            uri,
            version,
            opened: false,
            changed: true,
        })
    }

    /// 跳转定义。`position` 为 0-based；结果统一成 1-based 区间（可能为空）。
    pub async fn definition(
        &self,
        path: &Path,
        text: &str,
        position: Position,
    ) -> Result<Vec<SourceLocation>, LspError> {
        let synced = self.sync_document(path, text).await?;
        let value = self
            .request(
                METHOD_DEFINITION,
                Some(position_params(&synced.uri, position)),
            )
            .await?;
        Ok(parse_locations(&value))
    }

    /// 查找引用。`include_declaration` 决定是否把定义本身算进去（LSP 的
    /// `context.includeDeclaration`，不传时各服务器默认值不一致，所以显式给）。
    pub async fn references(
        &self,
        path: &Path,
        text: &str,
        position: Position,
        include_declaration: bool,
    ) -> Result<Vec<SourceLocation>, LspError> {
        let synced = self.sync_document(path, text).await?;
        let mut params = position_params(&synced.uri, position);
        params["context"] = json!({ "includeDeclaration": include_declaration });
        let value = self.request(METHOD_REFERENCES, Some(params)).await?;
        Ok(parse_locations(&value))
    }

    /// 悬停信息。`null` 结果归一成 `Ok(None)`（没有可展示内容，不是错误）。
    pub async fn hover(
        &self,
        path: &Path,
        text: &str,
        position: Position,
    ) -> Result<Option<HoverInfo>, LspError> {
        let synced = self.sync_document(path, text).await?;
        let value = self
            .request(METHOD_HOVER, Some(position_params(&synced.uri, position)))
            .await?;
        Ok(parse_hover(&value))
    }

    /// 补全候选。列表形态与数组形态都归一成 [`CompletionList`]。
    pub async fn completion(
        &self,
        path: &Path,
        text: &str,
        position: Position,
    ) -> Result<CompletionList, LspError> {
        let synced = self.sync_document(path, text).await?;
        let value = self
            .request(
                METHOD_COMPLETION,
                Some(position_params(&synced.uri, position)),
            )
            .await?;
        Ok(parse_completion(&value))
    }
}

fn position_params(uri: &str, position: Position) -> Value {
    json!({
        "textDocument": { "uri": uri },
        "position": position.to_lsp(),
    })
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// 把 `contents`（字符串 / MarkupContent / MarkedString / 数组）拼成一段文本。
fn markup_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Object(map) => map.get("value").and_then(Value::as_str).map(str::to_string),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(markup_text).collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n\n"))
            }
        }
        _ => None,
    }
}

/// `definition` / `references` 回包归一。
///
/// 兼容 `null`、单个 `Location`、`Location[]`、`LocationLink[]`；缺 `uri` 或
/// `range` 的条目直接跳过（宁可少一条，也不要给上层一个坐标缺失的位置去跳转）。
pub fn parse_locations(value: &Value) -> Vec<SourceLocation> {
    match value {
        Value::Array(items) => items.iter().filter_map(SourceLocation::from_lsp).collect(),
        Value::Object(_) => SourceLocation::from_lsp(value).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// `hover` 回包归一。没有 `contents` 或内容全为空时返回 `None`。
pub fn parse_hover(value: &Value) -> Option<HoverInfo> {
    let contents = markup_text(value.get("contents")?)?;
    if contents.is_empty() {
        return None;
    }
    Some(HoverInfo {
        contents,
        range: value.get("range").and_then(SourceRange::from_lsp),
    })
}

/// `completion` 回包归一（列表形态 `{isIncomplete, items}` 与裸数组都吃）。
pub fn parse_completion(value: &Value) -> CompletionList {
    let (is_incomplete, items): (bool, &[Value]) = match value {
        Value::Array(items) => (false, items.as_slice()),
        Value::Object(map) => {
            let is_incomplete = map
                .get("isIncomplete")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let items = map.get("items").and_then(Value::as_array);
            (is_incomplete, items.map_or(&[], Vec::as_slice))
        }
        _ => return CompletionList::default(),
    };
    CompletionList {
        is_incomplete,
        items: items.iter().filter_map(CompletionItem::from_lsp).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_shift_from_zero_based_to_one_based() {
        let lsp = json!({"start": {"line": 0, "character": 3}, "end": {"line": 2, "character": 0}});
        let range = SourceRange::from_lsp(&lsp).expect("必须能解析");
        assert_eq!(range.start, SourcePosition::new(1, 4));
        assert_eq!(range.end, SourcePosition::new(3, 1));
        assert_eq!(range.start.line, 1, "0 行必须映射成 1 行");
    }

    #[test]
    fn malformed_positions_are_rejected() {
        assert!(SourceRange::from_lsp(&json!({"start": {"line": 1}})).is_none());
        assert!(SourceRange::from_lsp(&json!({"line": 1, "character": 2})).is_none());
        assert_eq!(parse_locations(&Value::Null), Vec::new());
    }

    #[test]
    fn definition_accepts_single_location_and_location_links() {
        let single = json!({
            "uri": "file:///E:/ai_ide/src/a.ts",
            "range": {"start": {"line": 9, "character": 1}, "end": {"line": 9, "character": 6}}
        });
        let locations = parse_locations(&single);
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].path, Some(PathBuf::from("E:/ai_ide/src/a.ts")));
        assert_eq!(locations[0].range.start, SourcePosition::new(10, 2));

        let links = json!([{
            "targetUri": "file:///E:/ai_ide/src/b.ts",
            "targetRange": {"start": {"line": 0, "character": 0}, "end": {"line": 3, "character": 0}},
            "targetSelectionRange": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 7}}
        }]);
        let locations = parse_locations(&links);
        assert_eq!(locations.len(), 1);
        // 优先用 targetSelectionRange（标识符本身），不是整个符号体。
        assert_eq!(locations[0].range.start, SourcePosition::new(2, 3));
        assert_eq!(locations[0].range.end, SourcePosition::new(2, 8));
    }

    #[test]
    fn non_file_uris_keep_the_uri_but_have_no_path() {
        let locations = parse_locations(&json!([{
            "uri": "untitled:Untitled-1",
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}
        }]));
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].path, None);
        assert_eq!(locations[0].uri, "untitled:Untitled-1");
    }

    #[test]
    fn hover_contents_support_string_markup_and_marked_string_arrays() {
        let plain = parse_hover(&json!({"contents": "let a: i32"})).expect("字符串形态");
        assert_eq!(plain.contents, "let a: i32");
        assert!(plain.range.is_none());

        let markup = parse_hover(&json!({
            "contents": {"kind": "markdown", "value": "```rust\nfn f()\n```"},
            "range": {"start": {"line": 4, "character": 0}, "end": {"line": 4, "character": 3}}
        }))
        .expect("MarkupContent 形态");
        assert_eq!(markup.contents, "```rust\nfn f()\n```");
        assert_eq!(
            markup.range.expect("应有 range").start,
            SourcePosition::new(5, 1)
        );

        let array = parse_hover(&json!({
            "contents": [
                {"language": "rust", "value": "fn f()"},
                "文档说明"
            ]
        }))
        .expect("MarkedString 数组形态");
        assert_eq!(array.contents, "fn f()\n\n文档说明");

        assert!(parse_hover(&json!({"contents": ""})).is_none());
        assert!(parse_hover(&json!({})).is_none());
        assert!(parse_hover(&Value::Null).is_none());
    }

    #[test]
    fn completion_accepts_list_and_bare_array() {
        let list = parse_completion(&json!({
            "isIncomplete": true,
            "items": [
                {"label": "alpha", "kind": 3, "detail": "() => void", "insertText": "alpha()",
                 "sortText": "0001", "documentation": {"kind": "markdown", "value": "说明"}},
                {"label": "beta"}
            ]
        }));
        assert!(list.is_incomplete);
        assert_eq!(list.len(), 2);
        assert_eq!(list.items[0].label, "alpha");
        assert_eq!(list.items[0].kind, Some(3));
        assert_eq!(list.items[0].detail.as_deref(), Some("() => void"));
        assert_eq!(list.items[0].documentation.as_deref(), Some("说明"));
        assert_eq!(list.items[0].insert_text.as_deref(), Some("alpha()"));
        assert_eq!(list.items[0].sort_text.as_deref(), Some("0001"));
        assert_eq!(list.items[1].label, "beta");
        assert!(list.items[1].kind.is_none());

        let bare = parse_completion(&json!([{"label": "gamma"}]));
        assert!(!bare.is_incomplete);
        assert_eq!(bare.len(), 1);

        let empty = parse_completion(&Value::Null);
        assert!(empty.is_empty());
        assert!(!empty.is_incomplete);
    }

    #[test]
    fn position_params_use_zero_based_fields() {
        let params = position_params("file:///E:/a.ts", Position::new(3, 7));
        assert_eq!(params["textDocument"]["uri"], json!("file:///E:/a.ts"));
        assert_eq!(params["position"]["line"], json!(3));
        assert_eq!(params["position"]["character"], json!(7));
    }
}
