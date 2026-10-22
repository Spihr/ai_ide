//! 权限规则仓储：`allow_always` 的落库、读取与撤销。
//!
//! ## 为什么主键是「范围键」而不是随机 id
//!
//! `crates/agent` 判定 `Ask` 时会算出一个范围键（`command:cargo` /
//! `path:edit:src/lib` / `tool:foo`），进程内用它去重。这里**直接把同一个键当主键**：
//!
//! - 重启后 `SELECT id` 灌回 [`PermissionGate`] 的免问集合，粒度与内存态逐字一致，
//!   不会出现「内存里是 cargo、落盘后变成 run_command 全放行」这种放大；
//! - 重复点「总是允许」是幂等 upsert，不会攒出一堆同义行。
//!
//! ## 撤销是刚需
//!
//! 免问一旦落盘，用户就必须能收回——否则「同意过一次」等于「永久同意」，
//! 治理上是不可接受的。因此这里提供按 id 撤销与按工具批量撤销两条路径，
//! 内核侧 `permission.list` / `permission.revoke` 直接复用。

use crate::model::{now_millis, NewPermissionRule, PermissionRule};
use crate::{Result, Store};

/// 单次列出的条数上限：免问规则是人工攒出来的，几十条已属异常，不需要分页。
pub const MAX_PERMISSION_RULES: i64 = 200;

impl Store {
    /// 记下一条免问范围（同 id 重复写入按最新时间覆盖，幂等）。
    pub async fn remember_permission(&self, rule: NewPermissionRule<'_>) -> Result<PermissionRule> {
        let created_at = rule.created_at.unwrap_or_else(now_millis);
        sqlx::query(
            "INSERT INTO permission_rule (id, tool_pattern, scope, policy, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(id) DO UPDATE SET \
                 tool_pattern = excluded.tool_pattern, \
                 scope = excluded.scope, \
                 policy = excluded.policy, \
                 created_at = excluded.created_at",
        )
        .bind(rule.id)
        .bind(rule.tool_pattern)
        .bind(rule.scope)
        .bind(rule.policy)
        .bind(created_at)
        .execute(&self.pool)
        .await?;

        Ok(PermissionRule {
            id: rule.id.to_string(),
            tool_pattern: rule.tool_pattern.to_string(),
            scope: rule.scope.to_string(),
            policy: rule.policy.to_string(),
            created_at,
        })
    }

