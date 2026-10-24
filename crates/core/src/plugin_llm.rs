//! `host/llm.chat` 的授权策略、请求计划与执行（插件系统 P5 切片六）。
//!
//! 这一层与 [`crate::plugin_net`] 同构，只做免得住的几件事，且全部在**真正发起
//! 调用之前**完成：
//!
//! 1. **模型白名单**：清单 `allowedModels` 里没写 `provider/model` 的，一步都
//!    出不去——插件不能借用户的密钥去调一个「清单里没有」的模型。
//! 2. **`maxTokens` 上限**：清单里的 `maxTokens` 是插件能要到的最多输出，
//!    请求里写更大一律拒。上限只往下调不往上调，插件无法靠一次请求刷屏。
//! 3. **形状与体量**：消息数组、单条正文、消息总字节都有硬上限。
//!
//! 密钥不出宿主：本模块只拿 [`ai_ide_llm::LlmRuntime`] 里的注册表句柄，调用
//! `Provider::chat_stream`，插件侧看到的是纯文本回包，从来没有拿到 key 的入口。
//!
//! 传输实现走 [`LlmChannel`]：生产是 [`ProviderChannel`]（复用宿主已有的模型接入
//! 通道），单测换成假的——「超时」「截断」「成功」这些分支不必真连上游就能验。

use std::sync::Arc;
use std::time::Duration;

use ai_ide_llm::ChatRequest;

use crate::llm_runtime::LlmRuntime;
use ai_ide_plugin::host_api::HostApiError;
use ai_ide_protocol::dto::{ChatMessage, ChatParams, ChatRole, ModelRef};
use serde_json::{json, Value};

/// 一次 `host/llm.chat` 默认可用多少输出 token（清单没写 `maxTokens` 时）。
pub const DEFAULT_MAX_TOKENS: u64 = 512;
/// 插件能申请到的最大输出 token（清单 `maxTokens` 的上限）。
///
/// 512 是「够一次摘要」，8192 已经接近多数模型单轮输出的上限；再往上就不该
/// 由插件决定，而是走用户的 `llm.chat_stream`。
pub const MAX_TOKENS_CEILING: u64 = 8192;
/// 单次请求最多带几条消息。
pub const MAX_MESSAGES: usize = 64;
/// 单条消息正文上限（字符数，按 UTF-8 计 64 KB）。
pub const MAX_MESSAGE_CHARS: usize = 32 * 1024;
/// 全部消息正文合计上限（128 KB）。
pub const MAX_PROMPT_BYTES: usize = 128 * 1024;
/// 回包正文上限（64 KB）：超出的部分截断并置 `truncated`。
pub const MAX_REPLY_CHARS: usize = 32 * 1024;
/// 默认超时（30 s）。模型调用比 `net.fetch` 慢一个量级，取清单默认同款。
pub const DEFAULT_CHAT_TIMEOUT: Duration = Duration::from_millis(30_000);
/// 单次调用最大超时（60 s）。
pub const MAX_CHAT_TIMEOUT: Duration = Duration::from_millis(60_000);
/// 最小超时（100 ms）：再小只可能是「必然超时」，不如直接拒。
pub const MIN_CHAT_TIMEOUT: Duration = Duration::from_millis(100);

/// 某插件在 `host/llm.chat` 上的授权面：模型白名单 + 输出 token 上限。
///
/// 两份数据都来自清单，装载时灌进来（[`crate::plugin_host::FsHostApi::set_llm_policy_for`]）。
/// 缺省是「空名单 + 0 上限」= 全拒（fail closed）——没登记过策略的插件，
/// 即便账本里莫名其妙有 `llm.chat`，也调不动。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LlmPolicy {
    /// 允许的模型，形如 `openai/gpt-4o-mini`（小写、斜杠分隔）。
    pub models: Vec<String>,
    /// 单次请求允许的最大输出 token。
    pub max_tokens: u64,
}

