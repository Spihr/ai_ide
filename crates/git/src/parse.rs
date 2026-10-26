//! git 输出的解析。
//!
//! 两处解析都不太平：`status --porcelain=v2 -z` 的每条记录以 `NUL` 结尾，重命名条目
//! 还会多带一个「原路径」记录；`for-each-ref` 的字段本身可能含空格（分支名不能含
//! 空格，但上游表达式可以），所以用一个不会出现在字段里的分隔符：`NUL`。
//!
//! 这里是纯函数，输入输出都是字节 / 字符串，因此可以拿固定样本做单测——不必为了
//! 测「重命名怎么解析」去构造一个真仓库。

use crate::model::{
    EntryKind, GitBranchInfo, GitConflictBlock, GitFileHunks, GitHunk, GitStatus, GitStatusEntry,
};

/// 解析 `git status --porcelain=v2 --branch --untracked-files=all -z --relative`。
///
/// 记录类型：
///
/// | 前缀 | 含义 |
/// |---|---|
/// | `1` | 普通变更（改 / 增 / 删 / 类型变化） |
/// | `2` | 重命名或复制，路径后紧跟一条原路径记录 |
/// | `u` | 冲突未解决 |
/// | `?` | 未跟踪 |
/// | `!` | 被忽略（本 crate 不请求，出现则跳过） |
/// | `#` | 分支头信息 |
pub(crate) fn parse_status(stdout: &[u8]) -> GitStatus {
    let text = String::from_utf8_lossy(stdout);
    let mut status = GitStatus::default();
    let segments: Vec<&str> = text.split('\0').collect();
    let mut index = 0;

    while index < segments.len() {
        let raw = segments[index];
        index += 1;
        // `-z` 下头部行可能仍以 `\n` 结尾，前导换行要吃掉。
        let segment = raw.trim_start_matches('\n');
        if segment.is_empty() {
            continue;
        }
        if segment.starts_with('#') {
            for line in segment.split('\n') {
                parse_header(line, &mut status);
            }
            continue;
        }
        match segment.as_bytes()[0] {
            b'1' => {
                if let Some(entry) = parse_ordinary(segment) {
                    status.entries.push(entry);
                }
            }
            b'2' => {
                // 原路径是紧随其后的一条独立记录。
                let original = segments.get(index).map(|value| {
                    value
                        .trim_start_matches('\n')
                        .trim_end_matches('\n')
                        .to_string()
                });
                index += 1;
                if let Some(entry) = parse_renamed(segment, original) {
                    status.entries.push(entry);
                }
            }
            b'u' => {
                if let Some(entry) = parse_unmerged(segment) {
                    status.entries.push(entry);
                }
            }
            b'?' => {
                let path = segment.get(2..).unwrap_or("").to_string();
                if !path.is_empty() {
                    status.entries.push(GitStatusEntry {
                        path,
                        original_path: None,
                        index: ".".to_string(),
                        worktree: "?".to_string(),
                        kind: EntryKind::Untracked,
                    });
                }
            }
            // 被忽略的条目：内核不请求（未带 `--ignored`），出现也不该进变更清单。
            b'!' => {}
            _ => {}
        }
    }

    status
}

fn parse_header(line: &str, status: &mut GitStatus) {
    let Some(rest) = line.strip_prefix("# ") else {
        return;
    };
    if let Some(value) = rest.strip_prefix("branch.oid ") {
        status.head_oid = if value == "(initial)" {
            None
        } else {
            Some(value.to_string())
        };
    } else if let Some(value) = rest.strip_prefix("branch.head ") {
        if value == "(detached)" {
            status.detached = true;
            status.branch = None;
        } else {
            status.detached = false;
            status.branch = Some(value.to_string());
        }
    } else if let Some(value) = rest.strip_prefix("branch.upstream ") {
        status.upstream = Some(value.to_string());
    } else if let Some(value) = rest.strip_prefix("branch.ab ") {
        let mut parts = value.split(' ');
        if let Some(ahead) = parts
            .next()
            .and_then(|part| part.trim_start_matches('+').parse().ok())
        {
            status.ahead = ahead;
        }
        if let Some(behind) = parts
            .next()
            .and_then(|part| part.trim_start_matches('-').parse().ok())
        {
            status.behind = behind;
        }
    }
}

