//! JSON-RPC 2.0 报文编解码（LSP 方言）。
//!
//! LSP 的报文就是 JSON-RPC 2.0，和 MCP 一样；差别在**分帧**：MCP 走「一行一帧」，
//! LSP 走 `Content-Length` 头 + 原样消息体（见 [`crate::framing`]）。
//! 所以这一层只负责「一个完整 JSON 值 ↔ 三种报文形态」的转换，不碰字节边界。
//!
//! 三件事：
//!
//! 1. 把出站请求/通知/响应编成**单行** JSON——虽然分帧靠长度而非换行，但服务器
//!    日志与抓包时单行更好读，也顺手排除了裸换行被某些实现误判的可能；
//! 2. 把入站文本判成 [`Incoming`] 的三种形态：响应 / 通知 / 服务器发起的请求；
//! 3. 对不合规的帧给出明确的 [`LspError::Protocol`]，绝不静默丢弃
//!    ——静默丢弃会让调用方干等到超时，错误还指错方向。
//!
//! ## 为什么 id 是枚举而不是 `Value`
//!
//! 规范允许 number / string / null 三种 id。用 [`RequestId`] 把「我们只发数字 id，
//! 但必须能读懂服务器发的字符串 id」写进类型里，匹配时不会踩 `1` 与 `1.0` 不同值的坑。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::LspError;

/// 协议版本字符串。LSP 只认 `"2.0"`。
pub const JSONRPC_VERSION: &str = "2.0";

/// 解析失败（入站消息体不是合法 JSON）。
pub const PARSE_ERROR: i64 = -32700;
/// 请求对象不合法（缺字段、类型不对）。
pub const INVALID_REQUEST: i64 = -32600;
/// 方法不存在（本客户端收到未知的服务器反向请求时回这个）。
pub const METHOD_NOT_FOUND: i64 = -32601;
/// 参数不合法。
pub const INVALID_PARAMS: i64 = -32602;
/// 服务器内部错误。
pub const INTERNAL_ERROR: i64 = -32603;

/// JSON-RPC 请求 id。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// 数字 id（本客户端发出的一律是它，递增不重用）。
    Number(i64),
    /// 字符串 id（部分服务器在反向请求里会用它）。
    String(String),
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(value) => write!(f, "{value}"),
            Self::String(value) => write!(f, "\"{value}\""),
        }
    }
}

/// 出站请求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// 恒为 `"2.0"`。
    pub jsonrpc: String,
    /// 请求 id。
    pub id: RequestId,
    /// 方法名（`initialize` / `textDocument/definition` …）。
    pub method: String,
    /// 参数；无参数时省略字段（不是 `null`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    /// 构造一条请求。
    pub fn new(id: RequestId, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            method: method.into(),
            params,
        }
    }
}

/// 出站通知（没有 id，服务器不回响应）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    /// 恒为 `"2.0"`。
    pub jsonrpc: String,
    /// 方法名。
    pub method: String,
    /// 参数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcNotification {
    /// 构造一条通知。
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params,
        }
    }
}

/// JSON-RPC error 对象。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// 错误码。
    pub code: i64,
    /// 错误消息。
    pub message: String,
    /// 附加数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// 入站响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// 恒为 `"2.0"`。
    pub jsonrpc: String,
    /// 对应的请求 id；服务器解析失败时为 `null`（`None`）。
    #[serde(default)]
    pub id: Option<RequestId>,
    /// 成功结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// 失败原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    /// 成功响应。
    pub fn success(id: RequestId, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }

    /// 失败响应（也用于回服务器发起的请求）。
    pub fn failure(id: Option<RequestId>, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    /// 把 `result` / `error` 压成 `Result`。
    pub fn into_result(self) -> Result<Value, LspError> {
        match (self.result, self.error) {
            (_, Some(err)) => Err(LspError::Rpc {
                code: err.code,
                message: err.message,
                data: err.data,
            }),
            (Some(result), None) => Ok(result),
            // 两个都缺：规范不允许，但真实服务器偶尔给 `{}`（例如 `shutdown` 的回包）。
            // 当成空结果，比在这里报协议错更宽容。
            (None, None) => Ok(Value::Null),
        }
    }
}

