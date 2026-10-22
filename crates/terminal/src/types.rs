//! 终端会话的公开类型：建会话选项、会话视图、输出事件。
//!
//! 这里刻意不出现 `portable_pty` 的任何类型：PTY 是可替换的实现细节
//! （Windows 走 ConPTY，Unix 走 `openpty`），协议面只认「会话 id + 字节流」。

use std::collections::BTreeMap;
use std::path::PathBuf;

/// 缺省列数。与 xterm.js 的默认 80×24 对齐，客户端不传尺寸时不会看起来挤成一团。
pub const DEFAULT_COLS: u16 = 80;
/// 缺省行数。
pub const DEFAULT_ROWS: u16 = 24;
/// 列数下限：ConPTY 在 0 列时会直接建出 1×1 的伪控制台，resize 也修不回来。
pub const MIN_COLS: u16 = 2;
/// 列数上限。窗口再宽也不该让一行超过 500 列——那是异常值，不是用户意图。
pub const MAX_COLS: u16 = 500;
/// 行数下限。
pub const MIN_ROWS: u16 = 1;
/// 行数上限。
pub const MAX_ROWS: u16 = 200;

/// 建一个终端会话的选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnOptions {
    /// 要跑的 shell 可执行文件；`None` 取平台缺省（Windows 取 `%COMSPEC%`、Unix 取 `$SHELL`）。
    pub shell: Option<String>,
    /// 传给 shell 的命令行参数。
    pub args: Vec<String>,
    /// 起始工作目录；`None` 继承内核进程的当前目录。
    pub cwd: Option<PathBuf>,
    /// 追加 / 覆盖的环境变量（在继承来的环境之上）。
    pub env: BTreeMap<String, String>,
    /// 初始列数。
    pub cols: u16,
    /// 初始行数。
    pub rows: u16,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            shell: None,
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
        }
    }
}

impl SpawnOptions {
    /// 指定尺寸的选项（其余取缺省）。
    pub fn with_size(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            ..Self::default()
        }
    }

    /// 夹到合法区间的尺寸：越界的值不该让建会话失败，客户端多半只是量到了
    /// 一个还没布局完的容器。
    pub fn clamped_size(&self) -> (u16, u16) {
        (
            self.cols.clamp(MIN_COLS, MAX_COLS),
            self.rows.clamp(MIN_ROWS, MAX_ROWS),
        )
    }
}

/// 一个终端会话的只读视图（`terminal.create` 与 `terminal.list` 共用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalInfo {
    /// 会话 id（UUID v4，小写带连字符）。
    pub id: String,
    /// 实际启动的可执行文件路径。
    pub shell: String,
    /// 起始工作目录；`None` 表示继承内核进程的目录。
    pub cwd: Option<String>,
    /// 子进程 pid；平台拿不到时为 `None`。
    pub pid: Option<u32>,
    /// 当前列数（`resize` 会更新这里）。
    pub cols: u16,
    /// 当前行数。
    pub rows: u16,
    /// 建会话时刻（Unix 毫秒）。
    pub created_at_ms: i64,
}

/// 内核推给客户端的事件。
///
/// 输出按**原始字节**走 base64：PTY 里可能是半截 UTF-8、ANSI 转义序列，甚至
/// 二进制（`cat` 一个图片）。先解码成 `String` 再推送会在多字节字符跨读取块时
/// 直接丢字节——终端里看到乱码或吞字，这类问题几乎无法回溯。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    /// 有输出可读。
    Data {
        /// 会话 id。
        terminal_id: String,
        /// 原始字节的 base64（标准字母表，带 `=` 填充）。
        data_b64: String,
        /// 解码后的字节数，方便客户端做背压统计而不必先解码。
        bytes: usize,
    },
    /// 子进程已退出（无论是自然结束、崩溃还是被 `terminal.kill` 杀掉）。
    Exit {
        /// 会话 id。
        terminal_id: String,
        /// 退出码；拿不到（如被信号终止）时为 `None`。
        code: Option<i32>,
        /// 终止信号名（Unix）；Windows 恒为 `None`。
        signal: Option<String>,
        /// 退出时刻（Unix 毫秒）。
        at_ms: i64,
    },
}