/// `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>`
fn parse_ordinary(segment: &str) -> Option<GitStatusEntry> {
    let fields: Vec<&str> = segment.splitn(9, ' ').collect();
    if fields.len() != 9 {
        return None;
    }
    Some(GitStatusEntry {
        path: fields[8].to_string(),
        original_path: None,
        index: xy_index(fields[1]),
        worktree: xy_worktree(fields[1]),
        kind: EntryKind::Changed,
    })
}

/// `2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>` + 独立记录的原路径
fn parse_renamed(segment: &str, original: Option<String>) -> Option<GitStatusEntry> {
    let fields: Vec<&str> = segment.splitn(10, ' ').collect();
    if fields.len() != 10 {
        return None;
    }
    Some(GitStatusEntry {
        path: fields[9].to_string(),
        original_path: original.filter(|path| !path.is_empty()),
        index: xy_index(fields[1]),
        worktree: xy_worktree(fields[1]),
        kind: EntryKind::Renamed,
    })
}

/// `u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>`
fn parse_unmerged(segment: &str) -> Option<GitStatusEntry> {
    let fields: Vec<&str> = segment.splitn(11, ' ').collect();
    if fields.len() != 11 {
        return None;
    }
    Some(GitStatusEntry {
        path: fields[10].to_string(),
        original_path: None,
        index: xy_index(fields[1]),
        worktree: xy_worktree(fields[1]),
        kind: EntryKind::Unmerged,
    })
}

fn xy_index(xy: &str) -> String {
    xy.chars().next().unwrap_or('.').to_string()
}

fn xy_worktree(xy: &str) -> String {
    xy.chars().nth(1).unwrap_or('.').to_string()
}

/// 解析 `git for-each-ref --format=%(refname)%00%(objectname)%00%(objectname:short)%00%(upstream:short)%00%(HEAD)`。
///
/// 跳过 `refs/remotes/<remote>/HEAD` 这类符号引用：它不是可切换的分支，列出来只会
/// 让 UI 多一个点了会报错的条目。
pub(crate) fn parse_branches(stdout: &str) -> Vec<GitBranchInfo> {
    let mut locals = Vec::new();
    let mut remotes = Vec::new();
    for line in stdout.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        // for-each-ref 用 `%00` 表示 NUL，实际输出就是 NUL 字节。
        let fields: Vec<&str> = line.split('\0').collect();
        if fields.len() < 5 {
            continue;
        }
        let refname = fields[0];
        let (name, is_remote) = if let Some(name) = refname.strip_prefix("refs/heads/") {
            (name, false)
        } else if let Some(name) = refname.strip_prefix("refs/remotes/") {
            if name.ends_with("/HEAD") {
                continue;
            }
            (name, true)
        } else {
            continue;
        };
        let upstream = fields[3];
        let info = GitBranchInfo {
            name: name.to_string(),
            oid: fields[1].to_string(),
            short_oid: fields[2].to_string(),
            is_current: fields[4].trim() == "*",
            is_remote,
            upstream: if upstream.is_empty() {
                None
            } else {
                Some(upstream.to_string())
            },
        };
        if is_remote {
            remotes.push(info);
        } else {
            locals.push(info);
        }
    }
    locals.sort_by(|a, b| a.name.cmp(&b.name));
    remotes.sort_by(|a, b| a.name.cmp(&b.name));
    locals.extend(remotes);
    locals
}

// ---------------------------------------------------------------------------
// unified diff → 可寻址的 hunk
// ---------------------------------------------------------------------------

