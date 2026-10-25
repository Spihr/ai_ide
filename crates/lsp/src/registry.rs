//! 语言服务器注册表：文件扩展名 → 语言 → 默认启动命令。
//!
//! ## 为什么单独一层
//!
//! 上层（`crates/core` 的 `lsp.*` 能力）拿到的是「用户打开了 `a.tsx`」，它不该
//! 知道 `typescript-language-server` 怎么拼、参数是 `--stdio` 还是 `--node-ipc`，
//! 更不该在命令没装时 panic 到 UI 线程。这里把三件事收在一处：
//!
//! 1. **映射**：扩展名 → 语言标识（`ts` / `tsx` / `js` / `jsx` 都归 `typescript`
//!    这一套服务器，`py` 归 `python`）；
//! 2. **默认命令**：每种语言的首选命令与备选命令（例如 python 先试
//!    `pyright-langserver`，找不到再退 `pylsp`）；
//! 3. **可安装性检查**：命令不在 PATH 里时返回 [`LspError::NotInstalled`]，
//!    带上命令名和一句安装提示——这条错误是直接给用户看的。
//!
//! ## 语言标识的两级粒度（重要）
//!
//! 注册表里的 `language_id` 是**服务器分组**（`typescript`），而 `didOpen` 里的
//! `languageId` 由 [`crate::document::language_id_for_path`] 按扩展名给细粒度值
//! （`.tsx` → `typescriptreact`）。这两者不等是有意的：一个
//! `typescript-language-server` 同时服务 ts/tsx/js/jsx，但服务器需要知道具体是
//! 哪一种才能按对应语法解析。覆盖时用 [`ServerOverrides::language_id`] 改分组，
//! 不碰 `didOpen` 的细粒度标识。
//!
//! ## 覆盖
//!
//! [`ServerOverrides`] 允许调用方改 `command` / `args` / `root`：真实产品里用户
//! 会在设置里写「用我自己那份 `typescript-language-server`」，或者给 python 指定
//! `basedpyright`。覆盖了 `command` 就不再尝试备选命令（用户明确指定了要哪个）。

use std::path::{Path, PathBuf};

use crate::config::ServerConfig;
use crate::error::LspError;

/// 一条启动命令（命令名 + 参数）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// 可执行文件名或绝对路径。
    pub command: String,
    /// 命令行参数（例如 `--stdio`）。
    pub args: Vec<String>,
}

impl CommandSpec {
    /// 构造一条命令。
    pub fn new(command: impl Into<String>, args: &[&str]) -> Self {
        Self {
            command: command.into(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
        }
    }

    /// 命令是否可用于启动（见 [`command_available`]）。
    pub fn is_available(&self) -> bool {
        command_available(&self.command)
    }
}

/// 一种语言对应的服务器配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageServerSpec {
    /// 服务器分组语言标识，例如 `typescript`。
    pub language_id: String,
    /// 归属这套服务器的扩展名（不带点，小写）。
    pub extensions: Vec<String>,
    /// 首选命令。
    pub primary: CommandSpec,
    /// 备选命令，按顺序尝试（首选不在 PATH 里时才会用到）。
    pub fallbacks: Vec<CommandSpec>,
    /// 一条都起不来时给用户的安装提示。
    pub install_hint: String,
}

impl LanguageServerSpec {
    /// 用语言标识、扩展名、首选命令构造。
    pub fn new(language_id: impl Into<String>, extensions: &[&str], primary: CommandSpec) -> Self {
        Self {
            language_id: language_id.into(),
            extensions: extensions
                .iter()
                .map(|ext| ext.trim_start_matches('.').to_ascii_lowercase())
                .collect(),
            primary,
            fallbacks: Vec::new(),
            install_hint: String::new(),
        }
    }

    /// 追加一条备选命令。
    pub fn with_fallback(mut self, fallback: CommandSpec) -> Self {
        self.fallbacks.push(fallback);
        self
    }

