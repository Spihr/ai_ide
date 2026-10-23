//! `plugin.*` 的协议处理器（插件系统 P1：清单与安装）。
//!
//! P0 时这 10 个方法一律回 `6101`，作用是「协议面可路由、业务面未实现」。
//! P1 把其中 4 个换成真实现：
//!
//! | 方法 | P1 行为 |
//! |---|---|
//! | `plugin.install` | 校验清单 → 复制到 `<data>/plugins/<id>` → 写账本；**不 spawn 任何进程** |
//! | `plugin.list` | 读账本，按 id 升序 |
//! | `plugin.status` | 读账本 + 回读安装目录里的清单，给出工具/命令明细 |
//! | `plugin.uninstall` | 删账本 + 删目录 |
//!
//! `load` / `unload` / `reload` / `call` / `grant` / `revoke` 仍是 6101：它们要么需要
//! 子进程，要么需要授权面板，属于 P2/P3。**保持它们原样**是刻意的——P1 的验收里有一条
//! 「安装全程不产生进程」，只有把「起进程」的入口继续留在空分支上，这条断言才有意义。
//!
//! ## 错误码约定
//!
//! - 参数缺失/为空 → `-32602`（畸形请求不能答「插件不存在」，那会把排查带偏）；
//! - 清单不合法 → `6107`，`data.details[]` **一次列全**所有问题；
//!   每条 detail 自带细分 `code`（`6104` API 不兼容、`6108` 与核心工具撞名），
//!   所以顶层统一 6107 不会丢信息，外壳只需一个分支就能把所有问题渲染出来；
//! - 插件不存在 → `6101`；
//! - 账本/磁盘读写失败 → `5003`（与 `session.*` 同口径：库坏了就说库坏了）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ai_ide_plugin::{details_json, install as install_package, manifest, PackageError};
use ai_ide_protocol::dto::{
    PluginCallParams, PluginCommandSummary, PluginGrantParams, PluginInstallParams,
    PluginInstallResult, PluginListParams, PluginListResult, PluginLoadParams, PluginReloadParams,
    PluginRevokeParams, PluginState, PluginStatusParams, PluginStatusResult, PluginSummary,
    PluginToolSummary, PluginUninstallParams, PluginUninstallResult, PluginUnloadParams,
};
use ai_ide_protocol::{methods, ErrorCode, ErrorObject, Request};
use ai_ide_store::{plugin_status, NewPlugin, PluginRecord, StoreError};
use serde::de::DeserializeOwned;

use crate::dispatch::parse_params;
use crate::llm_runtime::default_data_dir;
use crate::peer::Peer;
use crate::session_api::store_of;
use crate::state::AppState;

/// P0 遗留话术，供仍未接线的 6 个方法使用。
const NOT_WIRED: &str = "插件宿主尚未接线（P1 只落地清单与安装）";

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

