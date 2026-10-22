//! 终端会话的协议处理器（M6 切片一）。
//!
//! PTY 本身在 `crates/terminal`（[`ai_ide_terminal::TerminalManager`]），这里只做三件事：
//!
//! 1. 把 `terminal.*` 的 DTO 翻成 [`SpawnOptions`]，把 [`TerminalInfo`] 翻回
//!    [`TerminalView`]；
//! 2. 把 [`TerminalError`] 翻成带域错误码的 [`ErrorObject`]（6001~6004）；
//! 3. [`pump`] 把管理器的事件通道抬成 `terminal.data` / `terminal.exit` 通知。
//!
//! ## 为什么输出要单开一条事件通道
//!
//! 终端输出是**持续且不可预期**的，不能塞进请求-响应里（否则一次 `ls` 要等到
//! 进程退出才有回执）。事件通道在整个进程里只有一条，由 [`pump`] 独占消费：
//! 客户端看到的顺序因此与内核产生事件的顺序完全一致，不需要在客户端侧重排。
//!
//! ## 为什么 `write` 不在这里上 `spawn_blocking`
//!
//! PTY 写口是阻塞的，但每次写的量都很小（键盘输入、粘贴的一段文本），并且
//! `TerminalManager::write` 内部只做一次 `write_all` + `flush`。为这点耗时切线程
//! 的调度成本比它本身更高；真正会长时间阻塞的是读方向，那已经在 `crates/terminal`
//! 里的独立线程上跑了。
//!
//! ## base64
//!
//! 入站 `data` 与出站 `data` 都按**原始字节的 base64** 走：终端里要传的不只是
//! 正文字符，还有 `\r`、`Ctrl-C`（`0x03`）、方向键（`ESC [ A`）这类任意字节，
//! 而输出侧可能是半截 UTF-8、ANSI 转义序列甚至二进制。任何一处先解成 `String`
//! 都会在边界上丢字节，且丢掉的字节在客户端表现为乱码或吞字，难以回溯。

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

use ai_ide_protocol::dto::{
    TerminalCreateParams, TerminalCreateResult, TerminalDataEvent, TerminalExitEvent,
    TerminalKillParams, TerminalListResult, TerminalResizeParams, TerminalResizeResult,
    TerminalView, TerminalWriteParams, TerminalWriteResult,
};
use ai_ide_protocol::{ErrorCode, ErrorObject, Request};
use ai_ide_terminal::{SpawnOptions, TerminalError, TerminalEvent, TerminalInfo};

use crate::dispatch::parse_params;
use crate::peer::Peer;
use crate::state::AppState;

/// `terminal.create`：起一个新会话。
pub async fn create(peer: &Peer, state: &AppState, req: &Request) {
    let params: TerminalCreateParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    // 尺寸缺省值与 `SpawnOptions::default()` 保持一致（80×24）；显式传了就照传，
    // 越界值由管理器夹到合法区间——客户端在容器还没布局完时量到 0 是常态，
    // 为它报错只会让终端抖成一片空白。
    let mut options = SpawnOptions::default();
    if let Some(cols) = params.cols {
        options.cols = cols;
    }
    if let Some(rows) = params.rows {
        options.rows = rows;
    }
    options.shell = params.shell;
    options.args = params.args;
    options.cwd = params.cwd.map(std::path::PathBuf::from);
    options.env = params.env;

    match state.terminal().create(options) {
        Ok(info) => {
            let result = TerminalCreateResult {
                terminal: view_of(info),
            };
            peer.respond(&req.id, &result).await;
        }
        Err(err) => peer.respond_error(&req.id, &error_of(err)).await,
    }
}

/// `terminal.write`：把 base64 解出的原始字节灌进会话。
pub async fn write(peer: &Peer, state: &AppState, req: &Request) {
    let params: TerminalWriteParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    // 解码失败是**参数**问题（6004），不是会话问题：客户端拼错了 base64，
    // 换个会话也一样失败。
    let data = match BASE64.decode(params.data.as_bytes()) {
        Ok(data) => data,
        Err(err) => {
            let error = ErrorObject::new(
                ErrorCode::TERMINAL_INVALID,
                format!("terminal.write 的 data 不是合法 base64: {err}"),
            )
            .with_data(serde_json::json!({ "terminal_id": params.terminal_id }));
            return peer.respond_error(&req.id, &error).await;
        }
    };

    match state.terminal().write(&params.terminal_id, &data) {
        Ok(bytes) => {
            let result = TerminalWriteResult {
                bytes: bytes as u64,
            };
            peer.respond(&req.id, &result).await;
        }
        Err(err) => peer.respond_error(&req.id, &error_of(err)).await,
    }
}

/// `terminal.resize`：改尺寸，回夹取后的真实尺寸。
pub async fn resize(peer: &Peer, state: &AppState, req: &Request) {
    let params: TerminalResizeParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    match state
        .terminal()
        .resize(&params.terminal_id, params.cols, params.rows)
    {
        Ok(info) => {
            let result = TerminalResizeResult {
                terminal: view_of(info),
            };
            peer.respond(&req.id, &result).await;
        }
        Err(err) => peer.respond_error(&req.id, &error_of(err)).await,
    }
}

/// `terminal.kill`：杀掉会话。
///
/// 子进程的退出信息照常经 `terminal.exit` 推出（客户端据此清标签页）；
/// 返回前管理器已把会话从登记表摘掉，所以紧随其后的 `terminal.list` 不会再看到它。
pub async fn kill(peer: &Peer, state: &AppState, req: &Request) {
    let params: TerminalKillParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    match state.terminal().kill(&params.terminal_id) {
        Ok(()) => {
            peer.respond(&req.id, &serde_json::json!({ "ok": true }))
                .await;
        }
        Err(err) => peer.respond_error(&req.id, &error_of(err)).await,
    }
}

