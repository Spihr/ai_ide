//! 流式对话与模型枚举的处理器（M2）。
//!
//! 这一层负责把 [`ai_ide_llm`] 的运行期事件翻译成协议里的 `llm.*` 通知，
//! 并且**把三条不变量收在一个实现点**（`docs/03` §2.4）：
//!
//! | 不变量 | 实现点 |
//! |---|---|
//! | 一次流恰好一条 `llm.started` | 派发前由本模块发出，provider 不参与 |
//! | `llm.finished` 与 `llm.error` 互斥且恰好一条 | 读流循环结束后**统一**收尾 |
//! | 取消只影响在途流，且重复取消无副作用 | [`StreamHub`] + `CancellationToken` |
//!
//! **时序**：`llm.chat_stream` **先回 `{stream_id}` 再开流**——UI 的「发送」不该等首字节，
//! 取消也才有东西可取消。因此处理器拿到请求后立刻 `respond`，随后 `spawn` 一个
//! 独立任务读上游：这条任务不在 `run()` 的在途集合里，进程退出时由
//! `StreamHub::cancel_all` 统一阻断（`docs/03` §3.3 的「流不跨进程存活」）。
//!
//! 密钥、协议适配与 SSE 分帧都在 `crates/llm`，本模块只做参数校验、注册表查找、
//! 事件翻译与错误码折返。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use ai_ide_llm::tokens::TokenCounter;
use ai_ide_llm::{CancelToken, ChatRequest, LlmError, Provider, StreamEvent};
use ai_ide_protocol::dto::{
    ChatMessage, ChatRole, FinishReason, LlmCancelParams, LlmChatStreamParams, LlmChatStreamResult,
    LlmCountTokensParams, LlmCountTokensResult, LlmDeltaEvent, LlmErrorEvent, LlmFinishedEvent,
    LlmListModelsParams, LlmListModelsResult, LlmReasoningEvent, LlmStartedEvent, LlmToolCallEvent,
    LlmUsageEvent, ModelRef, OkResult,
};
use ai_ide_protocol::{methods, ErrorCode, ErrorObject, Request};
use ai_ide_workspace::WorkspaceError;
use tokio::sync::mpsc::unbounded_channel;
use tracing::{debug, error, info, warn};

use crate::context;
use crate::dispatch::parse_params;
use crate::peer::Peer;
use crate::state::{now_ms, AppState};

// ---------------------------------------------------------------------------
// 在途流登记表
// ---------------------------------------------------------------------------

/// 在途流式对话的取消句柄表。
///
/// 存在的理由有两个：一是给 `llm.cancel` 一个「按 `stream_id` 找到那条流」的入口，
/// 二是让同一 `stream_id` 的并发派发能被拒绝——两个任务共用一个 id 时，
/// 取消会打到错误的流上，而 UI 只会看到一片错乱的事件。
#[derive(Default)]
pub struct StreamHub {
    live: Mutex<HashMap<String, CancelToken>>,
}

