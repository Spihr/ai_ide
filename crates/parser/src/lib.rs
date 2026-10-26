//! # ai-ide-parser
//!
//! 语法解析层：把「这行是不是声明」从正则猜测升级成**语法树判定**。
//!
//! ## 为什么单独一层
//!
//! M3 的 `search.symbols` 用的是 [`ai_ide_indexer`] 里的「行首声明 + 作用域跟踪」文本抽取：
//! 它对 `fn foo(` 这类行首形态够用，但遇到多行签名、`export const`、箭头函数、
//! 装饰器、字符串里疑似声明的文本就会误判。tree-sitter 给的是真正的语法树，
//! 判定点是节点种类而不是行首字符，因此：
//!
//! 1. **符号表准确**：`method` 与 `function` 按「是否定义在类型体内」区分，
//!    不再靠花括号深度猜。
//! 2. **可判语法健康**：应用补丁后能检查 `ERROR` 节点，给 Agent 的「应用即校验」提供依据。
//!
//! ## 语言支持
//!
//! 本轮先接 TypeScript / TSX / Python 三种（docs/02 §5.1 的「TS/Python 先行」）。
//! 其余语言走 [`supports`] 返回 `false`，调用方回落到原有文本抽取，行为不回退。
//!
//! ## 行号口径
//!
//! 所有行号 **1 起算、闭区间**，与 [`ai_ide_protocol::SymbolRange`] 一致。
//! tree-sitter 的 `Point::row` 是 0 起算，这里统一 `+ 1`。

use ai_ide_protocol::{Symbol, SymbolKind, SymbolRange};
use tree_sitter::{Language, Node, Parser};

/// 本层认识的语法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    /// TypeScript（`.ts` / `.mts` / `.cts`）。
    TypeScript,
    /// TSX（`.tsx`）。
    Tsx,
    /// Python（`.py` / `.pyi`）。
    Python,
}

impl Syntax {
    /// 该语法对应的 tree-sitter 语言。
    pub fn language(self) -> Language {
        match self {
            Syntax::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Syntax::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Syntax::Python => tree_sitter_python::LANGUAGE.into(),
        }
    }

    fn parser(self) -> Option<Parser> {
        let mut parser = Parser::new();
        parser.set_language(&self.language()).ok()?;
        Some(parser)
    }
}

/// 按路径后缀选语法；不认识的返回 `None`（调用方据此回落）。
///
/// 只认后缀，不读内容：这是热路径（索引时会逐文件调用），且 MIME 嗅探对源码没有意义。
pub fn syntax_for(path: &str) -> Option<Syntax> {
    let lower = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    // 用「最后一个点之后」判后缀，避免 `foo.test.ts` 这类名字里带点的走偏。
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "ts" | "mts" | "cts" => Some(Syntax::TypeScript),
        "tsx" => Some(Syntax::Tsx),
        "py" | "pyi" => Some(Syntax::Python),
        _ => None,
    }
}

/// 该文件能否走语法树抽取。
pub fn supports(path: &str) -> bool {
    syntax_for(path).is_some()
}

/// 用 tree-sitter 抽取符号。
///
/// - `None`：路径后缀不在支持范围（调用方回落文本抽取）。
/// - `Some(vec)`：即便解析出 `ERROR` 节点也返回能认出的那部分——半成品文件的符号表
///   比空表有用（用户正在打字时前端就在查符号）。
pub fn extract_symbols(path: &str, source: &str) -> Option<Vec<Symbol>> {
    let syntax = syntax_for(path)?;
    let mut parser = syntax.parser()?;
    let tree = parser.parse(source, None)?;
    let bytes = source.as_bytes();

    let mut out = Vec::new();
    walk(tree.root_node(), bytes, path, syntax, None, &mut out);
    // 同一符号可能被 `export` / `decorator` 包裹层重复访问，按「名字 + 种类 + 起始行」去重。
    out.sort_by(|a, b| {
        a.range
            .start_line
            .cmp(&b.range.start_line)
            .then(a.range.end_line.cmp(&b.range.end_line))
            .then(a.name.cmp(&b.name))
    });
    out.dedup_by(|a, b| {
        a.name == b.name && a.kind == b.kind && a.range.start_line == b.range.start_line
    });
    Some(out)
}

