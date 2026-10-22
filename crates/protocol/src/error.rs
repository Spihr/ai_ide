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
//! | `5xxx` | 会话持久化 |
//! | `6xxx` | 终端会话 |

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
    /// 上游不可达、超时或返回了无法解释的响应体（含本地未启用 HTTP 传输的骨架态）。
    pub const PROVIDER_UNAVAILABLE: Self = Self(2005);

    // ---------- 3xxx 工具 ----------
    pub const TOOL_NOT_ALLOWED: Self = Self(3001);
    pub const TOOL_TIMEOUT: Self = Self(3002);
    pub const DIFF_APPLY_CONFLICT: Self = Self(3003);

    // ---------- 4xxx 运行时 ----------
    pub const CANCELLED: Self = Self(4001);
    pub const PROTOCOL_VERSION_MISMATCH: Self = Self(4002);

    // ---------- 5xxx 会话持久化 ----------
    /// 会话不存在：读取 / 追加消息 / 改名 / 删除都可能命中。
    pub const SESSION_NOT_FOUND: Self = Self(5001);
    /// 会话库未就绪：迁移失败或落盘目录不可写。
    pub const STORE_UNAVAILABLE: Self = Self(5002);
    /// 会话库读写失败（SQLite 或 JSON 序列化报错）。
    pub const STORE_ERROR: Self = Self(5003);
    /// 导出落盘失败：路径不可写、父目录建不出来、磁盘满等文件系统侧原因。
    pub const EXPORT_FAILED: Self = Self(5004);
    /// 会话过大：导出 / 分叉单次只处理 10000 条消息，超过就明确报错而不是静默截断。
    pub const SESSION_TOO_LARGE: Self = Self(5005);
    /// 检查点不存在：回滚 / 删除都会命中。
    pub const CHECKPOINT_NOT_FOUND: Self = Self(5006);
    /// 检查点元数据损坏或回滚后回读校验不一致。
    pub const CHECKPOINT_CORRUPT: Self = Self(5007);

    // ---------- 6xxx 终端 ----------
    /// 终端会话不存在：已经退出、或被 `terminal.kill` 收掉了。
    pub const TERMINAL_NOT_FOUND: Self = Self(6001);
    /// PTY 起不来：shell 不存在、cwd 不可用、ConPTY 建不出来。
    pub const TERMINAL_SPAWN_FAILED: Self = Self(6002);
    /// 会话在但这次操作失败：写管道被对端关闭、resize 被拒。
    pub const TERMINAL_IO_ERROR: Self = Self(6003);
    /// 参数不可用：`data` 不是合法 base64、尺寸无法夹取等。
    pub const TERMINAL_INVALID: Self = Self(6004);

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

    /// 5xxx 是业务码，不能被误判成 JSON-RPC 标准错误（否则客户端会按规范错误处理）。
    #[test]
    fn session_error_codes_are_not_standard() {
        assert!(!ErrorCode::SESSION_NOT_FOUND.is_standard());
        assert!(!ErrorCode::STORE_UNAVAILABLE.is_standard());
        assert!(!ErrorCode::STORE_ERROR.is_standard());
        assert_eq!(ErrorCode::SESSION_NOT_FOUND.as_i32(), 5001);
    }
}
