//! `plugin.*` 的协议处理器（插件系统 P0：协议骨架）。
//!
//! P0 阶段这 10 个方法**都能路由到这里**，但一律返回
//! [`ErrorCode::PLUGIN_NOT_FOUND`]（`6101`）。这是刻意的取舍：
//!
//! - 不接线就直接落到 dispatch 的兜底分支，外壳收到的是 `-32601`；
//!   `-32601` 的语义是「这个版本不懂这个方法」，外壳无从区分「本版本没有插件系统」
//!   与「你敲错了方法名」，也没法据此把面板置灰。
//! - `6101` 是**业务错误**：协议层面「能路由」，业务层面「插件没装载」。面板拿到它
//!   可以老老实实写「插件系统尚未启用」，而不用去猜。
//!
//! 因此 `methods::IMPLEMENTED`、`core.hello` 的能力清单、外壳 `policy.ts` 白名单里
//! 都要有这 10 项——能力的定义是「可路由」，不是「业务已实现」。反过来说，等到
//! P1 起真正实现时，这里只换函数体，协议面一行不动。
//!
//! 参数仍然照常解析：空分支也要守住入口契约。`plugin_id` 这种寻址键缺失时必须回
//! `-32602` 而不是 6101——否则一个畸形请求会得到「插件不存在」的答复，排查时会被
//! 带到完全错误的方向。

use ai_ide_protocol::dto::{
    PluginCallParams, PluginGrantParams, PluginInstallParams, PluginListParams, PluginLoadParams,
    PluginReloadParams, PluginRevokeParams, PluginStatusParams, PluginUninstallParams,
    PluginUnloadParams,
};
use ai_ide_protocol::{methods, ErrorCode, ErrorObject, Request};
use serde::de::DeserializeOwned;

use crate::dispatch::parse_params;
use crate::peer::Peer;
use crate::state::AppState;

/// P0 统一话术。带 `method` 后缀，便于日志里看出是哪一个方法被调了。
const NOT_WIRED: &str = "插件宿主尚未接线（协议骨架 P0）";

/// 解析参数；不合法时已回错误，调用方直接返回。
async fn require_params<T: DeserializeOwned>(peer: &Peer, req: &Request) -> Option<T> {
    match parse_params::<T>(req.params.as_ref()) {
        Ok(params) => Some(params),
        Err(err) => {
            peer.respond_error(&req.id, &err).await;
            None
        }
    }
}

/// 空分支统一出口。
async fn not_wired(peer: &Peer, req: &Request, method: &str) {
    peer.respond_error(
        &req.id,
        &ErrorObject::new(
            ErrorCode::PLUGIN_NOT_FOUND,
            format!("{NOT_WIRED}：{method}"),
        ),
    )
    .await;
}

/// `plugin.list`：列已安装插件与状态。
pub async fn list(peer: &Peer, _state: &AppState, req: &Request) {
    if require_params::<PluginListParams>(peer, req)
        .await
        .is_none()
    {
        return;
    }
    not_wired(peer, req, methods::plugin::LIST).await;
}

/// `plugin.status`：单个插件详情。
pub async fn status(peer: &Peer, _state: &AppState, req: &Request) {
    if require_params::<PluginStatusParams>(peer, req)
        .await
        .is_none()
    {
        return;
    }
    not_wired(peer, req, methods::plugin::STATUS).await;
}

/// `plugin.install`：校验并写入插件目录，不启动进程。
pub async fn install(peer: &Peer, _state: &AppState, req: &Request) {
    if require_params::<PluginInstallParams>(peer, req)
        .await
        .is_none()
    {
        return;
    }
    not_wired(peer, req, methods::plugin::INSTALL).await;
}

/// `plugin.uninstall`：卸载运行中的插件并删目录。
pub async fn uninstall(peer: &Peer, _state: &AppState, req: &Request) {
    if require_params::<PluginUninstallParams>(peer, req)
        .await
        .is_none()
    {
        return;
    }
    not_wired(peer, req, methods::plugin::UNINSTALL).await;
}

/// `plugin.load`：授权齐备则起子进程并握手。
pub async fn load(peer: &Peer, _state: &AppState, req: &Request) {
    if require_params::<PluginLoadParams>(peer, req)
        .await
        .is_none()
    {
        return;
    }
    not_wired(peer, req, methods::plugin::LOAD).await;
}

/// `plugin.unload`：停进程并把工具从 agent 注册表下架。
pub async fn unload(peer: &Peer, _state: &AppState, req: &Request) {
    if require_params::<PluginUnloadParams>(peer, req)
        .await
        .is_none()
    {
        return;
    }
    not_wired(peer, req, methods::plugin::UNLOAD).await;
}

/// `plugin.reload`：停 + 起。
pub async fn reload(peer: &Peer, _state: &AppState, req: &Request) {
    if require_params::<PluginReloadParams>(peer, req)
        .await
        .is_none()
    {
        return;
    }
    not_wired(peer, req, methods::plugin::RELOAD).await;
}

/// `plugin.call`：调用网关。
pub async fn call(peer: &Peer, _state: &AppState, req: &Request) {
    if require_params::<PluginCallParams>(peer, req)
        .await
        .is_none()
    {
        return;
    }
    not_wired(peer, req, methods::plugin::CALL).await;
}

/// `plugin.grant`：授予能力。
pub async fn grant(peer: &Peer, _state: &AppState, req: &Request) {
    if require_params::<PluginGrantParams>(peer, req)
        .await
        .is_none()
    {
        return;
    }
    not_wired(peer, req, methods::plugin::GRANT).await;
}

/// `plugin.revoke`：撤销能力（关键能力则连带 unload）。
pub async fn revoke(peer: &Peer, _state: &AppState, req: &Request) {
    if require_params::<PluginRevokeParams>(peer, req)
        .await
        .is_none()
    {
        return;
    }
    not_wired(peer, req, methods::plugin::REVOKE).await;
}
