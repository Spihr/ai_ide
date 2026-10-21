//! 会话仓储：创建 / 读取 / 列表 / 改名 / 删除。

use crate::model::{
    decode_model_ref, encode_model_ref, now_millis, NewSession, Session, SessionQuery,
};
use crate::{Result, Store};

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
