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
//! 4. **超限不静默**：只要有引用没完整进来，注入正文末尾一定带说明行；预算小到
//!    连抬头与说明行都容不下时，内核把预算抬到保底值并在日志里记下这次抬升——
//!    静默丢内容比略超预算严重得多。
//!
//! `compress.strategy` 走两条**可观察行为不同**的路径（协议取值集合不变）：
//!
//! - `truncate`：放不下的引用按行保留**前缀**，留下的是被切断的内容；
//! - `summarize`：放不下的引用做**离线抽取式摘要**——保留段头（来源标注）与段尾
//!   （闭合标签），中间只留头尾若干行，并用一行标注交代省掉了多少行。摘要不调用
//!   模型、不读密钥、不联网，断网或没配 Provider 时照常可用（门禁因此不依赖外部
//!   服务）；*模型*摘要（`docs/04` §2.3 第 4 级）仍属后续切片。
//!
//! 也不引入分词器依赖：按 token 精确切分留给后续切片。

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

/// 剩余预算低于这个值时，不再保留半截内容，整段跳过。
const MIN_SEGMENT_TOKENS: u64 = 40;

/// 注入内容抬头：说明这批文本的来源与口径。
const PREAMBLE: &str =
    "以下是用户在本次对话中引用的工作区内容（路径相对工作区根，行号取自原文）：\n";

/// 省略说明里最多列几个路径。
const OMISSION_LIST_LIMIT: usize = 5;

/// 压缩策略的落地形态。
///
/// 协议层只有 `truncate` / `summarize` 两个取值（未知取值在反序列化阶段就被拒，
/// 见 `ai_ide_protocol::dto::CompressStrategy`），内核把它们映射成两条可观察行为
/// 不同的裁剪路径，而不是共用一个分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    /// 按行保留前缀：内容被切断，闭合标签也留不下。
    Truncate,
    /// 离线抽取式摘要：保留段头与段尾，中间以标注交代省略行数。
    Summarize,
}

impl Plan {
    fn of(strategy: CompressStrategy) -> Self {
        match strategy {
            CompressStrategy::Truncate => Plan::Truncate,
            CompressStrategy::Summarize => Plan::Summarize,
        }
    }
}

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
    /// 走了摘要裁剪（而非整段省略）的引用路径。只有 `strategy:"summarize"` 下非空。
    pub summarized: Vec<String>,
}

