//! 连接与迁移：`Store` 是 SQLite 连接池的封装，构造时自动跑完 `migrations/`。

use std::path::Path;
use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;

use crate::Result;

/// 编译期内嵌的迁移集合（读取 `crates/store/migrations/`，不依赖运行时文件）。
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// SQLite 连接池封装。
///
/// 所有读写接口都是本类型的固有方法，按职责分在 [`crate::repo::session`] /
/// [`crate::repo::message`] 两个模块里。
#[derive(Debug, Clone)]
pub struct Store {
    /// 对 crate 内其他模块（`repo::*`）开放，对外只给 [`Store::pool`] 只读引用。
    pub(crate) pool: SqlitePool,
}

impl Store {
    /// 打开（必要时创建）磁盘上的 SQLite 库，并执行迁移。
    ///
    /// - 目录不存在时自动创建；
    /// - 打开 `WAL` + `synchronous = NORMAL`（IDE 场景下写入频繁、单机使用）；
    /// - 开启外键约束，`message.session_id` 的引用会被实际检查。
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5));

        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;

        Self::from_pool(pool).await
    }

    /// 打开进程内内存库，并执行迁移（测试与试运行用）。
    ///
    /// 内存库按连接隔离，因此这里把池锁成 **单连接**，保证迁移建的表在同一连接上可见。
    pub async fn open_in_memory() -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(options)
            .await?;

        Self::from_pool(pool).await
    }

    /// 用现成的连接池构造 `Store` 并执行迁移。
    pub async fn from_pool(pool: SqlitePool) -> Result<Self> {
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    /// 执行迁移（幂等；已应用过的版本会被跳过）。
    pub async fn migrate(&self) -> Result<()> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    /// 已应用的迁移 `(version, description)`，按版本升序。
    pub async fn applied_migrations(&self) -> Result<Vec<(i64, String)>> {
        let rows = sqlx::query_as::<_, (i64, String)>(
            "SELECT version, description FROM _sqlx_migrations ORDER BY version",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 底层连接池（供后续切片做事务组合或 FTS 扩展）。
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// 关闭连接池，等待在途查询结束。
    pub async fn close(&self) {
        self.pool.close().await;
    }
}
