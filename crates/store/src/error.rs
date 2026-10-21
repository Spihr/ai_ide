//! 持久化层错误类型。

/// 存储层统一错误。
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite / 连接池错误。
    #[error("数据库错误：{0}")]
    Database(#[from] sqlx::Error),

    /// 迁移执行失败。
    #[error("迁移失败：{0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    /// JSON 列编解码失败（历史上写入过非法 JSON 时才会出现）。
    #[error("JSON 编解码失败：{0}")]
    Json(#[from] serde_json::Error),

    /// 目录创建等文件系统错误。
    #[error("文件系统错误：{0}")]
    Io(#[from] std::io::Error),

    /// 追加消息时目标会话不存在。
    #[error("会话不存在：{0}")]
    SessionNotFound(String),

    /// `message.role` 列存了未知取值。
    #[error("非法的消息角色：{0}")]
    InvalidRole(String),

    /// fork 的截断点消息不存在，或不属于被分叉的会话。
    #[error("消息不存在或不属于该会话：{0}")]
    MessageNotFound(String),

    /// 单次导出 / 分叉的消息数超过上限：宁可明确报错，也不静默截断历史。
    #[error("消息过多：{count} 条，单次上限 {max} 条")]
    SessionTooLarge { count: i64, max: i64 },
}

/// 持久化层 `Result` 别名。
pub type Result<T> = std::result::Result<T, StoreError>;
