//! # ai-ide-core
//!
//! AI IDE 的 Rust 内核。
//!
//! **M1 阶段的职责**：在 M0 的通信骨架上接入工作区能力——打开 / 关闭工作区、
//! 目录树、文件读写、创建 / 重命名 / 删除 / 定位，以及文件名模糊检索与内容检索。
//! 通信部分不变：从 stdin 逐行读 NDJSON → 分发到处理器 → 结果写回 stdout，
//! 另有 `core.*` 四个基础方法与 `$/cancelRequest` 取消机制。
//!
//! 已实现的方法全集见 [`ai_ide_protocol::methods::IMPLEMENTED`]，
//! `workspace.*` / `search.*` 的处理集中在 [`ws`] 模块。
//!
//! 后续里程碑在此骨架上继续挂载：llm / agent / terminal / mcp / store 等模块。
//!
//! ## 绝对约定：stdout 只跑协议
//!
//! 任何日志、诊断、进度输出**必须**走 stderr。一旦有别的字节混进 stdout，
//! 主进程侧的行解析就会崩，而且这种 bug 极难定位。

pub mod dispatch;
pub mod peer;
pub mod state;
pub mod sysinfo;
pub mod version;
pub mod ws;

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{info, warn};

use crate::peer::Peer;
use crate::state::AppState;

/// 内核主循环：读 stdin → 分发 → 写 stdout。
///
/// 每条到达的报文都会被 spawn 成一个独立任务，避免慢请求阻塞后续报文的读取；
/// 写回 stdout 由 [`Peer`] 内部串行化，保证帧不会交错。
pub async fn run() -> Result<()> {
    let state = Arc::new(AppState::new());
    let peer = Arc::new(Peer::new());
    // 把对端句柄交给状态：`workspace.changed` 这类事件需要主动推送。
    state.attach_peer(Arc::clone(&peer));

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    // 收拢在途请求，退出前统一收尾（见 `drain_inflight`）。
    let mut inflight = tokio::task::JoinSet::new();

    info!(
        core_version = version::CORE_VERSION,
        protocol_version = ai_ide_protocol::PROTOCOL_VERSION,
        "内核已启动，等待握手"
    );

    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                let peer = Arc::clone(&peer);
                let state = Arc::clone(&state);
                inflight.spawn(async move {
                    dispatch::handle_line(&peer, &state, &line).await;
                });
            }
            Ok(None) => {
                // stdin 关闭：父进程退出、管道断开，或客户端一次性写完后关闭。
                // 不能立刻返回——刚收到、还没写完的响应会被一起丢掉。
                info!("stdin 已关闭，等待在途请求收尾");
                break;
            }
            Err(err) => {
                warn!(error = %err, "读取 stdin 失败，内核退出");
                break;
            }
        }
    }

    drain_inflight(&mut inflight).await;

    Ok(())
}

/// 等在途请求把响应写完再退出。
///
/// `core.shutdown` 之外的请求也可能正在写 stdout：如果 stdin 一关就 `return`，
/// 这些响应会随进程一起消失，客户端看到的是「没有响应」而不是「报错」——
/// 这类问题最难查，因为日志里一切正常。超时是为了防止某个卡死的请求
/// 把内核吊在退出路径上。
async fn drain_inflight(inflight: &mut tokio::task::JoinSet<()>) {
    let drained = tokio::time::timeout(Duration::from_secs(5), async {
        while inflight.join_next().await.is_some() {}
    })
    .await;

    if drained.is_err() {
        warn!("在途请求收尾超时，放弃等待");
    }
}
