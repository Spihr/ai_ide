//! 插件账本仓储：安装记录与能力授权的读写。
//!
//! ## 为什么账本和能力分两张表
//!
//! `plugin` 一行一个插件（列表页一次查完），`plugin_capability` 一行一个能力。
//! 能力如果塞进一个 JSON 列，「撤销 `fs.write`」就得读整串、改一个元素、再写回去——
//! 两次并发授权会互相覆盖，而授权是**逐项点**出来的，最不适合整串读写。
//!
//! ## 为什么写账本要在一个事务里
//!
//! 「更新插件行 + 重置能力行」是同一个事实的两半。中途失败会留下「版本是 v1.1、
//! 能力还是 v1.0 的」这种行——它看起来完全正常，却会让 `needs_grant` 判定失准
//! （v1.1 新加的 `fs.write` 被当成早就授权过）。所以两者必须原子落地。
//!
//! ## 授权的默认值
//!
//! 新出现的申请一律 `granted = 0`：能力是**用户点出来**的，不是安装动作自带的。
//! 升级时保留旧能力原有的授权位，只把新增项置为未授权——这正是 docs/14 §5.4
//! 要的「只有新增能力才重新征询」。

use crate::model::{now_millis, plugin_status, NewPlugin, PluginCapability, PluginRecord};
use crate::{Result, Store};

/// 单次列出的插件条数上限。插件是人工装的，几十个已属异常，不需要分页。
pub const MAX_PLUGINS: i64 = 500;

impl Store {
    /// 写入/覆盖一行插件账本，并**整体替换**它的能力行（同一事务内）。
    ///
    /// `capabilities` 是 `(能力名, 是否已授权)`：调用方先读旧授权、算出新集合，
    /// 再整体交回来。这里不做「合并」——合并策略属于业务（§5.4），
    /// 放在仓储层会让「为什么这个能力被保留了」变得难以追查。
    pub async fn save_plugin(
        &self,
        plugin: NewPlugin<'_>,
        capabilities: &[(&str, bool)],
    ) -> Result<PluginRecord> {
        let now = now_millis();
        let installed_at = plugin.installed_at.unwrap_or(now);
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            "INSERT INTO plugin (id, name, version, description, plugin_api, main, install_dir, \
                 manifest_json, capabilities_json, commands_json, subscriptions_json, tool_count, \
                 status, last_error, restarts, installed_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, NULL, 0, ?14, ?15) \
             ON CONFLICT(id) DO UPDATE SET \
                 name = excluded.name, \
                 version = excluded.version, \
                 description = excluded.description, \
                 plugin_api = excluded.plugin_api, \
                 main = excluded.main, \
                 install_dir = excluded.install_dir, \
                 manifest_json = excluded.manifest_json, \
                 capabilities_json = excluded.capabilities_json, \
                 commands_json = excluded.commands_json, \
                 subscriptions_json = excluded.subscriptions_json, \
                 tool_count = excluded.tool_count, \
                 status = excluded.status, \
                 last_error = NULL, \
                 restarts = 0, \
                 updated_at = excluded.updated_at",
        )
        .bind(plugin.id)
        .bind(plugin.name)
        .bind(plugin.version)
        .bind(plugin.description)
        .bind(plugin.plugin_api)
        .bind(plugin.main)
        .bind(plugin.install_dir)
        .bind(plugin.manifest_json)
        .bind(plugin.capabilities_json)
        .bind(plugin.commands_json)
        .bind(plugin.subscriptions_json)
        .bind(plugin.tool_count)
        .bind(plugin.status)
        .bind(installed_at)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        // 先清后插：能力集合可能缩小（新版删掉了某个能力），留着旧行会让
        // 「这个插件申请了什么」与清单不一致。
        sqlx::query("DELETE FROM plugin_capability WHERE plugin_id = ?1")
            .bind(plugin.id)
            .execute(&mut *tx)
            .await?;

