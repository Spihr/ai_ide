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

    /// 上下文超出模型窗口。原始错误体一并带上：UI 要拿它算「超了多少」。
    #[error("上下文超限：{message}")]
    ContextOverflow {
        message: String,
        upstream: Option<Value>,
    },

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
            Self::ContextOverflow { .. } => ErrorCode::CONTEXT_OVERFLOW,
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
            Self::AuthFailed { upstream, .. }
            | Self::RateLimited { upstream, .. }
            | Self::ContextOverflow { upstream, .. } => upstream.clone(),
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

// ---------------------------------------------------------------------------
// 上游错误体的统一解释
//
// 同一段错误体有三个入口：请求被拒（HTTP 4xx/5xx）、流建好之后才报错（`data:`
// 里塞一个 `error` 帧）、以及测试。解释逻辑放在这里，是为了让「同一个错误在两条
// 入口上折成同一个码」——否则 UI 会因为「首字节前失败」和「首字节后失败」给出
// 两套互相矛盾的提示。
// ---------------------------------------------------------------------------

/// 机器可读的「上下文超限」标识（`error.code` / `error.type`）。
const CONTEXT_OVERFLOW_CODES: &[&str] = &[
    "context_length_exceeded",
    "context_overflow",
    "context_too_long",
    "max_context_length_exceeded",
    "token_limit_exceeded",
    "prompt_too_long",
    "string_above_max_length",
];

/// 人话里的「上下文超限」特征词（各家网关措辞差异极大，只能兜一层文本匹配）。
///
/// 为什么不索性把 400 一律当超限：400 里更多是「模型名写错」「参数不合法」，
/// 归错类会让 UI 提示用户去裁剪上下文——而真正该做的是改模型名。
const CONTEXT_OVERFLOW_NEEDLES: &[&str] = &[
    "context length",
    "context_length",
    "maximum context",
    "max context",
    "token limit",
    "too many tokens",
    "prompt is too long",
    "input is too long",
    "reduce the length",
    "长度超限",
    "上下文长度",
    "上下文超限",
    "超过最大长度",
];