/// 一帧入站消息的三种形态。
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// 服务器对某个请求的响应。
    Response(JsonRpcResponse),
    /// 服务器主动发的通知（`textDocument/publishDiagnostics` 等）。
    Notification {
        /// 方法名。
        method: String,
        /// 参数。
        params: Option<Value>,
    },
    /// 服务器反向发起的请求（`client/registerCapability` 等）。
    Request(JsonRpcRequest),
}

/// 把请求编成一行 JSON（不含换行符）。
pub fn encode_request(request: &JsonRpcRequest) -> Result<String, LspError> {
    encode_value(request)
}

/// 把通知编成一行 JSON（不含换行符）。
pub fn encode_notification(notification: &JsonRpcNotification) -> Result<String, LspError> {
    encode_value(notification)
}

/// 把响应编成一行 JSON（不含换行符）。
pub fn encode_response(response: &JsonRpcResponse) -> Result<String, LspError> {
    encode_value(response)
}

fn encode_value<T: Serialize>(value: &T) -> Result<String, LspError> {
    serde_json::to_string(value).map_err(|err| LspError::protocol(format!("JSON 编码失败：{err}")))
}

/// 解析一段入站消息体。
pub fn decode_text(text: &str) -> Result<Incoming, LspError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(LspError::protocol("收到空消息体"));
    }
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|err| LspError::protocol(format!("JSON 解析失败：{err}")))?;
    decode_value(value)
}

/// 解析一个已解出的 JSON 值（分帧层拿到消息体后统一走这里）。
pub fn decode_value(value: Value) -> Result<Incoming, LspError> {
    let object = value
        .as_object()
        .ok_or_else(|| LspError::protocol("JSON-RPC 帧必须是对象"))?;
    match object.get("jsonrpc").and_then(Value::as_str) {
        Some(JSONRPC_VERSION) => {}
        other => {
            return Err(LspError::protocol(format!(
                "jsonrpc 字段必须是 \"2.0\"，实际为 {other:?}"
            )))
        }
    }

    if object.contains_key("method") {
        let method = object
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| LspError::protocol("method 必须是字符串"))?;
        // 有非 null 的 id 才是请求；`id: null` 按通知处理（规范要求通知不带 id，
        // 但个别实现会显式给 null）。
        let has_id = object.get("id").map(|id| !id.is_null()).unwrap_or(false);
        if has_id {
            let request: JsonRpcRequest = serde_json::from_value(value)
                .map_err(|err| LspError::protocol(format!("请求帧字段非法：{err}")))?;
            return Ok(Incoming::Request(request));
        }
        return Ok(Incoming::Notification {
            method: method.to_string(),
            params: object.get("params").cloned(),
        });
    }

    if !object.contains_key("id") && !object.contains_key("error") {
        return Err(LspError::protocol(
            "既没有 method 也没有 id/error，不构成合法 JSON-RPC 帧",
        ));
    }
    let response: JsonRpcResponse = serde_json::from_value(value)
        .map_err(|err| LspError::protocol(format!("响应帧字段非法：{err}")))?;
    Ok(Incoming::Response(response))
}

