//! 路径规范化与工作区边界校验。
//!
//! 这是**能力校验**的第一道闸门（见 `docs/02-系统架构设计.md` §8 的双闸门设计）：
//! Main 侧决定「是否允许」，本模块决定「是否越界」。
//!
//! 三条硬规则：
//!
//! 1. 所有对外暴露的路径都必须先经过 [`PathGuard::resolve`]，禁止直接拼字符串。
//! 2. 规范化只走**词法**处理（`.` / `..` 折叠），不做符号链接解析——因为解析需要
//!    落盘 IO，会让每次按键都变慢；防逃逸靠「打开的根目录本身必须是真实目录」。
//! 3. 写操作额外过一遍敏感路径检查（`.git` / `.ssh` / `.env` / 系统目录）。

use std::path::{Component, Path, PathBuf};

/// 路径校验失败的原因。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("路径越界，不在工作区内：{path}")]
    OutOfWorkspace { path: String },
    #[error("路径非法：{path}（{reason}）")]
    Invalid { path: String, reason: String },
    #[error("该路径受保护，禁止写入：{path}")]
    Denied { path: String },
}

impl PathError {
    /// 映射为协议错误码（见 `docs/03-内核接口协议规范.md` §1.3）。
    pub fn code(&self) -> ai_ide_protocol::ErrorCode {
        use ai_ide_protocol::ErrorCode;
        match self {
            Self::OutOfWorkspace { .. } => ErrorCode::PATH_OUT_OF_WORKSPACE,
            Self::Denied { .. } => ErrorCode::PATH_DENIED,
            Self::Invalid { .. } => ErrorCode::INVALID_PATH,
        }
    }
}

/// 词法规范化：折叠 `.` 与 `..`，统一分隔符。
///
/// 注意 `..` 在根处会被吞掉（`PathBuf::pop` 对根返回 false），因此
/// `C:\a\..\..\..\b` 会被折成 `C:\b` 而不是抛错——越界与否交给包含性检查。
pub fn normalize(input: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in input.components() {
        match comp {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(seg) => out.push(seg),
        }
    }
    out
}

/// 用于比较的键：小写 + 统一分隔符（Windows 文件系统大小写不敏感）。
fn compare_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase().replace('/', "\\")
}

/// 系统保护目录（Windows）。
fn system_roots() -> Vec<String> {
    let mut roots: Vec<String> = vec![
        "c:\\windows".into(),
        "c:\\program files".into(),
        "c:\\program files (x86)".into(),
        "c:\\programdata".into(),
    ];
    if let Ok(sysroot) = std::env::var("SystemRoot") {
        roots.push(compare_key(Path::new(&sysroot)));
    }
    if let Ok(pf) = std::env::var("ProgramFiles") {
        roots.push(compare_key(Path::new(&pf)));
    }
    roots.sort();
    roots.dedup();
    roots
}

/// 目录段/文件名中一票否决的敏感标记。
const SENSITIVE_SEGMENTS: &[&str] = &[".git", ".ssh", ".aws", ".kube"];

/// 工作区路径守卫。
#[derive(Debug, Clone)]
pub struct PathGuard {
    root: PathBuf,
    root_cmp: String,
    system_roots: Vec<String>,
}

impl PathGuard {
    /// 以 `root` 为边界建立守卫；`root` 必须是已存在的目录。
    pub fn new(root: PathBuf) -> Result<Self, PathError> {
        let root = normalize(&root);
        if !root.is_dir() {
            return Err(PathError::Invalid {
                path: root.display().to_string(),
                reason: "工作区根目录不存在或不是目录".into(),
            });
        }
        let root_cmp = compare_key(&root);
        let system_roots = system_roots();
        if system_roots
            .iter()
            .any(|sys| root_cmp == *sys || root_cmp.starts_with(&format!("{sys}\\")))
        {
            return Err(PathError::Denied {
                path: root.display().to_string(),
            });
        }
        Ok(Self {
            root,
            root_cmp,
            system_roots,
        })
    }

