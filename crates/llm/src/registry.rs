//! Provider 注册表与工厂。
//!
//! core 持有的「当前有哪些 Provider 可用」就是这张表。每次目录变更
//! （`provider.upsert` / `provider.delete` / 启动加载）都重建对应条目，
//! 保证注册表与落盘配置一致；密钥不在这张表里，而是随 Provider 实例持有。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use ai_ide_protocol::dto::ProviderProtocol;

use crate::error::LlmError;
use crate::openai::OpenAiProvider;
use crate::provider::{Provider, ProviderSpec};
use crate::secret::SecretString;
use crate::store::ProviderRecord;
use crate::transport::HttpTransport;

/// 协议名称（用于 UI 展示与「暂不支持」错误文案）。
pub fn protocol_label(protocol: ProviderProtocol) -> &'static str {
    match protocol {
        ProviderProtocol::OpenaiChatCompletions => "OpenAI Chat Completions",
        ProviderProtocol::OpenaiResponses => "OpenAI Responses",
        ProviderProtocol::AnthropicMessages => "Anthropic Messages",
        ProviderProtocol::Gemini => "Google Gemini",
        ProviderProtocol::Ollama => "Ollama",
    }
}

/// Provider 注册表：`id -> Provider`。
#[derive(Default)]
pub struct ProviderRegistry {
    providers: RwLock<HashMap<String, Arc<dyn Provider>>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 插入或替换。
    pub fn upsert(&self, provider: Arc<dyn Provider>) {
        let mut guard = self.write();
        guard.insert(provider.id().to_string(), provider);
    }

    pub fn get(&self, provider_id: &str) -> Option<Arc<dyn Provider>> {
        self.read().get(provider_id).cloned()
    }

    pub fn remove(&self, provider_id: &str) -> Option<Arc<dyn Provider>> {
        self.write().remove(provider_id)
    }

    /// 按 id 排序的全部 Provider（顺序稳定，便于 UI 与测试断言）。
    pub fn list(&self) -> Vec<Arc<dyn Provider>> {
        let mut providers: Vec<Arc<dyn Provider>> = self.read().values().cloned().collect();
        providers.sort_by(|left, right| left.id().cmp(right.id()));
        providers
    }

