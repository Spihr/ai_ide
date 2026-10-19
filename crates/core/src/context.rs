//! 上下文引用的解析与注入（M3）。
//!
//! `llm.chat_stream` 拿到 `context.refs` 后，由本模块把引用内容渲染成一段带来源
//! 标注与行号的文本，按 token 预算裁剪，再插成一条 `system` 消息。
//!
//! ## 三条约束（与 `docs/03` §2.4 对齐）
//!
//! 1. **越界即拒绝**：引用路径先过 `PathGuard`。越界（`../`、工作区外的绝对路径）
//!    直接让整个请求失败，不降级成「读不到」——路径闸门是安全边界，不参与容错。
//! 2. **读不到就标注**：文件被删、目录改名算「引用失效」，渲染成
//!    `<file ... error="not_found" />` 继续注入。用户应当看到自己引的东西没进来，
//!    而不是让整条对话失败。
//! 3. **预算是硬上限**：注入文本的估算 token 不超过 `budget_tokens`——超出的内容
//!    宁可丢掉，也不能挤掉用户真正的问题。口径与 `llm.count_tokens` 共用同一个
//!    估算器，`exact:false` 语义不变。
//!
//! M3 **不做**真实摘要：`strategy:"summarize"` 按 `truncate` 处理并记日志，不假装
//! 压过；也不引入分词器依赖，按 token 精确切分留给后续切片。

use std::path::Path;

use ai_ide_llm::tokens::estimate_text;
use ai_ide_protocol::dto::{ChatContext, CompressStrategy, ContextRef, ContextRefKind, EntryKind};
use ai_ide_workspace::{fs_ops, tree, PathGuard, Workspace, WorkspaceError};
use tracing::{debug, warn};

/// 单个文件注入的字节上限：超出即截断（防一个大日志把预算一次吃光）。
pub const SINGLE_FILE_MAX_BYTES: u64 = 256 * 1024;

/// 单个文件注入的行数上限。
pub const SINGLE_FILE_MAX_LINES: usize = 2_000;

/// `@文件夹` 的目录树深度。不做全量递归：目录树的用途是让模型知道结构，
/// 不是让它把整个仓库列一遍。
pub const FOLDER_DEPTH: u32 = 2;

/// 未指定 `budget_tokens` 时的默认注入预算。
pub const DEFAULT_BUDGET_TOKENS: u64 = 8_000;

/// 为「有内容被裁剪/省略」的说明行预留的预算。
const OMISSION_HEADROOM_TOKENS: u64 = 64;

/// 剩余预算低于这个值时，不再保留半截内容，整段跳过。
const MIN_SEGMENT_TOKENS: u64 = 40;

/// 注入内容抬头：说明这批文本的来源与口径。
const PREAMBLE: &str =
    "以下是用户在本次对话中引用的工作区内容（路径相对工作区根，行号取自原文）：\n";

/// 省略说明里最多列几个路径。
const OMISSION_LIST_LIMIT: usize = 5;

/// 一个渲染好的引用块。
struct Segment {
    /// 相对工作区根的路径（记日志与省略清单用）。
    path: String,
    /// 注入正文。
    text: String,
}

/// 一次注入的结果。
#[derive(Debug, Clone)]
pub struct Injected {
    /// 注入正文（已按预算裁剪）。为空表示不该插入消息。
    pub content: String,
    /// 正文的估算 token 数（与 `llm.count_tokens` 同口径）。
    pub tokens: u64,
    /// 被裁剪或省略的引用路径，按出现顺序。
    pub omitted: Vec<String>,
}

impl Injected {
    /// 空注入：没有任何引用可注入。
    fn empty() -> Self {
        Self {
            content: String::new(),
            tokens: 0,
            omitted: Vec::new(),
        }
    }

    /// 正文是否为空（空则调用方不应插入消息）。
    pub fn is_empty(&self) -> bool {
        self.content.trim().is_empty()
    }
}

/// 把一批引用渲染成注入正文。
///
/// 返回 `Err` 只有一种来源：某条引用的路径越界或非法（安全闸门）。
/// 文件读不到不算错误，会以标注形式出现在正文里。
pub fn render(workspace: &Workspace, context: &ChatContext) -> Result<Injected, WorkspaceError> {
    if context.refs.is_empty() {
        return Ok(Injected::empty());
    }

    let options = context.compress.clone().unwrap_or_default();
    if matches!(options.strategy, CompressStrategy::Summarize) {
        debug!("compress.strategy=summarize 在 M3 未实现，本次按 truncate 处理");
    }
    let limit = if options.enabled {
        Some(options.budget_tokens.unwrap_or(DEFAULT_BUDGET_TOKENS))
    } else {
        None
    };

    let mut segments = Vec::with_capacity(context.refs.len());
    for reference in &context.refs {
        segments.push(render_ref(&workspace.guard, reference)?);
    }

    Ok(assemble(segments, limit))
}

