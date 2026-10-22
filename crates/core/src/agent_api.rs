//! `agent.*` 三个方法的处理器（M4 第四切片）。
//!
//! 这一层是「协议面」与「闭环内核」之间的接线板：把 [`ai_ide_agent`] 的循环、
//! 工具注册表与权限闸门，接到 [`ai_ide_llm`] 的模型流上，再把结果翻成协议里的
//! `agent.approval_required` / `agent.finished` 两条通知。
//!
//! ## 三条时序约定
//!
//! 1. **先回 `{run_id}` 再开跑**：与 `llm.chat_stream` 同口径——UI 的「运行」不该等
//!    首字节；`agent.stop` / `agent.decide` 也才有东西可指。因此处理器拿到请求后
//!    立刻 `respond`，随后 `spawn` 一个独立任务跑循环。这条任务不在 `run()` 的
//!    在途集合里，进程退出时由 [`AgentHub::cancel_all`] 统一阻断。
//! 2. **参数与路径在受理前校验完**：越界引用要当场失败（`1002`），而不是先回
//!    `run_id`、再补一条含糊的失败事件——那时 UI 已经在等事件了。
//! 3. **一次运行恰好一条 `agent.finished`**：成功 / 失败 / 被取消都从
//!    [`drive_run`] 的同一个出口发出。
//!
//! ## 模型流怎么变成一轮「模型回复」
//!
//! [`ProviderTurn`] 把 [`Provider::chat_stream`] 的增量事件攒回一条完整的
//! [`TurnReply`]：正文片段拼成 `content`，工具调用按 `index` 归并（首帧带
//! id/name，后续帧只有参数片段）。这一层不新增协议码——模型故障走既有
//! `AgentError::Model`（`4003 PROVIDER_UNAVAILABLE`，且终止循环）。
//!
//! ## 审批怎么走
//!
//! 闸门判定为「要问一声」时，[`PeerDecider`] 生成 `approval_id`、登记一条一次性
//! 应答通道，然后把 `agent.approval_required` 推给主进程；`agent.decide` 找到登记
//! 并回填答复。没人答复（超时 / 运行被停止 / 通道断开）一律判拒——与
//! `crates/agent` 的缺省策略一致，**绝不默认放行**。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use ai_ide_agent::{
    AgentError, AgentLoop, ApprovalReply, ApprovalRequest, Decider, ModelTurn, PermissionGate,
    RunLimits, RunOutcome, StopReason, ToolContext, ToolRegistry, TurnReply, TurnRequest,
    DEFAULT_MAX_ROUNDS, DEFAULT_MAX_STEPS,
};
use ai_ide_llm::{CancelToken, ChatRequest, LlmError, Provider, StreamEvent};
use ai_ide_protocol::dto::{
    AgentApprovalRequiredEvent, AgentDecideParams, AgentDecideResult, AgentFinishedEvent,
    AgentMode, AgentPermissionOverride, AgentRunStatus, AgentStartParams, AgentStartResult,
    AgentStopParams, AgentStopResult, ApprovalDecision, ApprovalRisk, ChatContext, ChatMessage,
    ChatParams, ChatRole, ChatToolCall, ModelRef, ToolChoice,
};
use ai_ide_protocol::{methods, ErrorCode, ErrorObject, Request};
use ai_ide_store::{audit_kind, NewAuditEvent, Store};
use ai_ide_workspace::Workspace;
use async_trait::async_trait;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::oneshot;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::audit_api;
use crate::checkpoint::{CheckpointRecorder, CheckpointStore};
use crate::context;
use crate::dispatch::parse_params;
use crate::llm_api::workspace_error;
use crate::peer::Peer;
use crate::permission_api;
use crate::state::AppState;

/// 审批等待上限。
///
/// 比 `crates/agent` 的缺省值（30 秒）宽得多：这里等的是**人在 IDE 里**读完
/// 「它打算干什么」再点一下，30 秒不够；超时即判拒，不会把运行挂死。
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);

/// 等待审批时检查「运行是否已被停止」的轮询间隔。
///
/// 有它，用户按「停止」之后不必再等满超时才收手。
const CANCEL_POLL: Duration = Duration::from_millis(200);

/// Agent 运行的系统提示。
///
/// 只讲两件事：身份与工具的用法边界。安全红线不写在这里——它们是闸门与 `PathGuard`
/// 的硬约束，靠提示词「请求」模型守规矩是不算数的。
const SYSTEM_PROMPT: &str = "\
你是 AI IDE 内嵌的编码助手。你可以调用工具读取、检索与修改用户工作区里的文件。

工作方式：
- 先自己动手查证再回答，不要凭猜测描述文件内容；需要看文件就用 read / grep / list。
- 改动用 edit（精确替换）或 write（整文件覆盖）或 create（新建），不要臆造已存在的内容。
- 每次工具调用只做一件事，拿到结果后再决定下一步。
- 收工前用一两句话说明你改了什么、为什么；无法完成时如实说明卡在哪。\
";

// ---------------------------------------------------------------------------
// 在途运行登记表
// ---------------------------------------------------------------------------

/// 在途 agent 运行的登记表。
///
/// 与 [`crate::llm_api::StreamHub`] 同族：给 `agent.stop` 一个「按 `run_id` 找到那条
/// 运行」的入口，同时存放尚未答复的审批询问。
#[derive(Default)]
pub struct AgentHub {
    runs: Mutex<HashMap<String, RunEntry>>,
    approvals: Mutex<HashMap<String, PendingApproval>>,
}

