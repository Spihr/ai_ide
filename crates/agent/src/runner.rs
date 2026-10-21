//! Agent 循环调度：多轮「模型 → 工具 → 模型」直到收敛。
//!
//! ## 形状
//!
//! ```text
//! ┌─ 检查上限 / 取消 ────────────────────────────┐
//! │                                              │
//! │  模型（TurnRequest）                          │
//! │      ↓ 无 tool_calls → Completed，收工        │
//! │      ↓ 有 tool_calls                          │
//! │  逐个派发（PermissionGate → dispatch）        │
//! │      ↓ 每条调用都回一条 role=tool 消息        │
//! │  回到开头                                     │
//! └──────────────────────────────────────────────┘
//! ```
//!
//! ## 三条不变量
//!
//! 1. **配对完整**：助手消息里的每个 `tool_calls[i].id` 都有且只有一条结果消息回应，
//!    包括「撞上步数上限没来得及执行」的那些（补一条「未执行」）。
//! 2. **上限兜底**：轮数（模型调用次数）与步数（工具执行次数）各自独立设限。工具执行
//!    失败**不**计入步数——否则模型陷入「参数一直写错」的循环时，会被无效重试耗光配额。
//! 3. **致命即停**：路径越界 / 敏感路径 / 模型故障直接向上抛，不做任何容错（见
//!    [`AgentError::is_fatal`]）。安全边界不是可以「再试一次」的东西。
//!
//! ## 权限闸门（M4 第三切片）
//!
//! 挂上 [`PermissionGate`]（[`AgentLoop::with_gate`]）后，每次派发前先过闸门：
//! 策略放行照常执行；策略硬拒（越界 `1002` / 敏感 `1003` / 高危命令 `3001`）与上面
//! 那条「致命即停」同口径，直接终止循环；需要审批却被拒或超时**不算**致命——只把
//! 拒绝原因折成一条 `role = tool` 的错误消息回灌给模型。工具根本没执行，磁盘不会有
//! 任何改动。
//!
//! 循环把「消息流」原样带出来（[`RunOutcome::messages`]），所以上层要落历史、要出 UI
//! 卡片、要做 diff 预览，都能直接拿到全量轨迹，不必再反向解析。

use crate::error::AgentError;
use crate::message;
use crate::permission::{PermissionGate, Ruling, ToolCall};
use crate::registry::{ToolContext, ToolRegistry};
use ai_ide_protocol::dto::{ChatMessage, ChatRole, ChatToolCall, ToolSpec};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// 默认最大轮数（模型调用次数）。
pub const DEFAULT_MAX_ROUNDS: u32 = 24;

/// 默认最大步数（工具执行次数）。
pub const DEFAULT_MAX_STEPS: u32 = 64;

/// 循环上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunLimits {
    /// 模型调用次数上限。
    pub max_rounds: u32,
    /// 工具执行次数上限。
    pub max_steps: u32,
}

impl RunLimits {
    /// 构造上限；`0` 会被抬到 `1`，避免「一次都不跑」的退化配置。
    pub fn new(max_rounds: u32, max_steps: u32) -> Self {
        Self {
            max_rounds: max_rounds.max(1),
            max_steps: max_steps.max(1),
        }
    }
}

impl Default for RunLimits {
    fn default() -> Self {
        Self {
            max_rounds: DEFAULT_MAX_ROUNDS,
            max_steps: DEFAULT_MAX_STEPS,
        }
    }
}

/// 循环为什么停下。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// 模型不再请求工具，正常收工。
    Completed,
    /// 撞上轮数上限。
    MaxRounds {
        /// 实际用掉的轮数。
        rounds: u32,
    },
    /// 撞上步数上限。
    MaxSteps {
        /// 实际用掉的步数。
        steps: u32,
    },
    /// 被取消。
    Cancelled {
        /// 取消前完成的轮数。
        after_rounds: u32,
    },
}

