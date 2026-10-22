//! `mcp.json` 配置解析。
//!
//! 配置面很小，但坑集中在两处：
//!
//! 1. **`${ENV}` 展开**。MCP Server 的凭据几乎都放环境变量里（`GITHUB_TOKEN` 之类），
//!    配置文件要能引用它们，同时又不能把值落进日志/诊断包。所以本模块：
//!    - 支持 `${NAME}` 与带默认值的 `${NAME:-默认值}`；
//!    - 支持 `$$` 转义成字面 `$`（避免 Windows 路径里的 `$` 被误吞）；
//!    - 变量缺失且无默认值时**直接报配置错**，并把变量名写进消息——绝不把值写进去；
//!    - `Debug` 实现只打印 `env` / `headers` 的**键名**，值一律 `<redacted>`。
//! 2. **两种传输的判定**。显式 `type` 优先；没有 `type` 时按「有 `command` 就是 stdio，
//!    有 `url` 就是 http」推断；两者都缺或都冲突时报错，不猜。
//!
//! 文件形态（与 docs/04 §8 一致）：
//!
//! ```json
//! {
//!   "mcpServers": {
//!     "filesystem": {
//!       "type": "stdio",
//!       "command": "npx",
//!       "args": ["-y", "@modelcontextprotocol/server-filesystem", "E:/work"],
//!       "env": { "NODE_OPTIONS": "--max-old-space-size=512" },
//!       "cwd": "E:/work"
//!     },
//!     "github": {
//!       "type": "http",
//!       "url": "https://api.example.com/mcp",
//!       "headers": { "Authorization": "Bearer ${GITHUB_TOKEN}" }
//!     }
//!   }
//! }
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::McpError;

/// 默认配置路径相对 `%APPDATA%`（Windows）或 `$XDG_CONFIG_HOME`（其他平台）的尾部。
pub const DEFAULT_CONFIG_TAIL: [&str; 2] = ["ai-ide", "mcp.json"];

/// 覆盖默认路径的环境变量（便于测试与多实例）。
pub const CONFIG_PATH_ENV: &str = "AI_IDE_MCP_CONFIG";

/// 环境变量来源。抽成 trait 是为了让配置解析**不依赖真实进程环境**即可单测。
pub trait EnvSource {
    /// 取变量值；不存在返回 `None`。
    fn lookup(&self, name: &str) -> Option<String>;
}

/// 只读当前进程环境。
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn lookup(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

impl EnvSource for BTreeMap<String, String> {
    fn lookup(&self, name: &str) -> Option<String> {
        self.get(name).cloned()
    }
}

/// 单条 `mcp.json` 解析结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct McpConfig {
    /// Server 名 → 配置。用 `BTreeMap` 保证遍历顺序稳定（诊断输出可 diff）。
    pub servers: BTreeMap<String, McpServerConfig>,
}

impl McpConfig {
    /// 用当前进程环境解析配置文本。
    pub fn parse(text: &str) -> Result<Self, McpError> {
        Self::parse_with_env(text, &ProcessEnv)
    }

    /// 用指定环境解析配置文本（测试入口）。
    pub fn parse_with_env(text: &str, env: &dyn EnvSource) -> Result<Self, McpError> {
        let raw: RawFile = serde_json::from_str(text)
            .map_err(|err| McpError::Config(format!("JSON 解析失败：{err}")))?;
        let mut servers = BTreeMap::new();
        for (name, server) in raw.servers {
            validate_server_name(&name)?;
            if server.disabled {
                tracing::info!(target: "mcp", server = %name, "mcp.json 中被标记 disabled，跳过");
                continue;
            }
            let config = server.into_config(&name, env)?;
            servers.insert(name, config);
        }
        Ok(Self { servers })
    }

    /// 从文件读取并解析。
    pub fn load_from(path: &Path) -> Result<Self, McpError> {
        let text = std::fs::read_to_string(path)
            .map_err(|err| McpError::Config(format!("读取 {} 失败：{err}", path.display())))?;
        Self::parse(&text)
    }