/// 一次在途运行的句柄。
struct RunEntry {
    /// 循环每执行完一步写一次；`agent.stop` 用它回答 `steps_done`。
    steps: Arc<AtomicU32>,
    /// 循环侧的取消旗标（[`AgentLoop::run_cancellable`] 读它）。
    cancelled: Arc<AtomicBool>,
    /// 上游请求侧的取消句柄（[`Provider::chat_stream`] 读它）。
    cancel: CancelToken,
}

/// 一条等待用户裁决的询问。
struct PendingApproval {
    run_id: String,
    reply: oneshot::Sender<ApprovalReply>,
}

/// `agent.decide` 的处理结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecideOutcome {
    /// 答复已送达循环。
    Accepted,
    /// 没有这条询问：重复点击、已超时、或运行已结束。按幂等处理，不报错。
    Unknown,
    /// `approval_id` 有，但属于另一次运行。
    Mismatch,
}

impl AgentHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一次新运行；返回 `false` 表示 `run_id` 已在途。
    ///
    /// 不对外：句柄里的取消令牌是内核内部约定，暴露出去等于让别人能替循环做决定。
    fn register_run(&self, run_id: &str, entry: RunEntry) -> bool {
        let mut runs = self.runs();
        if runs.contains_key(run_id) {
            return false;
        }
        runs.insert(run_id.to_string(), entry);
        true
    }

    /// 摘掉一次已结束的运行（幂等），并丢掉它名下尚未答复的询问。
    ///
    /// 丢掉询问等于**判拒**：应答通道没了，闸门拿不到答复就按拒绝走——运行已经结束，
    /// 再放行一次工具调用没有意义。
    pub fn finish_run(&self, run_id: &str) {
        self.runs().remove(run_id);
        let mut approvals = self.approvals();
        approvals.retain(|_, pending| pending.run_id != run_id);
    }

    /// 请求停止一次运行；返回停止前已完成的步数，未命中返回 `None`。
    pub fn cancel(&self, run_id: &str) -> Option<u32> {
        let runs = self.runs();
        let entry = runs.get(run_id)?;
        entry.cancelled.store(true, Ordering::SeqCst);
        entry.cancel.cancel();
        Some(entry.steps.load(Ordering::SeqCst))
    }

    /// 该运行是否已被请求停止。
    pub fn is_stopped(&self, run_id: &str) -> bool {
        self.runs()
            .get(run_id)
            .map(|entry| entry.cancelled.load(Ordering::SeqCst))
            .unwrap_or(true)
    }

    /// 阻断全部在途运行（进程退出前调用）；返回被阻断的条数。
    pub fn cancel_all(&self) -> usize {
        let runs = self.runs();
        for entry in runs.values() {
            entry.cancelled.store(true, Ordering::SeqCst);
            entry.cancel.cancel();
        }
        runs.len()
    }

    /// 当前在途运行条数（测试与诊断用）。
    pub fn live_count(&self) -> usize {
        self.runs().len()
    }

    /// 当前待裁决的询问条数（测试与诊断用）。
    pub fn pending_count(&self) -> usize {
        self.approvals().len()
    }

    /// 登记一条待裁决询问；`approval_id` 冲突时返回 `false`。
    fn register_approval(
        &self,
        approval_id: &str,
        run_id: &str,
        reply: oneshot::Sender<ApprovalReply>,
    ) -> bool {
        let mut approvals = self.approvals();
        if approvals.contains_key(approval_id) {
            return false;
        }
        approvals.insert(
            approval_id.to_string(),
            PendingApproval {
                run_id: run_id.to_string(),
                reply,
            },
        );
        true
    }

    /// 摘掉一条询问（答复已送达或已放弃）。
    fn take_approval(&self, approval_id: &str) {
        self.approvals().remove(approval_id);
    }

    /// 把用户裁决送进等在那里的循环。
    pub fn resolve(
        &self,
        run_id: &str,
        approval_id: &str,
        decision: ApprovalDecision,
        reason: Option<String>,
    ) -> DecideOutcome {
        let mut approvals = self.approvals();
        let Some(pending) = approvals.remove(approval_id) else {
            return DecideOutcome::Unknown;
        };
        if pending.run_id != run_id {
            // run_id 对不上：把登记放回去，免得一次误发把别人的询问吃掉。
            approvals.insert(approval_id.to_string(), pending);
            return DecideOutcome::Mismatch;
        }
        let reply = ApprovalReply { decision, reason };
        // 接收端可能已经走了（超时 / 运行被停止）：丢一个值，不算错误。
        let _ = pending.reply.send(reply);
        DecideOutcome::Accepted
    }

    fn runs(&self) -> MutexGuard<'_, HashMap<String, RunEntry>> {
        self.runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn approvals(&self) -> MutexGuard<'_, HashMap<String, PendingApproval>> {
        self.approvals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

// ---------------------------------------------------------------------------
// agent.start
// ---------------------------------------------------------------------------