/// 渲染单条引用。越界路径在此冒泡。
fn render_ref(guard: &PathGuard, reference: &ContextRef) -> Result<Segment, WorkspaceError> {
    // `@整个工作区` 用空路径表达：这是 `@文件夹` 的自然写法，而 `PathGuard::resolve("")`
    // 会以「路径为空」拒绝。这里把它解释成工作区根，而不是让用户去猜该填 `.` 还是 `.\`。
    // 文件类引用不吃这个例外——指向文件的引用缺路径就是脏数据，该报错。
    let abs = match reference.kind {
        ContextRefKind::Folder if reference.path.trim().is_empty() => guard.root().to_path_buf(),
        _ => guard.resolve(&reference.path)?,
    };
    let relative = display_path(guard, &abs);
    match reference.kind {
        ContextRefKind::Folder => render_folder(guard, &abs, &relative),
        _ => render_file(guard, reference, &abs, &relative),
    }
}

/// 文件 / 选区 / 当前文件：同一套渲染，只有标签与取行方式不同。
fn render_file(
    guard: &PathGuard,
    reference: &ContextRef,
    abs: &Path,
    relative: &str,
) -> Result<Segment, WorkspaceError> {
    let tag = tag_of(reference.kind);

    // 选区原文优先：编辑器里的缓冲区可能还没落盘，读盘反而会读到上一版内容。
    if matches!(reference.kind, ContextRefKind::Selection)
        && reference.start_line.is_none()
        && reference.end_line.is_none()
    {
        if let Some(text) = &reference.text {
            let mut out = format!("<{tag} path=\"{relative}\">\n");
            out.push_str(text);
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&format!("</{tag}>\n"));
            return Ok(Segment {
                path: relative.to_string(),
                text: out,
            });
        }
    }

    if !abs.is_file() {
        // 区分「没有这个文件」与「这是一个目录」：用户引错了目标时，
        // 「not_found」会把人送去检查拼写，而真正的问题是引用了目录。
        let reason = if abs.exists() {
            "not_file"
        } else {
            "not_found"
        };
        return Ok(Segment {
            path: relative.to_string(),
            text: format!("<{tag} path=\"{relative}\" error=\"{reason}\" />\n"),
        });
    }

    let file = fs_ops::read_file(guard, &reference.path, None, Some(SINGLE_FILE_MAX_BYTES))?;
    let lines: Vec<&str> = file.content.lines().collect();
    let (start, end) = select_range(reference, lines.len());

    let mut truncated = if file.truncated { Some("bytes") } else { None };
    if end >= start && end - start + 1 > SINGLE_FILE_MAX_LINES {
        truncated = Some("lines");
    }
    let last = if truncated == Some("lines") {
        start + SINGLE_FILE_MAX_LINES - 1
    } else {
        end
    };

    let mut out = format!("<{tag} path=\"{relative}\" lines=\"{start}-{last}\"");
    if let Some(kind) = truncated {
        out.push_str(&format!(" truncated=\"{kind}\""));
    }
    out.push_str(">\n");
    if end >= start {
        for (offset, line) in lines[start - 1..=last - 1].iter().enumerate() {
            out.push_str(&format!("{}| {}\n", start + offset, line));
        }
    }
    out.push_str(&format!("</{tag}>\n"));

    Ok(Segment {
        path: relative.to_string(),
        text: out,
    })
}

/// 目录树：浅层列出子项，目录带 `/` 后缀，缩进表达层级。
fn render_folder(guard: &PathGuard, abs: &Path, relative: &str) -> Result<Segment, WorkspaceError> {
    if !abs.is_dir() {
        return Ok(Segment {
            path: relative.to_string(),
            text: format!("<folder path=\"{relative}\" error=\"not_found\" />\n"),
        });
    }

    let entries = tree::list(guard, Some(relative), FOLDER_DEPTH, false)?;
    let base_depth = if relative.is_empty() {
        0
    } else {
        relative.split('/').count()
    };

    let mut out = format!("<folder path=\"{relative}\" depth=\"{FOLDER_DEPTH}\">\n");
    if entries.is_empty() {
        out.push_str("（空目录）\n");
    }
    for entry in &entries {
        let child = guard.to_relative(Path::new(&entry.path)).replace('\\', "/");
        let level = child.split('/').count().saturating_sub(base_depth);
        let indent = "  ".repeat(level.saturating_sub(1));
        let name = child.rsplit('/').next().unwrap_or(child.as_str());
        let slash = if matches!(entry.kind, EntryKind::Dir) {
            "/"
        } else {
            ""
        };
        out.push_str(&format!("{indent}{name}{slash}\n"));
    }
    out.push_str("</folder>\n");

    Ok(Segment {
        path: relative.to_string(),
        text: out,
    })
}