    /// 读取默认位置的配置。文件不存在时返回空配置（首次启动的正常状态，不算错误）。
    pub fn load_default() -> Result<Self, McpError> {
        let Some(path) = default_config_path() else {
            tracing::debug!(target: "mcp", "无法确定配置目录，按空配置处理");
            return Ok(Self::default());
        };
        if !path.is_file() {
            tracing::debug!(target: "mcp", path = %path.display(), "未发现 mcp.json，按空配置处理");
            return Ok(Self::default());
        }
        Self::load_from(&path)
    }

    /// 按名字取 Server 配置。
    pub fn server(&self, name: &str) -> Option<&McpServerConfig> {
        self.servers.get(name)
    }

    /// 是否没有任何 Server。
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }
}

/// 单个 MCP Server 的连接方式。
#[derive(Clone, PartialEq)]
pub enum McpServerConfig {
    /// 本地子进程，逐行 JSON 走 stdin/stdout。
    Stdio(StdioServerConfig),
    /// 远端 HTTP（响应可为 JSON 或 `text/event-stream`）。
    Http(HttpServerConfig),
}

impl McpServerConfig {
    /// Server 名（来自 `mcp.json` 的 key）。
    pub fn name(&self) -> &str {
        match self {
            Self::Stdio(config) => &config.name,
            Self::Http(config) => &config.name,
        }
    }

    /// 传输类型标签（诊断用）。
    pub fn transport_label(&self) -> &'static str {
        match self {
            Self::Stdio(_) => "stdio",
            Self::Http(config) => config.label(),
        }
    }
}

impl std::fmt::Debug for McpServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stdio(config) => std::fmt::Debug::fmt(config, f),
            Self::Http(config) => std::fmt::Debug::fmt(config, f),
        }
    }
}

/// stdio 传输配置。
#[derive(Clone, PartialEq)]
pub struct StdioServerConfig {
    /// Server 名。
    pub name: String,
    /// 可执行文件（已展开 `${ENV}`）。
    pub command: String,
    /// 参数（逐个展开）。
    pub args: Vec<String>,
    /// 追加/覆盖的环境变量（值已展开）。
    pub env: BTreeMap<String, String>,
    /// 工作目录。
    pub cwd: Option<PathBuf>,
}

impl std::fmt::Debug for StdioServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StdioServerConfig")
            .field("name", &self.name)
            .field("command", &self.command)
            .field("args", &self.args)
            // 只打键名：值可能是 token。
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .field("cwd", &self.cwd)
            .finish()
    }
}

/// HTTP 传输配置。
#[derive(Clone, PartialEq)]
pub struct HttpServerConfig {
    /// Server 名。
    pub name: String,
    /// 端点 URL（已展开 `${ENV}`）。
    pub url: String,
    /// 附加请求头（值已展开，打印时脱敏）。
    pub headers: BTreeMap<String, String>,
    /// 配置里写的是 `type: "sse"`。
    ///
    /// 现在两种写法走同一实现：`streamable HTTP` 的响应既可能是 `application/json`，
    /// 也可能是 `text/event-stream`，本 crate 都支持。但保留原始标记，
    /// 便于诊断时区分用户意图，也为将来接旧式双向 SSE 留出判据。
    pub declared_sse: bool,
}

impl HttpServerConfig {
    fn label(&self) -> &'static str {
        if self.declared_sse {
            "http(sse)"
        } else {
            "http"
        }
    }
}

impl std::fmt::Debug for HttpServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpServerConfig")
            .field("name", &self.name)
            .field("url", &self.url)
            .field("header_keys", &self.headers.keys().collect::<Vec<_>>())
            .field("declared_sse", &self.declared_sse)
            .finish()
    }
}

