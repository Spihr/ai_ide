//! 模型接入层错误。
//!
//! 与协议错误码（`docs/03` §1.3 的 `2xxx` 分区）一一对应：这里抛出的错误
//! 最终都会经 [`LlmError::to_error_object`] 变成 `llm.error` 事件或请求错误响应。
//! 这样「上游 401 / 429 / 超时」在 UI 侧能被区分成「换密钥 / 稍后重试 / 查网络」，
//! 而不是笼统一句「调用失败」。

use ai_ide_protocol::{ErrorCode, ErrorObject};
use serde_json::Value;

/// 模型接入层的统一错误类型。
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// Provider 不存在或未启用。
    #[error("Provider 未配置或未启用：{provider_id}")]
    ProviderNotConfigured { provider_id: String },

    /// 上游拒绝鉴权（401 / 403）。
    #[error("鉴权失败（HTTP {status}）：{message}")]
    AuthFailed {
        status: u16,
        message: String,
        upstream: Option<Value>,
    },

    /// 触发限流（429）。
    #[error("上游限流（HTTP {status}）：{message}")]
    RateLimited {
        status: u16,
        message: String,
        retry_after_ms: Option<u64>,
        upstream: Option<Value>,
    },

    /// 上游不可用：超时、5xx、流中途断开、以及本地未启用 HTTP 传输的骨架态。
    #[error("上游不可用：{message}")]
    Unavailable {
        message: String,
        status: Option<u16>,
    },

    /// 连接层失败（DNS / TLS / 连接被拒）。
    #[error("网络错误：{0}")]
    Transport(String),

    /// 上游响应体无法按协议解释。
    #[error("上游响应无法解析：{0}")]
    BadResponse(String),

    /// 上下文超出模型窗口。
    #[error("上下文超限：{0}")]
    ContextOverflow(String),

    /// 请求被取消（用户点了停止）。
    #[error("请求已取消")]
    Cancelled,

    /// 本地配置读写失败。
    #[error("配置存储错误：{0}")]
    Store(String),

    /// 协议适配器尚未实现（例如 M2 只落地了 Chat Completions）。
    #[error("暂不支持的协议：{0}")]
    Unsupported(String),
}

impl LlmError {
    /// 折算后的协议错误码。
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::ProviderNotConfigured { .. } => ErrorCode::PROVIDER_NOT_CONFIGURED,
            Self::AuthFailed { .. } => ErrorCode::PROVIDER_AUTH_FAILED,
            Self::RateLimited { .. } => ErrorCode::PROVIDER_RATE_LIMITED,
            Self::ContextOverflow(_) => ErrorCode::CONTEXT_OVERFLOW,
            Self::Cancelled => ErrorCode::CANCELLED,
            Self::Store(_) => ErrorCode::INTERNAL_ERROR,
            Self::Unavailable { .. }
            | Self::Transport(_)
            | Self::BadResponse(_)
            | Self::Unsupported(_) => ErrorCode::PROVIDER_UNAVAILABLE,
        }
    }

    /// 上游原始错误体（若拿到了）。
    pub fn upstream(&self) -> Option<Value> {
        match self {
            Self::AuthFailed { upstream, .. } | Self::RateLimited { upstream, .. } => upstream.clone(),
            _ => None,
        }
    }

    /// 建议的重试等待时长。
    pub fn retry_after_ms(&self) -> Option<u64> {
        match self {
            Self::RateLimited { retry_after_ms, .. } => *retry_after_ms,
            _ => None,
        }
    }

    /// 变成协议错误对象；`upstream` / `retry_after_ms` 落在 `error.data` 里。
    pub fn to_error_object(&self) -> ErrorObject {
        let mut data = serde_json::Map::new();
        if let Some(upstream) = self.upstream() {
            data.insert("upstream".to_string(), upstream);
        }
        if let Some(retry_after_ms) = self.retry_after_ms() {
            data.insert("retry_after_ms".to_string(), retry_after_ms.into());
        }
        let error = ErrorObject::new(self.code(), self.to_string());
        if data.is_empty() {
            error
        } else {
            error.with_data(Value::Object(data))
        }
    }

    pub fn unsupported(protocol: impl std::fmt::Display) -> Self {
        Self::Unsupported(protocol.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_map_to_protocol_domains() {
        assert_eq!(
            LlmError::ProviderNotConfigured {
                provider_id: "p1".into()
            }
            .code(),
            ErrorCode::PROVIDER_NOT_CONFIGURED
        );
        assert_eq!(
            LlmError::AuthFailed {
                status: 401,
                message: "bad key".into(),
                upstream: None
            }
            .code(),
            ErrorCode::PROVIDER_AUTH_FAILED
        );
        assert_eq!(
            LlmError::RateLimited {
                status: 429,
                message: "slow down".into(),
                retry_after_ms: Some(1200),
                upstream: None
            }
            .code(),
            ErrorCode::PROVIDER_RATE_LIMITED
        );
        assert_eq!(
            LlmError::Cancelled.code(),
            ErrorCode::CANCELLED,
            "取消必须能和上游错误区分开"
        );
        assert_eq!(
            LlmError::Transport("dns".into()).code(),
            ErrorCode::PROVIDER_UNAVAILABLE
        );
    }

    #[test]
    fn error_object_carries_upstream_and_retry_hint() {
        let err = LlmError::RateLimited {
            status: 429,
            message: "too many requests".into(),
            retry_after_ms: Some(2000),
            upstream: Some(serde_json::json!({ "error": { "message": "too many requests" } })),
        };
        let obj = err.to_error_object();
        let data = obj.data.expect("限流错误应带 data");
        assert_eq!(data["retry_after_ms"], 2000);
        assert!(data["upstream"]["error"]["message"].is_string());
    }

    #[test]
    fn error_object_stays_clean_without_data() {
        let obj = LlmError::Cancelled.to_error_object();
        assert!(obj.data.is_none());
    }
}
