//! HTTP(SSE) 端到端测试：内置回环桩，不连外网。
//!
//! 桩只做三件事：收一行 HTTP 请求、按方法回 JSON-RPC 响应、按模式决定用
//! `application/json` 还是 `text/event-stream` 回包。它刻意校验会话头——
//! 因为「Server 发了 `Mcp-Session-Id` 但客户端后续请求没带上」是很典型的集成 bug，
//! 用真实 socket 才测得到。

use std::collections::BTreeMap;
use std::time::Duration;

use ai_ide_mcp::http_transport::SESSION_HEADER;
use ai_ide_mcp::{ErrorClass, HttpServerConfig, McpClient, McpServerConfig};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// 回包形态。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// `Content-Type: application/json`，整包一个响应。
    Json,
    /// `Content-Type: text/event-stream`，一帧一个响应。
    Sse,
    /// 直接 500。
    ServerError,
}

const STUB_SESSION: &str = "stub-session-1";

/// 起一个回环 HTTP 桩，返回 URL。
async fn spawn_stub(mode: Mode) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定回环端口");
    let addr = listener.local_addr().expect("本地地址");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(handle(stream, mode));
        }
    });
    format!("http://{addr}/mcp")
}

fn config(name: &str, url: &str, declared_sse: bool) -> McpServerConfig {
    McpServerConfig::Http(HttpServerConfig {
        name: name.to_string(),
        url: url.to_string(),
        headers: BTreeMap::new(),
        declared_sse,
    })
}

