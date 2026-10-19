//! Provider / 模型目录与连通性测试的处理器（M2）。
//!
//! 这一层只做四件事：解析参数、读写落盘目录、让注册表跟上变更、回响应/事件。
//! 具体协议的适配器（`openai` 等）不在这里——`build_provider` 按协议挑实现，
//! 本模块拿到的是一个 `Arc<dyn Provider>`。
//!
//! **密钥边界**：`provider.upsert` 收到的明文密钥只进入 [`LlmRuntime`] 的内存密钥表，
//! 既不进落盘文件（[`ProviderRecord`] 结构上就没有密钥字段），也不进任何响应或事件
//! （`provider.list` 只回 `has_key` 与脱敏指纹）。
//!
//! 一条容易踩的语义：`timeout_ms` 用 `Option` 归一（`null` 即「用默认 60s」），
//! 而 `enabled` 在 DTO 层带 `serde(default = true)`——**省略 `enabled` 等于置为 `true`**，
//! 不想改启用状态的调用方必须显式回传当前值。

use std::collections::BTreeMap;
use std::sync::Arc;

use ai_ide_llm::{
    build_provider, model_pk, LlmError, ModelRecord, Provider, ProviderRecord, SecretString,
    StoreSnapshot,
};
use ai_ide_protocol::dto::{
    ModelDeleteParams, ModelListParams, ModelListResult, ModelUpsertParams, ModelUpsertResult,
    ModelView, OkResult, ProviderChangedEvent, ProviderDeleteParams, ProviderListParams,
    ProviderListResult, ProviderTestParams, ProviderTestResult, ProviderUpsertParams,
    ProviderUpsertResult, ProviderView,
};
use ai_ide_protocol::{methods, ErrorCode, ErrorObject, Request};
use tracing::{info, warn};

use crate::dispatch::parse_params;
use crate::llm_runtime::LlmRuntime;
use crate::peer::Peer;
use crate::state::{now_ms, AppState};

/// 内联探测（无 `provider_id`）时给临时实例用的占位 id，只出现在日志与错误文案里。
const INLINE_PROBE_ID: &str = "(inline)";

// ---------------------------------------------------------------------------
// provider.upsert
// ---------------------------------------------------------------------------

pub async fn provider_upsert(peer: &Peer, state: &AppState, req: &Request) {
    let params: ProviderUpsertParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let runtime = state.llm();
    let mut snapshot = match load(runtime) {
        Ok(snapshot) => snapshot,
        Err(err) => return respond_llm_error(peer, req, &err).await,
    };

    // 带 id 即原地更新，但记录必须存在——否则一次手滑的 id 就会静默新增一条脏记录。
    let existing = match params.id.as_deref() {
        Some(id) => match snapshot.provider(id).cloned() {
            Some(record) => Some(record),
            None => {
                return peer
                    .respond_error(
                        &req.id,
                        &ErrorObject::new(
                            ErrorCode::INVALID_PARAMS,
                            format!("Provider 不存在：{id}"),
                        ),
                    )
                    .await;
            }
        },
        None => None,
    };

    let now = now_ms();
    let record = ProviderRecord {
        id: params.id.unwrap_or_else(new_provider_id),
        name: params.name,
        protocol: params.protocol,
        base_url: params.base_url,
        headers: params.headers,
        enabled: params.enabled,
        timeout_ms: params.timeout_ms,
        // 测试结论是「历史上测得的结果」，入参里没有，只有 provider.test 能改。
        last_test_ok: existing.as_ref().and_then(|record| record.last_test_ok),
        created_at_ms: existing.as_ref().map_or(now, |record| record.created_at_ms),
        updated_at_ms: now,
    };
    let provider_id = record.id.clone();

    snapshot.upsert_provider(record.clone());
    if let Err(err) = runtime.store().save(&snapshot) {
        return respond_llm_error(peer, req, &err).await;
    }

    match params.api_key {
        // 非空密钥：更新内存密钥表。
        Some(api_key) if !api_key.is_empty() => {
            runtime.put_key(&provider_id, SecretString::new(api_key));
        }
        // 缺省或空串表示「沿用已存密钥」；新建时则明确清掉，避免留下不可能的残留。
        _ => {
            if existing.is_none() {
                runtime.drop_key(&provider_id);
            }
        }
    }

    if let Err(err) = runtime.sync_provider(&record) {
        // 记录已落盘，但实例构建不出来（例如协议尚未接线）：如实报错，
        // 让设置界面能显示「已保存、当前不可用」，而不是假装成功。
        warn!(provider_id = %provider_id, error = %err, "Provider 已保存但无法启用");
        return respond_llm_error(peer, req, &err).await;
    }

    info!(provider_id = %provider_id, protocol = ?record.protocol, "Provider 已保存");
    notify_changed(state, "upserted", &provider_id).await;
    peer.respond(&req.id, &ProviderUpsertResult { provider_id })
        .await;
}

