//! 模型接入层在 core 侧的运行期状态（M2）。
//!
//! `provider.*` 处理器只跟这一层打交道，它把四样东西收在一起：
//!
//! | 组成 | 作用 |
//! |---|---|
//! | [`ProviderStore`] | 落盘的 Provider / 模型目录（`providers.json`） |
//! | [`ProviderRegistry`] | 目录的运行期投影：真正能被调用的 Provider 实例 |
//! | 密钥表 | 只存在于内存的 [`SecretString`]，随 Provider 实例一起交给适配器 |
//! | [`HttpTransport`] | HTTP 传输；单测注入替身即可离线覆盖全部逻辑 |
//!
//! 两条不变量值得单独强调：
//!
//! 1. **密钥不落盘**。落盘的 [`ProviderRecord`] 结构上就没有密钥字段，密钥只在内存；
//!    进程重启后需要主进程重新注入（`docs/02` §6.3）。
//! 2. **注册表跟着目录走**。每次目录变更后按记录重建对应条目，
//!    避免出现「`provider.list` 说有、实际调不通」的两套状态。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use ai_ide_llm::{
    build_provider, rebuild_registry, HttpTransport, JsonFileStore, LlmError, ProviderRecord,
    ProviderRegistry, ProviderStore, ReqwestTransport, SecretString,
};
use tracing::warn;

/// 指定内核数据目录的环境变量。主进程可用它做多实例隔离，测试也用它把落盘位置挪到临时目录。
pub const DATA_DIR_ENV: &str = "AI_IDE_DATA_DIR";

/// Provider 目录文件名。
const STORE_FILE: &str = "providers.json";

/// 内核数据目录。
///
/// 优先级：`AI_IDE_DATA_DIR` → `%APPDATA%\ai-ide` → `$HOME/.ai-ide` → 系统临时目录。
/// 前三级都能给出稳定的可写位置；临时目录只是最后的兜底，不保证跨重启保留。
pub fn default_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(DATA_DIR_ENV).filter(|value| !value.is_empty()) {
        return PathBuf::from(dir);
    }
    if let Some(appdata) = std::env::var_os("APPDATA").filter(|value| !value.is_empty()) {
        return PathBuf::from(appdata).join("ai-ide");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(home).join(".ai-ide");
    }
    std::env::temp_dir().join("ai-ide")
}

/// Provider 目录的默认落盘路径。
pub fn default_store_path() -> PathBuf {
    default_data_dir().join(STORE_FILE)
}

/// core 持有的模型接入运行期状态。
pub struct LlmRuntime {
    store: Box<dyn ProviderStore>,
    registry: ProviderRegistry,
    keys: Mutex<HashMap<String, SecretString>>,
    transport: Arc<dyn HttpTransport>,
}

impl LlmRuntime {
    /// 用真实 HTTP 传输与指定落盘路径构建。
    pub fn new(store_path: impl Into<PathBuf>) -> Self {
        Self::with_transport(store_path, Arc::new(ReqwestTransport::default()))
    }

    /// 指定传输实现。单测注入测试替身后，整条 `provider.*` 链路无网络也能验收。
    pub fn with_transport(
        store_path: impl Into<PathBuf>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            store: Box::new(JsonFileStore::new(store_path)),
            registry: ProviderRegistry::new(),
            keys: Mutex::new(HashMap::new()),
            transport,
        }
    }

    /// 目录存储。
    pub fn store(&self) -> &dyn ProviderStore {
        self.store.as_ref()
    }

    /// Provider 注册表。
    pub fn registry(&self) -> &ProviderRegistry {
        &self.registry
    }

    /// HTTP 传输（构建 Provider 实例时透传）。
    pub fn transport(&self) -> Arc<dyn HttpTransport> {
        Arc::clone(&self.transport)
    }

    /// 记下一把密钥。**只进内存**，任何路径都不会把它写进磁盘。
    pub fn put_key(&self, provider_id: &str, key: SecretString) {
        self.keys().insert(provider_id.to_string(), key);
    }

    /// 取密钥副本（用于构建 Provider 实例）。
    pub fn key(&self, provider_id: &str) -> Option<SecretString> {
        self.keys().get(provider_id).cloned()
    }

    /// 丢弃密钥（删除 Provider 时级联调用）。
    pub fn drop_key(&self, provider_id: &str) -> Option<SecretString> {
        self.keys().remove(provider_id)
    }

    /// 脱敏指纹；未配置密钥（或空密钥）时为 `None`。
    pub fn key_hint(&self, provider_id: &str) -> Option<String> {
        self.key(provider_id)
            .filter(|key| !key.is_empty())
            .map(|key| key.hint().to_string())
    }

    /// 启动时按落盘目录重建注册表，返回构建失败的 provider id。
    ///
    /// 此刻密钥必定为空——磁盘上没有密钥，需要主进程解出 safeStorage 密文后
    /// 经 `provider.upsert` 重新注入。因此「重启后 Provider 在列表里但未配密钥」
    /// 是正常状态，`provider.list` 会如实回报 `has_key: false`。
    pub fn bootstrap(&self) -> Result<Vec<String>, LlmError> {
        let snapshot = self.store.load()?;
        let keys = self.keys().clone();
        rebuild_registry(&self.registry, &snapshot.providers, &keys, self.transport())
    }

    /// 让注册表跟上一条记录的最新状态：启用则重建，停用则摘除。
    pub fn sync_provider(&self, record: &ProviderRecord) -> Result<(), LlmError> {
        if !record.enabled {
            self.registry.remove(&record.id);
            return Ok(());
        }
        let provider = build_provider(record, self.key(&record.id), self.transport())?;
        self.registry.upsert(provider);
        Ok(())
    }

    /// 删除 Provider：注册表条目与内存密钥一起清掉。
    pub fn remove_provider(&self, provider_id: &str) {
        self.registry.remove(provider_id);
        self.drop_key(provider_id);
    }

    /// 密钥表加锁。锁中毒时取回内部值继续用——一次 panic 不该让整个 Provider 目录瘫掉。
    fn keys(&self) -> MutexGuard<'_, HashMap<String, SecretString>> {
        self.keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 启动告警：Provider 目录装载失败或部分条目不可用。
