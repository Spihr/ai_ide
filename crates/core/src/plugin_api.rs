//! `plugin.*` 的协议处理器（插件系统 P1：清单与安装 / P2：装载与握手）。
//!
//! P0 时这 10 个方法一律回 `6101`，作用是「协议面可路由、业务面未实现」。
//! P1 把 4 个换成真实现，P2 再补 3 个：
//!
//! | 方法 | 行为 |
//! |---|---|
//! | `plugin.install` | 校验清单 → 复制到 `<data>/plugins/<id>` → 写账本；**不 spawn 任何进程** |
//! | `plugin.list` | 读账本，按 id 升序（先做一次崩溃巡检） |
//! | `plugin.status` | 读账本 + 回读安装目录里的清单，给出工具/命令明细（先做一次崩溃巡检） |
//! | `plugin.uninstall` | 停进程 → 删账本 → 删目录 |
//! | `plugin.load` | 授权齐备 → 起子进程 → 握手 → 校验工具集合 → 落 `ready`；失败按类落账本 |
//! | `plugin.unload` | 停进程 → 工具下架 → 落 `installed` |
//! | `plugin.reload` | 停 + 起，账本 `restarts` 不变（那计数只记崩溃后的自动重启） |
//! | `plugin.grant` | 授权位落账本 → 当场回写运行态授权集（不 reload 即生效） |
//! | `plugin.revoke` | 撤授权位 → 回写授权集；撤掉关键能力则连带 `unload` 并退回 `needs_grant` |
//!
//! `call` 仍是 6101：它属于 P4 的网关切片，本切片只做授权面。
//! 「安装全程不产生进程」这条 P1 验收仍成立——只有 `load` / `reload` 与崩溃后的
//! 自动重启会 `spawn`。
//!
//! ## 账本状态机
//!
//! ```text
//! install ──► installed ──load──► ready ──unload──► installed
//!                │                 │
//!       缺授权   │                 │ 意外退出（工具立即下架）
//!                ▼                 ▼
//!           needs_grant     自动重启成功 ──► ready（restarts +1）
//!                                  │
//!                                  └─ 连续 3 次仍失败 ──► crashed ──load──► ready
//! ```
//!
//! 崩溃巡检（[`sweep_crashed`]）在宿主判定「进程已退出」后立刻做两件事：
//!
//! 1. 工具下架——`auto_restart` 会先把条目从装载表摘掉，摘掉的那一瞬间工具就不再
//!    可路由，这是「崩溃 → 工具立即下架」的落点；
//! 2. 自动重启——非确定性退出（进程自己死了 / 握手超时）才重启，同一装载会话最多
//!    3 次；确定性失败（协议不合规、工具集合与清单不一致、`pluginApi` 不兼容、入口
//!    丢失）一次都不重试，因为再跑一遍必然还是同一个错误。
//!
//! 到顶或撞上确定性失败后账本落在 `crashed`，`last_error` 里写明原因——「已达重启
//! 上限」这句话就是面板上那句「停在已崩溃」的依据。此后只有用户显式 `load` 才会
//! 再起进程（那是新的一次装载会话，重启预算重新给 3 次）。
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

use ai_ide_plugin::host::{CallError, LoadError, LoadSpec, Restart};
use ai_ide_plugin::{details_json, install as install_package, manifest, PackageError};
use ai_ide_protocol::dto::{
    PluginCallParams, PluginCallResult, PluginCommandSummary, PluginGrantParams, PluginGrantResult,
    PluginInstallParams, PluginInstallResult, PluginListParams, PluginListResult, PluginLoadParams,
    PluginLoadResult, PluginReloadParams, PluginReloadResult, PluginRevokeParams,
    PluginRevokeResult, PluginState, PluginStatusParams, PluginStatusResult, PluginSummary,
    PluginToolRisk, PluginToolSummary, PluginUninstallParams, PluginUninstallResult,
    PluginUnloadParams, PluginUnloadResult,
};
use ai_ide_protocol::{ErrorCode, ErrorObject, Request};
use ai_ide_store::{audit_kind, plugin_status, NewPlugin, PluginRecord, Store, StoreError};
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::dispatch::parse_params;
use crate::llm_runtime::default_data_dir;
use crate::peer::Peer;
use crate::plugin_audit::{
    self, PluginAuditRow, ACTION_CALL, ACTION_GRANT, ACTION_LOAD, ACTION_RELOAD, ACTION_REVOKE,
    ACTION_UNLOAD,
};
use crate::plugin_host::{DECISION_ALLOW, DECISION_DENY, DECISION_ERROR};
use crate::session_api::store_of;
use crate::state::AppState;

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
    // 列表面板不该展示「上一秒就已经崩了、但账本还写着 ready」的陈旧状态。
    sweep_crashed(state, store).await;
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

    // 详情页要能如实显示 `crashed`：崩溃是子进程结束，账本不会自己知道。
    sweep_crashed(state, store).await;
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

    // 卸载前必须先停进程：目录被一个还在跑的插件占着时，Windows 会拒绝删文件，
    // 结果就是「账本没了、进程还在、目录残留」——最难解释的一种半成品状态。
    // `removed_tools` 只在真的从装载表摘掉东西时才非零（P2）。
    let removed_tools = state
        .plugins()
        .unload(&plugin_id)
        .await
        .map(|tools| tools.len() as u32)
        .unwrap_or(0);

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

    tracing::info!(plugin_id = %plugin_id, removed_dir, removed_tools, "插件已卸载");
    peer.respond(
        &req.id,
        &PluginUninstallResult {
            plugin_id,
            removed_tools,
            removed_dir,
        },
    )
    .await;
}

/// `plugin.load`：授权齐备则起子进程并握手（P2）。
pub async fn load(peer: &Peer, state: &AppState, req: &Request) {
    let Some(params) = require_params::<PluginLoadParams>(peer, req).await else {
        return;
    };
    let Some(plugin_id) = require_plugin_id(peer, req, &params.plugin_id).await else {
        return;
    };
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    // 装载前先巡检：同一 id 若上一轮已经崩了，账本要先落 `crashed`，
    // 这次装载才谈得上「restarts + 1」。
    sweep_crashed(state, store).await;

    match load_into(state, store, &plugin_id).await {
        Ok(result) => {
            plugin_audit::record_user(
                store,
                PluginAuditRow::new(
                    audit_kind::PLUGIN_LIFECYCLE,
                    &plugin_id,
                    ACTION_LOAD,
                    DECISION_ALLOW,
                )
                .target(Some(result.plugin.id.as_str()))
                .note("tools", json!(result.tools.len()))
                .note("state", json!(result.plugin.state)),
            )
            .await;
            peer.respond(&req.id, &result).await
        }
        Err(err) => {
            // 起不来也是事件：审计里要能对上「面板上这个插件从 ready 掉成 crashed」
            // 的那一刻，而不是只看到一次工具调用失败。
            plugin_audit::record_user(
                store,
                PluginAuditRow::new(
                    audit_kind::PLUGIN_LIFECYCLE,
                    &plugin_id,
                    ACTION_LOAD,
                    DECISION_DENY,
                )
                .note("code", json!(err.code.as_i32()))
                .note("message", json!(err.message)),
            )
            .await;
            peer.respond_error(&req.id, &err).await
        }
    }
}

/// `plugin.unload`：停进程并把工具从 agent 注册表下架（P2）。
pub async fn unload(peer: &Peer, state: &AppState, req: &Request) {
    let Some(params) = require_params::<PluginUnloadParams>(peer, req).await else {
        return;
    };
    let Some(plugin_id) = require_plugin_id(peer, req, &params.plugin_id).await else {
        return;
    };
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    sweep_crashed(state, store).await;

    match store.get_plugin(&plugin_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            plugin_audit::record_user(
                store,
                PluginAuditRow::new(
                    audit_kind::PLUGIN_LIFECYCLE,
                    &plugin_id,
                    ACTION_UNLOAD,
                    DECISION_DENY,
                )
                .note("code", json!(ErrorCode::PLUGIN_NOT_FOUND.as_i32())),
            )
            .await;
            return peer.respond_error(&req.id, &not_found(&plugin_id)).await;
        }
        Err(err) => return peer.respond_error(&req.id, &store_error(err)).await,
    }

    // `None` = 这个插件已经不在装载表里（没 load 过，或刚刚崩了被收走）。
    // 两种情况都算「工具已下架」，回 0 条而不是报错——卸载一个没跑的插件是合法操作。
    let removed_tools = state
        .plugins()
        .unload(&plugin_id)
        .await
        .map(|tools| tools.len() as u32)
        .unwrap_or(0);

    // 用户显式停用：`crashed` / `ready` 都退回 `installed`，顺手抹掉上次的错误，
    // 否则面板会一直挂着一句「上次崩溃」的旧话术，看起来像刚发生的。
    if let Err(err) = store
        .set_plugin_status(&plugin_id, plugin_status::INSTALLED, None)
        .await
    {
        return peer.respond_error(&req.id, &store_error(err)).await;
    }
    // 卸载后它的 host/* 授权一并作废：进程已经没了，留着这份授权只会让下一次
    // 同名重装继承上一轮的权限。
    state.plugin_host().set_granted_for(&plugin_id, Vec::new());

    plugin_audit::record_user(
        store,
        PluginAuditRow::new(
            audit_kind::PLUGIN_LIFECYCLE,
            &plugin_id,
            ACTION_UNLOAD,
            DECISION_ALLOW,
        )
        .target(Some(plugin_id.as_str()))
        .note("removedTools", json!(removed_tools)),
    )
    .await;

    tracing::info!(plugin_id = %plugin_id, removed_tools, "插件已卸载");
    peer.respond(
        &req.id,
        &PluginUnloadResult {
            plugin_id,
            removed_tools,
        },
    )
    .await;
}