    /// 设置安装提示（说明该装什么）。
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.install_hint = hint.into();
        self
    }

    /// 扩展名（小写、不带点）是否归这套服务器。
    pub fn covers_extension(&self, extension: &str) -> bool {
        let extension = extension.trim_start_matches('.').to_ascii_lowercase();
        self.extensions.contains(&extension)
    }
}

/// 调用方对默认配置的覆盖。
///
/// `args` / `root` 在没覆盖 `command` 时也生效：改启动参数、钉死工作区根目录都是
/// 常见诉求。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerOverrides {
    /// 覆盖语言分组（跳过扩展名推断）。
    pub language_id: Option<String>,
    /// 覆盖可执行文件。
    pub command: Option<String>,
    /// 覆盖命令行参数。
    pub args: Option<Vec<String>>,
    /// 覆盖工作区根目录（同时作为 `initialize` 的 `rootUri`）。
    pub root: Option<PathBuf>,
}

impl ServerOverrides {
    /// 空覆盖：完全走注册表默认值。
    pub fn new() -> Self {
        Self::default()
    }

    /// 覆盖可执行文件。
    pub fn command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }

    /// 覆盖命令行参数。
    pub fn args(mut self, args: impl IntoIterator<Item = String>) -> Self {
        self.args = Some(args.into_iter().collect());
        self
    }

    /// 覆盖工作区根目录。
    pub fn root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = Some(root.into());
        self
    }

    /// 覆盖语言分组。
    pub fn language_id(mut self, language_id: impl Into<String>) -> Self {
        self.language_id = Some(language_id.into());
        self
    }
}

/// 注册表解析出的启动方案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedServer {
    /// 可以直接交给 [`crate::LspClient::start`] 的配置。
    pub config: ServerConfig,
    /// 生效的语言分组标识。
    pub language_id: String,
    /// 实际用到的命令（可能来自备选）。
    pub command: CommandSpec,
    /// 是否走了备选命令（`true` 说明首选没装）。
    pub used_fallback: bool,
}

/// 语言服务器注册表。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LanguageRegistry {
    specs: Vec<LanguageServerSpec>,
}

impl LanguageRegistry {
    /// 空表（测试里用来只放关心的那一条）。
    pub fn new(specs: Vec<LanguageServerSpec>) -> Self {
        Self { specs }
    }

    /// 内置默认表。
    ///
    /// 只收「有一句话就能装好、且 `--stdio` 就能起」的服务器；C/C++、Java 这类
    /// 需要额外工程文件的先不放，避免给用户一个必然起不来的默认值。
    pub fn builtin() -> Self {
        Self::new(vec![
            LanguageServerSpec::new(
                "typescript",
                &["ts", "tsx", "js", "jsx", "mts", "cts", "mjs", "cjs"],
                CommandSpec::new("typescript-language-server", &["--stdio"]),
            )
            .with_fallback(CommandSpec::new("vtsls", &["--stdio"]))
            .with_hint("请先安装：npm i -g typescript typescript-language-server"),
            LanguageServerSpec::new(
                "python",
                &["py", "pyi"],
                CommandSpec::new("pyright-langserver", &["--stdio"]),
            )
            .with_fallback(CommandSpec::new("pylsp", &[]))
            .with_hint(
                "请先安装 pyright（npm i -g pyright）或 pylsp（pip install python-lsp-server）",
            ),
            LanguageServerSpec::new("rust", &["rs"], CommandSpec::new("rust-analyzer", &[]))
                .with_hint("请先安装 rust-analyzer（rustup component add rust-analyzer）"),
            LanguageServerSpec::new("go", &["go"], CommandSpec::new("gopls", &[]))
                .with_hint("请先安装 gopls（go install golang.org/x/tools/gopls@latest）"),
        ])
    }

    /// 全部条目。
    pub fn specs(&self) -> &[LanguageServerSpec] {
        &self.specs
    }

    /// 追加一条（后加的优先：先匹配到就返回）。
    pub fn push(&mut self, spec: LanguageServerSpec) {
        self.specs.push(spec);
    }

    /// 按语言分组查条目。
    pub fn spec_for_language(&self, language_id: &str) -> Option<&LanguageServerSpec> {
        self.specs
            .iter()
            .find(|spec| spec.language_id == language_id)
    }

