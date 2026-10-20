//! unified diff 引擎：生成、解析、按 hunk 逐块接受 / 拒绝后应用。
//!
//! ## 格式约定
//!
//! 与 `git diff` / GNU diff 的 unified 形态对齐，便于肉眼比对：
//!
//! ```text
//! --- a/src/app.ts
//! +++ b/src/app.ts
//! @@ -1,4 +1,5 @@
//!  const a = 1;
//! -const b = 2;
//! +const b = 3;
//!  export { a, b };
//! ```
//!
//! - 文件头两行固定 `--- a/<path>` / `+++ b/<path>`；
//! - hunk 头 `@@ -<旧起点>,<旧行数> +<新起点>,<新行数> @@`，行号 **1 起**；
//!   行数为 0 时起点取「插入 / 删除点前一行」的编号，与 GNU 约定一致
//!   （在空文件里插入即 `-0,0`）；
//! - 上下文行 [`DEFAULT_CONTEXT`] 行，行前缀 ` `/`-`/`+`；
//! - 末行没有换行收尾时，在该行后补一行「反斜杠 + No newline at end of file」。
//!   少了这条，「只补一个行尾换行」这类改动会被 diff 吃掉，应用后无法逐字节还原；
//!   本实现对它的处理比 git 宽松一点——上下文行同样带标记，信息不丢。
//!
//! ## 确定性
//!
//! 同一对输入永远产出同一个字符串：差分走 Myers 贪心、先裁公共前后缀、
//! hunk 顺序即出现顺序，全程不做字典序或时间相关的排序。
//!
//! ## 冲突口径
//!
//! 「patch 与目标内容对不上」走 [`DiffError::Conflict`]，由调用方映射成协议既有的
//! `3003 DIFF_APPLY_CONFLICT`（见 `docs/03-内核接口协议规范.md` §1.3）。

use std::fmt::Write as _;

/// 默认上下文行数（与 `git diff -U3` 一致）。
pub const DEFAULT_CONTEXT: usize = 3;

/// 中间段合计行数超过该阈值时，放弃逐行差分，退化为「整段替换」。
///
/// Myers 的 `trace` 内存是 `O(D × (N + M))`：两万行的完全异构文件能吃掉几百 MB。
/// 退化路径仍然产出合法 hunk（一块删光再写满），只是不够精细。
const MAX_MYERS_CELLS: usize = 2_000;

/// 一行在 diff 里的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    /// 两侧相同（上下文）。
    Context,
    /// 只在旧内容里。
    Removed,
    /// 只在新内容里。
    Added,
}

/// diff 里的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    /// 角色。
    pub kind: DiffLineKind,
    /// 行文本，不含前缀字符与换行。
    pub text: String,
    /// 该行所属的一侧在此行之后没有换行（渲染成 no-newline 标记）。
    pub no_newline: bool,
}

/// 一个 hunk。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// 旧内容里的起点（1 起；`old_lines == 0` 时为插入点前一行号）。
    pub old_start: usize,
    /// 旧内容里被覆盖的行数（上下文 + 删除）。
    pub old_lines: usize,
    /// 新内容里的起点（1 起；`new_lines == 0` 时为删除点前一行号）。
    pub new_start: usize,
    /// 新内容里被覆盖的行数（上下文 + 新增）。
    pub new_lines: usize,
    /// hunk 正文，按顺序。
    pub lines: Vec<DiffLine>,
}

impl Hunk {
    /// 上下文 + 删除的行数（应当等于 [`Hunk::old_lines`]）。
    fn old_span(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| matches!(line.kind, DiffLineKind::Context | DiffLineKind::Removed))
            .count()
    }

    /// 上下文 + 新增的行数（应当等于 [`Hunk::new_lines`]）。
    fn new_span(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| matches!(line.kind, DiffLineKind::Context | DiffLineKind::Added))
            .count()
    }

    /// hunk 头声明的行数与正文是否自洽。
    fn check_shape(&self) -> Result<(), DiffError> {
        let (old_span, new_span) = (self.old_span(), self.new_span());
        if old_span == self.old_lines && new_span == self.new_lines {
            return Ok(());
        }
        let (old_start, old_lines, new_start, new_lines) = (
            self.old_start,
            self.old_lines,
            self.new_start,
            self.new_lines,
        );
        Err(DiffError::Parse(format!(
            "hunk 头 @@ -{old_start},{old_lines} +{new_start},{new_lines} @@ 与正文不符：正文实际旧 {old_span} 行、新 {new_span} 行"
        )))
    }
}