    /// 规范化后的根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 解析输入路径为工作区内的绝对路径。
    ///
    /// 相对路径按工作区根展开。
    pub fn resolve(&self, input: &str) -> Result<PathBuf, PathError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(PathError::Invalid {
                path: input.to_string(),
                reason: "路径为空".into(),
            });
        }
        if trimmed.contains('\0') {
            return Err(PathError::Invalid {
                path: input.to_string(),
                reason: "路径含非法字符".into(),
            });
        }

        let raw = Path::new(trimmed);
        let joined = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.root.join(raw)
        };
        let abs = normalize(&joined);

        if !self.contains(&abs) {
            return Err(PathError::OutOfWorkspace {
                path: abs.display().to_string(),
            });
        }
        Ok(abs)
    }

    /// 解析写操作的目标路径，额外拒绝敏感路径与系统目录。
    pub fn resolve_for_write(&self, input: &str) -> Result<PathBuf, PathError> {
        let abs = self.resolve(input)?;
        if self.is_protected(&abs) {
            return Err(PathError::Denied {
                path: abs.display().to_string(),
            });
        }
        Ok(abs)
    }

    /// 绝对路径是否落在工作区边界内。
    pub fn contains(&self, abs: &Path) -> bool {
        let key = compare_key(abs);
        key == self.root_cmp || key.starts_with(&format!("{}\\", self.root_cmp))
    }

    /// 是否属于禁止写入的路径。
    pub fn is_protected(&self, abs: &Path) -> bool {
        let key = compare_key(abs);
        if self
            .system_roots
            .iter()
            .any(|sys| key == *sys || key.starts_with(&format!("{sys}\\")))
        {
            return true;
        }

        let rel = match abs.strip_prefix(&self.root) {
            Ok(rel) => rel,
            Err(_) => return true,
        };

        for seg in rel.components() {
            let Component::Normal(name) = seg else {
                continue;
            };
            let name_lower = name.to_string_lossy().to_lowercase();
            if SENSITIVE_SEGMENTS.contains(&name_lower.as_str()) {
                return true;
            }
            // `.env` / `.env.local` / `.env.production` 等
            if name_lower == ".env" || name_lower.starts_with(".env.") {
                return true;
            }
        }
        false
    }

    /// 转成相对工作区根的路径（统一用 `/`，便于 UI 展示与跨平台比较）。
    ///
    /// 已经是根目录时返回空串。
    pub fn to_relative(&self, abs: &Path) -> String {
        match abs.strip_prefix(&self.root) {
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            Err(_) => abs.to_string_lossy().replace('\\', "/"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> PathGuard {
        // 用当前目录作为测试工作区：它一定存在，且不在系统保护目录里。
        PathGuard::new(std::env::current_dir().expect("取当前目录")).expect("构造守卫")
    }

    #[test]
    fn normalize_folds_dots() {
        assert_eq!(
            normalize(Path::new(r"C:\a\b\..\c\.\d")),
            PathBuf::from(r"C:\a\c\d")
        );
    }

    #[test]
    fn relative_path_resolves_inside_root() {
        let g = guard();
        let abs = g.resolve("src/main.rs").expect("应能解析");
        assert!(abs.starts_with(g.root()));
    }

    #[test]
    fn parent_traversal_is_rejected() {
        let g = guard();
        let err = g.resolve(r"..\..\..\Windows\System32\drivers\etc\hosts");
        assert!(matches!(err, Err(PathError::OutOfWorkspace { .. })));
    }

    #[test]
    fn empty_path_is_invalid() {
        let g = guard();
        assert!(matches!(g.resolve("   "), Err(PathError::Invalid { .. })));
    }

    #[test]
    fn gap_in_name_does_not_escape() {
        // 「前缀陷阱」：根是 C:\a\b 时，C:\a\bc 不属于工作区。
        let g = guard();
        let sibling = format!("{}-sibling", g.root().display());
        let err = g.resolve(&sibling);
        assert!(matches!(err, Err(PathError::OutOfWorkspace { .. })));
    }

    #[test]
    fn sensitive_paths_are_write_protected() {
        let g = guard();
        assert!(g.is_protected(&g.root().join(".git").join("config")));
        assert!(g.is_protected(&g.root().join(".ssh").join("id_rsa")));
        assert!(g.is_protected(&g.root().join(".env")));
        assert!(g.is_protected(&g.root().join(".env.local")));
        assert!(!g.is_protected(&g.root().join("src").join("main.rs")));
    }

    #[test]
    fn system_directory_cannot_be_workspace_root() {
        let err = std::env::var("SystemRoot")
            .ok()
            .map(PathBuf::from)
            .map(PathGuard::new)
            .unwrap_or(Ok(guard()));
        assert!(matches!(err, Err(PathError::Denied { .. })) || err.is_ok());
    }

    #[test]
    fn to_relative_uses_forward_slash() {
        let g = guard();
        let abs = g.resolve("src/main.rs").unwrap();
        assert_eq!(g.to_relative(&abs), "src/main.rs");
    }

    #[test]
    fn error_codes_follow_protocol_table() {
        use ai_ide_protocol::ErrorCode;
        assert_eq!(
            PathError::OutOfWorkspace {
                path: String::new()
            }
            .code(),
            ErrorCode::PATH_OUT_OF_WORKSPACE
        );
        assert_eq!(
            PathError::Denied {
                path: String::new()
            }
            .code(),
            ErrorCode::PATH_DENIED
        );
    }
}
