//! 最小假语言服务器（仅测试用）。
//!
//! 存在的理由：LSP 客户端最容易出错的不是「happy path 能不能通」，而是
//! **帧边界、id 错配、进程半路没了、超时后迟到响应**这些异常路径。
//! 拿真服务器（`rust-analyzer`）测这些既慢又不稳定（不能让它崩、不能让它慢，
//! 更没法让它故意发非法帧），所以这里写一个可以「按要求作恶」的假服务器：
//!
//! | 方法 | 行为 |
//! |---|---|
//! | `initialize` | 回 `textDocumentSync: 1`（Full）+ `serverInfo` |
//! | `initialized` | 记下「已初始化」 |
//! | `shutdown` | 回 `null` |
//! | `exit` | 直接退出（码 0） |
//! | `mock/echo` | 原样回 `params` |
//! | `mock/state` | 回当前文档表与握手状态（验证 didOpen/didChange/didClose） |
//! | `mock/diagnostics` | 先推一条 `textDocument/publishDiagnostics` 通知，再回 `null` |
//! | `mock/sleep` | 睡 `params.ms` 毫秒后再回 `null`（测超时） |
//! | `mock/crash` | **不回包直接退出**（码 3，测服务器中途退出） |
//! | `mock/malformed` | 写一个非法帧头（`Content-Length: oops`，测非法帧） |
//!
//! 只用 std：客户端要异步，假服务器反而越简单越好——**它不该有 bug**，
//! 否则测试失败时分不清是客户端错还是桩错。

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};

use serde_json::{json, Value};

/// 打开的文档快照。
#[derive(Debug, Clone)]
struct Document {
    language_id: String,
    version: i64,
    text: String,
}

#[derive(Default)]
struct Server {
    initialized: bool,
    shutdown_requested: bool,
    documents: BTreeMap<String, Document>,
    did_open_count: usize,
    did_change_count: usize,
    did_close_count: usize,
    server_requests: Vec<String>,
}

fn main() {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    let mut server = Server::default();

    loop {
        let frame = match read_frame(&mut reader) {
            Ok(Some(body)) => body,
            // stdin 关了：客户端不要我们了。
            Ok(None) => return,
            Err(reason) => {
                // 客户端发来的帧坏了：记一行就退，避免继续读错位字节。
                let _ = writeln!(std::io::stderr(), "mock-server: 帧读取失败：{reason}");
                return;
            }
        };

        let message: Value = match serde_json::from_str(&frame) {
            Ok(value) => value,
            Err(err) => {
                let _ = writeln!(std::io::stderr(), "mock-server: JSON 非法：{err}");
                return;
            }
        };

        if let Err(err) = handle(&mut server, &message, &mut writer) {
            let _ = writeln!(std::io::stderr(), "mock-server: 处理失败：{err}");
            return;
        }
    }
}