/// 按预算顺序装配：放得下就整段放，放不下先按行留一部分，其后整体省略。
fn assemble(segments: Vec<Segment>, limit: Option<u64>) -> Injected {
    let costs: Vec<u64> = segments.iter().map(|s| estimate_text(&s.text)).collect();
    let preamble_cost = estimate_text(PREAMBLE);
    let demand = preamble_cost + costs.iter().sum::<u64>();

    // 有内容装不下时先扣出说明行的位置，否则「说明自己被截掉」会变成静默丢弃。
    let headroom = match limit {
        Some(limit) if demand > limit => OMISSION_HEADROOM_TOKENS.min(limit),
        _ => 0,
    };
    let usable = match limit {
        Some(limit) => limit.saturating_sub(headroom),
        None => u64::MAX,
    };

    let mut content = String::new();
    let mut used = 0u64;
    if preamble_cost <= usable {
        content.push_str(PREAMBLE);
        used = preamble_cost;
    }

    let mut omitted: Vec<String> = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        let cost = costs[index];
        if used.saturating_add(cost) <= usable {
            content.push_str(&segment.text);
            used += cost;
            continue;
        }
        let remaining = usable.saturating_sub(used);
        if remaining >= MIN_SEGMENT_TOKENS {
            let (kept, kept_cost) = trim_to_budget(&segment.text, remaining);
            content.push_str(&kept);
            used += kept_cost;
        }
        omitted.push(segment.path.clone());
        omitted.extend(segments[index + 1..].iter().map(|s| s.path.clone()));
        break;
    }

    if !omitted.is_empty() {
        let note = omission_note(&omitted);
        let note_cost = estimate_text(&note);
        match limit {
            Some(limit) if used + note_cost > limit => {
                warn!(
                    omitted = omitted.len(),
                    "预算过紧，省略说明也放不下；已静默丢弃超出部分"
                );
            }
            _ => {
                content.push_str(&note);
                used += note_cost;
            }
        }
    }

    Injected {
        content,
        tokens: used,
        omitted,
    }
}

/// 逐行累加直到预算用尽。返回保留的文本与实际占用。
///
/// 逐行估算的系统性偏高（每行零头都会进位），因此这里**不会**超预算；
/// 代价是预算的利用率略低于理论值——这是刻意选的保守方向。
fn trim_to_budget(text: &str, budget: u64) -> (String, u64) {
    let mut kept = String::new();
    let mut used = 0u64;
    for line in text.lines() {
        let cost = estimate_text(line);
        if used + cost > budget {
            break;
        }
        kept.push_str(line);
        kept.push('\n');
        used += cost;
    }
    (kept, used)
}

/// 省略说明：让模型与用户都知道「有些引用没进来」。
fn omission_note(paths: &[String]) -> String {
    let listed: Vec<String> = paths.iter().take(OMISSION_LIST_LIMIT).cloned().collect();
    let mut note = format!(
        "<!-- 因 token 预算不足被裁剪或省略的引用：{}",
        listed.join(", ")
    );
    if paths.len() > listed.len() {
        note.push_str(&format!(" 等 {} 个", paths.len()));
    }
    note.push_str(" -->\n");
    note
}

/// 按 1-based 闭区间取行；越界自动收敛，`start > end` 时交换。
fn select_range(reference: &ContextRef, total: usize) -> (usize, usize) {
    if total == 0 {
        return (1, 0);
    }
    let clamp = |value: u32| (value.max(1) as usize).min(total);
    match (reference.start_line, reference.end_line) {
        (Some(start), Some(end)) => {
            let (start, end) = (clamp(start), clamp(end));
            if start <= end {
                (start, end)
            } else {
                (end, start)
            }
        }
        (Some(start), None) => (clamp(start), total),
        (None, Some(end)) => (1, clamp(end)),
        (None, None) => (1, total),
    }
}

/// 注入标签名（与 `ContextRefKind` 一一对应）。
fn tag_of(kind: ContextRefKind) -> &'static str {
    match kind {
        ContextRefKind::File => "file",
        ContextRefKind::Folder => "folder",
        ContextRefKind::Selection => "selection",
        ContextRefKind::CurrentFile => "current_file",
    }
}

