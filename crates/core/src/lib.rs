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
//! **`terminal.*` 已接线（M6 切片一）**：`terminal.create` / `terminal.write` /
//! `terminal.resize` / `terminal.kill` / `terminal.list` 五个方法在 [`terminal_api`]，
//! PTY 本体在 `crates/terminal`（Windows 走 ConPTY、Unix 走 openpty）。终端输出不走
//! 请求-响应，而是由 [`terminal_api::pump`] 把管理器的事件通道抬成 `terminal.data` /
//! `terminal.exit` 两条单向通知——一次 `ls` 不必等到进程退出才有回执。
//!
//! **`git.*` 已接线（M6 切片三）**：`git.status` / `git.diff` / `git.stage` /
//! `git.commit` / `git.branches` 五个方法在 [`git_api`]，仓库操作在 `crates/git`
//! （系统 git 子进程，`AI_IDE_GIT` 可覆盖可执行文件路径）。这些方法都带
//! `workspace_id` 可选参数，缺省作用于活动工作区；同步的子进程调用统一搬进
//! 阻塞线程池，不占 tokio worker。Git 不新增事件：索引变化已有 `workspace.changed`。
//!
//! **`plugin.*` 已定义、尚未实现（插件系统 P2 的 P0 切片）**：10 个方法的 dispatch
//! 分支都在 [`plugin_api`]，但每个分支只返回 `6101`——协议面先冻结，业务实现留给
//! P1 / P2。这样外壳可以先按 `6101` 把插件面板写成「未启用」，而不是靠 `-32601` 猜。
//!
//! 后续里程碑在此骨架上继续挂载：mcp 等模块。
//!
//! ## 绝对约定：stdout 只跑协议
//!
//! 任何日志、诊断、进度输出**必须**走 stderr。一旦有别的字节混进 stdout，
//! 主进程侧的行解析就会崩，而且这种 bug 极难定位。

