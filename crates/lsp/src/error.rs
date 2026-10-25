//! LSP 层错误与分类。
//!
//! 与 `crates/mcp`、`crates/git` 同一取舍：本 crate 不依赖 `ai-ide-protocol`，
//! 内核错误码的映射留在 `crates/core`。这里只做**分类**，让上层按
//! [`ErrorClass`] 决定「提示用户装语言服务器」「重连」「换个服务器」还是「直接上报」。
//!
//! LSP 的失败形态比 MCP 更集中，但对**分帧**特别敏感：
//!
//! - 进程起不来（`rust-analyzer` 不在 PATH）→ 用户改配置就能好；
//! - 服务器中途退出（崩了 / 被 OOM 杀掉）→ 可重连，但当前请求必然失败；
//! - `Content-Length` 缺失或超限、JSON 不合法 → 帧边界已不可信，整条连接作废；
//! - 请求超时 → 可重试，但迟到的响应必须被丢弃（见 [`crate::stdio_transport`]）；
//! - JSON-RPC error 对象 → 连接正常，错在参数或方法。

use serde_json::Value;

/// LSP 能力失败的原因。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LspError {
    /// 子进程启动失败（命令不存在、工作目录无效等）。
    #[error("语言服务器 `{server}` 启动失败：{reason}")]
    Spawn {
        /// 服务器名（配置里的 key，例如 `rust-analyzer`）。
        server: String,
        /// 原始错误文本。
        reason: String,
    },

    /// 传输层 IO 失败（写 stdin、读 stdout、管道断开等）。
    #[error("语言服务器 `{server}` 传输失败：{reason}")]
    Transport {
        /// 服务器名。
        server: String,
        /// 原始错误文本。
        reason: String,
    },

    /// 单次请求超过预算时间。
    ///
    /// 对 stdio 是**危险**的：超时后迟到的响应仍在管道里，本层会在读循环里按 id
    /// 丢弃它，避免错配给下一个请求；但调用方应当把这次失败当成「服务可能卡住」。
    #[error("LSP 请求超时：{method} 超过 {timeout_ms}ms")]
    Timeout {
        /// 方法名。
        method: String,
        /// 超时预算（毫秒）。
        timeout_ms: u64,
    },

    /// 服务器进程退出 / 连接被关闭，且没有待匹配的响应。
    #[error("语言服务器 `{server}` 连接已断开：{reason}")]
    Disconnected {
        /// 服务器名。
        server: String,
        /// 断开原因（含退出码，若可得）。
        reason: String,
    },

    /// 帧不合规：`Content-Length` 缺失/非法/超限、消息体不是 UTF-8、JSON-RPC 结构错误。
    ///
    /// 一旦命中就必须作废整条连接——帧边界错了，后续字节全是噪声。
    #[error("LSP 协议错误：{0}")]
    Protocol(String),

    /// JSON-RPC error 对象：连接正常，服务器明确拒绝了这次请求。
    #[error("语言服务器返回错误 {code}：{message}")]
    Rpc {
        /// JSON-RPC 错误码（`-32601` / `-32602` / 服务器自定义码等）。
        code: i64,
        /// 服务器给出的错误消息。
        message: String,
        /// 可选的附加数据。
        data: Option<Value>,
    },

    /// 还没握手就发请求（内部状态机拒绝）。
    #[error("语言服务器 `{0}` 尚未完成初始化")]
    NotInitialized(String),

    /// 连接已关闭后继续使用。
    #[error("语言服务器 `{0}` 的连接已关闭")]
    Closed(String),

    /// 文档同步状态不对（对未打开的文档发 `didChange` / `didClose`，或重复打开）。
    #[error("文档同步错误：{0}")]
    Document(String),

    /// 语言服务器命令未安装（注册表解析出默认命令，但 PATH 里找不到）。
    ///
    /// 单独成一个变体而不是复用 [`LspError::Spawn`]：这条错误要能直接给用户看
    /// ——「该装什么」，所以必须带上命令名与安装提示，而不是等到 spawn 失败再
    /// 拿到一句笼统的 `No such file or directory`。
    #[error("语言服务器未安装：`{command}`（{language}）。{hint}")]
    NotInstalled {
        /// 缺失的可执行文件名。
        command: String,
        /// 语言标识（用于提示用户装哪一套）。
        language: String,
        /// 安装提示（来自注册表）。
        hint: String,
    },

    /// 文件扩展名没有对应的语言服务器（注册表里没有），也不是调用方显式指定语言。
    #[error("没有为 `{path}` 找到语言服务器：扩展名未登记，也没有显式指定 language_id")]
    UnsupportedLanguage {
        /// 触发失败的文件路径或语言标识。
        path: String,
    },
}

/// 错误大类。上层按类决定重试与提示策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// 进程起不来：命令/权限/工作目录问题。
    Spawn,
    /// 传输中断：可重连。
    Transport,
    /// 超时：可重连。
    Timeout,
    /// 连接已断：可重连。
    Disconnected,
    /// 协议层不可自愈：重连无用，必须修服务器或升级协议实现。
    Protocol,
    /// 服务器业务错误：连接正常。
    Rpc,
    /// 状态机拒绝：调用方用错（未初始化 / 已关闭）。
    State,
}