/// 一轮模型调用的输入。
#[derive(Debug, Clone)]
pub struct TurnRequest {
    /// 当前全量消息（含系统提示、历史与本次新增的工具结果）。
    pub messages: Vec<ChatMessage>,
    /// 当前可用工具声明。
    pub tools: Vec<ToolSpec>,
}

/// 一轮模型调用的输出。
#[derive(Debug, Clone)]
pub struct TurnReply {
    /// 助手正文；可为空串（典型的「直接调工具、不说话」）。
    pub content: String,
    /// 请求执行的工具调用；为空即表示收工。
    pub tool_calls: Vec<ChatToolCall>,
}

impl TurnReply {
    /// 只说了一句话，不调工具。
    pub fn text_only(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            tool_calls: Vec::new(),
        }
    }

    /// 带工具调用。
    pub fn with_tool_calls(content: impl Into<String>, tool_calls: Vec<ChatToolCall>) -> Self {
        Self {
            content: content.into(),
            tool_calls,
        }
    }
}

/// 模型侧能力：给一轮消息，回一轮回复。
///
/// 抽成 trait 而不是直接依赖 `ai-ide-llm`，是为了让循环能被脚本化模型完全驱动——
/// 单元测试因此不需要网络、也不需要真模型。
#[async_trait]
pub trait ModelTurn: Send + Sync {
    /// 跑一轮。
    async fn complete(&self, request: TurnRequest) -> Result<TurnReply, AgentError>;
}

/// 循环结果。
#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// 输入消息 + 本轮新增的全部消息（助手与工具结果交错）。
    pub messages: Vec<ChatMessage>,
    /// 停止原因。
    pub stop_reason: StopReason,
    /// 实际轮数。
    pub rounds: u32,
    /// 实际步数（真正执行了的工具调用数）。
    pub steps: u32,
    /// 本轮新增的消息条数（不含输入）。
    pub appended: usize,
    /// 回灌给模型的失败摘要；不算致命错误，但值得暴露给上层做埋点。
    pub assistant_errors: Vec<String>,
}

impl RunOutcome {
    /// 是否正常收工（而非撞上限或被取消）。
    pub fn is_complete(&self) -> bool {
        matches!(self.stop_reason, StopReason::Completed)
    }

    /// 本轮新增的工具结果消息。
    pub fn tool_messages(&self) -> impl Iterator<Item = &ChatMessage> {
        self.messages
            .iter()
            .filter(|item| item.role == ChatRole::Tool)
    }
}

/// 循环驱动器。
pub struct AgentLoop<'a> {
    model: &'a dyn ModelTurn,
    registry: &'a ToolRegistry,
    gate: Option<&'a PermissionGate>,
    limits: RunLimits,
    progress: Option<&'a AtomicU32>,
}

impl<'a> AgentLoop<'a> {
    /// 用默认上限构造；不带权限闸门（默认策略不介入，行为与接入前完全一致）。
    pub fn new(model: &'a dyn ModelTurn, registry: &'a ToolRegistry) -> Self {
        Self {
            model,
            registry,
            gate: None,
            limits: RunLimits::default(),
            progress: None,
        }
    }

    /// 挂上权限闸门：每次派发前先过它（M4 第三切片）。
    ///
    /// 闸门按引用传入：`allow_always` 的进程内记忆住在闸门里，同一个闸门跨多次
    /// 循环复用，才能真正做到「同工具同范围不再问」。
    #[must_use]
    pub fn with_gate(mut self, gate: &'a PermissionGate) -> Self {
        self.gate = Some(gate);
        self
    }

    /// 换一套上限。
    #[must_use]
    pub fn with_limits(mut self, limits: RunLimits) -> Self {
        self.limits = limits;
        self
    }

    /// 挂一个步数计数器：每执行完一步就把**当前累计步数**写进去。
    ///
    /// 存在的理由是 `agent.stop`——`RunOutcome` 要等循环收工才有，而「停止」是循环
    /// 还在跑的时候发来的，需要当场回答「走到哪了」。计数器可选：不挂就完全不写，
    /// 行为与接入前一致。
    #[must_use]
    pub fn with_progress(mut self, counter: &'a AtomicU32) -> Self {
        self.progress = Some(counter);
        self
    }

