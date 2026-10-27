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

/// 事务内全量读取会话消息（按时间正序），供 `fork_session` 使用。
///
/// 单独抽出来的理由：fork 必须在**同一个事务**里读源、写新，不能复用
/// [`Store::list_messages`]——那会自己开一条连接，读到的是另一个快照。
pub(crate) async fn list_messages_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session_id: &str,
) -> Result<Vec<Message>> {
    let rows = sqlx::query_as::<_, MessageRow>(&format!(
        "SELECT {MESSAGE_COLUMNS} FROM message WHERE session_id = ? ORDER BY created_at ASC, rowid ASC"
    ))
    .bind(session_id)
    .fetch_all(&mut **tx)
    .await?;

    rows.into_iter().map(MessageRow::into_message).collect()
}

/// 事务内插入一条消息；`id` 与 `parent_id` 由调用方给定（fork 要重映射父子链）。
pub(crate) async fn insert_message_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: &str,
    message: &Message,
    parent_id: Option<&str>,
) -> Result<()> {
    let content_json = encode_content(&message.content, &message.attachments)?;
    let tool_calls_json = serde_json::to_string(&message.tool_calls)?;
    let model_ref_json = encode_model_ref(&message.model_ref)?;
    let usage_json = encode_usage(&message.usage)?;

    sqlx::query(
        "INSERT INTO message (id, session_id, parent_id, role, content_json, tool_calls_json, \
         tool_call_id, model_ref_json, usage_json, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )
    .bind(id)
    .bind(&message.session_id)
    .bind(parent_id)
    .bind(role_to_str(message.role))
    .bind(&content_json)
    .bind(&tool_calls_json)
    .bind(message.tool_call_id.as_deref())
    .bind(model_ref_json.as_deref())
    .bind(usage_json.as_deref())
    .bind(message.created_at)
    .execute(&mut **tx)
    .await?;

    Ok(())
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

    /// 从**末尾**取一页消息，仍按时间正序返回。
    ///
    /// `skip` 是「末尾往前跳过多少条」，也就是外壳已经显示的那部分——长会话打开时
    /// 先取最后 N 条，用户往上翻时把已显示条数当 `skip` 再要一页。与
    /// [`Store::list_messages`] 的区别只在起点方向：后者从最早一条开始。
    ///
    /// 实现上是「先倒序取一页，再套一层正序排」，因为 SQLite 没有 `LIMIT ... FROM END`；
    /// 外层必须带 `_row`（`rowid`）兜同毫秒顺序，否则同一毫秒的两条消息在两次查询里
    /// 可能换位，翻页时就会重复或漏条。
    pub async fn list_message_page_tail(
        &self,
        session_id: &str,
        limit: i64,
        skip: i64,
    ) -> Result<Vec<Message>> {
        let rows = sqlx::query_as::<_, MessageRow>(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM (SELECT rowid AS _row, {MESSAGE_COLUMNS} FROM message \
             WHERE session_id = ? ORDER BY created_at DESC, rowid DESC LIMIT ? OFFSET ?) \
             ORDER BY created_at ASC, _row ASC"
        ))
        .bind(session_id)
        .bind(limit.clamp(1, MAX_LIMIT))
        .bind(skip.max(0))
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(MessageRow::into_message).collect()
    }

    /// 按关键词检索消息正文，可选限定会话。
    ///
    /// 判定用 `instr(lower(content_json), lower(?))` 而不是 `LIKE '%?%'`：
    /// - `LIKE` 会把用户输入的 `%` / `_` 当通配符，搜 `snake_case` 就等于搜「任意字符」；
    /// - `instr` 是纯子串匹配，没有元字符问题。
    ///
    /// `lower()` 在 SQLite 里只对 ASCII 生效，正好是想要的语义：英文忽略大小写，
    /// 中文不受影响（也没有大小写）。
    ///
    /// 命中的是 `content_json` 而不是正文：正文以 JSON 存在列里，抽出来再比等于把
    /// 整表读进内存。代价是理论上可能命中附件字段里的同名字符串——可接受，因为
    /// 结果列表点进去看到的就是那条消息，不存在「搜到了却看不见」。
    ///
    /// 会话标题由调用方按 `session_id` 另查（命中的会话数受 `limit` 约束，很小）：
    /// 在这里 join 会让 `MESSAGE_COLUMNS` 这份显式列清单失去复用价值。
    pub async fn search_messages(
        &self,
        query: &str,
        session_id: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Message>> {
        let rows = sqlx::query_as::<_, MessageRow>(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM message \
             WHERE (?1 IS NULL OR session_id = ?1) \
               AND instr(lower(content_json), lower(?2)) > 0 \
             ORDER BY created_at DESC, rowid DESC LIMIT ?3 OFFSET ?4"
        ))
        .bind(session_id)
        .bind(query)
        .bind(limit.clamp(1, MAX_LIMIT))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(MessageRow::into_message).collect()
    }

    /// 同一过滤条件下的命中总数（给分页用）。
    pub async fn count_matching_messages(
        &self,
        query: &str,
        session_id: Option<&str>,
    ) -> Result<i64> {
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM message WHERE (?1 IS NULL OR session_id = ?1) \
               AND instr(lower(content_json), lower(?2)) > 0",
        )
        .bind(session_id)
        .bind(query)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }
}