/// 语法健康度：给「应用即校验」和符号浮层用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxHealth {
    /// 是否含 `ERROR` 节点或缺内容节点。
    pub has_error: bool,
    /// `ERROR` 节点个数（不递归统计重复父节点）。
    pub error_count: usize,
    /// 第一个 `ERROR` 节点所在行（1 起算）。
    pub first_error_line: Option<u32>,
}

/// 解析并统计语法错误；路径不支持时返回 `None`。
pub fn health(path: &str, source: &str) -> Option<SyntaxHealth> {
    let syntax = syntax_for(path)?;
    let mut parser = syntax.parser()?;
    let tree = parser.parse(source, None)?;
    let root = tree.root_node();
    if !root.has_error() {
        return Some(SyntaxHealth {
            has_error: false,
            error_count: 0,
            first_error_line: None,
        });
    }
    let mut count = 0;
    let mut first: Option<u32> = None;
    let mut cursor = root.walk();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_error() {
            count += 1;
            let line = node.start_position().row as u32 + 1;
            first = Some(match first {
                Some(prev) => prev.min(line),
                None => line,
            });
            // ERROR 子树内部不再计（父节点已经把整段算作一次错误）。
            continue;
        }
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    Some(SyntaxHealth {
        has_error: true,
        error_count: count,
        first_error_line: first,
    })
}

/// 递归遍历语法树，把声明节点翻成 [`Symbol`]。
///
/// `container` 记录「当前是否在类型体内」，用来区分 `function` 与 `method`
/// （TS 的 `method_definition` 本身已足够，但 `export default function` 之类包裹层
/// 需要把外层的上下文带下去）。
fn walk(
    node: Node<'_>,
    bytes: &[u8],
    path: &str,
    syntax: Syntax,
    container: Option<&str>,
    out: &mut Vec<Symbol>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(child, bytes, path, syntax, container, out);
    }
}

/// 进入「声明体」的子节点（类体、命名空间体）。
///
/// 不能直接 `walk(父节点)`：类体是 `class_body`、命名空间体是 `statement_block`，
/// 两者都在 [`visit`] 的默认分支里被跳过（否则函数体里的局部声明会漏进符号表）。
fn walk_body(
    node: Node<'_>,
    bytes: &[u8],
    path: &str,
    syntax: Syntax,
    container: Option<&str>,
    out: &mut Vec<Symbol>,
) {
    if let Some(body) = node.child_by_field_name("body") {
        walk(body, bytes, path, syntax, container, out);
    }
}

fn visit(
    node: Node<'_>,
    bytes: &[u8],
    path: &str,
    syntax: Syntax,
    container: Option<&str>,
    out: &mut Vec<Symbol>,
) {
    match syntax {
        Syntax::TypeScript | Syntax::Tsx => visit_ts(node, bytes, path, syntax, container, out),
        Syntax::Python => visit_py(node, bytes, path, container, out),
    }
}