        for (capability, granted) in capabilities {
            sqlx::query(
                "INSERT INTO plugin_capability (plugin_id, capability, granted, granted_at) \
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(plugin.id)
            .bind(*capability)
            .bind(i64::from(*granted))
            .bind(if *granted { Some(now) } else { None })
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        // 回读一次而不是手工拼：`installed_at` 在覆盖安装时要保留旧值，
        // 拼返回值就得再查一遍，不如直接读。
        self.get_plugin(plugin.id)
            .await?
            .ok_or_else(|| crate::StoreError::PluginNotFound(plugin.id.to_string()))
    }

    /// 按 id 取一行账本。
    pub async fn get_plugin(&self, id: &str) -> Result<Option<PluginRecord>> {
        let row = sqlx::query_as::<_, PluginRecord>(
            "SELECT id, name, version, description, plugin_api, main, install_dir, manifest_json, \
                 capabilities_json, commands_json, subscriptions_json, tool_count, status, \
                 last_error, restarts, installed_at, updated_at \
             FROM plugin WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// 列出全部插件，按 id 升序（面板顺序要稳定，不能随写入顺序漂）。
    pub async fn list_plugins(&self) -> Result<Vec<PluginRecord>> {
        let rows = sqlx::query_as::<_, PluginRecord>(
            "SELECT id, name, version, description, plugin_api, main, install_dir, manifest_json, \
                 capabilities_json, commands_json, subscriptions_json, tool_count, status, \
                 last_error, restarts, installed_at, updated_at \
             FROM plugin ORDER BY id ASC LIMIT ?1",
        )
        .bind(MAX_PLUGINS)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 数一数装了多少插件（不受 [`MAX_PLUGINS`] 截断）。
    pub async fn count_plugins(&self) -> Result<u64> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM plugin")
            .fetch_one(&self.pool)
            .await?;
        Ok(count.max(0) as u64)
    }

    /// 删除一行账本（能力行由外键级联删掉）。返回是否真的删掉了。
    pub async fn delete_plugin(&self, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM plugin WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 取某个插件的全部能力行，按能力名升序。
    pub async fn list_plugin_capabilities(&self, plugin_id: &str) -> Result<Vec<PluginCapability>> {
        let rows = sqlx::query_as::<_, PluginCapability>(
            "SELECT plugin_id, capability, granted, granted_at FROM plugin_capability \
             WHERE plugin_id = ?1 ORDER BY capability ASC",
        )
        .bind(plugin_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 已授予的能力名（`needs_grant` 判定的数据来源）。
    pub async fn granted_capabilities(&self, plugin_id: &str) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT capability FROM plugin_capability \
             WHERE plugin_id = ?1 AND granted = 1 ORDER BY capability ASC",
        )
        .bind(plugin_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(capability,)| capability).collect())
    }

    /// 单个能力的授权位（`plugin.grant` / `plugin.revoke` 用，P2 接线）。
    ///
    /// 返回 `None` 表示该插件根本没申请这个能力——与「申请了但没授权」区分开，
    /// 否则调用方会把拼错的能力名当成「撤销成功」。
    pub async fn set_plugin_capability(
        &self,
        plugin_id: &str,
        capability: &str,
        granted: bool,
    ) -> Result<Option<PluginCapability>> {
        let result = sqlx::query(
            "UPDATE plugin_capability SET granted = ?3, granted_at = ?4 \
             WHERE plugin_id = ?1 AND capability = ?2",
        )
        .bind(plugin_id)
        .bind(capability)
        .bind(i64::from(granted))
        .bind(if granted { Some(now_millis()) } else { None })
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Ok(None);
        }

        // 授权位变了，聚合状态要跟着变：全部授权齐了才算 `installed`，
        // 缺一项就退回 `needs_grant`。把这一步留在仓储层是有意的——
        // 任何改授权位的路径都得同步状态，漏改一处就会出现「状态说齐了、能力没齐」。
        self.refresh_grant_status(plugin_id).await?;

        let rows = self.list_plugin_capabilities(plugin_id).await?;
        Ok(rows.into_iter().find(|row| row.capability == capability))
    }

    /// 按当前授权位重算 `status`（只在 `installed` / `needs_grant` 之间迁移）。
    ///
    /// 不动 `ready` / `crashed`：那是运行态，由加载器负责；授权变化时把运行中的
    /// 插件强行标成 `needs_grant` 会让面板显示「未运行」而进程还在跑。
    pub async fn refresh_grant_status(&self, plugin_id: &str) -> Result<()> {
        let Some(record) = self.get_plugin(plugin_id).await? else {
            return Ok(());
        };
        if record.status != plugin_status::INSTALLED && record.status != plugin_status::NEEDS_GRANT
        {
            return Ok(());
        }
        let (pending,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM plugin_capability WHERE plugin_id = ?1 AND granted = 0",
        )
        .bind(plugin_id)
        .fetch_one(&self.pool)
        .await?;

        let next = if pending > 0 {
            plugin_status::NEEDS_GRANT
        } else {
            plugin_status::INSTALLED
        };
        if next != record.status {
            sqlx::query("UPDATE plugin SET status = ?2, updated_at = ?3 WHERE id = ?1")
                .bind(plugin_id)
                .bind(next)
                .bind(now_millis())
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// 更新状态与最近错误（P2 的加载器/重启器会用到；P1 只写 `installed` / `needs_grant`）。
    ///
    /// `last_error` 传 `None` 表示「这次成功了，把旧错误清掉」——过期错误留在面板上
    /// 比没有错误更糟。
    pub async fn set_plugin_status(
        &self,
        plugin_id: &str,
        status: &str,
        last_error: Option<&str>,
    ) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE plugin SET status = ?2, last_error = ?3, updated_at = ?4 WHERE id = ?1",
        )
        .bind(plugin_id)
        .bind(status)
        .bind(last_error)
        .bind(now_millis())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 崩溃重启计数 +1 并返回新值（P2 的「最多 3 次」上限靠它）。
    pub async fn bump_plugin_restarts(&self, plugin_id: &str) -> Result<i64> {
        sqlx::query("UPDATE plugin SET restarts = restarts + 1, updated_at = ?2 WHERE id = ?1")
            .bind(plugin_id)
            .bind(now_millis())
            .execute(&self.pool)
            .await?;
        let (restarts,): (i64,) = sqlx::query_as("SELECT restarts FROM plugin WHERE id = ?1")
            .bind(plugin_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(restarts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NewPlugin;

    async fn store() -> Store {
        Store::open_in_memory().await.expect("内存库")
    }

    fn new_plugin<'a>(id: &'a str, version: &'a str, status: &'a str) -> NewPlugin<'a> {
        NewPlugin {
            id,
            name: "演示插件",
            version,
            description: Some("演示"),
            plugin_api: "1.0",
            main: "index.mjs",
            install_dir: "C:/data/plugins/demo",
            manifest_json: "{}",
            capabilities_json: "[]",
            commands_json: "[]",
            subscriptions_json: "[]",
            tool_count: 1,
            status,
            installed_at: Some(1_700_000_000_000),
        }
    }

    #[tokio::test]
    async fn save_plugin_writes_row_and_capabilities() {
        let store = store().await;
        let saved = store
            .save_plugin(
                new_plugin("demo", "1.0.0", plugin_status::NEEDS_GRANT),
                &[("fs.read", true), ("fs.write", false)],
            )
            .await
            .expect("写入");

        assert_eq!(saved.id, "demo");
        assert_eq!(saved.tool_count, 1);
        assert!(saved.needs_grant());
        assert!(!saved.is_ready());

        let caps = store.list_plugin_capabilities("demo").await.expect("能力");
        assert_eq!(caps.len(), 2);
        assert!(caps[0].is_granted(), "fs.read 应已授权");
        assert!(!caps[1].is_granted(), "fs.write 应待授权");
        assert!(caps[1].granted_at.is_none(), "未授权不该有授权时间");
        assert_eq!(
            store.granted_capabilities("demo").await.expect("已授权"),
            vec!["fs.read".to_string()]
        );
    }

    #[tokio::test]
    async fn save_plugin_replaces_capability_set_and_keeps_installed_at() {
        let store = store().await;
        store
            .save_plugin(
                new_plugin("demo", "1.0.0", plugin_status::INSTALLED),
                &[("fs.read", true), ("net.fetch", true)],
            )
            .await
            .expect("首写");

        let updated = store
            .save_plugin(
                new_plugin("demo", "1.1.0", plugin_status::NEEDS_GRANT),
                &[("fs.read", true), ("fs.write", false)],
            )
            .await
            .expect("覆盖");

        assert_eq!(updated.version, "1.1.0");
        assert_eq!(
            updated.installed_at, 1_700_000_000_000,
            "覆盖安装应保留首次安装时间"
        );
        assert!(updated.updated_at >= updated.installed_at);
        let caps = store.list_plugin_capabilities("demo").await.expect("能力");
        assert_eq!(
            caps.iter()
                .map(|row| row.capability.as_str())
                .collect::<Vec<_>>(),
            vec!["fs.read", "fs.write"],
            "缩小的能力集要删掉旧行"
        );
    }

    #[tokio::test]
    async fn save_plugin_resets_error_and_restarts_on_upgrade() {
        let store = store().await;
        store
            .save_plugin(new_plugin("demo", "1.0.0", plugin_status::CRASHED), &[])
            .await
            .expect("首写");
        store
            .set_plugin_status("demo", plugin_status::CRASHED, Some("6102 入口退出"))
            .await
            .expect("写错误");
        store.bump_plugin_restarts("demo").await.expect("计数");

        let upgraded = store
            .save_plugin(new_plugin("demo", "1.1.0", plugin_status::INSTALLED), &[])
            .await
            .expect("升级");
        assert!(upgraded.last_error.is_none(), "升级后旧错误必须清掉");
        assert_eq!(upgraded.restarts, 0, "新版本不该继承旧版本的崩溃次数");
    }

    #[tokio::test]
    async fn list_and_count_are_stable() {
        let store = store().await;
        for id in ["zeta", "alpha"] {
            store
                .save_plugin(new_plugin(id, "1.0.0", plugin_status::INSTALLED), &[])
                .await
                .expect("写入");
        }
        let all = store.list_plugins().await.expect("列表");
        assert_eq!(
            all.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "zeta"],
            "列表按 id 升序"
        );
        assert_eq!(store.count_plugins().await.expect("计数"), 2);
    }

    #[tokio::test]
    async fn delete_plugin_cascades_capabilities() {
        let store = store().await;
        store
            .save_plugin(
                new_plugin("demo", "1.0.0", plugin_status::INSTALLED),
                &[("fs.read", true)],
            )
            .await
            .expect("写入");

        assert!(store.delete_plugin("demo").await.expect("删除"));
        assert!(!store.delete_plugin("demo").await.expect("再删"));
        assert!(store.get_plugin("demo").await.expect("查").is_none());
        assert!(
            store
                .list_plugin_capabilities("demo")
                .await
                .expect("能力")
                .is_empty(),
            "外键级联应删掉能力行"
        );
    }

    #[tokio::test]
    async fn granting_last_capability_flips_status_back_to_installed() {
        let store = store().await;
        store
            .save_plugin(
                new_plugin("demo", "1.0.0", plugin_status::NEEDS_GRANT),
                &[("fs.read", false), ("fs.write", false)],
            )
            .await
            .expect("写入");

        let grant = store
            .set_plugin_capability("demo", "fs.read", true)
            .await
            .expect("授权")
            .expect("能力存在");
        assert!(grant.is_granted());
        assert!(grant.granted_at.is_some());
        assert!(
            store
                .get_plugin("demo")
                .await
                .expect("查")
                .expect("行")
                .needs_grant(),
            "还有一项没授权，状态应仍是 needs_grant"
        );

        store
            .set_plugin_capability("demo", "fs.write", true)
            .await
            .expect("授权");
        let record = store.get_plugin("demo").await.expect("查").expect("行");
        assert_eq!(record.status, plugin_status::INSTALLED);
        assert!(!record.needs_grant());
    }

    #[tokio::test]
    async fn granting_unknown_capability_reports_absent() {
        let store = store().await;
        store
            .save_plugin(new_plugin("demo", "1.0.0", plugin_status::INSTALLED), &[])
            .await
            .expect("写入");
        assert!(
            store
                .set_plugin_capability("demo", "fs.write", true)
                .await
                .expect("授权")
                .is_none(),
            "没申请过的能力应返回 None，而不是伪造一条"
        );
    }

    #[tokio::test]
    async fn refresh_grant_status_never_touches_runtime_states() {
        let store = store().await;
        store
            .save_plugin(
                new_plugin("demo", "1.0.0", plugin_status::READY),
                &[("fs.read", false)],
            )
            .await
            .expect("写入");
        store.refresh_grant_status("demo").await.expect("重算");
        assert_eq!(
            store
                .get_plugin("demo")
                .await
                .expect("查")
                .expect("行")
                .status,
            plugin_status::READY,
            "运行态不该被授权重算覆盖"
        );
    }

    #[tokio::test]
    async fn restarts_counter_increments() {
        let store = store().await;
        store
            .save_plugin(new_plugin("demo", "1.0.0", plugin_status::CRASHED), &[])
            .await
            .expect("写入");
        assert_eq!(store.bump_plugin_restarts("demo").await.expect("一次"), 1);
        assert_eq!(store.bump_plugin_restarts("demo").await.expect("两次"), 2);
    }
}
