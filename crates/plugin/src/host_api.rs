//! 插件 → 宿主的反向请求出口（插件系统 P3.5：读侧宿主能力）。
//!
//! 这一层只定义**契约**：插件进程发出 `host/*` 请求时，`loader` 把帧交给
//! [`HostApi::call`]，具体怎么查能力、怎么判路径、怎么记审计全在实现方
//! （`crates/core` 的 `FsHostApi`）。
//!
//! 分成两个 crate 是有意的：`crates/plugin` 不认识账本、不认识工作区，它只
//! 负责「把插件的请求转给实现、把实现的结果/错误封回帧」。这样 `crates/plugin`
//! 的单测可以塞一个假实现，不需要搭出一个真工作区。
//!
//! 错误码沿用 docs/14 §8.1 的闸门口径：能力 6103、路径 1002/1003、配额 6109，
//! 参数畸形 -32602、方法不存在 -32601。**顺序不可交换**——先判资格、再判路径、
//! 最后判用量，否则未授权的插件会先撞配额，用户看到的提示会指错方向。

use std::sync::Arc;

use serde_json::Value;

/// 插件反向请求的失败原因。`code` 直接进错误帧的 `error.code`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostApiError {
    pub code: i64,
    pub message: String,
}

impl HostApiError {
    /// 插件没有该能力（docs/14 §8.1 ③）。
    pub const UNAUTHORIZED: i64 = 6103;
    /// 路径跑到工作区外（docs/14 §8.1 ④）。
    pub const PATH_OUT_OF_WORKSPACE: i64 = 1002;
    /// 命中敏感段（`.git` / `.ssh` / `.env` 等）。
    pub const PATH_DENIED: i64 = 1003;
    /// 工作区尚未打开。
    pub const WORKSPACE_NOT_OPENED: i64 = 1001;
    /// 目标文件/目录不存在。
    pub const NOT_FOUND: i64 = 1004;
    /// 读写失败（权限、编码、IO）。
    pub const IO: i64 = 1005;
    /// 单次调用宿主 API 次数超限（docs/14 §8.1 ⑥）。
    pub const QUOTA_EXCEEDED: i64 = 6109;
    /// 审批闸门未放行（docs/14 §8.1 ⑤：`risk=write` 且未 `always`）。
    pub const TOOL_NOT_ALLOWED: i64 = 3001;
    /// 参数畸形（缺字段 / 类型不对）。
    pub const INVALID_PARAMS: i64 = -32602;
    /// 方法不存在（v1 未实现的宿主能力也走它）。
    pub const METHOD_NOT_FOUND: i64 = -32601;

    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 能力未授予。
    pub fn unauthorized(method: &str, capability: &str) -> Self {
        Self::new(
            Self::UNAUTHORIZED,
            format!("插件未获得能力 {capability}，拒绝 {method}"),
        )
    }

    /// 方法不存在 / v1 未实现。
    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            Self::METHOD_NOT_FOUND,
            format!("宿主不提供该方法：{method}"),
        )
    }

    /// 参数畸形。
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(Self::INVALID_PARAMS, message)
    }

    /// 路径越界。
    pub fn path_out_of_workspace(path: &str) -> Self {
        Self::new(
            Self::PATH_OUT_OF_WORKSPACE,
            format!("路径越出工作区：{path}"),
        )
    }

    /// 敏感路径。
    pub fn path_denied(path: &str) -> Self {
        Self::new(Self::PATH_DENIED, format!("拒绝访问敏感路径：{path}"))
    }

    /// 写侧审批未通过。
    ///
    /// 走 `3001 TOOL_NOT_ALLOWED`（docs/14 §8.1 ⑤）：这一码在协议层已经登记为
    /// 「权限策略不放行」，和 Agent 侧拒绝工具调用是同一类，前端不必再认新码。
    pub fn tool_not_allowed(method: &str, scope_key: &str) -> Self {
        Self::new(
            Self::TOOL_NOT_ALLOWED,
            format!("{method} 未获审批：{scope_key}"),
        )
    }
}

/// 宿主能力实现方（P3.5 读侧五方法；P4 起含写侧 `host/fs.write`）。
///
/// `call` 是同步的：读文件、列目录都不长，实现方内部若需要等就更不该把
/// 帧泵（`pump_stdout`）拖成异步——一条慢请求会堵住该插件所有回包。
pub trait HostApi: Send + Sync {
    /// 处理一条插件反向请求。返回 `result`，失败给 [`HostApiError`]。
    fn call(&self, plugin_id: &str, method: &str, params: &Value) -> Result<Value, HostApiError>;

    /// 一次插件工具调用开始时调用，用于清零该插件的配额计数。
    ///
    /// 配额的口径是「**单次工具调用** ≤ 200 次宿主调用」（docs/14 §8.4），
    /// 所以归零必须挂在工具调用边界上，而不是每次 `host/*` 请求。
    /// 默认空实现：不需要配额的实现方不必关心它。
    fn begin_call(&self, plugin_id: &str) {
        let _ = plugin_id;
    }
}

/// 实现方的共享句柄。
pub type SharedHostApi = Arc<dyn HostApi>;