impl Injected {
    /// 空注入：没有任何引用可注入。
    fn empty() -> Self {
        Self {
            content: String::new(),
            tokens: 0,
            omitted: Vec::new(),
            summarized: Vec::new(),
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
    let plan = Plan::of(options.strategy);
    if plan == Plan::Summarize {
        // 摘要走本地抽取：不调模型、不读密钥、不联网，因此断网或没配 Provider 也不
        // 会影响这条路径（门禁因此不必依赖外部服务）。如实记录，方便对账。
        debug!("compress.strategy=summarize：按离线抽取式摘要处理（不调用模型）");
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

    Ok(assemble(segments, limit, plan))
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

/// 按预算顺序装配：放得下就整段放，放不下的段按 `plan` 裁剪，其后引用整体省略。
///
/// 两条硬约束：
/// 1. 正常情况下注入正文不超过预算；
/// 2. 「有引用没完整进来」这件事**必须**出现在正文里（末尾说明行）。预算小到连
///    抬头与说明行都容不下时，内核把预算抬到保底值并记日志——静默丢内容比略超
///    调用方给的数字严重得多。
fn assemble(segments: Vec<Segment>, limit: Option<u64>, plan: Plan) -> Injected {
    let costs: Vec<u64> = segments.iter().map(|s| estimate_text(&s.text)).collect();
    let preamble_cost = estimate_text(PREAMBLE);
    let demand = preamble_cost + costs.iter().sum::<u64>();

    let Some(budget) = limit else {
        // `compress.enabled:false`：调用方明确表示不要裁剪，全部注入。
        let mut content = String::from(PREAMBLE);
        for segment in &segments {
            content.push_str(&segment.text);
        }
        let tokens = estimate_text(&content);
        return Injected {
            content,
            tokens,
            omitted: Vec::new(),
            summarized: Vec::new(),
        };
    };

    // 说明行的位置先按「最坏情况（全部引用都要列进清单）」留出来，否则
    // 「说明自己被挤掉」又会退化成静默丢弃。
    let over_budget = demand > budget;
    let reserved = if over_budget {
        estimate_text(&omission_note(&paths_of(&segments)))
    } else {
        0
    };
    let floor = preamble_cost + reserved;
    let limit = if over_budget && budget < floor {
        warn!(
            requested = budget,
            floor, "budget_tokens 低于保底预算（抬头+省略说明），已按保底值处理"
        );
        floor
    } else {
        budget
    };
    let usable = limit.saturating_sub(reserved);

    let mut content = String::new();
    let mut used = 0u64;
    if preamble_cost <= usable {
        content.push_str(PREAMBLE);
        used = preamble_cost;
    }

    let mut omitted: Vec<String> = Vec::new();
    let mut summarized: Vec<String> = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        let cost = costs[index];
        if used.saturating_add(cost) <= usable {
            content.push_str(&segment.text);
            used += cost;
            continue;
        }
        let remaining = usable.saturating_sub(used);
        if remaining >= MIN_SEGMENT_TOKENS {
            let trimmed = match plan {
                Plan::Truncate => {
                    let (kept, kept_cost) = trim_to_budget(&segment.text, remaining);
                    (!kept.is_empty()).then_some((kept, kept_cost))
                }
                Plan::Summarize => summarize_to_budget(&segment.text, remaining),
            };
            match trimmed {
                Some((kept, kept_cost)) => {
                    content.push_str(&kept);
                    used += kept_cost;
                    if plan == Plan::Summarize {
                        summarized.push(segment.path.clone());
                    }
                }
                None => {
                    // 摘要骨架都放不下：整段省略，交给说明行交代，不塞残片。
                    debug!(
                        path = %segment.path,
                        remaining, "该段连摘要骨架都放不下，整段省略"
                    );
                }
            }
        }
        omitted.push(segment.path.clone());
        omitted.extend(segments[index + 1..].iter().map(|s| s.path.clone()));
        break;
    }

    if !omitted.is_empty() {
        let note = omission_note(&omitted);
        let note_cost = estimate_text(&note);
        if used + note_cost <= limit {
            content.push_str(&note);
            used += note_cost;
        } else {
            // 路径清单放不下就退成只报数量的短说明：说明行本身不能被挤掉。
            let short = omission_note_short(omitted.len());
            let short_cost = estimate_text(&short);
            debug!(
                omitted = omitted.len(),
                "省略说明放不下路径清单，已退成计数说明"
            );
            content.push_str(&short);
            used += short_cost;
        }
    }

    Injected {
        content,
        tokens: used,
        omitted,
        summarized,
    }
}

/// 段落路径清单（说明行按最坏情况预留位置时用）。
fn paths_of(segments: &[Segment]) -> Vec<String> {
    segments.iter().map(|s| s.path.clone()).collect()
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

/// 摘要式裁剪：保留段头（来源标注）与段尾（闭合标签），中间只留头尾若干行，
/// 并用一行标注交代省掉了多少行——结构完整，模型不会把半截内容当成全文。
///
/// 返回 `None` 表示连摘要骨架（段头 + 标注 + 段尾）都放不下：此时整段省略，
/// 由调用方记进 `omitted`，而不是塞一段没有闭合标签的残片。
///
/// **离线**：只用本地字符串处理，不调用模型、不读密钥、不联网。
fn summarize_to_budget(text: &str, budget: u64) -> Option<(String, u64)> {
    let lines: Vec<&str> = text.lines().collect();
    // 少于三行（段头 + 正文 + 段尾）就没有可摘要的中间层。
    if lines.len() < 3 {
        return None;
    }
    let head = lines[0];
    let tail = lines[lines.len() - 1];
    let body = &lines[1..lines.len() - 1];

    let skeleton = compose_summary(head, body, 0, 0, tail);
    let skeleton_cost = estimate_text(&skeleton);
    if skeleton_cost > budget {
        return None;
    }

    // 保留行越多成本越高（单调），所以二分找「放得下的最大保留行数」，
    // 避免在大文件上退化成逐行试错的 O(n²)。
    let (mut low, mut high) = (0usize, body.len());
    let mut best = (skeleton, skeleton_cost);
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        let (front, back) = split_keep(mid);
        let candidate = compose_summary(head, body, front, back, tail);
        let cost = estimate_text(&candidate);
        if cost <= budget {
            best = (candidate, cost);
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    Some(best)
}

/// 拼出摘要文本：段头 + 保留的前 `front` 行 + 省略标注 + 保留的后 `back` 行 + 段尾。
fn compose_summary(head: &str, body: &[&str], front: usize, back: usize, tail: &str) -> String {
    let omitted = body.len() - front - back;
    let mut out = String::new();
    out.push_str(head);
    out.push('\n');
    for line in &body[..front] {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&summary_marker(body.len(), omitted));
    for line in &body[body.len() - back..] {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(tail);
    out.push('\n');
    out
}

/// 摘要标注：写明「原文几行、省掉几行」，模型才知道这里不是全文。
fn summary_marker(total: usize, omitted: usize) -> String {
    if omitted == 0 {
        format!("<!-- 摘要：原文正文共 {total} 行，此处未省略 -->\n")
    } else {
        format!("<!-- 摘要：原文正文共 {total} 行，此处省略中间 {omitted} 行 -->\n")
    }
}

/// 把要保留的 `keep` 行交替分给前段与后段（前段多一行），让摘要同时留住开头与结尾。
fn split_keep(keep: usize) -> (usize, usize) {
    let front = keep.div_ceil(2);
    (front, keep - front)
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

/// 说明行的兜底形态：预算紧到列不下路径清单时，至少把「几个引用没进来」说清楚。
fn omission_note_short(count: usize) -> String {
    format!("<!-- 因 token 预算不足被裁剪或省略的引用：{count} 个（详见内核日志） -->\n")
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

    /// 摘要策略：段头与段尾都留住，中间只留头尾若干行，并用标注交代省了多少行。
    ///
    /// 顺带证明这条路径**不依赖任何外部服务**：测试进程没有配 Provider、也没有出网
    /// 许可，摘要照样成立——门禁因此不需要网络与密钥。
    #[test]
    fn summarize_keeps_head_and_tail_and_marks_omitted_lines() {
        let fixture = Fixture::new("summarize");
        let mut big = String::new();
        for line in 0..400 {
            big.push_str(&format!("export const value_{line} = {line};\n"));
        }
        fs::write(fixture.root.join("big.ts"), big).expect("写大文件");

        let budget = 300u64;
        let injected = fixture.render(
            vec![file_ref(ContextRefKind::File, "big.ts")],
            Some(ai_ide_protocol::dto::CompressOptions {
                enabled: true,
                strategy: CompressStrategy::Summarize,
                budget_tokens: Some(budget),
            }),
        );

        assert!(
            estimate_text(&injected.content) <= budget,
            "摘要后同样必须落在预算内：{} > {budget}",
            estimate_text(&injected.content)
        );
        assert!(
            injected
                .content
                .contains("<file path=\"big.ts\" lines=\"1-400\">"),
            "段头要留住：{}",
            injected.content
        );
        assert!(
            injected.content.contains("</file>"),
            "段尾要留住，模型才看得出这里被摘要过：{}",
            injected.content
        );
        assert!(
            injected.content.contains("<!-- 摘要：原文正文共 400 行"),
            "标注要写明原文多少行、省了多少行：{}",
            injected.content
        );
        assert!(
            injected.content.contains("1| export const value_0 = 0;"),
            "开头几行要留：{}",
            injected.content
        );
        assert!(
            injected
                .content
                .contains("400| export const value_399 = 399;"),
            "结尾几行也要留：{}",
            injected.content
        );
        assert!(
            !injected.content.contains("value_200"),
            "中间内容确实被摘要掉了"
        );
        assert_eq!(
            injected.summarized,
            vec!["big.ts".to_string()],
            "走了摘要的引用要单独记账"
        );
        assert!(
            injected.omitted.contains(&"big.ts".to_string()),
            "被摘要的引用同样没完整进来，必须进 omitted：用户与模型都要知道"
        );
    }

    /// 同一份文件、同一个预算，两条策略必须给出**可观察地不同**的结果。
    #[test]
    fn truncate_and_summarize_diverge_on_the_same_budget() {
        let fixture = Fixture::new("strategy-diverge");
        let mut big = String::new();
        for line in 0..400 {
            big.push_str(&format!("export const value_{line} = {line};\n"));
        }
        fs::write(fixture.root.join("big.ts"), big).expect("写大文件");

        let budget = 300u64;
        let render_with = |strategy| {
            fixture.render(
                vec![file_ref(ContextRefKind::File, "big.ts")],
                Some(ai_ide_protocol::dto::CompressOptions {
                    enabled: true,
                    strategy,
                    budget_tokens: Some(budget),
                }),
            )
        };
        let truncated = render_with(CompressStrategy::Truncate);
        let summarized = render_with(CompressStrategy::Summarize);

        assert!(estimate_text(&truncated.content) <= budget);
        assert!(estimate_text(&summarized.content) <= budget);

        assert!(
            !truncated.content.contains("</file>"),
            "截断只留前缀，闭合标签留不下：{}",
            truncated.content
        );
        assert!(
            !truncated.content.contains("<!-- 摘要"),
            "truncate 路径不该冒出摘要标注：{}",
            truncated.content
        );
        assert!(truncated.summarized.is_empty(), "truncate 不算摘要");

        assert!(
            summarized.content.contains("</file>"),
            "摘要要留段尾：{}",
            summarized.content
        );
        assert!(
            summarized.content.contains("<!-- 摘要："),
            "摘要路径要有标注：{}",
            summarized.content
        );
        assert_eq!(summarized.summarized, vec!["big.ts".to_string()]);
    }

    /// 没超预算时摘要不该无端改写内容。
    #[test]
    fn summarize_within_budget_leaves_content_untouched() {
        let fixture = Fixture::new("summarize-roomy");
        let injected = fixture.render(
            vec![file_ref(ContextRefKind::File, "a.ts")],
            Some(ai_ide_protocol::dto::CompressOptions {
                enabled: true,
                strategy: CompressStrategy::Summarize,
                budget_tokens: Some(10_000),
            }),
        );

        assert!(injected.omitted.is_empty());
        assert!(
            injected.summarized.is_empty(),
            "放得下就没有摘要这回事：{:?}",
            injected.summarized
        );
        assert!(!injected.content.contains("摘要："), "{}", injected.content);
        assert!(injected.content.contains("2| export const b = 2;"));
    }

    /// 摘要骨架（段头 + 标注 + 段尾）都放不下时整段省略，不塞残片。
    #[test]
    fn summarize_gives_up_the_whole_segment_when_skeleton_does_not_fit() {
        let mut body = String::new();
        for line in 0..50 {
            body.push_str(&format!("line-{line}\n"));
        }
        let text = format!("<file path=\"big.ts\" lines=\"1-50\">\n{body}</file>\n");

        let lines: Vec<&str> = text.lines().collect();
        let skeleton = compose_summary(
            lines[0],
            &lines[1..lines.len() - 1],
            0,
            0,
            lines[lines.len() - 1],
        );
        let skeleton_cost = estimate_text(&skeleton);

        let (kept, cost) =
            summarize_to_budget(&text, skeleton_cost).expect("刚好放得下骨架就该摘要");
        assert!(kept.contains("</file>"), "摘要必须留闭合标签：{kept}");
        assert!(kept.contains("<!-- 摘要：原文正文共 50 行"), "{kept}");
        assert!(cost <= skeleton_cost);

        assert!(
            summarize_to_budget(&text, skeleton_cost - 1).is_none(),
            "差一个 token 就该整段省略，而不是留下没有闭合标签的残片"
        );
        assert!(
            summarize_to_budget("只有一行\n", 1_000).is_none(),
            "单行文本没有可摘要的中间层"
        );
        assert!(
            summarize_to_budget("两行\n也没中间层\n", 1_000).is_none(),
            "两行同样没有中间层"
        );
    }

    /// 预算小到离谱时，抬头与省略说明仍要出现：**绝不静默丢内容**。
    #[test]
    fn tiny_budget_still_reports_omissions() {
        let fixture = Fixture::new("tiny-budget");
        fs::write(fixture.root.join("big.ts"), "x\n".repeat(50)).expect("写文件");

        let injected = fixture.render(
            vec![file_ref(ContextRefKind::File, "big.ts")],
            Some(ai_ide_protocol::dto::CompressOptions {
                enabled: true,
                strategy: CompressStrategy::Truncate,
                budget_tokens: Some(1),
            }),
        );

        assert!(
            injected.content.contains("因 token 预算不足"),
            "预算再小也要留下说明行，不能静默：{}",
            injected.content
        );
        assert_eq!(injected.omitted, vec!["big.ts".to_string()]);
        assert!(!injected.content.is_empty(), "抬头与说明行兜住了最坏情况");
    }
}