// ---------------------------------------------------------------------------
// provider.list
// ---------------------------------------------------------------------------

pub async fn provider_list(peer: &Peer, state: &AppState, req: &Request) {
    if let Err(err) = parse_params::<ProviderListParams>(req.params.as_ref()) {
        return peer.respond_error(&req.id, &err).await;
    }

    let runtime = state.llm();
    let snapshot = match load(runtime) {
        Ok(snapshot) => snapshot,
        Err(err) => return respond_llm_error(peer, req, &err).await,
    };

    let providers = snapshot
        .providers
        .iter()
        .map(|record| view_of(record, runtime))
        .collect();
    peer.respond(&req.id, &ProviderListResult { providers })
        .await;
}

// ---------------------------------------------------------------------------
// provider.delete
// ---------------------------------------------------------------------------

pub async fn provider_delete(peer: &Peer, state: &AppState, req: &Request) {
    let params: ProviderDeleteParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let runtime = state.llm();
    let mut snapshot = match load(runtime) {
        Ok(snapshot) => snapshot,
        Err(err) => return respond_llm_error(peer, req, &err).await,
    };

    // 记录与其下模型一起删（`remove_provider` 内级联），避免留下悬空模型条目。
    if !snapshot.remove_provider(&params.provider_id) {
        return peer
            .respond_error(
                &req.id,
                &ErrorObject::new(
                    ErrorCode::INVALID_PARAMS,
                    format!("Provider 不存在：{}", params.provider_id),
                ),
            )
            .await;
    }
    if let Err(err) = runtime.store().save(&snapshot) {
        return respond_llm_error(peer, req, &err).await;
    }

    runtime.remove_provider(&params.provider_id);
    info!(provider_id = %params.provider_id, "Provider 已删除");
    notify_changed(state, "deleted", &params.provider_id).await;
    peer.respond(&req.id, &OkResult { ok: true }).await;
}

// ---------------------------------------------------------------------------
// provider.test
// ---------------------------------------------------------------------------

pub async fn provider_test(peer: &Peer, state: &AppState, req: &Request) {
    let params: ProviderTestParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let runtime = state.llm();
    // 先挡参数不完整的请求：这是调用方写错了，不该落到「内部错误」那句糊话上。
    if params.provider_id.is_none() && params.override_spec.is_none() {
        return peer
            .respond_error(
                &req.id,
                &ErrorObject::invalid_params("provider.test 需要 provider_id 或 override 之一"),
            )
            .await;
    }
    let probe = match resolve_probe(&params, runtime) {
        Ok(probe) => probe,
        Err(err) => return respond_llm_error(peer, req, &err).await,
    };

    // 判定只看「能否拿到模型列表」：那比一次 ping 式的健康检查更能说明密钥与地址都对。
    let (ok, latency_ms, models_found, message) = match probe.health().await {
        Ok(report) => (
            report.ok,
            report.latency_ms,
            report.models_found,
            report.message,
        ),
        Err(err) => (false, 0, 0, Some(err.to_string())),
    };

    // 把结论写回记录（仅当这次测的是目录里的 Provider）。写盘失败不影响本次结论，
    // 但要让用户知道「结果没记下来」——否则下次打开设置会看到过期的 last_test_ok。
    if let Some(provider_id) = params.provider_id.as_deref() {
        match load(runtime) {
            Ok(mut snapshot) => match snapshot.provider(provider_id).cloned() {
                Some(mut record) => {
                    record.last_test_ok = Some(ok);
                    record.updated_at_ms = now_ms();
                    snapshot.upsert_provider(record);
                    if let Err(err) = runtime.store().save(&snapshot) {
                        warn!(provider_id = %provider_id, error = %err, "测试结论写盘失败");
                    }
                }
                None => warn!(provider_id = %provider_id, "测试结论无处可记：Provider 已不存在"),
            },
            Err(err) => warn!(provider_id = %provider_id, error = %err, "测试结论写盘失败"),
        }
    }

    info!(ok, latency_ms, models_found, "Provider 连通性测试完成");
    peer.respond(
        &req.id,
        &ProviderTestResult {
            ok,
            latency_ms,
            models_found: models_found as u64,
            message,
        },
    )
    .await;
}

