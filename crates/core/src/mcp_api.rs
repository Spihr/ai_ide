//! `mcp.*` 的协议处理器（M7 切片二）。
//!
//! 本模块只做三件事：
//!
//! 1. 解析参数（`parse_params`）；
//! 2. 调 [`McpManager`](crate::mcp_manager::McpManager) 干活；
//! 3. 把 [`McpError`] 折成**独立分段**的错误码（`8001`~`8007`）。
//!
//! 为什么错误码要单开 `8xxx`：`6xxx` 已被终端占住（`6001`~`6004`），而 MCP 的失败
//! 面比终端宽得多——「配置里没这个 Server」「连不上」「没连就调」「工具不存在」
//! 「调用失败」「超时」的处置完全不同（前两条要用户改 `mcp.json`，第三条要 UI 补一个
//! `mcp.connect`）。挤在四条码里，外壳只能靠 message 猜。

use ai_ide_mcp::{split_namespaced_tool, McpError};
use ai_ide_protocol::dto::{
    McpCallParams, McpCallResult, McpConnectParams, McpConnectResult, McpDisconnectParams,
    McpDisconnectResult, McpServersParams, McpServersResult, McpToolsParams, McpToolsResult,
};
use ai_ide_protocol::{ErrorCode, ErrorObject, Request};
use serde_json::{json, Value};

use crate::dispatch::parse_params;
use crate::mcp_manager::{content_blocks, result_text, McpManager};
use crate::peer::Peer;
use crate::state::AppState;

/// 失败发生在哪一段。同一个 [`McpError`] 在连接期与调用期该给的码不同：
/// 连接期连不上是「配置 / 环境」问题（`8002`），调用期断了是「这条连接没了」（`8005`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// 建连 + 列工具。
    Connect,
    /// 工具调用。
    Call,
}

/// `mcp.servers`：已配置 ∪ 已连接的 Server 清单。
pub async fn servers(peer: &Peer, state: &AppState, req: &Request) {
    if let Err(err) = parse_params::<McpServersParams>(req.params.as_ref()) {
        return peer.respond_error(&req.id, &err).await;
    }
    let Some(manager) = state.mcp() else {
        // 内核没装载 MCP 管理器：这不是错误，如实回空表（外壳据此显示「本版本无 MCP」）。
        return peer
            .respond(
                &req.id,
                &McpServersResult {
                    servers: Vec::new(),
                },
            )
            .await;
    };
    peer.respond(
        &req.id,
        &McpServersResult {
            servers: manager.server_infos(),
        },
    )
    .await;
}

/// `mcp.connect`：连一个 Server 并拉取工具清单。
pub async fn connect(peer: &Peer, state: &AppState, req: &Request) {
    let params: McpConnectParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let Some(manager) = state.mcp() else {
        return peer
            .respond_error(
                &req.id,
                &ErrorObject::new(
                    ErrorCode::MCP_SERVER_UNKNOWN,
                    "本内核未装载 MCP 管理器，无法连接 Server",
                ),
            )
            .await;
    };

    match manager.connect(&params.server).await {
        Ok(connected) => {
            peer.respond(
                &req.id,
                &McpConnectResult {
                    server: connected.server,
                    transport: connected.transport,
                    protocol_version: connected.protocol_version,
                    tool_count: connected.tools.len() as u32,
                    tools: connected.tools,
                },
            )
            .await;
        }
        Err(err) => {
            peer.respond_error(&req.id, &error_of(err, Phase::Connect))
                .await;
        }
    }
}

/// `mcp.disconnect`：断开一个 Server（幂等）。
pub async fn disconnect(peer: &Peer, state: &AppState, req: &Request) {
    let params: McpDisconnectParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let Some(manager) = state.mcp() else {
        // 没管理器 = 本来就没连着任何 Server：按幂等回 `closed = false`。
        return peer
            .respond(
                &req.id,
                &McpDisconnectResult {
                    server: params.server,
                    closed: false,
                    removed_tools: 0,
                },
            )
            .await;
    };

    match manager.disconnect(&params.server).await {
        Ok((closed, removed_tools)) => {
            peer.respond(
                &req.id,
                &McpDisconnectResult {
                    server: params.server,
                    closed,
                    removed_tools,
                },
            )
            .await;
        }
        Err(err) => {
            peer.respond_error(&req.id, &error_of(err, Phase::Connect))
                .await;
        }
    }
}

/// `mcp.tools`：列出已连接 Server 的工具。
pub async fn tools(peer: &Peer, state: &AppState, req: &Request) {
    let params: McpToolsParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let Some(manager) = state.mcp() else {
        return peer
            .respond(&req.id, &McpToolsResult { tools: Vec::new() })
            .await;
    };
    peer.respond(
        &req.id,
        &McpToolsResult {
            tools: manager.tool_infos(params.server.as_deref()),
        },
    )
    .await;
}