/// 解析出来的整份 diff。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedDiff {
    /// 从 `+++ b/<path>` 提取的路径（可缺省）。
    pub path: Option<String>,
    /// 全部 hunk，按 patch 里的顺序。
    pub hunks: Vec<Hunk>,
}

/// diff 失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiffError {
    /// patch 文本本身不合法（回灌模型，让它重发一版）。
    #[error("{0}")]
    Parse(String),
    /// patch 与目标内容对不上（复用协议 `3003`）。
    #[error("{0}")]
    Conflict(String),
}

// ---------------------------------------------------------------- 生成

/// 生成一份 unified diff（`context` 为上下文行数，一般传 [`DEFAULT_CONTEXT`]）。
///
/// `path` 是相对工作区根、以 `/` 分隔的展示路径；两侧内容无差异时返回空串。
pub fn unified_diff(path: &str, original: &str, updated: &str, context: usize) -> String {
    render_unified_diff(path, &diff_hunks(original, updated, context))
}

/// 只算 hunk，不渲染成文本。
pub fn diff_hunks(original: &str, updated: &str, context: usize) -> Vec<Hunk> {
    let old = Text::new(original);
    let new = Text::new(updated);
    let ops = diff_ops(&old.keys(), &new.keys());
    build_hunks(&old, &new, &ops, context)
}