/// 一个文件的 diff 片段：文件头行 + 若干 hunk。
///
/// `header_lines` 保留「`diff --git` 之后、第一个 `@@` 之前」的原始行（`index`、
/// `new file mode`、`rename from` 等）。按块暂存时要靠它们让 `git apply` 知道这是
/// 新增、改名还是纯内容改动——**不能**只回 `--- /+++` 两行。
pub(crate) struct ParsedDiffFile {
    /// 新侧路径（`b/` 侧）；删除文件时为旧侧路径。
    pub path: String,
    /// 旧侧路径（`a/` 侧）；新增文件时为 `None`。
    pub old_path: Option<String>,
    /// 旧侧是 `/dev/null`（新增文件）。
    pub old_null: bool,
    /// 新侧是 `/dev/null`（删除文件）。
    pub new_null: bool,
    /// 文件头行（原样）。
    pub header_lines: Vec<String>,
    /// 是否是二进制文件。
    pub binary: bool,
    /// 按出现顺序的 hunk，`index` 即下标。
    pub hunks: Vec<GitHunk>,
}

impl ParsedDiffFile {
    /// 翻成对外模型（不含二进制标记以外的元信息）。
    pub(crate) fn to_model(&self) -> GitFileHunks {
        GitFileHunks {
            path: self.path.clone(),
            binary: self.binary,
            hunks: self.hunks.clone(),
        }
    }
}

/// 二进制文件的标记行。
///
/// 两种形态都要认：现代 git 输出 `Binary files a/x and b/x differ`；`--binary` 或
/// 部分场景输出 `GIT binary patch` 后跟 base85 正文。无论哪种，都**没有** hunk。
fn is_binary_marker(line: &str) -> bool {
    line.starts_with("Binary files ") || line.starts_with("GIT binary patch")
}

/// 解析一段 unified diff（`git diff` 的原文）成按文件分组的 hunk。
///
/// 只认 git 自己输出的形态，因此可以放心依赖几条稳定事实：
///
/// - 文件之间以 `diff --git ` 开头；
/// - hunk 以 `@@ -a,b +c,d @@[ 上下文]` 开头；
/// - hunk 正文每行恰好一个前缀字符（` ` / `+` / `-`），
///   外加一种特殊情况：`\ No newline at end of file`（`\` 开头）；
/// - 二进制文件只给一句 `Binary files ... differ`，没有 hunk 正文。
///
/// 正文行用「前缀字符」判断而不是「按 hunk 头里的行数精确计数」：后者要在
/// `\ No newline` 上做特例（这一行不计入任何一侧的行数），一旦算错就会把下一块
/// hunk 的正文吞进上一块——前缀判断没有这个风险，且 git 的正文行永远带前缀。
pub(crate) fn parse_diff(text: &str) -> Vec<ParsedDiffFile> {
    let mut files: Vec<ParsedDiffFile> = Vec::new();
    let mut current: Option<ParsedDiffFile> = None;
    let mut in_body = false;

    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);

        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(file) = current.take() {
                files.push(file);
            }
            current = Some(new_diff_file(rest));
            in_body = false;
            continue;
        }

        let Some(file) = current.as_mut() else {
            // `diff --git` 之前的内容（例如 `warning:` 行）一律丢弃。
            continue;
        };

        if is_binary_marker(line) {
            file.binary = true;
            in_body = false;
            file.header_lines.push(line.to_string());
            continue;
        }

        if line.starts_with("@@") {
            in_body = false;
            if let Some(hunk) = parse_hunk_header(line, &file.path, file.hunks.len() as u32) {
                file.hunks.push(hunk);
                in_body = true;
            }
            continue;
        }

        if in_body {
            let is_body_line =
                line.starts_with('\\') || matches!(line.chars().next(), Some(' ' | '+' | '-'));
            if is_body_line {
                if let Some(hunk) = file.hunks.last_mut() {
                    hunk.lines.push(line.to_string());
                }
                continue;
            }
            // 正文结束（空行或新的文件头）——退回去当文件头处理。
            in_body = false;
        }

        if let Some(rest) = line.strip_prefix("--- ") {
            match side_path(rest) {
                Some(path) => {
                    file.old_null = false;
                    file.old_path = Some(path);
                }
                None => {
                    file.old_null = true;
                    file.old_path = None;
                }
            }
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            match side_path(rest) {
                Some(path) => {
                    file.new_null = false;
                    file.path = path;
                }
                None => file.new_null = true,
            }
        }
        file.header_lines.push(line.to_string());
    }

    if let Some(file) = current.take() {
        files.push(file);
    }
    files
}