// ---------------------------------------------------------------------------
// model.upsert / model.list / model.delete
// ---------------------------------------------------------------------------

pub async fn model_upsert(peer: &Peer, state: &AppState, req: &Request) {
    let params: ModelUpsertParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let runtime = state.llm();
    let mut snapshot = match load(runtime) {
        Ok(snapshot) => snapshot,
        Err(err) => return respond_llm_error(peer, req, &err).await,
    };

    // 模型挂在 Provider 下：没有宿主就必须拒绝，否则列表里会出现点不开的孤儿模型。
    if snapshot.provider(&params.provider_id).is_none() {
        return peer
            .respond_error(
                &req.id,
                &ErrorObject::new(
                    ErrorCode::INVALID_PARAMS,
                    format!("Provider 不存在：{}", params.provider_id),
                ),
            )
            .await;
    }

    let pk = model_pk(&params.provider_id, &params.model_id);
    let created_at_ms = snapshot
        .models
        .iter()
        .find(|record| record.model_pk == pk)
        .map_or_else(now_ms, |record| record.created_at_ms);

    snapshot.upsert_model(ModelRecord {
        model_pk: pk.clone(),
        provider_id: params.provider_id,
        model_id: params.model_id,
        display_name: params.display_name,
        caps: params.caps.unwrap_or_default(),
        params: params.params.unwrap_or_default(),
        created_at_ms,
    });
    if let Err(err) = runtime.store().save(&snapshot) {
        return respond_llm_error(peer, req, &err).await;
    }

    info!(model_pk = %pk, "模型条目已保存");
    peer.respond(&req.id, &ModelUpsertResult { model_pk: pk })
        .await;
}

pub async fn model_list(peer: &Peer, state: &AppState, req: &Request) {
    let params: ModelListParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let runtime = state.llm();
    let snapshot = match load(runtime) {
        Ok(snapshot) => snapshot,
        Err(err) => return respond_llm_error(peer, req, &err).await,
    };

    let models = snapshot
        .models
        .iter()
        .filter(|record| match params.provider_id.as_deref() {
            Some(provider_id) => provider_id == record.provider_id,
            None => true,
        })
        .map(|record| ModelView {
            model_pk: record.model_pk.clone(),
            provider_id: record.provider_id.clone(),
            model_id: record.model_id.clone(),
            display_name: record.display_name.clone(),
            caps: record.caps.clone(),
            params: record.params.clone(),
            created_at_ms: record.created_at_ms,
        })
        .collect();
    peer.respond(&req.id, &ModelListResult { models }).await;
}

pub async fn model_delete(peer: &Peer, state: &AppState, req: &Request) {
    let params: ModelDeleteParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let runtime = state.llm();
    let mut snapshot = match load(runtime) {
        Ok(snapshot) => snapshot,
        Err(err) => return respond_llm_error(peer, req, &err).await,
    };

    if !snapshot.remove_model(&params.model_pk) {
        return peer
            .respond_error(
                &req.id,
                &ErrorObject::new(
                    ErrorCode::INVALID_PARAMS,
                    format!("模型不存在：{}", params.model_pk),
                ),
            )
            .await;
    }
    if let Err(err) = runtime.store().save(&snapshot) {
        return respond_llm_error(peer, req, &err).await;
    }

    info!(model_pk = %params.model_pk, "模型条目已删除");
    peer.respond(&req.id, &OkResult { ok: true }).await;
}

