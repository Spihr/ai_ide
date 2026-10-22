//! MCP 层错误与分类。
//!
//! 与 `crates/git`、`crates/terminal` 同一取舍：本 crate 不依赖 `ai-ide-protocol`，
//! 内核协议错误码的映射留在 `crates/core`。所以这里只做**分类**，不做码表：
//! 上层按 [`ErrorClass`] 决定「提示用户改配置」「重连」「换个 Server」还是「直接上报」。
//!
//! 分类刻意做细，因为 MCP 的失败形态差别很大：
//!
//! - **讲人话的**：配置写错、命令不存在 → 用户改一下就能好，不该自动重试；
//! - **可自愈的**：Server 崩了、网络断了、响应超时 → 下一次调用应当重连；
//! - **不可自愈的**：协议版本不兼容、JSON 帧非法 → 重连多少次都一样，必须暴露出来；
//! - **Server 业务错误**：`-32602` 之类的 JSON-RPC error → 连接是好的，错在参数。

use serde_json::Value;

/// MCP 能力失败的原因。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum McpError {
    /// `mcp.json` 读不到 / 解析不了 / 展开变量失败 / 字段自相矛盾。
    #[error("MCP 配置错误：{0}")]
    Config(String),

    /// stdio 传输启动子进程失败（命令不存在、工作目录无效等）。
    #[error("MCP Server `{server}` 启动失败：{reason}")]
    Spawn {
        /// Server 名（`mcp.json` 里的 key）。
        server: String,
        /// 原始错误文本。
        reason: String,
    },

    /// 传输层 IO 失败（写 stdin、读 stdout、SSE 流中断、响应体超限等）。
    #[error("MCP Server `{server}` 传输失败：{reason}")]
    Transport {
        /// Server 名。
        server: String,
        /// 原始错误文本。
        reason: String,
    },

    /// HTTP 传输拿到了非 2xx 状态码。
    ///
    /// 单独成型是因为 401/403 与 5xx 的处理策略相反：前者要提示用户改 `headers`，
    /// 后者才值得重连。
    #[error("MCP Server `{server}` HTTP {status}：{detail}")]
    Http {
        /// Server 名。
        server: String,
        /// HTTP 状态码。
        status: u16,
        /// 响应体摘要（截断，不含请求头）。
        detail: String,
    },

    /// 单次请求超过预算时间。
    ///
    /// 对 stdio 是**致命**的：超时后迟到的响应会在下一次请求时被读到，造成 id 错配，
    /// 因此客户端会直接把连接标记为待重连（见 [`crate::McpClient`]）。
    #[error("MCP 请求超时：{method} 超过 {timeout_ms}ms")]
    Timeout {
        /// 方法名。
        method: String,
        /// 超时预算（毫秒）。
        timeout_ms: u64,
    },

    /// Server 进程退出 / 连接被关闭，且没有待匹配的响应。
    #[error("MCP Server `{server}` 连接已断开：{reason}")]
    Disconnected {
        /// Server 名。
        server: String,
        /// 断开原因（含退出码与 stderr 尾巴，若可得）。
        reason: String,
    },

    /// 帧不合 JSON-RPC 2.0，或响应结构与方法契约不符（含协议版本不兼容）。
    #[error("MCP 协议错误：{0}")]
    Protocol(String),

    /// JSON-RPC error 对象：连接正常，Server 明确拒绝了这次请求。
    #[error("MCP Server 返回错误 {code}：{message}")]
    Rpc {
        /// JSON-RPC 错误码（`-32601` / `-32602` / Server 自定义码等）。
        code: i64,
        /// Server 给出的错误消息。
        message: String,
        /// 可选的附加数据（如参数校验明细）。
        data: Option<Value>,
    },

    /// 还没握手就发请求（内部状态机拒绝）。
    #[error("MCP Server `{0}` 尚未完成初始化")]
    NotInitialized(String),

    /// 按名字找不到 Server（Manager 层）。
    #[error("未知 MCP Server：{0}")]
    UnknownServer(String),
}

/// 错误大类。上层按类决定重试与提示策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// 配置问题：用户改 `mcp.json` 才能好。
    Config,
    /// 进程起不来：命令/权限/工作目录问题。
    Spawn,
    /// 传输中断：可重连。
    Transport,
    /// HTTP 非 2xx：4xx 一般是配置/凭据，5xx 才值得重连。
    Http,
    /// 超时：可重连。
    Timeout,
    /// 连接已断：可重连。
    Disconnected,
    /// 协议层不可自愈：重连无用。
    Protocol,
    /// Server 业务错误：连接正常。
    Rpc,
    /// 状态机拒绝：调用方用错（未初始化 / 未知 Server）。
    State,
}

