//! 领域模型：会话、消息，以及 JSON 列的编解码。

use std::time::{SystemTime, UNIX_EPOCH};

use ai_ide_protocol::{ChatAttachment, ChatMessage, ChatRole, ChatToolCall, ModelRef};
use serde::{Deserialize, Serialize};

use crate::{Result, StoreError};

/// 单次导出 / 分叉允许携带的消息条数上限。
///
/// 与 [`crate::repo::message`] 里的查询上限同量级：超过这个量的会话已经不是
/// 「导出一份存档」而是「搬库」，需要流式实现，不能靠一次性读进内存硬扛。
pub const MAX_FORK_MESSAGES: i64 = 10_000;

/// fork 的结果：新会话 + 已重映射 id 的消息（按时间正序）。
#[derive(Debug, Clone)]
pub struct ForkOutcome {
    pub session: Session,
    pub messages: Vec<Message>,
}

/// 全库统一的时间口径：Unix 毫秒。
pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 会话
// ---------------------------------------------------------------------------

/// 一条会话（`session` 表）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// UUID v4。
    pub id: String,
    pub title: String,
    /// 会话绑定的工作区绝对路径；跨工作区会话为 `None`。
    pub workspace_path: Option<String>,
    /// 会话默认模型（对应 docs/04 §4.3 的 `model_ref`）。
    pub model_ref: Option<ModelRef>,
    /// Unix 毫秒。
    pub created_at: i64,
    /// Unix 毫秒；追加消息与会话改名都会推进。
    pub updated_at: i64,
}

/// 新建会话的入参。
#[derive(Debug, Clone)]
pub struct NewSession {
    pub title: String,
    pub workspace_path: Option<String>,
    pub model_ref: Option<ModelRef>,
    /// 显式指定创建时间（测试造数据用）；`None` 表示取 [`now_millis`]。
    pub created_at: Option<i64>,
}

impl NewSession {
    /// 以标题创建入参。
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            workspace_path: None,
            model_ref: None,
            created_at: None,
        }
    }

    /// 绑定工作区路径。
    pub fn with_workspace_path(mut self, workspace_path: impl Into<String>) -> Self {
        self.workspace_path = Some(workspace_path.into());
        self
    }

    /// 指定会话默认模型。
    pub fn with_model_ref(mut self, model_ref: ModelRef) -> Self {
        self.model_ref = Some(model_ref);
        self
    }

    /// 指定创建时间（Unix 毫秒）。
    pub fn with_created_at(mut self, created_at: i64) -> Self {
        self.created_at = Some(created_at);
        self
    }
}

/// 会话列表查询条件。
#[derive(Debug, Clone, Copy)]
pub struct SessionQuery<'a> {
    /// 只列指定工作区的会话；`None` 表示不过滤。
    pub workspace_path: Option<&'a str>,
    /// 返回条数上限（内部会夹到 1..=500）。
    pub limit: i64,
    pub offset: i64,
}

impl Default for SessionQuery<'_> {
    fn default() -> Self {
        Self {
            workspace_path: None,
            limit: 50,
            offset: 0,
        }
    }
}

impl<'a> SessionQuery<'a> {
    /// 限定某个工作区。
    pub fn workspace(workspace_path: &'a str) -> Self {
        Self {
            workspace_path: Some(workspace_path),
            ..Self::default()
        }
    }

    /// 设置返回条数上限。
    pub fn with_limit(mut self, limit: i64) -> Self {
        self.limit = limit;
        self
    }

    /// 设置偏移量。
    pub fn with_offset(mut self, offset: i64) -> Self {
        self.offset = offset;
        self
    }
}

// ---------------------------------------------------------------------------
// 消息
// ---------------------------------------------------------------------------

/// 一条消息（`message` 表）。
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// UUID v4。
    pub id: String,
    pub session_id: String,
    /// 父消息 id（编辑重发 / 分支链），暂允许为空。
    pub parent_id: Option<String>,
    pub role: ChatRole,
    /// 文本内容；纯工具调用消息可能为空串。
    pub content: String,
    pub attachments: Vec<ChatAttachment>,
    /// 助手消息携带的工具调用。
    pub tool_calls: Vec<ChatToolCall>,
    /// `role = tool` 时指向被回应的调用 id。
    pub tool_call_id: Option<String>,
    /// 产生该消息时使用的模型。
    pub model_ref: Option<ModelRef>,
    /// 上游用量（`llm.usage` 事件体），原样 JSON 保存。
    pub usage: Option<serde_json::Value>,
    /// Unix 毫秒。
    pub created_at: i64,
}