/// 从 `diff --git a/x b/y` 的剩余部分取两侧路径。
fn new_diff_file(rest: &str) -> ParsedDiffFile {
    let (old_path, path) = split_diff_header_paths(rest);
    ParsedDiffFile {
        path,
        old_path,
        old_null: false,
        new_null: false,
        header_lines: Vec::new(),
        binary: false,
        hunks: Vec::new(),
    }
}

/// 切 `a/x b/y`。
///
/// 路径里可能含空格（`a/my notes.md b/my notes.md`），所以按**最后一个** ` b/`
/// 切；切不出来时退回「按第一个空格切」。--- /+++ 行若出现会覆盖这里的结论。
fn split_diff_header_paths(rest: &str) -> (Option<String>, String) {
    let (a, b) = match rest.rfind(" b/") {
        Some(pos) => (&rest[..pos], &rest[pos + 1..]),
        None => match rest.split_once(' ') {
            Some((a, b)) => (a, b),
            None => return (None, unquote(rest)),
        },
    };
    let old = a.strip_prefix("a/").unwrap_or(a);
    let new = b.strip_prefix("b/").unwrap_or(b);
    let old = unquote(old);
    let new = unquote(new);
    (if old.is_empty() { None } else { Some(old) }, new)
}

/// `--- a/x` / `+++ b/x` 的路径；`/dev/null` 返回 `None`（表示这一侧不存在）。
fn side_path(rest: &str) -> Option<String> {
    if rest.starts_with("/dev/null") {
        return None;
    }
    let path = rest.strip_prefix("a/").or_else(|| rest.strip_prefix("b/"));
    let path = unquote(path.unwrap_or(rest));
    if path.is_empty() {
        return None;
    }
    Some(path)
}

