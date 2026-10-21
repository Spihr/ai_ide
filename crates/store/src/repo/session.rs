//! 会话仓储：创建 / 读取 / 列表 / 改名 / 删除。

use std::collections::HashMap;

use crate::model::{
    decode_model_ref, encode_model_ref, now_millis, ForkOutcome, NewSession, Session, SessionQuery,
    MAX_FORK_MESSAGES,
};
use crate::{Result, Store, StoreError};

const SELECT_SESSION: &str =
    "SELECT id, title, workspace_path, model_ref_json, created_at, updated_at FROM session";

/// `session` 表原始行。
#[derive(Debug, sqlx::FromRow)]
struct SessionRow {
    id: String,
    title: String,
    workspace_path: Option<String>,
    model_ref_json: Option<String>,
    created_at: i64,
    updated_at: i64,
}

impl SessionRow {
    fn into_session(self) -> Result<Session> {
        Ok(Session {
            id: self.id,
            title: self.title,
            workspace_path: self.workspace_path,
            model_ref: decode_model_ref(self.model_ref_json)?,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

impl Store {
    /// 新建会话；id 由本层生成（UUID v4）。
    pub async fn create_session(&self, new: NewSession) -> Result<Session> {
        let id = uuid::Uuid::new_v4().to_string();
        let created_at = new.created_at.unwrap_or_else(now_millis);
        let model_ref_json = encode_model_ref(&new.model_ref)?;

        sqlx::query(
            "INSERT INTO session (id, title, workspace_path, model_ref_json, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        )
        .bind(&id)
        .bind(&new.title)
        .bind(new.workspace_path.as_deref())
        .bind(model_ref_json.as_deref())
        .bind(created_at)
        .execute(&self.pool)
        .await?;

        Ok(Session {
            id,
            title: new.title,
            workspace_path: new.workspace_path,
            model_ref: new.model_ref,
            created_at,
            updated_at: created_at,
        })
    }

    /// 按 id 读取会话。
    pub async fn get_session(&self, id: &str) -> Result<Option<Session>> {
        let row = sqlx::query_as::<_, SessionRow>(&format!("{SELECT_SESSION} WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(SessionRow::into_session).transpose()
    }

    /// 列出会话，按 `updated_at` 倒序（同刻按 `created_at`，再按 id 保证稳定）。
    pub async fn list_sessions(&self, query: SessionQuery<'_>) -> Result<Vec<Session>> {
        let limit = query.limit.clamp(1, 500);
        let offset = query.offset.max(0);

        let rows = match query.workspace_path {
            Some(workspace_path) => {
                sqlx::query_as::<_, SessionRow>(&format!(
                    "{SELECT_SESSION} WHERE workspace_path = ? \
                     ORDER BY updated_at DESC, created_at DESC, id ASC LIMIT ? OFFSET ?"
                ))
                .bind(workspace_path)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as::<_, SessionRow>(&format!(
                    "{SELECT_SESSION} ORDER BY updated_at DESC, created_at DESC, id ASC LIMIT ? OFFSET ?"
                ))
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await?
            }
        };

        rows.into_iter().map(SessionRow::into_session).collect()
    }

    /// 会话计数；`workspace_path` 为 `None` 时统计全部。
    pub async fn count_sessions(&self, workspace_path: Option<&str>) -> Result<i64> {
        let count = match workspace_path {
            Some(workspace_path) => {
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM session WHERE workspace_path = ?",
                )
                .bind(workspace_path)
                .fetch_one(&self.pool)
                .await?
            }
            None => {
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM session")
                    .fetch_one(&self.pool)
                    .await?
            }
        };
        Ok(count)
    }

    /// 会话改名；`updated_at` 只增不减（避免回拨的时钟把会话顶到列表末尾）。返回是否命中一行。
    pub async fn rename_session(&self, id: &str, title: &str, updated_at: i64) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE session SET title = ?1, updated_at = MAX(updated_at, ?2) WHERE id = ?3",
        )
        .bind(title)
        .bind(updated_at)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 更新会话默认模型；`updated_at` 同样只增不减。返回是否命中一行。
    ///
    /// 没有「清空模型」分支：`model_ref` 传值一律解释为「改成它」，调用方真要清空
    /// 得走新的显式字段——静默把模型抹掉会让会话突然失去默认模型，比多一条协议
    /// 字段更贵。
    pub async fn set_session_model(
        &self,
        id: &str,
        model_ref: &ai_ide_protocol::ModelRef,
        updated_at: i64,
    ) -> Result<bool> {
        let model_ref_json = encode_model_ref(&Some(model_ref.clone()))?;
        let result = sqlx::query(
            "UPDATE session SET model_ref_json = ?1, updated_at = MAX(updated_at, ?2) WHERE id = ?3",
        )
        .bind(model_ref_json)
        .bind(updated_at)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 把会话及其消息复制成一个新会话，返回新会话与复制后的消息。
    ///
    /// 语义要点：
    /// - `from_message_id` 是**截断点（含）**：只复制到该消息为止；消息必须属于源会话，
    ///   否则报 [`StoreError::MessageNotFound`]——静默当成「复制全部」会让用户拿到远超预期的历史。
    /// - 父子链重映射：父消息也在本批里就换成新 id，被截断在外（或指向外部）就置空，
    ///   新会话不该指向源会话里的消息。
    /// - 消息 `created_at` 原样保留（fork 是复制历史，不是重写历史）；新会话 `created_at`
    ///   取当前时间，`updated_at` 取 `max(now, 最后一条消息时间)`，列表排序才符合直觉。
    /// - 全程一个事务：源会话在读的同时被删掉，也不会留下半截新会话。
    pub async fn fork_session(
        &self,
        source_id: &str,
        from_message_id: Option<&str>,
        title: Option<&str>,
    ) -> Result<ForkOutcome> {
        let mut tx = self.pool.begin().await?;

        let source =
            match sqlx::query_as::<_, SessionRow>(&format!("{SELECT_SESSION} WHERE id = ?"))
                .bind(source_id)
                .fetch_optional(&mut *tx)
                .await?
            {
                Some(row) => row.into_session()?,
                None => return Err(StoreError::SessionNotFound(source_id.to_string())),
            };

        let total =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM message WHERE session_id = ?")
                .bind(source_id)
                .fetch_one(&mut *tx)
                .await?;
        if total > MAX_FORK_MESSAGES {
            return Err(StoreError::SessionTooLarge {
                count: total,
                max: MAX_FORK_MESSAGES,
            });
        }

        let mut messages = crate::repo::message::list_messages_in_tx(&mut tx, source_id).await?;
        if let Some(cut) = from_message_id {
            match messages.iter().position(|message| message.id == cut) {
                Some(index) => messages.truncate(index + 1),
                None => return Err(StoreError::MessageNotFound(cut.to_string())),
            }
        }

        let new_id = uuid::Uuid::new_v4().to_string();
        let created_at = now_millis();
        let updated_at = messages
            .last()
            .map(|message| message.created_at)
            .map_or(created_at, |last| last.max(created_at));
        let title = match title.map(str::trim).filter(|title| !title.is_empty()) {
            Some(title) => title.to_string(),
            None => format!("{}（分支）", source.title),
        };
        let model_ref_json = encode_model_ref(&source.model_ref)?;

        sqlx::query(
            "INSERT INTO session (id, title, workspace_path, model_ref_json, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(&new_id)
        .bind(&title)
        .bind(source.workspace_path.as_deref())
        .bind(model_ref_json.as_deref())
        .bind(created_at)
        .bind(updated_at)
        .execute(&mut *tx)
        .await?;

        // 先把新旧 id 的对应关系一次算完，再逐条写：边写边查父 id 会让插入顺序变成隐含依赖。
        let id_map: HashMap<String, String> = messages
            .iter()
            .map(|message| (message.id.clone(), uuid::Uuid::new_v4().to_string()))
            .collect();

        for message in &messages {
            let new_message_id = id_map[message.id.as_str()].clone();
            let parent_id = message
                .parent_id
                .as_deref()
                .and_then(|parent| id_map.get(parent))
                .map(String::as_str);
            let mut copied = message.clone();
            copied.id = new_message_id.clone();
            copied.session_id = new_id.clone();
            copied.parent_id = parent_id.map(str::to_string);
            crate::repo::message::insert_message_in_tx(
                &mut tx,
                &new_message_id,
                &copied,
                parent_id,
            )
            .await?;
        }

        tx.commit().await?;

        for message in &mut messages {
            let new_message_id = id_map[message.id.as_str()].clone();
            message.parent_id = message
                .parent_id
                .as_deref()
                .and_then(|parent| id_map.get(parent))
                .cloned();
            message.id = new_message_id;
            message.session_id = new_id.clone();
        }

        Ok(ForkOutcome {
            session: Session {
                id: new_id,
                title,
                workspace_path: source.workspace_path,
                model_ref: source.model_ref,
                created_at,
                updated_at,
            },
            messages,
        })
    }

    /// 删除会话及其全部消息（同一事务内先删消息再删会话）。返回会话是否原本存在。
    ///
    /// 说明：SQLite 默认不开 `ON DELETE CASCADE`，这里显式删子表，删除语义不依赖外键配置。
    pub async fn delete_session(&self, id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;

        sqlx::query("DELETE FROM message WHERE session_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;

        let deleted = sqlx::query("DELETE FROM session WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            > 0;

        tx.commit().await?;
        Ok(deleted)
    }
}
