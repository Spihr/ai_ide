//! git 输出的解析。
//!
//! 两处解析都不太平：`status --porcelain=v2 -z` 的每条记录以 `NUL` 结尾，重命名条目
//! 还会多带一个「原路径」记录；`for-each-ref` 的字段本身可能含空格（分支名不能含
//! 空格，但上游表达式可以），所以用一个不会出现在字段里的分隔符：`NUL`。
//!
//! 这里是纯函数，输入输出都是字节 / 字符串，因此可以拿固定样本做单测——不必为了
//! 测「重命名怎么解析」去构造一个真仓库。

use crate::model::{EntryKind, GitBranchInfo, GitStatus, GitStatusEntry};

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
