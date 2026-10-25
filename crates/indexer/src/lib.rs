//! # ai-ide-indexer
//!
//! AI IDE 的**检索内核**：文件路径索引 + 内容检索。
//!
//! ## M1 的取舍：内存索引，不落盘
//!
//! `docs/02-系统架构设计.md` §4.1 给了「内存 + SQLite FTS5」的演进方向。
//! M1 只做**内存文件路径索引**，理由：
//!
//! - 文件名检索的数据量是「路径字符串」，10 万条约 10 MB，内存完全装得下；
//! - 落盘索引要处理失效、版本、迁移，是 M3 该付的成本，不该提前付；
//! - 内容检索先用「遍历 + 正则」直查，靠 `ignore` 的并行遍历控制开销。
//!
//! 符号索引（`search.symbols`）在 M3 切片一落地：只做「行首声明 + 作用域跟踪」的
//! 轻量抽取（见 [`symbols`]），不引语法树——理由写在那个模块的文档里。

pub mod fuzzy;
pub mod grep;
pub mod symbols;

use ai_ide_protocol::{FileMatch, SearchFilesParams, SearchFilesResult, WorkspaceStats};
use globset::Glob;
use ignore::WalkBuilder;
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;

use ai_ide_workspace::ignore_rules::is_default_ignored_name;

/// 检索层的错误。
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("IO 错误（{path}）：{source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("正则表达式非法：{0}")]
    BadRegex(String),

    #[error("glob 模式非法：{0}")]
    BadGlob(String),

    #[error("{0}")]
    Other(String),
}

impl IndexError {
    /// 映射为协议错误码。
    pub fn code(&self) -> ai_ide_protocol::ErrorCode {
        use ai_ide_protocol::ErrorCode;
        match self {
            Self::Io { .. } => ErrorCode::IO_ERROR,
            Self::BadRegex(_) | Self::BadGlob(_) => ErrorCode::INVALID_PARAMS,
            Self::Other(_) => ErrorCode::INTERNAL_ERROR,
        }
    }
}

/// 索引中的一个文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// 相对工作区根，`/` 分隔。
    pub rel: String,
    /// 文件名（含扩展名）。
    pub name: String,
}

/// 文件路径索引。
#[derive(Debug)]
pub struct FileIndex {
    root: PathBuf,
    entries: Vec<IndexEntry>,
    stats: WorkspaceStats,
}

impl FileIndex {
    /// 全量遍历并建立索引。
    ///
    /// 遍历遵守 `.gitignore`（`standard_filters`），并额外剔除内置默认忽略目录。
    /// `stats.ignored` 统计的是**内置默认集合**拦下的条目数——`.gitignore` 的
    /// 排除发生在 `ignore` 库内部，不经过我们的过滤器，无法在此计数。
    pub fn build(root: &Path) -> Result<Self, IndexError> {
        let ignored = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&ignored);

