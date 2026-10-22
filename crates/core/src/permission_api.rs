//! 权限免问规则的协议处理器（M5 切片六）。
//!
//! 三个入口，作用各不相同：
//!
//! - [`list`] / [`revoke`] 处理 `permission.list` / `permission.revoke`，给 UI 一个
//!   「我到底默许过什么、怎么收回」的出口；
//! - [`recorder`] / [`seed_gate`] 是给 Agent 运行时用的**接线**：前者把
//!   `allow_always` 的命中记进库，后者在开跑前把历史免问范围灌回闸门。
//!
//! ## 为什么没有 `permission.allow`
//!
//! 免问权限只能由用户当场点出来（`agent.decide` 的 `allow_always`）。协议面不开放
//! 「直接写一条规则」——能凭空授予权限的接口等于一把万能钥匙，任何拿到协议通道的
//! 进程都能自我提权。撤销则是安全方向的，开放出去没有风险。
//!
//! ## 落盘是旁路
//!
//! [`recorder`] 的 `remember` 在闸门的热路径上同步调用，所以它只把记录丢给后台任务，
//! 写库失败只告警：记不住最多是「下次再问一次」，不该让用户当下这次调用失败。

use ai_ide_agent::permission::AlwaysRecord;
use ai_ide_agent::PermissionGate;
use ai_ide_protocol::dto::{
    PermissionListParams, PermissionListResult, PermissionRevokeParams, PermissionRevokeResult,
    PermissionRuleView,
};
use ai_ide_protocol::{ErrorCode, ErrorObject, Request};
use ai_ide_store::{NewPermissionRule, PermissionRule, Store};

use crate::dispatch::parse_params;
use crate::peer::Peer;
use crate::session_api::store_of;
use crate::state::AppState;

/// `permission.list` 的默认返回条数，与 store 侧上限保持一致。
const PERMISSION_DEFAULT_LIMIT: u32 = ai_ide_store::MAX_PERMISSION_RULES as u32;

/// 规则的作用域标记：跨工作区。
const GLOBAL_SCOPE: &str = "global";

// ---------------------------------------------------------------------------
// permission.list
// ---------------------------------------------------------------------------

pub async fn list(peer: &Peer, state: &AppState, req: &Request) {
    let params: PermissionListParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let scope = match scope_of(state, params.workspace_id.as_deref()) {
        Ok(scope) => scope,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let limit = params
        .limit
        .unwrap_or(PERMISSION_DEFAULT_LIMIT)
        .clamp(1, PERMISSION_DEFAULT_LIMIT);

    // 总数用同一份过滤条件单独数：规则条数很少，但「有没有更多」得让 UI 说得准。
    let all = match store
        .list_permissions(scope.as_deref(), i64::from(limit))
        .await
    {
        Ok(rows) => rows,
        Err(err) => return peer.respond_error(&req.id, &store_error(err)).await,
    };
    let total = match store.count_permissions(scope.as_deref()).await {
        Ok(total) => total,
        Err(err) => return peer.respond_error(&req.id, &store_error(err)).await,
    };

    let result = PermissionListResult {
        rules: all.iter().map(to_view).collect(),
        total,
    };
    peer.respond(&req.id, &result).await;
}

// ---------------------------------------------------------------------------
// permission.revoke
// ---------------------------------------------------------------------------

pub async fn revoke(peer: &Peer, state: &AppState, req: &Request) {
    let params: PermissionRevokeParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };
    let id = params
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty());
    let tool = params
        .tool
        .as_deref()
        .map(str::trim)
        .filter(|tool| !tool.is_empty());
    // 两个都不给 = 不知道撤哪条，直接退回去而不是「静默撤全部」。
    if id.is_none() && tool.is_none() {
        let err = ErrorObject::invalid_params("撤销权限需要 `id` 或 `tool` 之一")
            .with_data(serde_json::json!({ "hint": "整批清空不开放给协议调用" }));
        return peer.respond_error(&req.id, &err).await;
    }

    let store = match store_of(state) {
        Ok(store) => store,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let removed = match (id, tool) {
        // `id` 更精确，两者同时给时以它为准。
        (Some(id), _) => match store.revoke_permission(id).await {
            Ok(removed) => u64::from(removed),
            Err(err) => return peer.respond_error(&req.id, &store_error(err)).await,
        },
        (None, Some(tool)) => match store.revoke_permissions_of_tool(tool).await {
            Ok(removed) => removed,
            Err(err) => return peer.respond_error(&req.id, &store_error(err)).await,
        },
        (None, None) => unreachable!("上面已拦掉两者皆空"),
    };

    // 剩下的条数顺手回给 UI：撤销后不用再发一次 list 才知道结果。
    let remaining = match store.count_permissions(None).await {
        Ok(remaining) => remaining,
        Err(err) => return peer.respond_error(&req.id, &store_error(err)).await,
    };

    let result = PermissionRevokeResult { removed, remaining };
    peer.respond(&req.id, &result).await;
}

