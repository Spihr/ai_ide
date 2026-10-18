//! 内容检索。
//!
//! 实现取的是「够用且可解释」的路线：`ignore` 负责走目录（自动吃掉 `.gitignore`），
//! `regex` 负责匹配，命中即收集，达到上限立刻停。
//!
//! 三个刻意的限制，避免一次搜索把机器拖垮：
//!
//! - 跳过超过 [`MAX_FILE_BYTES`] 的文件（日志、压缩包、数据文件）；
//! - 跳过头部含 `NUL` 的二进制文件；
//! - 只取每行**首次**命中（同一行出现十次没必要报十次）。

use ai_ide_protocol::{ContentMatch, SearchContentParams, SearchContentResult};
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::RegexBuilder;
use std::path::Path;

use crate::IndexError;

/// 单文件检索上限：4 MiB。
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// 二进制探测的头部字节数。
const BINARY_SNIFF_BYTES: usize = 8 * 1024;
/// 默认命中上限。
pub const DEFAULT_MAX_RESULTS: u32 = 500;
/// 预览行最大字符数。
const MAX_PREVIEW_CHARS: usize = 240;

/// 执行内容检索。
pub fn search(
    root: &Path,
    params: &SearchContentParams,
) -> Result<SearchContentResult, IndexError> {
    if params.query.is_empty() {
        return Err(IndexError::Other("查询串不能为空".into()));
    }

    // 字面量查询转义后交给正则引擎：复用同一套实现，少一条分支少一处 bug。
    let pattern = if params.is_regex {
        params.query.clone()
    } else {
        regex::escape(&params.query)
    };
    let regex = RegexBuilder::new(&pattern)
        .case_insensitive(!params.case_sensitive)
        .build()
        .map_err(|e| IndexError::BadRegex(e.to_string()))?;

    let includes = build_globset(&params.includes)?;
    let excludes = build_globset(&params.excludes)?;
    let max_results = params
        .max_results
        .unwrap_or(DEFAULT_MAX_RESULTS)
        .clamp(1, 10_000) as usize;

    let mut matches: Vec<ContentMatch> = Vec::new();
    let mut truncated = false;

    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .standard_filters(true)
        .require_git(false)
        .follow_links(false);
    builder.filter_entry(|entry| {
        if entry.depth() == 0 {
            return true;
        }
        match entry.file_name().to_str() {
            Some(name) => !ai_ide_workspace::ignore_rules::is_default_ignored_name(name),
            None => true,
        }
    });

    'files: for result in builder.build() {
        let entry = match result {
            Ok(entry) => entry,
            Err(err) => {
                tracing::debug!(error = %err, "内容检索跳过条目");
                continue;
            }
        };
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }

        let Ok(rel_path) = entry.path().strip_prefix(root) else {
            continue;
        };
        let rel = rel_path.to_string_lossy().replace('\\', "/");

        if let Some(set) = &includes {
            if !set.is_match(&rel) {
                continue;
            }
        }
        if let Some(set) = &excludes {
            if set.is_match(&rel) {
                continue;
            }
        }

        if entry
            .metadata()
            .map(|m| m.len() > MAX_FILE_BYTES)
            .unwrap_or(false)
        {
            continue;
        }

        let bytes = match std::fs::read(entry.path()) {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::debug!(path = %rel, error = %err, "读取失败，跳过");
                continue;
            }
        };
        if is_binary(&bytes) {
            continue;
        }

        // 内容按 UTF-8 尽力解码：GBK 文件里的中文可能匹配不到，这是 M1 的已知边界。
        let text = String::from_utf8_lossy(&bytes);

        for (idx, line) in text.lines().enumerate() {
            let Some(hit) = regex.find(line) else {
                continue;
            };
            matches.push(ContentMatch {
                path: rel.clone(),
                line: (idx + 1) as u32,
                column: char_column(line, hit.start()),
                preview: preview(line),
            });
            if matches.len() >= max_results {
                truncated = true;
                break 'files;
            }
        }
    }

    Ok(SearchContentResult { matches, truncated })
}

/// 按字节偏移算「第几个字符」，避免多字节字符让列号偏小。
fn char_column(line: &str, byte_index: usize) -> u32 {
    let end = byte_index.min(line.len());
    (line[..end].chars().count() + 1) as u32
}

/// 生成预览：去掉首尾空白，过长则截断。
fn preview(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.chars().count() <= MAX_PREVIEW_CHARS {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(MAX_PREVIEW_CHARS).collect();
    out.push('…');
    out
}

/// 头部含 `NUL` 即视为二进制。
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(BINARY_SNIFF_BYTES).any(|byte| *byte == 0)
}