/// 把 hunk 渲染成 unified diff 文本。
pub fn render_unified_diff(path: &str, hunks: &[Hunk]) -> String {
    let mut out = String::new();
    if hunks.is_empty() {
        return out;
    }
    let path = clean_path(path).unwrap_or_else(|| path.to_string());
    let _ = writeln!(out, "--- a/{path}");
    let _ = writeln!(out, "+++ b/{path}");
    for hunk in hunks {
        let (old_start, old_lines, new_start, new_lines) = (
            hunk.old_start,
            hunk.old_lines,
            hunk.new_start,
            hunk.new_lines,
        );
        let _ = writeln!(
            out,
            "@@ -{old_start},{old_lines} +{new_start},{new_lines} @@"
        );
        for line in &hunk.lines {
            out.push(match line.kind {
                DiffLineKind::Context => ' ',
                DiffLineKind::Removed => '-',
                DiffLineKind::Added => '+',
            });
            out.push_str(&line.text);
            out.push('\n');
            if line.no_newline {
                out.push_str("\\ No newline at end of file\n");
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 解析

/// 解析 unified diff 文本。
///
/// 宽容处理文件头（`diff --git` / `index` / `---` / `+++` 都跳过）与 CRLF patch；
/// hunk 头与正文行数不自洽、出现无法识别的行前缀时报 [`DiffError::Parse`]。
pub fn parse_unified_diff(patch: &str) -> Result<ParsedDiff, DiffError> {
    let mut parsed = ParsedDiff::default();
    let mut current: Option<Hunk> = None;

    for raw in patch.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);

        if let Some(rest) = line.strip_prefix("@@") {
            if let Some(hunk) = current.take() {
                hunk.check_shape()?;
                parsed.hunks.push(hunk);
            }
            current = Some(parse_hunk_header(rest)?);
            continue;
        }

        if let Some(rest) = line.strip_prefix("+++") {
            parsed.path = clean_path(rest);
            continue;
        }
        if line.starts_with("---") || line.starts_with("diff ") || line.starts_with("index ") {
            continue;
        }

        let Some(hunk) = current.as_mut() else {
            // hunk 之外的杂项（空行、注释）。
            continue;
        };

        if line.starts_with('\\') {
            // no-newline 标记：挂到上一行。
            match hunk.lines.last_mut() {
                Some(last) => last.no_newline = true,
                None => {
                    return Err(DiffError::Parse(
                        "diff 里出现了孤立的 no-newline 标记".into(),
                    ))
                }
            }
            continue;
        }

        let mut chars = line.chars();
        let prefix = chars.next();
        let text = chars.as_str();
        let kind = match prefix {
            Some(' ') => DiffLineKind::Context,
            Some('-') => DiffLineKind::Removed,
            Some('+') => DiffLineKind::Added,
            // 空行按「空上下文行」处理：有些工具会把「空格 + 空」写成真正的空行。
            None => DiffLineKind::Context,
            Some(other) => {
                return Err(DiffError::Parse(format!("无法识别的 diff 行前缀：{other}")));
            }
        };
        hunk.lines.push(DiffLine {
            kind,
            text: text.to_string(),
            no_newline: false,
        });
    }

    if let Some(hunk) = current.take() {
        hunk.check_shape()?;
        parsed.hunks.push(hunk);
    }
    Ok(parsed)
}

/// 解析 `@@ -1,4 +1,5 @@` 里 `@@` 之后的剩余部分。
fn parse_hunk_header(rest: &str) -> Result<Hunk, DiffError> {
    let body = rest.trim_end();
    let body = body.strip_suffix("@@").unwrap_or(body).trim();
    let mut parts = body.split_whitespace();
    let (Some(old), Some(new)) = (parts.next(), parts.next()) else {
        return Err(DiffError::Parse(format!("hunk 头不合法：@@{rest}")));
    };
    let old = old
        .strip_prefix('-')
        .ok_or_else(|| DiffError::Parse(format!("hunk 头缺少旧文件范围：{old}")))?;
    let new = new
        .strip_prefix('+')
        .ok_or_else(|| DiffError::Parse(format!("hunk 头缺少新文件范围：{new}")))?;
    let (old_start, old_lines) = parse_range(old)?;
    let (new_start, new_lines) = parse_range(new)?;
    Ok(Hunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        lines: Vec::new(),
    })
}

/// 解析 `10,7`，或省略行数的 `10`（等同 `10,1`）。
fn parse_range(text: &str) -> Result<(usize, usize), DiffError> {
    let (start, count) = match text.split_once(',') {
        Some((start, count)) => (start, count),
        None => (text, "1"),
    };
    let start = start
        .trim()
        .parse::<usize>()
        .map_err(|_| DiffError::Parse(format!("hunk 头行号不是数字：{start}")))?;
    let count = count
        .trim()
        .parse::<usize>()
        .map_err(|_| DiffError::Parse(format!("hunk 头行数不是数字：{count}")))?;
    Ok((start, count))
}

/// 把 `a/src/app.ts` / `b/src/app.ts` / `./src/app.ts` 折成 `src/app.ts`；
/// `/dev/null`（或 Windows 的 `nul`）返回 `None`。
fn clean_path(raw: &str) -> Option<String> {
    let token = raw.split_whitespace().next()?;
    if token == "/dev/null" || token.eq_ignore_ascii_case("nul") {
        return None;
    }
    let token = token
        .strip_prefix("a/")
        .or_else(|| token.strip_prefix("b/"))
        .unwrap_or(token);
    let token = token.strip_prefix("./").unwrap_or(token);
    Some(token.replace('\\', "/"))
}

// ---------------------------------------------------------------- 应用

/// 按 hunk 逐块应用：`accept[i] == false` 的块整块跳过（新增行丢弃，其余原样保留）。
///
/// `original` 是目标文件的当前内容。任一块的上下文对不上、块之间重叠或越界，
/// 都返回 [`DiffError::Conflict`]——校验与拼装在同一次遍历里完成，出错时调用方
/// 拿不到半成品，因此不会写出被改坏的文件。
pub fn apply_hunks(original: &str, hunks: &[Hunk], accept: &[bool]) -> Result<String, DiffError> {
    if hunks.len() != accept.len() {
        return Err(DiffError::Conflict(format!(
            "接受标记有 {} 个，hunk 有 {} 个，两者必须一致",
            accept.len(),
            hunks.len()
        )));
    }

    let old = Text::new(original);
    let mut out: Vec<(String, bool)> = Vec::with_capacity(old.lines.len());
    let mut cursor = 0usize;

    for (hunk, accepted) in hunks.iter().zip(accept) {
        hunk.check_shape().map_err(|err| match err {
            DiffError::Parse(message) => DiffError::Conflict(message),
            other => other,
        })?;

        let start = if hunk.old_lines == 0 {
            hunk.old_start
        } else {
            hunk.old_start.saturating_sub(1)
        };
        if start < cursor {
            return Err(DiffError::Conflict(format!(
                "hunk @@ -{},{}. 与上一个 hunk 重叠或顺序颠倒",
                hunk.old_start, hunk.old_lines
            )));
        }
        if start > old.lines.len() {
            return Err(DiffError::Conflict(format!(
                "hunk @@ -{},{}. 起点超出文件范围（文件共 {} 行）",
                hunk.old_start,
                hunk.old_lines,
                old.lines.len()
            )));
        }

        push_original(&mut out, &old, cursor, start);
        cursor = start;

        for line in &hunk.lines {
            match line.kind {
                // 上下文行：接受与否都留着（拒绝一块只是不采纳它的新增）。
                DiffLineKind::Context => {
                    check_context(&old, cursor, line)?;
                    push_original(&mut out, &old, cursor, cursor + 1);
                    cursor += 1;
                }
                // 删除行：接受就真删，拒绝就原样留着。
                DiffLineKind::Removed => {
                    check_context(&old, cursor, line)?;
                    if !accepted {
                        push_original(&mut out, &old, cursor, cursor + 1);
                    }
                    cursor += 1;
                }
                // 新增行：只有接受才写进去。
                DiffLineKind::Added => {
                    if *accepted {
                        out.push((line.text.clone(), line.no_newline));
                    }
                }
            }
        }
    }

    push_original(&mut out, &old, cursor, old.lines.len());
    Ok(assemble(&out))
}

/// 只接受指定序号的 hunk（序号按 patch 里的顺序，0 起）。
pub fn apply_selection(
    original: &str,
    hunks: &[Hunk],
    accepted: &[usize],
) -> Result<String, DiffError> {
    let mut flags = vec![false; hunks.len()];
    for index in accepted {
        let Some(slot) = flags.get_mut(*index) else {
            return Err(DiffError::Conflict(format!(
                "要接受的 hunk 序号 {index} 越界（共 {} 块）",
                hunks.len()
            )));
        };
        *slot = true;
    }
    apply_hunks(original, hunks, &flags)
}

/// 干跑校验：不产出内容，只判断这份 patch 能不能干净地贴上去。
pub fn validate_hunks(original: &str, hunks: &[Hunk]) -> Result<(), DiffError> {
    let accept = vec![true; hunks.len()];
    apply_hunks(original, hunks, &accept).map(|_| ())
}

/// 校验一行上下文：行号在范围内且文本一致。
fn check_context(old: &Text, index: usize, line: &DiffLine) -> Result<(), DiffError> {
    let Some(actual) = old.lines.get(index) else {
        return Err(DiffError::Conflict(format!(
            "第 {} 行已超出文件末尾（文件共 {} 行）",
            index + 1,
            old.lines.len()
        )));
    };
    if actual != &line.text {
        return Err(DiffError::Conflict(format!(
            "第 {} 行对不上：patch 期望 {:?}，文件实际 {:?}",
            index + 1,
            line.text,
            actual
        )));
    }
    Ok(())
}

/// 原样搬运 `[from, to)` 区间的旧行（带上各自的换行状态）。
fn push_original(out: &mut Vec<(String, bool)>, old: &Text, from: usize, to: usize) {
    for (offset, line) in old.lines[from..to].iter().enumerate() {
        out.push((line.clone(), old.no_newline_at(from + offset)));
    }
}

/// 把「行 + 换行状态」拼回文本。
fn assemble(lines: &[(String, bool)]) -> String {
    let Some((_, last_no_newline)) = lines.last() else {
        return String::new();
    };
    let mut text = lines
        .iter()
        .map(|(line, _)| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if !last_no_newline {
        text.push('\n');
    }
    text
}

// ---------------------------------------------------------------- 内部实现

/// 一次编辑动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Keep,
    Remove,
    Insert,
}

/// 按行切开的文本 + 是否以换行收尾。
#[derive(Debug, Clone)]
struct Text {
    lines: Vec<String>,
    trailing_newline: bool,
}

impl Text {
    fn new(text: &str) -> Self {
        if text.is_empty() {
            return Self {
                lines: Vec::new(),
                trailing_newline: false,
            };
        }
        let trailing_newline = text.ends_with('\n');
        let body = if trailing_newline {
            &text[..text.len() - 1]
        } else {
            text
        };
        Self {
            lines: body.split('\n').map(str::to_string).collect(),
            trailing_newline,
        }
    }

    /// 第 `index` 行是否「没有换行收尾的末行」。
    fn no_newline_at(&self, index: usize) -> bool {
        !self.trailing_newline && index + 1 == self.lines.len()
    }

    /// 行比较键：文本相同还不够，末行的换行状态也要一致。
    ///
    /// 少了这一维，`a\nb` → `a\nb\n` 这种「只补行尾换行」的改动会被判成无差异。
    fn keys(&self) -> Vec<(String, bool)> {
        self.lines
            .iter()
            .enumerate()
            .map(|(index, line)| (line.clone(), self.no_newline_at(index)))
            .collect()
    }
}

/// 求操作序列：先裁掉公共前后缀，再对中间段做差分。
fn diff_ops(old: &[(String, bool)], new: &[(String, bool)]) -> Vec<Op> {
    let (old_len, new_len) = (old.len(), new.len());

    let mut prefix = 0;
    while prefix < old_len && prefix < new_len && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_len - prefix
        && suffix < new_len - prefix
        && old[old_len - 1 - suffix] == new[new_len - 1 - suffix]
    {
        suffix += 1;
    }

    let mut ops: Vec<Op> = Vec::with_capacity(old_len + new_len);
    ops.extend(vec![Op::Keep; prefix]);

    let left = &old[prefix..old_len - suffix];
    let right = &new[prefix..new_len - suffix];
    if left.len() + right.len() > MAX_MYERS_CELLS {
        // 太大就整段替换：仍然合法，只是不精细。
        ops.extend(vec![Op::Remove; left.len()]);
        ops.extend(vec![Op::Insert; right.len()]);
    } else {
        ops.extend(myers(left, right));
    }

    ops.extend(vec![Op::Keep; suffix]);
    ops
}

/// Myers 贪心差分（调用方已裁掉公共前后缀）。
fn myers(old: &[(String, bool)], new: &[(String, bool)]) -> Vec<Op> {
    if old.is_empty() {
        return vec![Op::Insert; new.len()];
    }
    if new.is_empty() {
        return vec![Op::Remove; old.len()];
    }

    let n = old.len() as isize;
    let m = new.len() as isize;
    let max = (n + m) as usize;
    let offset = max as isize;

    // v[k] = 对角线 k（k = x - y）上走得最远的 x。
    let mut v = vec![0isize; 2 * max + 1];
    let mut trace: Vec<Vec<isize>> = Vec::with_capacity(max + 1);
    let mut final_d = 0usize;

    'search: for d in 0..=max {
        trace.push(v.clone());
        let di = d as isize;
        let mut k = -di;
        while k <= di {
            let index = (k + offset) as usize;
            let mut x = if k == -di || (k != di && v[index - 1] < v[index + 1]) {
                v[index + 1]
            } else {
                v[index - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && old[x as usize] == new[y as usize] {
                x += 1;
                y += 1;
            }
            v[index] = x;
            if x >= n && y >= m {
                final_d = d;
                break 'search;
            }
            k += 2;
        }
    }

    let mut ops: Vec<Op> = Vec::with_capacity((n + m) as usize);
    let mut x = n;
    let mut y = m;
    for d in (0..=final_d).rev() {
        if d == 0 {
            while x > 0 && y > 0 {
                ops.push(Op::Keep);
                x -= 1;
                y -= 1;
            }
            break;
        }
        let previous = &trace[d];
        let di = d as isize;
        let k = x - y;
        let index = (k + offset) as usize;
        let prev_k = if k == -di || (k != di && previous[index - 1] < previous[index + 1]) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = previous[(prev_k + offset) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            ops.push(Op::Keep);
            x -= 1;
            y -= 1;
        }
        if x == prev_x {
            ops.push(Op::Insert);
            y -= 1;
        } else {
            ops.push(Op::Remove);
            x -= 1;
        }
    }
    while x > 0 {
        ops.push(Op::Remove);
        x -= 1;
    }
    while y > 0 {
        ops.push(Op::Insert);
        y -= 1;
    }

    ops.reverse();
    ops
}

/// 把操作序列切成 hunk：变更块左右各扩 `context` 行，靠得太近的块合并。
fn build_hunks(old: &Text, new: &Text, ops: &[Op], context: usize) -> Vec<Hunk> {
    if ops.is_empty() {
        return Vec::new();
    }

    // 每个操作之前的行号游标（0 起），用于把操作序号换算成文件行号。
    let mut old_cursor = vec![0usize; ops.len() + 1];
    let mut new_cursor = vec![0usize; ops.len() + 1];
    for (index, op) in ops.iter().enumerate() {
        old_cursor[index + 1] =
            old_cursor[index] + usize::from(matches!(op, Op::Keep | Op::Remove));
        new_cursor[index + 1] =
            new_cursor[index] + usize::from(matches!(op, Op::Keep | Op::Insert));
    }

    // 1) 找出所有「连续非 Keep」的变更块。
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    let mut index = 0;
    while index < ops.len() {
        if ops[index] == Op::Keep {
            index += 1;
            continue;
        }
        let mut end = index;
        while end + 1 < ops.len() && ops[end + 1] != Op::Keep {
            end += 1;
        }
        blocks.push((index, end));
        index = end + 1;
    }

    // 2) 两个变更块之间的上下文行数 ≤ 2×context 时合并（与 git 行为一致）。
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in blocks {
        match merged.last_mut() {
            Some((_, previous_end)) if start - *previous_end - 1 <= 2 * context => {
                if end > *previous_end {
                    *previous_end = end;
                }
            }
            _ => merged.push((start, end)),
        }
    }

    // 3) 每个块左右各扩 context 行，再渲染成 hunk。
    let mut hunks = Vec::with_capacity(merged.len());
    for (start, end) in merged {
        let start = start.saturating_sub(context);
        let end = (end + context).min(ops.len() - 1);

        let old_start_index = old_cursor[start];
        let old_lines = old_cursor[end + 1] - old_start_index;
        let new_start_index = new_cursor[start];
        let new_lines = new_cursor[end + 1] - new_start_index;

        let mut lines = Vec::with_capacity(end - start + 1);
        for (offset, op) in ops[start..=end].iter().enumerate() {
            let cursor = start + offset;
            let (kind, text, no_newline) = match op {
                Op::Keep => {
                    let at = old_cursor[cursor];
                    (
                        DiffLineKind::Context,
                        old.lines[at].clone(),
                        old.no_newline_at(at),
                    )
                }
                Op::Remove => {
                    let at = old_cursor[cursor];
                    (
                        DiffLineKind::Removed,
                        old.lines[at].clone(),
                        old.no_newline_at(at),
                    )
                }
                Op::Insert => {
                    let at = new_cursor[cursor];
                    (
                        DiffLineKind::Added,
                        new.lines[at].clone(),
                        new.no_newline_at(at),
                    )
                }
            };
            lines.push(DiffLine {
                kind,
                text,
                no_newline,
            });
        }

        hunks.push(Hunk {
            // 行数为 0 时起点取「插入 / 删除点前一行」，与 GNU 约定一致。
            old_start: if old_lines == 0 {
                old_start_index
            } else {
                old_start_index + 1
            },
            old_lines,
            new_start: if new_lines == 0 {
                new_start_index
            } else {
                new_start_index + 1
            },
            new_lines,
            lines,
        });
    }
    hunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunks_of(original: &str, updated: &str) -> Vec<Hunk> {
        diff_hunks(original, updated, DEFAULT_CONTEXT)
    }

    fn line(kind: DiffLineKind, text: &str) -> DiffLine {
        DiffLine {
            kind,
            text: text.to_string(),
            no_newline: false,
        }
    }

    #[test]
    fn renders_file_header_hunk_header_and_context() {
        let original = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let updated = "a\nb\nc\nD\ne\nf\ng\nh\n";
        let patch = unified_diff("src/a.ts", original, updated, DEFAULT_CONTEXT);
        assert_eq!(
            patch,
            "--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1,7 +1,7 @@\n a\n b\n c\n-d\n+D\n e\n f\n g\n"
        );
    }

    #[test]
    fn identical_content_produces_nothing() {
        assert_eq!(
            unified_diff("a.txt", "a\nb\n", "a\nb\n", DEFAULT_CONTEXT),
            ""
        );
        assert!(hunks_of("a\nb\n", "a\nb\n").is_empty());
    }

    #[test]
    fn generated_output_is_deterministic() {
        let original = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let updated = "A\nb\nc\nd\ne\nf\ng\nh\ni\nJ\n";
        let first = unified_diff("a.txt", original, updated, DEFAULT_CONTEXT);
        let second = unified_diff("a.txt", original, updated, DEFAULT_CONTEXT);
        let third = unified_diff("a.txt", original, updated, DEFAULT_CONTEXT);
        assert_eq!(first, second, "同一对输入必须产出同一个字符串");
        assert_eq!(second, third);
        assert_eq!(
            hunks_of(original, updated).len(),
            2,
            "两处改动相隔够远，应该是两块"
        );
    }

    #[test]
    fn nearby_changes_share_one_hunk() {
        let original = "a\nb\nc\nd\ne\nf\ng\nh\ni\n";
        let updated = "a\nB\nc\nd\ne\nf\ng\nH\ni\n";
        assert_eq!(
            hunks_of(original, updated).len(),
            1,
            "相隔 5 行（≤ 2×context）应合并成一块"
        );
    }

    #[test]
    fn applying_every_hunk_round_trips() {
        let original = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n";
        let updated = "one\nTWO\nthree\nfour\nfive\nsix\nSEVEN\neight\nnine\n";
        let hunks = hunks_of(original, updated);
        let applied = apply_hunks(original, &hunks, &vec![true; hunks.len()]).unwrap();
        assert_eq!(applied, updated);
    }

    #[test]
    fn applying_a_subset_keeps_the_other_change_out() {
        // 两处改动必须真的分得开，否则测不到「逐块选择」：`DEFAULT_CONTEXT = 3` 时，
        // 只有间隔超过 `2 × context = 6` 行才会切成两块，否则按既有口径合并成一块
        // （见 `nearby_changes_share_one_hunk`）。这里动的第 2 行与第 11 行中间隔着
        // 8 行，正好跨过合并阈值，引擎必然产出两块。
        let original =
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\n";
        let updated = "one\nTWO\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\nELEVEN\ntwelve\n";
        let hunks = hunks_of(original, updated);
        assert_eq!(
            hunks.len(),
            2,
            "两处改动隔着 8 行（> 2×context），必须是两块"
        );

        // 只接受第一块：第二块的新增行不能落进来，原来的行也不能被删掉。
        let only_first = apply_selection(original, &hunks, &[0]).unwrap();
        assert_eq!(
            only_first,
            "one\nTWO\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\n",
            "未选中的第二块（第 11 行）必须原样留着"
        );

        // 只接受第二块：第一块同样不能落进来。
        let only_second = apply_selection(original, &hunks, &[1]).unwrap();
        assert_eq!(
            only_second,
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\nELEVEN\ntwelve\n",
            "未选中的第一块（第 2 行）必须原样留着"
        );

        let none = apply_selection(original, &hunks, &[]).unwrap();
        assert_eq!(none, original, "一块都不接受就等于原文件");

        let out_of_range = apply_selection(original, &hunks, &[2]).unwrap_err();
        assert!(matches!(out_of_range, DiffError::Conflict(_)));
    }

    #[test]
    fn stale_context_is_a_conflict() {
        let hunks = hunks_of("a\nb\nc\n", "a\nB\nc\n");
        let moved_on = "a\nb\nCHANGED\n";
        let err = apply_hunks(moved_on, &hunks, &[true]).unwrap_err();
        match err {
            DiffError::Conflict(message) => {
                assert!(message.contains("第 3 行"), "错误里要带出行号：{message}");
            }
            other => panic!("应是冲突，实际 {other:?}"),
        }
    }

    #[test]
    fn overlapping_and_out_of_range_hunks_are_rejected() {
        let overlapping = vec![
            Hunk {
                old_start: 1,
                old_lines: 2,
                new_start: 1,
                new_lines: 2,
                lines: vec![
                    line(DiffLineKind::Context, "a"),
                    line(DiffLineKind::Removed, "b"),
                ],
            },
            Hunk {
                old_start: 2,
                old_lines: 1,
                new_start: 2,
                new_lines: 1,
                lines: vec![line(DiffLineKind::Context, "b")],
            },
        ];
        assert!(matches!(
            apply_hunks("a\nb\nc\n", &overlapping, &[true, true]).unwrap_err(),
            DiffError::Conflict(_)
        ));

        let beyond = vec![Hunk {
            old_start: 9,
            old_lines: 1,
            new_start: 9,
            new_lines: 0,
            lines: vec![line(DiffLineKind::Removed, "z")],
        }];
        assert!(matches!(
            apply_hunks("a\nb\n", &beyond, &[true]).unwrap_err(),
            DiffError::Conflict(_)
        ));

        let mismatched_flags = apply_hunks("a\nb\n", &beyond, &[]).unwrap_err();
        assert!(matches!(mismatched_flags, DiffError::Conflict(_)));
    }

    #[test]
    fn missing_trailing_newline_is_carried_through() {
        let original = "a\nb";
        let updated = "a\nB";
        let patch = unified_diff("a.txt", original, updated, DEFAULT_CONTEXT);
        assert!(
            patch.contains("-b\n\\ No newline at end of file\n"),
            "删除行要带标记：{patch}"
        );
        assert!(
            patch.contains("+B\n\\ No newline at end of file\n"),
            "新增行要带标记：{patch}"
        );

        let parsed = parse_unified_diff(&patch).unwrap();
        let applied = apply_hunks(original, &parsed.hunks, &[true]).unwrap();
        assert_eq!(applied, updated, "应用后不能凭空多出一个换行");
    }

    #[test]
    fn adding_a_trailing_newline_is_visible() {
        let original = "a\nb";
        let updated = "a\nb\n";
        let patch = unified_diff("a.txt", original, updated, DEFAULT_CONTEXT);
        assert_eq!(
            patch,
            "--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+b\n"
        );
        let parsed = parse_unified_diff(&patch).unwrap();
        assert_eq!(
            apply_hunks(original, &parsed.hunks, &[true]).unwrap(),
            updated
        );
    }

    #[test]
    fn insertion_into_empty_file_uses_gnu_zero_range() {
        let hunks = hunks_of("", "x\n");
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_start, 0);
        assert_eq!(hunks[0].old_lines, 0);
        assert_eq!(hunks[0].new_start, 1);
        assert_eq!(hunks[0].new_lines, 1);
        assert_eq!(
            unified_diff("a.txt", "", "x\n", DEFAULT_CONTEXT),
            "--- a/a.txt\n+++ b/a.txt\n@@ -0,0 +1,1 @@\n+x\n"
        );
        assert_eq!(apply_hunks("", &hunks, &[true]).unwrap(), "x\n");
    }

    #[test]
    fn deleting_everything_uses_zero_range_on_the_new_side() {
        let patch = unified_diff("a.txt", "a\nb\n", "", DEFAULT_CONTEXT);
        assert_eq!(patch, "--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +0,0 @@\n-a\n-b\n");
        let parsed = parse_unified_diff(&patch).unwrap();
        assert_eq!(apply_hunks("a\nb\n", &parsed.hunks, &[true]).unwrap(), "");
    }

    #[test]
    fn rendered_patch_parses_back_into_the_same_hunks() {
        let original = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let updated = "a\nB\nc\nd\ne\nf\ng\nh\ni\nJ";
        let hunks = hunks_of(original, updated);
        let patch = unified_diff("deep/dir/file.txt", original, updated, DEFAULT_CONTEXT);
        let parsed = parse_unified_diff(&patch).unwrap();
        assert_eq!(parsed.path.as_deref(), Some("deep/dir/file.txt"));
        assert_eq!(parsed.hunks, hunks, "渲染再解析必须回到同一组 hunk");
        assert_eq!(
            apply_hunks(original, &parsed.hunks, &[true; 2]).unwrap(),
            updated
        );
    }

    #[test]
    fn malformed_patch_is_a_parse_error() {
        let bad_prefix = "--- a/a\n+++ b/a\n@@ -1,1 +1,1 @@\n?oops\n";
        assert!(matches!(
            parse_unified_diff(bad_prefix).unwrap_err(),
            DiffError::Parse(_)
        ));

        let bad_header = "@@ -1,x +1,1 @@\n a\n";
        assert!(matches!(
            parse_unified_diff(bad_header).unwrap_err(),
            DiffError::Parse(_)
        ));

        let shape_mismatch = "--- a/a\n+++ b/a\n@@ -1,5 +1,5 @@\n a\n";
        assert!(matches!(
            parse_unified_diff(shape_mismatch).unwrap_err(),
            DiffError::Parse(_)
        ));
    }

    #[test]
    fn validate_reports_conflict_without_producing_text() {
        let hunks = hunks_of("a\nb\nc\n", "a\nB\nc\n");
        assert!(validate_hunks("a\nb\nc\n", &hunks).is_ok());
        assert!(validate_hunks("a\nb\nD\n", &hunks).is_err());
    }

    #[test]
    fn crlf_patch_is_tolerated() {
        let patch = "--- a/a.txt\r\n+++ b/a.txt\r\n@@ -1,2 +1,2 @@\r\n a\r\n-b\r\n+B\r\n";
        let parsed = parse_unified_diff(patch).unwrap();
        assert_eq!(parsed.hunks.len(), 1);
        assert_eq!(
            apply_hunks("a\nb\n", &parsed.hunks, &[true]).unwrap(),
            "a\nB\n"
        );
    }
}