/// TS / TSX 的节点种类映射。
///
/// 只认「声明类」节点；表达式里出现的函数（箭头函数）视为实现细节，不进符号表，
/// 否则列表会被大量匿名回调淹没。
fn visit_ts(
    node: Node<'_>,
    bytes: &[u8],
    path: &str,
    syntax: Syntax,
    container: Option<&str>,
    out: &mut Vec<Symbol>,
) {
    let kind = node.kind();
    match kind {
        // 包裹层：`export ...`、`declare ...`、`export default ...`
        "export_statement" | "ambient_declaration" => {
            walk(node, bytes, path, syntax, container, out);
        }
        // `namespace X { ... }`：声明在 `body` 里，直接把 body 的子节点当模块层看。
        "internal_module" | "module" => {
            walk_body(node, bytes, path, syntax, container, out);
        }
        "function_declaration" | "generator_function_declaration" => {
            push_named(
                node,
                bytes,
                path,
                SymbolKind::Function,
                out,
                container.is_some(),
            );
        }
        "method_definition" => {
            push_named(node, bytes, path, SymbolKind::Method, out, true);
        }
        "class_declaration" | "abstract_class_declaration" => {
            if let Some(name) = push_named(node, bytes, path, SymbolKind::Class, out, false) {
                walk_body(node, bytes, path, syntax, Some(&name), out);
            }
        }
        "class" => {
            // `const X = class {}`：有名字才收，祖先已在 `lexical_declaration` 里处理常量。
            walk(node, bytes, path, syntax, Some("<anonymous>"), out);
        }
        "interface_declaration" => {
            push_named(node, bytes, path, SymbolKind::Interface, out, false);
        }
        "enum_declaration" => {
            push_named(node, bytes, path, SymbolKind::Enum, out, false);
        }
        "lexical_declaration" => {
            collect_ts_declarators(node, bytes, path, out);
        }
        _ => {
            // 其余节点不递归：声明只会出现在语句 / 类体 / 模块体层，
            // 继续下探会把函数体里的局部变量也算进来。
        }
    }
}

/// `const` 声明的三种形态，分别映射成不同符号：
///
/// - `const X = () => {}` / `= function () {}` → **function**（React 组件与工具函数的
///   主流写法，当成常量会让符号列表里全是 `Const`）。
/// - `const X = class {}` → **class**（命名类表达式）。
/// - 其余 `const X = ...` 且名字全大写 → **const**（全大写是命名约定层面的常量信号，
///   避免把 `const count = 0` 这类局部状态塞进符号表）。
///
/// 只处理模块层：函数体里的 `const` 是实现细节，不属于「符号表」的粒度。
fn collect_ts_declarators(node: Node<'_>, bytes: &[u8], path: &str, out: &mut Vec<Symbol>) {
    if !is_top_level(node) {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "variable_declarator" {
            continue;
        }
        let Some(name_node) = child.child_by_field_name("name") else {
            continue;
        };
        if name_node.kind() != "identifier" {
            continue;
        }
        let Ok(name) = name_node.utf8_text(bytes) else {
            continue;
        };
        let value_kind = child
            .child_by_field_name("value")
            .map(|v| v.kind())
            .unwrap_or("");
        let kind = match value_kind {
            "arrow_function" | "function_expression" | "function" | "generator_function" => {
                Some(SymbolKind::Function)
            }
            "class" => Some(SymbolKind::Class),
            _ if is_const_name(name) => Some(SymbolKind::Const),
            _ => None,
        };
        let Some(kind) = kind else {
            continue;
        };
        out.push(Symbol {
            name: name.to_string(),
            kind,
            path: path.to_string(),
            range: range_of(child),
        });
    }
}

/// Python 的节点种类映射。
fn visit_py(
    node: Node<'_>,
    bytes: &[u8],
    path: &str,
    container: Option<&str>,
    out: &mut Vec<Symbol>,
) {
    match node.kind() {
        "decorated_definition" => {
            // 装饰器是 `decorated_definition` 的 `definition` 子节点，行范围要含装饰器行。
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if matches!(child.kind(), "function_definition" | "class_definition") {
                    push_py_decl(child, Some(node), bytes, path, container, out);
                }
            }
        }
        "function_definition" => {
            push_py_decl(node, None, bytes, path, container, out);
        }
        "class_definition" => {
            push_py_decl(node, None, bytes, path, container, out);
        }
        // 模块级全大写赋值 → const。
        "expression_statement" if is_top_level(node) => {
            collect_py_consts(node, bytes, path, out);
        }
        _ => {}
    }
}

/// 把一个 Python 声明推入结果；`outer` 是装饰器包裹层（用于把行范围起点提到装饰器）。
fn push_py_decl(
    node: Node<'_>,
    outer: Option<Node<'_>>,
    bytes: &[u8],
    path: &str,
    container: Option<&str>,
    out: &mut Vec<Symbol>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Ok(name) = name_node.utf8_text(bytes) else {
        return;
    };
    let kind = match node.kind() {
        "class_definition" => SymbolKind::Class,
        _ => {
            if container.is_some() {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            }
        }
    };
    let anchor = outer.unwrap_or(node);
    let symbol = Symbol {
        name: name.to_string(),
        kind,
        path: path.to_string(),
        range: range_of(anchor),
    };
    out.push(symbol);
    if kind == SymbolKind::Class {
        // 类体内部继续找方法。
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk(child, bytes, path, Syntax::Python, Some(name), out);
        }
    }
}