impl LlmPolicy {
    /// 这个 `provider/model` 是否在名单里。比较按归一化口径（大小写不敏感）。
    pub fn allows(&self, provider: &str, model: &str) -> bool {
        let wanted = format!(
            "{}/{}",
            provider.to_ascii_lowercase(),
            model.to_ascii_lowercase()
        );
        self.models.iter().any(|entry| entry == &wanted)
    }
}

/// 一次 `host/llm.chat` 的完整计划：闸门产物 + 请求形状。
///
/// 闸门通过之后才建得出来，所以 [`execute`] 不必再判「这个模型该不该放行」。
#[derive(Debug, Clone)]
pub struct ChatPlan {
    pub provider: String,
    pub model: String,
    pub messages: Vec<ChatMessage>,
    /// 已按「清单上限」夹过的输出 token（调用方写小了才是它自己的值）。
    pub max_tokens: u64,
    pub temperature: Option<f64>,
    pub timeout: Duration,
}

/// 归一化模型条目：`Provider/Model` → `provider/model`。
pub fn normalize_model_entry(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

/// 把 `params` 解成 [`ChatPlan`]：先判形状（`-32602`），再判资格（`6103`）。
///
/// 顺序是刻意的，和 `net.fetch` 同一条理由：参数畸形与「模型不在白名单」是
/// 两件事，混在一起会让插件作者从错误码看不出该改参数还是该改清单。
pub fn plan_chat(params: &Value, policy: &LlmPolicy) -> Result<ChatPlan, HostApiError> {
    // ---------- 形状 ----------
    let provider = match params.get("provider") {
        Some(Value::String(raw)) if !raw.trim().is_empty() => raw.trim().to_string(),
        Some(Value::String(_)) => return Err(HostApiError::invalid_params("provider 不能是空串")),
        None | Some(Value::Null) => return Err(HostApiError::invalid_params("缺少 provider 参数")),
        Some(_) => return Err(HostApiError::invalid_params("provider 必须是字符串")),
    };
    let model = match params.get("model") {
        Some(Value::String(raw)) if !raw.trim().is_empty() => raw.trim().to_string(),
        Some(Value::String(_)) => return Err(HostApiError::invalid_params("model 不能是空串")),
        None | Some(Value::Null) => return Err(HostApiError::invalid_params("缺少 model 参数")),
        Some(_) => return Err(HostApiError::invalid_params("model 必须是字符串")),
    };
    // 斜杠是清单条目的分隔符：插件把 `provider` 写成 `a/b` 就等于自造条目，
    // 那样白名单比对会被「拼一次」蒙对。两边都不许带斜杠。
    if provider.contains('/') || model.contains('/') {
        return Err(HostApiError::invalid_params(
            "provider / model 不要带 '/'：斜杠是清单条目的分隔符",
        ));
    }

    let messages = parse_messages(params)?;

    let max_tokens = match params.get("maxTokens") {
        None | Some(Value::Null) => policy.max_tokens.min(DEFAULT_MAX_TOKENS),
        Some(value) => match value.as_u64() {
            Some(tokens) if tokens > 0 => tokens,
            Some(_) => return Err(HostApiError::invalid_params("maxTokens 必须大于 0")),
            None => return Err(HostApiError::invalid_params("maxTokens 必须是正整数")),
        },
    };

    let temperature = match params.get("temperature") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_f64() {
            Some(temp) if (0.0..=2.0).contains(&temp) => Some(temp),
            Some(_) => {
                return Err(HostApiError::invalid_params(
                    "temperature 必须在 0.0–2.0 之间",
                ))
            }
            None => return Err(HostApiError::invalid_params("temperature 必须是数字")),
        },
    };

    let timeout = match params.get("timeoutMs") {
        None | Some(Value::Null) => DEFAULT_CHAT_TIMEOUT,
        Some(value) => match value.as_u64() {
            Some(ms) if ms >= MIN_CHAT_TIMEOUT.as_millis() as u64 => {
                Duration::from_millis(ms.min(MAX_CHAT_TIMEOUT.as_millis() as u64))
            }
            Some(_) => {
                return Err(HostApiError::invalid_params(format!(
                    "timeoutMs 不得小于 {} ms",
                    MIN_CHAT_TIMEOUT.as_millis()
                )))
            }
            None => return Err(HostApiError::invalid_params("timeoutMs 必须是整数毫秒")),
        },
    };

    // ---------- 资格 ----------
    // 先白名单：`maxTokens` 越界对「根本没资格调用」的插件毫无意义，先报越界
    // 会让它以为只要把数字改小就能用。
    if policy.models.is_empty() {
        return Err(HostApiError::new(
            HostApiError::LLM_DENIED,
            format!("清单未声明 allowedModels，拒绝 {provider}/{model}"),
        ));
    }
    if !policy.allows(&provider, &model) {
        return Err(HostApiError::new(
            HostApiError::LLM_DENIED,
            format!(
                "模型 {provider}/{model} 不在清单 allowedModels 内；可调用：{}",
                policy.models.join(" / ")
            ),
        ));
    }
    if policy.max_tokens == 0 {
        return Err(HostApiError::new(
            HostApiError::LLM_DENIED,
            "清单未声明 maxTokens，拒绝 host/llm.chat",
        ));
    }
    // 越界按参数畸形报（`-32602`）：这不是「资质不够」，而是「这次请求写错了」，
    // 插件把数字改小就能过，前端不必弹授权。
    if max_tokens > policy.max_tokens {
        return Err(HostApiError::invalid_params(format!(
            "maxTokens {max_tokens} 超过清单上限 {}",
            policy.max_tokens
        )));
    }

    Ok(ChatPlan {
        provider,
        model,
        messages,
        max_tokens,
        temperature,
        timeout,
    })
}