    /// 列出免问范围，按时间倒序。
    ///
    /// `scope` 为 `None` 时不过滤；给值时按「该工作区 + `global`」一并返回——
    /// 全局规则在工作区视图里也必须看得见，否则用户找不到它去撤销。
    pub async fn list_permissions(
        &self,
        scope: Option<&str>,
        limit: i64,
    ) -> Result<Vec<PermissionRule>> {
        let limit = limit.clamp(1, MAX_PERMISSION_RULES);
        let rows = match scope {
            Some(scope) => {
                sqlx::query_as::<_, PermissionRule>(
                    "SELECT id, tool_pattern, scope, policy, created_at FROM permission_rule \
                     WHERE scope = ?1 OR scope = 'global' \
                     ORDER BY created_at DESC, id DESC LIMIT ?2",
                )
                .bind(scope)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as::<_, PermissionRule>(
                    "SELECT id, tool_pattern, scope, policy, created_at FROM permission_rule \
                     ORDER BY created_at DESC, id DESC LIMIT ?1",
                )
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows)
    }

    /// 数一数免问规则有多少条（不受 [`MAX_PERMISSION_RULES`] 截断）。
    ///
    /// 列表接口有上限，`total` 必须单独数——否则规则攒过 200 条后分页会「数不准」，
    /// UI 上的「还有更多」就永远说不明白。
    pub async fn count_permissions(&self, scope: Option<&str>) -> Result<u64> {
        let count: (i64,) = match scope {
            Some(scope) => {
                sqlx::query_as(
                    "SELECT COUNT(*) FROM permission_rule WHERE scope = ?1 OR scope = 'global'",
                )
                .bind(scope)
                .fetch_one(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as("SELECT COUNT(*) FROM permission_rule")
                    .fetch_one(&self.pool)
                    .await?
            }
        };
        Ok(count.0.max(0) as u64)
    }

    /// 列出所有免问范围的**键**（启动时灌回闸门用，只要 id）。
    pub async fn permission_ids(&self, scope: Option<&str>) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = match scope {
            Some(scope) => {
                sqlx::query_as(
                    "SELECT id FROM permission_rule WHERE scope = ?1 OR scope = 'global' \
                     ORDER BY created_at DESC",
                )
                .bind(scope)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as("SELECT id FROM permission_rule ORDER BY created_at DESC")
                    .fetch_all(&self.pool)
                    .await?
            }
        };
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// 撤销一条免问范围，返回是否真的删掉了。
    pub async fn revoke_permission(&self, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM permission_rule WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 按工具撤销全部免问范围，返回删除条数。
    pub async fn revoke_permissions_of_tool(&self, tool: &str) -> Result<u64> {
        let result = sqlx::query("DELETE FROM permission_rule WHERE tool_pattern = ?1")
            .bind(tool)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// 清空全部免问范围，返回删除条数。
    pub async fn clear_permissions(&self) -> Result<u64> {
        let result = sqlx::query("DELETE FROM permission_rule")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> Store {
        Store::open_in_memory().await.expect("内存库")
    }

    fn rule<'a>(id: &'a str, tool: &'a str, scope: &'a str) -> NewPermissionRule<'a> {
        NewPermissionRule {
            id,
            tool_pattern: tool,
            scope,
            policy: "allow",
            created_at: Some(1_700_000_000_000),
        }
    }

    #[tokio::test]
    async fn remember_is_idempotent_and_lists_newest_first() {
        let store = store().await;
        store
            .remember_permission(rule("path:edit:src", "edit", "/ws"))
            .await
            .expect("写入");
        store
            .remember_permission(NewPermissionRule {
                created_at: Some(1_700_000_001_000),
                ..rule("command:cargo", "run_command", "/ws")
            })
            .await
            .expect("写入");
        // 同一键再写一次：只更新时间，不新增行。
        store
            .remember_permission(NewPermissionRule {
                created_at: Some(1_700_000_002_000),
                ..rule("command:cargo", "run_command", "/ws")
            })
            .await
            .expect("覆盖");

        let all = store.list_permissions(None, 50).await.expect("列表");
        assert_eq!(all.len(), 2, "同键 upsert 不应新增行：{all:?}");
        assert_eq!(all[0].id, "command:cargo", "最近同意的排最前");
        assert_eq!(all[0].created_at, 1_700_000_002_000);
    }

    #[tokio::test]
    async fn scope_filter_keeps_global_visible() {
        let store = store().await;
        store
            .remember_permission(rule("tool:foo", "foo", "global"))
            .await
            .expect("写入");
        store
            .remember_permission(rule("path:edit:/other", "edit", "/other"))
            .await
            .expect("写入");

        let mine = store.list_permissions(Some("/ws"), 50).await.expect("列表");
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].id, "tool:foo", "全局规则在工作区视图里也要看得见");
    }

    #[tokio::test]
    async fn permission_ids_only_returns_keys() {
        let store = store().await;
        store
            .remember_permission(rule("command:cargo", "run_command", "/ws"))
            .await
            .expect("写入");
        let ids = store.permission_ids(None).await.expect("键");
        assert_eq!(ids, vec!["command:cargo".to_string()]);
    }

    #[tokio::test]
    async fn revoke_one_by_id() {
        let store = store().await;
        store
            .remember_permission(rule("command:cargo", "run_command", "/ws"))
            .await
            .expect("写入");

        assert!(store
            .revoke_permission("command:cargo")
            .await
            .expect("撤销"));
        assert!(
            !store
                .revoke_permission("command:cargo")
                .await
                .expect("再撤"),
            "已删过的键应返回 false"
        );
        assert!(store
            .list_permissions(None, 50)
            .await
            .expect("列表")
            .is_empty());
    }

    #[tokio::test]
    async fn revoke_by_tool_and_clear() {
        let store = store().await;
        for id in ["path:edit:a", "path:edit:b"] {
            store
                .remember_permission(rule(id, "edit", "/ws"))
                .await
                .expect("写入");
        }
        store
            .remember_permission(rule("command:cargo", "run_command", "/ws"))
            .await
            .expect("写入");

        assert_eq!(
            store
                .revoke_permissions_of_tool("edit")
                .await
                .expect("按工具撤"),
            2
        );
        assert_eq!(store.clear_permissions().await.expect("清空"), 1);
        assert!(store
            .list_permissions(None, 50)
            .await
            .expect("列表")
            .is_empty());
    }

    #[tokio::test]
    async fn count_ignores_limit_clamp() {
        let store = store().await;
        for index in 0..3 {
            store
                .remember_permission(NewPermissionRule {
                    created_at: Some(1_700_000_000_000 + index),
                    ..rule(&format!("tool:t{index}"), "t", "/ws")
                })
                .await
                .expect("写入");
        }
        assert_eq!(store.count_permissions(None).await.expect("计数"), 3);
        assert_eq!(
            store.list_permissions(None, 2).await.expect("列表").len(),
            2,
            "列表按 limit 截断"
        );
    }

    #[tokio::test]
    async fn limit_is_clamped() {
        let store = store().await;
        for index in 0..5 {
            store
                .remember_permission(NewPermissionRule {
                    created_at: Some(1_700_000_000_000 + index),
                    ..rule(&format!("tool:t{index}"), "t", "/ws")
                })
                .await
                .expect("写入");
        }
        let rows = store.list_permissions(None, 0).await.expect("列表");
        assert_eq!(rows.len(), 1, "limit 0 应被夹到 1");
    }
}