impl McpError {
    /// 错误大类。
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Config(_) => ErrorClass::Config,
            Self::Spawn { .. } => ErrorClass::Spawn,
            Self::Transport { .. } => ErrorClass::Transport,
            Self::Http { .. } => ErrorClass::Http,
            Self::Timeout { .. } => ErrorClass::Timeout,
            Self::Disconnected { .. } => ErrorClass::Disconnected,
            Self::Protocol(_) => ErrorClass::Protocol,
            Self::Rpc { .. } => ErrorClass::Rpc,
            Self::NotInitialized(_) | Self::UnknownServer(_) => ErrorClass::State,
        }
    }

    /// 是否值得自动重连后再试一次。
    ///
    /// 只覆盖「换个连接就可能好」的类：传输断、超时、断连、HTTP 5xx。
    /// 配置错、协议不兼容、参数错都不算——重试只会把同一个错误刷两遍。
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport { .. } | Self::Timeout { .. } | Self::Disconnected { .. } => true,
            Self::Http { status, .. } => *status >= 500,
            _ => false,
        }
    }

    /// 是否属于「鉴权/授权」类 HTTP 失败，UI 据此引导用户检查 `headers`。
    pub fn is_auth(&self) -> bool {
        matches!(self, Self::Http { status, .. } if *status == 401 || *status == 403)
    }

    /// 便捷构造：把任意 IO / 第三方错误收敛成传输错误。
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_like_errors_are_retryable() {
        assert!(McpError::transport("fs", "pipe closed").is_retryable());
        assert!(McpError::disconnected("fs", "exit 1").is_retryable());
        assert!(McpError::Timeout {
            method: "tools/call".into(),
            timeout_ms: 30_000
        }
        .is_retryable());
        assert!(McpError::Http {
            server: "github".into(),
            status: 502,
            detail: String::new()
        }
        .is_retryable());
    }

    #[test]
    fn configuration_and_protocol_errors_are_not_retryable() {
        assert!(!McpError::Config("缺少 command".into()).is_retryable());
        assert!(!McpError::Protocol("jsonrpc 版本非法".into()).is_retryable());
        assert!(!McpError::Rpc {
            code: -32602,
            message: "invalid params".into(),
            data: None
        }
        .is_retryable());
        assert!(!McpError::Http {
            server: "github".into(),
            status: 401,
            detail: String::new()
        }
        .is_retryable());
    }

    #[test]
    fn auth_is_recognized_from_http_status() {
        assert!(McpError::Http {
            server: "github".into(),
            status: 401,
            detail: String::new()
        }
        .is_auth());
        assert!(McpError::Http {
            server: "github".into(),
            status: 403,
            detail: String::new()
        }
        .is_auth());
        assert!(!McpError::Http {
            server: "github".into(),
            status: 500,
            detail: String::new()
        }
        .is_auth());
    }

    #[test]
    fn classes_cover_every_variant() {
        let cases = [
            (McpError::Config("x".into()), ErrorClass::Config),
            (
                McpError::Spawn {
                    server: "a".into(),
                    reason: "x".into(),
                },
                ErrorClass::Spawn,
            ),
            (McpError::transport("a", "x"), ErrorClass::Transport),
            (
                McpError::Timeout {
                    method: "m".into(),
                    timeout_ms: 1,
                },
                ErrorClass::Timeout,
            ),
            (McpError::disconnected("a", "x"), ErrorClass::Disconnected),
            (McpError::Protocol("x".into()), ErrorClass::Protocol),
            (
                McpError::Rpc {
                    code: -32601,
                    message: "x".into(),
                    data: None,
                },
                ErrorClass::Rpc,
            ),
            (McpError::NotInitialized("a".into()), ErrorClass::State),
            (McpError::UnknownServer("a".into()), ErrorClass::State),
        ];
        for (err, expected) in cases {
            assert_eq!(err.class(), expected, "{err}");
        }
    }

    #[test]
    fn timeout_message_carries_budget() {
        let text = McpError::Timeout {
            method: "tools/call".into(),
            timeout_ms: 1500,
        }
        .to_string();
        assert!(text.contains("tools/call"), "{text}");
        assert!(text.contains("1500"), "{text}");
    }
}