    pub fn len(&self) -> usize {
        self.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    pub fn clear(&self) {
        self.write().clear();
    }

    /// 读锁。锁中毒时取回内部值继续用——一次 panic 不该让整个 Provider 目录瘫掉。
    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, Arc<dyn Provider>>> {
        self.providers
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, Arc<dyn Provider>>> {
        self.providers
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 按记录与协议构建 Provider 实例。
///
/// M2 只实现 Chat Completions；其余协议显式返回「暂不支持」，而不是静默降级到
/// Chat Completions——静默降级会让用户得到一个看似能用、实际语义不对的接口。
pub fn build_provider(
    record: &ProviderRecord,
    api_key: Option<SecretString>,
    transport: Arc<dyn HttpTransport>,
) -> Result<Arc<dyn Provider>, LlmError> {
    match record.protocol {
        ProviderProtocol::OpenaiChatCompletions => Ok(Arc::new(OpenAiProvider::new(
            record.spec(),
            api_key,
            transport,
        ))),
        other => Err(LlmError::unsupported(protocol_label(other))),
    }
}

/// 便捷：把一批记录全部构建进注册表（启动时使用）。
///
/// 单个 Provider 构建失败不影响其它条目——记下失败的 id 交给调用方记日志。
pub fn rebuild_registry(
    registry: &ProviderRegistry,
    records: &[ProviderRecord],
    keys: &HashMap<String, SecretString>,
    transport: Arc<dyn HttpTransport>,
) -> Result<Vec<String>, LlmError> {
    let mut failed = Vec::new();
    registry.clear();
    for record in records.iter().filter(|record| record.enabled) {
        match build_provider(record, keys.get(&record.id).cloned(), transport.clone()) {
            Ok(provider) => registry.upsert(provider),
            Err(_) => failed.push(record.id.clone()),
        }
    }
    Ok(failed)
}

/// 便于 core 组装：按 id 建立空的密钥表。
#[must_use]
pub fn empty_keys() -> HashMap<String, SecretString> {
    HashMap::new()
}

/// 供测试/UI 展示的 spec 摘要。
#[must_use]
pub fn describe(spec: &ProviderSpec) -> String {
    format!("{} ({})", spec.name, protocol_label(spec.protocol))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::spec_of;
    use crate::store::model_pk;
    use std::collections::BTreeMap;

    struct StubProvider {
        spec: ProviderSpec,
    }

    #[async_trait::async_trait]
    impl Provider for StubProvider {
        fn spec(&self) -> &ProviderSpec {
            &self.spec
        }

        fn has_key(&self) -> bool {
            true
        }

        async fn chat_stream(
            &self,
            _request: crate::types::ChatRequest,
            _sink: crate::types::EventSink,
            _cancel: crate::provider::CancelToken,
        ) -> Result<crate::types::Usage, LlmError> {
            Ok(crate::types::Usage::default())
        }

        async fn list_models(&self) -> Result<Vec<crate::types::ModelInfo>, LlmError> {
            Ok(vec![])
        }

        async fn health(&self) -> Result<crate::types::HealthReport, LlmError> {
            Ok(crate::types::HealthReport {
                ok: true,
                latency_ms: 1,
                models_found: 0,
                message: None,
            })
        }
    }

    fn record(id: &str, protocol: ProviderProtocol) -> ProviderRecord {
        ProviderRecord {
            id: id.to_string(),
            name: id.to_string(),
            protocol,
            base_url: "https://gw.example.com/v1".to_string(),
            headers: BTreeMap::new(),
            enabled: true,
            timeout_ms: None,
            last_test_ok: None,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    #[test]
    fn registry_replaces_and_sorts() {
        let registry = ProviderRegistry::new();
        for id in ["b", "a"] {
            registry.upsert(Arc::new(StubProvider {
                spec: spec_of(id, id, "https://x", BTreeMap::new(), None),
            }));
        }
        let ids: Vec<String> = registry.list().iter().map(|p| p.id().to_string()).collect();
        assert_eq!(ids, vec!["a", "b"]);
        assert_eq!(registry.len(), 2);
        assert!(!registry.is_empty());

        registry.upsert(Arc::new(StubProvider {
            spec: spec_of("a", "a2", "https://x", BTreeMap::new(), None),
        }));
        assert_eq!(registry.len(), 2, "同 id 应替换而不是新增");
        assert_eq!(registry.get("a").unwrap().spec().name, "a2");

        assert!(registry.remove("a").is_some());
        assert_eq!(registry.len(), 1);
        registry.clear();
        assert!(registry.is_empty());
    }

    #[test]
    fn build_provider_supports_chat_completions_only_in_m2() {
        let transport: Arc<dyn HttpTransport> =
            Arc::new(crate::transport::ReqwestTransport::default());
        let built = build_provider(
            &record("p1", ProviderProtocol::OpenaiChatCompletions),
            Some(SecretString::new("sk-abcdefghijkl")),
            transport.clone(),
        );
        assert!(built.is_ok());
        assert!(built.unwrap().has_key());

        let unsupported = build_provider(
            &record("p2", ProviderProtocol::AnthropicMessages),
            None,
            transport,
        );
        match unsupported {
            Ok(_) => panic!("M2 未实现 Anthropic Messages，不应构建成功"),
            Err(err) => assert!(matches!(err, LlmError::Unsupported(_))),
        }
    }

    #[test]
    fn rebuild_skips_disabled_and_reports_failures() {
        let registry = ProviderRegistry::new();
        let mut disabled = record("disabled", ProviderProtocol::OpenaiChatCompletions);
        disabled.enabled = false;
        let records = vec![
            record("ok", ProviderProtocol::OpenaiChatCompletions),
            record("bad", ProviderProtocol::Gemini),
            disabled,
        ];
        let failed = rebuild_registry(
            &registry,
            &records,
            &empty_keys(),
            Arc::new(crate::transport::ReqwestTransport::default()),
        )
        .unwrap();
        assert_eq!(failed, vec!["bad".to_string()]);
        assert_eq!(registry.len(), 1);
        assert!(registry.get("ok").is_some());
    }

    #[test]
    fn protocol_labels_are_human_readable() {
        assert_eq!(
            protocol_label(ProviderProtocol::Ollama),
            "Ollama",
            "未实现协议也要能报出人话"
        );
        assert!(
            describe(&spec_of("p", "网关", "https://x", BTreeMap::new(), None)).contains("网关")
        );
        assert_eq!(model_pk("p1", "m1"), "p1:m1");
    }
}