/// `plugin.reload`：停 + 起（P2）。
pub async fn reload(peer: &Peer, state: &AppState, req: &Request) {
    let Some(params) = require_params::<PluginReloadParams>(peer, req).await else {
        return;
    };
    let Some(plugin_id) = require_plugin_id(peer, req, &params.plugin_id).await else {
        return;
    };
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    sweep_crashed(state, store).await;

    match store.get_plugin(&plugin_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return peer.respond_error(&req.id, &not_found(&plugin_id)).await,
        Err(err) => return peer.respond_error(&req.id, &store_error(err)).await,
    }

    // 先停再起。停不下来就直接报错：带着旧进程起新进程会得到两个抢同一份状态
    // 的插件，工具注册也会出现重名，后面所有排查都会被这份脏状态带偏。
    let _ = state.plugins().unload(&plugin_id).await;

    match load_into(state, store, &plugin_id).await {
        Ok(result) => {
            plugin_audit::record_user(
                store,
                PluginAuditRow::new(
                    audit_kind::PLUGIN_LIFECYCLE,
                    &plugin_id,
                    ACTION_RELOAD,
                    DECISION_ALLOW,
                )
                .target(Some(result.plugin.id.as_str()))
                .note("tools", json!(result.tools.len()))
                .note("restarts", json!(result.plugin.restarts)),
            )
            .await;
            peer.respond(
                &req.id,
                &PluginReloadResult {
                    plugin: result.plugin,
                    tools: result.tools,
                },
            )
            .await
        }
        Err(err) => {
            plugin_audit::record_user(
                store,
                PluginAuditRow::new(
                    audit_kind::PLUGIN_LIFECYCLE,
                    &plugin_id,
                    ACTION_RELOAD,
                    DECISION_DENY,
                )
                .note("code", json!(err.code.as_i32()))
                .note("message", json!(err.message)),
            )
            .await;
            peer.respond_error(&req.id, &err).await
        }
    }
}

// ---------------------------------------------------------------------------
// 装载实现（P2）
// ---------------------------------------------------------------------------

/// 统一校验 `plugin_id`（空白值不能答「插件不存在」，那是 -32602）。
async fn require_plugin_id(peer: &Peer, req: &Request, raw: &str) -> Option<String> {
    let plugin_id = raw.trim();
    if plugin_id.is_empty() {
        peer.respond_error(&req.id, &ErrorObject::invalid_params("plugin_id 不能为空"))
            .await;
        return None;
    }
    Some(plugin_id.to_string())
}

/// 一次完整的装载：回读清单 → 查授权 → 起进程握手 → 落账本。
///
/// 失败时也写账本：面板要能解释「为什么它没起来」，只回一个错误码说明不了。
async fn load_into(
    state: &AppState,
    store: &Store,
    plugin_id: &str,
) -> Result<PluginLoadResult, ErrorObject> {
    let record = match store.get_plugin(plugin_id).await {
        Ok(Some(record)) => record,
        Ok(None) => return Err(not_found(plugin_id)),
        Err(err) => return Err(store_error(err)),
    };

    // 装目录是用户可写的地方：账本记着 `ready`，磁盘上的文件却可能已经被人删了。
    // 先判存在，再回读清单——顺序反了会把「目录没了」报成「清单不合法」。
    let install_dir = PathBuf::from(&record.install_dir);
    if !install_dir.is_dir() {
        let message = format!("插件安装目录已不存在：{}（请重新安装）", record.install_dir);
        let _ = store
            .set_plugin_status(plugin_id, plugin_status::CRASHED, Some(&message))
            .await;
        return Err(ErrorObject::new(ErrorCode::PLUGIN_NOT_FOUND, message)
            .with_data(serde_json::json!({ "plugin_id": plugin_id })));
    }
    let validated = match ai_ide_plugin::install::inspect(&install_dir) {
        Ok(validated) => validated,
        Err(PackageError::Invalid(violations)) => {
            let message = format!("插件清单不合法（{} 条问题）", violations.len());
            let _ = store
                .set_plugin_status(plugin_id, plugin_status::CRASHED, Some(&message))
                .await;
            return Err(
                ErrorObject::new(ErrorCode::PLUGIN_MANIFEST_INVALID, message)
                    .with_data(details_json(&violations)),
            );
        }
        Err(PackageError::Io(message)) => {
            return Err(ErrorObject::new(
                ErrorCode::STORE_ERROR,
                format!("读取插件安装目录失败：{message}"),
            ));
        }
    };

    let granted = match store.granted_capabilities(plugin_id).await {
        Ok(granted) => granted,
        Err(err) => return Err(store_error(err)),
    };
    let requested = validated.manifest.capabilities.clone();

    let spec = LoadSpec {
        plugin_id,
        install_dir: &install_dir,
        validated: &validated,
        requested: requested.clone(),
        granted: granted.clone(),
        workspace: workspace_json(state),
    };

    match state.plugins().load(spec).await {
        Ok(loaded) => {
            // 工具明细以握手为准：清单只声明「有哪些工具」，描述/风险/schema 允许
            // 插件在运行时收窄（校验已保证名字集合一致）。
            let tools = tools_from_handshake(plugin_id, &validated.manifest, &loaded.tools);
            // 这里**不**动 `restarts`：计数只记「崩溃后的自动重启」（见
            // `recover_crashed`）。用户手动 `load` 一次失败过的插件，是他自己按下
            // 按钮重试，不是内核自动重启，记进同一个计数会让面板把「用户重试」
            // 说成「内核重启了 N 次」。
            if let Err(err) = store
                .set_plugin_status(plugin_id, plugin_status::READY, None)
                .await
            {
                // 账本写不进去就把刚起来的进程停掉：留下一个「在跑但账本说没跑」
                // 的插件，下一次 load 会造出第二个进程，工具也会重名。
                let _ = state.plugins().unload(plugin_id).await;
                return Err(store_error(err));
            }

            let mut fresh = record.clone();
            fresh.status = plugin_status::READY.to_string();
            fresh.last_error = None;
            // 授权集同时塞进宿主能力层：`plugin.grant` 改的是账本，而 `host/*` 的
            // 能力闸门要**当场**知道结果，不能每次调用都回库查一遍。
            state
                .plugin_host()
                .set_granted_for(plugin_id, granted.clone());
            tracing::info!(
                plugin_id = %plugin_id,
                pid = ?loaded.pid,
                reused = loaded.reused,
                tools = tools.len(),
                "插件已装载并握手成功"
            );
            Ok(PluginLoadResult {
                plugin: summary_from_record(&fresh, &requested, &granted),
                tools,
            })
        }
        Err(error) => {
            let mapped = load_error(plugin_id, &error);
            let status = if matches!(error, LoadError::MissingCapabilities { .. }) {
                plugin_status::NEEDS_GRANT
            } else {
                plugin_status::CRASHED
            };
            let _ = store
                .set_plugin_status(plugin_id, status, Some(&error.to_string()))
                .await;
            Err(mapped)
        }
    }
}

/// 握手工具声明 → 协议视图（描述/风险/schema 缺省时回落清单里的同类字段）。
fn tools_from_handshake(
    plugin_id: &str,
    declared: &manifest::Manifest,
    tools: &[ai_ide_plugin::host::HandshakeTool],
) -> Vec<PluginToolSummary> {
    tools
        .iter()
        .map(|tool| {
            let fallback = declared
                .tools
                .iter()
                .find(|candidate| candidate.name == tool.name);
            PluginToolSummary {
                name: tool.name.clone(),
                namespaced_name: ai_ide_plugin::namespaced_tool(plugin_id, &tool.name),
                description: tool
                    .description
                    .clone()
                    .or_else(|| fallback.and_then(|item| item.description.clone()))
                    .unwrap_or_else(|| format!("{} 插件提供的工具 {}", declared.name, tool.name)),
                risk: tool
                    .risk
                    .or_else(|| fallback.map(|item| item.risk))
                    .unwrap_or(PluginToolRisk::Read),
                input_schema: tool
                    .parameters
                    .clone()
                    .or_else(|| fallback.map(|item| item.parameters.clone()))
                    .unwrap_or_else(|| serde_json::json!({ "type": "object" })),
            }
        })
        .collect()
}

/// 握手参数里的 `workspace`：没打开工作区就是 `null`，不编一个假路径。
fn workspace_json(state: &AppState) -> serde_json::Value {
    match state.workspace(None) {
        Ok(workspace) => serde_json::json!({
            "id": workspace.id,
            "root": workspace.root.to_string_lossy().replace('\\', "/"),
        }),
        Err(_) => serde_json::Value::Null,
    }
}

