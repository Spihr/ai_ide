//! 消息仓储：追加（顺带推进会话 `updated_at`）、按 id 读、分页与最近 N 条查询。

use ai_ide_protocol::ChatToolCall;

use crate::model::{
    decode_content, decode_model_ref, decode_usage, encode_content, encode_model_ref, encode_usage,
    now_millis, role_from_str, role_to_str, Message, NewMessage,
};
use crate::{Result, Store, StoreError};

/// `message` 表的查询列（顺序与 [`MessageRow`] 对齐）。
///
/// 注意：`rowid` 不在其中——它不是表的显式列，只有直接查基表时才能出现在 `ORDER BY` 里；
/// 需要跨派生表保序时（见 [`Store::list_recent_messages`]）在 SQL 里另行投影。
const MESSAGE_COLUMNS: &str = "id, session_id, parent_id, role, content_json, tool_calls_json, \
     tool_call_id, model_ref_json, usage_json, created_at";

/// 单次查询允许的最大条数，防止 UI 误传 `i64::MAX` 把整表拉进内存。
const MAX_LIMIT: i64 = 10_000;

/// `message` 表原始行。
#[derive(Debug, sqlx::FromRow)]
struct MessageRow {
    id: String,
    session_id: String,
    parent_id: Option<String>,
    role: String,
    content_json: String,
    tool_calls_json: String,
    tool_call_id: Option<String>,
    model_ref_json: Option<String>,
    usage_json: Option<String>,
    created_at: i64,
}

impl MessageRow {
    fn into_message(self) -> Result<Message> {
        let (content, attachments) = decode_content(&self.content_json)?;
        let tool_calls: Vec<ChatToolCall> = serde_json::from_str(&self.tool_calls_json)?;

        Ok(Message {
            id: self.id,
            session_id: self.session_id,
            parent_id: self.parent_id,
            role: role_from_str(&self.role)?,
            content,
            attachments,
            tool_calls,
            tool_call_id: self.tool_call_id,
            model_ref: decode_model_ref(self.model_ref_json)?,
            usage: decode_usage(self.usage_json)?,
            created_at: self.created_at,
        })
    }
}

impl Store {
    /// 追加一条消息；同一事务内把会话 `updated_at` 推进到不早于该消息时间。
    ///
    /// 目标会话不存在时返回 [`StoreError::SessionNotFound`]（先查后插，给出比外键报错更明确的错误）。
    pub async fn append_message(&self, new: NewMessage) -> Result<Message> {
        let id = uuid::Uuid::new_v4().to_string();
        let created_at = new.created_at.unwrap_or_else(now_millis);
        let content_json = encode_content(&new.content, &new.attachments)?;
        let tool_calls_json = serde_json::to_string(&new.tool_calls)?;
        let model_ref_json = encode_model_ref(&new.model_ref)?;
        let usage_json = encode_usage(&new.usage)?;

        let mut tx = self.pool.begin().await?;

        let exists = sqlx::query_scalar::<_, i64>("SELECT 1 FROM session WHERE id = ?")
            .bind(&new.session_id)
            .fetch_optional(&mut *tx)
            .await?;
        if exists.is_none() {
            return Err(StoreError::SessionNotFound(new.session_id));
        }

        sqlx::query(
            "INSERT INTO message (id, session_id, parent_id, role, content_json, tool_calls_json, \
             tool_call_id, model_ref_json, usage_json, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .bind(&id)
        .bind(&new.session_id)
        .bind(new.parent_id.as_deref())
        .bind(role_to_str(new.role))
        .bind(&content_json)
        .bind(&tool_calls_json)
        .bind(new.tool_call_id.as_deref())
        .bind(model_ref_json.as_deref())
        .bind(usage_json.as_deref())
        .bind(created_at)
        .execute(&mut *tx)
        .await?;

        sqlx::query("UPDATE session SET updated_at = MAX(updated_at, ?1) WHERE id = ?2")
            .bind(created_at)
            .bind(&new.session_id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;

        Ok(Message {
            id,
            session_id: new.session_id,
            parent_id: new.parent_id,
            role: new.role,
            content: new.content,
            attachments: new.attachments,
            tool_calls: new.tool_calls,
            tool_call_id: new.tool_call_id,
            model_ref: new.model_ref,
            usage: new.usage,
            created_at,
        })
    }

    /// 按 id 读取消息。
    pub async fn get_message(&self, id: &str) -> Result<Option<Message>> {
        let row = sqlx::query_as::<_, MessageRow>(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM message WHERE id = ?"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(MessageRow::into_message).transpose()
    }

    /// 按时间正序列出会话消息；同毫秒的消息按插入顺序（SQLite 隐式 `rowid`）排列。
    pub async fn list_messages(
        &self,
        session_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Message>> {
        let rows = sqlx::query_as::<_, MessageRow>(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM message WHERE session_id = ? ORDER BY created_at ASC, rowid ASC LIMIT ? OFFSET ?"
        ))
        .bind(session_id)
        .bind(limit.clamp(1, MAX_LIMIT))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(MessageRow::into_message).collect()
    }

    /// 取会话最近 `limit` 条消息，仍按时间正序返回（给上下文组装用）。
    pub async fn list_recent_messages(&self, session_id: &str, limit: i64) -> Result<Vec<Message>> {
        let rows = sqlx::query_as::<_, MessageRow>(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM (SELECT rowid AS _row, {MESSAGE_COLUMNS} FROM message \
             WHERE session_id = ? ORDER BY created_at DESC, rowid DESC LIMIT ?) \
             ORDER BY created_at ASC, _row ASC"
        ))
        .bind(session_id)
        .bind(limit.clamp(1, MAX_LIMIT))
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(MessageRow::into_message).collect()
    }

    /// 会话消息条数。
    pub async fn count_messages(&self, session_id: &str) -> Result<i64> {
        let count =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM message WHERE session_id = ?")
                .bind(session_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(count)
    }

    /// 删除单条消息（编辑重发 / 撤销分支用）。返回是否命中一行。
    pub async fn delete_message(&self, id: &str) -> Result<bool> {
        let deleted = sqlx::query("DELETE FROM message WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0;
        Ok(deleted)
    }
}