pub async fn start(peer: &Peer, state: &AppState, req: &Request) {
    let params: AgentStartParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    // 1) 参数前置校验：`goal` 空串会让模型无从下手，`max_steps = 0` 会让循环一步不跑。
    if let Some(err) = validate_start(&params) {
        return peer.respond_error(&req.id, &err).await;
    }

    // 2) Provider 必须在注册表里（目录里有记录但未启用时注册表里就没有它），
    //    与 `llm.chat_stream` 同口径：如实报 4003，而不是把请求发到不存在的上游。
    let provider = match state.llm().registry().get(&params.model_ref.provider_id) {
        Some(provider) => provider,
        None => {
            let err = AgentError::Model(format!(
                "Provider 未配置或未启用：{}",
                params.model_ref.provider_id
            ));
            return peer.respond_error(&req.id, &err.to_error_object()).await;
        }
    };

    // 3) 事件要推给主进程；没有对端句柄只能如实拒绝——静默丢掉事件比报错更难查。
    let Some(run_peer) = state.peer() else {
        let err = ErrorObject::internal("对端句柄未注入，无法推送 agent.* 事件");
        warn!("拒绝派发：对端未注入");
        return peer.respond_error(&req.id, &err).await;
    };

    // 4) 工作区与上下文引用。`chat` 模式不跑工具，没开工作区也能聊；其余模式必须
    //    有工作区——工具的目标就是它，没有工作区等于无的放矢。
    let needs_workspace = params.mode != AgentMode::Chat || !params.context_refs.is_empty();
    let workspace = if needs_workspace {
        match state.workspace(None) {
            Ok(workspace) => Some(workspace),
            Err(err) => return peer.respond_error(&req.id, &workspace_error(&err)).await,
        }
    } else {
        None
    };

    // 5) 引用内容在**受理前**渲染完。越界路径要让请求当场失败（`1002`），而不是先回
    //    `run_id`、再补一条含糊的失败事件——那时 UI 已经在等事件了。
    let injection = match (&workspace, params.context_refs.is_empty()) {
        (Some(workspace), false) => {
            let reference = ChatContext {
                workspace_id: None,
                refs: params.context_refs.clone(),
                compress: None,
            };
            let probe = Arc::clone(workspace);
            match tokio::task::spawn_blocking(move || context::render(&probe, &reference)).await {
                Ok(Ok(injected)) => {
                    info!(
                        tokens = injected.tokens,
                        omitted = injected.omitted.len(),
                        summarized = injected.summarized.len(),
                        "已渲染 agent 上下文引用"
                    );
                    Some(injected.content)
                }
                Ok(Err(err)) => return peer.respond_error(&req.id, &workspace_error(&err)).await,
                Err(join_err) => {
                    let err = ErrorObject::internal(format!("上下文引用任务异常退出：{join_err}"));
                    return peer.respond_error(&req.id, &err).await;
                }
            }
        }
        _ => None,
    };

    // 6) 登记运行句柄。`run_id` 由内核生成：让调用方指定会让「同 id 并发」变成
    //    一个必须处理的错误面，而它对本协议没有价值。
    let run_id = Uuid::new_v4().to_string();
    let hub = state.agent();
    let steps = Arc::new(AtomicU32::new(0));
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel = CancelToken::new();
    if !hub.register_run(
        &run_id,
        RunEntry {
            steps: Arc::clone(&steps),
            cancelled: Arc::clone(&cancelled),
            cancel: cancel.clone(),
        },
    ) {
        let err = ErrorObject::internal("run_id 冲突，请重试");
        return peer.respond_error(&req.id, &err).await;
    }

    // 7) 先回结果，再开跑：`{run_id}` 是「已受理」的回执，内容是后续事件的事。
    peer.respond(
        &req.id,
        &AgentStartResult {
            run_id: run_id.clone(),
        },
    )
    .await;

    let ctx = RunCtx {
        peer: run_peer,
        hub,
        run_id: run_id.clone(),
        model_ref: params.model_ref.clone(),
        goal: params.goal.clone(),
        mode: params.mode,
        session_id: params.session_id.clone(),
        limits: RunLimits::new(
            DEFAULT_MAX_ROUNDS,
            params.max_steps.unwrap_or(DEFAULT_MAX_STEPS),
        ),
        override_rules: params.permissions_override.clone().unwrap_or_default(),
        steps,
        cancelled,
        cancel,
        store: state.store().cloned(),
    };

    info!(
        run_id = %run_id,
        provider_id = %params.model_ref.provider_id,
        model_id = %params.model_ref.model_id,
        mode = ?params.mode,
        "受理 agent 运行"
    );
    tokio::spawn(async move {
        drive_run(ctx, provider, workspace, injection).await;
    });
}

/// 参数校验：返回第一个不合格项对应的协议错误。
fn validate_start(params: &AgentStartParams) -> Option<ErrorObject> {
    if params.goal.trim().is_empty() {
        return Some(ErrorObject::new(ErrorCode::INVALID_PARAMS, "goal 不能为空"));
    }
    if params.model_ref.provider_id.trim().is_empty() {
        return Some(ErrorObject::new(
            ErrorCode::INVALID_PARAMS,
            "model_ref.provider_id 不能为空",
        ));
    }
    if params.model_ref.model_id.trim().is_empty() {
        return Some(ErrorObject::new(
            ErrorCode::INVALID_PARAMS,
            "model_ref.model_id 不能为空",
        ));
    }
    if params.max_steps == Some(0) {
        return Some(ErrorObject::new(
            ErrorCode::INVALID_PARAMS,
            "max_steps 至少要 1",
        ));
    }
    None
}

// ---------------------------------------------------------------------------
// agent.stop / agent.decide
// ---------------------------------------------------------------------------

pub async fn stop(peer: &Peer, state: &AppState, req: &Request) {
    let params: AgentStopParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let result = match state.agent().cancel(&params.run_id) {
        Some(steps) => {
            info!(run_id = %params.run_id, steps, "已请求停止 agent 运行");
            AgentStopResult {
                ok: true,
                steps_done: steps,
            }
        }
        // 幂等：运行可能刚好自己收工了，也可能 id 早已作废。这里如实回 `ok:false`，
        // 而不是报一个「没有这个 run」的错误——UI 的停止按钮不该弹错误框。
        None => {
            debug!(run_id = %params.run_id, "停止未命中在途运行，按幂等返回");
            AgentStopResult {
                ok: false,
                steps_done: 0,
            }
        }
    };
    peer.respond(&req.id, &result).await;
}