impl StreamHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一条新流；返回 `false` 表示该 `stream_id` 已在途（调用方应拒绝本次派发）。
    pub fn register(&self, stream_id: &str, cancel: CancelToken) -> bool {
        let mut live = self.live();
        if live.contains_key(stream_id) {
            return false;
        }
        live.insert(stream_id.to_string(), cancel);
        true
    }

    /// 摘掉一条已结束的流。**幂等**：找不到也算完成。
    pub fn finish(&self, stream_id: &str) {
        self.live().remove(stream_id);
    }

    /// 取消一条在途流；返回是否命中。未命中不算错误——`llm.cancel` 的语义就是幂等。
    pub fn cancel(&self, stream_id: &str) -> bool {
        match self.live().get(stream_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// 阻断全部在途流（进程退出前调用）；返回被阻断的条数。
    pub fn cancel_all(&self) -> usize {
        let live = self.live();
        for token in live.values() {
            token.cancel();
        }
        live.len()
    }

    /// 当前在途条数（测试与诊断用）。
    pub fn live_count(&self) -> usize {
        self.live().len()
    }

    /// 加锁；锁中毒时取回内部值继续用——一次 panic 不该让整个对话面瘫掉。
    fn live(&self) -> MutexGuard<'_, HashMap<String, CancelToken>> {
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

// ---------------------------------------------------------------------------
// llm.chat_stream
// ---------------------------------------------------------------------------

pub async fn chat_stream(peer: &Peer, state: &AppState, req: &Request) {
    let params: LlmChatStreamParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    // 1) 参数前置校验：`stream_id` 是取消的唯一抓手，空串会让「停止」按钮无从下手。
    if let Some(err) = validate_stream_params(&params) {
        return peer.respond_error(&req.id, &err).await;
    }

    // 2) Provider 必须在注册表里。目录里有记录但未启用（或协议未接线）时，
    //    注册表里就没有它——这里如实报 2001，而不是把请求发到一个不存在的上游。
    let provider = match state.llm().registry().get(&params.model_ref.provider_id) {
        Some(provider) => provider,
        None => {
            let err = LlmError::ProviderNotConfigured {
                provider_id: params.model_ref.provider_id.clone(),
            };
            return respond_llm_error(peer, req, &err).await;
        }
    };

    // 3) 事件要推给主进程，没有对端句柄就只能如实拒绝：静默丢掉事件比报错更难查。
    let Some(stream_peer) = state.peer() else {
        let error = ErrorObject::internal("对端句柄未注入，无法推送 llm.* 事件");
        warn!(stream_id = %params.stream_id, "拒绝派发：对端未注入");
        return peer.respond_error(&req.id, &error).await;
    };

    // 4) 上下文引用（M3）：把 `@文件 / @文件夹 / @选区` 的内容渲染出来。
    //    必须在登记取消句柄之前做完——越界路径要让请求当场失败，而不是先受理、
    //    发完 `{stream_id}` 再报错（那时 UI 已经在等事件，收尾语义会变得含糊）。
    let injection = match &params.context {
        Some(reference) if !reference.refs.is_empty() => {
            let workspace = match state.workspace(reference.workspace_id.as_deref()) {
                Ok(workspace) => workspace,
                Err(err) => return peer.respond_error(&req.id, &workspace_error(&err)).await,
            };
            let reference = reference.clone();
            // 读盘与列目录都是阻塞活，扔进阻塞池，别占住运行时的线程。
            match tokio::task::spawn_blocking(move || context::render(&workspace, &reference)).await
            {
                Ok(Ok(injected)) => Some(injected),
                Ok(Err(err)) => return peer.respond_error(&req.id, &workspace_error(&err)).await,
                Err(join_err) => {
                    let error =
                        ErrorObject::internal(format!("上下文引用任务异常退出：{join_err}"));
                    return peer.respond_error(&req.id, &error).await;
                }
            }
        }
        _ => None,
    };

    // 5) 登记取消句柄。同 id 并发会互相干扰，直接拒绝。
    let streams = state.streams();
    let cancel = CancelToken::new();
    if !streams.register(&params.stream_id, cancel.clone()) {
        let error = ErrorObject::new(
            ErrorCode::INVALID_PARAMS,
            format!("stream_id 已在途：{}", params.stream_id),
        );
        return peer.respond_error(&req.id, &error).await;
    }

    // 6) 引用内容就位：插成**第一条** `system` 消息，排在全部用户消息之前。
    //    顺序是有意的——把外部材料放在对话历史前面，模型读到时已有全局视角；
    //    插在中间会让「引用」看起来像上一轮的回复。
    let mut messages = params.messages.clone();
    if let Some(injected) = injection {
        if injected.is_empty() {
            debug!(stream_id = %params.stream_id, "上下文引用渲染为空，未插入消息");
        } else {
            info!(
                stream_id = %params.stream_id,
                refs = params.context.as_ref().map_or(0, |r| r.refs.len()),
                injected_tokens = injected.tokens,
                omitted = injected.omitted.len(),
                summarized = injected.summarized.len(),
                strategy = ?params
                    .context
                    .as_ref()
                    .and_then(|r| r.compress.as_ref())
                    .map(|c| c.strategy)
                    .unwrap_or(ai_ide_protocol::dto::CompressStrategy::Truncate),
                "已注入上下文引用"
            );
            messages.insert(
                0,
                ChatMessage {
                    role: ChatRole::System,
                    content: injected.content,
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                },
            );
        }
    }

    // 7) 先回结果，再开流：`{stream_id}` 是「已受理」的回执，内容是后续事件的事。
    peer.respond(
        &req.id,
        &LlmChatStreamResult {
            stream_id: params.stream_id.clone(),
        },
    )
    .await;

    let ctx = StreamCtx {
        peer: stream_peer,
        streams,
        stream_id: params.stream_id.clone(),
        session_id: params.session_id.clone(),
        model_ref: params.model_ref.clone(),
    };
    let request = ChatRequest {
        model: params.model_ref.clone(),
        messages,
        tools: params.tools.clone(),
        tool_choice: params.tool_choice,
        params: params.params.clone().unwrap_or_default(),
    };

    info!(
        stream_id = %ctx.stream_id,
        provider_id = %ctx.model_ref.provider_id,
        model_id = %ctx.model_ref.model_id,
        "受理流式对话"
    );
    tokio::spawn(async move {
        drive_stream(ctx, provider, request, cancel).await;
    });
}

/// 参数校验：返回第一个不合格项对应的协议错误。
fn validate_stream_params(params: &LlmChatStreamParams) -> Option<ErrorObject> {
    if params.stream_id.trim().is_empty() {
        return Some(ErrorObject::new(
            ErrorCode::INVALID_PARAMS,
            "stream_id 不能为空",
        ));
    }
    if params.model_ref.model_id.trim().is_empty() {
        return Some(ErrorObject::new(
            ErrorCode::INVALID_PARAMS,
            "model_ref.model_id 不能为空",
        ));
    }
    None
}

/// 一次在途流的运行上下文：把「发事件要用的东西」打成一包，免得函数参数一排排。
struct StreamCtx {
    peer: Arc<Peer>,
    streams: Arc<StreamHub>,
    stream_id: String,
    /// 原样回落到 `llm.started`（M3 会话持久化才真正用它）。
    session_id: Option<String>,
    model_ref: ModelRef,
}

/// 读上游流 → 翻译成 `llm.*` 事件 → 统一收尾。
///
/// 读循环结束的唯一出口就是下面的 `match`：因此「恰好一条收尾事件」不需要额外标志位，
/// 也不可能出现 `finished` 与 `error` 同时发出。
async fn drive_stream(
    ctx: StreamCtx,
    provider: Arc<dyn Provider>,
    request: ChatRequest,
    cancel: CancelToken,
) {
    ctx.peer
        .notify(
            methods::event::LLM_STARTED,
            &LlmStartedEvent {
                stream_id: ctx.stream_id.clone(),
                model_ref: ctx.model_ref.clone(),
                session_id: ctx.session_id.clone(),
                started_at_ms: now_ms(),
            },
        )
        .await;

    // provider 往 sink 里推事件，本任务负责翻成协议通知。
    // 上游任务的返回值单独接住：`join` 时 sink 已关闭，`recv` 也已返回 `None`。
    let (tx, mut rx) = unbounded_channel::<StreamEvent>();
    let producer = tokio::spawn(async move { provider.chat_stream(request, tx, cancel).await });

    // 上游在正常路径上会先给 Usage 再给 Finished；这里记住结束原因，
    // 供收尾事件使用（provider 不直接发 `llm.finished`，见 `types.rs` 的约定）。
    let mut completion: Option<(FinishReason, Option<String>)> = None;
    while let Some(event) = rx.recv().await {
        if let StreamEvent::Finished { reason, message_id } = &event {
            completion = Some((*reason, message_id.clone()));
        }
        notify_event(&ctx.peer, &ctx.stream_id, event).await;
    }

    // 先摘登记再发收尾事件：登记表里留一条已结束的流，会让同名重试被误判为并发。
    ctx.streams.finish(&ctx.stream_id);

    match producer.await {
        Ok(Ok(_usage)) => {
            // 用量已随 `llm.usage` 下发，返回值这里只用来判断成败。
            // 适配器违约（正常返回却没给 Finished）时按 Unknown 兜底，别把流悬着。
            let (finish_reason, message_id) = match completion {
                Some(done) => done,
                None => {
                    warn!(
                        stream_id = %ctx.stream_id,
                        "上游正常结束但未给出结束原因，按 unknown 收尾"
                    );
                    (FinishReason::Unknown, None)
                }
            };
            ctx.peer
                .notify(
                    methods::event::LLM_FINISHED,
                    &LlmFinishedEvent {
                        stream_id: ctx.stream_id.clone(),
                        finish_reason,
                        message_id,
                    },
                )
                .await;
        }
        // 取消不发 `llm.error`：用户自己按的停止不该被当成故障弹窗。
        Ok(Err(LlmError::Cancelled)) => {
            debug!(stream_id = %ctx.stream_id, "流式对话已取消");
        }
        Ok(Err(err)) => notify_stream_error(&ctx.peer, &ctx.stream_id, &err).await,
        Err(join_err) if join_err.is_cancelled() => {
            warn!(stream_id = %ctx.stream_id, "流式任务被中止（进程正在退出）");
        }
        Err(join_err) => {
            error!(stream_id = %ctx.stream_id, error = %join_err, "流式任务异常退出");
            let err = LlmError::Unavailable {
                message: format!("模型接入任务异常退出：{join_err}"),
                status: None,
            };
            notify_stream_error(&ctx.peer, &ctx.stream_id, &err).await;
        }
    }
}

/// 把一帧运行期事件翻成协议通知。
///
/// `StreamEvent::Finished` 在这里被**吞掉**：结束语义由 `drive_stream` 统一发，
/// 免得同一个 `FinishReason` 有两条出口。
async fn notify_event(peer: &Peer, stream_id: &str, event: StreamEvent) {
    match event {
        StreamEvent::Delta { text } => {
            peer.notify(
                methods::event::LLM_DELTA,
                &LlmDeltaEvent {
                    stream_id: stream_id.to_string(),
                    text,
                },
            )
            .await;
        }
        StreamEvent::Reasoning { text } => {
            peer.notify(
                methods::event::LLM_REASONING,
                &LlmReasoningEvent {
                    stream_id: stream_id.to_string(),
                    text,
                },
            )
            .await;
        }
        StreamEvent::ToolCall {
            index,
            id,
            name,
            arguments_delta,
        } => {
            peer.notify(
                methods::event::LLM_TOOL_CALL,
                &LlmToolCallEvent {
                    stream_id: stream_id.to_string(),
                    index,
                    id,
                    name,
                    arguments_delta,
                },
            )
            .await;
        }
        StreamEvent::Usage(usage) => {
            peer.notify(
                methods::event::LLM_USAGE,
                &LlmUsageEvent {
                    stream_id: stream_id.to_string(),
                    prompt_tokens: usage.prompt_tokens,
                    completion_tokens: usage.completion_tokens,
                    cached_tokens: usage.cached_tokens,
                },
            )
            .await;
        }
        StreamEvent::Finished { .. } => {}
    }
}

/// 推一条 `llm.error`：错误码决定 UI 是「换密钥」「稍后重试」还是「查网络」。
async fn notify_stream_error(peer: &Peer, stream_id: &str, err: &LlmError) {
    let event = LlmErrorEvent {
        stream_id: stream_id.to_string(),
        code: err.code(),
        message: err.to_string(),
        upstream: err.upstream(),
        retry_after_ms: err.retry_after_ms(),
    };
    warn!(
        stream_id = %stream_id,
        code = event.code.as_i32(),
        message = %event.message,
        "流式对话异常终止"
    );
    peer.notify(methods::event::LLM_ERROR, &event).await;
}

// ---------------------------------------------------------------------------
// llm.cancel
// ---------------------------------------------------------------------------

/// 取消一条在途流。**幂等**：不存在的 `stream_id` 也回 `{ok:true}`。
///
/// 之所以不报「找不到」：取消请求常常与「流刚好自己结束」竞态，此时报错会变成
/// 一条毫无意义的红色提示。
pub async fn cancel(peer: &Peer, state: &AppState, req: &Request) {
    let params: LlmCancelParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    if state.streams().cancel(&params.stream_id) {
        info!(stream_id = %params.stream_id, "已请求取消在途流");
    } else {
        debug!(stream_id = %params.stream_id, "取消请求未命中在途流（已结束或 id 有误）");
    }
    peer.respond(&req.id, &OkResult { ok: true }).await;
}

// ---------------------------------------------------------------------------
// llm.list_models
// ---------------------------------------------------------------------------

/// 转发上游模型清单（也是连通性探测的实现基础）。
///
/// 与 `provider.test` 的分工：这里**原样回上游结果**并把失败报成协议错误，
/// `provider.test` 则把失败折成 `HealthReport`，因为「测不通」本身就是它的结果。
pub async fn list_models(peer: &Peer, state: &AppState, req: &Request) {
    let params: LlmListModelsParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    let provider = match state.llm().registry().get(&params.provider_id) {
        Some(provider) => provider,
        None => {
            let err = LlmError::ProviderNotConfigured {
                provider_id: params.provider_id.clone(),
            };
            return respond_llm_error(peer, req, &err).await;
        }
    };

    match provider.list_models().await {
        Ok(models) => {
            debug!(provider_id = %params.provider_id, count = models.len(), "已取回模型清单");
            peer.respond(&req.id, &LlmListModelsResult { models }).await;
        }
        Err(err) => respond_llm_error(peer, req, &err).await,
    }
}

// ---------------------------------------------------------------------------
// llm.count_tokens
// ---------------------------------------------------------------------------

/// token 计数。
///
/// 口径按 `model_ref.model_id` 分流（映射见 [`ai_ide_llm::tokens::encoding_for_model`]）：
/// 认识的模型族走 tiktoken 真实词表精确分词（`exact: true`，`note` 点明用的哪套词表），
/// 其余模型回落内核估算器（`exact: false`）。调用方只依赖 `exact` 与 `note` 两个字段，
/// 协议字段含义不变。
///
/// 入参有两种口径，都算进同一个结果：`messages`（对话历史）与 `texts`（额外的
/// 裸文本，典型用途是输入框里还没拼成消息的草稿、以及 @引用到的片段）。两者可以同时给，
/// 服务的是「发送前看清总量」这一个目的。
///
/// **不查注册表**：计数纯本地计算，调不通上游也要能算——输入框的 token 计数
/// 不该因为网关挂了而消失。首次精确计数会解析一次内嵌词表（百毫秒量级，进程内只做一次）。
pub async fn count_tokens(peer: &Peer, _state: &AppState, req: &Request) {
    let params: LlmCountTokensParams = match parse_params(req.params.as_ref()) {
        Ok(params) => params,
        Err(err) => return peer.respond_error(&req.id, &err).await,
    };

    // 工具定义也占 token，但 DTO 里这一项还没定（`docs/03` §2.4 只列了 messages），
    // 因此先按空表计；口径在 note 里说明，不假装比实际更全。
    let counter = TokenCounter::for_model(&params.model_ref.model_id);
    let mut tokens = counter.count_messages(&params.messages);
    for text in &params.texts {
        tokens += counter.count_text(text);
    }
    peer.respond(
        &req.id,
        &LlmCountTokensResult {
            tokens,
            exact: counter.is_exact(),
            note: Some(counter.note().to_string()),
        },
    )
    .await;
}

// ---------------------------------------------------------------------------
// 内部工具
// ---------------------------------------------------------------------------

/// 工作区 / 路径错误 → 协议错误对象。
///
/// 越界（`PATH_OUT_OF_WORKSPACE`）走这里原样透出：UI 需要能区分「路径不合法」
/// 与「工作区没打开」，两者对用户的下一步动作完全不同。
fn workspace_error(err: &WorkspaceError) -> ErrorObject {
    ErrorObject::new(err.code(), err.to_string())
}

/// 把模型接入层错误翻成协议错误对象再回（与 `provider_api` 同一套折返口径）。
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

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> StreamHub {
        StreamHub::new()
    }

    #[test]
    fn duplicate_stream_id_is_rejected() {
        let hub = guard();
        assert!(hub.register("s1", CancelToken::new()));
        assert!(
            !hub.register("s1", CancelToken::new()),
            "同 id 并发必须被拒，否则取消会打到错误的流上"
        );
        assert_eq!(hub.live_count(), 1);
    }

    #[test]
    fn finish_frees_the_id_for_reuse() {
        let hub = guard();
        assert!(hub.register("s1", CancelToken::new()));
        hub.finish("s1");
        hub.finish("s1"); // 幂等
        assert_eq!(hub.live_count(), 0);
        assert!(hub.register("s1", CancelToken::new()), "结束后 id 应可复用");
    }

    #[test]
    fn cancel_hits_live_stream_and_is_idempotent() {
        let hub = guard();
        let token = CancelToken::new();
        assert!(hub.register("s1", token.clone()));

        assert!(hub.cancel("s1"));
        assert!(token.is_cancelled(), "取消必须真的打到句柄上");
        assert!(hub.cancel("s1"), "重复取消仍回命中，不算错误");
        assert!(!hub.cancel("从未存在"), "未命中交给调用方决定语义");
    }

    #[test]
    fn cancel_all_blocks_every_live_stream() {
        let hub = guard();
        let a = CancelToken::new();
        let b = CancelToken::new();
        assert!(hub.register("a", a.clone()));
        assert!(hub.register("b", b.clone()));

        assert_eq!(hub.cancel_all(), 2);
        assert!(a.is_cancelled() && b.is_cancelled());
    }

    #[test]
    fn stream_params_reject_empty_ids() {
        let params = LlmChatStreamParams {
            stream_id: "  ".to_string(),
            session_id: None,
            model_ref: ModelRef {
                provider_id: "p1".to_string(),
                model_id: "m1".to_string(),
            },
            messages: Vec::new(),
            tools: Vec::new(),
            tool_choice: None,
            params: None,
            context: None,
        };
        let err = validate_stream_params(&params).expect("空 stream_id 必须被拒");
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);

        let mut fixed = params.clone();
        fixed.stream_id = "s1".to_string();
        fixed.model_ref.model_id = " ".to_string();
        let err = validate_stream_params(&fixed).expect("空 model_id 必须被拒");
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);

        fixed.model_ref.model_id = "deepseek-chat".to_string();
        assert!(validate_stream_params(&fixed).is_none());
    }
}
