//! 符号抽取（`search.symbols` 的内核面）。
//!
//! ## 为什么不是真正的语法分析
//!
//! 七种语言各引一棵解析器（`syn` / `swc` / `tree-sitter` / …）能给到精确的 AST，
//! 代价是编译时间、依赖体积与「同一份文件被两种语法树描述」的维护成本。而前端要的
//! 只是「这个声明叫什么、在哪、覆盖到哪一行」——**逐行正则 + 块级深度跟踪**就够用，
//! 且误报/漏报都只影响这次查询的质量，不会污染索引或磁盘。
//!
//! 因此这里明确接受三类近似：
//!
//! 1. **只认行首声明**。`let x = || {};` 这类表达式里的函数不会被当成符号；
//!    反过来说，缩进后的嵌套声明（Rust 的 `impl` 内、TS 的 `class` 内）靠作用域栈识别。
//! 2. **`end_line` 是块级近似**。花括号语言按 `{}` 深度闭合算，Python 按缩进算；
//!    单行声明（`fn main() {}`）的 `start_line == end_line`。
//! 3. **字符串与注释先抹平再计数**。`format!("{")` 里的花括号若被计入，后面所有
//!    作用域都会错位——所以 [`code_only`] 会把注释与字面量替换成空格，并保留
//!    Rust 的生命周期标记（`&'a str` 不是字符字面量）。
//!
//! ## 与索引的关系
//!
//! 抽取本身不遍历目录：文件清单来自 [`FileIndex`]（`workspace.open` 已建好，或由
//! 调用方兜底建一次）。这样忽略规则、隐藏文件口径、路径分隔符都与 `search.files`
//! 完全一致，不会出现「树里看不见、符号却能搜到」。

use ai_ide_protocol::{SearchSymbolsParams, SearchSymbolsResult, Symbol, SymbolKind, SymbolRange};
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

use crate::FileIndex;

/// 一次响应最多返回多少条符号。
///
/// 上限固定在内核而不是入参：符号检索的典型用法是「定位一个定义」，200 条之后
/// 的命中对判断没有帮助，反而是把整个工作区的声明清单灌给前端。
pub const DEFAULT_MAX_SYMBOLS: usize = 200;

/// 单个文件参与抽取的字节上限（1 MiB）。
///
/// 超过这个体量的源码文件通常是生成物（打包产物、压缩后的 JS），抽取它们只会
/// 拖慢一次查询，且结果对人没有意义。
pub const MAX_SYMBOL_FILE_BYTES: u64 = 1024 * 1024;

/// 支持的源码语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    /// `.ts` / `.tsx`：TSX 的 JSX 只在表达式里，声明语法与 TS 一致，共用一套规则。
    TypeScript,
    /// `.js` / `.jsx` / `.mjs` / `.cjs`。
    JavaScript,
    Python,
    Go,
    Java,
}

fn lang_of(rel: &str) -> Option<Lang> {
    let ext = rel.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Some(Lang::Rust),
        "ts" | "tsx" => Some(Lang::TypeScript),
        "js" | "jsx" | "mjs" | "cjs" => Some(Lang::JavaScript),
        "py" | "pyi" => Some(Lang::Python),
        "go" => Some(Lang::Go),
        "java" => Some(Lang::Java),
        _ => None,
    }
}

/// 按协议参数检索符号。
///
/// 单文件读取失败（权限、非 UTF-8、超限）只跳过该文件：符号检索是「尽力而为」的
/// 浏览型查询，一个坏文件不该让整次查询报错。
pub fn search(root: &Path, index: &FileIndex, params: &SearchSymbolsParams) -> SearchSymbolsResult {
    let query = params.query.trim().to_lowercase();
    let kinds = params.kinds.clone().unwrap_or_default();

    let mut hits: Vec<(u8, Symbol)> = Vec::new();
    for entry in index.entries() {
        if lang_of(&entry.rel).is_none() {
            continue;
        }
        let Ok(text) = read_source(root, &entry.rel) else {
            continue;
        };
        for symbol in extract(&entry.rel, &text) {
            if !kinds.is_empty() && !kinds.contains(&symbol.kind) {
                continue;
            }
            if let Some(score) = match_score(&symbol.name, &query) {
                hits.push((score, symbol));
            }
        }
    }

    // 全序：得分 → 名字短优先 → 字典序 → 路径 → 行号。落定顺序，前端列表才不会跳。
    hits.sort_by(|(a_score, a), (b_score, b)| {
        b_score
            .cmp(a_score)
            .then_with(|| a.name.len().cmp(&b.name.len()))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.range.start_line.cmp(&b.range.start_line))
    });
    hits.truncate(DEFAULT_MAX_SYMBOLS);

    SearchSymbolsResult {
        symbols: hits.into_iter().map(|(_, symbol)| symbol).collect(),
        // 索引在手时符号结果是完整的；降级标记由内核在「索引未就绪」时置真。
        indexing: false,
    }
}