fn handle(
    server: &mut Server,
    message: &Value,
    writer: &mut impl Write,
) -> Result<(), std::io::Error> {
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let id = message.get("id").cloned();
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    // 通知（没有 id）：只记状态，绝不回包。
    if id.is_none() || id.as_ref().is_some_and(Value::is_null) {
        match method.as_str() {
            "initialized" => server.initialized = true,
            "textDocument/didOpen" => {
                server.did_open_count += 1;
                let doc = &params["textDocument"];
                server.documents.insert(
                    doc["uri"].as_str().unwrap_or_default().to_string(),
                    Document {
                        language_id: doc["languageId"].as_str().unwrap_or_default().to_string(),
                        version: doc["version"].as_i64().unwrap_or_default(),
                        text: doc["text"].as_str().unwrap_or_default().to_string(),
                    },
                );
            }
            "textDocument/didChange" => {
                server.did_change_count += 1;
                let doc = &params["textDocument"];
                let uri = doc["uri"].as_str().unwrap_or_default().to_string();
                let version = doc["version"].as_i64().unwrap_or_default();
                let text = params["contentChanges"]
                    .get(0)
                    .and_then(|change| change.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if let Some(existing) = server.documents.get_mut(&uri) {
                    existing.version = version;
                    existing.text = text;
                }
            }
            "textDocument/didClose" => {
                server.did_close_count += 1;
                if let Some(uri) = params["textDocument"]["uri"].as_str() {
                    server.documents.remove(uri);
                }
            }
            "exit" => std::process::exit(if server.shutdown_requested { 0 } else { 1 }),
            _ => {}
        }
        return Ok(());
    }

    let id = id.expect("上面已排除 null/缺失");
    match method.as_str() {
        "initialize" => {
            server.server_requests.push("initialize".to_string());
            send_result(
                writer,
                &id,
                json!({
                    "capabilities": {
                        "textDocumentSync": 1,
                        "hoverProvider": true,
                        "definitionProvider": {"workDoneProgress": false}
                    },
                    "serverInfo": {"name": "lsp-mock-server", "version": "0.1.0"}
                }),
            )?;
        }
        "shutdown" => {
            server.shutdown_requested = true;
            send_result(writer, &id, Value::Null)?;
        }
        "mock/echo" => send_result(writer, &id, params)?,
        "mock/state" => {
            let documents: Vec<Value> = server
                .documents
                .iter()
                .map(|(uri, document)| {
                    json!({
                        "uri": uri,
                        "languageId": document.language_id,
                        "version": document.version,
                        "text": document.text
                    })
                })
                .collect();
            send_result(
                writer,
                &id,
                json!({
                    "initialized": server.initialized,
                    "documents": documents,
                    "didOpen": server.did_open_count,
                    "didChange": server.did_change_count,
                    "didClose": server.did_close_count
                }),
            )?;
        }
        "mock/diagnostics" => {
            let uri = params
                .get("uri")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let message_text = params
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("mock diagnostic")
                .to_string();
            send_notification(
                writer,
                "textDocument/publishDiagnostics",
                json!({
                    "uri": uri,
                    "version": params.get("version").cloned().unwrap_or(Value::Null),
                    "diagnostics": [{
                        "range": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 0, "character": 1}
                        },
                        "severity": 1,
                        "message": message_text
                    }]
                }),
            )?;
            send_result(writer, &id, Value::Null)?;
        }
        "mock/sleep" => {
            let millis = params.get("ms").and_then(Value::as_u64).unwrap_or(1_000);
            std::thread::sleep(std::time::Duration::from_millis(millis));
            send_result(writer, &id, Value::Null)?;
        }
        "mock/crash" => {
            // 故意不回包：模拟服务器被 OOM 杀掉 / 自己崩了。
            let _ = writer.flush();
            std::process::exit(3);
        }
        "mock/malformed" => {
            // 头写了非数字长度：客户端必须在解析头时就判定帧边界不可信。
            writer.write_all(b"Content-Length: oops\r\n\r\n{}")?;
            writer.flush()?;
            // 之后什么都不回，让客户端把连接判死而不是干等。
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        other => {
            send_error(writer, &id, -32601, format!("mock-server 未实现：{other}"))?;
        }
    }
    Ok(())
}

/// 读一帧消息体；`Ok(None)` 表示干净 EOF。
fn read_frame(reader: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|err| format!("读头失败：{err}"))?;
        if read == 0 {
            return if content_length.is_none() {
                Ok(None)
            } else {
                Err("帧头中途 EOF".to_string())
            };
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .map_err(|_| format!("Content-Length 非法：{value:?}"))?,
                );
            }
        }
    }
    let length = content_length.ok_or_else(|| "缺 Content-Length".to_string())?;
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|err| format!("读消息体失败：{err}"))?;
    String::from_utf8(body)
        .map(Some)
        .map_err(|err| format!("消息体不是 UTF-8：{err}"))
}

fn send_result(writer: &mut impl Write, id: &Value, result: Value) -> Result<(), std::io::Error> {
    write_message(
        writer,
        &json!({"jsonrpc": "2.0", "id": id, "result": result}),
    )
}

fn send_error(
    writer: &mut impl Write,
    id: &Value,
    code: i64,
    message: String,
) -> Result<(), std::io::Error> {
    write_message(
        writer,
        &json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
    )
}

fn send_notification(
    writer: &mut impl Write,
    method: &str,
    params: Value,
) -> Result<(), std::io::Error> {
    write_message(
        writer,
        &json!({"jsonrpc": "2.0", "method": method, "params": params}),
    )
}

fn write_message(writer: &mut impl Write, message: &Value) -> Result<(), std::io::Error> {
    let body = serde_json::to_string(message).expect("JSON 序列化");
    // 长度必须是字节数：诊断消息里带中文也不会算错。
    write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    writer.flush()
}