impl Message {
    /// 转回协议层 DTO，供上下文组装（M3/M5 后续切片）直接复用。
    pub fn to_chat_message(&self) -> ChatMessage {
        ChatMessage {
            role: self.role,
            content: self.content.clone(),
            attachments: self.attachments.clone(),
            tool_calls: self.tool_calls.clone(),
            tool_call_id: self.tool_call_id.clone(),
        }
    }
}

/// 追加消息的入参。
#[derive(Debug, Clone)]
pub struct NewMessage {
    pub session_id: String,
    pub parent_id: Option<String>,
    pub role: ChatRole,
    pub content: String,
    pub attachments: Vec<ChatAttachment>,
    pub tool_calls: Vec<ChatToolCall>,
    pub tool_call_id: Option<String>,
    pub model_ref: Option<ModelRef>,
    pub usage: Option<serde_json::Value>,
    /// 显式指定创建时间（测试造数据用）；`None` 表示取 [`now_millis`]。
    pub created_at: Option<i64>,
}

impl NewMessage {
    fn base(session_id: impl Into<String>, role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            parent_id: None,
            role,
            content: content.into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            model_ref: None,
            usage: None,
            created_at: None,
        }
    }

    /// 用户消息。
    pub fn user(session_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::base(session_id, ChatRole::User, content)
    }

    /// 助手消息。
    pub fn assistant(session_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::base(session_id, ChatRole::Assistant, content)
    }

    /// 系统消息。
    pub fn system(session_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::base(session_id, ChatRole::System, content)
    }

    /// 工具结果消息。
    pub fn tool(
        session_id: impl Into<String>,
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        let mut message = Self::base(session_id, ChatRole::Tool, content);
        message.tool_call_id = Some(tool_call_id.into());
        message
    }

    /// 从协议层 DTO 构造（对话面板回写历史时用）。
    pub fn from_chat(session_id: impl Into<String>, message: &ChatMessage) -> Self {
        let mut new = Self::base(session_id, message.role, message.content.clone());
        new.attachments = message.attachments.clone();
        new.tool_calls = message.tool_calls.clone();
        new.tool_call_id = message.tool_call_id.clone();
        new
    }

    /// 设置父消息。
    pub fn with_parent(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_id = Some(parent_id.into());
        self
    }

    /// 设置附件列表。
    pub fn with_attachments(mut self, attachments: Vec<ChatAttachment>) -> Self {
        self.attachments = attachments;
        self
    }

    /// 设置工具调用列表。
    pub fn with_tool_calls(mut self, tool_calls: Vec<ChatToolCall>) -> Self {
        self.tool_calls = tool_calls;
        self
    }

    /// 设置产生该消息的模型。
    pub fn with_model_ref(mut self, model_ref: ModelRef) -> Self {
        self.model_ref = Some(model_ref);
        self
    }

    /// 设置用量 JSON。
    pub fn with_usage(mut self, usage: serde_json::Value) -> Self {
        self.usage = Some(usage);
        self
    }

    /// 指定创建时间（Unix 毫秒）。
    pub fn with_created_at(mut self, created_at: i64) -> Self {
        self.created_at = Some(created_at);
        self
    }
}

// ---------------------------------------------------------------------------
// 审计
// ---------------------------------------------------------------------------

/// 审计事件种类。
///
/// 用 `&str` 常量而不是 `enum`：审计表只增不改，新增一种事件不该让老版本
/// 客户端反序列化失败，也不该逼着老库做一次数据迁移。
pub mod audit_kind {
    /// 会话创建。
    pub const SESSION_CREATED: &str = "session.created";
    /// 会话改名 / 改默认模型。
    pub const SESSION_UPDATED: &str = "session.updated";
    /// 会话删除。
    pub const SESSION_DELETED: &str = "session.deleted";
    /// 会话导出。
    pub const SESSION_EXPORTED: &str = "session.exported";
    /// 会话分叉。
    pub const SESSION_FORKED: &str = "session.forked";
}

/// 审计事件的发起方。
pub mod audit_actor {
    /// 内核自身（协议请求驱动）。
    pub const CORE: &str = "core";
    /// 用户显式操作。
    pub const USER: &str = "user";
    /// Agent / 模型侧发起。
    pub const AGENT: &str = "agent";
}