    /// 按路径（扩展名）查条目。
    pub fn spec_for_path(&self, path: &Path) -> Option<&LanguageServerSpec> {
        let extension = extension_of(path)?;
        self.specs
            .iter()
            .find(|spec| spec.covers_extension(&extension))
    }

    /// 扩展名（不带点，大小写不敏感）→ 语言分组。
    pub fn language_id_for_extension(&self, extension: &str) -> Option<&str> {
        let extension = extension.trim_start_matches('.');
        self.specs
            .iter()
            .find(|spec| spec.covers_extension(extension))
            .map(|spec| spec.language_id.as_str())
    }

    /// 路径 → 语言分组。
    pub fn language_id_for_path(&self, path: &Path) -> Option<&str> {
        self.spec_for_path(path)
            .map(|spec| spec.language_id.as_str())
    }

    /// 为某个文件解析启动方案（默认 root 取文件所在目录，若该目录存在）。
    pub fn resolve_for_path(
        &self,
        path: &Path,
        overrides: &ServerOverrides,
    ) -> Result<ResolvedServer, LspError> {
        let language_id = match &overrides.language_id {
            Some(language_id) => language_id.clone(),
            None => self
                .language_id_for_path(path)
                .map(str::to_string)
                .ok_or_else(|| LspError::UnsupportedLanguage {
                    path: path.display().to_string(),
                })?,
        };

        let mut effective = overrides.clone();
        if effective.root.is_none() {
            // 只有当目录真的存在时才当 cwd：不存在的 cwd 会让 spawn 直接失败，
            // 把「文件还没落盘 / 路径写错」伪装成「服务器没装」。
            effective.root = path
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty() && dir.is_dir())
                .map(Path::to_path_buf);
        }
        self.resolve_for_language(&language_id, &effective)
    }

    /// 为某个语言分组解析启动方案。
    pub fn resolve_for_language(
        &self,
        language_id: &str,
        overrides: &ServerOverrides,
    ) -> Result<ResolvedServer, LspError> {
        let spec = self.spec_for_language(language_id);

        let primary = match (&overrides.command, spec) {
            (Some(command), spec) => CommandSpec {
                command: command.clone(),
                args: match &overrides.args {
                    Some(args) => args.clone(),
                    None => spec
                        .map(|spec| spec.primary.args.clone())
                        .unwrap_or_default(),
                },
            },
            (None, Some(spec)) => CommandSpec {
                command: spec.primary.command.clone(),
                args: overrides
                    .args
                    .clone()
                    .unwrap_or_else(|| spec.primary.args.clone()),
            },
            (None, None) => {
                return Err(LspError::UnsupportedLanguage {
                    path: language_id.to_string(),
                })
            }
        };

        if primary.is_available() {
            return Ok(self.finish(primary, language_id, overrides, false));
        }

        // 调用方明确指定了命令，就别自作主张换一个：他要的是那个。
        if overrides.command.is_none() {
            if let Some(spec) = spec {
                for fallback in &spec.fallbacks {
                    if fallback.is_available() {
                        return Ok(self.finish(fallback.clone(), language_id, overrides, true));
                    }
                }
            }
        }

        Err(LspError::NotInstalled {
            command: primary.command,
            language: language_id.to_string(),
            hint: spec
                .map(|spec| spec.install_hint.clone())
                .filter(|hint| !hint.is_empty())
                .unwrap_or_else(|| {
                    format!("请先安装 `{language_id}` 对应的语言服务器并确保它在 PATH 中")
                }),
        })
    }

    fn finish(
        &self,
        command: CommandSpec,
        language_id: &str,
        overrides: &ServerOverrides,
        used_fallback: bool,
    ) -> ResolvedServer {
        let mut config = ServerConfig::new(language_id, command.command.clone());
        config.args = command.args.clone();
        config.cwd = overrides.root.clone();
        ResolvedServer {
            config,
            language_id: language_id.to_string(),
            command,
            used_fallback,
        }
    }
}

fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
}

