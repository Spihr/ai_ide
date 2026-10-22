//! 测试专用 mock MCP Server（stdio）。
//!
//! 只服务于本 crate 的端到端测试：真实子进程 + 真实管道，验证「逐行帧」这条路径
//! 在不打网络、不用真实凭据的前提下能跑通，也方便复现各种坏 Server 行为
//! （握手失败、超时、调用中途断开、回非法 JSON）。
//!
//! 场景用命令行参数或环境变量 `MCP_MOCK_SCENARIO` 选择，默认 `ok`：
//!
//! | 场景 | 行为 |
//! |---|---|
//! | `ok` | 正常握手、`tools/list`、`tools/call`（`echo` / `fail_tool` / `slow_tool`） |
//! | `paginate` | `tools/list` 第一页带 `nextCursor`，第二页收尾 |
//! | `unsupported-version` | 握手返回不受支持的协议版本 |
//! | `malformed` | 握手返回一行非 JSON |
//! | `rpc-error` | `tools/call` 返回 JSON-RPC 错误 |
//! | `silent` | 永远不回包（用于超时） |
//! | `crash-after-init` | 握手后立即退出（用于断连） |
//! | `exit-on-call` | `tools/call` 不回复直接退出（用于调用中断连） |

use std::io::{BufRead, Write};
use std::process::ExitCode;

use serde_json::{json, Value};

const SUPPORTED: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

fn main() -> ExitCode {
    let scenario = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("MCP_MOCK_SCENARIO").ok())
        .unwrap_or_else(|| "ok".to_string());

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    // `crash-after-init` 用：等下一次**请求**（通知不算）到来时直接退出，
    // 这样客户端一定是在「等响应」的中途看到管道断开，而不是握手刚写完就断。
    let mut exit_on_next_request = false;

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(_) => continue,
        };
        let Some(id) = message.get("id").cloned() else {
            // 通知（`notifications/initialized` 等）：不需要回包。
            continue;
        };
        if exit_on_next_request {
            return ExitCode::FAILURE;
        }
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        match method.as_str() {
            "initialize" => match scenario.as_str() {
                "malformed" => {
                    // 是合法 JSON，但 jsonrpc 版本不对——典型的「协议违约」而非日志噪声。
                    if writeln!(
                        stdout,
                        "{{\"jsonrpc\":\"1.0\",\"id\":{},\"result\":{{}}}}",
                        id
                    )
                    .is_err()
                    {
                        return ExitCode::SUCCESS;
                    }
                    let _ = stdout.flush();
                }
                _ => {
                    let version = if scenario == "unsupported-version" {
                        "1999-01-01".to_string()
                    } else {
                        pick_version(&params)
                    };
                    let body = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": version,
                            "capabilities": {"tools": {"listChanged": false}},
                            "serverInfo": {"name": "mock", "version": "0.1.0"}
                        }
                    });
                    if !reply(&mut stdout, &body) {
                        return ExitCode::SUCCESS;
                    }
                    if scenario == "crash-after-init" {
                        exit_on_next_request = true;
                    }
                }
            },
            "tools/list" => {
                let page = params
                    .get("cursor")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let body = if scenario == "paginate" && page.is_empty() {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "tools": [tool("echo", "回声")],
                            "nextCursor": "page-2"
                        }
                    })
                } else if scenario == "paginate" {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {"tools": [tool("second", "第二页工具")]}
                    })
                } else {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "tools": [
                                tool("echo", "回声"),
                                tool("fail_tool", "必定失败的工具"),
                                tool("slow_tool", "慢工具")
                            ]
                        }
                    })
                };
                if !reply(&mut stdout, &body) {
                    return ExitCode::SUCCESS;
                }
            }
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if scenario == "silent" {
                    continue;
                }
                if scenario == "exit-on-call" {
                    return ExitCode::FAILURE;
                }
                if scenario == "rpc-error" {
                    let body = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32000, "message": "mock 刻意失败"}
                    });
                    if !reply(&mut stdout, &body) {
                        return ExitCode::SUCCESS;
                    }
                    continue;
                }
                match name {
                    "fail_tool" => {
                        let body = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{"type": "text", "text": "工具执行失败：mock"}],
                                "isError": true
                            }
                        });
                        if !reply(&mut stdout, &body) {
                            return ExitCode::SUCCESS;
                        }
                    }
                    "slow_tool" => {
                        std::thread::sleep(std::time::Duration::from_secs(3));
                        let body = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {"content": [{"type": "text", "text": "太慢了"}]}
                        });
                        if !reply(&mut stdout, &body) {
                            return ExitCode::SUCCESS;
                        }
                    }
                    _ => {
                        let body = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{"type": "text", "text": format!("echo:{params}")}]
                            }
                        });
                        if !reply(&mut stdout, &body) {
                            return ExitCode::SUCCESS;
                        }
                    }
                }
            }
            _ => {
                let body = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": format!("未知方法 {method}")}
                });
                if !reply(&mut stdout, &body) {
                    return ExitCode::SUCCESS;
                }
            }
        }
    }
    ExitCode::SUCCESS
}

/// 从客户端声明里挑一个双方都支持的版本（挑不出就回自己最新的，让客户端拒绝）。
fn pick_version(params: &Value) -> String {
    let offered = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if SUPPORTED.contains(&offered) {
        return offered.to_string();
    }
    SUPPORTED[0].to_string()
}

/// 构造一个工具描述。
fn tool(name: &str, description: &str) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"]
        }
    })
}

/// 写一行帧并 flush；管道已断时返回 `false`。
fn reply(stdout: &mut std::io::Stdout, body: &Value) -> bool {
    if writeln!(stdout, "{body}").is_err() {
        return false;
    }
    stdout.flush().is_ok()
}