fn collect_py_consts(node: Node<'_>, bytes: &[u8], path: &str, out: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "assignment" {
            continue;
        }
        let Some(left) = child.child_by_field_name("left") else {
            continue;
        };
        if left.kind() != "identifier" {
            continue;
        }
        let Ok(name) = left.utf8_text(bytes) else {
            continue;
        };
        if !is_const_name(name) {
            continue;
        }
        out.push(Symbol {
            name: name.to_string(),
            kind: SymbolKind::Const,
            path: path.to_string(),
            range: range_of(child),
        });
    }
}

/// 推入一个「有 name 字段」的声明，返回名字（供类体继续下探）。
fn push_named(
    node: Node<'_>,
    bytes: &[u8],
    path: &str,
    kind: SymbolKind,
    out: &mut Vec<Symbol>,
    as_method: bool,
) -> Option<String> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(bytes).ok()?.to_string();
    let kind = if as_method && kind == SymbolKind::Function {
        SymbolKind::Method
    } else {
        kind
    };
    out.push(Symbol {
        name: name.clone(),
        kind,
        path: path.to_string(),
        range: range_of(node),
    });
    Some(name)
}

/// 节点的 1 起算闭区间行范围。
fn range_of(node: Node<'_>) -> SymbolRange {
    SymbolRange {
        start_line: node.start_position().row as u32 + 1,
        end_line: node.end_position().row as u32 + 1,
    }
}

/// 是否处于模块 / 语句层（父链上没有可执行体）。
fn is_top_level(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return true;
    };
    matches!(
        parent.kind(),
        "program" | "module" | "program_repeat" | "export_statement" | "ambient_declaration"
    )
}

/// 常量名口径：至少两个字符、无小写字母、含字母。
///
/// 单字符常量（`X`）噪声太大；`A1` 这类算常量（全大写），`foo` 不算。
fn is_const_name(name: &str) -> bool {
    let mut has_alpha = false;
    let mut has_lower = false;
    let mut len = 0usize;
    for c in name.chars() {
        len += 1;
        if c.is_alphabetic() {
            has_alpha = true;
            if c.is_lowercase() {
                has_lower = true;
            }
        }
    }
    has_alpha && !has_lower && len >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(symbols: &[Symbol]) -> Vec<String> {
        symbols
            .iter()
            .map(|s| {
                format!(
                    "{:?}:{}:{}-{}",
                    s.kind, s.name, s.range.start_line, s.range.end_line
                )
            })
            .collect()
    }

    #[test]
    fn syntax_by_extension() {
        assert_eq!(syntax_for("src/a.ts"), Some(Syntax::TypeScript));
        assert_eq!(syntax_for("src/a.mts"), Some(Syntax::TypeScript));
        assert_eq!(syntax_for("src/a.tsx"), Some(Syntax::Tsx));
        assert_eq!(syntax_for("pkg/mod.py"), Some(Syntax::Python));
        assert_eq!(syntax_for("pkg/stub.pyi"), Some(Syntax::Python));
        assert_eq!(syntax_for("src/main.rs"), None);
        assert_eq!(syntax_for("Makefile"), None);
        assert!(!supports("src/main.go"));
    }

    #[test]
    fn typescript_functions_and_classes() {
        let src = r#"export function top(a: number): number {
  return a;
}

export default function defaulted() {}

export const MAX_RETRY = 3;
const local = 1;

export class Service {
  private count = 0;

  run() {
    const inner = () => 1;
    return inner();
  }

  static create(): Service {
    return new Service();
  }
}

export interface Port {
  send(x: string): void;
}

export enum Mode {
  Fast,
}
"#;
        let symbols = extract_symbols("src/a.ts", src).expect("ts supported");
        let got = names(&symbols);
        assert_eq!(
            got,
            vec![
                "Function:top:1-3",
                "Function:defaulted:5-5",
                "Const:MAX_RETRY:7-7",
                "Class:Service:10-21",
                "Method:run:13-16",
                "Method:create:18-20",
                "Interface:Port:23-25",
                "Enum:Mode:27-29",
            ]
        );
    }

    #[test]
    fn tsx_component_and_hooks() {
        let src = r#"export function App() {
  return <div />;
}