/// 构建 glob 集合；空列表返回 `None`（表示不过滤）。
fn build_globset(patterns: &[String]) -> Result<Option<GlobSet>, IndexError> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let trimmed = pattern.trim();
        if trimmed.is_empty() {
            continue;
        }
        let glob = Glob::new(trimmed).map_err(|e| IndexError::BadGlob(e.to_string()))?;
        builder.add(glob);
    }
    let set = builder
        .build()
        .map_err(|e| IndexError::BadGlob(e.to_string()))?;
    Ok(Some(set))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_ide_protocol::SearchContentParams;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ai-ide-grep-{}-{}-{}",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn params(query: &str) -> SearchContentParams {
        SearchContentParams {
            workspace_id: "w".into(),
            query: query.into(),
            is_regex: false,
            case_sensitive: false,
            includes: vec![],
            excludes: vec![],
            max_results: None,
        }
    }

    #[test]
    fn finds_literal_matches_with_positions() {
        let root = temp_workspace("literal");
        fs::write(root.join("a.txt"), "alpha\nneedle here\nbeta\n").unwrap();
        fs::write(root.join("b.txt"), "nothing\n").unwrap();

        let result = search(&root, &params("needle")).unwrap();
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].path, "a.txt");
        assert_eq!(result.matches[0].line, 2);
        assert_eq!(result.matches[0].column, 1);
        assert!(!result.truncated);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn column_counts_chars_not_bytes() {
        let root = temp_workspace("column");
        fs::write(root.join("cn.txt"), "中文needle\n").unwrap();

        let result = search(&root, &params("needle")).unwrap();
        assert_eq!(result.matches[0].column, 3, "两个汉字后应是第 3 列");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn case_insensitive_by_default() {
        let root = temp_workspace("case");
        fs::write(root.join("a.txt"), "Hello World\n").unwrap();

        assert_eq!(search(&root, &params("hello")).unwrap().matches.len(), 1);

        let mut strict = params("hello");
        strict.case_sensitive = true;
        assert_eq!(search(&root, &strict).unwrap().matches.len(), 0);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn regex_mode_works_and_reports_errors() {
        let root = temp_workspace("regex");
        fs::write(root.join("a.txt"), "fn main() {}\nfn helper() {}\n").unwrap();

        let mut p = params(r"fn\s+(\w+)");
        p.is_regex = true;
        assert_eq!(search(&root, &p).unwrap().matches.len(), 2);

        p.query = "(".into();
        let err = search(&root, &p).unwrap_err();
        assert!(matches!(err, IndexError::BadRegex(_)));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn literal_queries_are_escaped() {
        let root = temp_workspace("escape");
        fs::write(root.join("a.txt"), "let x = a.b;\nlet y = axb;\n").unwrap();

        // `a.b` 作为字面量只应命中第一行
        let result = search(&root, &params("a.b")).unwrap();
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].line, 1);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn binary_files_are_skipped() {
        let root = temp_workspace("binary");
        fs::write(root.join("bin.dat"), [0u8, 1, 2, b'n', b'e', b'e', b'd']).unwrap();
        fs::write(root.join("text.txt"), "needle\n").unwrap();

        let result = search(&root, &params("needle")).unwrap();
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].path, "text.txt");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn includes_and_excludes_filter_paths() {
        let root = temp_workspace("glob");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("src").join("a.ts"), "needle\n").unwrap();
        fs::write(root.join("docs").join("a.md"), "needle\n").unwrap();

        let mut p = params("needle");
        p.includes = vec!["*.ts".into()];
        let result = search(&root, &p).unwrap();
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].path, "src/a.ts");

        p.includes.clear();
        p.excludes = vec!["docs/**".into()];
        let result = search(&root, &p).unwrap();
        assert!(result.matches.iter().all(|m| m.path != "docs/a.md"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn result_limit_truncates() {
        let root = temp_workspace("limit");
        let content: String = (0..10).map(|i| format!("needle {i}\n")).collect();
        fs::write(root.join("a.txt"), content).unwrap();

        let mut p = params("needle");
        p.max_results = Some(3);
        let result = search(&root, &p).unwrap();
        assert_eq!(result.matches.len(), 3);
        assert!(result.truncated);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn empty_query_is_rejected() {
        let root = temp_workspace("empty");
        let err = search(&root, &params("")).unwrap_err();
        assert!(matches!(err, IndexError::Other(_)));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn preview_trims_and_caps_length() {
        let long = format!("   {}   ", "x".repeat(MAX_PREVIEW_CHARS + 50));
        let out = preview(&long);
        assert!(!out.starts_with(' '));
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), MAX_PREVIEW_CHARS + 1);
    }

    #[test]
    fn gitignored_files_are_not_searched() {
        let root = temp_workspace("gitignore");
        fs::write(root.join(".gitignore"), "secret.txt\n").unwrap();
        fs::write(root.join("secret.txt"), "needle\n").unwrap();
        fs::write(root.join("open.txt"), "needle\n").unwrap();

        let result = search(&root, &params("needle")).unwrap();
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].path, "open.txt");

        fs::remove_dir_all(&root).ok();
    }
}
