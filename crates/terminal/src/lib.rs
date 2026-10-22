//! 终端会话内核（M6 切片一）：多会话、resize、输出事件流。
//!
//! # 职责边界
//!
//! 这里只管「把 PTY 用一个稳定的接口包起来」：建会话、写输入、改尺寸、杀掉、
//! 列清单，以及把输出与退出抬成事件（[`TerminalEvent`]）。协议编解码、JSON-RPC
//! 通知推送、权限判定都在 `crates/core`，本 crate 不依赖协议层，也不认识
//! `terminal.*` 这些方法名。
//!
//! # 为什么用 `portable-pty` 而不是直连 ConPTY
//!
//! Windows 的 ConPTY 不是开个句柄就完事：要 `CreatePseudoConsole`、建两条管道、
//! 手工拼 `STARTUPINFOEX` 的属性列表，还得在句柄生命周期上小心翼翼。这些细节
//! `portable-pty` 已经处理好了，并且同一套接口在 Unix 上落到 `openpty`——一个依赖
//! 换来两平台一套代码，比自己写两套划算。
//!
//! # 输出为什么按字节而不是字符串
//!
//! PTY 面向的是「终端」，不是文本流：`cat` 一个二进制、程序输出半截 UTF-8、
//! ANSI 转义序列跨读取块断开，都是常态。按 `String` 处理会在这些边界上丢字节，
//! 而丢掉的字节在客户端表现为乱码或吞字，几乎无法回溯。所以事件里带的是
//! base64 编码的原始字节。

#![deny(missing_docs)]

mod error;
mod manager;
mod types;

pub use error::clamp_size;
pub use error::TerminalError;
pub use manager::ExitInfo;
pub use manager::TerminalManager;
pub use types::SpawnOptions;
pub use types::TerminalEvent;
pub use types::TerminalInfo;
pub use types::DEFAULT_COLS;
pub use types::DEFAULT_ROWS;
pub use types::MAX_COLS;
pub use types::MAX_ROWS;
pub use types::MIN_COLS;
pub use types::MIN_ROWS;

#[cfg(test)]
mod tests;