/// 命令是否可以启动。
///
/// 带路径分隔符的按「文件是否存在」判断（用户可能写绝对路径），否则在 `PATH` 里找；
/// Windows 还要按 `PATHEXT` 补后缀，否则 `typescript-language-server.cmd`
/// （npm 全局装的都是 `.cmd` 垫片）会被判成「没装」——这是最典型的误报。
pub fn command_available(command: &str) -> bool {
    if command.trim().is_empty() {
        return false;
    }
    let looks_like_path = command.contains('/') || command.contains('\\');
    if looks_like_path {
        return Path::new(command).is_file();
    }

    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let extensions = executable_extensions();
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(command);
        if candidate.is_file() {
            return true;
        }
        for extension in &extensions {
            if candidate.with_extension(extension).is_file() {
                return true;
            }
        }
    }
    false
}

#[cfg(windows)]
fn executable_extensions() -> Vec<String> {
    let raw = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    // `with_extension` 要求不带点。
    raw.split(';')
        .map(|ext| ext.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|ext| !ext.is_empty())
        .collect()
}

#[cfg(not(windows))]
fn executable_extensions() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_maps_extensions_to_language_groups() {
        let registry = LanguageRegistry::builtin();
        let cases = [
            ("a.ts", "typescript"),
            ("a.tsx", "typescript"),
            ("a.js", "typescript"),
            ("a.jsx", "typescript"),
            ("a.py", "python"),
            ("a.rs", "rust"),
            ("a.go", "go"),
        ];
        for (name, expected) in cases {
            assert_eq!(
                registry.language_id_for_path(Path::new(name)),
                Some(expected),
                "{name}"
            );
        }
        // 大小写与点号前缀都要能吃。
        assert_eq!(
            registry.language_id_for_extension(".TSX"),
            Some("typescript")
        );
        assert_eq!(registry.language_id_for_extension("unknownext"), None);
        assert_eq!(registry.language_id_for_path(Path::new("noext")), None);
    }

    #[test]
    fn builtin_declares_expected_default_commands() {
        let registry = LanguageRegistry::builtin();

        let typescript = registry
            .spec_for_language("typescript")
            .expect("必须有 typescript");
        assert_eq!(typescript.primary.command, "typescript-language-server");
        assert_eq!(typescript.primary.args, vec!["--stdio".to_string()]);

        let python = registry.spec_for_language("python").expect("必须有 python");
        assert_eq!(python.primary.command, "pyright-langserver");
        assert_eq!(python.primary.args, vec!["--stdio".to_string()]);
        assert!(
            python
                .fallbacks
                .iter()
                .any(|fallback| fallback.command == "pylsp"),
            "{:?}",
            python.fallbacks
        );
    }

    #[test]
    fn every_builtin_spec_carries_an_install_hint() {
        for spec in LanguageRegistry::builtin().specs() {
            assert!(
                !spec.install_hint.is_empty(),
                "{} 缺安装提示",
                spec.language_id
            );
        }
    }

    #[test]
    fn missing_command_reports_not_installed_instead_of_panicking() {
        let registry = LanguageRegistry::builtin();
        let overrides = ServerOverrides::new().command("ai-ide-missing-lsp-binary-xyz");
        let error = registry
            .resolve_for_path(Path::new("E:/tmp/a.ts"), &overrides)
            .expect_err("命令不存在必须报错");
        match &error {
            LspError::NotInstalled {
                command,
                language,
                hint,
            } => {
                assert_eq!(command, "ai-ide-missing-lsp-binary-xyz");
                assert_eq!(language, "typescript");
                assert!(hint.contains("typescript-language-server"), "{hint}");
            }
            other => panic!("期望 NotInstalled，实际 {other:?}"),
        }
        assert_eq!(error.class(), crate::error::ErrorClass::Spawn);
    }

    #[test]
    fn unsupported_extension_is_a_state_error_not_a_spawn_error() {
        let registry = LanguageRegistry::builtin();
        let error = registry
            .resolve_for_path(Path::new("E:/tmp/a.unknownext"), &ServerOverrides::new())
            .expect_err("未登记扩展名必须报错");
        assert!(
            matches!(error, LspError::UnsupportedLanguage { .. }),
            "{error}"
        );
        assert_eq!(error.class(), crate::error::ErrorClass::State);
        // 但显式指定语言就能救回来（调用方比扩展名更懂）。
        let resolved = registry
            .resolve_for_language(
                "go",
                &ServerOverrides::new()
                    .command(std::env::current_exe().expect("exe").display().to_string()),
            )
            .expect("显式指定语言应可解析");
        assert_eq!(resolved.language_id, "go");
    }

    #[test]
    fn overrides_replace_command_args_and_root() {
        let registry = LanguageRegistry::builtin();
        let root = std::env::current_dir().expect("cwd");
        let overrides = ServerOverrides::new()
            .command(std::env::current_exe().expect("exe").display().to_string())
            .args(["--experimental".to_string()])
            .root(root.clone());
        let resolved = registry
            .resolve_for_path(Path::new("E:/tmp/a.ts"), &overrides)
            .expect("覆盖后应可解析");
        assert_eq!(resolved.config.name, "typescript");
        assert_eq!(resolved.config.args, vec!["--experimental".to_string()]);
        assert_eq!(resolved.config.cwd, Some(root));
        assert!(!resolved.used_fallback);
    }

    #[test]
    fn fallback_is_used_only_when_primary_is_missing() {
        let exe = std::env::current_exe().expect("exe").display().to_string();
        let registry = LanguageRegistry::new(vec![LanguageServerSpec::new(
            "typescript",
            &["ts"],
            CommandSpec::new("ai-ide-missing-ts-server", &["--stdio"]),
        )
        .with_fallback(CommandSpec::new(exe.clone(), &[]))
        .with_hint("装 ts 服务器")]);

        let resolved = registry
            .resolve_for_path(Path::new("E:/tmp/a.ts"), &ServerOverrides::new())
            .expect("应回退到备选命令");
        assert!(resolved.used_fallback);
        assert_eq!(resolved.command.command, exe);
        assert!(resolved.config.args.is_empty());

        // 首选可用时不走备选。
        let registry = LanguageRegistry::new(vec![LanguageServerSpec::new(
            "typescript",
            &["ts"],
            CommandSpec::new(exe.clone(), &["--stdio"]),
        )
        .with_fallback(CommandSpec::new("ai-ide-missing-ts-server", &[]))]);
        let resolved = registry
            .resolve_for_path(Path::new("E:/tmp/a.ts"), &ServerOverrides::new())
            .expect("首选可用");
        assert!(!resolved.used_fallback);
        assert_eq!(resolved.config.args, vec!["--stdio".to_string()]);
    }

    #[test]
    fn root_defaults_to_the_file_directory_when_it_exists() {
        let registry = LanguageRegistry::builtin();
        let dir = std::env::current_dir().expect("cwd");
        let file = dir.join("a.ts");
        let overrides = ServerOverrides::new().command(command_that_exists());
        let resolved = registry.resolve_for_path(&file, &overrides).expect("ok");
        assert_eq!(resolved.config.cwd, Some(dir));

        // 目录不存在时不要塞一个必然 spawn 失败的 cwd。
        let overrides = ServerOverrides::new().command(command_that_exists());
        let resolved = registry
            .resolve_for_path(Path::new("E:/ai-ide-no-such-dir-xyz/a.ts"), &overrides)
            .expect("ok");
        assert_eq!(resolved.config.cwd, None);
    }

    fn command_that_exists() -> String {
        std::env::current_exe().expect("exe").display().to_string()
    }

    #[test]
    fn command_availability_accepts_paths_and_rejects_ghosts() {
        assert!(command_available(&command_that_exists()));
        assert!(!command_available("ai-ide-missing-lsp-binary-xyz"));
        assert!(!command_available(""));
        assert!(!command_available("   "));
        // 带路径但文件不存在：不能当成 PATH 里的名字去搜。
        assert!(!command_available("E:/ai-ide-no-such-dir-xyz/ghost.exe"));
    }
}
