//! Provider / 模型目录的持久化。
//!
//! 落盘文件里**没有任何明文密钥**：`ProviderRecord` 结构本身就没有 `api_key` 字段，
//! 密钥由主进程用 safeStorage 加密后单独保管（`docs/02` §6.3），内核只在内存里持有。
//! 这是「密钥安全存储」在骨架阶段的落地方式——**用类型系统保证不漏**，
//! 而不是靠「记得别写进去」。
//!
//! 写入策略：先写同目录临时文件再 `rename` 覆盖，避免进程中断留下半截 JSON。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ai_ide_protocol::dto::{ModelCaps, ModelDefaultParams, ProviderProtocol};
use serde::{Deserialize, Serialize};

use crate::error::LlmError;
use crate::provider::ProviderSpec;

/// 快照格式版本。将来字段演进靠它做迁移。
pub const STORE_VERSION: u32 = 1;

/// Provider 的落盘记录。**故意不包含密钥字段**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderRecord {
    pub id: String,
    pub name: String,
    pub protocol: ProviderProtocol,
    pub base_url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// 最近一次 `provider.test` 的结论。
    #[serde(default)]
    pub last_test_ok: Option<bool>,
    #[serde(default)]
    pub created_at_ms: i64,
    #[serde(default)]
    pub updated_at_ms: i64,
}

impl ProviderRecord {
    /// 转成运行期描述（剥离落盘专有字段）。
    pub fn spec(&self) -> ProviderSpec {
        ProviderSpec {
            id: self.id.clone(),
            name: self.name.clone(),
            protocol: self.protocol,
            base_url: self.base_url.clone(),
            headers: self.headers.clone(),
            enabled: self.enabled,
            timeout_ms: self.timeout_ms,
        }
    }
}

/// 模型条目的落盘记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRecord {
    pub model_pk: String,
    pub provider_id: String,
    pub model_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub caps: ModelCaps,
    #[serde(default)]
    pub params: ModelDefaultParams,
    #[serde(default)]
    pub created_at_ms: i64,
}

/// 模型主键：`<provider_id>:<model_id>`。
///
/// 用可读拼接而不是哈希：出错时日志里一眼能看出是哪个模型，也便于用户手工修配置。
pub fn model_pk(provider_id: &str, model_id: &str) -> String {
    format!("{provider_id}:{model_id}")
}

/// 一次落盘快照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreSnapshot {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub providers: Vec<ProviderRecord>,
    #[serde(default)]
    pub models: Vec<ModelRecord>,
}

impl Default for StoreSnapshot {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            providers: Vec::new(),
            models: Vec::new(),
        }
    }
}

impl StoreSnapshot {
    /// 按 `provider_id` 找记录。
    pub fn provider(&self, provider_id: &str) -> Option<&ProviderRecord> {
        self.providers.iter().find(|item| item.id == provider_id)
    }

    /// 新增或替换 Provider 记录。
    pub fn upsert_provider(&mut self, record: ProviderRecord) {
        match self.providers.iter_mut().find(|item| item.id == record.id) {
            Some(existing) => *existing = record,
            None => self.providers.push(record),
        }
        self.providers.sort_by(|left, right| left.id.cmp(&right.id));
    }

    /// 删除 Provider，并级联删除其下模型。
    pub fn remove_provider(&mut self, provider_id: &str) -> bool {
        let before = self.providers.len();
        self.providers.retain(|item| item.id != provider_id);
        self.models.retain(|item| item.provider_id != provider_id);
        before != self.providers.len()
    }

    /// 新增或替换模型记录。
    pub fn upsert_model(&mut self, record: ModelRecord) {
        match self
            .models
            .iter_mut()
            .find(|item| item.model_pk == record.model_pk)
        {
            Some(existing) => *existing = record,
            None => self.models.push(record),
        }
        self.models.sort_by(|left, right| left.model_pk.cmp(&right.model_pk));
    }

    /// 删除模型记录。
    pub fn remove_model(&mut self, model_pk: &str) -> bool {
        let before = self.models.len();
        self.models.retain(|item| item.model_pk != model_pk);
        before != self.models.len()
    }
}

/// 存储抽象。M2 只有 JSON 文件实现，将来可换 SQLite 而不动 core。
pub trait ProviderStore: Send + Sync {
    fn load(&self) -> Result<StoreSnapshot, LlmError>;
    fn save(&self, snapshot: &StoreSnapshot) -> Result<(), LlmError>;
}

/// JSON 文件存储（内核数据目录下的 `providers.json`）。
pub struct JsonFileStore {
    path: PathBuf,
    guard: Mutex<()>,
}

impl JsonFileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            guard: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.guard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl ProviderStore for JsonFileStore {
    fn load(&self) -> Result<StoreSnapshot, LlmError> {
        let _guard = self.lock();
        match std::fs::read_to_string(&self.path) {
            Ok(text) => {
                if text.trim().is_empty() {
                    return Ok(StoreSnapshot::default());
                }
                serde_json::from_str(&text).map_err(|err| {
                    LlmError::Store(format!("{} 解析失败: {err}", self.path.display()))
                })
            }
            // 首次启动：文件还不存在，等价于空目录。
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(StoreSnapshot::default()),
            Err(err) => Err(LlmError::Store(format!(
                "{} 读取失败: {err}",
                self.path.display()
            ))),
        }
    }