/// `mcp.call`：调一个 MCP 工具。
///
/// `server` 可省略——此时 `tool` 必须是 `mcp__<server>__<tool>` 全名。两种写法都接受，
/// 是因为模型拿到的是全名（注册表里注入的），而外壳手点工具时更可能只写原始名。
pub async fn call(peer: &Peer, state: &AppState, req: &Request) {
    let params: McpCallParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let Some(manager) = state.mcp() else {
        return peer
            .respond_error(
                &req.id,
                &ErrorObject::new(
                    ErrorCode::MCP_SERVER_UNKNOWN,
                    "本内核未装载 MCP 管理器，无法调用工具",
                ),
            )
            .await;
    };

    let Some(target) = resolve_target(&params) else {
        return peer
            .respond_error(
                &req.id,
                &ErrorObject::new(
                    ErrorCode::MCP_INVALID_PARAMS,
                    "必须给出 server，或使用 mcp__<server>__<tool> 形式的全名工具",
                )
                .with_data(json!({ "tool": params.tool })),
            )
            .await;
    };

    // 先看连接，再查工具：没连上的时候「工具不在」是必然结论，照直报 8004 会把
    // 模型引到「换工具名」这条死路上，而它真正该做的是 `mcp.connect`。
    if let Err(err) = ensure_connected(manager, &target.server) {
        return peer.respond_error(&req.id, &err).await;
    }

    // 连上了还不存在，才是真的参数错：模型可能拼错了工具名（连接期内工具下架
    // 也走这条）。
    if manager.tool_of(&target.server, &target.tool).is_none() {
        return peer
            .respond_error(
                &req.id,
                &ErrorObject::new(
                    ErrorCode::MCP_TOOL_NOT_FOUND,
                    format!(
                        "MCP Server `{}` 上没有工具 `{}`（可能未连接，或工具已下架）",
                        target.server, target.tool
                    ),
                )
                .with_data(json!({
                    "server": target.server,
                    "tool": target.tool,
                })),
            )
            .await;
    }

    let arguments = params
        .arguments
        .clone()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    if !arguments.is_object() {
        return peer
            .respond_error(
                &req.id,
                &ErrorObject::new(ErrorCode::MCP_INVALID_PARAMS, "arguments 必须是 JSON 对象")
                    .with_data(json!({ "tool": params.tool })),
            )
            .await;
    }

    match manager
        .call_tool(&target.server, &target.tool, arguments)
        .await
    {
        Ok(result) => {
            let response = McpCallResult {
                namespaced_name: ai_ide_mcp::tool_namespace(&target.server, &target.tool),
                server: target.server,
                tool: target.tool,
                text: result_text(&result),
                content: content_blocks(&result.content),
                is_error: result.is_error,
                structured_content: result.structured_content,
            };
            peer.respond(&req.id, &response).await;
        }
        // 工具**执行**失败（`is_error = true`）走的是 `Ok` 分支：MCP 刻意让工具错误
        // 作为正常结果回给调用方，让它自己决定换路。真正进到这里的是协议/传输失败。
        Err(err) => {
            peer.respond_error(&req.id, &error_of(err, Phase::Call))
                .await
        }
    }
}

/// 解析目标：`server` 显式给出，或从全名里拆。
struct Target {
    server: String,
    tool: String,
}

fn resolve_target(params: &McpCallParams) -> Option<Target> {
    if let Some(server) = params
        .server
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // 给了 server，但 tool 仍是全名：拆掉前缀，避免把 `mcp__fs__read` 当成原始名。
        let tool = match split_namespaced_tool(&params.tool) {
            Some((_, tool)) => tool.to_string(),
            None => params.tool.clone(),
        };
        // 只有 server 没有工具名，或 tool 只剩空白：属于参数错，不能落到
        // 「去 Server 上调一个空名字」那条路上。
        let tool = tool.trim();
        if tool.is_empty() {
            return None;
        }
        return Some(Target {
            server: server.to_string(),
            tool: tool.to_string(),
        });
    }
    split_namespaced_tool(&params.tool).map(|(server, tool)| Target {
        server: server.to_string(),
        tool: tool.to_string(),
    })
}

/// 调用前的连接检查。
///
/// 「Server 没连上」和「这个工具不存在」对模型是两条完全不同的指令：前者让它去
/// `mcp.connect`，后者让它改工具名。若不做这一步前置检查，未连接时 `tool_of`
/// 必然查不到，就会被误报成 8004，模型于是开始瞎换工具名——实际上它第一次写的
/// 名字是对的，只是连接没建。所以必须先看连接，再谈工具在不在。
fn ensure_connected(manager: &McpManager, server: &str) -> Result<(), ErrorObject> {
    if !manager.is_configured(server) {
        return Err(error_of(
            McpError::UnknownServer(server.to_string()),
            Phase::Call,
        ));
    }
    if manager.client_of(server).is_none() {
        return Err(error_of(
            McpError::NotInitialized(server.to_string()),
            Phase::Call,
        ));
    }
    Ok(())
}

