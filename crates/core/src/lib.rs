//! # ai-ide-core
//!
//! AI IDE 的 Rust 内核。
//!
//! **M2 阶段的职责**：把 Provider / 模型目录接进通信骨架——`provider.upsert` /
//! `provider.list` / `provider.delete` / `provider.test` 与 `model.upsert` /
//! `model.list` / `model.delete` 七个方法真正可路由，目录落盘在
//! `AI_IDE_DATA_DIR`（缺省 `%APPDATA%\ai-ide`）下的 `providers.json`。
//! M1 的工作区能力（打开 / 目录树 / 读写 / 检索）保持不变。
//!
//! **`llm.*` 已接线**：`llm.chat_stream` / `llm.cancel` / `llm.list_models` /
//! `llm.count_tokens` 四个方法在 [`llm_api`] 里，会把 `ai-ide-llm` 的运行期事件
//! 翻成 `llm.started` / `llm.delta` / `llm.reasoning` / `llm.tool_call` /
//! `llm.usage` / `llm.finished` / `llm.error` 推送，并保证「一次流恰好一条开始事件、
//! `finished` 与 `error` 互斥」这两条不变量。因此
//! [`ai_ide_protocol::methods::DECLARED_NOT_IMPLEMENTED`] 现在是空表——
//! 那两张表互斥，谁没接线谁才待在那里。
//!
//! 已实现的方法全集见 [`ai_ide_protocol::methods::IMPLEMENTED`]：
//! `workspace.*` / `search.*` 在 [`ws`] 模块，`provider.*` / `model.*` 在
//! [`provider_api`] 模块，两者共用的运行期状态在 [`state`] / [`llm_runtime`]。
//!
//! **M3 起 `llm.chat_stream` 消费上下文引用**：`context.refs` 里的
//! `@文件 / @文件夹 / @选区 / 当前文件` 由 [`context`] 渲染成带行号与来源标注的文本，
//! 按 token 预算裁剪后插成第一条 `system` 消息；越界路径直接拒绝请求。
//! `llm.count_tokens` 同时接受 `messages` 与 `texts`；认识的模型族按 tiktoken 真实词表
//! 精确计数（`exact:true`），其余回落估算（`exact:false`），字段含义不变。
//! 引用预算裁剪仍走估算口径，不因精确计数而改变。
//!
//! 后续里程碑在此骨架上继续挂载：agent / terminal / mcp / store 等模块。
//!
//! ## 绝对约定：stdout 只跑协议
//!
//! 任何日志、诊断、进度输出**必须**走 stderr。一旦有别的字节混进 stdout，
//! 主进程侧的行解析就会崩，而且这种 bug 极难定位。

pub mod agent_api;
pub mod context;
pub mod dispatch;
pub mod llm_api;
pub mod llm_runtime;
pub mod peer;
pub mod provider_api;
pub mod session_api;
pub mod session_export;
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

/// 会话库文件路径：与 `providers.json` 同目录（`AI_IDE_DATA_DIR` 优先）。
fn session_store_path() -> std::path::PathBuf {
    llm_runtime::default_data_dir().join("sessions.db")
}

/// 内核主循环：读 stdin → 分发 → 写 stdout。
///
/// 每条到达的报文都会被 spawn 成一个独立任务，避免慢请求阻塞后续报文的读取；
/// 写回 stdout 由 [`Peer`] 内部串行化，保证帧不会交错。
pub async fn run() -> Result<()> {
    let state = Arc::new(AppState::new());
    let peer = Arc::new(Peer::new());
    // 把对端句柄交给状态：`workspace.changed` 这类事件需要主动推送。
    state.attach_peer(Arc::clone(&peer));

    // 启动即装载落盘目录：进程重启后 Provider 列表不丢（密钥需主进程重新注入）。
    llm_runtime::report_bootstrap(state.llm().bootstrap());

    // 会话库：与 `providers.json` 同目录。连不上不致命——工作区 / 模型 / Agent 能力
    // 照常可用，`session.*` 会如实返回 5002，而不是把消息悄悄丢掉。
    let session_db = session_store_path();
    match ai_ide_store::Store::connect(&session_db).await {
        Ok(store) => {
            info!(path = %session_db.display(), "会话库已就绪");
            state.install_store(store);
        }
        Err(err) => warn!(
            path = %session_db.display(),
            error = %err,
            "会话库打开失败，session.* 将返回 5002"
        ),
    }

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

    // 在途流式对话不在 `inflight` 里（它读的是上游，不是 stdin），得单独阻断：
    // 否则退出时那条任务的请求会一直挂在网络上，直到进程被系统收走。
    let blocked = state.streams().cancel_all();
    if blocked > 0 {
        info!(streams = blocked, "退出：已阻断在途流式对话");
    }
    // agent 运行同理：它跑在独立任务里等模型，而模型那头已经被上面那句掐断，
    // 这里补一句是为了让「被停止」这件事在循环里立刻可见，不必等下一轮。
    let runs = state.agent().cancel_all();
    if runs > 0 {
        info!(runs, "退出：已阻断在途 agent 运行");
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