/// 构造一条错误响应（回服务器发起的请求时用）。
pub fn error_response(id: Option<RequestId>, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": JSONRPC_VERSION,
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_encoding_omits_absent_params() {
        let request = JsonRpcRequest::new(RequestId::Number(1), "shutdown", None);
        let text = encode_request(&request).expect("encode");
        assert_eq!(text, r#"{"jsonrpc":"2.0","id":1,"method":"shutdown"}"#);
    }

    #[test]
    fn encode_never_emits_newlines() {
        let request = JsonRpcRequest::new(
            RequestId::Number(1),
            "textDocument/hover",
            Some(json!({"text": "第一行\n第二行"})),
        );
        let text = encode_request(&request).expect("encode");
        assert!(!text.contains('\n'), "帧内不能有裸换行：{text}");
        assert!(text.contains("\\n"), "{text}");
    }

    #[test]
    fn notification_has_no_id_field() {
        let note = JsonRpcNotification::new("initialized", Some(json!({})));
        let text = encode_notification(&note).expect("encode");
        assert!(!text.contains("\"id\""), "{text}");
        assert!(text.contains("initialized"), "{text}");
    }

    #[test]
    fn response_decoding_success_result() {
        let incoming = decode_text(r#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#).expect("ok");
        match incoming {
            Incoming::Response(response) => {
                assert_eq!(response.id, Some(RequestId::Number(7)));
                let result = response.into_result().expect("result");
                assert_eq!(result["ok"], json!(true));
            }
            other => panic!("期望响应，得到 {other:?}"),
        }
    }

    #[test]
    fn response_decoding_error_becomes_rpc_error() {
        let incoming = decode_text(
            r#"{"jsonrpc":"2.0","id":"abc","error":{"code":-32601,"message":"no such method"}}"#,
        )
        .expect("ok");
        match incoming {
            Incoming::Response(response) => {
                assert_eq!(response.id, Some(RequestId::String("abc".into())));
                let err = response.into_result().expect_err("error");
                assert_eq!(
                    err,
                    LspError::Rpc {
                        code: -32601,
                        message: "no such method".into(),
                        data: None
                    }
                );
            }
            other => panic!("期望响应，得到 {other:?}"),
        }
    }

    #[test]
    fn notification_is_recognized_without_id() {
        let incoming = decode_text(
            r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///a.ts"}}"#,
        )
        .expect("ok");
        assert_eq!(
            incoming,
            Incoming::Notification {
                method: "textDocument/publishDiagnostics".into(),
                params: Some(json!({"uri": "file:///a.ts"}))
            }
        );
    }

    #[test]
    fn explicit_null_id_is_treated_as_notification() {
        let incoming =
            decode_text(r#"{"jsonrpc":"2.0","id":null,"method":"window/logMessage"}"#).expect("ok");
        assert!(matches!(incoming, Incoming::Notification { .. }));
    }

    #[test]
    fn server_request_keeps_id() {
        let incoming =
            decode_text(r#"{"jsonrpc":"2.0","id":"srv-1","method":"workspace/configuration"}"#)
                .expect("ok");
        match incoming {
            Incoming::Request(request) => {
                assert_eq!(request.method, "workspace/configuration");
                assert_eq!(request.id, RequestId::String("srv-1".into()));
            }
            other => panic!("期望请求，得到 {other:?}"),
        }
    }

    #[test]
    fn wrong_version_is_a_protocol_error() {
        let err = decode_text(r#"{"jsonrpc":"1.0","id":1,"result":null}"#).expect_err("must fail");
        assert_eq!(err.class(), crate::error::ErrorClass::Protocol);
        assert!(err.to_string().contains("2.0"), "{err}");
    }

    #[test]
    fn malformed_json_is_a_protocol_error() {
        let err = decode_text("{not json").expect_err("must fail");
        assert!(matches!(err, LspError::Protocol(_)));
    }

    #[test]
    fn frame_without_method_or_id_is_rejected() {
        let err = decode_text(r#"{"jsonrpc":"2.0","foo":1}"#).expect_err("must fail");
        assert!(matches!(err, LspError::Protocol(_)), "{err}");
    }

    #[test]
    fn non_object_frame_is_rejected() {
        let err = decode_text("[1,2,3]").expect_err("must fail");
        assert!(matches!(err, LspError::Protocol(_)));
    }

    #[test]
    fn empty_frame_is_rejected() {
        assert!(decode_text("   ").is_err());
    }

    #[test]
    fn error_response_shape_is_serializable() {
        let value = error_response(
            Some(RequestId::String("x".into())),
            METHOD_NOT_FOUND,
            "unsupported",
        );
        assert_eq!(value["error"]["code"], json!(METHOD_NOT_FOUND));
        assert_eq!(value["id"], json!("x"));
    }
}