/// 去掉 git 给「含特殊字符的路径」加的双引号与转义。
///
/// 非 ASCII 路径默认**不加引号**（`core.quotePath` 只影响控制字符与反斜杠），
/// 所以这里只需要处理最常见的 `\\` / `\"` / `\t` / `\n`。
fn unquote(text: &str) -> String {
    let text = text.trim();
    let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) else {
        return text.to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// 解析 `@@ -old_start,old_lines +new_start,new_lines @@[ 上下文]`。
fn parse_hunk_header(line: &str, path: &str, index: u32) -> Option<GitHunk> {
    let rest = line.strip_prefix("@@ ")?;
    let close = rest.find(" @@")?;
    let ranges = &rest[..close];
    // `@@` 之后原样保留（含 git 写入的前导空格），回喂补丁时逐字还原。
    let header = rest[close + 3..].to_string();
    let mut parts = ranges.split(' ');
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let (old_start, old_lines) = parse_range(old)?;
    let (new_start, new_lines) = parse_range(new)?;
    Some(GitHunk {
        path: path.to_string(),
        index,
        old_start,
        old_lines,
        new_start,
        new_lines,
        header,
        lines: Vec::new(),
    })
}

/// `5` → `(5, 1)`；`5,7` → `(5, 7)`。行数省略时 git 约定为 1。
fn parse_range(text: &str) -> Option<(u32, u32)> {
    match text.split_once(',') {
        Some((start, lines)) => Some((start.parse().ok()?, lines.parse().ok()?)),
        None => Some((text.parse().ok()?, 1)),
    }
}

/// 把单块 hunk 拼成可以喂给 `git apply --cached` 的最小补丁。
///
/// 只带这块自己的 `@@` 头与正文，但保留文件头里**影响索引写入**的行（模式变化、
/// 新增/删除标记、改名）。其余行（例如 `index` 之外的装饰）不带，避免让 `git apply`
/// 去校验它不需要的前置条件。
pub(crate) fn hunk_patch(file: &ParsedDiffFile, hunk: &GitHunk) -> String {
    let old_name = file.old_path.clone().unwrap_or_else(|| file.path.clone());
    let mut out = String::new();
    out.push_str(&format!("diff --git a/{} b/{}\n", old_name, file.path));
    for line in &file.header_lines {
        let keep = line.starts_with("old mode ")
            || line.starts_with("new mode ")
            || line.starts_with("deleted file mode ")
            || line.starts_with("new file mode ")
            || line.starts_with("similarity index ")
            || line.starts_with("rename from ")
            || line.starts_with("rename to ");
        if keep {
            out.push_str(line);
            out.push('\n');
        }
    }
    if file.old_null {
        out.push_str("--- /dev/null\n");
    } else {
        out.push_str(&format!("--- a/{}\n", old_name));
    }
    if file.new_null {
        out.push_str("+++ /dev/null\n");
    } else {
        out.push_str(&format!("+++ b/{}\n", file.path));
    }
    out.push_str(&format!(
        "@@ -{},{} +{},{} @@{}\n",
        hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines, hunk.header
    ));
    for line in &hunk.lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// 冲突标记扫描（缺口第五批）
// ---------------------------------------------------------------------------

/// 冲突标记的重复字符数。
///
/// git 写出来的一律是整整 7 个（`<<<<<<<` / `|||||||` / `=======` / `>>>>>>>`），
/// 因此这里按**恰好 7 个**认。这条不只是格式洁癖：正文里本来就存在看起来像标记的
/// 东西——Markdown 的分隔线 `--------`、RST 标题下划线 `========`、从别处粘进文件的
/// diff 片段——放宽成「7 个及以上」会让 `========`（8 个）之类的行被当成段分隔。
const MARKER_WIDTH: usize = 7;

/// 带标签的标记行最短长度：7 个标记 + 1 个空格 + 至少 1 个字符的标签。
const LABELED_MARKER_MIN_LEN: usize = MARKER_WIDTH + 2;

/// 识别一行是不是冲突标记，返回 `(标记字符, 标签)`；`=======` 的标签是空串。
///
/// 三重门槛，缺一不可：
///
/// 1. 首字符必须是四个标记字符之一；
/// 2. 前缀**恰好** [`MARKER_WIDTH`] 个重复字符；
/// 3. `<<<<<<<` / `|||||||` / `>>>>>>>` 后面必须紧跟空格与**非空**标签（git 会把
///    分支名或提交说明写在那里），`=======` 后面必须是空的。
///
/// 第 3 条是最关键的一条：`<<<<<<<` 单独出现在代码里（注释、正则、字符串）时后面
/// 往往没有 ` <label>`，据此就能把它留在正文里。
fn conflict_marker(line: &str) -> Option<(char, String)> {
    let lead = line.chars().next()?;
    if !matches!(lead, '<' | '|' | '=' | '>') {
        return None;
    }
    let width = line
        .as_bytes()
        .iter()
        .take_while(|byte| **byte == lead as u8)
        .count();
    if width != MARKER_WIDTH {
        return None;
    }
    let rest = &line[width..];
    if lead == '=' {
        return if rest.trim().is_empty() {
            Some((lead, String::new()))
        } else {
            None
        };
    }
    if line.len() < LABELED_MARKER_MIN_LEN {
        return None;
    }
    let label = rest.strip_prefix(' ')?.trim().to_string();
    if label.is_empty() {
        return None;
    }
    Some((lead, label))
}

/// 冲突块当前收的是哪一段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// `<<<<<<<` 之后、`|||||||` / `=======` 之前。
    Ours,
    /// `|||||||` 之后、`=======` 之前（只有 `diff3` 风格才有这一段）。
    Base,
    /// `=======` 之后、`>>>>>>>` 之前。
    Theirs,
}

/// 正在收集的冲突块。
struct OpenBlock {
    start_line: u32,
    ours_label: String,
    base_label: Option<String>,
    section: Section,
    ours: Vec<String>,
    base: Option<Vec<String>>,
    theirs: Vec<String>,
    /// 已经吃进来的原始行（含标记行）。配对校验失败时整块退回正文，一个字都不能丢。
    raw: Vec<String>,
}

impl OpenBlock {
    fn start(line_no: u32, label: String, marker_line: &str) -> Self {
        Self {
            start_line: line_no,
            ours_label: label,
            base_label: None,
            section: Section::Ours,
            ours: Vec::new(),
            base: None,
            theirs: Vec::new(),
            raw: vec![marker_line.to_string()],
        }
    }
}

/// 冲突文本里的一个片段：普通行，或一个解析成功的冲突块。
enum ConflictPiece {
    Lines(Vec<String>),
    Block(GitConflictBlock),
}

fn take_plain(plain: &mut Vec<String>) -> Option<ConflictPiece> {
    if plain.is_empty() {
        return None;
    }
    Some(ConflictPiece::Lines(std::mem::take(plain)))
}

/// 一个文本文件的冲突扫描结果。
///
/// 除了冲突块，还记住原文件的**换行风格**与**末尾有没有换行**：解决冲突要按原样
/// 写回，否则「解决完了」会顺手把整份文件的换行从 `CRLF` 改成 `LF`，`git status`
/// 里就成了一处用户没做过的改动。
pub(crate) struct ConflictScan {
    pieces: Vec<ConflictPiece>,
    eol: &'static str,
    trailing_newline: bool,
}

impl ConflictScan {
    /// 解析出的冲突块（按出现顺序）。
    pub(crate) fn blocks(&self) -> Vec<&GitConflictBlock> {
        self.pieces
            .iter()
            .filter_map(|piece| match piece {
                ConflictPiece::Block(block) => Some(block),
                ConflictPiece::Lines(_) => None,
            })
            .collect()
    }

    /// 冲突块数量。
    pub(crate) fn block_count(&self) -> u32 {
        self.blocks().len() as u32
    }

    /// 第一个冲突块的起始行（1 起），没有则为 `None`。
    pub(crate) fn first_start_line(&self) -> Option<u32> {
        self.pieces.iter().find_map(|piece| match piece {
            ConflictPiece::Block(block) => Some(block.start_line),
            ConflictPiece::Lines(_) => None,
        })
    }

    /// 按 `pick` 挑出的内容重建全文：冲突块被替换，冲突块之外的每一行逐字保留。
    pub(crate) fn render<F>(&self, pick: F) -> String
    where
        F: Fn(&GitConflictBlock) -> Vec<String>,
    {
        let mut out: Vec<String> = Vec::new();
        for piece in &self.pieces {
            match piece {
                ConflictPiece::Lines(lines) => out.extend(lines.iter().cloned()),
                ConflictPiece::Block(block) => out.extend(pick(block)),
            }
        }
        if out.is_empty() {
            return String::new();
        }
        let mut text = out.join(self.eol);
        if self.trailing_newline {
            text.push_str(self.eol);
        }
        text
    }
}

/// 扫描一份冲突文本，切出所有**成对**的冲突块。
///
/// 配对校验的口径：`<<<<<<<` → （可选 `|||||||`）→ `=======` → `>>>>>>>`，四者必须
/// 按序齐全才算一个块。缺任何一环（`<<<<<<<` 之后到文件结尾都没等到 `>>>>>>>`、
/// 孤立的 `=======`、`<<<<<<<` 里又套了一个 `<<<<<<<`）都不会被认成冲突块，而是
/// **整块连同上一个未闭合块的内容一起退回正文**。宁可报「没有冲突块」让上层走
/// 整份取一侧的路，也不能凭半截标记重建出一份缺了半个文件的「解决结果」。
pub(crate) fn scan_conflict_text(text: &str) -> ConflictScan {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing_newline = text.ends_with('\n');
    let mut pieces: Vec<ConflictPiece> = Vec::new();
    let mut plain: Vec<String> = Vec::new();
    let mut block_count: u32 = 0;
    let mut open: Option<OpenBlock> = None;
    let mut line_no: u32 = 0;

    for line in text.lines() {
        line_no += 1;
        let marker = conflict_marker(line);
        let Some(mut current) = open.take() else {
            match marker {
                Some(('<', label)) => {
                    pieces.extend(take_plain(&mut plain));
                    open = Some(OpenBlock::start(line_no, label, line));
                }
                // 孤立的 `=======` / `|||||||` / `>>>>>>>`：前面没有 `<<<<<<<`，
                // 只是正文里的一行。
                _ => plain.push(line.to_string()),
            }
            continue;
        };

        match marker {
            // 上一个块还没闭合就又来了 `<<<<<<<`：整块退回正文，再以这一行为新开头。
            Some(('<', label)) => {
                plain.append(&mut current.raw);
                open = Some(OpenBlock::start(line_no, label, line));
            }
            // `|||||||` 只能出现在 ours 段之后、且只能出现一次。
            Some(('|', label)) if current.section == Section::Ours && current.base.is_none() => {
                current.raw.push(line.to_string());
                current.base = Some(Vec::new());
                current.base_label = Some(label);
                current.section = Section::Base;
                open = Some(current);
            }
            // `=======` 把 ours（或 diff3 的 base）与 theirs 分开；重复出现的不算标记。
            Some(('=', _)) if current.section != Section::Theirs => {
                current.raw.push(line.to_string());
                current.section = Section::Theirs;
                open = Some(current);
            }
            // `>>>>>>>` 收尾：只有已经从 `=======` 走到 theirs 段才认。
            Some(('>', label)) if current.section == Section::Theirs => {
                current.raw.push(line.to_string());
                pieces.extend(take_plain(&mut plain));
                pieces.push(ConflictPiece::Block(GitConflictBlock {
                    index: block_count,
                    start_line: current.start_line,
                    end_line: line_no,
                    ours_label: current.ours_label,
                    base_label: current.base_label,
                    theirs_label: label,
                    ours: current.ours,
                    base: current.base,
                    theirs: current.theirs,
                }));
                block_count += 1;
            }
            // 其余（非标记行、或本块接不上的标记）：当内容收进当前段。
            _ => {
                current.raw.push(line.to_string());
                match current.section {
                    Section::Ours => current.ours.push(line.to_string()),
                    Section::Base => current
                        .base
                        .get_or_insert_with(Vec::new)
                        .push(line.to_string()),
                    Section::Theirs => current.theirs.push(line.to_string()),
                }
                open = Some(current);
            }
        }
    }

    if let Some(mut current) = open.take() {
        // 到文件结尾都没闭合：整块退回正文。
        pieces.extend(take_plain(&mut plain));
        plain.append(&mut current.raw);
    }
    pieces.extend(take_plain(&mut plain));

    ConflictScan {
        pieces,
        eol,
        trailing_newline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_of(records: &[&str]) -> GitStatus {
        parse_status(records.join("\0").as_bytes())
    }

    #[test]
    fn branch_headers_are_parsed() {
        let status = status_of(&[
            "# branch.oid 8b0e2b1d0f0a1c2d3e4f5a6b7c8d9e0f1a2b3c4d",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -3",
        ]);
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert!(!status.detached);
        assert_eq!(
            status.head_oid.as_deref(),
            Some("8b0e2b1d0f0a1c2d3e4f5a6b7c8d9e0f1a2b3c4d")
        );
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!((status.ahead, status.behind), (2, 3));
        assert!(status.is_clean());
    }

    #[test]
    fn initial_repository_has_no_head_but_keeps_branch() {
        let status = status_of(&["# branch.oid (initial)", "# branch.head main"]);
        assert!(status.head_oid.is_none());
        assert_eq!(status.branch.as_deref(), Some("main"));
    }

    #[test]
    fn detached_head_reports_detached_without_branch() {
        let status = status_of(&["# branch.oid abc123", "# branch.head (detached)"]);
        assert!(status.detached);
        assert!(status.branch.is_none());
    }

    #[test]
    fn ordinary_and_untracked_entries_are_parsed() {
        let status = status_of(&[
            "# branch.head main",
            "1 .M N... 100644 100644 100644 aaaa bbbb src/main.rs",
            "? 文档/说明.md",
        ]);
        assert_eq!(status.entries.len(), 2);
        let changed = &status.entries[0];
        assert_eq!(changed.path, "src/main.rs");
        assert_eq!(
            (changed.index.as_str(), changed.worktree.as_str()),
            (".", "M")
        );
        assert_eq!(changed.kind, EntryKind::Changed);
        let untracked = &status.entries[1];
        assert_eq!(untracked.path, "文档/说明.md");
        assert_eq!(untracked.worktree, "?");
        assert_eq!(untracked.kind, EntryKind::Untracked);
    }

    #[test]
    fn staged_addition_reports_index_letter() {
        let status = status_of(&["1 A. N... 000000 100644 100644 0000 aaaa new.txt"]);
        let entry = &status.entries[0];
        assert_eq!(entry.index, "A");
        assert_eq!(entry.worktree, ".");
    }

    /// 重命名的原路径是**紧随其后的一条独立记录**，`-z` 下路径本身不会被引号包起来，
    /// 也不能按行切——这条用例就是防有人把解析改回 `lines()`。
    #[test]
    fn rename_consumes_the_next_record_as_original_path() {
        let status = parse_status(
            b"1 .M N... 100644 100644 100644 aaaa bbbb keep.txt\0\
              2 R. N... 100644 100644 100644 aaaa bbbb R100 new name.txt\0old name.txt\0\
              1 M. N... 100644 100644 100644 aaaa bbbb tail.txt\0",
        );
        assert_eq!(status.entries.len(), 3, "{:?}", status.entries);
        let renamed = &status.entries[1];
        assert_eq!(renamed.path, "new name.txt");
        assert_eq!(renamed.original_path.as_deref(), Some("old name.txt"));
        assert_eq!(renamed.kind, EntryKind::Renamed);
        // 原路径不能被当成一条独立变更。
        assert_eq!(status.entries[2].path, "tail.txt");
    }

    #[test]
    fn unmerged_entries_are_marked() {
        let status =
            status_of(&["u UU N... 100644 100644 100644 100644 aaaa bbbb cccc conflict.txt"]);
        let entry = &status.entries[0];
        assert_eq!(entry.kind, EntryKind::Unmerged);
        assert_eq!(entry.path, "conflict.txt");
        assert_eq!((entry.index.as_str(), entry.worktree.as_str()), ("U", "U"));
    }

    #[test]
    fn ignored_records_are_skipped() {
        let status = status_of(&["! target/"]);
        assert!(status.entries.is_empty());
    }

    #[test]
    fn branches_are_split_into_local_and_remote() {
        let raw = "refs/heads/main\0aaa\0aaa1111\0origin/main\0*\n\
                   refs/heads/dev\0bbb\0bbb2222\0\0 \n\
                   refs/remotes/origin/HEAD\0aaa\0aaa1111\0\0 \n\
                   refs/remotes/origin/main\0aaa\0aaa1111\0\0 \n";
        let branches = parse_branches(raw);
        let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, vec!["dev", "main", "origin/main"]);
        let main = &branches[1];
        assert!(main.is_current);
        assert!(!main.is_remote);
        assert_eq!(main.upstream.as_deref(), Some("origin/main"));
        assert!(branches[2].is_remote);
        assert!(branches[0].upstream.is_none());
    }
}
