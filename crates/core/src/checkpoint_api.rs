//! `checkpoint.*` 的协议处理器（M5 切片五）。
//!
//! 与 `session_api` 同一分工：这里只做「解析参数 → 调 [`crate::checkpoint`] →
//! 翻错误码 → 记审计 → 回包」。快照怎么落盘、回滚怎么校验，全在 checkpoint 模块里。
//!
//! 文件 IO 走 `spawn_blocking`：一个大工作区快照几百毫秒，不能把 tokio worker 占死。

use std::path::PathBuf;

use ai_ide_protocol::dto::{
    CheckpointCreateParams, CheckpointCreateResult, CheckpointDeleteParams, CheckpointDeleteResult,
    CheckpointListParams, CheckpointListResult, CheckpointOrigin, CheckpointRestoreParams,
    CheckpointRestoreResult,
};
use ai_ide_protocol::{ErrorCode, ErrorObject, Request};
use ai_ide_store::{audit_kind, NewAuditEvent};
use serde_json::json;

use crate::audit_api;
use crate::checkpoint::{CheckpointError, CheckpointStore, SnapshotRequest};
use crate::dispatch::parse_params;
use crate::peer::Peer;
use crate::state::AppState;

/// `checkpoint.list` 的默认 / 最大返回条数。
const LIST_DEFAULT_LIMIT: u32 = 50;
const LIST_MAX_LIMIT: u32 = 200;

// ---------------------------------------------------------------------------
// checkpoint.create
// ---------------------------------------------------------------------------