/// 装载失败 → 61xx。错误码按「外壳该给用户看哪种补救」划分，不按内部枚举名。
fn load_error(plugin_id: &str, error: &LoadError) -> ErrorObject {
    match error {
        LoadError::NotInstalled(dir) => ErrorObject::new(
            ErrorCode::PLUGIN_NOT_FOUND,
            format!("插件安装目录不存在：{dir}"),
        )
        .with_data(serde_json::json!({ "plugin_id": plugin_id })),
        LoadError::MissingCapabilities { missing } => ErrorObject::new(
            ErrorCode::PLUGIN_CAPABILITY_DENIED,
            format!("插件需要的能力尚未授权：{}", missing.join(", ")),
        )
        .with_data(serde_json::json!({
            "plugin_id": plugin_id,
            "missing": missing,
        })),
        LoadError::Spawn(spawn) => ErrorObject::new(
            ErrorCode::PLUGIN_CRASHED,
            format!("拉起插件进程失败：{spawn}"),
        )
        .with_data(serde_json::json!({ "plugin_id": plugin_id })),
        LoadError::Timeout { stage, timeout } => ErrorObject::new(
            ErrorCode::PLUGIN_TIMEOUT,
            format!("{stage}超时（{} ms）", timeout.as_millis()),
        )
        .with_data(serde_json::json!({ "plugin_id": plugin_id })),
        LoadError::Crashed { code, tail } => ErrorObject::new(
            ErrorCode::PLUGIN_CRASHED,
            match code {
                Some(code) => format!("插件进程在握手期间退出（退出码 {code}）"),
                None => "插件进程在握手期间退出".to_string(),
            },
        )
        .with_data(serde_json::json!({
            "plugin_id": plugin_id,
            "exit_code": code,
            "stderr_tail": tail,
        })),
        LoadError::Protocol { message } => ErrorObject::new(
            ErrorCode::PLUGIN_PROTOCOL_VIOLATION,
            format!("插件协议错误：{message}"),
        )
        .with_data(serde_json::json!({ "plugin_id": plugin_id })),
        LoadError::ApiMismatch { plugin, host } => ErrorObject::new(
            ErrorCode::PLUGIN_API_MISMATCH,
            format!("pluginApi {plugin} 与宿主 {host} 不兼容"),
        )
        .with_data(serde_json::json!({
            "plugin_id": plugin_id,
            "plugin_api": plugin,
            "host_api": host,
        })),
    }
}

/// 崩溃巡检：宿主判死的插件先下架工具，再尝试自动重启，最后把账本写成真实状态。
///
/// 三种结局对应的账本写法：
///
/// - 自动重启成功 → `ready` + `restarts + 1`（工具已重新上架，`last_error` 清空）；
/// - 撞上确定性失败 → `crashed` + 具体错误（一次都不重试）；
/// - 连试 3 次仍失败 → `crashed` + 「已达自动重启上限」。
///
/// 账本写失败只记日志：插件确实没了这个事实比账本状态更重要，不能因为写库失败
/// 就把巡检卡住——否则每轮巡检都会重复报同一个崩溃。
pub async fn sweep_crashed(state: &AppState, store: &Store) {
    for plugin_id in state.plugins().crashed_ids() {
        recover_crashed(state, store, &plugin_id).await;
    }
}

/// 对单个已崩插件执行「下架 → 自动重启（≤3 次）→ 落账本」。
async fn recover_crashed(state: &AppState, store: &Store, plugin_id: &str) {
    match state.plugins().auto_restart(plugin_id).await {
        Restart::Recovered { attempts, .. } => {
            let restarts = match store.bump_plugin_restarts(plugin_id).await {
                Ok(restarts) => restarts,
                Err(err) => {
                    tracing::warn!(plugin_id = %plugin_id, %err, "重启计数写入账本失败");
                    -1
                }
            };
            if let Err(err) = store
                .set_plugin_status(plugin_id, plugin_status::READY, None)
                .await
            {
                tracing::warn!(plugin_id = %plugin_id, %err, "重启成功但状态写入失败");
            }
            tracing::info!(
                plugin_id = %plugin_id,
                attempt = attempts,
                restarts,
                "插件崩溃后自动重启成功，工具已重新上架"
            );
        }
        Restart::Deterministic { error, attempts } => {
            tracing::warn!(
                plugin_id = %plugin_id,
                attempt = attempts,
                %error,
                "插件重启撞上确定性失败，不再重试"
            );
            if let Err(err) = store
                .set_plugin_status(plugin_id, plugin_status::CRASHED, Some(&error.to_string()))
                .await
            {
                tracing::warn!(plugin_id = %plugin_id, %err, "崩溃状态写入账本失败");
            }
        }
        Restart::Exhausted {
            attempts,
            exit_code,
            message,
            ..
        } => {
            tracing::warn!(
                plugin_id = %plugin_id,
                attempts,
                exit_code = ?exit_code,
                "插件连续崩溃，已达自动重启上限，停在 crashed"
            );
            if let Err(err) = store
                .set_plugin_status(plugin_id, plugin_status::CRASHED, Some(&message))
                .await
            {
                tracing::warn!(plugin_id = %plugin_id, %err, "崩溃状态写入账本失败");
            }
        }
        // 装载表里已经没有它：说明这一轮巡检前被 unload / 新一轮 load 收走了，
        // 账本状态由那条路径负责，这里不要覆盖。
        Restart::NotLoaded => {}
    }
}

// ---------------------------------------------------------------------------
// 调用网关（P4 切片三）
// ---------------------------------------------------------------------------

/// `plugin.call` 正文上限：64 KB（docs/14 §6.3 第 6 步）。
pub const CALL_CONTENT_LIMIT: usize = 64 * 1024;

/// 网关判定用的运行时快照。
///
/// 把「账本里有它吗 / 现在什么状态 / 进程还活着吗 / 它声明了哪些方法」四件事抽成
/// 纯数据，六步定序就能脱离真实子进程验收——起插件要 node，单测不该被它拖住。
#[derive(Debug, Default, Clone)]
pub struct CallSnapshot {
    /// 账本里有这条记录。**装了就算存在**：与「在不在跑」是两件事，
    /// 混成一件事的话，「装了但没装载」会被报成 6101（插件不存在），
    /// 而它明明就在列表里，用户按提示重装一遍也不会好。
    pub installed: bool,
    /// 账本状态（`plugin_status::*`）。`installed` 为假时无意义。
    pub status: String,
    /// 进程是否活着（[`ai_ide_plugin::host::Host::is_running`]）。
    pub running: bool,
    /// 该插件声明过的工具名：内置名与 `plugin__<id>__<name>` 全名都收。
    pub declared: Vec<String>,
}

/// 第 ① 步：参数快照校验（`plugin_id` / `method` 必须是非空字符串）。
///
/// 单独拎出来是因为它必须**早于**碰库：空 `plugin_id` 拿去查账本，只会把
/// 「外壳塞进来的畸形请求」一路走到 6101，排查时被带偏成「插件没了」。
fn check_call_params(params: &PluginCallParams) -> Result<(&str, &str), ErrorObject> {
    let plugin_id = params.plugin_id.trim();
    if plugin_id.is_empty() {
        return Err(ErrorObject::invalid_params(
            "plugin_id 不能为空：plugin.call 必须指名插件",
        ));
    }
    let method = params.method.trim();
    if method.is_empty() {
        return Err(
            ErrorObject::invalid_params("method 不能为空：必须指名该插件声明的工具")
                .with_data(serde_json::json!({ "plugin_id": plugin_id })),
        );
    }
    Ok((plugin_id, method))
}

/// 前四步：参数快照校验 → 6101 → 6102 → 6106。
///
/// **顺序不可交换**，每一对相邻步骤都在防一种具体的误报：
///
/// - ①→②：畸形请求先拒，不能借 6101 回答「你参数写错了」；
/// - ②→③：没这个插件（6101）与有这个插件但跑不起来（6102）分开报，
///   前者让模型「别再编这个名字」，后者让用户「去面板看它为什么挂了」；
/// - ③→④：**6106 早于能力判定**。归属校验只看「这个插件自己声明的清单」，
///   不碰任何能力位；插件后续在 `host/*` 上撞到的 6103 必然发生在这之后。
///   反过来先判能力的话，一个拿别人 `plugin_id` 调别人的方法的请求会拿到
///   「你没授权」，把方向指到授权上，而真正的问题是越权。
pub fn check_call(
    params: &PluginCallParams,
    snapshot: &CallSnapshot,
) -> Result<(String, String), ErrorObject> {
    let (plugin_id, method) = check_call_params(params)?;

    if !snapshot.installed {
        return Err(ErrorObject::new(
            ErrorCode::PLUGIN_NOT_FOUND,
            format!("插件不存在：{plugin_id}"),
        )
        .with_data(serde_json::json!({ "plugin_id": plugin_id })));
    }

    if snapshot.status != plugin_status::READY || !snapshot.running {
        let status = if snapshot.status.is_empty() {
            "未装载"
        } else {
            snapshot.status.as_str()
        };
        return Err(ErrorObject::new(
            ErrorCode::PLUGIN_CRASHED,
            format!("插件当前不可调用（状态 {status}）：{plugin_id}"),
        )
        .with_data(serde_json::json!({
            "plugin_id": plugin_id,
            "status": snapshot.status,
            "running": snapshot.running,
        })));
    }

    if !snapshot.declared.iter().any(|name| name == method) {
        return Err(ErrorObject::new(
            ErrorCode::PLUGIN_PROTOCOL_VIOLATION,
            format!("插件 {plugin_id} 没有声明方法 {method}，不能跨插件调用"),
        )
        .with_data(serde_json::json!({
            "plugin_id": plugin_id,
            "method": method,
            "declared": snapshot.declared,
        })));
    }

    Ok((plugin_id.to_string(), method.to_string()))
}