/// 消息数组解码：角色白名单 + 单条 / 合计长度上限。
fn parse_messages(params: &Value) -> Result<Vec<ChatMessage>, HostApiError> {
    let raw = match params.get("messages") {
        Some(Value::Array(items)) => items,
        None | Some(Value::Null) => return Err(HostApiError::invalid_params("缺少 messages 参数")),
        Some(_) => return Err(HostApiError::invalid_params("messages 必须是数组")),
    };
    if raw.is_empty() {
        return Err(HostApiError::invalid_params("messages 不能为空"));
    }
    if raw.len() > MAX_MESSAGES {
        return Err(HostApiError::invalid_params(format!(
            "messages 最多 {MAX_MESSAGES} 条，收到 {} 条",
            raw.len()
        )));
    }

    let mut messages = Vec::with_capacity(raw.len());
    let mut total = 0usize;
    for (index, item) in raw.iter().enumerate() {
        if !item.is_object() {
            return Err(HostApiError::invalid_params(format!(
                "messages[{index}] 必须是对象"
            )));
        }
        let role = match item.get("role").and_then(Value::as_str) {
            Some("system") => ChatRole::System,
            Some("user") => ChatRole::User,
            Some("assistant") => ChatRole::Assistant,
            Some(other) => {
                return Err(HostApiError::invalid_params(format!(
                    "messages[{index}].role 只支持 system / user / assistant，收到 {other}"
                )))
            }
            None => {
                return Err(HostApiError::invalid_params(format!(
                    "messages[{index}].role 必须是字符串"
                )))
            }
        };
        let content = match item.get("content") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(text)) => text.clone(),
            Some(_) => {
                return Err(HostApiError::invalid_params(format!(
                    "messages[{index}].content 必须是字符串"
                )))
            }
        };
        if content.chars().count() > MAX_MESSAGE_CHARS {
            return Err(HostApiError::invalid_params(format!(
                "messages[{index}].content 超过 {MAX_MESSAGE_CHARS} 字符"
            )));
        }
        total += content.len();
        if total > MAX_PROMPT_BYTES {
            return Err(HostApiError::invalid_params(format!(
                "messages 正文合计超过 {MAX_PROMPT_BYTES} 字节"
            )));
        }
        // 附件 / 工具调用 / tool_call_id 都不透传：插件调 llm.chat 是「问一句
        // 拿一句」，引用与工具编排属于用户侧的 `llm.chat_stream`。字段留空而不是
        // 原样转发，避免插件用 `attachments` 摸到工作区外的路径。
        messages.push(ChatMessage {
            role,
            content,
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
    Ok(messages)
}

/// 一次模型回包（已截断到 [`MAX_REPLY_CHARS`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatReply {
    pub text: String,
    pub truncated: bool,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

/// 模型调用方（宿主侧）。
///
/// 同步签名，理由同 [`crate::plugin_net::NetTransport`]：插件反向请求跑在帧泵
/// 线程上，实现方自己做「异步转同步」，不能在这里 `block_on` 已有运行时。
pub trait LlmChannel: Send + Sync {
    fn chat(&self, plan: &ChatPlan) -> Result<ChatReply, HostApiError>;
}

/// 生产实现：复用 [`LlmRuntime`] 的注册表与 Provider 调用通道。
///
/// 密钥只存在于 Provider 实例里（宿主内存），本结构只做「取 Provider → 发起流式
/// 调用 → 把 delta 拼成一段文本」，插件的参数里没有任何塞 key 的口子。
pub struct ProviderChannel {
    runtime: Arc<LlmRuntime>,
}

impl ProviderChannel {
    pub fn new(runtime: Arc<LlmRuntime>) -> Self {
        Self { runtime }
    }
}

impl LlmChannel for ProviderChannel {
    fn chat(&self, plan: &ChatPlan) -> Result<ChatReply, HostApiError> {
        let provider = self
            .runtime
            .registry()
            .get(&plan.provider)
            .ok_or_else(|| HostApiError::new(HostApiError::IO, "provider 未配置或未启用"))?;

        let request = ChatRequest {
            model: ModelRef {
                provider_id: plan.provider.clone(),
                model_id: plan.model.clone(),
            },
            messages: plan.messages.clone(),
            tools: Vec::new(),
            tool_choice: None,
            params: ChatParams {
                temperature: plan.temperature,
                max_output_tokens: Some(plan.max_tokens),
                ..ChatParams::default()
            },
        };
        let timeout = plan.timeout;

        // 专用线程 + current-thread 运行时：调用点同步，且不能借用别的运行时。
        // 线程里自留 [`TIMEOUT_GRACE`] 的宽限：外层 `recv_timeout` 到点时先回
        // 超时给插件，这个线程随后自行结束，不会越积越多。
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("plugin-llm-chat".to_string())
            .spawn(move || {
                let outcome = (|| -> Result<ChatReply, HostApiError> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|err| {
                            HostApiError::new(
                                HostApiError::IO,
                                format!("模型调用运行时启动失败：{err}"),
                            )
                        })?;
                    runtime.block_on(async move {
                        let budget = timeout + TIMEOUT_GRACE;
                        match tokio::time::timeout(budget, run_stream(provider, request)).await {
                            Ok(outcome) => outcome,
                            Err(_) => Err(timeout_error(timeout)),
                        }
                    })
                })();
                let _ = tx.send(outcome);
            })
            .map_err(|err| {
                HostApiError::new(HostApiError::IO, format!("模型调用线程启动失败：{err}"))
            })?;

        // 外层按 `plan.timeout` 卡死等待：插件帧泵不能被一条挂死的调用拖住。
        match rx.recv_timeout(timeout) {
            Ok(outcome) => outcome,
            Err(_) => Err(timeout_error(timeout)),
        }
    }
}

