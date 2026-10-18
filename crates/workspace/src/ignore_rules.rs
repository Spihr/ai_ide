//! 忽略规则：内置默认集合 + 逐层 `.gitignore`。
//!
//! 树与检索都要用同一套判断，所以独立成模块，避免两处实现漂移。
//!
//! 规则来源按优先级从高到低：
//! 1. 内置默认集合（`.git` / `node_modules` / `target` …）——始终生效；
//! 2. 从工作区根到目标目录的每一层 `.gitignore`——内层压外层；
//! 3. 白名单（`!pattern`）可以推翻外层的忽略。

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::path::{Component, Path, PathBuf};

/// 内置默认忽略的目录名（大小写不敏感）。
///
/// 刻意不含 `bin` / `obj` 之类的通用名，避免误伤源码目录；
/// 用户可在自己的 `.gitignore` 里补充。
const DEFAULT_IGNORED: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "dist",
    "__pycache__",
    ".venv",
    "venv",
    ".next",
    ".turbo",
    ".cache",
    ".gradle",
];

/// 名称是否命中内置默认忽略集合。
pub fn is_default_ignored_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    DEFAULT_IGNORED.iter().any(|d| *d == lower)
}

/// 路径中任意一段是否命中内置默认忽略集合。
///
/// ⚠️ 只对**相对工作区根**的路径使用。绝对路径会连带扫到工作区根之上的段，
/// 于是「工作区恰好位于 `D:\dist\myapp`」会导致整个工作区被忽略。
pub fn is_default_ignored_path(path: &Path) -> bool {
    path.components().any(|comp| match comp {
        Component::Normal(name) => is_default_ignored_name(&name.to_string_lossy()),
        _ => false,
    })
}

/// 从根到某一层的 `.gitignore` 匹配器集合。
#[derive(Debug)]
pub struct IgnoreStack {
    /// 工作区根，用于把待判定路径折成相对路径再比对默认集合。
    root: PathBuf,
    /// 由外到内排列。
    layers: Vec<Gitignore>,
}

impl Default for IgnoreStack {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            layers: Vec::new(),
        }
    }
}

impl IgnoreStack {
    /// 构建根目录到 `dir`（含）的匹配器栈。
    ///
    /// `dir` 不在 `root` 之内时退化为只看 `root`。
    pub fn build(root: &Path, dir: &Path) -> Self {
        let mut layers = Vec::new();
        for base in chain(root, dir) {
            let candidate = base.join(".gitignore");
            if !candidate.is_file() {
                continue;
            }
            let mut builder = GitignoreBuilder::new(&base);
            // add 的返回值是「有错」而非「有匹配」，解析失败只记日志，不阻断。
            if let Some(err) = builder.add(&candidate) {
                tracing::debug!(path = %candidate.display(), error = %err, "跳过非法 .gitignore");
                continue;
            }
            match builder.build() {
                Ok(gi) => layers.push(gi),
                Err(err) => {
                    tracing::debug!(path = %candidate.display(), error = %err, "构建 .gitignore 失败");
                }
            }
        }
        Self {
            root: root.to_path_buf(),
            layers,
        }
    }

    /// 该路径是否应被忽略。
    ///
    /// `path` 可以是绝对路径（工作区内）或相对路径，两者都支持。
    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        // 默认集合按「相对工作区根」的段判定，避免工作区自身的目录名造成误伤。
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        if is_default_ignored_path(rel) {
            return true;
        }

        // 内层优先：从最里层的 .gitignore 往外看，先命中者胜。
        for layer in self.layers.iter().rev() {
            match layer.matched(path, is_dir) {
                ignore::Match::Ignore(_) => return true,
                ignore::Match::Whitelist(_) => return false,
                ignore::Match::None => {}
            }
        }
        false
    }

    /// 已加载的 `.gitignore` 层数，用于诊断。
    pub fn depth(&self) -> usize {
        self.layers.len()
    }
}

/// 从 `root` 到 `dir` 的目录链（含两端）。
fn chain(root: &Path, dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let rel = dir.strip_prefix(root).unwrap_or(Path::new(""));
    let mut acc = root.to_path_buf();
    for comp in rel.components() {
        acc.push(comp);
        out.push(acc.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_set_covers_heavy_dirs() {
        assert!(is_default_ignored_name(".git"));
        assert!(is_default_ignored_name("NODE_MODULES"));
        assert!(is_default_ignored_name("target"));
        assert!(!is_default_ignored_name("src"));
        assert!(!is_default_ignored_name("crates"));
    }

    #[test]
    fn path_check_scans_all_segments() {
        assert!(is_default_ignored_path(Path::new(
            r"C:\proj\node_modules\react\index.js"
        )));
        assert!(!is_default_ignored_path(Path::new(r"C:\proj\src\main.rs")));
    }

    #[test]
    fn chain_includes_root_and_every_level() {
        let root = Path::new(r"C:\proj");
        let dir = Path::new(r"C:\proj\a\b");
        let dirs = chain(root, dir);
        assert_eq!(dirs.len(), 3);
        assert_eq!(dirs[0], PathBuf::from(r"C:\proj"));
        assert_eq!(dirs[2], PathBuf::from(r"C:\proj\a\b"));
    }

    #[test]
    fn empty_stack_ignores_nothing_but_defaults() {
        let stack = IgnoreStack::default();
        assert_eq!(stack.depth(), 0);
        assert!(!stack.is_ignored(Path::new(r"C:\proj\src\main.rs"), false));
        assert!(stack.is_ignored(Path::new(r"C:\proj\target\debug"), true));
    }

    #[test]
    fn middle_segment_of_absolute_path_counts() {
        let stack = IgnoreStack::build(Path::new(r"C:\proj"), Path::new(r"C:\proj"));
        assert!(stack.is_ignored(Path::new(r"C:\proj\target\debug\app.pdb"), false));
        assert!(stack.is_ignored(Path::new(r"C:\proj\node_modules\react\index.js"), false));
    }

    #[test]
    fn workspace_root_own_name_does_not_poison_everything() {
        // 工作区根正好叫 dist：其内部文件不应被默认集合一网打尽。
        let root = Path::new(r"C:\proj\dist");
        let stack = IgnoreStack::build(root, root);
        assert!(!stack.is_ignored(Path::new(r"C:\proj\dist\app.js"), false));
        assert!(stack.is_ignored(Path::new(r"C:\proj\dist\target\app.js"), false));
    }
}