/// 读一次网关判定所需的现状（账本 + 装载表）。
async fn call_snapshot(state: &AppState, plugin_id: &str) -> Result<CallSnapshot, ErrorObject> {
    let record = match state.store() {
        Some(store) => match store.get_plugin(plugin_id).await {
            Ok(record) => record,
            Err(err) => return Err(store_error(err)),
        },
        // 库没连上时只能靠装载表：进程在跑就是「存在且可用」，没跑就说没这个插件。
        None => None,
    };
    let running = state.plugins().is_running(plugin_id);
    let declared = state
        .plugins()
        .tool_catalog()
        .into_iter()
        .filter(|tool| tool.plugin_id == plugin_id)
        .flat_map(|tool| [tool.name, tool.namespaced_name])
        .collect();
    Ok(CallSnapshot {
        installed: record.is_some() || running,
        status: record.map(|record| record.status).unwrap_or_default(),
        running,
        declared,
    })
}

/// 第 ⑥ 步：正文超过 64 KB 就截断。
///
/// 按**字节**截（上限是内存与上下文预算，不是字数），但落点回退到最近的字符
/// 边界：切在 UTF-8 多字节序列中间会让 `String` 变成非法编码，回灌成 JSON 时
/// 直接炸。截断与否由 [`PluginCallResult::truncated`] 标记，正文里不加
/// 「已截断」之类后缀——模型会把后缀当成插件返回的数据复述出来。
fn truncate_content(text: String) -> (String, bool) {
    if text.len() <= CALL_CONTENT_LIMIT {
        return (text, false);
    }
    let mut end = CALL_CONTENT_LIMIT;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut content = text;
    content.truncate(end);
    (content, true)
}

/// 插件回包折成网关错误。
///
/// 口径：插件的**结构性**问题（越权方法、帧不合协议）走 61xx；插件自己回的
/// 业务错误原样带上它的 code（`data.plugin_code`），顶层用 `-32603`——那是
/// 「插件说它失败了」，不是「内核出故障了」，但外壳只需要一个转发分支。
fn call_error(plugin_id: &str, method: &str, err: CallError) -> ErrorObject {
    let data = serde_json::json!({ "plugin_id": plugin_id, "method": method });
    match err {
        // 转发前刚确认过在跑，跑到一半没了：按「炸了」报，让用户去面板看退出码。
        CallError::NotRunning(_) | CallError::Exited { .. } => ErrorObject::new(
            ErrorCode::PLUGIN_CRASHED,
            format!("插件已不可用：{plugin_id}"),
        )
        .with_data(data),
        CallError::UnknownTool(tool) => ErrorObject::new(
            ErrorCode::PLUGIN_PROTOCOL_VIOLATION,
            format!("插件 {plugin_id} 没有声明工具 {tool}"),
        )
        .with_data(data),
        CallError::Timeout { timeout, .. } => ErrorObject::new(
            ErrorCode::PLUGIN_TIMEOUT,
            format!("插件调用超时（{} ms）", timeout.as_millis()),
        )
        .with_data(data),
        CallError::Protocol { message } => ErrorObject::new(
            ErrorCode::PLUGIN_PROTOCOL_VIOLATION,
            format!("插件协议错误：{message}"),
        )
        .with_data(data),
        CallError::Plugin { code, message } => ErrorObject::new(
            ErrorCode::INTERNAL_ERROR,
            format!("插件返回错误：{message}"),
        )
        .with_data(serde_json::json!({
            "plugin_id": plugin_id,
            "method": method,
            "plugin_code": code,
        })),
    }
}

/// 给 `plugin.call` 落一行审计（kind = `plugin.call`，`action` = `call`）。
///
/// `target` 记**方法名**而不是插件 id：插件 id 已经在 `pluginId` 里，而
/// 「这个插件被要求干什么」才是排查时真正要看的（同一个插件十几个工具，
/// 拒绝率往往集中在其中一两个）。`callId` 用请求 id——外壳拿它跟自己的日志对号。
///
/// 库没连上就跳过：审计写不进时不能让「调用本来成功了」变成失败。
async fn audit_call(
    state: &AppState,
    call_id: &str,
    plugin_id: &str,
    method: &str,
    decision: &str,
    code: Option<i32>,
    truncated: Option<bool>,
) {
    let Some(store) = state.store() else {
        return;
    };
    let mut row = PluginAuditRow::new(audit_kind::PLUGIN_CALL, plugin_id, ACTION_CALL, decision)
        .target(if method.is_empty() {
            None
        } else {
            Some(method)
        })
        .call_id(Some(call_id));
    if let Some(code) = code {
        row = row.note("code", json!(code));
    }
    if let Some(truncated) = truncated {
        row = row.note("truncated", json!(truncated));
    }
    plugin_audit::record(store, row).await;
}

/// `plugin.call`：调用网关（六步定序见 [`check_call`]）。
///
/// 每一次请求都落一行审计，**包括被拒的**：网关的四种拒绝（畸形请求 / 6101 /
/// 6102 / 6106）在审计里各留一行 `deny`，否则「模型一直调不动某个插件」这件事
/// 只能从插件自己的日志里猜——而插件恰恰是不可信的那一侧。
pub async fn call(peer: &Peer, state: &AppState, req: &Request) {
    let Some(params) = require_params::<PluginCallParams>(peer, req).await else {
        return;
    };
    // ① 先校验、再碰库：畸形请求不该产生一次账本读。
    if let Err(err) = check_call_params(&params) {
        // 畸形请求没有可信的 plugin_id / method，就按原始值记：审计要能看出
        // 「有人发过一条指名不到插件的调用」，但不能替它编一个。
        audit_call(
            state,
            &req.id,
            params.plugin_id.trim(),
            params.method.trim(),
            DECISION_DENY,
            Some(err.code.as_i32()),
            None,
        )
        .await;
        return peer.respond_error(&req.id, &err).await;
    }

    let snapshot = match call_snapshot(state, params.plugin_id.trim()).await {
        Ok(snapshot) => snapshot,
        Err(err) => {
            audit_call(
                state,
                &req.id,
                params.plugin_id.trim(),
                params.method.trim(),
                DECISION_DENY,
                Some(err.code.as_i32()),
                None,
            )
            .await;
            return peer.respond_error(&req.id, &err).await;
        }
    };
    let (plugin_id, method) = match check_call(&params, &snapshot) {
        Ok(pair) => pair,
        Err(err) => {
            audit_call(
                state,
                &req.id,
                params.plugin_id.trim(),
                params.method.trim(),
                DECISION_DENY,
                Some(err.code.as_i32()),
                None,
            )
            .await;
            return peer.respond_error(&req.id, &err).await;
        }
    };

    // ⑤ 转发 `plugin/tool-invoke`：`params` 缺省按 `{}`，与插件侧的 schema 校验一致。
    let args = params
        .params
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));
    let value = match state.plugins().call_tool(&plugin_id, &method, args).await {
        Ok(value) => value,
        Err(err) => {
            let error = call_error(&plugin_id, &method, err);
            // 插件自己失败（超时 / 协议 / 回包错误）记 `error` 而不是 `deny`：
            // `deny` 是「内核不让」，`error` 是「内核放行了但插件没干成」，两者
            // 的处理动作完全不同（前者改授权，后者修插件）。
            audit_call(
                state,
                &req.id,
                &plugin_id,
                &method,
                DECISION_ERROR,
                Some(error.code.as_i32()),
                None,
            )
            .await;
            return peer.respond_error(&req.id, &error).await;
        }
    };

    // ⑥ 截断后再回灌。
    let (content, truncated) = truncate_content(crate::plugin_tools::result_text(&value));
    if truncated {
        tracing::warn!(
            plugin_id = %plugin_id,
            method = %method,
            limit = CALL_CONTENT_LIMIT,
            "plugin.call 结果超限，已截断"
        );
    }
    audit_call(
        state,
        &req.id,
        &plugin_id,
        &method,
        DECISION_ALLOW,
        None,
        Some(truncated),
    )
    .await;
    peer.respond(
        &req.id,
        &PluginCallResult {
            plugin_id,
            method,
            content,
            truncated,
        },
    )
    .await;
}

// ---------------------------------------------------------------------------
// 授权面（P4 切片一：grant / revoke）
// ---------------------------------------------------------------------------

/// 关键能力：撤销后插件已经无法履行清单承诺，必须连带卸载（docs/14 §4.2 硬规则 3）。
///
/// 判据是「撤掉之后工具还能不能干活」，不是「名字看起来像不像写操作」：
///
/// - 读类（`fs.read` / `search` / `workspace.info`）撤销只是**收窄可读面**：插件照常
///   跑，撞上闸门的那一次 `host/*` 当场拿 6103，模型读得懂「这条路被封了」；
/// - 写 / 网络 / 外部模型能力是**工具承诺的前提**（清单里 `risk=write` 的工具没有
///   `fs.write` 必然失败）。继续把它挂在工具表里，只会让模型陷进「调用→6103」的
///   重试循环，所以整个插件下架。
const KEY_CAPABILITIES: &[&str] = &["fs.write", "net.fetch", "llm.chat"];

fn is_key_capability(capability: &str) -> bool {
    KEY_CAPABILITIES.contains(&capability)
}