///
/// 骨架期这条路径很常见（当前只落地了 Chat Completions，其它协议会落到 `failed`），
/// 所以只记日志、不阻断启动：内核起不来比少一个 Provider 严重得多。
pub fn report_bootstrap(loaded: Result<Vec<String>, LlmError>) {
    match loaded {
        Ok(failed) if failed.is_empty() => {}
        Ok(failed) => warn!(?failed, "部分 Provider 未装载（协议暂不支持或记录非法）"),
        Err(err) => warn!(error = %err, "Provider 目录装载失败"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_ide_llm::StoreSnapshot;
    use ai_ide_protocol::dto::ProviderProtocol;
    use std::collections::BTreeMap;

    fn temp_store_path(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ai-ide-m2-runtime-{tag}-{}", uuid::Uuid::new_v4()));
        dir.join(STORE_FILE)
    }

    fn record(id: &str, enabled: bool) -> ProviderRecord {
        ProviderRecord {
            id: id.to_string(),
            name: id.to_string(),
            protocol: ProviderProtocol::OpenaiChatCompletions,
            base_url: "https://gw.example.com/v1".to_string(),
            headers: BTreeMap::new(),
            enabled,
            timeout_ms: None,
            last_test_ok: None,
            created_at_ms: 1,
            updated_at_ms: 2,
        }
    }

    #[test]
    fn bootstrap_loads_persisted_records_into_registry() {
        let path = temp_store_path("bootstrap");
        let runtime = LlmRuntime::new(&path);
        let mut snapshot = StoreSnapshot::default();
        snapshot.upsert_provider(record("p1", true));
        snapshot.upsert_provider(record("p2", false));
        runtime.store().save(&snapshot).unwrap();

        // 新建一个 runtime 模拟进程重启：注册表从空开始。
        let restarted = LlmRuntime::new(&path);
        assert!(restarted.registry().is_empty());
        assert!(restarted.bootstrap().unwrap().is_empty());
        assert_eq!(
            restarted.registry().len(),
            1,
            "停用的 Provider 不该进注册表"
        );
        assert!(restarted.registry().get("p1").is_some());
        assert!(
            !restarted.registry().get("p1").unwrap().has_key(),
            "重启后密钥未注入，应如实回报未配密钥"
        );
    }

    #[test]
    fn bootstrap_skips_records_whose_protocol_is_not_wired_yet() {
        let path = temp_store_path("unsupported");
        let runtime = LlmRuntime::new(&path);
        let mut snapshot = StoreSnapshot::default();
        let mut gemini = record("p-gemini", true);
        gemini.protocol = ProviderProtocol::Gemini;
        snapshot.upsert_provider(gemini);
        runtime.store().save(&snapshot).unwrap();

        let failed = LlmRuntime::new(&path).bootstrap().unwrap();
        assert_eq!(failed, vec!["p-gemini".to_string()]);
    }

    #[test]
    fn sync_provider_adds_and_removes_entries() {
        let runtime = LlmRuntime::new(temp_store_path("sync"));
        runtime.sync_provider(&record("p1", true)).unwrap();
        assert_eq!(runtime.registry().len(), 1);
        runtime.sync_provider(&record("p1", false)).unwrap();
        assert!(runtime.registry().is_empty(), "停用后应从注册表摘除");
    }

    #[test]
    fn keys_live_in_memory_only_and_are_dropped_with_the_provider() {
        let runtime = LlmRuntime::new(temp_store_path("keys"));
        runtime.put_key("p1", SecretString::new("sk-abcdefghijklmnop"));
        assert_eq!(runtime.key_hint("p1").as_deref(), Some("sk-a…mnop"));
        runtime.remove_provider("p1");
        assert!(runtime.key("p1").is_none());
        assert!(runtime.key_hint("p1").is_none());
        assert!(runtime.key_hint("从未配置").is_none());
    }

    #[test]
    fn empty_key_counts_as_missing() {
        let runtime = LlmRuntime::new(temp_store_path("empty-key"));
        runtime.put_key("p1", SecretString::new(""));
        assert!(
            runtime.key_hint("p1").is_none(),
            "空密钥不该给出指纹，否则 UI 会以为已经配好了"
        );
    }

    #[test]
    fn default_store_path_follows_data_dir_env() {
        // 只在显式设置时断言，避免和其它测试的环境变量相互干扰。
        let expected = std::env::var_os(DATA_DIR_ENV).filter(|value| !value.is_empty());
        let path = default_store_path();
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some(STORE_FILE)
        );
        if let Some(dir) = expected {
            assert!(path.starts_with(PathBuf::from(dir)));
        }
    }
}