/// `terminal.list`：列出存活会话（按建会话时间升序）。
pub async fn list(peer: &Peer, state: &AppState, req: &Request) {
    // 没有参数：多会话是内核管的事，客户端只要全量清单。
    if let Err(err) = parse_params::<serde_json::Value>(req.params.as_ref()) {
        return peer.respond_error(&req.id, &err).await;
    }

    let terminals: Vec<TerminalView> = state.terminal().list().into_iter().map(view_of).collect();
    let result = TerminalListResult {
        total: terminals.len() as u64,
        terminals,
    };
    peer.respond(&req.id, &result).await;
}

/// 事件泵：把管理器的事件通道抬成 JSON-RPC 通知。
///
/// 由 `run()` 在开始读 stdin **之前**拉起：`take_events` 只能取一次，取晚了
/// 建会话时产生的首批输出就会丢。通道关闭（所有发送端被丢弃）时本任务自然结束。
pub async fn pump(
    peer: std::sync::Arc<Peer>,
    mut events: tokio::sync::mpsc::UnboundedReceiver<TerminalEvent>,
) {
    while let Some(event) = events.recv().await {
        match event {
            TerminalEvent::Data {
                terminal_id,
                data_b64,
                bytes,
            } => {
                let payload = TerminalDataEvent {
                    terminal_id,
                    data: data_b64,
                    bytes: bytes as u64,
                };
                peer.notify(ai_ide_protocol::methods::event::TERMINAL_DATA, &payload)
                    .await;
            }
            TerminalEvent::Exit {
                terminal_id,
                code,
                signal,
                at_ms,
            } => {
                let payload = TerminalExitEvent {
                    terminal_id,
                    code,
                    signal,
                    at_ms,
                };
                peer.notify(ai_ide_protocol::methods::event::TERMINAL_EXIT, &payload)
                    .await;
            }
        }
    }
}

/// 停掉全部会话。
///
/// 内核退出前调用。不做的话，ConPTY 里的 `cmd.exe` 会变成孤儿进程挂在任务管理器里
/// ——它已经脱离了内核的 stdin，谁也管不了它。
pub fn shutdown_all(state: &AppState) {
    state.terminal().shutdown_all();
}

/// 管理器视图 → 协议视图。
fn view_of(info: TerminalInfo) -> TerminalView {
    TerminalView {
        id: info.id,
        shell: info.shell,
        cwd: info.cwd,
        pid: info.pid,
        cols: info.cols,
        rows: info.rows,
        created_at_ms: info.created_at_ms,
    }
}

/// 终端错误 → 协议错误。
///
/// 四类一一对应，不做兜底合并：客户端要靠码判断「提示用户重开终端」（6001）
/// 还是「重试这次写」（6003）。
fn error_of(err: TerminalError) -> ErrorObject {
    match err {
        TerminalError::NotFound(id) => ErrorObject::new(
            ErrorCode::TERMINAL_NOT_FOUND,
            format!("终端会话不存在: {id}"),
        )
        .with_data(serde_json::json!({ "terminal_id": id })),
        TerminalError::Spawn(message) => {
            ErrorObject::new(ErrorCode::TERMINAL_SPAWN_FAILED, message)
        }
        TerminalError::Io(message) => ErrorObject::new(ErrorCode::TERMINAL_IO_ERROR, message),
        TerminalError::Invalid(message) => ErrorObject::new(ErrorCode::TERMINAL_INVALID, message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_keeps_every_field() {
        let info = TerminalInfo {
            id: "t-1".into(),
            shell: "cmd.exe".into(),
            cwd: Some("E:\\proj".into()),
            pid: Some(4242),
            cols: 120,
            rows: 30,
            created_at_ms: 1_700_000_000_000,
        };
        let view = view_of(info);
        assert_eq!(view.id, "t-1");
        assert_eq!(view.shell, "cmd.exe");
        assert_eq!(view.cwd.as_deref(), Some("E:\\proj"));
        assert_eq!(view.pid, Some(4242));
        assert_eq!(view.cols, 120);
        assert_eq!(view.rows, 30);
        assert_eq!(view.created_at_ms, 1_700_000_000_000);
    }

    #[test]
    fn view_without_cwd_or_pid_stays_empty() {
        let info = TerminalInfo {
            id: "t-2".into(),
            shell: "sh".into(),
            cwd: None,
            pid: None,
            cols: 80,
            rows: 24,
            created_at_ms: 0,
        };
        let view = view_of(info);
        assert!(view.cwd.is_none());
        assert!(view.pid.is_none());
    }

    #[test]
    fn errors_map_to_their_own_codes() {
        assert_eq!(
            error_of(TerminalError::NotFound("t-1".into())).code,
            ErrorCode::TERMINAL_NOT_FOUND
        );
        assert_eq!(
            error_of(TerminalError::Spawn("boom".into())).code,
            ErrorCode::TERMINAL_SPAWN_FAILED
        );
        assert_eq!(
            error_of(TerminalError::Io("boom".into())).code,
            ErrorCode::TERMINAL_IO_ERROR
        );
        assert_eq!(
            error_of(TerminalError::Invalid("boom".into())).code,
            ErrorCode::TERMINAL_INVALID
        );
    }

    /// 6001 要把 `terminal_id` 带回去：客户端据此决定清哪个标签页。
    #[test]
    fn not_found_error_carries_terminal_id() {
        let error = error_of(TerminalError::NotFound("t-9".into()));
        let data = error.data.expect("6001 应带 data");
        assert_eq!(data["terminal_id"], serde_json::json!("t-9"));
    }
}