/// 把请求里的能力名折成「这次真正要动的清单项」。
///
/// 三条规则，都在防「静默成功」：
///
/// 1. 空串不接受：`[""]` 看着像「一项没指定」，写进 SQL 却是一行都改不到，
///    回头报成功就成了撒谎；
/// 2. 清单没申请过的能力直接拒（`-32602` + `capability` 字段）：`set_plugin_capability`
///    的 `Ok(None)` 就是这个意思，不能让拼错的名字被当成「撤掉了」；
/// 3. 去重但保序：一次请求里重复写同一能力不该产生两行 UPDATE。
fn resolve_capabilities(
    plugin_id: &str,
    wanted: &[String],
    requested: &[String],
    empty_means_all: bool,
) -> Result<Vec<String>, ErrorObject> {
    if wanted.iter().any(|capability| capability.trim().is_empty()) {
        return Err(ErrorObject::invalid_params(
            "capabilities 里不能有空字符串；留空数组才表示「全部」",
        )
        .with_data(serde_json::json!({ "plugin_id": plugin_id })));
    }

    let source: Vec<String> = if wanted.is_empty() {
        if empty_means_all {
            requested.to_vec()
        } else {
            return Err(ErrorObject::invalid_params(
                "capabilities 不能为空：撤销至少要指名一项能力",
            )
            .with_data(serde_json::json!({ "plugin_id": plugin_id })));
        }
    } else {
        wanted
            .iter()
            .map(|capability| capability.trim().to_string())
            .collect()
    };

    let mut targets: Vec<String> = Vec::new();
    for capability in source {
        if !requested.iter().any(|applied| applied == &capability) {
            return Err(ErrorObject::invalid_params(format!(
                "插件 {plugin_id} 的清单里没有申请能力 {capability}"
            ))
            .with_data(serde_json::json!({
                "plugin_id": plugin_id,
                "capability": capability,
                "requested": requested,
            })));
        }
        if !targets.contains(&capability) {
            targets.push(capability);
        }
    }
    Ok(targets)
}

/// 读账本里的插件记录，顺带把「不存在」折成 6100。
async fn plugin_record(store: &Store, plugin_id: &str) -> Result<PluginRecord, ErrorObject> {
    match store.get_plugin(plugin_id).await {
        Ok(Some(record)) => Ok(record),
        Ok(None) => Err(not_found(plugin_id)),
        Err(err) => Err(store_error(err)),
    }
}

/// `plugin.grant`：授予能力。
///
/// 两步都要做，缺一步就是「账本说授权了、插件却还用不了」：
/// 先把授权位写进 `plugin_capability`（持久化，重启后仍在），再把当前授权集
/// `set_granted_for` 灌进 `FsHostApi`（运行态，下一次 `host/*` 当场生效）。
pub async fn grant(peer: &Peer, state: &AppState, req: &Request) {
    let Some(params) = require_params::<PluginGrantParams>(peer, req).await else {
        return;
    };
    let Some(plugin_id) = require_plugin_id(peer, req, &params.plugin_id).await else {
        return;
    };
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    match grant_capabilities(state, store, &plugin_id, &params.capabilities).await {
        Ok(plugin) => {
            let granted = plugin.granted_capabilities.join(",");
            plugin_audit::record_user(
                store,
                PluginAuditRow::new(
                    audit_kind::PLUGIN_CAPABILITY_CHANGED,
                    &plugin_id,
                    ACTION_GRANT,
                    DECISION_ALLOW,
                )
                .target(Some(&granted))
                .note("capabilities", json!(plugin.granted_capabilities)),
            )
            .await;
            peer.respond(&req.id, &PluginGrantResult { plugin }).await
        }
        Err(err) => {
            // 失败的授权同样要留痕：审计要能回答「有人试图把这项能力开给这个插件，
            // 但被拒了」。`target` 记用户想动的那几项，`capabilities` 留原样。
            let wanted = params.capabilities.join(",");
            plugin_audit::record_user(
                store,
                PluginAuditRow::new(
                    audit_kind::PLUGIN_CAPABILITY_CHANGED,
                    &plugin_id,
                    ACTION_GRANT,
                    DECISION_DENY,
                )
                .target(if wanted.is_empty() {
                    None
                } else {
                    Some(&wanted)
                })
                .note("capabilities", json!(params.capabilities))
                .note("code", json!(err.code.as_i32())),
            )
            .await;
            peer.respond_error(&req.id, &err).await
        }
    }
}

/// `grant` 的业务内核（不碰 `Peer`，便于单测直接驱动）。
pub async fn grant_capabilities(
    state: &AppState,
    store: &Store,
    plugin_id: &str,
    wanted: &[String],
) -> Result<PluginSummary, ErrorObject> {
    let record = plugin_record(store, plugin_id).await?;
    let requested = capabilities_of(&record);
    // 空数组 = 授予清单里申请的全部能力（面板「一键同意」走的这条路）。
    let targets = resolve_capabilities(plugin_id, wanted, &requested, true)?;

    for capability in &targets {
        match store
            .set_plugin_capability(plugin_id, capability, true)
            .await
        {
            Ok(Some(_)) => {}
            // 上面已按清单一对一校验过，走到这里说明记录在两步之间被删了。
            Ok(None) => return Err(not_found(plugin_id)),
            Err(err) => return Err(store_error(err)),
        }
    }

    // 授权只增不减，直接按账本重算：不差分比差分安全（漏算一项就白授权了）。
    let granted = store
        .granted_capabilities(plugin_id)
        .await
        .map_err(store_error)?;
    // 落账本只是「记下来」，运行态要单独灌：`FsHostApi` 的授权集是内存快照，
    // 不回写的话新授权的插件得等到下次 `reload` 才拿得到能力。
    state
        .plugin_host()
        .set_granted_for(plugin_id, granted.clone());

    let record = plugin_record(store, plugin_id).await?;
    Ok(summary_from_record(&record, &requested, &granted))
}

/// `plugin.revoke`：撤销能力。
///
/// 与 `grant` 对称，但多了两件撤销特有的事：
///
/// 1. **关键能力连带卸载**：撤掉 `fs.write` 之类的前提能力后，插件已无法履行清单
///    承诺，规则是立刻 `unload`（不重启），工具下架、状态退回 `needs_grant`；
/// 2. **运行态授权集当场回写**：非关键能力撤销时进程还在跑，如果不回写，
///    插件能一直用到下次 `reload` 才被拦——那等于撤销按钮是假的。
pub async fn revoke(peer: &Peer, state: &AppState, req: &Request) {
    let Some(params) = require_params::<PluginRevokeParams>(peer, req).await else {
        return;
    };
    let Some(plugin_id) = require_plugin_id(peer, req, &params.plugin_id).await else {
        return;
    };
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    match revoke_capabilities(state, store, &plugin_id, &params.capabilities).await {
        Ok((plugin, unloaded)) => {
            let revoked = params.capabilities.join(",");
            plugin_audit::record_user(
                store,
                PluginAuditRow::new(
                    audit_kind::PLUGIN_CAPABILITY_CHANGED,
                    &plugin_id,
                    ACTION_REVOKE,
                    DECISION_ALLOW,
                )
                .target(Some(&revoked))
                .note("capabilities", json!(params.capabilities))
                // 撤前提能力会连带卸载，这件事必须在同一行里看得见——否则审计上
                // 只剩「撤销了 fs.write」，解释不了插件进程为什么跟着没了。
                .note("unloaded", json!(unloaded))
                .note("granted", json!(plugin.granted_capabilities)),
            )
            .await;
            peer.respond(&req.id, &PluginRevokeResult { plugin, unloaded })
                .await
        }
        Err(err) => {
            let wanted = params.capabilities.join(",");
            plugin_audit::record_user(
                store,
                PluginAuditRow::new(
                    audit_kind::PLUGIN_CAPABILITY_CHANGED,
                    &plugin_id,
                    ACTION_REVOKE,
                    DECISION_DENY,
                )
                .target(if wanted.is_empty() {
                    None
                } else {
                    Some(&wanted)
                })
                .note("capabilities", json!(params.capabilities))
                .note("code", json!(err.code.as_i32())),
            )
            .await;
            peer.respond_error(&req.id, &err).await
        }
    }
}