    /// 当前上限。
    pub fn limits(&self) -> RunLimits {
        self.limits
    }

    /// 跑一轮完整循环。
    pub async fn run(
        &self,
        ctx: &ToolContext<'_>,
        messages: Vec<ChatMessage>,
    ) -> Result<RunOutcome, AgentError> {
        self.run_with_cancel(ctx, messages, None).await
    }

    /// 同上，但每轮开始前检查取消标志。
    ///
    /// UI 的「停止」按钮就是往这个标志里写 `true`：不打断正在跑的那一轮工具，
    /// 但在下一轮开始前干净地收手，已经产生的工具结果不会丢。
    pub async fn run_cancellable(
        &self,
        ctx: &ToolContext<'_>,
        messages: Vec<ChatMessage>,
        cancel: &AtomicBool,
    ) -> Result<RunOutcome, AgentError> {
        self.run_with_cancel(ctx, messages, Some(cancel)).await
    }

    async fn run_with_cancel(
        &self,
        ctx: &ToolContext<'_>,
        messages: Vec<ChatMessage>,
        cancel: Option<&AtomicBool>,
    ) -> Result<RunOutcome, AgentError> {
        let mut state = State {
            messages,
            rounds: 0,
            steps: 0,
            appended: 0,
            assistant_errors: Vec::new(),
        };
        let specs = self.registry.specs();
        let available = self.registry.names().join(", ");

        loop {
            if state.steps >= self.limits.max_steps {
                let steps = state.steps;
                return Ok(state.finish(StopReason::MaxSteps { steps }));
            }
            if state.rounds >= self.limits.max_rounds {
                let rounds = state.rounds;
                return Ok(state.finish(StopReason::MaxRounds { rounds }));
            }
            if is_cancelled(cancel) {
                let rounds = state.rounds;
                return Ok(state.finish(StopReason::Cancelled {
                    after_rounds: rounds,
                }));
            }

            let reply = self
                .model
                .complete(TurnRequest {
                    messages: state.messages.clone(),
                    tools: specs.clone(),
                })
                .await?;
            state.rounds += 1;

            // 模型跑完刚好被点了「停止」：到此打住，不再执行这一轮的调用。
            if is_cancelled(cancel) {
                let rounds = state.rounds;
                return Ok(state.finish(StopReason::Cancelled {
                    after_rounds: rounds,
                }));
            }

            state
                .messages
                .push(message::assistant_message(reply.content, &reply.tool_calls));
            state.appended += 1;

            if reply.tool_calls.is_empty() {
                return Ok(state.finish(StopReason::Completed));
            }

            let mut results: Vec<ChatMessage> = Vec::with_capacity(reply.tool_calls.len());
            let mut handled = 0usize;
            for call in &reply.tool_calls {
                if state.steps >= self.limits.max_steps {
                    break;
                }
                handled += 1;

                if self.registry.get(&call.name).is_none() {
                    // 未注册的工具不算一步：它不是「模型用错了参数」，而是「根本不该被调用」。
                    let reason = format!("工具 `{}` 未注册；本切片可用：{available}", call.name);
                    state.assistant_errors.push(reason.clone());
                    results.push(message::tool_error_message(&call.id, &call.name, &reason));
                    continue;
                }

                // 权限闸门：调用前先判一次。策略放行照常往下走；策略硬拒（越界 1002 /
                // 敏感 1003 / 高危命令 3001）按既有 fatal 口径终止循环；需要审批却被拒
                // 或超时则只回一条工具错误——工具没执行，磁盘不会有任何改动，循环继续。
                if let Some(gate) = self.gate {
                    if let Some(args) = gate_arguments(&call.arguments) {
                        let probe = gate_call(&call.name, &args);
                        match gate.ruling(ctx, &probe) {
                            Ruling::Allow => {}
                            Ruling::Deny(err) => {
                                for pending in &reply.tool_calls[handled..] {
                                    results.push(unexecuted(pending));
                                }
                                state.messages.append(&mut results);
                                return Err(err);
                            }
                            Ruling::Ask(_) => {
                                // `ruling` 是纯函数，`authorize` 会再判一次并走审批，
                                // 所以这里只认它的答复，不重复解释判定结果。
                                //
                                // 审批被拒 / 超时回的也是 `3001`，但它不是策略硬拒：
                                // 这里**不**按 `is_fatal` 分流，一律折成工具错误回灌，
                                // 让模型换个办法——反正工具根本没执行，磁盘没动过。
                                if let Err(err) = gate.authorize(ctx, &probe).await {
                                    let reason = err.to_string();
                                    state
                                        .assistant_errors
                                        .push(format!("{}: {reason}", call.name));
                                    results.push(message::tool_error_message(
                                        &call.id, &call.name, &reason,
                                    ));
                                    continue;
                                }
                            }
                        }
                    }
                }

                state.steps += 1;
                // 先记账再执行：这一步「已经发生」了，即便工具自己报错也不退回。
                if let Some(counter) = self.progress {
                    counter.store(state.steps, Ordering::SeqCst);
                }
                match self.registry.dispatch(ctx, &call.name, &call.arguments) {
                    Ok(outcome) => results.push(outcome.into_message(&call.id)),
                    Err(err) if err.is_fatal() => {
                        // 致命错误：先把没处理的调用补齐（保持配对完整，便于事后审计），
                        // 再把错误原样抛出。
                        for pending in &reply.tool_calls[handled..] {
                            results.push(unexecuted(pending));
                        }
                        state.messages.append(&mut results);
                        return Err(err);
                    }
                    Err(err) => {
                        let reason = err.to_string();
                        state
                            .assistant_errors
                            .push(format!("{}: {reason}", call.name));
                        results.push(message::tool_error_message(&call.id, &call.name, &reason));
                    }
                }
            }

            // 步数上限可能正好卡在一串调用的中间：剩下的调用也必须各回一条消息，
            // 否则下一轮的上游会因为「助手消息里的调用没人应答」直接报错。
            for pending in &reply.tool_calls[handled..] {
                results.push(unexecuted(pending));
            }

            state.appended += results.len();
            state.messages.extend(results);
        }
    }
}

