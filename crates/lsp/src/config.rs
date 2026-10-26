//! 语言服务器启动配置。
//!
//! 只描述「怎么起一个进程」。真实产品里这份配置来自 `lsp.json` 或内置表
//! （`rust-analyzer` / `pyright` / `typescript-language-server`），解析与默认表
//! 留给 `crates/core` 或设置层——本 crate 不读磁盘、不做路径发现，方便测试。

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::framing::TransportLimits;

/// 一个语言服务器的启动参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// 服务器名（配置里的 key，也用于错误信息），例如 `rust-analyzer`。
    pub name: String,
    /// 可执行文件（绝对路径或 PATH 内的名字）。
    pub command: String,
    /// 命令行参数。
    pub args: Vec<String>,
    /// 追加的环境变量（不覆盖的变量继承父进程）。
    pub env: BTreeMap<String, String>,
    /// 工作目录；`None` 表示继承客户端进程的目录。
    pub cwd: Option<PathBuf>,
    /// 分帧上限（默认 8 KiB 头 / 16 MiB 体）。
    pub limits: TransportLimits,
}

impl ServerConfig {
    /// 用名字与命令构造一份最小配置。
    pub fn new(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            limits: TransportLimits::default(),
        }
    }

    /// 追加一个命令行参数。
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// 设置工作目录。
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// 覆盖分帧上限。
    pub fn limits(mut self, limits: TransportLimits) -> Self {
        self.limits = limits;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_fills_defaults() {
        let config = ServerConfig::new("rust-analyzer", "rust-analyzer").arg("--stdio");
        assert_eq!(config.args, vec!["--stdio".to_string()]);
        assert!(config.env.is_empty());
        assert!(config.cwd.is_none());
        assert_eq!(config.limits, TransportLimits::default());
    }

    #[test]
    fn limits_can_be_narrowed_for_tests() {
        let config = ServerConfig::new("mock", "mock")
            .cwd("C:/tmp")
            .limits(TransportLimits::new(64, 128));
        assert_eq!(config.limits.max_header_bytes, 64);
        assert_eq!(config.limits.max_content_bytes, 128);
        assert_eq!(config.cwd, Some(PathBuf::from("C:/tmp")));
    }
}