// ---------------------------------------------------------------------------
// 内部工具
// ---------------------------------------------------------------------------

/// 读一次落盘目录。
fn load(runtime: &LlmRuntime) -> Result<StoreSnapshot, LlmError> {
    runtime.store().load()
}

/// 脱敏视图：任何字段都不含明文密钥。
fn view_of(record: &ProviderRecord, runtime: &LlmRuntime) -> ProviderView {
    let key_hint = runtime.key_hint(&record.id);
    ProviderView {
        id: record.id.clone(),
        name: record.name.clone(),
        protocol: record.protocol,
        base_url: record.base_url.clone(),
        headers: record.headers.clone(),
        enabled: record.enabled,
        has_key: key_hint.is_some(),
        key_hint,
        timeout_ms: record.timeout_ms,
        last_test_ok: record.last_test_ok,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

/// 挑出这次要探测的 Provider 实例。
///
/// 两种来源：注册表里的现成实例（`provider_id`），或由内联覆盖临时构造的实例
/// （允许「先试连通、再决定是否保存」）。后者**不进注册表也不落盘**。
fn resolve_probe(
    params: &ProviderTestParams,
    runtime: &LlmRuntime,
) -> Result<Arc<dyn Provider>, LlmError> {
    if let Some(over) = params.override_spec.as_ref() {
        let id = params
            .provider_id
            .clone()
            .unwrap_or_else(|| INLINE_PROBE_ID.to_string());
        // 内联覆盖没带密钥时，回落到该 Provider 已存的密钥，否则测的就不是同一套凭据了。
        let api_key = over
            .api_key
            .as_ref()
            .filter(|key| !key.is_empty())
            .map(SecretString::new)
            .or_else(|| runtime.key(&id));
        let record = ProviderRecord {
            id,
            name: INLINE_PROBE_ID.to_string(),
            protocol: over.protocol,
            base_url: over.base_url.clone(),
            headers: BTreeMap::new(),
            enabled: true,
            timeout_ms: None,
            last_test_ok: None,
            created_at_ms: 0,
            updated_at_ms: 0,
        };
        return build_provider(&record, api_key, runtime.transport());
    }

    // 两个来源都没有：`provider_test` 已经提前挡掉了，这里是防御性兜底。
    let provider_id = params.provider_id.as_deref().ok_or_else(|| {
        LlmError::Store("provider.test 需要 provider_id 或 override 之一".to_string())
    })?;
    runtime
        .registry()
        .get(provider_id)
        .ok_or_else(|| LlmError::ProviderNotConfigured {
            provider_id: provider_id.to_string(),
        })
}

/// 新增 Provider 的 id：`p-` + 12 位十六进制。
///
/// 不用自增序号：目录文件可能被用户手工编辑或跨机器同步，序号会撞。
fn new_provider_id() -> String {
    let raw = uuid::Uuid::new_v4().simple().to_string();
    format!("p-{}", &raw[..12])
}

/// 推一条 `provider.changed`；单测里没注入对端，事件静默丢弃。
async fn notify_changed(state: &AppState, kind: &str, provider_id: &str) {
    let Some(peer) = state.peer() else {
        return;
    };
    peer.notify(
        methods::event::PROVIDER_CHANGED,
        &ProviderChangedEvent {
            kind: kind.to_string(),
            provider_id: provider_id.to_string(),
        },
    )
    .await;
}

/// 把模型接入层错误翻成协议错误对象再回。
///
/// 错误码（`2xxx` 区）决定 UI 是「换密钥」「稍后重试」还是「查网络」，
/// 所以这里必须走 [`LlmError::to_error_object`] 而不是笼统的 `INTERNAL_ERROR`。
async fn respond_llm_error(peer: &Peer, req: &Request, err: &LlmError) {
    let error = err.to_error_object();
    warn!(
        method = %req.method,
        code = error.code.as_i32(),
        message = %error.message,
        "模型接入层报错"
    );
    peer.respond_error(&req.id, &error).await;
}