/// 从 JSON 错误体里取出人能读的消息（兼容 `{"error":{"message":…}}` 与 `{"message":…}`）。
pub fn upstream_message(payload: &Value) -> Option<String> {
    payload
        .get("error")
        .and_then(|error| error.get("message"))
        .or_else(|| payload.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// 错误体里的机器可读码（`error.code` / `error.type` / 顶层的 `code` / `type`）。
fn upstream_error_code(payload: &Value) -> String {
    ["/error/code", "/error/type", "/code", "/type"]
        .iter()
        .find_map(|pointer| payload.pointer(pointer).and_then(Value::as_str))
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// 解析错误体；不是 JSON（很多网关在 502 时回 HTML）就当作没有。
pub fn parse_upstream_error(body: &str) -> Option<Value> {
    serde_json::from_str(body).ok()
}

/// 按字符截断，保证不切开 UTF-8。
pub fn truncate(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    let mut out: String = chars[..max_chars].iter().collect();
    out.push('…');
    out
}

/// 错误体是否在说「上下文超限」。
pub fn context_overflow_signal(message: &str, code: &str) -> bool {
    if CONTEXT_OVERFLOW_CODES
        .iter()
        .any(|known| code.contains(known))
    {
        return true;
    }
    let haystack = message.to_ascii_lowercase();
    CONTEXT_OVERFLOW_NEEDLES
        .iter()
        .any(|needle| haystack.contains(needle))
}

/// 请求被拒（HTTP 状态码 + 错误体）时判定是否属于上下文超限。
///
/// 只看 4xx：5xx 是上游自己的问题，跟用户塞了多少上下文无关。
pub fn is_context_overflow(status: u16, message: &str, upstream: &Option<Value>) -> bool {
    if !(400..500).contains(&status) {
        return false;
    }
    let code = upstream
        .as_ref()
        .map(upstream_error_code)
        .unwrap_or_default();
    context_overflow_signal(message, &code)
}

/// 流已建立、上游中途塞进一个 `error` 帧时的分类。
///
/// 这类帧此前会被静默忽略：`choices` 为空、`error` 又不是已知字段，于是整条流
/// 表现为「没有内容地正常结束」——用户看到的是一句半截回答，日志里什么都没有。
pub fn classify_stream_error(payload: &Value) -> LlmError {
    let message = upstream_message(payload).unwrap_or_else(|| truncate(&payload.to_string(), 400));
    let code = upstream_error_code(payload);
    let upstream = Some(payload.clone());

    if context_overflow_signal(&message, &code) {
        return LlmError::ContextOverflow { message, upstream };
    }
    if ["rate_limit", "too_many_requests", "overloaded"]
        .iter()
        .any(|known| code.contains(known))
    {
        return LlmError::RateLimited {
            status: 429,
            message,
            retry_after_ms: None,
            upstream: Some(payload.clone()),
        };
    }
    if ["auth", "api_key", "permission", "insufficient_quota"]
        .iter()
        .any(|known| code.contains(known))
    {
        return LlmError::AuthFailed {
            status: 401,
            message,
            upstream,
        };
    }
    LlmError::Unavailable {
        message: format!("上游在流中报错：{message}"),
        status: None,
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

    #[test]
    fn context_overflow_is_recognised_across_vendor_wording() {
        // 各家的措辞：OpenAI 用 code，Anthropic/Kimi/DeepSeek 把话写在 message 里。
        let cases: Vec<(u16, &str, Option<Value>)> = vec![
            (
                400,
                "This model's maximum context length is 8192 tokens",
                None,
            ),
            (
                400,
                "prompt is too long: 210000 tokens > 200000 maximum",
                None,
            ),
            (
                400,
                "Invalid request: Your request exceeded model token limit: 262144",
                None,
            ),
            (
                400,
                "",
                Some(serde_json::json!({ "error": { "code": "context_length_exceeded" } })),
            ),
            (400, "上下文长度超出限制", None),
            (
                422,
                "reduce the length of the messages",
                Some(serde_json::json!({ "error": { "type": "invalid_request_error" } })),
            ),
        ];
        for (status, message, upstream) in cases {
            assert!(
                is_context_overflow(status, message, &upstream),
                "应判定为上下文超限：{message}"
            );
        }
    }

    #[test]
    fn ordinary_bad_requests_are_not_mistaken_for_overflow() {
        // 400 里更多是「模型名写错 / 参数不合法」：归错类会让 UI 让用户去裁剪上下文。
        for message in [
            "model `gpt-5` does not exist",
            "invalid temperature: 3.0, must be <= 2.0",
            "tool_choice is required when tools are provided",
        ] {
            assert!(
                !is_context_overflow(400, message, &None),
                "不该判成超限：{message}"
            );
        }
        // 5xx 是上游自己的问题，跟用户塞了多少上下文无关。
        assert!(!is_context_overflow(
            500,
            "maximum context length exceeded",
            &None
        ));
    }

    #[test]
    fn stream_error_frame_folds_into_the_same_codes_as_http_errors() {
        let overflow = classify_stream_error(&serde_json::json!({
            "code": "context_length_exceeded",
            "message": "This model's maximum context length is 65536 tokens",
        }));
        assert_eq!(overflow.code(), ErrorCode::CONTEXT_OVERFLOW);
        assert!(
            overflow.to_error_object().data.is_some(),
            "原始错误体要带给 UI，否则「超了多少」无从判断"
        );

        let rate_limited = classify_stream_error(&serde_json::json!({
            "type": "rate_limit_exceeded",
            "message": "slow down",
        }));
        assert_eq!(rate_limited.code(), ErrorCode::PROVIDER_RATE_LIMITED);

        let auth = classify_stream_error(&serde_json::json!({
            "code": "invalid_api_key",
            "message": "Incorrect API key provided",
        }));
        assert_eq!(auth.code(), ErrorCode::PROVIDER_AUTH_FAILED);

        let other = classify_stream_error(&serde_json::json!({
            "type": "server_error",
            "message": "internal error, please retry",
        }));
        assert_eq!(other.code(), ErrorCode::PROVIDER_UNAVAILABLE);
        assert!(
            other.to_string().contains("流中报错"),
            "至少要让人看出这条是流中途来的：{other}"
        );
    }
}
