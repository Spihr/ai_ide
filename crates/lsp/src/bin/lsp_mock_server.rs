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
//! | `textDocument/definition` | 回 `Location` + `LocationLink` + 一条缺 `range` 的坏条目 |
//! | `textDocument/references` | 按 `context.includeDeclaration` 回 2 条 / 1 条 |
//! | `textDocument/hover` | 文档非空时回 MarkedString 数组 + `range`，文档为空回 `null` |
//! | `textDocument/completion` | 回 `{isIncomplete: true, items: [...]}` |
//!
//! 查询类方法（definition / references / hover / completion）**要求文档先
//! `didOpen`**，否则回 `-32000` 错误——客户端如果漏了自动同步，这里会立刻炸，
//! 而不是回一个空结果让人误以为「这个符号没有定义」。
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
    /// 每个请求方法被调用了几次（验证「同一份文本只 didChange 一次」这类不变量）。
    request_counts: BTreeMap<String, usize>,
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
    *server.request_counts.entry(method.clone()).or_insert(0) += 1;

    // 查询类方法的前置条件：文档必须已经 didOpen。真实服务器对未打开文档
    // 往往沉默或回空，客户端漏掉自动 didOpen 时会表现成「查不到」，很难查；
    // 这里直接报错，把问题暴露在最早的断点上。
    if matches!(
        method.as_str(),
        "textDocument/definition"
            | "textDocument/references"
            | "textDocument/hover"
            | "textDocument/completion"
    ) {
        let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
        if !server.documents.contains_key(uri) {
            send_error(
                writer,
                &id,
                -32000,
                format!("文档未打开：{uri}（客户端应先发 textDocument/didOpen）"),
            )?;
            return Ok(());
        }
    }

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
                    "didClose": server.did_close_count,
                    "requests": server.request_counts
                }),
            )?;
        }
        "textDocument/definition" => {
            let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
            let line = params["position"]["line"].as_u64().unwrap_or(0);
            let character = params["position"]["character"].as_u64().unwrap_or(0);
            // 三种形态一次覆盖：普通 Location、LocationLink（只有 target* 字段）、
            // 以及一条缺 range 的坏条目（客户端必须跳过，而不是交出一个坐标为空的位置）。
            send_result(
                writer,
                &id,
                json!([
                    {
                        "uri": uri,
                        "range": {
                            "start": {"line": line + 2, "character": character},
                            "end": {"line": line + 2, "character": character + 5}
                        }
                    },
                    {
                        "targetUri": "file:///E:/ai_ide/src/%E4%B8%AD%E6%96%87.ts",
                        "targetRange": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 9, "character": 0}
                        },
                        "targetSelectionRange": {
                            "start": {"line": 4, "character": 6},
                            "end": {"line": 4, "character": 11}
                        }
                    },
                    {"uri": uri}
                ]),
            )?;
        }
        "textDocument/references" => {
            let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
            // 用 includeDeclaration 决定条数：客户端没把 context 传下去的话，
            // 条数就会和预期对不上。
            let include_declaration = params["context"]["includeDeclaration"]
                .as_bool()
                .unwrap_or(true);
            let count = if include_declaration { 2 } else { 1 };
            let locations: Vec<Value> = (0..count)
                .map(|index| {
                    json!({
                        "uri": uri,
                        "range": {
                            "start": {"line": index, "character": 0},
                            "end": {"line": index, "character": 4}
                        }
                    })
                })
                .collect();
            send_result(writer, &id, Value::Array(locations))?;
        }
        "textDocument/hover" => {
            let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
            let empty = server
                .documents
                .get(uri)
                .map(|document| document.text.is_empty())
                .unwrap_or(true);
            if empty {
                // 空文档没有可悬停的东西：规范允许回 null，客户端不该当成错误。
                send_result(writer, &id, Value::Null)?;
            } else {
                let line = params["position"]["line"].as_u64().unwrap_or(0);
                send_result(
                    writer,
                    &id,
                    json!({
                        "contents": [
                            {"language": "typescript", "value": "const value: number"},
                            "来自 mock 的说明"
                        ],
                        "range": {
                            "start": {"line": line, "character": 0},
                            "end": {"line": line, "character": 5}
                        }
                    }),
                )?;
            }
        }
        "textDocument/completion" => {
            send_result(
                writer,
                &id,
                json!({
                    "isIncomplete": true,
                    "items": [
                        {"label": "alpha", "kind": 3, "detail": "() => void", "insertText": "alpha()"},
                        {"label": "beta", "kind": 6}
                    ]
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
