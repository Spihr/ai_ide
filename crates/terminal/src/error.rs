//! 终端会话的错误类型。
//!
//! 分三类而不是一个大 `anyhow`：`crates/core` 要把它们翻成不同的协议错误码，
//! 客户端据此决定「提示用户重开一个终端」还是「重试一次写操作」。

use crate::types::MAX_COLS;
use crate::types::MAX_ROWS;
use crate::types::MIN_COLS;
use crate::types::MIN_ROWS;

/// 终端子系统的错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TerminalError {
    /// 会话不存在，或已经退出并从登记表里摘掉了。
    #[error("终端会话不存在: {0}")]
    NotFound(String),
    /// 起不来：可执行文件找不到、cwd 不存在、ConPTY 建不出来。
    #[error("终端启动失败: {0}")]
    Spawn(String),
    /// 起来了但这次操作失败：写管道已被对端关闭、resize 被系统拒绝。
    #[error("终端操作失败: {0}")]
    Io(String),
    /// 参数不可用（空 shell、尺寸越界到无法夹取等）。
    #[error("终端参数非法: {0}")]
    Invalid(String),
}

impl TerminalError {
    /// 是否是「会话没了」这一类。`crates/core` 用它映射成 6001。
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound(_))
    }
}

/// 把外部尺寸夹到合法区间。
///
/// 协议侧的 `resize` 与 `create` 共用：客户端在窗口拖拽过程中会连发一串
/// 中间值（含 0），拒绝它们只会让终端抖成一片空白。
pub fn clamp_size(cols: u16, rows: u16) -> (u16, u16) {
    (
        cols.clamp(MIN_COLS, MAX_COLS),
        rows.clamp(MIN_ROWS, MAX_ROWS),
    )
}