/// 计算默认配置文件路径。
///
/// 优先级：`AI_IDE_MCP_CONFIG` > `%APPDATA%` > `$XDG_CONFIG_HOME` > `$HOME/.config`。
pub fn default_config_path() -> Option<PathBuf> {
    let override_path = std::env::var(CONFIG_PATH_ENV).ok();
    let appdata = std::env::var("APPDATA").ok();
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .map(PathBuf::from);
    resolve_config_path(
        override_path.as_deref(),
        appdata.as_deref(),
        xdg.as_deref(),
        home.as_deref(),
    )
}

/// [`default_config_path`] 的纯函数形态（便于离线单测与上层注入）。
pub fn resolve_config_path(
    override_path: Option<&str>,
    appdata: Option<&str>,
    xdg_config_home: Option<&str>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(path) = override_path.map(str::trim).filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let join = |root: &str| {
        let mut path = PathBuf::from(root);
        for part in DEFAULT_CONFIG_TAIL {
            path.push(part);
        }
        path
    };
    if let Some(root) = appdata.map(str::trim).filter(|root| !root.is_empty()) {
        return Some(join(root));
    }
    if let Some(root) = xdg_config_home
        .map(str::trim)
        .filter(|root| !root.is_empty())
    {
        return Some(join(root));
    }
    // 最后兜底：`$HOME/.config/ai-ide/mcp.json`（Linux/macOS 的惯例位置）。
    // Windows 上 APPDATA 基本一定存在，走不到这里；真走到也是合理猜测。
    home.map(|root| {
        let mut path = root.join(".config");
        for part in DEFAULT_CONFIG_TAIL {
            path.push(part);
        }
        path
    })
}