        let mut builder = WalkBuilder::new(root);
        builder
            .hidden(false) // 隐藏文件要索引：`.github/`、`.eslintrc.js` 都是正经文件
            .standard_filters(true)
            // 非 git 目录下的 `.gitignore` 同样生效：与工作区自身的忽略栈口径一致，
            // 也让「打开一个随手新建的文件夹」时忽略规则不至于突然失灵。
            .require_git(false)
            .follow_links(false);
        builder.filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            match entry.file_name().to_str() {
                Some(name) if is_default_ignored_name(name) => {
                    counter.fetch_add(1, AtomicOrdering::Relaxed);
                    false
                }
                _ => true,
            }
        });

        let mut entries: Vec<IndexEntry> = Vec::new();
        let mut files: u64 = 0;
        let mut dirs: u64 = 0;

        for result in builder.build() {
            let entry = match result {
                Ok(entry) => entry,
                Err(err) => {
                    tracing::debug!(error = %err, "遍历中跳过条目");
                    continue;
                }
            };
            if entry.depth() == 0 {
                continue;
            }
            let file_type = match entry.file_type() {
                Some(ft) => ft,
                None => continue,
            };
            if file_type.is_dir() {
                dirs += 1;
                continue;
            }
            if !file_type.is_file() {
                continue;
            }

            let Ok(rel) = entry.path().strip_prefix(root) else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            entries.push(IndexEntry {
                rel: rel.to_string_lossy().replace('\\', "/"),
                name,
            });
            files += 1;
        }

        // 稳定排序：让相同查询在多次调用间返回一致顺序，便于测试与 UI 去抖。
        entries.sort_by(|a, b| a.rel.cmp(&b.rel));

        Ok(Self {
            root: root.to_path_buf(),
            entries,
            stats: WorkspaceStats {
                files,
                dirs,
                ignored: ignored.load(AtomicOrdering::Relaxed),
            },
        })
    }

    /// 索引根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 条目数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空索引。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 规模统计。
    pub fn stats(&self) -> WorkspaceStats {
        self.stats
    }

    /// 全部条目（调试与测试用）。
    pub fn entries(&self) -> &[IndexEntry] {
        &self.entries
    }

    /// 文件名模糊检索。
    ///
    /// `glob` 同时匹配相对路径与文件名（`*.ts` 既能命中 `a.ts` 也能命中 `src/a.ts`），
    /// 因为「`*.ts` 应该搜到所有 TS 文件」是绝大多数人的预期。
    pub fn search_files(&self, query: &str, glob: Option<&str>, limit: usize) -> Vec<FileMatch> {
        let trimmed = query.trim();
        let matcher = match glob.map(str::trim).filter(|g| !g.is_empty()) {
            Some(pattern) => match Glob::new(pattern) {
                Ok(glob) => Some(glob.compile_matcher()),
                Err(err) => {
                    tracing::debug!(pattern, error = %err, "glob 模式非法，忽略过滤条件");
                    None
                }
            },
            None => None,
        };

        let mut scored: Vec<FileMatch> = Vec::with_capacity(64);
        for entry in &self.entries {
            if let Some(matcher) = &matcher {
                if !matcher.is_match(&entry.rel) && !matcher.is_match(&entry.name) {
                    continue;
                }
            }

            let name_score = fuzzy::subsequence_score(&entry.name, trimmed);
            let path_score = fuzzy::subsequence_score(&entry.rel, trimmed);
            let score = match (name_score, path_score) {
                (Some(name), Some(path)) => {
                    (name * fuzzy::NAME_WEIGHT).max(path * fuzzy::PATH_WEIGHT)
                }
                (Some(name), None) => name * fuzzy::NAME_WEIGHT,
                (None, Some(path)) => path * fuzzy::PATH_WEIGHT,
                (None, None) => continue,
            };

            scored.push(FileMatch {
                path: entry.rel.clone(),
                score,
            });
        }

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
                // 同分时短路径优先，最后用字典序兜底保证全序稳定。
                .then_with(|| a.path.len().cmp(&b.path.len()))
                .then_with(|| a.path.cmp(&b.path))
        });
        scored.truncate(limit);
        scored
    }

    /// 按协议参数检索。
    pub fn search(&self, params: &SearchFilesParams) -> SearchFilesResult {
        const DEFAULT_LIMIT: usize = 200;
        let limit = params
            .limit
            .map(|l| l as usize)
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, 2000);
        SearchFilesResult {
            matches: self.search_files(&params.query, params.glob.as_deref(), limit),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ai-ide-idx-{}-{}-{}",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, AtomicOrdering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn fixture() -> PathBuf {
        let root = temp_workspace("fixture");
        fs::create_dir_all(root.join("src").join("features")).unwrap();
        fs::write(root.join("src").join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("src").join("App.tsx"), "export default 1\n").unwrap();
        fs::write(root.join("README.md"), "# hi\n").unwrap();
        fs::write(
            root.join("src").join("features").join("Editor.tsx"),
            "editor\n",
        )
        .unwrap();
        root
    }

    #[test]
    fn builds_index_with_relative_paths() {
        let root = fixture();
        let index = FileIndex::build(&root).unwrap();

        assert_eq!(index.len(), 4);
        assert!(index.entries().iter().any(|e| e.rel == "src/main.rs"));
        assert!(index.entries().iter().all(|e| !e.rel.contains('\\')));
        assert_eq!(index.stats().files, 4);
        assert!(index.stats().dirs >= 2);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn default_ignored_dirs_are_excluded_and_counted() {
        let root = fixture();
        fs::create_dir_all(root.join("node_modules").join("react")).unwrap();
        fs::write(
            root.join("node_modules").join("react").join("index.js"),
            "x",
        )
        .unwrap();
        fs::write(root.join("target"), "not a dir").ok();

        let index = FileIndex::build(&root).unwrap();
        assert!(index
            .entries()
            .iter()
            .all(|e| !e.rel.contains("node_modules")));
        assert!(index.stats().ignored >= 1, "应统计被默认集合拦下的条目");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn gitignore_is_respected() {
        let root = fixture();
        fs::write(root.join(".gitignore"), "README.md\n").unwrap();

        let index = FileIndex::build(&root).unwrap();
        assert!(
            index.entries().iter().all(|e| e.rel != "README.md"),
            ".gitignore 中的条目不应进入索引"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn search_prefers_filename_hits() {
        let root = fixture();
        let index = FileIndex::build(&root).unwrap();

        let hits = index.search_files("editor", None, 10);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].path, "src/features/Editor.tsx");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn acronym_query_finds_camel_case_file() {
        let root = fixture();
        let index = FileIndex::build(&root).unwrap();

        let hits = index.search_files("App", None, 10);
        assert_eq!(hits[0].path, "src/App.tsx");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn glob_filter_matches_bare_name_and_path() {
        let root = fixture();
        let index = FileIndex::build(&root).unwrap();

        let tsx = index.search_files("", Some("*.tsx"), 10);
        assert_eq!(tsx.len(), 2, "`*.tsx` 应命中深层的 tsx 文件");

        let scoped = index.search_files("", Some("src/**"), 10);
        assert_eq!(scoped.len(), 3);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn limit_is_respected() {
        let root = fixture();
        let index = FileIndex::build(&root).unwrap();
        assert_eq!(index.search_files("", None, 2).len(), 2);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn results_are_deterministic() {
        let root = fixture();
        let index = FileIndex::build(&root).unwrap();
        let first: Vec<String> = index
            .search_files("t", None, 10)
            .into_iter()
            .map(|m| m.path)
            .collect();
        let second: Vec<String> = index
            .search_files("t", None, 10)
            .into_iter()
            .map(|m| m.path)
            .collect();
        assert_eq!(first, second);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn protocol_search_clamps_limit() {
        let root = fixture();
        let index = FileIndex::build(&root).unwrap();
        let result = index.search(&SearchFilesParams {
            workspace_id: "w".into(),
            query: "".into(),
            glob: None,
            limit: Some(0),
        });
        assert_eq!(result.matches.len(), 1, "limit=0 会被夹到 1");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn index_error_codes_are_mapped() {
        use ai_ide_protocol::ErrorCode;
        assert_eq!(
            IndexError::BadRegex("x".into()).code(),
            ErrorCode::INVALID_PARAMS
        );
        assert_eq!(
            IndexError::BadGlob("x".into()).code(),
            ErrorCode::INVALID_PARAMS
        );
    }
}