pub async fn decide(peer: &Peer, state: &AppState, req: &Request) {
    let params: AgentDecideParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let outcome = state.agent().resolve(
        &params.run_id,
        &params.approval_id,
        params.decision,
        params.reason.clone(),
    );
    match outcome {
        DecideOutcome::Accepted => {
            info!(
                run_id = %params.run_id,
                approval_id = %params.approval_id,
                decision = ?params.decision,
                "裁决已送达 agent 运行"
            );
            peer.respond(&req.id, &AgentDecideResult { ok: true }).await;
        }
        DecideOutcome::Unknown => {
            // 重复点击「允许」、询问已超时、运行已结束都会走到这里：不是错误。
            debug!(
                approval_id = %params.approval_id,
                "裁决未命中待决询问，按幂等返回 ok:false"
            );
            peer.respond(&req.id, &AgentDecideResult { ok: false })
                .await;
        }
        DecideOutcome::Mismatch => {
            let err = ErrorObject::new(
                ErrorCode::INVALID_PARAMS,
                format!("run_id 与询问不匹配：{}", params.approval_id),
            );
            peer.respond_error(&req.id, &err).await;
        }
    }
}

// ---------------------------------------------------------------------------
// 运行任务
// ---------------------------------------------------------------------------

/// 一次运行的上下文：把「跑完要用的东西」打成一包，免得函数参数一排排。
struct RunCtx {
    peer: Arc<Peer>,
    hub: Arc<AgentHub>,
    run_id: String,
    model_ref: ModelRef,
    goal: String,
    mode: AgentMode,
    /// 原样回落：M4 还没有会话持久化，留作下一片切片的入口。
    session_id: Option<String>,
    limits: RunLimits,
    override_rules: AgentPermissionOverride,
    steps: Arc<AtomicU32>,
    cancelled: Arc<AtomicBool>,
    cancel: CancelToken,
    /// 会话库句柄：Agent 改文件前拍的检查点要记一条审计（M5 切片五）。
    /// 启动时库没连上就是 `None`，此时只是少一条审计，不影响运行。
    store: Option<Store>,
}