fn read_source(root: &Path, rel: &str) -> std::io::Result<String> {
    let path = root.join(rel);
    let meta = std::fs::metadata(&path)?;
    if meta.len() > MAX_SYMBOL_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "文件超过符号抽取上限",
        ));
    }
    let bytes = std::fs::read(&path)?;
    // 二进制文件（含 NUL）不给抽取：源码文件不会是二进制。
    if bytes.iter().take(1024).any(|byte| *byte == 0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "文件疑似二进制",
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// 查询打分：精确 > 前缀 > 子串（均忽略大小写）> 子序列模糊。
///
/// 空查询返回 0 而不是 `None`——「列全部符号」是合法请求（面板首次打开时就要它）。
fn match_score(name: &str, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let lower = name.to_lowercase();
    if lower == query {
        return Some(4);
    }
    if lower.starts_with(query) {
        return Some(3);
    }
    if lower.contains(query) {
        return Some(2);
    }
    if crate::fuzzy::subsequence_score(name, query).is_some() {
        return Some(1);
    }
    None
}

/// 从一段源码抽符号。`rel` 只用来决定语言与回填 `path`。
///
/// TS / TSX / Python 走 [`ai_ide_parser`] 的 tree-sitter 语法树（判定点是节点种类，
/// 不会把字符串里的 `function` 当成声明）；其余语言仍走本模块的文本抽取。
/// 语法树抽取只对「支持的后缀」返回 `Some`，因此这里天然是「精确优先、文本兜底」。
pub fn extract(rel: &str, text: &str) -> Vec<Symbol> {
    if let Some(symbols) = ai_ide_parser::extract_symbols(rel, text) {
        return symbols;
    }
    match lang_of(rel) {
        Some(Lang::Python) => extract_python(rel, text),
        Some(lang) => extract_braced(lang, rel, text),
        None => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// 花括号语言：Rust / TS / JS / Go / Java
// ---------------------------------------------------------------------------

/// 作用域种类只分两档：类型体内 → 方法，其余 → 函数。
///
/// 不再细分 `impl` / `class` / `interface`：判定方法只需要知道「当前在不在类型体内」，
/// 多一个维度只会多一处漏判。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Type,
    Other,
}

struct Frame {
    /// 该作用域**闭合时**所在的深度（进入作用域前深度 + 该行净增）。
    body_depth: i32,
    scope: Scope,
    /// 关联的符号下标（类型声明才有；纯块为 `None`）。
    symbol: Option<usize>,
}

struct Decl {
    name: String,
    kind: SymbolKind,
    scope: Scope,
}

fn extract_braced(lang: Lang, rel: &str, text: &str) -> Vec<Symbol> {
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut frames: Vec<Frame> = Vec::new();
    let mut depth: i32 = 0;
    let mut in_block_comment = false;

    for (idx, raw) in text.lines().enumerate() {
        let line_no = (idx + 1) as u32;

        // 本行起始处的深度已经把更内层的作用域关掉了：弹栈并回填 end_line。
        while let Some(frame) = frames.last() {
            if depth >= frame.body_depth {
                break;
            }
            let frame = frames.pop().expect("上一步已确认非空");
            if let Some(sym) = frame.symbol {
                let start = symbols[sym].range.start_line;
                symbols[sym].range.end_line = line_no.saturating_sub(1).max(start);
            }
        }

        // 匹配用「抹平后」的行：注释里的 `fn foo` 与字符串里的 `class` 都不该命中。
        let code = code_only(raw, false, &mut in_block_comment);
        let delta = brace_delta(&code);

        if let Some(decl) = classify(lang, &code, &frames) {
            let sym_idx = symbols.len();
            symbols.push(Symbol {
                name: decl.name,
                kind: decl.kind,
                path: rel.to_string(),
                range: SymbolRange {
                    start_line: line_no,
                    end_line: line_no,
                },
            });
            if delta > 0 {
                frames.push(Frame {
                    body_depth: depth + delta,
                    scope: decl.scope,
                    symbol: Some(sym_idx),
                });
            }
        } else if delta > 0 {
            frames.push(Frame {
                body_depth: depth + delta,
                scope: scope_hint(lang, &code),
                symbol: None,
            });
        }

        depth += delta;
    }

    // 文件末尾没闭合的作用域（缺右括号 / 截断文件）收到最后一行，别留 `end_line = start_line`。
    let last_line = text.lines().count() as u32;
    for frame in frames {
        if let Some(sym) = frame.symbol {
            let start = symbols[sym].range.start_line;
            symbols[sym].range.end_line = last_line.max(start);
        }
    }

    symbols
}

/// 无名块的作用域种类（`impl` / `mod` / 控制流 / 函数体）。
fn scope_hint(lang: Lang, code: &str) -> Scope {
    match lang {
        // Rust 的方法只可能出现在 `impl` / `trait` 体内（`trait` 本身是具名声明，
        // 走 `classify`）。`mod` 里的 `fn` 仍是函数。
        Lang::Rust if patterns().rust_impl.is_match(code) => Scope::Type,
        _ => Scope::Other,
    }
}

fn classify(lang: Lang, line: &str, frames: &[Frame]) -> Option<Decl> {
    let p = patterns();
    let inside_type = frames.iter().any(|frame| frame.scope == Scope::Type);

    match lang {
        Lang::Rust => {
            // `const fn` / `async unsafe fn` 都归函数；必须先于 `const` 判定。
            if let Some(caps) = p.rust_fn.captures(line) {
                let kind = if inside_type {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                return Some(decl(&caps, 1, kind, Scope::Other));
            }
            if let Some(caps) = p.rust_struct.captures(line) {
                return Some(decl(&caps, 1, SymbolKind::Struct, Scope::Type));
            }
            if let Some(caps) = p.rust_enum.captures(line) {
                return Some(decl(&caps, 1, SymbolKind::Enum, Scope::Type));
            }
            if let Some(caps) = p.rust_trait.captures(line) {
                return Some(decl(&caps, 1, SymbolKind::Trait, Scope::Type));
            }
            if let Some(caps) = p.rust_const.captures(line) {
                return Some(decl(&caps, 1, SymbolKind::Const, Scope::Other));
            }
            None
        }
        Lang::TypeScript | Lang::JavaScript => {
            if let Some(caps) = p.ts_class.captures(line) {
                return Some(decl(&caps, 1, SymbolKind::Class, Scope::Type));
            }
            if let Some(caps) = p.ts_interface.captures(line) {
                return Some(decl(&caps, 1, SymbolKind::Interface, Scope::Type));
            }
            if let Some(caps) = p.ts_enum.captures(line) {
                return Some(decl(&caps, 1, SymbolKind::Enum, Scope::Type));
            }
            if let Some(caps) = p.ts_function.captures(line) {
                return Some(decl(&caps, 1, SymbolKind::Function, Scope::Other));
            }
            // `const Foo = () => …` / `const Foo = async function …`：React 组件的常见写法。
            if let Some(caps) = p.ts_arrow.captures(line) {
                return Some(decl(&caps, 1, SymbolKind::Function, Scope::Other));
            }
            if let Some(caps) = p.ts_method.captures(line) {
                let name = &caps[1];
                if inside_type && !is_control_keyword(name) {
                    return Some(decl(&caps, 1, SymbolKind::Method, Scope::Other));
                }
            }
            if let Some(caps) = p.ts_const.captures(line) {
                return Some(decl(&caps, 1, SymbolKind::Const, Scope::Other));
            }
            None
        }
        Lang::Java => {
            if let Some(caps) = p.java_class.captures(line) {
                return Some(decl(&caps, 1, SymbolKind::Class, Scope::Type));
            }
            if let Some(caps) = p.java_interface.captures(line) {
                return Some(decl(&caps, 1, SymbolKind::Interface, Scope::Type));
            }
            if let Some(caps) = p.java_enum.captures(line) {
                return Some(decl(&caps, 1, SymbolKind::Enum, Scope::Type));
            }
            if let Some(caps) = p.java_method.captures(strip_java_modifiers(line)) {
                let name = &caps[2];
                if inside_type && !is_control_keyword(name) {
                    return Some(decl(&caps, 2, SymbolKind::Method, Scope::Other));
                }
            }
            if let Some(caps) = p.java_const.captures(line) {
                return Some(decl(&caps, 1, SymbolKind::Const, Scope::Other));
            }
            None
        }
        Lang::Go => {
            if let Some(caps) = p.go_func.captures(line) {
                // 带接收者 `func (r *T) Name(` 才是方法。
                let kind = if caps.get(1).is_some() {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                return Some(decl(&caps, 2, kind, Scope::Other));
            }
            if let Some(caps) = p.go_struct.captures(line) {
                return Some(decl(&caps, 1, SymbolKind::Struct, Scope::Type));
            }
            if let Some(caps) = p.go_interface.captures(line) {
                return Some(decl(&caps, 1, SymbolKind::Interface, Scope::Type));
            }
            if let Some(caps) = p.go_const.captures(line) {
                return Some(decl(&caps, 1, SymbolKind::Const, Scope::Other));
            }
            None
        }
        Lang::Python => None,
    }
}

fn decl(caps: &regex::Captures<'_>, idx: usize, kind: SymbolKind, scope: Scope) -> Decl {
    Decl {
        name: caps
            .get(idx)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default(),
        kind,
        scope,
    }
}

/// 剥掉 Java 行首的修饰符（`public static final` 之类）。
///
/// 方法正则要分辨「返回类型 + 方法名」和「构造器 `Order(`」，而 `regex` crate 没有
/// 前瞻回溯可用：把修饰符写成可选分支时，`public Order(String id)` 会把 `public` 当作
/// 返回类型、把 `Order` 当作方法名。先统一剥掉修饰符，正则里就不留这条歧义。
fn strip_java_modifiers(line: &str) -> &str {
    const MODIFIERS: [&str; 10] = [
        "public",
        "private",
        "protected",
        "static",
        "final",
        "abstract",
        "synchronized",
        "native",
        "strictfp",
        "default",
    ];
    let mut rest = line.trim_start();
    'outer: loop {
        for modifier in MODIFIERS {
            if let Some(tail) = rest.strip_prefix(modifier) {
                if tail.starts_with(char::is_whitespace) {
                    rest = tail.trim_start();
                    continue 'outer;
                }
            }
        }
        return rest;
    }
}

/// 控制流关键字会被「方法」正则误命中（`if (x) {`、`for (…) {`），挡掉。
fn is_control_keyword(name: &str) -> bool {
    matches!(
        name,
        "if" | "for"
            | "while"
            | "switch"
            | "catch"
            | "return"
            | "new"
            | "else"
            | "do"
            | "try"
            | "finally"
            | "function"
            | "with"
            | "using"
            | "lock"
            | "foreach"
    )
}

// ---------------------------------------------------------------------------
// Python：缩进决定作用域
// ---------------------------------------------------------------------------

fn extract_python(rel: &str, text: &str) -> Vec<Symbol> {
    let p = patterns();
    let mut symbols: Vec<Symbol> = Vec::new();
    // (class 的缩进, 符号下标)
    let mut classes: Vec<(usize, usize)> = Vec::new();
    let mut in_block_comment = false;
    let mut last_code_line: u32 = 1;

    for (idx, raw) in text.lines().enumerate() {
        let line_no = (idx + 1) as u32;
        // `#` 注释 + 三引号 docstring 都要抹平：docstring 里常写着 `class Foo:` 的示例。
        let code = code_only(raw, true, &mut in_block_comment);
        if code.trim().is_empty() {
            continue;
        }
        let indent = raw.chars().take_while(|c| *c == ' ' || *c == '\t').count();

        // 缩进回到 class 同级或更浅 → class 结束。
        while let Some(&(class_indent, sym)) = classes.last() {
            if indent > class_indent {
                break;
            }
            classes.pop();
            let start = symbols[sym].range.start_line;
            symbols[sym].range.end_line = last_code_line.max(start);
        }

        if let Some(caps) = p.py_class.captures(&code) {
            let sym_idx = symbols.len();
            symbols.push(Symbol {
                name: name_of(&caps, 2),
                kind: SymbolKind::Class,
                path: rel.to_string(),
                range: SymbolRange {
                    start_line: line_no,
                    end_line: line_no,
                },
            });
            classes.push((indent, sym_idx));
        } else if let Some(caps) = p.py_def.captures(&code) {
            let kind = if classes.is_empty() {
                SymbolKind::Function
            } else {
                SymbolKind::Method
            };
            symbols.push(Symbol {
                name: name_of(&caps, 2),
                kind,
                path: rel.to_string(),
                range: SymbolRange {
                    start_line: line_no,
                    end_line: line_no,
                },
            });
        } else if indent == 0 {
            if let Some(caps) = p.py_const.captures(&code) {
                symbols.push(Symbol {
                    name: name_of(&caps, 1),
                    kind: SymbolKind::Const,
                    path: rel.to_string(),
                    range: SymbolRange {
                        start_line: line_no,
                        end_line: line_no,
                    },
                });
            }
        }

        last_code_line = line_no;
    }

    for (_, sym) in classes {
        let start = symbols[sym].range.start_line;
        symbols[sym].range.end_line = last_code_line.max(start);
    }

    symbols
}

fn name_of(caps: &regex::Captures<'_>, idx: usize) -> String {
    caps.get(idx)
        .map(|m| m.as_str().to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 行预处理
// ---------------------------------------------------------------------------

fn brace_delta(code: &str) -> i32 {
    let mut delta = 0i32;
    for ch in code.chars() {
        match ch {
            '{' => delta += 1,
            '}' => delta -= 1,
            _ => {}
        }
    }
    delta
}

/// 把注释与字符串字面量替换成空格（长度不变，便于对齐），只留「真代码」。
///
/// - `hash_comments`：Python 的 `#` 才是注释；Rust 的 `#[attr]`、TS 的 private field
///   （`#x`）都不能当注释砍掉。
/// - 块注释状态跨行，由调用方持有 `in_block_comment`。
/// - `'` 要区分生命周期与字符字面量：`&'a str` 是代码，`'{'` 是字符。
fn code_only(line: &str, hash_comments: bool, in_block_comment: &mut bool) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0usize;
    // Some(quote_char) 普通字面量；`"` 开头且后跟两个同字符时按三引号处理。
    let mut quote: Option<char> = None;
    let mut triple = false;

    while i < chars.len() {
        let ch = chars[i];

        if *in_block_comment {
            if ch == '*' && chars.get(i + 1) == Some(&'/') {
                *in_block_comment = false;
                out.push_str("  ");
                i += 2;
                continue;
            }
            out.push(' ');
            i += 1;
            continue;
        }

        if let Some(q) = quote {
            if triple {
                if ch == q && chars.get(i + 1) == Some(&q) && chars.get(i + 2) == Some(&q) {
                    quote = None;
                    triple = false;
                    out.push_str("   ");
                    i += 3;
                    continue;
                }
                out.push(' ');
                i += 1;
                continue;
            }
            if ch == '\\' {
                out.push(' ');
                if i + 1 < chars.len() {
                    out.push(' ');
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            if ch == q {
                quote = None;
                out.push(' ');
                i += 1;
                continue;
            }
            out.push(' ');
            i += 1;
            continue;
        }

        if (ch == '/' && chars.get(i + 1) == Some(&'/')) || (hash_comments && ch == '#') {
            break;
        }
        if ch == '/' && chars.get(i + 1) == Some(&'*') {
            *in_block_comment = true;
            out.push_str("  ");
            i += 2;
            continue;
        }
        if ch == '"' || ch == '`' {
            triple = ch == '"' && chars.get(i + 1) == Some(&'"') && chars.get(i + 2) == Some(&'"');
            quote = Some(ch);
            if triple {
                out.push_str("   ");
                i += 3;
            } else {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        if ch == '\'' {
            let next = chars.get(i + 1).copied();
            let after = chars.get(i + 2).copied();
            let lifetime =
                matches!(next, Some(c) if c.is_alphanumeric() || c == '_') && after != Some('\'');
            if lifetime {
                out.push(ch);
                i += 1;
                continue;
            }
            quote = Some('\'');
            out.push(' ');
            i += 1;
            continue;
        }

        out.push(ch);
        i += 1;
    }

    out
}

// ---------------------------------------------------------------------------
// 内置正则（一次编译，全进程复用）
// ---------------------------------------------------------------------------

struct Patterns {
    // Rust
    rust_fn: Regex,
    rust_struct: Regex,
    rust_enum: Regex,
    rust_trait: Regex,
    rust_const: Regex,
    rust_impl: Regex,
    // TypeScript / JavaScript
    ts_class: Regex,
    ts_interface: Regex,
    ts_enum: Regex,
    ts_function: Regex,
    ts_arrow: Regex,
    ts_method: Regex,
    ts_const: Regex,
    // Java
    java_class: Regex,
    java_interface: Regex,
    java_enum: Regex,
    java_method: Regex,
    java_const: Regex,
    // Go
    go_func: Regex,
    go_struct: Regex,
    go_interface: Regex,
    go_const: Regex,
    // Python
    py_class: Regex,
    py_def: Regex,
    py_const: Regex,
}

fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let re = |pattern: &str| Regex::new(pattern).expect("内置符号正则必须可编译");
        Patterns {
            rust_fn: re(concat!(
                r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:default\s+)?(?:const\s+)?",
                r"(?:async\s+)?(?:unsafe\s+)?(?:extern\s+(?:\x22[^\x22]*\x22\s+)?)?",
                r"fn\s+([A-Za-z_][A-Za-z0-9_]*)"
            )),
            rust_struct: re(r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)"),
            rust_enum: re(r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)"),
            rust_trait: re(concat!(
                r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:unsafe\s+)?(?:auto\s+)?",
                r"trait\s+([A-Za-z_][A-Za-z0-9_]*)"
            )),
            rust_const: re(concat!(
                r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:const|static)\s+(?:mut\s+)?",
                r"([A-Za-z_][A-Za-z0-9_]*)\s*:"
            )),
            rust_impl: re(r"^\s*(?:unsafe\s+)?impl\b"),

            ts_class: re(concat!(
                r"^\s*(?:export\s+)?(?:default\s+)?(?:abstract\s+)?(?:declare\s+)?",
                r"class\s+([A-Za-z_$][A-Za-z0-9_$]*)"
            )),
            ts_interface: re(concat!(
                r"^\s*(?:export\s+)?(?:default\s+)?(?:declare\s+)?",
                r"interface\s+([A-Za-z_$][A-Za-z0-9_$]*)"
            )),
            ts_enum: re(concat!(
                r"^\s*(?:export\s+)?(?:declare\s+)?(?:const\s+)?",
                r"enum\s+([A-Za-z_$][A-Za-z0-9_$]*)"
            )),
            ts_function: re(concat!(
                r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?",
                r"function\s*\*?\s*([A-Za-z_$][A-Za-z0-9_$]*)"
            )),
            ts_arrow: re(concat!(
                r"^\s*(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*",
                r"(?::[^=]+)?=\s*(?:async\s+)?(?:function\b|\(|[A-Za-z_$][A-Za-z0-9_$]*\s*=>)"
            )),
            ts_method: re(concat!(
                r"^\s*(?:(?:public|private|protected|static|async|readonly|override",
                r"|abstract|declare|get|set)\s+)*\*?\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*",
                r"(?:<[^>]*>)?\s*\([^;{]*\)\s*(?::\s*[^{;]+)?\{"
            )),
            ts_const: re(concat!(
                r"^\s*(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*",
                r"(?::[^=]+)?="
            )),

            java_class: re(concat!(
                r"^\s*(?:(?:public|private|protected|abstract|final|static|sealed",
                r"|strictfp|non-sealed)\s+)*class\s+([A-Za-z_$][A-Za-z0-9_$]*)"
            )),
            java_interface: re(concat!(
                r"^\s*(?:(?:public|private|protected|abstract|final|static|sealed",
                r"|strictfp|non-sealed)\s+)*interface\s+([A-Za-z_$][A-Za-z0-9_$]*)"
            )),
            java_enum: re(concat!(
                r"^\s*(?:(?:public|private|protected|static|final)\s+)*",
                r"enum\s+([A-Za-z_$][A-Za-z0-9_$]*)"
            )),
            // 先解析 `static final` 常量，再判方法：`static final Map<String, String> M = …` 没有括号。
            java_const: re(concat!(
                r"^\s*(?:(?:public|private|protected)\s+)?(?:static\s+final|final\s+static)\s+",
                r"[A-Za-z_$][A-Za-z0-9_$<>\[\],.?\s]*?\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*="
            )),
            java_method: re(concat!(
                r"^\s*([A-Za-z_$][A-Za-z0-9_$<>\[\],.?\s]*?)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*",
                r"\([^;{]*\)\s*(?:throws\s+[^{]+)?\{"
            )),

            go_func: re(concat!(
                r"^\s*func\s+(?:\(([^)]*)\)\s*)?([A-Za-z_][A-Za-z0-9_]*)\s*",
                r"(?:\[[^\]]*\])?\s*\("
            )),
            go_struct: re(r"^\s*type\s+([A-Za-z_][A-Za-z0-9_]*)\s+struct\b"),
            go_interface: re(r"^\s*type\s+([A-Za-z_][A-Za-z0-9_]*)\s+interface\b"),
            go_const: re(r"^\s*const\s+([A-Za-z_][A-Za-z0-9_]*)"),

            py_class: re(r"^(\s*)class\s+([A-Za-z_][A-Za-z0-9_]*)"),
            py_def: re(r"^(\s*)(?:async\s+)?def\s+([A-Za-z_][A-Za-z0-9_]*)"),
            py_const: re(r"^([A-Z][A-Z0-9_]*)\s*(?::[^=]+)?="),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn names(symbols: &[Symbol]) -> Vec<&str> {
        symbols.iter().map(|s| s.name.as_str()).collect()
    }

    fn find<'a>(symbols: &'a [Symbol], name: &str) -> &'a Symbol {
        symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("没抽到符号 {name}，实际：{:?}", names(symbols)))
    }

    fn temp_workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ai-ide-symbols-{}-{}-{}",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    #[test]
    fn rust_extracts_types_functions_and_impl_methods() {
        let src = r#"
use std::fmt;

pub const MAX_ITEMS: usize = 10;

pub struct Index {
    pub name: String,
}

pub enum Level {
    Low,
    High,
}

pub trait Store {
    fn get(&self, key: &str) -> Option<String>;
}

impl Store for Index {
    fn get(&self, key: &str) -> Option<String> {
        None
    }

    async fn put(&mut self, key: String) {
        let text = format!("{}{}", key, "}");
        self.name = text;
    }
}

pub fn helper() {}
"#;
        let symbols = extract("src/index.rs", src);
        assert_eq!(find(&symbols, "MAX_ITEMS").kind, SymbolKind::Const);
        assert_eq!(find(&symbols, "Index").kind, SymbolKind::Struct);
        assert_eq!(find(&symbols, "Level").kind, SymbolKind::Enum);
        assert_eq!(find(&symbols, "Store").kind, SymbolKind::Trait);
        assert_eq!(find(&symbols, "helper").kind, SymbolKind::Function);
        // `impl` 体内的 fn 是方法，trait 里的签名也是方法形态。
        let gets: Vec<&Symbol> = symbols.iter().filter(|s| s.name == "get").collect();
        assert_eq!(gets.len(), 2);
        assert!(gets.iter().all(|s| s.kind == SymbolKind::Method));
        assert_eq!(find(&symbols, "put").kind, SymbolKind::Method);
        // 字符串里的 `}` 不该把后续作用域算歪：`put` 的块要闭合在 `impl` 内部。
        let put = find(&symbols, "put");
        let impl_end = gets
            .iter()
            .map(|s| s.range.end_line)
            .max()
            .expect("有 impl 方法");
        assert!(put.range.end_line >= put.range.start_line);
        assert!(
            put.range.end_line <= impl_end + 6,
            "put 的结束行 {} 明显越界（impl 方法最晚 {}）",
            put.range.end_line,
            impl_end
        );
    }

    #[test]
    fn syntax_tree_beats_text_scanning_on_ts_and_python() {
        // 文本抽取最容易翻车的两种形态：字符串里的伪声明、装饰器包裹的函数。
        // 语法树按节点种类判定，这两类都不会误报 / 漏报。
        let ts = r#"
const help = "function fake() {}";
const tpl = `class Fake {}`;

export function real() {}

export const Button = () => null;
"#;
        let symbols = extract("src/ui.tsx", ts);
        assert!(symbols.iter().all(|s| s.name != "fake"));
        assert!(symbols.iter().all(|s| s.name != "Fake"));
        assert_eq!(find(&symbols, "real").kind, SymbolKind::Function);
        assert_eq!(find(&symbols, "Button").kind, SymbolKind::Function);

        let py = r#"
DOC = """
def fake():
    pass
"""


@decorator
def real():
    return 1


class Store:
    def get(self):
        return 1
"#;
        let symbols = extract("pkg/mod.py", py);
        assert!(symbols.iter().all(|s| s.name != "fake"));
        assert_eq!(find(&symbols, "real").kind, SymbolKind::Function);
        assert_eq!(find(&symbols, "Store").kind, SymbolKind::Class);
        assert_eq!(find(&symbols, "get").kind, SymbolKind::Method);
        // 装饰器行也是声明的一部分（跳转会落到 `@decorator`，不是 `def real`）。
        assert_eq!(find(&symbols, "real").range.start_line, 8);
    }

    #[test]
    fn typescript_covers_class_interface_enum_function_const_and_method() {
        let src = r#"
import { Store } from "./store";

export interface CacheLike {
  get(key: string): string;
}

export type Alias = string;

export enum Level {
  Low,
  High,
}

export class Cache implements CacheLike {
  private items = new Map<string, string>();

  constructor() {
    this.items = new Map();
  }

  get(key: string): string {
    if (this.items.has(key)) {
      return this.items.get(key)!;
    }
    return "";
  }
}

export function helper(a: number): number {
  return a;
}

export const load = async (path: string) => {
  return path;
};

const NAME = "cache";
"#;
        let symbols = extract("src/Cache.ts", src);
        assert_eq!(find(&symbols, "CacheLike").kind, SymbolKind::Interface);
        assert_eq!(find(&symbols, "Level").kind, SymbolKind::Enum);
        assert_eq!(find(&symbols, "Cache").kind, SymbolKind::Class);
        assert_eq!(find(&symbols, "helper").kind, SymbolKind::Function);
        assert_eq!(find(&symbols, "load").kind, SymbolKind::Function);
        assert_eq!(find(&symbols, "NAME").kind, SymbolKind::Const);
        assert_eq!(find(&symbols, "get").kind, SymbolKind::Method);
        // 控制流不该被当成方法
        assert!(!names(&symbols).contains(&"if"));
        // `private items = …` 是字段不是符号：不入清单（其类型标注不该被当方法）
        assert!(!names(&symbols).contains(&"items"));
    }

    #[test]
    fn tsx_and_js_reuse_the_typescript_rules() {
        let tsx = r#"
export const Panel = () => {
  return <div>hi</div>;
};

export function usePanel() {
  return 1;
}
"#;
        let symbols = extract("src/Panel.tsx", tsx);
        assert_eq!(find(&symbols, "Panel").kind, SymbolKind::Function);
        assert_eq!(find(&symbols, "usePanel").kind, SymbolKind::Function);

        let js = "function boot() {\n  return 1;\n}\nconst LIMIT = 3;\n";
        let symbols = extract("lib/boot.js", js);
        assert_eq!(find(&symbols, "boot").kind, SymbolKind::Function);
        assert_eq!(find(&symbols, "LIMIT").kind, SymbolKind::Const);
    }

    #[test]
    fn python_uses_indent_to_split_methods_from_functions() {
        let src = r#"
import os

MAX_SIZE = 10

class Store:
    """一个示例：class Fake: 不该被抽出来。"""

    def __init__(self, path):
        self.path = path

    async def put(self, key):
        await self.path

def helper(x):
    return x

LOOSE = 1
"#;
        let symbols = extract("app/store.py", src);
        assert_eq!(find(&symbols, "MAX_SIZE").kind, SymbolKind::Const);
        assert_eq!(find(&symbols, "LOOSE").kind, SymbolKind::Const);
        assert_eq!(find(&symbols, "Store").kind, SymbolKind::Class);
        assert_eq!(find(&symbols, "__init__").kind, SymbolKind::Method);
        assert_eq!(find(&symbols, "put").kind, SymbolKind::Method);
        assert_eq!(find(&symbols, "helper").kind, SymbolKind::Function);
        // docstring 里的 `class Fake:` 不能变成符号
        assert!(!names(&symbols).contains(&"Fake"));
        // class 的结束行应停在 def helper 之前
        let store = find(&symbols, "Store");
        assert!(store.range.end_line < find(&symbols, "helper").range.start_line);
    }

    #[test]
    fn go_uses_receiver_to_split_methods_from_functions() {
        let src = r#"
package demo

const MaxItems = 10

type Item struct {
    Name string
}

type Store interface {
    Get(key string) string
}

func NewStore() *Item {
    return nil
}

func (i *Item) Get(key string) string {
    return key
}
"#;
        let symbols = extract("internal/store.go", src);
        assert_eq!(find(&symbols, "MaxItems").kind, SymbolKind::Const);
        assert_eq!(find(&symbols, "Item").kind, SymbolKind::Struct);
        assert_eq!(find(&symbols, "Store").kind, SymbolKind::Interface);
        assert_eq!(find(&symbols, "NewStore").kind, SymbolKind::Function);
        assert_eq!(find(&symbols, "Get").kind, SymbolKind::Method);
        let item = find(&symbols, "Item");
        assert_eq!(item.range.start_line + 2, item.range.end_line);
    }

    #[test]
    fn java_extracts_class_members_and_skips_control_flow() {
        let src = r#"
package demo;

public class Order {
    private static final int MAX = 10;

    public Order(String id) {
        this.id = id;
    }

    public String getId() {
        return id;
    }

    private void apply(int qty) {
        if (qty > 0) {
            qty--;
        }
    }
}

interface Handler {
    void handle(String s);
}

enum Level {
    LOW,
    HIGH
}
"#;
        let symbols = extract("src/main/java/demo/Order.java", src);
        assert_eq!(find(&symbols, "Order").kind, SymbolKind::Class);
        assert_eq!(find(&symbols, "Handler").kind, SymbolKind::Interface);
        assert_eq!(find(&symbols, "Level").kind, SymbolKind::Enum);
        assert_eq!(find(&symbols, "MAX").kind, SymbolKind::Const);
        assert_eq!(find(&symbols, "getId").kind, SymbolKind::Method);
        assert_eq!(find(&symbols, "apply").kind, SymbolKind::Method);
        assert!(!names(&symbols).contains(&"if"));
        // 构造器 `public Order(String id)` 没有返回类型，不应当成方法
        assert!(symbols
            .iter()
            .filter(|s| s.name == "Order")
            .all(|s| s.kind == SymbolKind::Class));
    }

    #[test]
    fn block_end_line_spans_the_whole_declaration() {
        let src = "fn main() {\n    let x = 1;\n    let y = 2;\n}\n";
        let symbols = extract("src/main.rs", src);
        let main = find(&symbols, "main");
        assert_eq!(main.range.start_line, 1);
        assert_eq!(main.range.end_line, 4);
    }

    #[test]
    fn comments_do_not_produce_symbols() {
        let src = r#"
// fn fake_in_line_comment() {}
/*
fn fake_in_block_comment() {}
*/
/// 文档注释里的 `fn fake_in_doc()` 也不算
pub fn real() {}
"#;
        let symbols = extract("src/comments.rs", src);
        assert_eq!(names(&symbols), vec!["real"]);
    }

    #[test]
    fn unsupported_files_are_ignored() {
        assert!(extract("README.md", "# 标题\nfn main() {}\n").is_empty());
        assert!(extract("data.json", "{\"a\": 1}\n").is_empty());
    }

    #[test]
    fn kinds_filter_keeps_only_requested_kinds() {
        let root = temp_workspace("kinds");
        fs::write(
            root.join("lib.rs"),
            "pub struct Foo;\npub const FOO: u8 = 1;\npub fn foo() {}\n",
        )
        .unwrap();
        let index = FileIndex::build(&root).unwrap();

        let result = search(
            &root,
            &index,
            &SearchSymbolsParams {
                workspace_id: "w".into(),
                query: String::new(),
                kinds: Some(vec![SymbolKind::Struct, SymbolKind::Const]),
            },
        );
        let kinds: Vec<SymbolKind> = result.symbols.iter().map(|s| s.kind).collect();
        assert_eq!(kinds.len(), 2);
        assert!(kinds.contains(&SymbolKind::Struct));
        assert!(kinds.contains(&SymbolKind::Const));
        assert!(!kinds.contains(&SymbolKind::Function));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn no_match_returns_empty_list() {
        let root = temp_workspace("nomatch");
        fs::write(root.join("lib.rs"), "pub fn alpha() {}\n").unwrap();
        let index = FileIndex::build(&root).unwrap();

        let result = search(
            &root,
            &index,
            &SearchSymbolsParams {
                workspace_id: "w".into(),
                query: "zzzz-not-a-symbol".into(),
                kinds: None,
            },
        );
        assert!(result.symbols.is_empty());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn query_is_case_insensitive_and_ranks_prefix_first() {
        let root = temp_workspace("rank");
        fs::write(
            root.join("a.ts"),
            "export function renderAll() {}\nexport function preRender() {}\n",
        )
        .unwrap();
        let index = FileIndex::build(&root).unwrap();

        let result = search(
            &root,
            &index,
            &SearchSymbolsParams {
                workspace_id: "w".into(),
                query: "RENDER".into(),
                kinds: None,
            },
        );
        assert_eq!(
            result
                .symbols
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["renderAll", "preRender"],
            "前缀命中（renderAll）应排在子串命中（preRender）之前"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn empty_query_lists_everything_and_skips_ignored_files() {
        let root = temp_workspace("all");
        fs::create_dir_all(root.join("node_modules")).unwrap();
        fs::write(
            root.join("node_modules").join("dep.js"),
            "function fromDep() {}\n",
        )
        .unwrap();
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("notes.md"), "fn notCode() {}\n").unwrap();
        let index = FileIndex::build(&root).unwrap();

        let result = search(
            &root,
            &index,
            &SearchSymbolsParams {
                workspace_id: "w".into(),
                query: "  ".into(),
                kinds: None,
            },
        );
        let got = names(&result.symbols);
        assert_eq!(got, vec!["main"], "只应返回源码文件里的符号");

        fs::remove_dir_all(&root).ok();
    }
}