/// 生产通道比对外承诺的超时多给的宽限（见 [`ProviderChannel::chat`]）。
const TIMEOUT_GRACE: Duration = Duration::from_millis(500);

fn timeout_error(timeout: Duration) -> HostApiError {
    HostApiError::new(
        HostApiError::IO,
        format!("模型调用超时（{} ms）", timeout.as_millis()),
    )
}

/// 把一帧流事件并进正文。只有 `Delta` 算正文：`Reasoning`（思维链）与
/// `Usage` / `ToolCall` 都不能混进给插件的回包。
fn push_delta(event: &ai_ide_llm::StreamEvent, text: &mut String, truncated: &mut bool) {
    let ai_ide_llm::StreamEvent::Delta { text: delta } = event else {
        return;
    };
    if *truncated {
        return;
    }
    let room = MAX_REPLY_CHARS.saturating_sub(text.chars().count());
    if delta.chars().count() > room {
        text.extend(delta.chars().take(room));
        *truncated = true;
    } else {
        text.push_str(delta);
    }
}

/// 驱动一次流式调用，把 `Delta` 拼成文本，最多留 [`MAX_REPLY_CHARS`]。
async fn run_stream(
    provider: Arc<dyn ai_ide_llm::Provider>,
    request: ChatRequest,
) -> Result<ChatReply, HostApiError> {
    let (sink, mut events) = tokio::sync::mpsc::unbounded_channel();
    let cancel = ai_ide_llm::CancelToken::new();
    let mut call = Box::pin(provider.chat_stream(request, sink, cancel));
    let mut text = String::new();
    let mut truncated = false;
    // 事件通道关闭后把这个分支关掉：`recv()` 返回 `None` 会**立刻再次就绪**，
    // 继续 select 它等于空转吃满一个核。关掉之后只等调用本体结束。
    let mut draining = false;

    let usage = loop {
        tokio::select! {
            biased;
            outcome = &mut call => {
                let usage = outcome.map_err(|err| {
                    HostApiError::new(HostApiError::IO, format!("模型调用失败：{err}"))
                })?;
                // 一次调用可能瞬间就返回（本地模型、缓存命中）：正文早已进了通道
                // 却一帧都没被取走。这里必须把残留事件抽干，否则回包会是一段空文
                // 本——「成功但内容为空」是最难查的一类故障。
                while let Ok(event) = events.try_recv() {
                    push_delta(&event, &mut text, &mut truncated);
                }
                break usage;
            }
            event = events.recv(), if !draining => {
                let Some(event) = event else {
                    // 发送端已关闭：不再等后续事件，只等 provider 收尾。
                    draining = true;
                    continue;
                };
                push_delta(&event, &mut text, &mut truncated);
            }
        }
    };
    Ok(ChatReply {
        text,
        truncated,
        prompt_tokens: Some(usage.prompt_tokens),
        completion_tokens: Some(usage.completion_tokens),
    })
}