/// 一条已落库的审计事件。
///
/// `detail` 在读取时已再次脱敏（见 [`crate::redact`]）：老版本写入的行、被手工
/// 改过的库，都不能经由查询接口把密钥带出去。
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEvent {
    /// 自增主键，也是翻页排序的稳定依据。
    pub id: i64,
    /// 事件种类，见 [`audit_kind`]。
    pub kind: String,
    /// 关联会话；跨会话事件为空。
    pub session_id: Option<String>,
    /// 发起方，见 [`audit_actor`]。
    pub actor: String,
    /// 人可读摘要（读取时已脱敏）。
    pub summary: String,
    /// 结构化补充信息（读取时已脱敏）。
    pub detail: serde_json::Value,
    /// 事件时刻（Unix 毫秒）。
    pub created_at: i64,
}

/// 待写入的审计事件。
///
/// 构造时**不做**脱敏，脱敏统一在 [`Store::record_audit`] 里做——少一个入口就少
/// 一处能忘。
#[derive(Debug, Clone, PartialEq)]
pub struct NewAuditEvent {
    pub kind: String,
    pub session_id: Option<String>,
    pub actor: String,
    pub summary: String,
    pub detail: serde_json::Value,
    pub created_at: Option<i64>,
}

impl NewAuditEvent {
    /// 以「内核」身份构造。
    pub fn core(kind: impl Into<String>, summary: impl Into<String>) -> Self {
        Self::by(audit_actor::CORE, kind, summary)
    }

    /// 以指定身份构造。
    pub fn by(
        actor: impl Into<String>,
        kind: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            session_id: None,
            actor: actor.into(),
            summary: summary.into(),
            detail: serde_json::Value::Null,
            created_at: None,
        }
    }

    /// 关联到某个会话。
    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// 附带结构化明细；必须是 JSON 对象（数组 / 标量在这里就被拍平成对象，
    /// 免得查询侧还要处理三种形态）。
    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = match detail {
            serde_json::Value::Object(_) => detail,
            serde_json::Value::Null => serde_json::Value::Null,
            other => serde_json::json!({ "value": other }),
        };
        self
    }

    /// 指定事件时刻（Unix 毫秒），缺省取当前时间。
    pub fn with_created_at(mut self, created_at: i64) -> Self {
        self.created_at = Some(created_at);
        self
    }
}

/// 审计查询过滤条件（不含分页）。
#[derive(Debug, Clone, Copy, Default)]
pub struct AuditFilter<'a> {
    /// 只看某个会话的事件。
    pub session_id: Option<&'a str>,
    /// 只看某种事件。
    pub kind: Option<&'a str>,
    /// 起始时刻（含，Unix 毫秒）。
    pub since_ms: Option<i64>,
    /// 结束时刻（含，Unix 毫秒）。
    pub until_ms: Option<i64>,
}

/// 审计查询条件（过滤 + 分页）。
#[derive(Debug, Clone, Copy, Default)]
pub struct AuditQuery<'a> {
    pub filter: AuditFilter<'a>,
    /// 返回条数上限（内部会夹到 1..=500）。
    pub limit: i64,
    pub offset: i64,
}

impl<'a> AuditQuery<'a> {
    /// 空条件，默认取最近 50 条。
    pub fn new() -> Self {
        Self {
            filter: AuditFilter::default(),
            limit: 50,
            offset: 0,
        }
    }

    /// 限定会话。
    pub fn session(mut self, session_id: &'a str) -> Self {
        self.filter.session_id = Some(session_id);
        self
    }

    /// 限定事件种类。
    pub fn kind(mut self, kind: &'a str) -> Self {
        self.filter.kind = Some(kind);
        self
    }

    /// 起始时刻（含）。
    pub fn since(mut self, since_ms: i64) -> Self {
        self.filter.since_ms = Some(since_ms);
        self
    }

    /// 结束时刻（含）。
    pub fn until(mut self, until_ms: i64) -> Self {
        self.filter.until_ms = Some(until_ms);
        self
    }

    /// 设置返回条数上限。
    pub fn with_limit(mut self, limit: i64) -> Self {
        self.limit = limit;
        self
    }

    /// 设置偏移量。
    pub fn with_offset(mut self, offset: i64) -> Self {
        self.offset = offset;
        self
    }
}

// ---------------------------------------------------------------------------
// 列编解码（`content_json` / `*_json`）
// ---------------------------------------------------------------------------

/// `content_json` 列的载荷：文本 + 附件。
#[derive(Debug, Serialize, Deserialize)]
struct ContentPayload {
    content: String,
    #[serde(default)]
    attachments: Vec<ChatAttachment>,
}