async fn handle(mut stream: TcpStream, mode: Mode) {
    let Some((request_line, headers, body)) = read_request(&mut stream).await else {
        return;
    };
    if request_line.starts_with("DELETE") {
        // 关闭会话：规范里的收尾动作，客户端不该因此报错。
        write_response(&mut stream, "200 OK", "application/json", "", &[]).await;
        return;
    }

    let request: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let id = request.get("id").cloned();

    let has_session = header_value(&headers, SESSION_HEADER).is_some();
    if method != "initialize" && !has_session {
        write_response(
            &mut stream,
            "400 Bad Request",
            "application/json",
            "{\"error\":\"missing mcp-session-id\"}",
            &[],
        )
        .await;
        return;
    }

    let Some(id) = id else {
        // 通知：202 + 空体。
        write_response(&mut stream, "202 Accepted", "application/json", "", &[]).await;
        return;
    };

    if mode == Mode::ServerError {
        write_response(
            &mut stream,
            "500 Internal Server Error",
            "text/plain",
            "stub 内部错误",
            &[],
        )
        .await;
        return;
    }

    let response = match method.as_str() {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "stub", "version": "0.1.0"}
            }
        }),
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"tools": [{"name": "echo"}, {"name": "fail"}]}
        }),
        "tools/call" => {
            let tool = request
                .get("params")
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if tool == "fail" {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32000, "message": "stub 刻意失败"}
                })
            } else {
                let arguments = request
                    .get("params")
                    .and_then(|params| params.get("arguments"))
                    .cloned()
                    .unwrap_or(Value::Null);
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {"content": [{"type": "text", "text": format!("stub echo {arguments}")}]}
                })
            }
        }
        other => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": format!("未知方法 {other}")}
        }),
    };

    // 只有 initialize 需要发会话 id（后续请求都要求带上它）。
    let extra: Vec<(&str, String)> = if method == "initialize" {
        vec![(SESSION_HEADER, STUB_SESSION.to_string())]
    } else {
        Vec::new()
    };

    match mode {
        Mode::Json => {
            let body = response.to_string();
            write_response(&mut stream, "200 OK", "application/json", &body, &extra).await;
        }
        Mode::Sse => {
            // 故意把一帧拆成两段写，验证解码器能跨 chunk 组帧。
            let payload = format!("event: message\ndata: {response}\n\n");
            // 在字符边界上切，避免把多字节汉字切成两半。
            let mut split = payload.len() / 2;
            while !payload.is_char_boundary(split) {
                split -= 1;
            }
            let (head, tail) = payload.split_at(split);
            let header = format!(
                "{}\r\n",
                response_head("200 OK", "text/event-stream", &extra)
            );
            let _ = stream.write_all(header.as_bytes()).await;
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.flush().await;
            let _ = stream.write_all(tail.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
        Mode::ServerError => {}
    }
}

/// 状态行 + 各头（每行以 CRLF 结尾），**不含**结束头块的空行。
fn response_head(status: &str, content_type: &str, extra: &[(&str, String)]) -> String {
    let mut head =
        format!("HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\nconnection: close\r\n");
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head
}

async fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
    extra: &[(&str, String)],
) {
    let mut head = response_head(status, content_type, extra);
    head.push_str(&format!("content-length: {}\r\n\r\n", body.len()));
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn read_request(stream: &mut TcpStream) -> Option<(String, String, String)> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut header_end: Option<usize> = None;
    loop {
        if let Some(end) = header_end {
            let headers = String::from_utf8_lossy(&buffer[..end]).to_string();
            let needed = end + content_length(&headers);
            if buffer.len() >= needed {
                let body = String::from_utf8_lossy(&buffer[end..needed]).to_string();
                let request_line = headers
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                return Some((request_line, headers, body));
            }
        }
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if header_end.is_none() {
            header_end = find_subslice(&buffer, b"\r\n\r\n").map(|pos| pos + 4);
        }
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn content_length(headers: &str) -> usize {
    header_value(headers, "content-length")
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

/// 从头块里取某个头的值（大小写不敏感）。
fn header_value(headers: &str, name: &str) -> Option<String> {
    headers.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if key.trim().eq_ignore_ascii_case(name) {
            Some(value.trim().to_string())
        } else {
            None
        }
    })
}

#[tokio::test]
async fn json_transport_completes_handshake_list_and_call() {
    let url = spawn_stub(Mode::Json).await;
    let client = tokio::time::timeout(
        Duration::from_secs(10),
        McpClient::connect(&config("stub", &url, false)),
    )
    .await
    .expect("连接不得挂住")
    .expect("握手应成功");

    assert_eq!(client.protocol_version().as_deref(), Some("2025-06-18"));
    // 第二条请求能成功，说明 `Mcp-Session-Id` 被正确带上（桩缺头会回 400）。
    let tools = client.list_tools().await.expect("tools/list");
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, vec!["echo", "fail"]);

    let result = client
        .call_tool("echo", json!({"text": "hi"}))
        .await
        .expect("调用成功");
    assert!(result.text().contains("stub echo"), "{}", result.text());
    client.close().await;
}

#[tokio::test]
async fn sse_transport_parses_event_stream() {
    let url = spawn_stub(Mode::Sse).await;
    let client = tokio::time::timeout(
        Duration::from_secs(10),
        McpClient::connect(&config("stub-sse", &url, true)),
    )
    .await
    .expect("连接不得挂住")
    .expect("握手应成功");

    let tools = client.list_tools().await.expect("tools/list");
    assert_eq!(tools.len(), 2);
    let result = client
        .call_tool("echo", json!({"text": "sse"}))
        .await
        .expect("调用成功");
    assert!(result.text().contains("stub echo"), "{}", result.text());
    client.close().await;
}

#[tokio::test]
async fn rpc_error_over_http_is_classified_as_rpc() {
    let url = spawn_stub(Mode::Json).await;
    let client = McpClient::connect(&config("stub", &url, false))
        .await
        .expect("握手应成功");
    let err = client
        .call_tool("fail", json!({}))
        .await
        .expect_err("必须失败");
    assert_eq!(err.class(), ErrorClass::Rpc);
    client.close().await;
}

#[tokio::test]
async fn http_500_is_classified_as_http_error() {
    let url = spawn_stub(Mode::ServerError).await;
    let err = McpClient::connect(&config("stub", &url, false))
        .await
        .expect_err("500 必须让握手失败");
    assert_eq!(err.class(), ErrorClass::Http);
    assert!(err.to_string().contains("500"), "{err}");
}