/// 循环的累积状态，收工时一次性折成 [`RunOutcome`]。
struct State {
    messages: Vec<ChatMessage>,
    rounds: u32,
    steps: u32,
    appended: usize,
    assistant_errors: Vec<String>,
}

impl State {
    fn finish(self, stop_reason: StopReason) -> RunOutcome {
        RunOutcome {
            messages: self.messages,
            stop_reason,
            rounds: self.rounds,
            steps: self.steps,
            appended: self.appended,
            assistant_errors: self.assistant_errors,
        }
    }
}

fn unexecuted(call: &ChatToolCall) -> ChatMessage {
    message::tool_error_message(&call.id, &call.name, "本轮已达到步数上限，该调用未执行")
}

/// 为闸门解析一次参数。
///
/// 返回 `None` 表示「参数压根不是合法 JSON 对象」：这种调用连工具都进不去，
/// 由 `dispatch` 回一条可恢复的 `BadArgs` 就够了，没必要为它弹一次审批。
fn gate_arguments(arguments: &str) -> Option<Value> {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return Some(Value::Object(serde_json::Map::new()));
    }
    serde_json::from_str::<Value>(trimmed)
        .ok()
        .filter(Value::is_object)
}

/// 从「工具名 + 参数」抽出闸门判定要的三样：工具名、目标路径、命令内容。
///
/// 纯读取参数、不碰磁盘：路径是否越界、命令是否高危，全交给闸门判。
fn gate_call<'a>(name: &'a str, args: &'a Value) -> ToolCall<'a> {
    let mut probe = ToolCall::new(name);
    if let Some(path) = args.get("path").and_then(Value::as_str) {
        probe = probe.path(path);
    }
    if let Some(program) = args.get("cmd").and_then(Value::as_str) {
        let argv: Vec<String> = args
            .get("args")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        probe = probe.command(program, &argv);
    }
    probe
}