/// 编码 `content_json`。
pub(crate) fn encode_content(content: &str, attachments: &[ChatAttachment]) -> Result<String> {
    let payload = ContentPayload {
        content: content.to_string(),
        attachments: attachments.to_vec(),
    };
    Ok(serde_json::to_string(&payload)?)
}

/// 解码 `content_json`：返回 (文本, 附件)。
pub(crate) fn decode_content(raw: &str) -> Result<(String, Vec<ChatAttachment>)> {
    let payload: ContentPayload = serde_json::from_str(raw)?;
    Ok((payload.content, payload.attachments))
}

/// 编码可空的 `model_ref_json`。
pub(crate) fn encode_model_ref(model_ref: &Option<ModelRef>) -> Result<Option<String>> {
    match model_ref {
        Some(model_ref) => Ok(Some(serde_json::to_string(model_ref)?)),
        None => Ok(None),
    }
}

/// 解码可空的 `model_ref_json`。
pub(crate) fn decode_model_ref(raw: Option<String>) -> Result<Option<ModelRef>> {
    match raw {
        Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
        None => Ok(None),
    }
}

/// 编码可空的 `usage_json`。
pub(crate) fn encode_usage(usage: &Option<serde_json::Value>) -> Result<Option<String>> {
    match usage {
        Some(usage) => Ok(Some(serde_json::to_string(usage)?)),
        None => Ok(None),
    }
}

/// 解码可空的 `usage_json`。
pub(crate) fn decode_usage(raw: Option<String>) -> Result<Option<serde_json::Value>> {
    match raw {
        Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
        None => Ok(None),
    }
}

/// `message.role` 列取值。
pub(crate) fn role_to_str(role: ChatRole) -> &'static str {
    match role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    }
}

/// 解析 `message.role` 列取值。
pub(crate) fn role_from_str(raw: &str) -> Result<ChatRole> {
    match raw {
        "system" => Ok(ChatRole::System),
        "user" => Ok(ChatRole::User),
        "assistant" => Ok(ChatRole::Assistant),
        "tool" => Ok(ChatRole::Tool),
        other => Err(StoreError::InvalidRole(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_round_trip_covers_all_variants() {
        for role in [
            ChatRole::System,
            ChatRole::User,
            ChatRole::Assistant,
            ChatRole::Tool,
        ] {
            assert_eq!(role_from_str(role_to_str(role)).unwrap(), role);
        }
    }

    #[test]
    fn unknown_role_is_rejected() {
        let err = role_from_str("wizard").unwrap_err();
        assert!(matches!(err, StoreError::InvalidRole(raw) if raw == "wizard"));
    }

    #[test]
    fn content_payload_round_trip_keeps_attachments() {
        let attachments = vec![ChatAttachment {
            kind: "file".into(),
            path: "src/lib.rs".into(),
        }];
        let raw = encode_content("看下这个文件", &attachments).unwrap();
        let (content, decoded) = decode_content(&raw).unwrap();
        assert_eq!(content, "看下这个文件");
        assert_eq!(decoded, attachments);
    }

    #[test]
    fn content_payload_without_attachments_field_is_tolerated() {
        let (content, attachments) = decode_content(r#"{"content":"hi"}"#).unwrap();
        assert_eq!(content, "hi");
        assert!(attachments.is_empty());
    }

    #[test]
    fn broken_json_column_reports_json_error() {
        let err = decode_content("{oops").unwrap_err();
        assert!(matches!(err, StoreError::Json(_)));
        let err = decode_model_ref(Some("{oops".into())).unwrap_err();
        assert!(matches!(err, StoreError::Json(_)));
    }

    #[test]
    fn model_ref_and_usage_round_trip() {
        let model_ref = ModelRef {
            provider_id: "p1".into(),
            model_id: "m1".into(),
        };
        let raw = encode_model_ref(&Some(model_ref.clone())).unwrap();
        assert_eq!(decode_model_ref(raw).unwrap(), Some(model_ref));
        assert!(decode_model_ref(None).unwrap().is_none());

        let usage = serde_json::json!({ "total_tokens": 15 });
        let raw = encode_usage(&Some(usage.clone())).unwrap();
        assert_eq!(decode_usage(raw).unwrap(), Some(usage));
        assert!(decode_usage(None).unwrap().is_none());
    }

    #[test]
    fn now_millis_is_positive_and_monotonic_enough() {
        let first = now_millis();
        let second = now_millis();
        assert!(first > 1_700_000_000_000, "时间戳应为 2023 年之后");
        assert!(second >= first);
    }
}