/// `plugin.install`：校验清单、复制包目录、写账本，不启动进程。
pub async fn install(peer: &Peer, state: &AppState, req: &Request) {
    let Some(params) = require_params::<PluginInstallParams>(peer, req).await else {
        return;
    };
    let source = params.path.trim();
    if source.is_empty() {
        return peer
            .respond_error(&req.id, &ErrorObject::invalid_params("path 不能为空"))
            .await;
    }
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    // 复制是阻塞 IO（包可能几百 KB 到几 MB），不放在协议循环的线程上。
    let source = PathBuf::from(source);
    let data_dir = default_data_dir();
    let installed = match tokio::task::spawn_blocking(move || install_package(&source, &data_dir))
        .await
    {
        Ok(Ok(package)) => package,
        Ok(Err(PackageError::Invalid(violations))) => {
            let message = format!("插件清单不合法（{} 条问题）", violations.len());
            return peer
                .respond_error(
                    &req.id,
                    &ErrorObject::new(ErrorCode::PLUGIN_MANIFEST_INVALID, message)
                        .with_data(details_json(&violations)),
                )
                .await;
        }
        Ok(Err(PackageError::Io(message))) => {
            return peer
                .respond_error(
                    &req.id,
                    &ErrorObject::new(ErrorCode::STORE_ERROR, format!("安装插件失败：{message}")),
                )
                .await;
        }
        Err(error) => {
            return peer
                .respond_error(
                    &req.id,
                    &ErrorObject::internal(format!("安装任务异常结束：{error}")),
                )
                .await;
        }
    };

    let id = installed.manifest.manifest.id.clone();
    let previous = match store.list_plugin_capabilities(&id).await {
        Ok(rows) => rows,
        Err(err) => return peer.respond_error(&req.id, &store_error(err)).await,
    };
    let granted_before: HashSet<&str> = previous
        .iter()
        .filter(|row| row.is_granted())
        .map(|row| row.capability.as_str())
        .collect();

    // §5.4：升级时**只保留旧能力已有的授权位**，新增项一律 `granted = 0`。
    // 这正是「v1.0 只要 fs.read，v1.1 偷偷加 fs.write」时状态会退回 `needs_grant` 的原因。
    let capabilities: Vec<(&str, bool)> = installed
        .manifest
        .manifest
        .capabilities
        .iter()
        .map(|capability| {
            (
                capability.as_str(),
                granted_before.contains(capability.as_str()),
            )
        })
        .collect();
    let needs_grant = capabilities.iter().any(|(_, granted)| !granted);
    let status = if needs_grant {
        plugin_status::NEEDS_GRANT
    } else {
        plugin_status::INSTALLED
    };

    let manifest = &installed.manifest.manifest;
    let capabilities_json =
        serde_json::to_string(&manifest.capabilities).unwrap_or_else(|_| "[]".into());
    let commands_json = serde_json::to_string(&manifest.commands).unwrap_or_else(|_| "[]".into());
    let subscriptions_json =
        serde_json::to_string(&manifest.subscriptions).unwrap_or_else(|_| "[]".into());
    let manifest_json = installed.manifest.to_manifest_json();
    let install_dir = installed.dir.to_string_lossy().replace('\\', "/");
    let replaced = installed.replaced || !previous.is_empty();

    let saved = store
        .save_plugin(
            NewPlugin {
                id: &id,
                name: &manifest.name,
                version: &manifest.version,
                description: manifest.description.as_deref(),
                plugin_api: &manifest.plugin_api,
                main: &installed.manifest.entry,
                install_dir: &install_dir,
                manifest_json: &manifest_json,
                capabilities_json: &capabilities_json,
                commands_json: &commands_json,
                subscriptions_json: &subscriptions_json,
                tool_count: manifest.tools.len() as i64,
                status,
                installed_at: None,
            },
            &capabilities,
        )
        .await;

    let record = match saved {
        Ok(record) => record,
        Err(err) => {
            // 账本写不进去就把刚拷的包撤掉：留下一个「磁盘有、账本没有」的目录，
            // 会被下一次安装当成旧版本覆盖，排查时极难看出它从没被登记过。
            if !replaced {
                let data_dir = default_data_dir();
                let id_for_cleanup = id.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    ai_ide_plugin::remove_installed(&data_dir, &id_for_cleanup)
                })
                .await;
            }
            return peer.respond_error(&req.id, &store_error(err)).await;
        }
    };

    let summary = summary_from_caps(&record, &manifest.capabilities, &capabilities);
    tracing::info!(
        plugin_id = %id,
        version = %record.version,
        replaced,
        needs_grant,
        "插件已安装（未启动进程）"
    );
    peer.respond(
        &req.id,
        &PluginInstallResult {
            plugin: summary,
            replaced,
        },
    )
    .await;
}

/// `plugin.list`：列已安装插件与状态。
pub async fn list(peer: &Peer, state: &AppState, req: &Request) {
    if require_params::<PluginListParams>(peer, req)
        .await
        .is_none()
    {
        return;
    }
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let records = match store.list_plugins().await {
        Ok(records) => records,
        Err(err) => return peer.respond_error(&req.id, &store_error(err)).await,
    };

    let mut plugins = Vec::with_capacity(records.len());
    for record in &records {
        let granted = match store.granted_capabilities(&record.id).await {
            Ok(granted) => granted,
            Err(err) => return peer.respond_error(&req.id, &store_error(err)).await,
        };
        let requested = capabilities_of(record);
        plugins.push(summary_from_record(record, &requested, &granted));
    }

    peer.respond(&req.id, &PluginListResult { plugins }).await;
}