fn is_cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel
        .map(|flag| flag.load(Ordering::Relaxed))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ToolRegistry;
    use crate::testing::Sandbox;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// 脚本化模型：按顺序吐出预设回复，并记下每次收到的请求。
    struct Scripted {
        turns: Mutex<VecDeque<Result<TurnReply, AgentError>>>,
        seen: Mutex<Vec<TurnRequest>>,
    }

    impl Scripted {
        fn new(turns: Vec<Result<TurnReply, AgentError>>) -> Self {
            Self {
                turns: Mutex::new(turns.into()),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> usize {
            self.seen.lock().expect("seen 锁").len()
        }

        fn last_request(&self) -> TurnRequest {
            self.seen
                .lock()
                .expect("seen 锁")
                .last()
                .expect("至少应有一轮请求")
                .clone()
        }
    }

    #[async_trait]
    impl ModelTurn for Scripted {
        async fn complete(&self, request: TurnRequest) -> Result<TurnReply, AgentError> {
            let reply = {
                self.seen.lock().expect("seen 锁").push(request);
                let mut turns = self.turns.lock().expect("turns 锁");
                turns
                    .pop_front()
                    .unwrap_or_else(|| Ok(TurnReply::text_only("（脚本已耗尽）")))
            };
            reply
        }
    }

    fn call(id: &str, name: &str, args: &str) -> ChatToolCall {
        ChatToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args.into(),
        }
    }

    fn tool_call_ids(messages: &[ChatMessage]) -> Vec<String> {
        messages
            .iter()
            .flat_map(|item| item.tool_calls.iter().map(|call| call.id.clone()))
            .collect()
    }

    fn tool_result_ids(messages: &[ChatMessage]) -> Vec<String> {
        messages
            .iter()
            .filter(|item| item.role == ChatRole::Tool)
            .filter_map(|item| item.tool_call_id.clone())
            .collect()
    }

    #[tokio::test]
    async fn finishes_when_the_model_stops_asking_for_tools() {
        let sandbox = Sandbox::new("loop-simple");
        let model = Scripted::new(vec![Ok(TurnReply::text_only("好了"))]);
        let registry = ToolRegistry::core();

        let outcome = AgentLoop::new(&model, &registry)
            .run(&sandbox.ctx(), vec![message::user_message("你好")])
            .await
            .expect("循环应成功");

        assert_eq!(outcome.stop_reason, StopReason::Completed);
        assert!(outcome.is_complete());
        assert_eq!(outcome.rounds, 1);
        assert_eq!(outcome.steps, 0);
        assert_eq!(outcome.appended, 1);
        assert_eq!(outcome.messages.len(), 2);
        assert_eq!(outcome.messages[1].content, "好了");
        assert!(outcome.assistant_errors.is_empty());
    }

    #[tokio::test]
    async fn executes_tools_then_finishes() {
        let sandbox = Sandbox::new("loop-tools");
        sandbox.write("a.txt", "hello\n");

        let model = Scripted::new(vec![
            Ok(TurnReply::with_tool_calls(
                "先读一下",
                vec![call("c1", "read", r#"{"path":"a.txt"}"#)],
            )),
            Ok(TurnReply::text_only("读到了")),
        ]);
        let registry = ToolRegistry::core();

        let outcome = AgentLoop::new(&model, &registry)
            .run(&sandbox.ctx(), vec![message::user_message("看看 a.txt")])
            .await
            .unwrap();

        assert_eq!(outcome.stop_reason, StopReason::Completed);
        assert_eq!(outcome.rounds, 2);
        assert_eq!(outcome.steps, 1);
        assert_eq!(outcome.appended, 3, "两条助手消息 + 一条工具结果");
        assert_eq!(tool_call_ids(&outcome.messages), vec!["c1"]);
        assert_eq!(tool_result_ids(&outcome.messages), vec!["c1"]);

        let results: Vec<&ChatMessage> = outcome.tool_messages().collect();
        assert!(results[0].content.contains("hello"));
    }

    #[tokio::test]
    async fn request_carries_specs_and_the_growing_history() {
        let sandbox = Sandbox::new("loop-history");
        sandbox.write("a.txt", "hello\n");

        let model = Scripted::new(vec![
            Ok(TurnReply::with_tool_calls(
                "看看",
                vec![call("c1", "read", r#"{"path":"a.txt"}"#)],
            )),
            Ok(TurnReply::text_only("好")),
        ]);
        let registry = ToolRegistry::core();

        AgentLoop::new(&model, &registry)
            .run(&sandbox.ctx(), vec![message::user_message("看看 a.txt")])
            .await
            .unwrap();

        let request = model.last_request();
        assert_eq!(request.tools.len(), 8, "每轮都要把工具声明带上");
        assert_eq!(request.messages.len(), 3);
        assert_eq!(request.messages[0].role, ChatRole::User);
        assert_eq!(request.messages[1].role, ChatRole::Assistant);
        assert_eq!(request.messages[2].role, ChatRole::Tool);
        assert_eq!(request.messages[2].tool_call_id.as_deref(), Some("c1"));
    }

    #[tokio::test]
    async fn stops_at_max_rounds() {
        let sandbox = Sandbox::new("loop-maxrounds");
        sandbox.write("a.txt", "hello\n");

        let model = Scripted::new(vec![
            Ok(TurnReply::with_tool_calls(
                "",
                vec![call("c1", "read", r#"{"path":"a.txt"}"#)],
            )),
            Ok(TurnReply::text_only("不该跑到这里")),
        ]);
        let registry = ToolRegistry::core();

        let outcome = AgentLoop::new(&model, &registry)
            .with_limits(RunLimits::new(1, 10))
            .run(&sandbox.ctx(), vec![message::user_message("go")])
            .await
            .unwrap();

        assert_eq!(outcome.stop_reason, StopReason::MaxRounds { rounds: 1 });
        assert!(!outcome.is_complete());
        assert_eq!(model.calls(), 1, "撞到轮数上限后不该再喊模型");
    }

    #[tokio::test]
    async fn stops_at_max_steps_and_still_answers_every_call() {
        let sandbox = Sandbox::new("loop-maxsteps");
        sandbox.write("a.txt", "hello\n");

        let model = Scripted::new(vec![Ok(TurnReply::with_tool_calls(
            "",
            vec![
                call("c1", "read", r#"{"path":"a.txt"}"#),
                call("c2", "read", r#"{"path":"a.txt"}"#),
                call("c3", "read", r#"{"path":"a.txt"}"#),
            ],
        ))]);
        let registry = ToolRegistry::core();

        let outcome = AgentLoop::new(&model, &registry)
            .with_limits(RunLimits::new(10, 2))
            .run(&sandbox.ctx(), vec![message::user_message("go")])
            .await
            .unwrap();

        assert_eq!(outcome.stop_reason, StopReason::MaxSteps { steps: 2 });
        assert_eq!(outcome.steps, 2);

        let asked = tool_call_ids(&outcome.messages);
        let answered = tool_result_ids(&outcome.messages);
        assert_eq!(asked, vec!["c1", "c2", "c3"]);
        assert_eq!(
            answered, asked,
            "每个调用都必须有结果消息，包括没来得及执行的 c3"
        );
        let last = outcome.tool_messages().last().expect("应有工具结果");
        assert!(last.content.contains("未执行"));
    }

    #[tokio::test]
    async fn unknown_tool_becomes_a_recoverable_tool_error() {
        let sandbox = Sandbox::new("loop-unknown");
        let model = Scripted::new(vec![
            Ok(TurnReply::with_tool_calls(
                "",
                vec![call("c1", "web_search", "{}")],
            )),
            Ok(TurnReply::text_only("换个工具")),
        ]);
        let registry = ToolRegistry::core();

        let outcome = AgentLoop::new(&model, &registry)
            .run(&sandbox.ctx(), vec![message::user_message("go")])
            .await
            .unwrap();

        assert!(outcome.is_complete());
        assert_eq!(outcome.steps, 0, "未注册的工具不该计入步数");
        assert_eq!(outcome.assistant_errors.len(), 1);
        assert!(outcome.assistant_errors[0].contains("未注册"));

        let result = outcome.tool_messages().next().expect("应有工具结果");
        assert!(result.content.contains("<tool_error"));
        assert!(result.content.contains("未注册"));
    }

    #[tokio::test]
    async fn failing_tool_is_fed_back_and_the_loop_continues() {
        let sandbox = Sandbox::new("loop-feedback");
        let model = Scripted::new(vec![
            Ok(TurnReply::with_tool_calls(
                "",
                vec![call("c1", "read", r#"{"path":"nope.txt"}"#)],
            )),
            Ok(TurnReply::text_only("那就别读了")),
        ]);
        let registry = ToolRegistry::core();

        let outcome = AgentLoop::new(&model, &registry)
            .run(&sandbox.ctx(), vec![message::user_message("go")])
            .await
            .unwrap();

        assert!(outcome.is_complete());
        assert_eq!(outcome.rounds, 2);
        assert_eq!(outcome.steps, 1, "失败的工具也真的跑过一步");
        assert_eq!(outcome.assistant_errors.len(), 1);

        let result = outcome.tool_messages().next().unwrap();
        assert!(result.content.contains("<tool_error"));
        assert!(result.content.contains("文件不存在"));
    }

    #[tokio::test]
    async fn fatal_path_error_aborts_the_loop() {
        let sandbox = Sandbox::new("loop-fatal");
        let model = Scripted::new(vec![Ok(TurnReply::with_tool_calls(
            "",
            vec![call(
                "c1",
                "write",
                r#"{"path":".env","content":"SECRET=1"}"#,
            )],
        ))]);
        let registry = ToolRegistry::core();

        let err = AgentLoop::new(&model, &registry)
            .run(&sandbox.ctx(), vec![message::user_message("go")])
            .await
            .expect_err("敏感路径必须终止循环");

        assert_eq!(err.code(), ai_ide_protocol::ErrorCode::PATH_DENIED);
        assert_eq!(model.calls(), 1, "致命错误后不该再喊模型");
        assert!(!sandbox.exists(".env"));
    }

    #[tokio::test]
    async fn out_of_workspace_read_aborts_the_loop() {
        let sandbox = Sandbox::new("loop-escape");
        let model = Scripted::new(vec![Ok(TurnReply::with_tool_calls(
            "",
            vec![call("c1", "read", r#"{"path":"../../../etc/hosts"}"#)],
        ))]);
        let registry = ToolRegistry::core();

        let err = AgentLoop::new(&model, &registry)
            .run(&sandbox.ctx(), vec![message::user_message("go")])
            .await
            .expect_err("越界路径必须终止循环");

        assert_eq!(
            err.code(),
            ai_ide_protocol::ErrorCode::PATH_OUT_OF_WORKSPACE
        );
    }

    #[tokio::test]
    async fn cancel_flag_stops_before_the_first_round() {
        let sandbox = Sandbox::new("loop-cancel");
        let model = Scripted::new(vec![Ok(TurnReply::text_only("不该跑到这里"))]);
        let registry = ToolRegistry::core();
        let cancel = AtomicBool::new(true);

        let outcome = AgentLoop::new(&model, &registry)
            .run_cancellable(&sandbox.ctx(), vec![message::user_message("go")], &cancel)
            .await
            .unwrap();

        assert_eq!(
            outcome.stop_reason,
            StopReason::Cancelled { after_rounds: 0 }
        );
        assert_eq!(outcome.rounds, 0);
        assert_eq!(model.calls(), 0);
        assert_eq!(outcome.messages.len(), 1, "只有原始输入，没添乱");
    }

    #[tokio::test]
    async fn model_failure_propagates() {
        let sandbox = Sandbox::new("loop-modelfail");
        let model = Scripted::new(vec![Err(AgentError::Model("上游 500".into()))]);
        let registry = ToolRegistry::core();

        let err = AgentLoop::new(&model, &registry)
            .run(&sandbox.ctx(), vec![message::user_message("go")])
            .await
            .expect_err("模型故障应向上抛");
        assert_eq!(err.code(), ai_ide_protocol::ErrorCode::PROVIDER_UNAVAILABLE);
    }

    #[tokio::test]
    async fn write_then_read_round_trip() {
        // 一次真实一点的闭环：新建 → 写 → 读回确认。
        let sandbox = Sandbox::new("loop-roundtrip");
        let model = Scripted::new(vec![
            Ok(TurnReply::with_tool_calls(
                "",
                vec![call("c1", "create", r#"{"path":"src/a.ts"}"#)],
            )),
            Ok(TurnReply::with_tool_calls(
                "",
                vec![call(
                    "c2",
                    "write",
                    r#"{"path":"src/a.ts","content":"export {};\n"}"#,
                )],
            )),
            Ok(TurnReply::with_tool_calls(
                "",
                vec![call(
                    "c3",
                    "edit",
                    r#"{"path":"src/a.ts","old_text":"export {};","new_text":"export const a = 1;"}"#,
                )],
            )),
            Ok(TurnReply::with_tool_calls(
                "",
                vec![call("c4", "read", r#"{"path":"src/a.ts"}"#)],
            )),
            Ok(TurnReply::text_only("改好了")),
        ]);
        let registry = ToolRegistry::core();

        let outcome = AgentLoop::new(&model, &registry)
            .run(
                &sandbox.ctx(),
                vec![message::user_message("建个文件并改一下")],
            )
            .await
            .unwrap();

        assert!(outcome.is_complete());
        assert_eq!(outcome.rounds, 5);
        assert_eq!(outcome.steps, 4);
        assert!(outcome.assistant_errors.is_empty());
        assert_eq!(sandbox.read("src/a.ts"), "export const a = 1;\n");

        let ids = tool_result_ids(&outcome.messages);
        assert_eq!(ids, vec!["c1", "c2", "c3", "c4"]);
    }

    #[tokio::test]
    async fn progress_counter_tracks_executed_steps() {
        let sandbox = Sandbox::new("loop-progress");
        sandbox.write("a.txt", "hello\n");

        let model = Scripted::new(vec![
            Ok(TurnReply::with_tool_calls(
                "",
                vec![
                    call("c1", "read", r#"{"path":"a.txt"}"#),
                    call("c2", "read", r#"{"path":"a.txt"}"#),
                ],
            )),
            Ok(TurnReply::text_only("done")),
        ]);
        let registry = ToolRegistry::core();
        let progress = AtomicU32::new(0);

        let outcome = AgentLoop::new(&model, &registry)
            .with_progress(&progress)
            .run(
                &sandbox.ctx(),
                vec![message::user_message("read the file twice")],
            )
            .await
            .unwrap();

        assert!(outcome.is_complete());
        assert_eq!(outcome.steps, 2);
        assert_eq!(
            progress.load(Ordering::SeqCst),
            2,
            "计数器要能在外层读到「走到第几步」，agent.stop 靠它回答 steps_done"
        );
    }

    #[tokio::test]
    async fn progress_counter_absent_keeps_behaviour() {
        // 不挂计数器时不写任何东西，且循环结果与挂了计数器时一致（可选性验证）。
        let sandbox = Sandbox::new("loop-progress-off");
        sandbox.write("a.txt", "hello\n");

        let model = Scripted::new(vec![
            Ok(TurnReply::with_tool_calls(
                "",
                vec![call("c1", "read", r#"{"path":"a.txt"}"#)],
            )),
            Ok(TurnReply::text_only("done")),
        ]);
        let registry = ToolRegistry::core();

        let outcome = AgentLoop::new(&model, &registry)
            .run(&sandbox.ctx(), vec![message::user_message("read")])
            .await
            .unwrap();

        assert_eq!(outcome.steps, 1);
    }
}