/// 执行一次计划：调通道 → 截断 → 拼回包 JSON。
///
/// 截断放在这里而不是各通道里：通道只负责「拿到全文」，上限是宿主对外承诺的
/// 一部分，不能因为换了个通道就变。
pub fn execute(channel: &dyn LlmChannel, plan: &ChatPlan) -> Result<Value, HostApiError> {
    let mut reply = channel.chat(plan)?;
    if reply.text.chars().count() > MAX_REPLY_CHARS {
        reply.text = reply.text.chars().take(MAX_REPLY_CHARS).collect();
        reply.truncated = true;
    }
    Ok(json!({
        "model": format!("{}/{}", plan.provider, plan.model),
        "text": reply.text,
        "truncated": reply.truncated,
        "usage": {
            "promptTokens": reply.prompt_tokens,
            "completionTokens": reply.completion_tokens,
            "maxTokens": plan.max_tokens,
        },
    }))
}

/// 审计里的「目标」：只记 `provider/model`。
///
/// **绝不记消息正文**：审计库是长期留存，把用户/插件的提示词写进去等于二次
/// 扩散。token 数与耗时另由调用方按字段记（见 `plugin_host::HostCallAudit`）。
pub fn audit_target(provider: &str, model: &str) -> String {
    format!("{provider}/{model}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> LlmPolicy {
        LlmPolicy {
            models: vec![
                "openai/gpt-4o-mini".to_string(),
                "deepseek/deepseek-chat".to_string(),
            ],
            max_tokens: 1024,
        }
    }

    fn params(extra: Value) -> Value {
        let mut base = json!({
            "provider": "openai",
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "你好"}],
        });
        if let (Some(obj), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                obj.insert(key.clone(), value.clone());
            }
        }
        base
    }

    #[test]
    fn plan_accepts_whitelisted_model_and_defaults() {
        let plan = plan_chat(&params(json!({})), &policy()).expect("应当通过");
        assert_eq!(plan.provider, "openai");
        assert_eq!(plan.model, "gpt-4o-mini");
        assert_eq!(plan.max_tokens, DEFAULT_MAX_TOKENS);
        assert_eq!(plan.messages.len(), 1);
        assert!(plan.timeout <= MAX_CHAT_TIMEOUT);
    }

    #[test]
    fn model_outside_whitelist_is_denied_with_6103() {
        let err =
            plan_chat(&params(json!({"model": "gpt-4o"})), &policy()).expect_err("不在白名单");
        assert_eq!(err.code, HostApiError::LLM_DENIED);
        assert!(err.message.contains("allowedModels"), "{}", err.message);
    }

    #[test]
    fn empty_policy_is_denied_not_invalid_params() {
        let err = plan_chat(&params(json!({})), &LlmPolicy::default()).expect_err("空名单");
        assert_eq!(err.code, HostApiError::LLM_DENIED);
    }

    #[test]
    fn max_tokens_over_policy_is_invalid_params() {
        let err = plan_chat(&params(json!({"maxTokens": 2048})), &policy()).expect_err("越界");
        assert_eq!(err.code, HostApiError::INVALID_PARAMS);
        assert!(err.message.contains("maxTokens"), "{}", err.message);
        // 等于上限是允许的。
        assert!(plan_chat(&params(json!({"maxTokens": 1024})), &policy()).is_ok());
    }

    #[test]
    fn malformed_shapes_are_invalid_params() {
        for bad in [
            json!({}),
            json!({"provider": "", "model": "m", "messages": [{"role": "user", "content": "x"}]}),
            json!({"provider": "openai", "model": 1, "messages": []}),
            json!({"provider": "openai/x", "model": "m", "messages": [{"role": "user", "content": "x"}]}),
            json!({"provider": "openai", "model": "m", "messages": []}),
            json!({"provider": "openai", "model": "m", "messages": [{"role": "tool", "content": "x"}]}),
            json!({"provider": "openai", "model": "m", "maxTokens": 0, "messages": [{"role": "user", "content": "x"}]}),
            json!({"provider": "openai", "model": "m", "temperature": 9.0, "messages": [{"role": "user", "content": "x"}]}),
            json!({"provider": "openai", "model": "m", "timeoutMs": 1, "messages": [{"role": "user", "content": "x"}]}),
        ] {
            let err = plan_chat(&bad, &policy()).expect_err("应当拒");
            assert_eq!(err.code, HostApiError::INVALID_PARAMS, "{bad}");
        }
    }

    #[test]
    fn oversized_prompt_is_rejected() {
        let big = "x".repeat(MAX_MESSAGE_CHARS + 1);
        let err = plan_chat(
            &json!({
                "provider": "openai",
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": big}],
            }),
            &policy(),
        )
        .expect_err("单条超限");
        assert_eq!(err.code, HostApiError::INVALID_PARAMS);

        let many: Vec<Value> = (0..MAX_MESSAGES + 1)
            .map(|_| json!({"role": "user", "content": "x"}))
            .collect();
        let err = plan_chat(
            &json!({"provider": "openai", "model": "gpt-4o-mini", "messages": many}),
            &policy(),
        )
        .expect_err("条数超限");
        assert_eq!(err.code, HostApiError::INVALID_PARAMS);
    }

    #[test]
    fn timeout_is_clamped_to_ceiling() {
        let plan = plan_chat(&params(json!({"timeoutMs": 10_000_000})), &policy()).unwrap();
        assert_eq!(plan.timeout, MAX_CHAT_TIMEOUT);
    }

    /// 通道返回超长文本时统一截断，且 `truncated` 置位。
    #[test]
    fn execute_truncates_reply() {
        struct Long;
        impl LlmChannel for Long {
            fn chat(&self, _plan: &ChatPlan) -> Result<ChatReply, HostApiError> {
                Ok(ChatReply {
                    text: "a".repeat(MAX_REPLY_CHARS + 10),
                    truncated: false,
                    prompt_tokens: Some(3),
                    completion_tokens: Some(4),
                })
            }
        }
        let plan = plan_chat(&params(json!({})), &policy()).unwrap();
        let value = execute(&Long, &plan).expect("应当成功");
        assert_eq!(value["truncated"], json!(true));
        assert_eq!(
            value["text"].as_str().unwrap().chars().count(),
            MAX_REPLY_CHARS
        );
        assert_eq!(value["usage"]["promptTokens"], json!(3));
    }

    /// 审计目标只有 `provider/model`：正文一个字都不许出现。
    #[test]
    fn audit_target_never_leaks_prompt() {
        let plan = plan_chat(
            &params(json!({"messages": [{"role": "user", "content": "机密正文"}]})),
            &policy(),
        )
        .unwrap();
        let target = audit_target(&plan.provider, &plan.model);
        assert_eq!(target, "openai/gpt-4o-mini");
        assert!(!target.contains("机密"));
    }

    // -----------------------------------------------------------------------
    // 生产通道（`ProviderChannel`）：成功 / 超时。这两个分支不真的连上游——
    // 用假 Provider 直接塞进注册表，验的是「宿主怎么驱动 provider、怎么卡时间」。
    // -----------------------------------------------------------------------

    use ai_ide_llm::{Provider, ProviderSpec, StreamEvent, Usage};
    use ai_ide_protocol::dto::ProviderProtocol;
    use std::collections::BTreeMap;

    fn spec(id: &str) -> ProviderSpec {
        ProviderSpec {
            id: id.to_string(),
            name: id.to_string(),
            protocol: ProviderProtocol::OpenaiChatCompletions,
            base_url: "https://example.test/v1".to_string(),
            headers: BTreeMap::new(),
            enabled: true,
            timeout_ms: None,
        }
    }

    /// 正常回包：发两帧 delta 后返回用量。
    struct EchoProvider {
        spec: ProviderSpec,
    }

    #[async_trait::async_trait]
    impl Provider for EchoProvider {
        fn spec(&self) -> &ProviderSpec {
            &self.spec
        }

        fn has_key(&self) -> bool {
            true
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
            sink: ai_ide_llm::EventSink,
            _cancel: ai_ide_llm::CancelToken,
        ) -> Result<Usage, ai_ide_llm::LlmError> {
            let _ = sink.send(StreamEvent::Delta {
                text: "你".to_string(),
            });
            let _ = sink.send(StreamEvent::Delta {
                text: "好".to_string(),
            });
            // 非正文事件必须被忽略：`Reasoning` / `Usage` 不该混进回包正文。
            let _ = sink.send(StreamEvent::Reasoning {
                text: "思考中".to_string(),
            });
            Ok(Usage {
                prompt_tokens: 7,
                completion_tokens: 3,
                cached_tokens: None,
            })
        }

        async fn list_models(
            &self,
        ) -> Result<Vec<ai_ide_protocol::dto::ModelInfo>, ai_ide_llm::LlmError> {
            unimplemented!("本测试不走模型清单")
        }

        async fn health(&self) -> Result<ai_ide_llm::HealthReport, ai_ide_llm::LlmError> {
            unimplemented!("本测试不走连通性探测")
        }
    }

    /// 一直不返回：用来验超时是真的「到点就回」，而不是等上游。
    struct HangProvider {
        spec: ProviderSpec,
    }

    #[async_trait::async_trait]
    impl Provider for HangProvider {
        fn spec(&self) -> &ProviderSpec {
            &self.spec
        }

        fn has_key(&self) -> bool {
            true
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
            _sink: ai_ide_llm::EventSink,
            _cancel: ai_ide_llm::CancelToken,
        ) -> Result<Usage, ai_ide_llm::LlmError> {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(Usage::default())
        }

        async fn list_models(
            &self,
        ) -> Result<Vec<ai_ide_protocol::dto::ModelInfo>, ai_ide_llm::LlmError> {
            unimplemented!("本测试不走模型清单")
        }

        async fn health(&self) -> Result<ai_ide_llm::HealthReport, ai_ide_llm::LlmError> {
            unimplemented!("本测试不走连通性探测")
        }
    }

    fn channel_with(provider: Arc<dyn Provider>) -> ProviderChannel {
        let path = std::env::temp_dir().join(format!(
            "aiide-plugin-llm-{}-{}.json",
            std::process::id(),
            provider.spec().id
        ));
        let runtime = LlmRuntime::new(path);
        runtime.registry().upsert(provider);
        ProviderChannel::new(Arc::new(runtime))
    }

    /// 成功路径：delta 拼成正文、用量回填、`Reasoning` 不进正文，且回包形状固定。
    #[test]
    fn provider_channel_assembles_deltas_and_usage() {
        let channel = channel_with(Arc::new(EchoProvider {
            spec: spec("openai"),
        }));
        let plan = plan_chat(&params(json!({})), &policy()).expect("应当通过");
        let value = execute(&channel, &plan).expect("应当成功");
        assert_eq!(value["text"], json!("你好"));
        assert_eq!(value["truncated"], json!(false));
        assert_eq!(value["model"], json!("openai/gpt-4o-mini"));
        assert_eq!(value["usage"]["promptTokens"], json!(7));
        assert_eq!(value["usage"]["completionTokens"], json!(3));
        assert_eq!(value["usage"]["maxTokens"], json!(DEFAULT_MAX_TOKENS));
    }

    /// 超时：上游挂死时按 `plan.timeout` 返回，插件帧泵不被拖住。
    #[test]
    fn provider_channel_times_out() {
        let channel = channel_with(Arc::new(HangProvider {
            spec: spec("openai"),
        }));
        let plan = plan_chat(&params(json!({ "timeoutMs": 100 })), &policy()).expect("应当通过");
        let started = std::time::Instant::now();
        let err = execute(&channel, &plan).expect_err("必须超时");
        let waited = started.elapsed();
        assert_eq!(err.code, HostApiError::IO);
        assert!(err.message.contains("超时"), "{}", err.message);
        // 只断言「明显早于上游的 30 s」，不卡具体毫秒：CI 上线程调度会抖。
        assert!(waited < Duration::from_secs(5), "超时没有生效：{waited:?}");
    }

    /// provider 没配 / 没启用：报执行失败，不回 6103——这不是「被拒」，是宿主侧缺配置。
    #[test]
    fn provider_channel_reports_missing_provider_as_io_error() {
        let channel = channel_with(Arc::new(EchoProvider {
            spec: spec("deepseek"),
        }));
        let plan = plan_chat(&params(json!({})), &policy()).expect("应当通过");
        let err = execute(&channel, &plan).expect_err("provider 不在注册表");
        assert_eq!(err.code, HostApiError::IO);
        assert!(err.message.contains("provider"), "{}", err.message);
    }
}