/// `plugin.status`：单个插件详情。
pub async fn status(peer: &Peer, state: &AppState, req: &Request) {
    let Some(params) = require_params::<PluginStatusParams>(peer, req).await else {
        return;
    };
    let plugin_id = params.plugin_id.trim().to_string();
    if plugin_id.is_empty() {
        return peer
            .respond_error(&req.id, &ErrorObject::invalid_params("plugin_id 不能为空"))
            .await;
    }
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let record = match store.get_plugin(&plugin_id).await {
        Ok(Some(record)) => record,
        Ok(None) => return peer.respond_error(&req.id, &not_found(&plugin_id)).await,
        Err(err) => return peer.respond_error(&req.id, &store_error(err)).await,
    };
    let granted = match store.granted_capabilities(&plugin_id).await {
        Ok(granted) => granted,
        Err(err) => return peer.respond_error(&req.id, &store_error(err)).await,
    };

    // 工具明细不在账本里（账本只存计数与能力，避免清单原文反复落库），
    // 因此详情页回读安装目录里的 plugin.json。回读失败不影响「插件存在」这个事实，
    // 只是工具/命令列表为空——面板仍能看到版本与授权状态。
    let installed_manifest = read_installed_manifest(&record.install_dir);
    let (tools, commands) = match &installed_manifest {
        Some(manifest) => (tools_of(&plugin_id, manifest), commands_of(manifest)),
        None => (Vec::new(), Vec::new()),
    };
    let requested = capabilities_of(&record);
    let summary = summary_from_record(&record, &requested, &granted);

    peer.respond(
        &req.id,
        &PluginStatusResult {
            plugin: summary,
            tools,
            commands,
        },
    )
    .await;
}

/// `plugin.uninstall`：删账本 + 删安装目录。
///
/// 顺序是「先账本、后目录」：目录被占用（编辑器打开了插件的文件）时，
/// 账本照样该清掉——否则面板上会留一个永远删不掉的幽灵条目。
/// 目录残留通过 `removed_dir = false` 如实告知，让用户自己重试或手工清理。
pub async fn uninstall(peer: &Peer, state: &AppState, req: &Request) {
    let Some(params) = require_params::<PluginUninstallParams>(peer, req).await else {
        return;
    };
    let plugin_id = params.plugin_id.trim().to_string();
    if plugin_id.is_empty() {
        return peer
            .respond_error(&req.id, &ErrorObject::invalid_params("plugin_id 不能为空"))
            .await;
    }
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    match store.get_plugin(&plugin_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return peer.respond_error(&req.id, &not_found(&plugin_id)).await,
        Err(err) => return peer.respond_error(&req.id, &store_error(err)).await,
    }

    if let Err(err) = store.delete_plugin(&plugin_id).await {
        return peer.respond_error(&req.id, &store_error(err)).await;
    }

    let data_dir = default_data_dir();
    let id_for_removal = plugin_id.clone();
    let removed_dir = match tokio::task::spawn_blocking(move || {
        ai_ide_plugin::remove_installed(&data_dir, &id_for_removal)
    })
    .await
    {
        Ok(removed) => removed,
        Err(error) => {
            tracing::warn!(plugin_id = %plugin_id, %error, "卸载任务异常结束");
            false
        }
    };

    tracing::info!(plugin_id = %plugin_id, removed_dir, "插件已卸载");
    // P1 从不加载插件，agent 注册表里没有任何插件工具可下架，因此恒为 0；
    // P2 起这里要换成「实际从注册表摘掉的条数」。
    peer.respond(
        &req.id,
        &PluginUninstallResult {
            plugin_id,
            removed_tools: 0,
            removed_dir,
        },
    )
    .await;
}

/// `plugin.load`：授权齐备则起子进程并握手（P2）。
pub async fn load(peer: &Peer, _state: &AppState, req: &Request) {
    if require_params::<PluginLoadParams>(peer, req)
        .await
        .is_none()
    {
        return;
    }
    not_wired(peer, req, methods::plugin::LOAD).await;
}

/// `plugin.unload`：停进程并把工具从 agent 注册表下架（P2）。
pub async fn unload(peer: &Peer, _state: &AppState, req: &Request) {
    if require_params::<PluginUnloadParams>(peer, req)
        .await
        .is_none()
    {
        return;
    }
    not_wired(peer, req, methods::plugin::UNLOAD).await;
}

/// `plugin.reload`：停 + 起（P2）。
pub async fn reload(peer: &Peer, _state: &AppState, req: &Request) {
    if require_params::<PluginReloadParams>(peer, req)
        .await
        .is_none()
    {
        return;
    }
    not_wired(peer, req, methods::plugin::RELOAD).await;
}

/// `plugin.call`：调用网关（P3）。
pub async fn call(peer: &Peer, _state: &AppState, req: &Request) {
    if require_params::<PluginCallParams>(peer, req)
        .await
        .is_none()
    {
        return;
    }
    not_wired(peer, req, methods::plugin::CALL).await;
}

/// `plugin.grant`：授予能力（P2）。
pub async fn grant(peer: &Peer, _state: &AppState, req: &Request) {
    if require_params::<PluginGrantParams>(peer, req)
        .await
        .is_none()
    {
        return;
    }
    not_wired(peer, req, methods::plugin::GRANT).await;
}