// ---------------------------------------------------------------------------
// Agent 运行时接线
// ---------------------------------------------------------------------------

/// 给闸门用的落盘器：把 `allow_always` 命中转成一条后台写库任务。
///
/// 用 `tokio::spawn` 而不是 `await`：`AlwaysRecorder::remember` 是同步签名，且跑在
/// 判定热路径上——在这里阻塞等一次磁盘写，等于让每次「总是允许」都卡一下 IO。
#[derive(Clone)]
pub struct StoreRecorder {
    store: Store,
}

impl StoreRecorder {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

impl ai_ide_agent::permission::AlwaysRecorder for StoreRecorder {
    fn remember(&self, record: AlwaysRecord) {
        let store = self.store.clone();
        tokio::spawn(async move {
            let rule = NewPermissionRule::allow(&record.key, &record.tool, &record.scope);
            if let Err(err) = store.remember_permission(rule).await {
                // 只是「下次还得问」——不升级成用户可见的错误。
                tracing::warn!(
                    key = %record.key,
                    tool = %record.tool,
                    error = %err,
                    "免问规则落盘失败（下次仍会询问）"
                );
            }
        });
    }
}

/// 把库里已有的免问范围灌回闸门，返回灌入条数。
///
/// 只读**键**：落盘存的就是哪个键，重启后还原成哪个键，中间不做任何推导，
/// 粒度不会因为「过了一次库」而变粗。
pub async fn seed_gate(gate: &PermissionGate, store: &Store, scope: Option<&str>) -> usize {
    match store.permission_ids(scope).await {
        Ok(keys) => gate.seed_always(keys),
        Err(err) => {
            tracing::warn!(error = %err, "读取免问规则失败（按空集合起跑）");
            0
        }
    }
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

/// 取规则作用域：优先按 `workspace_id` 折成绝对路径，缺省取活动工作区。
///
/// 工作区没开时返回 `None` —— 此时仍应能看到 `global` 规则，所以这不算错误。
fn scope_of(state: &AppState, workspace_id: Option<&str>) -> Result<Option<String>, ErrorObject> {
    match state.workspace(workspace_id) {
        Ok(workspace) => Ok(Some(normalize(workspace.root.to_string_lossy().as_ref()))),
        Err(err) if workspace_id.is_none() => {
            tracing::debug!(error = %err, "无活动工作区，只列全局免问规则");
            Ok(None)
        }
        Err(err) => Err(ErrorObject::new(
            ErrorCode::WORKSPACE_NOT_OPENED,
            format!("权限规则需要已打开的工作区：{err}"),
        )),
    }
}

/// 工作区根路径折成规则的 `scope`。
///
/// 与 `session_api` 落盘工作区路径的口径必须一致：否则同一次「总是允许」在库里
/// 记成反斜杠版本，重启后按正斜杠去查就查不到，等于规则丢了。
pub(crate) fn scope_for_root(root: &str) -> String {
    normalize(root)
}

/// 路径归一：统一正斜杠，与 `session_api` 落盘工作区路径的口径一致。
fn normalize(path: &str) -> String {
    path.replace('\\', "/")
}

fn store_error(err: ai_ide_store::StoreError) -> ErrorObject {
    ErrorObject::new(ErrorCode::STORE_ERROR, format!("会话库读写失败: {err}"))
}

fn to_view(rule: &PermissionRule) -> PermissionRuleView {
    PermissionRuleView {
        id: rule.id.clone(),
        tool_pattern: rule.tool_pattern.clone(),
        scope: rule.scope.clone(),
        policy: rule.policy.clone(),
        created_at_ms: rule.created_at,
    }
}

/// 供 `agent_api` 判定「这条规则是不是全局的」（目前仅用于日志与展示）。
pub fn is_global_scope(scope: &str) -> bool {
    scope == GLOBAL_SCOPE
}
