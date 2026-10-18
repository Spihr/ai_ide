//! JSON-RPC 2.0 帧结构与编解码。
//!
//! 传输层为 NDJSON：一条消息占一行，UTF-8 编码，以 `\n` 结尾。
//! 由于 `serde_json` 的紧凑序列化会把消息体内的换行转义为 `\n`，
//! 因此任何合法报文都不会破坏「一行一消息」的帧边界。

use crate::error::{ErrorCode, ErrorObject};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// JSON-RPC 版本字面量。
pub const JSONRPC_VERSION: &str = "2.0";

const TS_DIR: &str = "../../packages/shared-types/src/generated/";

// ---------------------------------------------------------------------------
// 帧定义
// ---------------------------------------------------------------------------

/// 请求帧：需要对方返回结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Request {
    /// 恒为 `"2.0"`。
    pub jsonrpc: String,
    /// 字符串形式的请求标识（本协议约定为 UUID v4）。
    pub id: String,
    /// 方法名，见 [`crate::methods`]。
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub params: Option<Value>,
}

/// 通知帧：不需要对方返回结果（Core → Main 的事件走这里）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub params: Option<Value>,
}

/// 成功响应帧。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Response {
    pub jsonrpc: String,
    pub id: String,
    pub result: Value,
}

/// 失败响应帧。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ErrorResponse {
    pub jsonrpc: String,
    pub id: String,
    pub error: ErrorObject,
}

impl Request {
    pub fn new(id: impl Into<String>, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.into(),
            method: method.into(),
            params,
        }
    }
}

impl Notification {
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params,
        }
    }
}

impl Response {
    pub fn new(id: impl Into<String>, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.into(),
            result,
        }
    }
}

impl ErrorResponse {
    pub fn new(id: impl Into<String>, error: ErrorObject) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.into(),
            error,
        }
    }
}

// ---------------------------------------------------------------------------
// 入站消息
// ---------------------------------------------------------------------------

/// Core 视角能收到的两种入站消息。
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Request(Request),
    Notification(Notification),
}

impl Incoming {
    /// 取出请求标识；通知返回 [`None`]。
    pub fn id(&self) -> Option<&str> {
        match self {
            Incoming::Request(req) => Some(&req.id),
            Incoming::Notification(_) => None,
        }
    }

    pub fn method(&self) -> &str {
        match self {
            Incoming::Request(req) => &req.method,
            Incoming::Notification(n) => &n.method,
        }
    }
}

// ---------------------------------------------------------------------------
// 编解码
// ---------------------------------------------------------------------------

/// 把任意可序列化值编码为一行 NDJSON（含结尾换行）。
pub fn encode<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    Ok(line)
}

/// 同 [`encode`]，但不追加换行，便于测试与日志。
pub fn encode_to_string<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(value)
}

/// 解码一行 NDJSON 为入站消息。
///
/// 手写而非 derive，是为了在报文畸形时给出**结构化**的错误码，
/// 而不是 serde 那种 `data did not match any variant` 的黑盒信息。
pub fn decode(line: &str) -> Result<Incoming, ErrorObject> {
    let value: Value = serde_json::from_str(line)
        .map_err(|e| ErrorObject::new(ErrorCode::PARSE_ERROR, format!("JSON 解析失败: {e}")))?;
    decode_value(value)
}

/// 见 [`decode`]，接受已解析的 JSON 值。
pub fn decode_value(value: Value) -> Result<Incoming, ErrorObject> {
    let object = value
        .as_object()
        .ok_or_else(|| ErrorObject::new(ErrorCode::INVALID_REQUEST, "报文必须是 JSON 对象"))?;

    match object.get("jsonrpc").and_then(Value::as_str) {
        Some(version) if version == JSONRPC_VERSION => {}
        Some(other) => {
            return Err(ErrorObject::new(
                ErrorCode::INVALID_REQUEST,
                format!("jsonrpc 字段必须是 \"{JSONRPC_VERSION}\"，收到 \"{other}\""),
            ))
        }
        None => {
            return Err(ErrorObject::new(
                ErrorCode::INVALID_REQUEST,
                format!("缺少 jsonrpc 字段（应为 \"{JSONRPC_VERSION}\"）"),
            ))
        }
    }

    let method = object
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| ErrorObject::new(ErrorCode::INVALID_REQUEST, "缺少 method 字段"))?
        .to_string();

    let params = object.get("params").cloned();

    match object.get("id") {
        Some(id_value) => {
            let id = id_value.as_str().ok_or_else(|| {
                ErrorObject::new(
                    ErrorCode::INVALID_REQUEST,
                    "id 必须是字符串（本协议约定为 UUID v4）",
                )
            })?;
            Ok(Incoming::Request(Request {
                jsonrpc: JSONRPC_VERSION.to_string(),
                id: id.to_string(),
                method,
                params,
            }))
        }
        None => Ok(Incoming::Notification(Notification {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method,
            params,
        })),
    }
}

/// 供 TS 侧引用的导出目录说明（占位，不参与序列化）。
pub const _TS_EXPORT_DIR: &str = TS_DIR;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn encode_produces_single_line() {
        let req = Request::new("1", "core.ping", Some(json!({})));
        let line = encode(&req).unwrap();
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
    }

    #[test]
    fn decode_request() {
        let line = r#"{"jsonrpc":"2.0","id":"1","method":"core.ping","params":{}}"#;
        let incoming = decode(line).unwrap();
        assert_eq!(incoming.id(), Some("1"));
        assert_eq!(incoming.method(), "core.ping");
    }

    #[test]
    fn decode_notification_has_no_id() {
        let line = r#"{"jsonrpc":"2.0","method":"core.log","params":{"level":"info"}}"#;
        let incoming = decode(line).unwrap();
        assert!(matches!(incoming, Incoming::Notification(_)));
        assert_eq!(incoming.id(), None);
    }

    #[test]
    fn malformed_json_yields_parse_error() {
        let err = decode("{not json").unwrap_err();
        assert_eq!(err.code, ErrorCode::PARSE_ERROR);
    }

    #[test]
    fn missing_jsonrpc_yields_invalid_request() {
        let err = decode(r#"{"id":"1","method":"core.ping"}"#).unwrap_err();
        assert_eq!(err.code, ErrorCode::INVALID_REQUEST);
    }

    #[test]
    fn numeric_id_is_rejected() {
        let err = decode(r#"{"jsonrpc":"2.0","id":1,"method":"core.ping"}"#).unwrap_err();
        assert_eq!(err.code, ErrorCode::INVALID_REQUEST);
    }

    #[test]
    fn params_omitted_when_none() {
        let req = Request::new("1", "core.ping", None);
        let line = encode_to_string(&req).unwrap();
        assert!(
            !line.contains("params"),
            "params 为 None 时不应出现: {line}"
        );
    }

    #[test]
    fn newline_inside_message_is_escaped() {
        let notif = Notification::new("core.log", Some(json!({ "message": "a\nb" })));
        let line = encode(&notif).unwrap();
        // 报文体内的换行被转义，整帧仍只有结尾那一个真实换行
        assert_eq!(line.matches('\n').count(), 1);
    }
}