/// `revoke` 的业务内核，返回（撤销后的摘要，是否连带卸载）。
pub async fn revoke_capabilities(
    state: &AppState,
    store: &Store,
    plugin_id: &str,
    wanted: &[String],
) -> Result<(PluginSummary, bool), ErrorObject> {
    let record = plugin_record(store, plugin_id).await?;
    let requested = capabilities_of(&record);
    let targets = resolve_capabilities(plugin_id, wanted, &requested, false)?;

    for capability in &targets {
        match store
            .set_plugin_capability(plugin_id, capability, false)
            .await
        {
            Ok(Some(_)) => {}
            Ok(None) => return Err(not_found(plugin_id)),
            Err(err) => return Err(store_error(err)),
        }
    }

    let granted = store
        .granted_capabilities(plugin_id)
        .await
        .map_err(store_error)?;

    let critical = targets
        .iter()
        .any(|capability| is_key_capability(capability));
    let was_running = state.plugins().is_running(plugin_id);
    let unloaded = critical && was_running;
    if unloaded {
        // 不重启：撤掉前提能力的插件起来也只会一路 6103，重试没有意义。
        state.plugins().unload(plugin_id).await;
        tracing::info!(plugin_id = %plugin_id, capabilities = ?targets, "撤销关键能力，连带卸载插件");
    }

    if critical {
        // 撤销前它是 `ready`（刚被停掉），就不能再对外报「就绪」：清单缺了一项，
        // 只能退回 `needs_grant` 重新征询。`installed` / `needs_grant` 已由仓储层的
        // `refresh_grant_status` 处理；`crashed` 保留原样——崩溃原因比这次撤销更
        // 值得留在面板上。
        let current = plugin_record(store, plugin_id).await?;
        if current.status == plugin_status::READY {
            store
                .set_plugin_status(plugin_id, plugin_status::NEEDS_GRANT, None)
                .await
                .map_err(store_error)?;
        }
    }

    // 连带卸载时清空（与 `unload` 同一口径），否则把剩余授权集灌回去 —— 下一次
    // `host/*` 就地撞 6103，不必等 reload。
    state.plugin_host().set_granted_for(
        plugin_id,
        if unloaded {
            Vec::new()
        } else {
            granted.clone()
        },
    );

    let record = plugin_record(store, plugin_id).await?;
    Ok((summary_from_record(&record, &requested, &granted), unloaded))
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
        // 计数只在「崩溃后重新装载成功」时 +1（见 `load_into`），面板据此分辨
        // 「刚装好」与「崩过 N 次、可能还要崩」。
        restarts: record.restarts.max(0) as u32,
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

#[cfg(test)]
mod tests {
    use super::*;
    use ai_ide_plugin::{HostApi, HostApiError};
    use ai_ide_store::NewPlugin;

    const PLUGIN_ID: &str = "demo-plugin";

    /// 造一条「已安装、能力全未授权」的账本记录，并把它挂进 `AppState`。
    ///
    /// 用真库（内存 SQLite）而不是桩：授权位写入与 `refresh_grant_status` 的状态
    /// 迁移本身就是这次要验的行为，桩掉就只剩「函数被调用过」这种什么也证明不了的断言。
    async fn seeded(capabilities: &[&str]) -> (AppState, Store) {
        let store = Store::open_in_memory().await.expect("内存库应能打开");
        let capabilities_json = serde_json::to_string(capabilities).unwrap();
        let flags: Vec<(&str, bool)> = capabilities
            .iter()
            .map(|capability| (*capability, false))
            .collect();
        store
            .save_plugin(
                NewPlugin {
                    id: PLUGIN_ID,
                    name: "演示插件",
                    version: "1.0.0",
                    description: None,
                    plugin_api: "1.0",
                    main: "index.mjs",
                    install_dir: "C:/tmp/demo-plugin",
                    manifest_json: "{}",
                    capabilities_json: &capabilities_json,
                    commands_json: "[]",
                    subscriptions_json: "[]",
                    tool_count: 1,
                    status: plugin_status::NEEDS_GRANT,
                    installed_at: None,
                },
                &flags,
            )
            .await
            .expect("账本写入应成功");

        let state = AppState::new();
        state.install_store(store.clone());
        (state, store)
    }

    /// 直接撞宿主能力闸门，只关心「放行 / 6103」。
    fn gate(
        state: &AppState,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, HostApiError> {
        state.plugin_host().call(PLUGIN_ID, method, &params)
    }

    /// 授权一次之后，运行态授权集立刻生效：不必 reload，`host/fs.read` 不再撞 6103。
    #[tokio::test]
    async fn grant_writes_ledger_and_runtime_grants_without_reload() {
        let (state, store) = seeded(&["fs.read", "search"]).await;

        // 授权前：运行态还没有这个插件 → 6103。
        let err = gate(
            &state,
            "host/fs.read",
            serde_json::json!({ "path": "a.txt" }),
        )
        .expect_err("未授权的插件不该读到任何东西");
        assert_eq!(err.code, HostApiError::UNAUTHORIZED);

        // 空数组 = 授予清单里的全部能力。
        let summary = grant_capabilities(&state, &store, PLUGIN_ID, &[])
            .await
            .expect("一键授权应成功");
        assert_eq!(
            summary.granted_capabilities,
            vec!["fs.read".to_string(), "search".to_string()]
        );
        // 状态从 `needs_grant` 走回 `installed`（协议侧叫 `Disabled`：装好了但没加载）。
        assert_eq!(summary.state, PluginState::Disabled);

        // 账本落盘：换个读法再确认一次，防「返回值对、库没写」。
        let persisted = store.granted_capabilities(PLUGIN_ID).await.unwrap();
        assert_eq!(persisted, vec!["fs.read".to_string(), "search".to_string()]);

        // 运行态：能力闸门已放行，接下来的失败只可能是别的原因（没开工作区 / 文件不存在），
        // 绝不能还是 6103。
        let err = gate(
            &state,
            "host/fs.read",
            serde_json::json!({ "path": "a.txt" }),
        )
        .expect_err("没开工作区，读不到文件");
        assert_ne!(err.code, HostApiError::UNAUTHORIZED);
    }

    /// 撤销读类能力：插件还在跑（不卸载），但下一次 `host/*` 当场 6103。
    #[tokio::test]
    async fn revoke_takes_effect_on_next_host_call_without_reload() {
        let (state, store) = seeded(&["fs.read", "search"]).await;
        grant_capabilities(&state, &store, PLUGIN_ID, &[])
            .await
            .expect("一键授权应成功");
        // 授权后闸门放行：没开工作区，撞的是 1001 而不是能力 6103。
        let err = gate(
            &state,
            "host/fs.read",
            serde_json::json!({ "path": "a.txt" }),
        )
        .expect_err("没开工作区，读不到文件");
        assert_ne!(err.code, HostApiError::UNAUTHORIZED);

        let (summary, unloaded) =
            revoke_capabilities(&state, &store, PLUGIN_ID, &["fs.read".to_string()])
                .await
                .expect("撤销应成功");

        assert!(!unloaded, "读类能力不是关键能力，不该连带卸载");
        assert_eq!(summary.granted_capabilities, vec!["search".to_string()]);
        assert_eq!(summary.state, PluginState::NeedsGrant);

        // 撤销即时生效：没有 reload、没有 unload，下一次调用就已经被拦。
        let err = gate(
            &state,
            "host/fs.read",
            serde_json::json!({ "path": "a.txt" }),
        )
        .expect_err("撤销后必须拒绝");
        assert_eq!(err.code, HostApiError::UNAUTHORIZED);
    }

    /// 撤销关键能力：即使插件没在跑，也要退回 `needs_grant`（工具不该再被路由）。
    #[tokio::test]
    async fn revoke_key_capability_flips_ready_plugin_back_to_needs_grant() {
        let (state, store) = seeded(&["fs.read", "fs.write"]).await;
        grant_capabilities(&state, &store, PLUGIN_ID, &[])
            .await
            .expect("一键授权应成功");

        // 伪造「已加载」：真起进程要看 node，本机不一定有；这里验的是「撤销关键能力后
        // 账本与打授权集的走向」，真实装载由 plugin_flow 集成测试覆盖。
        store
            .set_plugin_status(PLUGIN_ID, plugin_status::READY, None)
            .await
            .expect("状态写入应成功");

        let (summary, unloaded) =
            revoke_capabilities(&state, &store, PLUGIN_ID, &["fs.write".to_string()])
                .await
                .expect("撤销应成功");

        // 进程本来就不在，谈不上卸载；但状态必须退回 `needs_grant`。
        assert!(!unloaded);
        assert_eq!(summary.state, PluginState::NeedsGrant);
        assert_eq!(summary.granted_capabilities, vec!["fs.read".to_string()]);
        // 写侧能力已从账本撤下（`host/fs.write` 属于 P4 写侧切片，本切片尚未挂载，
        // 所以这里只验账本，运行态回写由上面读侧的用例覆盖）。
    }

    /// 空 `capabilities` 对 `grant` 是「全部」，对 `revoke` 是「没说清楚」。
    #[tokio::test]
    async fn empty_capabilities_mean_all_for_grant_but_are_rejected_for_revoke() {
        let (state, store) = seeded(&["fs.read"]).await;

        let err = revoke_capabilities(&state, &store, PLUGIN_ID, &[])
            .await
            .expect_err("撤销不能空手来");
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);

        let summary = grant_capabilities(&state, &store, PLUGIN_ID, &[])
            .await
            .expect("空数组应理解为全部授权");
        assert_eq!(summary.granted_capabilities, vec!["fs.read".to_string()]);
    }

    /// 清单没申请过的能力、空字符串都要拒，不能静默成功。
    #[tokio::test]
    async fn unknown_and_blank_capabilities_are_rejected() {
        let (state, store) = seeded(&["fs.read"]).await;

        let err = grant_capabilities(&state, &store, PLUGIN_ID, &["shell.exec".to_string()])
            .await
            .expect_err("清单外的能力必须拒");
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        // 拒了就不能留下半张授权：库里的授权位还是全 0。
        assert!(store
            .granted_capabilities(PLUGIN_ID)
            .await
            .unwrap()
            .is_empty());

        let err = grant_capabilities(&state, &store, PLUGIN_ID, &["  ".to_string()])
            .await
            .expect_err("空字符串必须拒");
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    }

    /// 插件不存在时是 6100，不是「授权成功」也不是 5003。
    #[tokio::test]
    async fn missing_plugin_reports_not_found() {
        let (state, store) = seeded(&["fs.read"]).await;
        let err = grant_capabilities(&state, &store, "nope", &[])
            .await
            .expect_err("不存在的插件不能授权");
        assert_eq!(err.code, ErrorCode::PLUGIN_NOT_FOUND);
    }

    /// 关键能力清单就是「撤了工具干不了活」的那三个。
    #[test]
    fn key_capabilities_cover_write_network_and_llm_only() {
        for capability in ["fs.write", "net.fetch", "llm.chat"] {
            assert!(is_key_capability(capability), "{capability} 应算关键能力");
        }
        for capability in ["fs.read", "search", "workspace.info"] {
            assert!(!is_key_capability(capability), "{capability} 不该连带卸载");
        }
    }

    // -------------------------------------------------------------- 调用网关

    fn call_params(plugin_id: &str, method: &str) -> PluginCallParams {
        PluginCallParams {
            plugin_id: plugin_id.to_string(),
            method: method.to_string(),
            params: None,
        }
    }

    /// 一条「一切就绪」的快照：装了、`ready`、在跑、声明了 `echo`。
    fn ready_snapshot() -> CallSnapshot {
        CallSnapshot {
            installed: true,
            status: plugin_status::READY.to_string(),
            running: true,
            declared: vec!["echo".to_string(), "plugin__demo-plugin__echo".to_string()],
        }
    }

    /// 六步里的前四步必须按序报错：同一次「全错」的请求，错误码只能是最前面那一关的。
    #[test]
    fn gate_reports_the_first_failing_step_only() {
        let broken = CallSnapshot::default();

        // ① 参数快照校验先于一切：空 method 配「插件不存在」的账本，报 -32602 而不是 6101。
        let err = check_call(&call_params("demo-plugin", "  "), &broken).expect_err("空方法必须拒");
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        // 空 plugin_id 同理：不能一路走到 6101 把畸形请求说成「插件没了」。
        let err = check_call(&call_params(" ", "echo"), &broken).expect_err("空 id 必须拒");
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);

        // ② 插件不存在 → 6101：即便状态、装载、方法全都对不上，也只能报这一条。
        let err =
            check_call(&call_params("demo-plugin", "echo"), &broken).expect_err("没装就没得调");
        assert_eq!(err.code, ErrorCode::PLUGIN_NOT_FOUND);

        // ③ 装了但没装载 / 状态不是 ready → 6102，而不是 6106（方法归属还没轮到判）。
        let installed_only = CallSnapshot {
            installed: true,
            status: plugin_status::NEEDS_GRANT.to_string(),
            running: false,
            declared: Vec::new(),
        };
        let err = check_call(&call_params("demo-plugin", "echo"), &installed_only)
            .expect_err("needs_grant 不可调用");
        assert_eq!(err.code, ErrorCode::PLUGIN_CRASHED);
        // 账本说 ready 但进程不在，同样是 6102。
        let dead = CallSnapshot {
            installed: true,
            status: plugin_status::READY.to_string(),
            running: false,
            declared: vec!["echo".to_string()],
        };
        let err =
            check_call(&call_params("demo-plugin", "echo"), &dead).expect_err("进程不在不可调");
        assert_eq!(err.code, ErrorCode::PLUGIN_CRASHED);

        // ④ 状态齐了才判归属：没声明的方�� → 6106。
        let err = check_call(&call_params("demo-plugin", "nope"), &ready_snapshot())
            .expect_err("越权方法必须拒");
        assert_eq!(err.code, ErrorCode::PLUGIN_PROTOCOL_VIOLATION);

        // 五关全过：返回的是归一化后的 id / method（trim 过的，与后续转发口径一致）。
        let (plugin_id, method) =
            check_call(&call_params(" demo-plugin ", "echo"), &ready_snapshot())
                .expect("一切就绪应放行");
        assert_eq!(plugin_id, "demo-plugin");
        assert_eq!(method, "echo");
    }

    /// **6106 早于能力判定**：这里的插件一个能力都没授权，越权方法仍然必须是 6106，
    /// 绝不能落到「能力不足」的 6103 上——那会把「改了别人的方法名」误导成「去授权」。
    #[test]
    fn attribution_error_precedes_any_capability_verdict() {
        let no_capability = ready_snapshot();
        let err = check_call(
            &call_params("demo-plugin", "other-plugin__secret"),
            &no_capability,
        )
        .expect_err("别人的方法不能借道");
        assert_eq!(err.code, ErrorCode::PLUGIN_PROTOCOL_VIOLATION);
        assert_ne!(
            err.code,
            ErrorCode::PLUGIN_CAPABILITY_DENIED,
            "归属校验不能退化成能力判定"
        );
        // `declared` 一并回给外壳：面板能直接说出「它到底声明了什么」。
        let declared = err
            .data
            .as_ref()
            .and_then(|data| data.get("declared"))
            .and_then(|value| value.as_array())
            .expect("错误里要带声明清单");
        assert_eq!(declared.len(), 2);
    }

    /// 第 ⑥ 步：64 KB 是硬上限，且截断点必须落在字符边界上。
    #[test]
    fn content_over_limit_is_truncated_on_char_boundary() {
        // 正好等于上限：不截。
        let exact = "a".repeat(CALL_CONTENT_LIMIT);
        let (content, truncated) = truncate_content(exact);
        assert_eq!(content.len(), CALL_CONTENT_LIMIT);
        assert!(!truncated, "等于上限不算超限");

        // 多一个字节：截到上限。
        let over = "a".repeat(CALL_CONTENT_LIMIT + 1);
        let (content, truncated) = truncate_content(over);
        assert_eq!(content.len(), CALL_CONTENT_LIMIT);
        assert!(truncated);

        // 上限处正好切在 3 字节汉字中间：回退到字符边界，绝不产出非法 UTF-8。
        let mut mixed = "a".repeat(CALL_CONTENT_LIMIT - 1);
        mixed.push('汉');
        assert!(mixed.len() > CALL_CONTENT_LIMIT);
        let (content, truncated) = truncate_content(mixed);
        assert!(truncated);
        assert_eq!(content.len(), CALL_CONTENT_LIMIT - 1);
        assert!(content.is_char_boundary(content.len()));
        assert_eq!(content.chars().last(), Some('a'));
    }

    /// 转发失败按类折码：超时 6105、越权工具 6106、插件自报错误原样带上它的 code。
    #[test]
    fn relay_failures_map_to_their_own_codes() {
        let timeout = call_error(
            "demo-plugin",
            "echo",
            CallError::Timeout {
                tool: "echo".to_string(),
                timeout: std::time::Duration::from_millis(5000),
            },
        );
        assert_eq!(timeout.code, ErrorCode::PLUGIN_TIMEOUT);

        let unknown = call_error(
            "demo-plugin",
            "nope",
            CallError::UnknownTool("nope".to_string()),
        );
        assert_eq!(unknown.code, ErrorCode::PLUGIN_PROTOCOL_VIOLATION);

        let exited = call_error(
            "demo-plugin",
            "echo",
            CallError::Exited {
                code: Some(1),
                tail: Vec::new(),
            },
        );
        assert_eq!(exited.code, ErrorCode::PLUGIN_CRASHED);

        let plugin = call_error(
            "demo-plugin",
            "echo",
            CallError::Plugin {
                code: -32000,
                message: "业务失败".to_string(),
            },
        );
        assert_eq!(plugin.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(
            plugin
                .data
                .as_ref()
                .and_then(|data| data.get("plugin_code"))
                .and_then(|value| value.as_i64()),
            Some(-32000),
            "插件自己的错误码不能丢"
        );
    }

    /// 快照读的是真账本：装了但没装载 → `installed` 为真、`running` 为假，
    /// 于是第二步放行、第三步 6102；账本里没有的 id 才落 6101。
    #[tokio::test]
    async fn snapshot_distinguishes_installed_from_running() {
        let (state, _store) = seeded(&["fs.read"]).await;

        let snapshot = call_snapshot(&state, PLUGIN_ID)
            .await
            .expect("读账本应成功");
        assert!(snapshot.installed, "账本里有它就算存在");
        assert!(!snapshot.running, "没 load 过就不该有进程");
        assert!(snapshot.declared.is_empty(), "没装载就没有可路由的方法");
        let err = check_call(&call_params(PLUGIN_ID, "echo"), &snapshot).expect_err("未装载不可调");
        assert_eq!(err.code, ErrorCode::PLUGIN_CRASHED);

        let snapshot = call_snapshot(&state, "nope").await.expect("读账本应成功");
        assert!(!snapshot.installed);
        assert!(!snapshot.running);
        let err = check_call(&call_params("nope", "echo"), &snapshot).expect_err("不存在应 6101");
        assert_eq!(err.code, ErrorCode::PLUGIN_NOT_FOUND);
    }

    // ---------------------------------------------------------------- 审计闭环
    //
    // 这一节验的是**处理器**而不是业务内核：审计行由 `grant` / `revoke` / `call` /
    // `load` / `unload` 这些 `Peer` 入口落库，只测 `*_capabilities` 看不到任何一行。

    use ai_ide_store::{audit_actor, AuditQuery};

    fn request(id: &str, method: &str, params: serde_json::Value) -> Request {
        Request::new(id, method, Some(params))
    }

    /// 取某个 kind 下的全部审计行。`audit.list` 的实现就是这条查询
    /// （`AuditQuery::kind` + `list_audit`），所以这里查得到就等于协议侧查得到。
    async fn rows_of_kind(store: &Store, kind: &str) -> Vec<ai_ide_store::AuditEvent> {
        store
            .list_audit(AuditQuery::new().kind(kind).with_limit(100))
            .await
            .expect("审计查询应成功")
    }

    /// 授权成功与被拒各留一行，五个基础字段齐全。
    #[tokio::test]
    async fn grant_records_allow_and_deny_rows() {
        let (state, store) = seeded(&["fs.read", "search"]).await;
        let peer = Peer::new();

        let ok = request(
            "req-grant-ok",
            ai_ide_protocol::methods::plugin::GRANT,
            json!({ "plugin_id": PLUGIN_ID, "capabilities": ["fs.read"] }),
        );
        grant(&peer, &state, &ok).await;

        // 清单外的能力：业务侧拒了，但审计必须留下「有人试图开 shell.exec」。
        let deny = request(
            "req-grant-deny",
            ai_ide_protocol::methods::plugin::GRANT,
            json!({ "plugin_id": PLUGIN_ID, "capabilities": ["shell.exec"] }),
        );
        grant(&peer, &state, &deny).await;

        let rows = rows_of_kind(&store, audit_kind::PLUGIN_CAPABILITY_CHANGED).await;
        assert_eq!(
            rows.len(),
            2,
            "成功与失败各一行，一次请求不能两行也不能零行"
        );

        let allow = rows
            .iter()
            .find(|row| row.detail["decision"] == DECISION_ALLOW)
            .expect("授权成功必须留行");
        assert_eq!(
            allow.actor,
            audit_actor::USER,
            "是用户点的，不是插件自己开的"
        );
        assert_eq!(allow.detail["pluginId"], PLUGIN_ID);
        assert_eq!(allow.detail["action"], ACTION_GRANT);
        assert_eq!(allow.detail["target"], "fs.read");
        assert_eq!(allow.detail["capabilities"], json!(["fs.read"]));
        assert!(
            allow.detail["callId"].is_null(),
            "能力变更不是调用，callId 要留空而不是编一个"
        );

        let rejected = rows
            .iter()
            .find(|row| row.detail["decision"] == DECISION_DENY)
            .expect("被拒的授权也要留行");
        assert_eq!(rejected.detail["action"], ACTION_GRANT);
        assert_eq!(rejected.detail["target"], "shell.exec");
        assert_eq!(
            rejected.detail["code"],
            json!(ErrorCode::INVALID_PARAMS.as_i32())
        );
    }

    /// 撤销同样一行：`target` 是撤掉的那项能力，`unloaded` 说清有没有连带下架。
    #[tokio::test]
    async fn revoke_records_capability_row_with_unloaded_flag() {
        let (state, store) = seeded(&["fs.read", "fs.write"]).await;
        let peer = Peer::new();

        grant_capabilities(&state, &store, PLUGIN_ID, &[])
            .await
            .expect("先全部授权");

        let revoke_req = request(
            "req-revoke",
            ai_ide_protocol::methods::plugin::REVOKE,
            json!({ "plugin_id": PLUGIN_ID, "capabilities": ["fs.write"] }),
        );
        revoke(&peer, &state, &revoke_req).await;

        let rows = rows_of_kind(&store, audit_kind::PLUGIN_CAPABILITY_CHANGED).await;
        assert_eq!(
            rows.len(),
            1,
            "只该有撤销这一行（授权走的是业务内核，没经过处理器）"
        );
        let row = &rows[0];
        assert_eq!(row.detail["action"], ACTION_REVOKE);
        assert_eq!(row.detail["decision"], DECISION_ALLOW);
        assert_eq!(row.detail["target"], "fs.write");
        assert_eq!(
            row.detail["unloaded"],
            json!(false),
            "插件本来就没在跑，不能谎报卸载过"
        );
        assert_eq!(row.detail["granted"], json!(["fs.read"]));
    }

    /// 网关的三种拒绝各留一行，`callId` 用请求 id，能从审计对回外壳日志。
    #[tokio::test]
    async fn call_gateway_records_deny_rows_with_call_id() {
        let (state, store) = seeded(&["fs.read"]).await;
        let peer = Peer::new();

        let not_found = request(
            "req-call-404",
            ai_ide_protocol::methods::plugin::CALL,
            json!({ "plugin_id": "ghost", "method": "echo" }),
        );
        call(&peer, &state, &not_found).await;

        // 装了但没装载：6102，同样要留痕。
        let crashed = request(
            "req-call-6102",
            ai_ide_protocol::methods::plugin::CALL,
            json!({ "plugin_id": PLUGIN_ID, "method": "echo" }),
        );
        call(&peer, &state, &crashed).await;

        // 畸形请求：method 是空的，仍要能看出「发生过一次指名不到工具的调用」。
        let malformed = request(
            "req-call-bad",
            ai_ide_protocol::methods::plugin::CALL,
            json!({ "plugin_id": PLUGIN_ID, "method": "  " }),
        );
        call(&peer, &state, &malformed).await;

        let rows = rows_of_kind(&store, audit_kind::PLUGIN_CALL).await;
        assert_eq!(rows.len(), 3, "被拒的调用同样是一次尝试");

        for row in &rows {
            assert_eq!(row.detail["action"], ACTION_CALL);
            assert_eq!(row.detail["decision"], DECISION_DENY);
            assert_eq!(row.actor, audit_actor::PLUGIN);
        }

        let row = rows
            .iter()
            .find(|row| row.detail["callId"] == "req-call-404")
            .expect("callId 必须与请求 id 一致");
        assert_eq!(row.detail["pluginId"], "ghost");
        assert_eq!(row.detail["target"], "echo");
        assert_eq!(
            row.detail["code"],
            json!(ErrorCode::PLUGIN_NOT_FOUND.as_i32())
        );

        let row = rows
            .iter()
            .find(|row| row.detail["callId"] == "req-call-6102")
            .expect("6102 那次也要留行");
        assert_eq!(row.detail["pluginId"], PLUGIN_ID);
        assert_eq!(
            row.detail["code"],
            json!(ErrorCode::PLUGIN_CRASHED.as_i32())
        );

        let row = rows
            .iter()
            .find(|row| row.detail["callId"] == "req-call-bad")
            .expect("畸形请求也要留行");
        assert!(row.detail["target"].is_null(), "空方法不能编出一个 target");
    }

    /// 生命周期：卸载成功与被拒（插件不存在）都落 `plugin.lifecycle`。
    #[tokio::test]
    async fn unload_records_lifecycle_rows() {
        let (state, store) = seeded(&["fs.read"]).await;
        let peer = Peer::new();

        let ok = request(
            "req-unload",
            ai_ide_protocol::methods::plugin::UNLOAD,
            json!({ "plugin_id": PLUGIN_ID }),
        );
        unload(&peer, &state, &ok).await;

        let deny = request(
            "req-unload-404",
            ai_ide_protocol::methods::plugin::UNLOAD,
            json!({ "plugin_id": "ghost" }),
        );
        unload(&peer, &state, &deny).await;

        let rows = rows_of_kind(&store, audit_kind::PLUGIN_LIFECYCLE).await;
        assert_eq!(rows.len(), 2);

        let allowed = rows
            .iter()
            .find(|row| row.detail["decision"] == DECISION_ALLOW)
            .expect("卸载成功要留行");
        assert_eq!(allowed.detail["action"], ACTION_UNLOAD);
        assert_eq!(allowed.detail["target"], PLUGIN_ID);
        assert_eq!(allowed.detail["removedTools"], json!(0));
        assert_eq!(allowed.actor, audit_actor::USER);

        let rejected = rows
            .iter()
            .find(|row| row.detail["decision"] == DECISION_DENY)
            .expect("卸载一个不存在的插件也要留行");
        assert_eq!(rejected.detail["pluginId"], "ghost");
        assert_eq!(
            rejected.detail["code"],
            json!(ErrorCode::PLUGIN_NOT_FOUND.as_i32())
        );
    }

    /// 审计对插件是只读且不可达的：`HostApi` 里没有任何 `audit.*` 方法，
    /// 仓储层也没有删改接口——插件既查不到别人干了什么，也擦不掉自己那一行。
    #[tokio::test]
    async fn plugins_cannot_reach_or_rewrite_audit() {
        let (state, store) = seeded(&["fs.read"]).await;
        let peer = Peer::new();

        let grant_req = request(
            "req-audit-proof",
            ai_ide_protocol::methods::plugin::GRANT,
            json!({ "plugin_id": PLUGIN_ID, "capabilities": [] }),
        );
        grant(&peer, &state, &grant_req).await;

        // `audit.list` 的查询条件：kind + 分页，能查到刚才那一行。
        let query = AuditQuery::new().kind(audit_kind::PLUGIN_CAPABILITY_CHANGED);
        assert_eq!(
            store.count_audit(&query.filter).await.expect("计数应成功"),
            1
        );

        // 插件侧：连方法都没有，谈不上删改。
        for method in ["host/audit.list", "host/audit.clear", "host/audit.prune"] {
            let err = gate(&state, method, json!({})).expect_err("插件不该有审计入口");
            assert_eq!(
                err.code,
                HostApiError::METHOD_NOT_FOUND,
                "{method} 不该存在于宿主能力表"
            );
        }

        // 行数只增：再发生一次事件只会多一行，没有任何「覆盖」的路径。
        let revoke_req = request(
            "req-audit-proof-2",
            ai_ide_protocol::methods::plugin::REVOKE,
            json!({ "plugin_id": PLUGIN_ID, "capabilities": ["fs.read"] }),
        );
        revoke(&peer, &state, &revoke_req).await;
        let rows = rows_of_kind(&store, audit_kind::PLUGIN_CAPABILITY_CHANGED).await;
        assert_eq!(rows.len(), 2, "旧行不能被新事件顶掉");
    }
}