/// 统一成 `/` 分隔的相对路径；根目录为空串。
fn display_path(guard: &PathGuard, abs: &Path) -> String {
    let relative = guard.to_relative(abs).replace('\\', "/");
    if relative == "." {
        String::new()
    } else {
        relative
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_ide_workspace::WorkspaceManager;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// 造一个真实工作区（引用渲染要真的读盘、真的列目录）。
    struct Fixture {
        root: PathBuf,
        workspace: Arc<Workspace>,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("ai-ide-context-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("src")).expect("建目录");
            fs::write(
                root.join("a.ts"),
                "export const a = 1;\nexport const b = 2;\n",
            )
            .expect("写文件");
            fs::write(root.join("src/inner.ts"), "inner\n").expect("写文件");
            let manager = WorkspaceManager::new();
            let workspace = manager
                .open(root.to_str().expect("临时目录应为 UTF-8"))
                .expect("打开工作区");
            Self { root, workspace }
        }

        fn render(
            &self,
            refs: Vec<ContextRef>,
            compress: Option<ai_ide_protocol::dto::CompressOptions>,
        ) -> Injected {
            let context = ChatContext {
                workspace_id: None,
                refs,
                compress,
            };
            render(&self.workspace, &context).expect("渲染应当成功")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn file_ref(kind: ContextRefKind, path: &str) -> ContextRef {
        ContextRef {
            kind,
            path: path.to_string(),
            start_line: None,
            end_line: None,
            text: None,
            label: None,
        }
    }

    #[test]
    fn full_file_is_numbered_and_labelled() {
        let fixture = Fixture::new("full-file");
        let injected = fixture.render(vec![file_ref(ContextRefKind::File, "a.ts")], None);
        assert!(
            injected
                .content
                .contains("<file path=\"a.ts\" lines=\"1-2\">"),
            "来源与行区间必须标注：{}",
            injected.content
        );
        assert!(injected.content.contains("1| export const a = 1;"));
        assert!(injected.content.contains("2| export const b = 2;"));
        assert!(injected.content.ends_with("</file>\n"), "闭合标签不能少");
    }

    #[test]
    fn selection_keeps_requested_line_range() {
        let fixture = Fixture::new("selection-range");
        let mut reference = file_ref(ContextRefKind::Selection, "a.ts");
        reference.start_line = Some(2);
        reference.end_line = Some(2);
        let injected = fixture.render(vec![reference], None);

        assert!(injected
            .content
            .contains("<selection path=\"a.ts\" lines=\"2-2\">"));
        assert!(injected.content.contains("2| export const b = 2;"));
        assert!(
            !injected.content.contains("1| export const a = 1;"),
            "区间外的行不该被注入"
        );
    }

    #[test]
    fn selection_text_wins_when_no_lines_are_given() {
        let fixture = Fixture::new("selection-text");
        let mut reference = file_ref(ContextRefKind::Selection, "a.ts");
        reference.text = Some("const unsaved = true;".to_string());
        let injected = fixture.render(vec![reference], None);

        assert!(
            injected.content.contains("const unsaved = true;"),
            "未保存的缓冲区内容必须原样注入：{}",
            injected.content
        );
        assert!(!injected.content.contains("export const a"));
    }

    #[test]
    fn folder_lists_shallow_tree_with_indent() {
        let fixture = Fixture::new("folder");
        let injected = fixture.render(vec![file_ref(ContextRefKind::Folder, "")], None);

        assert!(injected.content.contains("<folder path=\"\" depth=\"2\">"));
        assert!(injected.content.contains("a.ts\n"), "{}", injected.content);
        assert!(injected.content.contains("src/\n"), "{}", injected.content);
        assert!(
            injected.content.contains("  inner.ts\n"),
            "子目录条目要缩进一层：{}",
            injected.content
        );
    }

    #[test]
    fn missing_file_is_marked_not_failed() {
        let fixture = Fixture::new("missing");
        let injected = fixture.render(vec![file_ref(ContextRefKind::File, "nope.ts")], None);
        assert!(injected
            .content
            .contains("<file path=\"nope.ts\" error=\"not_found\" />"));
    }

    #[test]
    fn file_ref_targeting_a_directory_is_marked_not_file() {
        let fixture = Fixture::new("dir-as-file");
        let injected = fixture.render(vec![file_ref(ContextRefKind::File, "src")], None);
        assert!(
            injected
                .content
                .contains("<file path=\"src\" error=\"not_file\" />"),
            "指向目录的引用要如实标注，而不是含糊的 not_found：{}",
            injected.content
        );
    }

    #[test]
    fn current_file_uses_its_own_tag() {
        let fixture = Fixture::new("current-file");
        let injected = fixture.render(vec![file_ref(ContextRefKind::CurrentFile, "a.ts")], None);
        assert!(
            injected.content.contains("<current_file path=\"a.ts\""),
            "四类引用各有标签，UI 与模型都靠它区分来源：{}",
            injected.content
        );
    }

    #[test]
    fn out_of_workspace_path_is_rejected() {
        let fixture = Fixture::new("escape");
        let context = ChatContext {
            workspace_id: None,
            refs: vec![file_ref(ContextRefKind::File, "../secret.txt")],
            compress: None,
        };
        let err = render(&fixture.workspace, &context).expect_err("越界必须失败");
        assert_eq!(
            err.code(),
            ai_ide_protocol::ErrorCode::PATH_OUT_OF_WORKSPACE,
            "越界要走路径错误码，而不是「读不到」"
        );
    }

    #[test]
    fn injection_order_follows_refs_order() {
        let fixture = Fixture::new("order");
        let injected = fixture.render(
            vec![
                file_ref(ContextRefKind::File, "src/inner.ts"),
                file_ref(ContextRefKind::File, "a.ts"),
            ],
            None,
        );
        let inner = injected
            .content
            .find("path=\"src/inner.ts\"")
            .expect("第一个引用");
        let outer = injected.content.find("path=\"a.ts\"").expect("第二个引用");
        assert!(inner < outer, "注入顺序必须等于 refs 顺序");
    }

    #[test]
    fn one_file_over_the_line_cap_is_truncated() {
        let fixture = Fixture::new("single-cap");
        let mut big = String::new();
        for line in 0..(SINGLE_FILE_MAX_LINES + 50) {
            big.push_str(&format!("line-{line}\n"));
        }
        fs::write(fixture.root.join("big.txt"), big).expect("写大文件");

        let injected = fixture.render(vec![file_ref(ContextRefKind::File, "big.txt")], None);
        assert!(
            injected.content.contains("truncated=\"lines\""),
            "超行数上限必须标注：{}",
            &injected.content[..injected.content.len().min(200)]
        );
        assert!(
            !injected
                .content
                .contains(&format!("line-{}", SINGLE_FILE_MAX_LINES + 10)),
            "上限之后的内容不该被注入"
        );
    }

    #[test]
    fn budget_caps_injected_tokens_and_reports_omissions() {
        let fixture = Fixture::new("budget");
        let mut big = String::new();
        for line in 0..400 {
            big.push_str(&format!("export const value_{line} = {line};\n"));
        }
        fs::write(fixture.root.join("big.ts"), big).expect("写大文件");

        let budget = 300u64;
        let injected = fixture.render(
            vec![
                file_ref(ContextRefKind::File, "big.ts"),
                file_ref(ContextRefKind::File, "a.ts"),
            ],
            Some(ai_ide_protocol::dto::CompressOptions {
                enabled: true,
                strategy: CompressStrategy::Truncate,
                budget_tokens: Some(budget),
            }),
        );

        assert!(
            estimate_text(&injected.content) <= budget,
            "注入内容必须落在预算内：{} > {budget}",
            estimate_text(&injected.content)
        );
        assert!(
            injected.content.contains("因 token 预算不足"),
            "被丢掉的引用要有说明：{}",
            injected.content
        );
        assert_eq!(
            injected.omitted,
            vec!["big.ts".to_string(), "a.ts".to_string()],
            "被裁剪的与整体省略的都要记进 omitted：用户需要知道哪些引用没完整进来"
        );
        assert!(
            !injected.content.contains("<file path=\"a.ts\""),
            "省略的引用不该留下残缺块"
        );
    }

    #[test]
    fn compress_disabled_injects_everything() {
        let fixture = Fixture::new("compress-off");
        let injected = fixture.render(
            vec![file_ref(ContextRefKind::File, "a.ts")],
            Some(ai_ide_protocol::dto::CompressOptions {
                enabled: false,
                strategy: CompressStrategy::Summarize,
                budget_tokens: Some(1),
            }),
        );
        assert!(injected.omitted.is_empty());
        assert!(injected.content.contains("2| export const b = 2;"));
    }

    #[test]
    fn empty_refs_produce_empty_injection() {
        let fixture = Fixture::new("empty");
        let injected = fixture.render(Vec::new(), None);
        assert!(injected.is_empty(), "没有引用就不该插入消息");
        assert_eq!(injected.tokens, 0);
    }
}
