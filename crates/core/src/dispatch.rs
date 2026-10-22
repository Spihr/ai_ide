//! 方法分发：把一条入站报文路由到对应处理器。
//!
//! 方法集仍然用 `match` 分派——协议方法数是两位数，穷举分支比注册表更好读，
//! 编译器还能替我们检查漏接。M2 之后 `provider.*` / `model.*` / `llm.*` 都有分支，
//! `methods::IMPLEMENTED` 与这里一一对应；尚未接线的方法留在
//! `methods::DECLARED_NOT_IMPLEMENTED` 里，调用会如实返回 `-32601`。

use crate::agent_api;
use crate::audit_api;
use crate::checkpoint_api;
use crate::llm_api;
use crate::peer::Peer;
use crate::permission_api;
use crate::provider_api;
use crate::session_api;
use crate::state::{now_ms, AppState};
use crate::sysinfo;
use crate::version::CORE_VERSION;
use crate::ws;
use ai_ide_protocol::dto::{
    CancelParams, HelloParams, HelloResult, PingParams, PingResult, ShutdownParams, ShutdownResult,
    StatsParams, StatsResult,
};
use ai_ide_protocol::methods;
use ai_ide_protocol::{
    decode, ErrorCode, ErrorObject, Incoming, Notification, ProtocolVersion, Request,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::{debug, info, warn};

/// 处理一行 NDJSON。
pub async fn handle_line(peer: &Peer, state: &AppState, line: &str) {
    let incoming = match decode(line) {
        Ok(incoming) => incoming,
        Err(err) => {
            // 解析阶段拿不到 id，按 JSON-RPC 2.0 规定以 null 回错误。
            warn!(code = err.code.as_i32(), message = %err.message, "报文解析失败");
            peer.respond_anonymous_error(&err).await;
            return;
        }
    };

    match incoming {
        Incoming::Request(req) => handle_request(peer, state, req).await,
        Incoming::Notification(notif) => handle_notification(peer, state, notif).await,
    }
}

async fn handle_request(peer: &Peer, state: &AppState, req: Request) {
    peer.begin_request();
    let method = req.method.clone();
    debug!(id = %req.id, method = %method, "收到请求");

    if state.is_cancelled(&req.id) {
        state.clear_cancellation(&req.id);
        info!(id = %req.id, method = %method, "请求在派发前已被取消");
        peer.respond_error(
            &req.id,
            &ErrorObject::new(ErrorCode::CANCELLED, "请求已被取消"),
        )
        .await;
        peer.end_request();
        return;
    }

    match method.as_str() {
        methods::core::HELLO => core_hello(peer, &req).await,
        methods::core::PING => core_ping(peer, &req).await,
        methods::core::STATS => core_stats(peer, &req).await,
        methods::core::SHUTDOWN => core_shutdown(peer, &req).await,

        // 工作区与文件系统（M1）
        methods::workspace::OPEN => ws::open(peer, state, &req).await,
        methods::workspace::CLOSE => ws::close(peer, state, &req).await,
        methods::workspace::TREE => ws::tree(peer, state, &req).await,
        methods::workspace::READ_FILE => ws::read_file(peer, state, &req).await,
        methods::workspace::WRITE_FILE => ws::write_file(peer, state, &req).await,
        methods::workspace::CREATE => ws::create(peer, state, &req).await,
        methods::workspace::RENAME => ws::rename(peer, state, &req).await,
        methods::workspace::DELETE => ws::delete(peer, state, &req).await,
        methods::workspace::REVEAL => ws::reveal(peer, state, &req).await,

        // 检索（M1）
        methods::search::FILES => ws::search_files(peer, state, &req).await,
        methods::search::CONTENT => ws::search_content(peer, state, &req).await,

        // Provider 与模型目录（M2）
        methods::provider::UPSERT => provider_api::provider_upsert(peer, state, &req).await,
        methods::provider::LIST => provider_api::provider_list(peer, state, &req).await,
        methods::provider::DELETE => provider_api::provider_delete(peer, state, &req).await,
        methods::provider::TEST => provider_api::provider_test(peer, state, &req).await,
        methods::model::UPSERT => provider_api::model_upsert(peer, state, &req).await,
        methods::model::LIST => provider_api::model_list(peer, state, &req).await,
        methods::model::DELETE => provider_api::model_delete(peer, state, &req).await,

        // 流式对话与模型枚举（M2）
        methods::llm::CHAT_STREAM => llm_api::chat_stream(peer, state, &req).await,
        methods::llm::CANCEL => llm_api::cancel(peer, state, &req).await,
        methods::llm::LIST_MODELS => llm_api::list_models(peer, state, &req).await,
        methods::llm::COUNT_TOKENS => llm_api::count_tokens(peer, state, &req).await,

        // Agent 运行时（M4）
        methods::agent::START => agent_api::start(peer, state, &req).await,
        methods::agent::STOP => agent_api::stop(peer, state, &req).await,
        methods::agent::DECIDE => agent_api::decide(peer, state, &req).await,

        // 会话与消息（M5）：落盘在 crates/store，这里只做协议翻译
        methods::session::CREATE => session_api::create(peer, state, &req).await,
        methods::session::LIST => session_api::list(peer, state, &req).await,
        methods::session::GET => session_api::get(peer, state, &req).await,
        methods::session::UPDATE => session_api::update(peer, state, &req).await,
        methods::session::DELETE => session_api::delete(peer, state, &req).await,
        methods::session::APPEND_MESSAGE => session_api::append_message(peer, state, &req).await,
        methods::session::MESSAGES => session_api::messages(peer, state, &req).await,
        // M5 切片三：导出与分叉。
        methods::session::EXPORT => session_api::export(peer, state, &req).await,
        methods::session::FORK => session_api::fork(peer, state, &req).await,
        methods::audit::LIST => audit_api::list(peer, state, &req).await,

        // 检查点与回滚（M5 切片五）：快照落文件，元数据落 meta.json
        methods::checkpoint::CREATE => checkpoint_api::create(peer, state, &req).await,
        methods::checkpoint::LIST => checkpoint_api::list(peer, state, &req).await,
        methods::checkpoint::RESTORE => checkpoint_api::restore(peer, state, &req).await,
        methods::checkpoint::DELETE => checkpoint_api::delete(peer, state, &req).await,

        // 权限免问规则（M5 切片六）：只读列表与撤销。授予只能走 `agent.decide`。
        methods::permission::LIST => permission_api::list(peer, state, &req).await,
        methods::permission::REVOKE => permission_api::revoke(peer, state, &req).await,

        other => {
            warn!(method = %other, "未实现的方法");
            peer.respond_error(&req.id, &ErrorObject::method_not_found(other))
                .await;
        }
    }

    peer.end_request();
}

async fn handle_notification(peer: &Peer, state: &AppState, notif: Notification) {
    match notif.method.as_str() {
        methods::cancel::REQUEST => match parse_params::<CancelParams>(notif.params.as_ref()) {
            Ok(params) => {
                // `id` 既可能是「还没派发的请求」，也可能是「已在途的流」：
                // 前者登记给派发前检查，后者直接掐掉上游连接。两条路都走一遍，
                // 顺序无所谓——命中在途流的概率更高，就放前面。
                if state.streams().cancel(&params.id) {
                    debug!(stream_id = %params.id, "已请求取消在途流");
                } else {
                    state.mark_cancelled(&params.id);
                    debug!(id = %params.id, "已登记取消");
                }
            }
            Err(err) => warn!(message = %err.message, "$/cancelRequest 参数非法，已忽略"),
        },
        other => debug!(method = %other, "忽略未知通知"),
    }
    let _ = peer; // 通知不回复
}

// ---------------------------------------------------------------------------
// core.hello
// ---------------------------------------------------------------------------

async fn core_hello(peer: &Peer, req: &Request) {
    let params: HelloParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let core_version = ProtocolVersion::CURRENT;
    if !params.protocol_version.is_compatible_with(core_version) {
        let err = ErrorObject::new(
            ErrorCode::PROTOCOL_VERSION_MISMATCH,
            format!(
                "协议主版本不兼容：客户端 {} / 内核 {}",
                params.protocol_version, core_version
            ),
        )
        .with_data(serde_json::json!({
            "client": params.protocol_version,
            "core": core_version,
        }));
        warn!(client = %params.protocol_version, "握手被拒绝：协议版本不兼容");
        return peer.respond_error(&req.id, &err).await;
    }

    info!(
        client_version = %params.client_version,
        platform = %params.platform,
        locale = %params.locale,
        "握手成功"
    );

    let result = HelloResult {
        core_version: CORE_VERSION.to_string(),
        protocol_version: core_version,
        capabilities: methods::IMPLEMENTED
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
        started_at_ms: now_ms(),
    };
    peer.respond(&req.id, &result).await;
}

// ---------------------------------------------------------------------------
// core.ping
// ---------------------------------------------------------------------------

async fn core_ping(peer: &Peer, req: &Request) {
    if let Err(err) = parse_params::<PingParams>(req.params.as_ref()) {
        return peer.respond_error(&req.id, &err).await;
    }
    let result = PingResult {
        pong: true,
        uptime_ms: peer.uptime_ms(),
    };
    peer.respond(&req.id, &result).await;
}

// ---------------------------------------------------------------------------
// core.stats
// ---------------------------------------------------------------------------

async fn core_stats(peer: &Peer, req: &Request) {
    if let Err(err) = parse_params::<StatsParams>(req.params.as_ref()) {
        return peer.respond_error(&req.id, &err).await;
    }
    let result = StatsResult {
        rss_mb: sysinfo::current_rss_mb(),
        requests_handled: peer.requests_handled(),
        tasks_active: peer.tasks_active(),
        uptime_ms: peer.uptime_ms(),
    };
    peer.respond(&req.id, &result).await;
}

// ---------------------------------------------------------------------------
// core.shutdown
// ---------------------------------------------------------------------------

async fn core_shutdown(peer: &Peer, req: &Request) {
    let params: ShutdownParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    info!(graceful = params.graceful, "收到退出指令");
    // `Peer` 每次写入都会 flush，因此 respond 返回即代表结果已落到 stdout。
    peer.respond(&req.id, &ShutdownResult { ok: true }).await;

    // 让出一次调度，确保写入任务彻底完成后再退出。
    tokio::task::yield_now().await;
    std::process::exit(0);
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

/// 把可选 `params` 反序列化为具体 DTO。
///
/// `params` 缺省时视为空对象，这样 `core.ping` 可以接受
/// `{}`、缺字段、甚至完全不传三种写法。
pub(crate) fn parse_params<T: DeserializeOwned>(params: Option<&Value>) -> Result<T, ErrorObject> {
    let value = match params {
        Some(Value::Null) | None => Value::Object(serde_json::Map::new()),
        Some(value) => value.clone(),
    };
    serde_json::from_value(value)
        .map_err(|err| ErrorObject::invalid_params(format!("参数非法: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_ide_protocol::dto::{HelloParams, PingParams};

    #[test]
    fn parse_params_accepts_missing_params_for_empty_dto() {
        let parsed: PingParams = parse_params(None).unwrap();
        assert_eq!(parsed, PingParams {});
    }

    #[test]
    fn parse_params_accepts_null_params() {
        let parsed: PingParams = parse_params(Some(&Value::Null)).unwrap();
        assert_eq!(parsed, PingParams {});
    }

    #[test]
    fn parse_params_rejects_wrong_shape() {
        let err = parse_params::<HelloParams>(Some(&serde_json::json!({ "platform": "win32" })))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn parse_params_accepts_valid_hello() {
        let value = serde_json::json!({
            "client_version": "0.1.0",
            "protocol_version": { "major": 1, "minor": 0 },
            "platform": "win32",
            "locale": "zh-CN",
        });
        let parsed: HelloParams = parse_params(Some(&value)).unwrap();
        assert_eq!(parsed.platform, "win32");
        assert!(parsed.capabilities.is_empty());
    }
}