pub mod agent_api;
pub mod audit_api;
pub mod checkpoint;
pub mod checkpoint_api;
pub mod context;
pub mod dispatch;
pub mod git_api;
/// Inline Edit 处理器（M3 切片三）：`inline_edit.propose` —— 读选区、补全一次、算 diff。
pub mod inline_edit_api;
pub mod llm_api;
pub mod llm_runtime;
/// 结构化日志（M5 任务 4）：JSONL 按天滚动 + 出口脱敏。
pub mod logging;
/// LSP 协议处理器（LSP 切片二）：`lsp.start` / `stop` / `servers` / `request` / `sync`，
/// 连接本体在 `crates/lsp`，这里只管「按需起、按 id 复用、按 id 关」。
pub mod lsp_api;
/// MCP 协议处理器（M7 切片二）：`mcp.servers` / `connect` / `disconnect` / `tools` / `call`。
pub mod mcp_api;
/// MCP 连接管理器（M7 切片二）：在 core state 里管多 Server 的活连接。
pub mod mcp_manager;
/// MCP 工具注入（M7 切片二）：把 Server 工具包成 `mcp__<server>__<tool>` 挂到 agent 注册表。
pub mod mcp_tools;
pub mod peer;
pub mod permission_api;
/// 插件协议处理器（P0 协议骨架）：10 个 `plugin.*` 方法的空分支。
pub mod plugin_api;
pub mod plugin_approver;
/// 插件审计落行（P4 切片四）：能力变更 / 工具调用 / 生命周期的统一行形状。
pub mod plugin_audit;
/// 插件的读侧宿主能力（P3.5）：`host/fs.read` 等五个方法的闸门、配额与审计。
pub mod plugin_host;
pub mod plugin_llm;
/// 插件的出站网络（P5 切片五）：`host/net.fetch` 的协议 / 域名单 / 地址闸门与逐跳重定向。
pub mod plugin_net;
/// 插件工具注入（P3）：把 `plugin__<id>__<tool>` 挂到 agent 注册表。
pub mod plugin_tools;
pub mod provider_api;
pub mod session_api;
pub mod session_export;
pub mod state;
pub mod sysinfo;
pub mod terminal_api;
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

    // 插件日志的磁盘副本（P5 切片八）：与内核日志同目录、同滚动、同脱敏，
    // 只是文件族从 `core-*` 换成 `plugin-*`。装在 `run()` 而不是 `AppState::new()`：
    // 单测会构造 AppState，装载器跑到用户默认目录里写文件是副作用污染。
    match logging::plugin_log_writer() {
        Some(writer) => {
            info!(dir = %logging::log_dir().display(), "插件日志将追加写入 plugin-*.jsonl");
            state.plugin_host().install_plugin_log_sink(writer);
        }
        None => info!("AI_IDE_LOG_JSONL 已关，插件日志只走事件通道"),
    }
    // 把对端句柄交给状态：`workspace.changed` 这类事件需要主动推送。
    state.attach_peer(Arc::clone(&peer));

    // 终端事件泵：`take_events` 只能取一次，必须在任何 `terminal.create` 之前取走，
    // 否则首个会话建立时的横向输出（shell 提示符 / banner）会随通道一起丢。
    // 通道的生命周期与进程一致，泵任务不需要 join——进程退出时一起收走。
    match state.terminal().take_events() {
        Some(events) => {
            tokio::spawn(terminal_api::pump(Arc::clone(&peer), events));
        }
        None => warn!("终端事件通道已被取走，terminal.data 将无法推送"),
    }

    // 启动即装载落盘目录：进程重启后 Provider 列表不丢（密钥需主进程重新注入）。
    llm_runtime::report_bootstrap(state.llm().bootstrap());

    // MCP（M7 切片二）：读 `mcp.json`，把管理器交给状态。连接**是惰性的**——
    // 这里只解析配置，不拉起任何 Server 子进程；真正的握手发生在 `mcp.connect`。
    // 配置坏了也不致命：空表起步，`mcp.servers` 会如实汇报「没有可用 Server」。
    match ai_ide_mcp::McpConfig::load_default() {
        Ok(config) => {
            let server_count = config.servers.len();
            state.install_mcp(Arc::new(mcp_manager::McpManager::new(config)));
            info!(servers = server_count, "MCP 管理器已就绪（连接惰性）");
        }
        Err(err) => {
            warn!(error = %err, "mcp.json 解析失败，本次以空配置起步");
            state.install_mcp(Arc::new(mcp_manager::McpManager::new(
                ai_ide_mcp::McpConfig::default(),
            )));
        }
    }

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

    // 终端子进程必须显式收掉：ConPTY 里的 shell 已经和内核的 stdin 脱钩，
    // 内核一退它就成了任务管理器里谁也管不了的孤儿进程。
    terminal_api::shutdown_all(&state);

    // 插件子进程同理。Windows 的 Job Object（`KILL_ON_JOB_CLOSE`）只在「内核被强杀」
    // 时兜底，正常退出路径必须自己走一遍 `plugin/shutdown`，让插件有机会落盘收尾。
    shutdown_plugins(&state).await;

    drain_inflight(&mut inflight).await;

    Ok(())
}

/// 停掉所有已装载的插件子进程。
///
/// 抽成函数是因为有两条退出路径：stdin 关闭（[`run`] 末尾）与 `core.shutdown`
/// （[`dispatch`] 里直接 `exit`）。漏掉任何一条，插件都会变成孤儿进程留在任务管理器里
/// ——Windows 上它还会继续占着安装目录，导致下一次卸载删不掉文件。
pub async fn shutdown_plugins(state: &AppState) {
    let running = state.plugins().running_ids();
    if running.is_empty() {
        return;
    }
    info!(plugins = running.len(), "退出：停用已装载插件");
    state.plugins().shutdown_all().await;
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