/// `plugin.revoke`：撤销能力（P2）。
pub async fn revoke(peer: &Peer, _state: &AppState, req: &Request) {
    if require_params::<PluginRevokeParams>(peer, req)
        .await
        .is_none()
    {
        return;
    }
    not_wired(peer, req, methods::plugin::REVOKE).await;
}

// ---------------------------------------------------------------------------
// 视图拼装
// ---------------------------------------------------------------------------

/// 账本状态串 → 协议枚举。
///
/// 账本的 `installed` 与协议的 `Disabled` 是同一件事的两种叫法：装上但没加载。
/// 强行把账本改名叫 `disabled` 会让 SQL 里出现「状态叫 disabled 但其实是装好了」的
/// 反直觉命名，宁可在边界做一次映射。
fn state_of(record: &PluginRecord) -> PluginState {
    match record.status.as_str() {
        plugin_status::NEEDS_GRANT => PluginState::NeedsGrant,
        plugin_status::READY => PluginState::Ready,
        plugin_status::CRASHED => PluginState::Crashed,
        _ => PluginState::Disabled,
    }
}

/// 账本里申请的能力列表（`capabilities_json` 解析失败时退化为空表）。
fn capabilities_of(record: &PluginRecord) -> Vec<String> {
    serde_json::from_str(&record.capabilities_json).unwrap_or_default()
}

fn summary_from_record(
    record: &PluginRecord,
    requested: &[String],
    granted: &[String],
) -> PluginSummary {
    PluginSummary {
        id: record.id.clone(),
        name: record.name.clone(),
        version: record.version.clone(),
        description: record.description.clone(),
        state: state_of(record),
        capabilities: requested.to_vec(),
        granted_capabilities: granted.to_vec(),
        tool_count: record.tool_count.max(0) as u32,
        last_error: record.last_error.clone(),
    }
}

/// 把「申请 + 授权位」折成响应摘要（安装路径专用：手上有授权位，不必再查库）。
fn summary_from_caps(
    record: &PluginRecord,
    requested: &[String],
    capabilities: &[(&str, bool)],
) -> PluginSummary {
    let owned: Vec<String> = capabilities
        .iter()
        .filter(|(_, granted)| *granted)
        .map(|(capability, _)| (*capability).to_string())
        .collect();
    summary_from_record(record, requested, &owned)
}

/// 读取安装目录里的清单（用于 `plugin.status` 的工具明细）。
fn read_installed_manifest(install_dir: &str) -> Option<manifest::Manifest> {
    match ai_ide_plugin::install::inspect(Path::new(install_dir)) {
        Ok(validated) => Some(validated.manifest),
        Err(error) => {
            tracing::warn!(%install_dir, %error, "回读已安装插件清单失败，详情页工具列表为空");
            None
        }
    }
}

fn tools_of(plugin_id: &str, manifest: &manifest::Manifest) -> Vec<PluginToolSummary> {
    manifest
        .tools
        .iter()
        .map(|tool| PluginToolSummary {
            name: tool.name.clone(),
            namespaced_name: ai_ide_plugin::namespaced_tool(plugin_id, &tool.name),
            // 清单允许省略 description（校验里只告警）。回给模型时给个能读的兜底，
            // 空字符串会让模型完全猜不出这个工具干什么。
            description: tool
                .description
                .clone()
                .unwrap_or_else(|| format!("{} 插件提供的工具 {}", manifest.name, tool.name)),
            risk: tool.risk,
            input_schema: tool.parameters.clone(),
        })
        .collect()
}

fn commands_of(manifest: &manifest::Manifest) -> Vec<PluginCommandSummary> {
    manifest
        .commands
        .iter()
        .map(|command| PluginCommandSummary {
            name: command.name.clone(),
            title: command.title.clone(),
            description: command.description.clone(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 错误映射
// ---------------------------------------------------------------------------

fn not_found(plugin_id: &str) -> ErrorObject {
    ErrorObject::new(
        ErrorCode::PLUGIN_NOT_FOUND,
        format!("插件不存在或未安装：{plugin_id}"),
    )
    .with_data(serde_json::json!({ "plugin_id": plugin_id }))
}

/// 账本错误映射：`PluginNotFound` 是「插件不存在」，其余是库本身出问题。
fn store_error(err: StoreError) -> ErrorObject {
    match err {
        StoreError::PluginNotFound(id) => not_found(&id),
        other => ErrorObject::new(ErrorCode::STORE_ERROR, format!("插件账本读写失败：{other}")),
    }
}