const API_BASE = '/api';

export default function Panel(props: { title: string }) {
  return <section>{props.title}</section>;
}
"#;
        let symbols = extract_symbols("src/App.tsx", src).expect("tsx supported");
        let got = names(&symbols);
        assert_eq!(
            got,
            vec![
                "Function:App:1-3",
                "Const:API_BASE:5-5",
                "Function:Panel:7-9",
            ]
        );
    }

    #[test]
    fn arrow_function_consts_are_functions() {
        let src = r#"export const Button = (props: { label: string }) => {
  return null;
};

const helper = async function () {};

const MAX = 10;
const counter = 0;
"#;
        let symbols = extract_symbols("src/Button.tsx", src).expect("tsx supported");
        assert_eq!(
            names(&symbols),
            vec![
                "Function:Button:1-3",
                "Function:helper:5-5",
                "Const:MAX:7-7",
            ]
        );
    }

    #[test]
    fn python_functions_methods_and_decorators() {
        let src = r#"TIMEOUT = 30
retries = 3


def helper(x):
    return x


@cache(maxsize=8)
def cached(x):
    return x


class Store:
    LIMIT = 10

    def get(self, key):
        return key

    @staticmethod
    def build():
        return Store()


class Empty:
    pass
"#;
        let symbols = extract_symbols("pkg/store.py", src).expect("py supported");
        let got = names(&symbols);
        assert_eq!(
            got,
            vec![
                "Const:TIMEOUT:1-1",
                "Function:helper:5-6",
                "Function:cached:9-11",
                "Class:Store:14-22",
                "Method:get:17-18",
                "Method:build:20-22",
                "Class:Empty:25-26",
            ]
        );
    }

    #[test]
    fn string_literals_do_not_produce_symbols() {
        // 文本抽取最容易在这里翻车：字符串里的 `function` 被当成声明。
        let src = r#"const help = "function fake() {}";

function real() {}
"#;
        let symbols = extract_symbols("src/x.ts", src).expect("ts supported");
        assert_eq!(names(&symbols), vec!["Function:real:3-3"]);
    }

    #[test]
    fn error_recovery_keeps_partial_symbols() {
        let src = "export function ok() {}\n\nexport function broken( {\n";
        let symbols = extract_symbols("src/broken.ts", src).expect("ts supported");
        assert!(symbols.iter().any(|s| s.name == "ok"));
        let h = health("src/broken.ts", src).expect("ts supported");
        assert!(h.has_error);
        assert!(h.error_count >= 1);
        assert!(h.first_error_line.is_some());
    }

    #[test]
    fn healthy_source_has_no_error() {
        let h = health("a.py", "def f():\n    return 1\n").expect("py supported");
        assert!(!h.has_error);
        assert_eq!(h.error_count, 0);
        assert_eq!(h.first_error_line, None);
    }

    #[test]
    fn unsupported_paths_return_none() {
        assert!(extract_symbols("src/main.rs", "fn main() {}").is_none());
        assert!(health("README.md", "# hi").is_none());
    }

    #[test]
    fn anonymous_class_and_nested_declarations_are_not_duplicated() {
        let src = r#"export namespace inner {
  export function helper() {}
}

const Factory = class {};
"#;
        let symbols = extract_symbols("src/n.ts", src).expect("ts supported");
        let got = names(&symbols);
        // 命名空间里的函数要收；`const Factory = class {}` 是命名类表达式，
        // 按变量名记成 class（而不是既漏 class 又多一条 const）。
        assert_eq!(got, vec!["Function:helper:2-2", "Class:Factory:5-5"]);
    }
}
