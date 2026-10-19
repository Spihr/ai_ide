//! Provider 抽象。
//!
//! 「Provider」= 一个可对话的上游（厂商或本地服务）。M2 只落地 Chat Completions
//! 一种协议，但抽象按 docs/04 §1 的形态定好，后续加 Anthropic / Gemini / Ollama
//! 只需要新增实现，不改 core 的调度与协议。
//!
//! 关键约定：**Provider 不负责发 `llm.started` / `llm.finished` / `llm.error`**。
//! 它只负责把上游字节流翻译成 [`StreamEvent`]，并把最终结果（用量或错误）返回；
//! 流的开闭由 core 统一决定。这样「started 恰好一条、finished 与 error 互斥」
//! 这条不变量就只有一个实现点，不会因新增适配器而破坏。

use std::collections::BTreeMap;

use ai_ide_protocol::dto::ProviderProtocol;
use tokio_util::sync::CancellationToken;

use crate::error::LlmError;
use crate::types::{ChatRequest, EventSink, HealthReport, ModelInfo, Usage};

/// 取消句柄（由 core 建立，用户点「停止」时触发）。
pub type CancelToken = CancellationToken;

/// 运行期默认超时（毫秒）。
pub const DEFAULT_TIMEOUT_MS: u64 = 60_000;

/// Provider 的运行期描述。**不含明文密钥**——密钥单独经 `SecretString` 注入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSpec {
    pub id: String,
    pub name: String,
    pub protocol: ProviderProtocol,
    pub base_url: String,
    pub headers: BTreeMap<String, String>,
    pub enabled: bool,
    pub timeout_ms: Option<u64>,
}

impl ProviderSpec {
    /// 有效超时：未配置时用 [`DEFAULT_TIMEOUT_MS`]。
    pub fn timeout_ms_or_default(&self) -> u64 {
        self.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)
    }

    /// 归一化后的接口根地址（去掉尾部斜杠，便于拼路径）。
    pub fn normalized_base_url(&self) -> String {
        self.base_url.trim_end_matches('/').to_string()
    }
}

/// 上游 Provider 能力。
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// 运行期描述。
    fn spec(&self) -> &ProviderSpec;

    /// Provider id（等价于 `self.spec().id`）。
    fn id(&self) -> &str {
        self.spec().id.as_str()
    }

    /// 是否已注入密钥。UI 的「未配置密钥」提示以此为准。
    fn has_key(&self) -> bool;

    /// 流式对话。
    ///
    /// - 事件通过 `sink` 增量下发；`sink` 关闭（UI 已离开）时返回 [`LlmError::Cancelled`]；
    /// - `cancel` 触发时立即返回 [`LlmError::Cancelled`]（不发 `llm.error`，由 core 决定语义）；
    /// - 正常结束时返回最终用量；上游异常以 `Err` 返回，由 core 转成 `llm.error` 事件。
    async fn chat_stream(
        &self,
        request: ChatRequest,
        sink: EventSink,
        cancel: CancelToken,
    ) -> Result<Usage, LlmError>;

    /// 拉取上游模型清单（也是连通性探测的实现基础）。
    async fn list_models(&self) -> Result<Vec<ModelInfo>, LlmError>;

    /// 连通性探测。
    ///
    /// 注意语义：**失败不返回 `Err`**，而是 `HealthReport { ok: false, message }`——
    /// 因为「探测失败」本身就是探测的有效结果，UI 要显示的是失败原因而不是一个错误弹窗。
    async fn health(&self) -> Result<HealthReport, LlmError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(base_url: &str, timeout_ms: Option<u64>) -> ProviderSpec {
        ProviderSpec {
            id: "p1".to_string(),
            name: "测试".to_string(),
            protocol: ProviderProtocol::OpenaiChatCompletions,
            base_url: base_url.to_string(),
            headers: BTreeMap::new(),
            enabled: true,
            timeout_ms,
        }
    }

    #[test]
    fn timeout_falls_back_to_default() {
        assert_eq!(
            spec("https://x", None).timeout_ms_or_default(),
            DEFAULT_TIMEOUT_MS
        );
        assert_eq!(spec("https://x", Some(1234)).timeout_ms_or_default(), 1234);
    }

    #[test]
    fn base_url_is_normalized() {
        assert_eq!(
            spec("https://api.x.com/v1/", None).normalized_base_url(),
            "https://api.x.com/v1"
        );
        assert_eq!(
            spec("https://api.x.com/v1", None).normalized_base_url(),
            "https://api.x.com/v1"
        );
    }
}
