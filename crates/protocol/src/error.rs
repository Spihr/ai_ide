//! JSON-RPC 2.0 错误对象与错误码表。
//!
//! 错误码分区（见 `docs/03-内核接口协议规范.md` §1.3）：
//!
//! | 区间 | 归属 |
//! |---|---|
//! | `-327xx` | JSON-RPC 2.0 标准错误 |
//! | `1xxx` | 工作区与路径 |
//! | `2xxx` | Provider / 模型 |
//! | `3xxx` | 工具与 Agent 执行 |
//! | `4xxx` | 运行时（取消等） |

use serde::{Deserialize, Serialize};
use std::fmt;
use ts_rs::TS;

/// 错误码。使用 newtype 包住 `i32`，既保住类型安全，又按数字序列化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export)]
pub struct ErrorCode(pub i32);

impl ErrorCode {
    // ---------- JSON-RPC 2.0 标准 ----------
    pub const PARSE_ERROR: Self = Self(-32700);
    pub const INVALID_REQUEST: Self = Self(-32600);
    pub const METHOD_NOT_FOUND: Self = Self(-32601);
    pub const INVALID_PARAMS: Self = Self(-32602);
    pub const INTERNAL_ERROR: Self = Self(-32603);

    // ---------- 1xxx 工作区 ----------
    pub const WORKSPACE_NOT_OPENED: Self = Self(1001);
    pub const PATH_OUT_OF_WORKSPACE: Self = Self(1002);
    pub const PATH_DENIED: Self = Self(1003);
    pub const FILE_NOT_FOUND: Self = Self(1004);
    pub const IO_ERROR: Self = Self(1005);
    pub const INVALID_PATH: Self = Self(1006);

    // ---------- 2xxx 模型 ----------
    pub const PROVIDER_NOT_CONFIGURED: Self = Self(2001);
    pub const PROVIDER_AUTH_FAILED: Self = Self(2002);
    pub const PROVIDER_RATE_LIMITED: Self = Self(2003);
    pub const CONTEXT_OVERFLOW: Self = Self(2004);

    // ---------- 3xxx 工具 ----------
    pub const TOOL_NOT_ALLOWED: Self = Self(3001);
    pub const TOOL_TIMEOUT: Self = Self(3002);
    pub const DIFF_APPLY_CONFLICT: Self = Self(3003);

    // ---------- 4xxx 运行时 ----------
    pub const CANCELLED: Self = Self(4001);
    pub const PROTOCOL_VERSION_MISMATCH: Self = Self(4002);

    pub const fn as_i32(self) -> i32 {
        self.0
    }

    /// 是否为 JSON-RPC 标准错误码。
    pub const fn is_standard(self) -> bool {
        self.0 <= -32000
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<i32> for ErrorCode {
    fn from(value: i32) -> Self {
        Self(value)
    }
}

impl From<ErrorCode> for i32 {
    fn from(value: ErrorCode) -> Self {
        value.0
    }
}

/// JSON-RPC 错误对象。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ErrorObject {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub data: Option<serde_json::Value>,
}

impl ErrorObject {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// 附加结构化数据。协议约定常用字段：
    /// - `upstream`：上游模型的原始错误体
    /// - `retry_after_ms`：限流建议等待时长
    /// - `path` / `tokens`：路径越界、上下文超限的上下文信息
    #[must_use]
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::INVALID_PARAMS, message)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            ErrorCode::METHOD_NOT_FOUND,
            format!("未实现的方法: {method}"),
        )
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::INTERNAL_ERROR, message)
    }
}

impl fmt::Display for ErrorObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for ErrorObject {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_serializes_as_number() {
        let json = serde_json::to_string(&ErrorCode::METHOD_NOT_FOUND).unwrap();
        assert_eq!(json, "-32601");
    }

    #[test]
    fn error_object_omits_none_data() {
        let err = ErrorObject::new(ErrorCode::CANCELLED, "cancelled");
        let json = serde_json::to_string(&err).unwrap();
        assert!(!json.contains("data"), "None 的 data 不应被序列化: {json}");
    }

    #[test]
    fn error_object_keeps_data_when_present() {
        let err = ErrorObject::new(ErrorCode::PATH_OUT_OF_WORKSPACE, "越界")
            .with_data(serde_json::json!({ "path": "..\\..\\Windows" }));
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("Windows"));
    }

    #[test]
    fn standard_code_detection() {
        assert!(ErrorCode::PARSE_ERROR.is_standard());
        assert!(!ErrorCode::CANCELLED.is_standard());
    }
}