impl LspError {
    /// 错误大类。
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Spawn { .. } | Self::NotInstalled { .. } => ErrorClass::Spawn,
            Self::Transport { .. } => ErrorClass::Transport,
            Self::Timeout { .. } => ErrorClass::Timeout,
            Self::Disconnected { .. } => ErrorClass::Disconnected,
            Self::Protocol(_) => ErrorClass::Protocol,
            Self::Rpc { .. } => ErrorClass::Rpc,
            Self::NotInitialized(_) | Self::Closed(_) | Self::Document(_) => ErrorClass::State,
            Self::UnsupportedLanguage { .. } => ErrorClass::State,
        }
    }

    /// 是否值得自动重连后再试一次。
    ///
    /// 只覆盖「换个连接就可能好」的类。协议错与状态错重试只会把同一个错误刷两遍。
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Transport { .. } | Self::Timeout { .. } | Self::Disconnected { .. }
        )
    }

    /// 是否说明「连接已经不可再用」——命中后本层会立即标记关闭并唤醒所有等待者。
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::Protocol(_) | Self::Disconnected { .. } | Self::Transport { .. }
        )
    }

    /// 便捷构造：把任意 IO 错误收敛成传输错误。
    pub fn transport(server: impl Into<String>, reason: impl std::fmt::Display) -> Self {
        Self::Transport {
            server: server.into(),
            reason: reason.to_string(),
        }
    }

    /// 便捷构造：断连。
    pub fn disconnected(server: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Disconnected {
            server: server.into(),
            reason: reason.into(),
        }
    }

    /// 便捷构造：协议错误。
    pub fn protocol(reason: impl Into<String>) -> Self {
        Self::Protocol(reason.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_classes_are_exactly_the_connection_level_ones() {
        assert!(LspError::transport("rs", "pipe closed").is_retryable());
        assert!(LspError::disconnected("rs", "exit 1").is_retryable());
        assert!(LspError::Timeout {
            method: "textDocument/hover".into(),
            timeout_ms: 30_000,
        }
        .is_retryable());

        assert!(!LspError::Spawn {
            server: "rs".into(),
            reason: "not found".into(),
        }
        .is_retryable());
        assert!(!LspError::protocol("缺 Content-Length").is_retryable());
        assert!(!LspError::Rpc {
            code: -32602,
            message: "invalid params".into(),
            data: None,
        }
        .is_retryable());
        assert!(!LspError::NotInitialized("rs".into()).is_retryable());
        assert!(!LspError::Closed("rs".into()).is_retryable());
    }

    #[test]
    fn fatal_errors_cover_frame_boundary_loss_and_pipe_breaks() {
        assert!(LspError::protocol("bad frame").is_fatal());
        assert!(LspError::disconnected("rs", "exit").is_fatal());
        assert!(LspError::transport("rs", "broken pipe").is_fatal());
        assert!(!LspError::Timeout {
            method: "m".into(),
            timeout_ms: 1,
        }
        .is_fatal());
    }

    #[test]
    fn classes_cover_every_variant() {
        let cases = [
            (
                LspError::Spawn {
                    server: "a".into(),
                    reason: "x".into(),
                },
                ErrorClass::Spawn,
            ),
            (LspError::transport("a", "x"), ErrorClass::Transport),
            (
                LspError::Timeout {
                    method: "m".into(),
                    timeout_ms: 1,
                },
                ErrorClass::Timeout,
            ),
            (LspError::disconnected("a", "x"), ErrorClass::Disconnected),
            (LspError::protocol("x"), ErrorClass::Protocol),
            (
                LspError::Rpc {
                    code: -32601,
                    message: "x".into(),
                    data: None,
                },
                ErrorClass::Rpc,
            ),
            (LspError::NotInitialized("a".into()), ErrorClass::State),
            (LspError::Closed("a".into()), ErrorClass::State),
            (LspError::Document("未打开".into()), ErrorClass::State),
            (
                LspError::NotInstalled {
                    command: "typescript-language-server".into(),
                    language: "typescript".into(),
                    hint: "npm i -g typescript-language-server".into(),
                },
                ErrorClass::Spawn,
            ),
            (
                LspError::UnsupportedLanguage {
                    path: "a.unknownext".into(),
                },
                ErrorClass::State,
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.class(), expected, "{err}");
        }
    }

    #[test]
    fn timeout_message_carries_budget() {
        let text = LspError::Timeout {
            method: "textDocument/definition".into(),
            timeout_ms: 1500,
        }
        .to_string();
        assert!(text.contains("textDocument/definition"), "{text}");
        assert!(text.contains("1500"), "{text}");
    }

    #[test]
    fn not_installed_message_tells_the_user_what_to_install() {
        let error = LspError::NotInstalled {
            command: "pyright-langserver".into(),
            language: "python".into(),
            hint: "npm i -g pyright".into(),
        };
        let text = error.to_string();
        assert!(text.contains("未安装"), "{text}");
        assert!(text.contains("pyright-langserver"), "{text}");
        assert!(text.contains("python"), "{text}");
        assert!(text.contains("npm i -g pyright"), "{text}");
        // 「没装」属于可修复的配置问题：不重试、也不是致命断连。
        assert!(!error.is_retryable(), "{text}");
        assert!(!error.is_fatal(), "{text}");
    }
}