/// 展开 `${NAME}` / `${NAME:-默认值}` / `$$`。
///
/// 只做单层展开：默认值里再写 `${...}` 不会被递归解释（保持实现可预测）。
pub fn expand_env(input: &str, env: &dyn EnvSource) -> Result<String, McpError> {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(position) = rest.find('$') {
        output.push_str(&rest[..position]);
        rest = &rest[position..];
        if let Some(stripped) = rest.strip_prefix("$$") {
            output.push('$');
            rest = stripped;
            continue;
        }
        if !rest.starts_with("${") {
            output.push('$');
            rest = &rest[1..];
            continue;
        }
        let close = rest
            .find('}')
            .ok_or_else(|| McpError::Config(format!("环境变量引用未闭合：{input}")))?;
        let expression = &rest[2..close];
        let (name, default) = match expression.split_once(":-") {
            Some((name, default)) => (name, Some(default)),
            None => (expression, None),
        };
        if name.is_empty() {
            return Err(McpError::Config(format!("环境变量名为空：{input}")));
        }
        match env.lookup(name) {
            Some(value) if !value.is_empty() => output.push_str(&value),
            _ => match default {
                Some(default) => output.push_str(default),
                None => {
                    return Err(McpError::Config(format!(
                        "环境变量 {name} 未设置（可写成 ${{{name}:-默认值}}）"
                    )))
                }
            },
        }
        rest = &rest[close + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

fn validate_server_name(name: &str) -> Result<(), McpError> {
    if name.trim().is_empty() {
        return Err(McpError::Config("Server 名不能为空".into()));
    }
    if name.chars().any(char::is_whitespace) {
        // 工具名会被拼成 `mcp__<server>__<tool>`，空白会让调用侧的解析与展示都变得可疑。
        return Err(McpError::Config(format!(
            "Server 名 `{name}` 不能包含空白字符"
        )));
    }
    if name.contains("__") {
        return Err(McpError::Config(format!(
            "Server 名 `{name}` 不能包含 `__`（与工具命名空间分隔符冲突）"
        )));
    }
    Ok(())
}

#[derive(Debug, Default, Deserialize)]
struct RawFile {
    #[serde(rename = "mcpServers", default)]
    servers: BTreeMap<String, RawServer>,
}

#[derive(Debug, Default, Deserialize)]
struct RawServer {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    disabled: bool,
}

impl RawServer {
    fn into_config(self, name: &str, env: &dyn EnvSource) -> Result<McpServerConfig, McpError> {
        let kind = self
            .kind
            .as_deref()
            .map(|kind| kind.trim().to_ascii_lowercase());
        match kind.as_deref() {
            Some("stdio") => self.into_stdio(name, env),
            Some("http") | Some("streamable-http") => self.into_http(name, env, false),
            Some("sse") => self.into_http(name, env, true),
            Some(other) => Err(McpError::Config(format!(
                "Server `{name}` 的 type `{other}` 不受支持（可选 stdio / http / sse）"
            ))),
            None => {
                if self.command.is_some() && self.url.is_some() {
                    return Err(McpError::Config(format!(
                        "Server `{name}` 同时写了 command 与 url，请用 type 明确传输方式"
                    )));
                }
                if self.command.is_some() {
                    self.into_stdio(name, env)
                } else if self.url.is_some() {
                    self.into_http(name, env, false)
                } else {
                    Err(McpError::Config(format!(
                        "Server `{name}` 缺少 command（stdio）或 url（http）"
                    )))
                }
            }
        }
    }

    fn into_stdio(self, name: &str, env: &dyn EnvSource) -> Result<McpServerConfig, McpError> {
        if self.url.is_some() {
            return Err(McpError::Config(format!(
                "Server `{name}` 声明为 stdio，却写了 url"
            )));
        }
        let command = self
            .command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
            .ok_or_else(|| McpError::Config(format!("Server `{name}` 缺少 command")))?;
        let command = expand_env(command, env)?;
        let mut args = Vec::with_capacity(self.args.len());
        for arg in &self.args {
            args.push(expand_env(arg, env)?);
        }
        let mut expanded_env = BTreeMap::new();
        for (key, value) in &self.env {
            expanded_env.insert(key.clone(), expand_env(value, env)?);
        }
        let cwd = match self
            .cwd
            .as_deref()
            .map(str::trim)
            .filter(|cwd| !cwd.is_empty())
        {
            Some(cwd) => Some(PathBuf::from(expand_env(cwd, env)?)),
            None => None,
        };
        Ok(McpServerConfig::Stdio(StdioServerConfig {
            name: name.to_string(),
            command,
            args,
            env: expanded_env,
            cwd,
        }))
    }

    fn into_http(
        self,
        name: &str,
        env: &dyn EnvSource,
        declared_sse: bool,
    ) -> Result<McpServerConfig, McpError> {
        if self.command.is_some() || !self.args.is_empty() {
            return Err(McpError::Config(format!(
                "Server `{name}` 声明为 http，却写了 command/args"
            )));
        }
        let url = self
            .url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .ok_or_else(|| McpError::Config(format!("Server `{name}` 缺少 url")))?;
        let url = expand_env(url, env)?;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(McpError::Config(format!(
                "Server `{name}` 的 url 必须以 http:// 或 https:// 开头"
            )));
        }
        let mut headers = BTreeMap::new();
        for (key, value) in &self.headers {
            headers.insert(key.clone(), expand_env(value, env)?);
        }
        Ok(McpServerConfig::Http(HttpServerConfig {
            name: name.to_string(),
            url,
            headers,
            declared_sse,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    fn parse_with(text: &str, pairs: &[(&str, &str)]) -> Result<McpConfig, McpError> {
        McpConfig::parse_with_env(text, &env(pairs))
    }

    #[test]
    fn parses_stdio_server_verbatim() {
        let config = parse_with(
            r#"{"mcpServers":{"fs":{"command":"npx","args":["-y","server-filesystem"],"cwd":"E:/work"}}}"#,
            &[],
        )
        .expect("parse");
        let server = config.server("fs").expect("fs");
        assert_eq!(server.transport_label(), "stdio");
        match server {
            McpServerConfig::Stdio(stdio) => {
                assert_eq!(stdio.command, "npx");
                assert_eq!(stdio.args, vec!["-y", "server-filesystem"]);
                assert_eq!(stdio.cwd, Some(PathBuf::from("E:/work")));
                assert!(stdio.env.is_empty());
            }
            other => panic!("期望 stdio，得到 {other:?}"),
        }
    }

    #[test]
    fn parses_http_server_and_keeps_declared_sse() {
        let config = parse_with(
            r#"{"mcpServers":{"gh":{"type":"http","url":"https://example.com/mcp","headers":{"Authorization":"Bearer t"}},"legacy":{"type":"sse","url":"https://example.com/sse"}}}"#,
            &[],
        )
        .expect("parse");
        match config.server("gh").expect("gh") {
            McpServerConfig::Http(http) => {
                assert_eq!(http.headers["Authorization"], "Bearer t");
                assert!(!http.declared_sse);
            }
            other => panic!("期望 http，得到 {other:?}"),
        }
        match config.server("legacy").expect("legacy") {
            McpServerConfig::Http(http) => assert!(http.declared_sse),
            other => panic!("期望 http，得到 {other:?}"),
        }
    }

    #[test]
    fn expands_env_in_command_args_env_and_headers() {
        let config = parse_with(
            r#"{
                "mcpServers": {
                    "fs": {"command":"${BIN}","args":["${ROOT:-E:/fallback}","$$literal"],"env":{"TOKEN":"${TOKEN}"}},
                    "gh": {"type":"http","url":"https://${HOST}/mcp","headers":{"Authorization":"Bearer ${TOKEN}"}}
                }
            }"#,
            &[("BIN", "npx"), ("TOKEN", "s3cret"), ("HOST", "example.com")],
        )
        .expect("parse");
        match config.server("fs").expect("fs") {
            McpServerConfig::Stdio(stdio) => {
                assert_eq!(stdio.command, "npx");
                assert_eq!(stdio.args, vec!["E:/fallback", "$literal"]);
                assert_eq!(stdio.env["TOKEN"], "s3cret");
            }
            other => panic!("期望 stdio，得到 {other:?}"),
        }
        match config.server("gh").expect("gh") {
            McpServerConfig::Http(http) => {
                assert_eq!(http.url, "https://example.com/mcp");
                assert_eq!(http.headers["Authorization"], "Bearer s3cret");
            }
            other => panic!("期望 http，得到 {other:?}"),
        }
    }

    #[test]
    fn missing_env_without_default_is_a_config_error() {
        let err = parse_with(
            r#"{"mcpServers":{"fs":{"command":"${MCP_MISSING_BIN}"}}}"#,
            &[],
        )
        .expect_err("must fail");
        assert!(matches!(err, McpError::Config(_)), "{err}");
        assert!(err.to_string().contains("MCP_MISSING_BIN"), "{err}");
    }

    #[test]
    fn empty_env_value_uses_default() {
        let expanded = expand_env("value=${EMPTY:-fallback}", &env(&[("EMPTY", "")])).expect("ok");
        assert_eq!(expanded, "value=fallback");
    }

    #[test]
    fn unclosed_placeholder_is_a_config_error() {
        let err = expand_env("${TOKEN", &env(&[("TOKEN", "x")])).expect_err("must fail");
        assert!(matches!(err, McpError::Config(_)), "{err}");
    }

    #[test]
    fn bare_dollar_is_kept() {
        let expanded = expand_env("cost=5$ price=$", &env(&[])).expect("ok");
        assert_eq!(expanded, "cost=5$ price=$");
    }

    #[test]
    fn debug_output_never_leaks_secret_values() {
        let config = parse_with(
            r#"{"mcpServers":{"fs":{"command":"npx","env":{"TOKEN":"s3cret"}},"gh":{"type":"http","url":"https://example.com/mcp","headers":{"Authorization":"Bearer s3cret"}}}}"#,
            &[],
        )
        .expect("parse");
        let text = format!("{:?}", config);
        assert!(!text.contains("s3cret"), "配置 Debug 泄漏了密钥：{text}");
        assert!(text.contains("TOKEN"), "{text}");
        assert!(text.contains("Authorization"), "{text}");
    }

    #[test]
    fn rejects_conflicting_transport_fields() {
        let err = parse_with(
            r#"{"mcpServers":{"bad":{"command":"npx","url":"https://example.com/mcp"}}}"#,
            &[],
        )
        .expect_err("must fail");
        assert!(err.to_string().contains("type"), "{err}");

        let err = parse_with(
            r#"{"mcpServers":{"bad":{"type":"stdio","command":"npx","url":"https://example.com/mcp"}}}"#,
            &[],
        )
        .expect_err("must fail");
        assert!(err.to_string().contains("stdio"), "{err}");
    }

    #[test]
    fn rejects_unknown_type() {
        let err = parse_with(
            r#"{"mcpServers":{"bad":{"type":"websocket","url":"ws://example.com"}}}"#,
            &[],
        )
        .expect_err("must fail");
        assert!(err.to_string().contains("websocket"), "{err}");
    }

    #[test]
    fn rejects_server_without_transport_fields() {
        let err =
            parse_with(r#"{"mcpServers":{"bad":{"args":["-y"]}}}"#, &[]).expect_err("must fail");
        assert!(err.to_string().contains("command"), "{err}");
    }

    #[test]
    fn rejects_non_http_url() {
        let err = parse_with(
            r#"{"mcpServers":{"bad":{"type":"http","url":"ftp://example.com/mcp"}}}"#,
            &[],
        )
        .expect_err("must fail");
        assert!(err.to_string().contains("http://"), "{err}");
    }

    #[test]
    fn rejects_server_name_with_namespace_separator_or_space() {
        for name in ["a__b", "a b"] {
            let text = format!(r#"{{"mcpServers":{{"{name}":{{"command":"npx"}}}}}}"#);
            let err = parse_with(&text, &[]).expect_err("must fail");
            assert!(matches!(err, McpError::Config(_)), "{name}: {err}");
        }
    }

    #[test]
    fn disabled_server_is_skipped() {
        let config = parse_with(
            r#"{"mcpServers":{"off":{"command":"npx","disabled":true},"on":{"command":"npx"}}}"#,
            &[],
        )
        .expect("parse");
        assert!(config.server("off").is_none());
        assert!(config.server("on").is_some());
    }

    #[test]
    fn empty_object_yields_no_servers() {
        let config = parse_with("{}", &[]).expect("parse");
        assert!(config.is_empty());
    }

    #[test]
    fn unknown_field_is_tolerated() {
        let config = parse_with(
            r#"{"mcpServers":{"fs":{"command":"npx","note":"自定义备注"}},"comment":"x"}"#,
            &[],
        )
        .expect("parse");
        assert!(config.server("fs").is_some());
    }

    #[test]
    fn loads_from_file_and_reports_missing_file() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ai-ide-mcp-config-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::write(&path, r#"{"mcpServers":{"fs":{"command":"npx"}}}"#).expect("write");
        let config = McpConfig::load_from(&path).expect("load");
        assert!(config.server("fs").is_some());
        std::fs::remove_file(&path).expect("cleanup");

        let err = McpConfig::load_from(&path).expect_err("已删除，必须失败");
        assert!(err.to_string().contains("读取"), "{err}");
    }

    #[test]
    fn config_path_prefers_override_then_appdata_then_xdg_then_home() {
        let home = PathBuf::from("C:/Users/tester");
        assert_eq!(
            resolve_config_path(
                Some("D:/cfg/mcp.json"),
                Some("C:/AppData"),
                None,
                Some(&home)
            ),
            Some(PathBuf::from("D:/cfg/mcp.json"))
        );
        assert_eq!(
            resolve_config_path(None, Some("C:/AppData"), Some("/xdg"), Some(&home)),
            Some(PathBuf::from("C:/AppData").join("ai-ide").join("mcp.json"))
        );
        assert_eq!(
            resolve_config_path(None, None, Some("/xdg"), Some(&home)),
            Some(PathBuf::from("/xdg").join("ai-ide").join("mcp.json"))
        );
        assert_eq!(
            resolve_config_path(None, None, None, Some(&home)),
            Some(home.join(".config").join("ai-ide").join("mcp.json"))
        );
        assert_eq!(resolve_config_path(None, None, None, None), None);
    }
}