/// 跑完一次运行并把结果翻成 `agent.finished`。
///
/// 无论成功、失败还是被取消，收尾事件都从**这里**发出——这就是「恰好一条
/// `agent.finished`」的实现点。
async fn drive_run(
    ctx: RunCtx,
    provider: Arc<dyn Provider>,
    workspace: Option<Arc<Workspace>>,
    injection: Option<String>,
) {
    let model = ProviderTurn {
        provider,
        model_ref: ctx.model_ref.clone(),
        params: ChatParams::default(),
        tools: ctx.mode != AgentMode::Chat,
        cancel: ctx.cancel.clone(),
    };

    // 首轮消息：系统提示 → 引用材料 → 用户目标。
    // 顺序是有意的——外部材料放在目标之前，模型读到时已有全局视角。
    let mut messages = vec![ChatMessage {
        role: ChatRole::System,
        content: SYSTEM_PROMPT.to_string(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
    }];
    if let Some(injected) = injection {
        messages.push(ChatMessage {
            role: ChatRole::System,
            content: injected,
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
    let goal = if ctx.goal.trim().is_empty() {
        "（未给出目标）".to_string()
    } else {
        ctx.goal.clone()
    };
    messages.push(ChatMessage {
        role: ChatRole::User,
        content: goal,
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
    });

    let (status, summary) = if ctx.mode == AgentMode::Chat {
        single_turn(&model, messages, &ctx).await
    } else {
        match workspace.as_ref() {
            Some(workspace) => run_loop(&model, &ctx, workspace, messages).await,
            None => (
                AgentRunStatus::Failed,
                Some("尚未打开工作区，无法运行工具".to_string()),
            ),
        }
    };

    let steps = ctx.steps.load(Ordering::SeqCst);
    // 先摘登记再发事件：登记表里留一条已结束的运行，会让同 id 的停止请求误命中。
    ctx.hub.finish_run(&ctx.run_id);
    ctx.peer
        .notify(
            methods::event::AGENT_FINISHED,
            &AgentFinishedEvent {
                run_id: ctx.run_id.clone(),
                status,
                steps,
                summary,
            },
        )
        .await;
    info!(
        run_id = %ctx.run_id,
        session_id = ?ctx.session_id,
        status = ?status,
        steps,
        "agent 运行结束"
    );
}

/// `chat` 模式：一次模型调用，不跑工具。
async fn single_turn(
    model: &ProviderTurn,
    messages: Vec<ChatMessage>,
    ctx: &RunCtx,
) -> (AgentRunStatus, Option<String>) {
    let request = TurnRequest {
        messages,
        tools: Vec::new(),
    };
    match model.complete(request).await {
        Ok(reply) => {
            let status = if ctx.cancelled.load(Ordering::SeqCst) {
                AgentRunStatus::Cancelled
            } else {
                AgentRunStatus::Completed
            };
            (status, non_empty(reply.content))
        }
        Err(err) => (AgentRunStatus::Failed, Some(err.to_string())),
    }
}

/// `agent` / `inline` 模式：跑完整闭环（模型 → 工具 → 模型）。
async fn run_loop(
    model: &ProviderTurn,
    ctx: &RunCtx,
    workspace: &Workspace,
    messages: Vec<ChatMessage>,
) -> (AgentRunStatus, Option<String>) {
    let registry = ToolRegistry::core();
    let gate = PermissionGate::with_decider(PeerDecider {
        peer: Arc::clone(&ctx.peer),
        hub: Arc::clone(&ctx.hub),
        run_id: ctx.run_id.clone(),
        allow: ctx.override_rules.allow_tools.clone(),
        deny: ctx.override_rules.deny_tools.clone(),
        seq: AtomicU64::new(1),
    })
    .with_timeout(APPROVAL_TIMEOUT);

    // 免问规则落盘（M5 切片六）：`allow_always` 在闸门里命中时就记进库，重启后
    // 由 `seed_always` 灌回来——否则「总是允许」只是这一次进程的承诺。
    // 库里读不到就按空集合起跑，最多是再问一遍。
    let gate = if let Some(store) = &ctx.store {
        let scope = permission_api::scope_for_root(workspace.root.to_string_lossy().as_ref());
        let seeded = permission_api::seed_gate(&gate, store, Some(scope.as_str())).await;
        if seeded > 0 {
            debug!(seeded, scope = %scope, "已灌入历史免问规则");
        }
        gate.with_recorder(permission_api::StoreRecorder::new(store.clone()))
    } else {
        gate
    };

    // Agent 改动前的自动检查点（M5 切片五）：懒建目录，一次运行里同一文件只拍一次，
    // 一个文件都没改就什么都不留。记录器初始化失败只告警——少一份快照不该让整次
    // 运行作废。
    let recorder = match CheckpointRecorder::new(
        CheckpointStore::from_env(),
        workspace.root.clone(),
        ctx.session_id.clone(),
        &ctx.run_id,
    ) {
        Ok(recorder) => Some(recorder),
        Err(err) => {
            warn!(error = %err, "检查点记录器初始化失败，本次运行不留快照");
            None
        }
    };

    let tool_ctx = ToolContext::new(workspace);
    let mut runner = AgentLoop::new(model, &registry)
        .with_gate(&gate)
        .with_limits(ctx.limits)
        .with_progress(&ctx.steps);
    if let Some(recorder) = &recorder {
        runner = runner.with_write_hook(recorder);
    }

    let outcome = runner
        .run_cancellable(&tool_ctx, messages, &ctx.cancelled)
        .await;

    // 收工即定稿：把这一轮改过的文件落成一份检查点，并留一条审计。
    // 必须在 `outcome` 分支前做——被取消 / 撞上限的运行同样改过磁盘，同样该能回滚。
    if let Some(recorder) = &recorder {
        if let Some(checkpoint_id) = recorder.finish() {
            if let Some(store) = &ctx.store {
                audit_api::record(
                    store,
                    NewAuditEvent::core(
                        audit_kind::CHECKPOINT_CREATED,
                        format!("Agent 改动前自动留痕（run {}）", short_run_id(&ctx.run_id)),
                    )
                    .with_session(ctx.session_id.as_deref().unwrap_or_default())
                    .with_detail(serde_json::json!({
                        "checkpoint_id": checkpoint_id,
                        "run_id": ctx.run_id,
                        "workspace_root": workspace.root.display().to_string(),
                        "origin": "agent",
                    })),
                )
                .await;
            }
            info!(run_id = %ctx.run_id, checkpoint_id = %checkpoint_id, "已留下 Agent 自动检查点");
        }
    }

    match outcome {
        Ok(outcome) => match outcome.stop_reason {
            StopReason::Completed => (
                AgentRunStatus::Completed,
                last_assistant_text(&outcome.messages),
            ),
            StopReason::Cancelled { .. } => (
                AgentRunStatus::Cancelled,
                last_assistant_text(&outcome.messages),
            ),
            StopReason::MaxRounds { .. } | StopReason::MaxSteps { .. } => {
                (AgentRunStatus::Failed, Some(exhausted_summary(&outcome)))
            }
        },
        Err(err) => (AgentRunStatus::Failed, Some(err.to_string())),
    }
}

/// 模型回复的最后一段人话（收尾用）。
fn last_assistant_text(messages: &[ChatMessage]) -> Option<String> {
    messages
        .iter()
        .rfind(|item| item.role == ChatRole::Assistant)
        .and_then(|item| non_empty(item.content.clone()))
}

/// 取 run_id 前 8 位：检查点标题要短到能在列表里一眼看完。
fn short_run_id(run_id: &str) -> &str {
    run_id.get(..8).unwrap_or(run_id)
}

/// 撞上限时给用户的一句交代。
fn exhausted_summary(outcome: &RunOutcome) -> String {
    let note = match outcome.stop_reason {
        StopReason::MaxRounds { rounds } => format!("已用满 {rounds} 轮仍未收工"),
        StopReason::MaxSteps { steps } => format!("已用满 {steps} 步仍未收工"),
        _ => "运行未收敛".to_string(),
    };
    match last_assistant_text(&outcome.messages) {
        Some(text) => format!("{note}；模型最后说：{text}"),
        None => note,
    }
}

/// 去掉空白后为空的字符串当作「没有内容」。
fn non_empty(text: String) -> Option<String> {
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

// ---------------------------------------------------------------------------
// 模型流 → 一轮回复
// ---------------------------------------------------------------------------

/// 把 [`Provider::chat_stream`] 的增量事件攒成一条完整的模型回复。
struct ProviderTurn {
    provider: Arc<dyn Provider>,
    model_ref: ModelRef,
    params: ChatParams,
    /// `chat` 模式不带工具声明，模型也就不会请求工具。
    tools: bool,
    cancel: CancelToken,
}

#[async_trait]
impl ModelTurn for ProviderTurn {
    async fn complete(&self, request: TurnRequest) -> Result<TurnReply, AgentError> {
        let chat = ChatRequest {
            model: self.model_ref.clone(),
            messages: request.messages,
            tools: if self.tools {
                request.tools
            } else {
                Vec::new()
            },
            tool_choice: if self.tools {
                Some(ToolChoice::Auto)
            } else {
                None
            },
            params: self.params.clone(),
        };

        let (tx, mut rx) = unbounded_channel::<StreamEvent>();
        let provider = Arc::clone(&self.provider);
        let cancel = self.cancel.clone();
        let upstream = tokio::spawn(async move { provider.chat_stream(chat, tx, cancel).await });

        let mut content = String::new();
        let mut calls: Vec<PendingCall> = Vec::new();
        while let Some(event) = rx.recv().await {
            match event {
                StreamEvent::Delta { text } => content.push_str(&text),
                // 思维链不回灌：它是给人看的，不是对话历史的一部分。
                StreamEvent::Reasoning { text } => debug!(chars = text.len(), "思维链片段已忽略"),
                StreamEvent::ToolCall {
                    index,
                    id,
                    name,
                    arguments_delta,
                } => {
                    let slot = index as usize;
                    if calls.len() <= slot {
                        calls.resize(slot + 1, PendingCall::default());
                    }
                    if let Some(call) = calls.get_mut(slot) {
                        call.apply(id, name, arguments_delta);
                    }
                }
                StreamEvent::Usage(_) | StreamEvent::Finished { .. } => {}
            }
        }

        match upstream.await {
            Ok(Ok(_usage)) => {}
            // 取消不是故障：返回一条空回复，循环下一轮开头就会看到取消旗标并收手。
            // 这里若报错，用户自己按的「停止」会被当成运行失败。
            Ok(Err(LlmError::Cancelled)) => return Ok(TurnReply::text_only(String::new())),
            Ok(Err(err)) => return Err(AgentError::Model(err.to_string())),
            Err(join_err) => {
                return Err(AgentError::Model(format!(
                    "模型接入任务异常退出：{join_err}"
                )))
            }
        }

        let tool_calls = calls
            .into_iter()
            .filter_map(PendingCall::finish)
            .collect::<Vec<_>>();
        Ok(TurnReply::with_tool_calls(content, tool_calls))
    }
}

/// 攒在一半的工具调用：首帧带 id / name，后续帧只有参数片段。
#[derive(Default, Clone)]
struct PendingCall {
    id: String,
    name: String,
    arguments: String,
}

impl PendingCall {
    fn apply(&mut self, id: Option<String>, name: Option<String>, arguments_delta: String) {
        if let Some(id) = id {
            self.id = id;
        }
        if let Some(name) = name {
            self.name = name;
        }
        self.arguments.push_str(&arguments_delta);
    }

    /// 收块：没有名字的残块丢掉（派发不出去，留着只会变成一条「未注册的工具」回灌）。
    fn finish(self) -> Option<ChatToolCall> {
        if self.name.is_empty() {
            return None;
        }
        let id = if self.id.is_empty() {
            // 上游没给 id 时兜一个：工具结果消息要靠它配对，缺了会让配对整个错位。
            format!("call-{}", Uuid::new_v4())
        } else {
            self.id
        };
        Some(ChatToolCall {
            id,
            name: self.name,
            arguments: self.arguments,
        })
    }
}

// ---------------------------------------------------------------------------
// 审批
// ---------------------------------------------------------------------------

/// 把审批询问送到主进程、等用户点一次的裁决者。
struct PeerDecider {
    peer: Arc<Peer>,
    hub: Arc<AgentHub>,
    run_id: String,
    /// `permissions_override.allow_tools`：直接放行，不打扰用户。
    allow: Vec<String>,
    /// `permissions_override.deny_tools`：直接拒绝，优先于 `allow`。
    deny: Vec<String>,
    /// 同一运行内的询问序号，保证 `approval_id` 不重。
    seq: AtomicU64,
}

#[async_trait]
impl Decider for PeerDecider {
    async fn decide(&self, request: &ApprovalRequest) -> Option<ApprovalReply> {
        // 覆盖表先判，且**拒绝优先**：口径与协议注释一致（`deny_tools` 优先于
        // `allow_tools`）。覆盖表只作用于「本来要问一声」的调用——闸门的硬拒
        // （越界路径、敏感路径、高危命令）根本不会走到这里，翻不动。
        if self.deny.iter().any(|name| name == &request.tool) {
            debug!(run_id = %self.run_id, tool = %request.tool, "权限覆盖表判拒，未打扰用户");
            return Some(ApprovalReply::with_reason(
                ApprovalDecision::DenyWithReason,
                "本次运行的权限覆盖表把它列为拒绝",
            ));
        }
        if self.allow.iter().any(|name| name == &request.tool) {
            debug!(run_id = %self.run_id, tool = %request.tool, "权限覆盖表放行，未打扰用户");
            return Some(ApprovalReply::new(ApprovalDecision::AllowOnce));
        }

        let approval_id = format!(
            "{}-{}",
            self.run_id,
            self.seq.fetch_add(1, Ordering::Relaxed)
        );
        let (tx, mut rx) = oneshot::channel::<ApprovalReply>();
        if !self.hub.register_approval(&approval_id, &self.run_id, tx) {
            warn!(approval_id = %approval_id, "审批 id 冲突，按拒绝处理");
            return None;
        }

        self.peer
            .notify(
                methods::event::AGENT_APPROVAL_REQUIRED,
                &AgentApprovalRequiredEvent {
                    run_id: self.run_id.clone(),
                    approval_id: approval_id.clone(),
                    tool: request.tool.clone(),
                    args: approval_args(request),
                    risk: risk_of(&request.tool),
                    reason: request.reason.clone(),
                },
            )
            .await;

        // 一边等用户点，一边盯着取消旗标：用户按「停止」之后，不该再等满超时。
        let reply = loop {
            tokio::select! {
                reply = &mut rx => break reply.ok(),
                _ = tokio::time::sleep(CANCEL_POLL) => {
                    if self.hub.is_stopped(&self.run_id) {
                        debug!(approval_id = %approval_id, "运行已停止，审批按判拒收手");
                        break None;
                    }
                }
            }
        };
        self.hub.take_approval(&approval_id);
        reply
    }
}

/// 给 UI 展示的调用参数。
///
/// 闸门只把「目标路径」与「命令行」两样东西交出来（`ApprovalRequest` 的全部字段），
/// 因此这里也只展示这两样——不臆造工具自己的完整参数。
fn approval_args(request: &ApprovalRequest) -> serde_json::Value {
    let mut args = serde_json::Map::new();
    if let Some(path) = &request.path {
        args.insert("path".to_string(), serde_json::Value::String(path.clone()));
    }
    if let Some(command) = &request.command {
        args.insert(
            "command".to_string(),
            serde_json::Value::String(command.clone()),
        );
    }
    serde_json::Value::Object(args)
}

/// 工具名 → 风险等级（UI 据此标红）。
///
/// 判断依据是「这一步会造成什么后果」：跑命令不可预知 → 高；会改工作区内容 → 中；
/// 只读 → 低。认不出来的工具一律按「中」——不知道的事不做乐观假设。
fn risk_of(tool: &str) -> ApprovalRisk {
    match tool {
        "run_command" => ApprovalRisk::High,
        "write" | "edit" | "create" | "apply_diff" => ApprovalRisk::Medium,
        "read" | "list" | "grep" => ApprovalRisk::Low,
        _ => ApprovalRisk::Medium,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_run(hub: &Arc<AgentHub>) -> (String, Arc<AtomicU32>, Arc<AtomicBool>) {
        let run_id = "run-1".to_string();
        let steps = Arc::new(AtomicU32::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        assert!(hub.register_run(
            &run_id,
            RunEntry {
                steps: Arc::clone(&steps),
                cancelled: Arc::clone(&cancelled),
                cancel: CancelToken::new(),
            },
        ));
        (run_id, steps, cancelled)
    }

    #[test]
    fn validate_start_rejects_empty_goal_and_zero_steps() {
        let base = AgentStartParams {
            session_id: None,
            model_ref: ModelRef {
                provider_id: "p1".into(),
                model_id: "m1".into(),
            },
            goal: "看一下 README".into(),
            mode: AgentMode::Agent,
            context_refs: Vec::new(),
            max_steps: None,
            permissions_override: None,
        };
        assert!(validate_start(&base).is_none());

        let mut empty_goal = base.clone();
        empty_goal.goal = "   ".into();
        assert_eq!(
            validate_start(&empty_goal).map(|err| err.code),
            Some(ErrorCode::INVALID_PARAMS)
        );

        let mut zero_steps = base.clone();
        zero_steps.max_steps = Some(0);
        assert!(validate_start(&zero_steps).is_some());

        let mut no_model = base;
        no_model.model_ref.model_id = "".into();
        assert!(validate_start(&no_model).is_some());
    }

    #[test]
    fn stop_flags_cancel_and_reports_steps_so_far() {
        let hub = Arc::new(AgentHub::new());
        let (run_id, steps, cancelled) = ctx_run(&hub);
        steps.store(3, Ordering::SeqCst);

        assert_eq!(hub.cancel(&run_id), Some(3));
        assert!(cancelled.load(Ordering::SeqCst), "停止要真的传到循环那一侧");
        assert!(hub.is_stopped(&run_id));

        // 运行已结束（摘了登记）之后再停：幂等，回 None。
        hub.finish_run(&run_id);
        assert_eq!(hub.cancel(&run_id), None);
        assert_eq!(hub.live_count(), 0);
    }

    #[tokio::test]
    async fn decide_reaches_the_waiting_loop() {
        let hub = Arc::new(AgentHub::new());
        let (run_id, _steps, _cancelled) = ctx_run(&hub);
        let (tx, rx) = oneshot::channel::<ApprovalReply>();
        assert!(hub.register_approval("a-1", &run_id, tx));
        assert_eq!(hub.pending_count(), 1);

        let decision = hub.resolve(
            &run_id,
            "a-1",
            ApprovalDecision::DenyWithReason,
            Some("别写这个文件".into()),
        );
        assert_eq!(decision, DecideOutcome::Accepted);
        assert_eq!(hub.pending_count(), 0);

        let reply = rx.await.expect("答复应送达");
        assert_eq!(reply.decision, ApprovalDecision::DenyWithReason);
        assert_eq!(reply.reason.as_deref(), Some("别写这个文件"));

        // 重复点击：幂等，不报错。
        assert_eq!(
            hub.resolve(&run_id, "a-1", ApprovalDecision::AllowOnce, None),
            DecideOutcome::Unknown
        );

        // run_id 对不上：登记要留在原地，不能被误吃。
        let (tx2, _rx2) = oneshot::channel::<ApprovalReply>();
        assert!(hub.register_approval("a-2", &run_id, tx2));
        assert_eq!(
            hub.resolve("run-2", "a-2", ApprovalDecision::AllowOnce, None),
            DecideOutcome::Mismatch
        );
        assert_eq!(hub.pending_count(), 1);
    }

    #[test]
    fn finishing_a_run_drops_its_pending_approvals() {
        let hub = Arc::new(AgentHub::new());
        let (run_id, _steps, _cancelled) = ctx_run(&hub);
        let (tx, mut rx) = oneshot::channel::<ApprovalReply>();
        assert!(hub.register_approval("a-1", &run_id, tx));

        hub.finish_run(&run_id);

        assert_eq!(hub.pending_count(), 0, "运行结束后不该留下待决询问");
        assert!(
            matches!(rx.try_recv(), Err(oneshot::error::TryRecvError::Closed)),
            "发送端一丢，等在那里的闸门就当没答复——判拒，绝不放行"
        );
    }

    #[test]
    fn cancel_all_blocks_every_live_run() {
        let hub = Arc::new(AgentHub::new());
        let (_id, _steps, cancelled) = ctx_run(&hub);
        assert_eq!(hub.cancel_all(), 1);
        assert!(cancelled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn decider_honours_permission_override() {
        let hub = Arc::new(AgentHub::new());
        let decider = PeerDecider {
            peer: Arc::new(Peer::new()),
            hub: Arc::clone(&hub),
            run_id: "run-1".into(),
            allow: vec!["edit".into()],
            deny: vec!["run_command".into()],
            seq: AtomicU64::new(1),
        };
        let request = |tool: &str| ApprovalRequest {
            tool: tool.to_string(),
            path: Some("src/a.ts".into()),
            command: None,
            reason: "写入工作区文件".into(),
        };

        let denied = decider
            .decide(&request("run_command"))
            .await
            .expect("要有答复");
        assert_eq!(denied.decision, ApprovalDecision::DenyWithReason);
        assert!(denied.reason.is_some(), "拒绝理由要能回灌给模型");

        let allowed = decider.decide(&request("edit")).await.expect("要有答复");
        assert_eq!(allowed.decision, ApprovalDecision::AllowOnce);

        // 两条都命中覆盖表：拒绝优先，与协议注释一致。
        let both = decider
            .decide(&request("run_command"))
            .await
            .expect("要有答复");
        assert_eq!(both.decision, ApprovalDecision::DenyWithReason);

        // 覆盖表放行 / 判拒都不该产生待决询问（没打扰用户）。
        assert_eq!(hub.pending_count(), 0);
    }

    #[test]
    fn pending_call_splits_fragments_back_into_one_call() {
        let mut call = PendingCall::default();
        call.apply(Some("c1".into()), Some("edit".into()), "{\"path\":".into());
        call.apply(None, None, "\"a.ts\"}".into());

        let done = call.finish().expect("有名有参，应该收成一调用");
        assert_eq!(done.id, "c1");
        assert_eq!(done.name, "edit");
        assert_eq!(done.arguments, "{\"path\":\"a.ts\"}");
    }

    #[test]
    fn pending_call_drops_nameless_remnants() {
        let mut call = PendingCall::default();
        call.apply(None, None, "{}".into());
        assert!(
            call.finish().is_none(),
            "没有名字的残块派发不出去，必须丢掉"
        );
    }

    #[test]
    fn pending_call_falls_back_to_a_generated_id() {
        let mut call = PendingCall::default();
        call.apply(None, Some("read".into()), "{}".into());
        let done = call.finish().expect("有名即可派发");
        assert!(
            !done.id.is_empty(),
            "缺 id 时要兜一个，否则工具结果配不上对"
        );
    }

    #[test]
    fn risk_of_is_conservative_for_unknown_tools() {
        assert_eq!(risk_of("run_command"), ApprovalRisk::High);
        assert_eq!(risk_of("write"), ApprovalRisk::Medium);
        assert_eq!(risk_of("read"), ApprovalRisk::Low);
        assert_eq!(risk_of("some_future_tool"), ApprovalRisk::Medium);
    }

    #[test]
    fn approval_args_carries_only_what_the_gate_exposed() {
        let empty = approval_args(&ApprovalRequest {
            tool: "edit".into(),
            path: None,
            command: None,
            reason: "改文件".into(),
        });
        assert_eq!(empty, serde_json::json!({}));

        let full = approval_args(&ApprovalRequest {
            tool: "run_command".into(),
            path: None,
            command: Some("npm test".into()),
            reason: "跑测试".into(),
        });
        assert_eq!(full, serde_json::json!({ "command": "npm test" }));
    }

    #[test]
    fn summaries_prefer_the_last_thing_the_model_said() {
        let messages = vec![
            ChatMessage {
                role: ChatRole::Assistant,
                content: "第一步".into(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            ChatMessage {
                role: ChatRole::Tool,
                content: "结果".into(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: Some("c1".into()),
            },
            ChatMessage {
                role: ChatRole::Assistant,
                content: "好了，已改完".into(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
        ];
        assert_eq!(
            last_assistant_text(&messages).as_deref(),
            Some("好了，已改完")
        );

        let outcome = RunOutcome {
            messages: messages.clone(),
            stop_reason: StopReason::MaxSteps { steps: 64 },
            rounds: 12,
            steps: 64,
            appended: 3,
            assistant_errors: Vec::new(),
        };
        let note = exhausted_summary(&outcome);
        assert!(note.contains("64"), "要交代撞的是哪个上限：{note}");
        assert!(
            note.contains("好了，已改完"),
            "也别把模型最后的话丢掉：{note}"
        );
    }

    #[test]
    fn blank_content_counts_as_no_summary() {
        assert!(non_empty("  \n ".to_string()).is_none());
        assert_eq!(non_empty("有内容".to_string()).as_deref(), Some("有内容"));
    }
}