pub async fn create(peer: &Peer, state: &AppState, req: &Request) {
    let params: CheckpointCreateParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let root = match workspace_root(state, params.workspace_id.as_deref()) {
        Ok(root) => root,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let store = CheckpointStore::from_env();
    let label = params.label.clone();
    let paths = params.paths.clone();
    let session_id = params.session_id.clone();
    let created = tokio::task::spawn_blocking(move || {
        store.create(&SnapshotRequest {
            root: &root,
            label: label.as_deref(),
            paths: paths.as_deref(),
            origin: CheckpointOrigin::Manual,
            session_id: session_id.as_deref(),
        })
    })
    .await;

    match created {
        Ok(Ok(snapshot)) => {
            if let Some(store) = state.store() {
                audit_api::record(
                    store,
                    NewAuditEvent::core(
                        audit_kind::CHECKPOINT_CREATED,
                        format!("建立检查点「{}」", snapshot.label),
                    )
                    .with_session(snapshot.session_id.as_deref().unwrap_or_default())
                    .with_detail(json!({
                        "checkpoint_id": snapshot.checkpoint_id,
                        "workspace_root": snapshot.workspace_root,
                        "files": snapshot.files.len() as u64,
                        "total_bytes": snapshot.total_bytes,
                        "skipped": snapshot.skipped,
                    })),
                )
                .await;
            }
            peer.respond(
                &req.id,
                &CheckpointCreateResult {
                    checkpoint: snapshot,
                },
            )
            .await;
        }
        Ok(Err(err)) => peer.respond_error(&req.id, &map_error(err)).await,
        Err(err) => {
            let err = ErrorObject::internal(format!("检查点任务未能完成：{err}"));
            peer.respond_error(&req.id, &err).await;
        }
    }
}

// ---------------------------------------------------------------------------
// checkpoint.list
// ---------------------------------------------------------------------------

pub async fn list(peer: &Peer, state: &AppState, req: &Request) {
    let params: CheckpointListParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let root = match workspace_root(state, params.workspace_id.as_deref()) {
        Ok(root) => root,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let limit = params
        .limit
        .unwrap_or(LIST_DEFAULT_LIMIT)
        .clamp(1, LIST_MAX_LIMIT);
    let offset = params.offset.unwrap_or(0);
    let store = CheckpointStore::from_env();
    let outcome = tokio::task::spawn_blocking(move || store.list(&root, limit, offset)).await;

    match outcome {
        Ok(Ok((checkpoints, total))) => {
            let result = CheckpointListResult { checkpoints, total };
            peer.respond(&req.id, &result).await;
        }
        Ok(Err(err)) => peer.respond_error(&req.id, &map_error(err)).await,
        Err(err) => {
            let err = ErrorObject::internal(format!("检查点任务未能完成：{err}"));
            peer.respond_error(&req.id, &err).await;
        }
    }
}

// ---------------------------------------------------------------------------
// checkpoint.restore
// ---------------------------------------------------------------------------

pub async fn restore(peer: &Peer, state: &AppState, req: &Request) {
    let params: CheckpointRestoreParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let store = CheckpointStore::from_env();
    let checkpoint_id = params.checkpoint_id.clone();
    let outcome = tokio::task::spawn_blocking(move || store.restore(&checkpoint_id)).await;

    match outcome {
        Ok(Ok(outcome)) => {
            if let Some(store) = state.store() {
                audit_api::record(
                    store,
                    NewAuditEvent::core(
                        audit_kind::CHECKPOINT_RESTORED,
                        format!(
                            "回滚到检查点「{}」（{}）",
                            outcome.checkpoint.label,
                            if outcome.verified {
                                "校验通过"
                            } else {
                                "校验未通过"
                            }
                        ),
                    )
                    .with_session(outcome.checkpoint.session_id.as_deref().unwrap_or_default())
                    .with_detail(json!({
                        "checkpoint_id": outcome.checkpoint.checkpoint_id,
                        "workspace_root": outcome.checkpoint.workspace_root,
                        "verified": outcome.verified,
                        "files": outcome.files.len() as u64,
                        "failed": outcome
                            .files
                            .iter()
                            .filter(|file| file.detail.is_some())
                            .count() as u64,
                    })),
                )
                .await;
            }
            let result = CheckpointRestoreResult {
                checkpoint_id: outcome.checkpoint.checkpoint_id,
                verified: outcome.verified,
                files: outcome.files,
            };
            peer.respond(&req.id, &result).await;
        }
        Ok(Err(err)) => peer.respond_error(&req.id, &map_error(err)).await,
        Err(err) => {
            let err = ErrorObject::internal(format!("检查点任务未能完成：{err}"));
            peer.respond_error(&req.id, &err).await;
        }
    }
}

// ---------------------------------------------------------------------------
// checkpoint.delete
// ---------------------------------------------------------------------------

pub async fn delete(peer: &Peer, state: &AppState, req: &Request) {
    let params: CheckpointDeleteParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let store = CheckpointStore::from_env();
    let target = params.checkpoint_id.clone();
    let outcome = tokio::task::spawn_blocking(move || store.delete(&target)).await;

    match outcome {
        Ok(Ok((removed, freed_bytes))) => {
            if removed {
                if let Some(store) = state.store() {
                    audit_api::record(
                        store,
                        NewAuditEvent::core(
                            audit_kind::CHECKPOINT_DELETED,
                            format!("删除检查点 {}", params.checkpoint_id),
                        )
                        .with_detail(json!({
                            "checkpoint_id": params.checkpoint_id,
                            "freed_bytes": freed_bytes,
                        })),
                    )
                    .await;
                }
            }
            let result = CheckpointDeleteResult {
                checkpoint_id: params.checkpoint_id,
                removed,
                freed_bytes,
            };
            peer.respond(&req.id, &result).await;
        }
        Ok(Err(err)) => peer.respond_error(&req.id, &map_error(err)).await,
        Err(err) => {
            let err = ErrorObject::internal(format!("检查点任务未能完成：{err}"));
            peer.respond_error(&req.id, &err).await;
        }
    }
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

/// 定位工作区根目录：给了 id 按 id 取，没给退回活动工作区。
fn workspace_root(state: &AppState, workspace_id: Option<&str>) -> Result<PathBuf, ErrorObject> {
    match state.workspace(workspace_id) {
        Ok(workspace) => Ok(workspace.root.clone()),
        Err(err) => Err(ErrorObject::new(
            ErrorCode::WORKSPACE_NOT_OPENED,
            format!("检查点需要已打开的工作区：{err}"),
        )
        .with_data(json!({ "workspace_id": workspace_id }))),
    }
}

fn map_error(err: CheckpointError) -> ErrorObject {
    match &err {
        CheckpointError::NotFound(id) => ErrorObject::new(
            ErrorCode::CHECKPOINT_NOT_FOUND,
            format!("检查点不存在: {id}"),
        )
        .with_data(json!({ "checkpoint_id": id })),
        CheckpointError::Empty(message) => ErrorObject::invalid_params(message.clone()),
        _ => ErrorObject::new(err.code(), err.to_string()),
    }
}