/// [`McpError`] → 协议错误。
fn error_of(err: McpError, phase: Phase) -> ErrorObject {
    let (code, data) = match (&err, phase) {
        (McpError::UnknownServer(name), _) => (
            ErrorCode::MCP_SERVER_UNKNOWN,
            json!({ "server": name, "hint": "检查 mcp.json 里的 server 名" }),
        ),
        (McpError::NotInitialized(_), _) => (
            ErrorCode::MCP_NOT_CONNECTED,
            json!({ "hint": "先调 mcp.connect" }),
        ),
        (McpError::Timeout { method, timeout_ms }, _) => (
            ErrorCode::MCP_TIMEOUT,
            json!({ "method": method, "timeout_ms": timeout_ms }),
        ),
        (McpError::Spawn { server, .. }, _) | (McpError::Http { server, .. }, _) => {
            (ErrorCode::MCP_CONNECT_FAILED, json!({ "server": server }))
        }
        (McpError::Config(_), _) | (McpError::Protocol(_), _) => {
            (ErrorCode::MCP_CONNECT_FAILED, json!({}))
        }
        (McpError::Transport { server, .. }, Phase::Connect) => {
            (ErrorCode::MCP_CONNECT_FAILED, json!({ "server": server }))
        }
        (McpError::Transport { server, .. }, Phase::Call)
        | (McpError::Disconnected { server, .. }, Phase::Call) => {
            (ErrorCode::MCP_CALL_FAILED, json!({ "server": server }))
        }
        (McpError::Disconnected { server, .. }, Phase::Connect) => {
            (ErrorCode::MCP_CONNECT_FAILED, json!({ "server": server }))
        }
        (McpError::Rpc { code: rpc_code, .. }, Phase::Connect) => (
            ErrorCode::MCP_CONNECT_FAILED,
            json!({ "rpc_code": rpc_code }),
        ),
        (McpError::Rpc { code: rpc_code, .. }, Phase::Call) => {
            (ErrorCode::MCP_CALL_FAILED, json!({ "rpc_code": rpc_code }))
        }
    };
    ErrorObject::new(code, err.to_string()).with_data(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(server: Option<&str>, tool: &str) -> McpCallParams {
        McpCallParams {
            server: server.map(str::to_string),
            tool: tool.to_string(),
            arguments: None,
        }
    }

    #[test]
    fn target_prefers_explicit_server_and_strips_namespace() {
        let target = resolve_target(&params(Some("fs"), "mcp__fs__read_file")).expect("可解析");
        assert_eq!(target.server, "fs");
        assert_eq!(target.tool, "read_file");
    }

    #[test]
    fn target_falls_back_to_namespaced_name() {
        let target = resolve_target(&params(None, "mcp__git__status")).expect("可解析");
        assert_eq!(target.server, "git");
        assert_eq!(target.tool, "status");
    }

    #[test]
    fn target_rejects_bare_name_without_server() {
        assert!(resolve_target(&params(None, "status")).is_none());
        // 只有 server 而没给工具名同样是参数错。
        assert!(resolve_target(&params(Some("fs"), "")).is_none());
    }

    /// 8001~8007 各归其位：这组断言是「错误码不许撞 6xxx」的守门员。
    #[test]
    fn error_codes_land_in_the_mcp_band() {
        let cases = vec![
            (
                McpError::UnknownServer("x".into()),
                Phase::Call,
                ErrorCode::MCP_SERVER_UNKNOWN,
            ),
            (
                McpError::NotInitialized("x".into()),
                Phase::Call,
                ErrorCode::MCP_NOT_CONNECTED,
            ),
            (
                McpError::Spawn {
                    server: "x".into(),
                    reason: "no such binary".into(),
                },
                Phase::Connect,
                ErrorCode::MCP_CONNECT_FAILED,
            ),
            (
                McpError::Protocol("bad".into()),
                Phase::Connect,
                ErrorCode::MCP_CONNECT_FAILED,
            ),
            (
                McpError::Timeout {
                    method: "tools/call".into(),
                    timeout_ms: 1,
                },
                Phase::Call,
                ErrorCode::MCP_TIMEOUT,
            ),
            (
                McpError::Transport {
                    server: "x".into(),
                    reason: "pipe closed".into(),
                },
                Phase::Call,
                ErrorCode::MCP_CALL_FAILED,
            ),
        ];
        for (err, phase, expected) in cases {
            let object = error_of(err, phase);
            assert_eq!(object.code, expected, "{}", object.message);
            let code = object.code.as_i32();
            assert!((8000..9000).contains(&code), "必须落在 8xxx：{code}");
        }
    }

    /// 连接期的传输失败与调用期的传输失败要分开：前者提示改配置，后者只值得重连。
    #[test]
    fn transport_error_depends_on_phase() {
        let err = || McpError::Transport {
            server: "fs".into(),
            reason: "eof".into(),
        };
        assert_eq!(
            error_of(err(), Phase::Connect).code,
            ErrorCode::MCP_CONNECT_FAILED
        );
        assert_eq!(
            error_of(err(), Phase::Call).code,
            ErrorCode::MCP_CALL_FAILED
        );
    }
}