    fn save(&self, snapshot: &StoreSnapshot) -> Result<(), LlmError> {
        let _guard = self.lock();
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    LlmError::Store(format!("{} 创建目录失败: {err}", parent.display()))
                })?;
            }
        }
        let text = serde_json::to_string_pretty(snapshot)
            .map_err(|err| LlmError::Store(format!("序列化失败: {err}")))?;
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, text)
            .map_err(|err| LlmError::Store(format!("{} 写入失败: {err}", temp.display())))?;
        // 同目录 rename 覆盖：要么是旧文件，要么是新文件，不会出现半截内容。
        std::fs::rename(&temp, &self.path).map_err(|err| {
            LlmError::Store(format!(
                "{} 替换失败: {err}",
                self.path.display()
            ))
        })?;
        Ok(())
    }
}

fn default_version() -> u32 {
    STORE_VERSION
}

fn default_enabled() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> JsonFileStore {
        let dir = std::env::temp_dir().join(format!(
            "ai-ide-llm-store-{}",
            uuid::Uuid::new_v4()
        ));
        JsonFileStore::new(dir.join("providers.json"))
    }

    fn provider_record(id: &str) -> ProviderRecord {
        ProviderRecord {
            id: id.to_string(),
            name: id.to_string(),
            protocol: ProviderProtocol::OpenaiChatCompletions,
            base_url: "https://api.example.com/v1".to_string(),
            headers: BTreeMap::new(),
            enabled: true,
            timeout_ms: Some(30_000),
            last_test_ok: Some(true),
            created_at_ms: 1,
            updated_at_ms: 2,
        }
    }

    #[test]
    fn load_missing_file_is_empty_snapshot() {
        let store = temp_store();
        let snapshot = store.load().unwrap();
        assert_eq!(snapshot.version, STORE_VERSION);
        assert!(snapshot.providers.is_empty());
        assert!(snapshot.models.is_empty());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let store = temp_store();
        let mut snapshot = StoreSnapshot::default();
        snapshot.upsert_provider(provider_record("p1"));
        snapshot.upsert_model(ModelRecord {
            model_pk: model_pk("p1", "deepseek-chat"),
            provider_id: "p1".to_string(),
            model_id: "deepseek-chat".to_string(),
            display_name: Some("DeepSeek Chat".to_string()),
            caps: ModelCaps::default(),
            params: ModelDefaultParams::default(),
            created_at_ms: 3,
        });
        store.save(&snapshot).unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded, snapshot);
        assert!(store.path().exists());
        // 临时文件不应该留下
        assert!(!store.path().with_extension("json.tmp").exists());
    }

    #[test]
    fn persisted_file_never_contains_a_key_field() {
        let store = temp_store();
        let mut snapshot = StoreSnapshot::default();
        snapshot.upsert_provider(provider_record("p1"));
        store.save(&snapshot).unwrap();
        let text = std::fs::read_to_string(store.path()).unwrap();
        assert!(!text.contains("api_key"), "落盘文件不应出现 api_key 字段");
        assert!(!text.contains("sk-"), "落盘文件不应出现密钥前缀");
    }

    #[test]
    fn empty_file_is_treated_as_empty_snapshot() {
        let store = temp_store();
        std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        std::fs::write(store.path(), "   \n").unwrap();
        assert!(store.load().unwrap().providers.is_empty());
    }

    #[test]
    fn broken_json_reports_store_error() {
        let store = temp_store();
        std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        std::fs::write(store.path(), "{ not json").unwrap();
        let err = store.load().unwrap_err();
        assert!(matches!(err, LlmError::Store(_)));
    }

    #[test]
    fn provider_removal_cascades_to_models() {
        let mut snapshot = StoreSnapshot::default();
        snapshot.upsert_provider(provider_record("p1"));
        snapshot.upsert_model(ModelRecord {
            model_pk: model_pk("p1", "m1"),
            provider_id: "p1".to_string(),
            model_id: "m1".to_string(),
            display_name: None,
            caps: ModelCaps::default(),
            params: ModelDefaultParams::default(),
            created_at_ms: 0,
        });
        assert!(snapshot.remove_provider("p1"));
        assert!(snapshot.models.is_empty(), "删 Provider 必须级联清掉模型");
        assert!(snapshot.provider("p1").is_none());
        assert!(!snapshot.remove_provider("p1"));
    }

    #[test]
    fn upsert_keeps_records_sorted_and_unique() {
        let mut snapshot = StoreSnapshot::default();
        snapshot.upsert_provider(provider_record("b"));
        snapshot.upsert_provider(provider_record("a"));
        snapshot.upsert_provider(provider_record("b"));
        let ids: Vec<&str> = snapshot
            .providers
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids, vec!["a", "b"]);

        let mut record = ModelRecord {
            model_pk: model_pk("a", "m1"),
            provider_id: "a".to_string(),
            model_id: "m1".to_string(),
            display_name: None,
            caps: ModelCaps::default(),
            params: ModelDefaultParams::default(),
            created_at_ms: 0,
        };
        snapshot.upsert_model(record.clone());
        record.display_name = Some("改名".to_string());
        snapshot.upsert_model(record);
        assert_eq!(snapshot.models.len(), 1);
        assert_eq!(snapshot.models[0].display_name.as_deref(), Some("改名"));
        assert!(snapshot.remove_model(&model_pk("a", "m1")));
        assert!(!snapshot.remove_model(&model_pk("a", "m1")));
    }
}
