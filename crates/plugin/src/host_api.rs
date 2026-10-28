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

use serde_json::{json, Value};

/// 插件 → 宿主的日志通知方法名（通知帧：没有 `id`，不等回包）。
pub const LOG_METHOD: &str = "host/log";
/// 插件 → 宿主的进度上报方法名（P5 切片四，转 `plugin.progress` 事件）。
pub const PROGRESS_METHOD: &str = "host/progress";
/// 进度上报所需的能力名。
///
/// 命名沿用清单既有口径（`fs.read` / `search` / `workspace.info`）：取方法名去掉
/// `host/` 的那一段，全小写、不加分隔符。授权仍是按插件一套，走 `plugin.grant`。
pub const PROGRESS_CAPABILITY: &str = "progress";
/// 插件 → 宿主的视图刷新提示方法名（docs/24 §4.5，转 `plugin.view` 事件）。
///
/// 通知帧，不等回包：插件只声明「我这个视图的内容变了」，不说变了什么。数据仍由
/// 渲染侧拿 `viewId` 走 `plugin.call` 去取，那条路有闸门与审计。
///
/// **刻意不配能力名**：它既不读也不写，唯一的副作用是在用户眼前把某个已声明的
/// 视图标脏；能造成的最坏情况是多一次 `plugin.call`，而那次调用自身还要过六步
/// 闸门。反过来，如果按 `progress` 那样要求能力，清单里已声明的 `views[].refresh`
/// 就变成「装了但永远刷不了」，反而是更难解释的坑。
pub const VIEW_REFRESH_METHOD: &str = "host/view.refresh";
/// 插件 → 宿主的出站网络方法名（P5 切片五）。
pub const NET_FETCH_METHOD: &str = "host/net.fetch";
/// 出站网络所需的能力名。授权粒度是「按域名」：能力 + 清单 `allowedDomains`
/// 两道都要过，缺一不可（见 docs/14 §7.1）。
pub const NET_FETCH_CAPABILITY: &str = "net.fetch";
/// 插件 → 宿主的模型调用方法名（P5 切片六）。
pub const LLM_CHAT_METHOD: &str = "host/llm.chat";
/// 模型调用所需的能力名。授权粒度是「按模型」：能力 + 清单 `allowedModels` /
/// `maxTokens` 三道都要过（见 docs/14 §7.4）。密钥不下发插件——参数里没有、
/// 回包里也没有任何塞 key 的口子。
pub const LLM_CHAT_CAPABILITY: &str = "llm.chat";

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
    /// 出站网络被拒（P5 切片五）：协议不符、域名不在清单、地址是内网/回环。
    ///
    /// 复用 `6103`（能力未授予）这一码：对插件作者来说「这个走法没被授权」
    /// 才是同一件事，前端也不必再认一个新码；`message` 里写明是哪一道闸门拦的。
    pub const NET_DENIED: i64 = Self::UNAUTHORIZED;
    /// 模型调用被拒（P5 切片六）：模型不在清单 `allowedModels` 内，或清单没声明
    /// `maxTokens`。
    ///
    /// 同样复用 `6103`：对插件作者而言「这个模型没被授权」与「这个能力没被授权」
    /// 是同一类；`message` 里写明是清单哪一条拦的，前端也只需认一个码。
    /// 注意 `maxTokens` **写大了**不归它管——那是 `-32602`：把数字改小就能过，
    /// 属于「这次请求写错了」，不该让用户去改授权。
    pub const LLM_DENIED: i64 = Self::UNAUTHORIZED;
    /// 参数畸形（缺字段 / 类型不对）。
    pub const INVALID_PARAMS: i64 = -32602;
    /// 方法不存在（能力表里查不到这个名字：从未实现，或插件把方法名拼错了）。
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

    /// 方法不存在（能力表里查不到这个名字）。
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

    /// 处理一条插件通知帧（无 `id`，插件不等回包）。
    ///
    /// P5 只开放两个：`host/log` 与 `host/progress`。两者都不是「取数据」的
    /// 通道，而是**旁路遥测**——宿主把它们转成 `plugin.log` / `plugin.progress`
    /// 事件，`pluginId` 由宿主填。区别在闸门：`host/log` 不要能力（写日志没有
    /// 额外信息面），`host/progress` 要 [`PROGRESS_CAPABILITY`]（它会被推给外壳
    /// 窗口，等于一条向用户表态的通道）。默认实现回「方法不存在」。
    fn notify(&self, plugin_id: &str, method: &str, params: &Value) -> Result<(), HostApiError> {
        let _ = (plugin_id, params);
        Err(HostApiError::method_not_found(method))
    }
}

/// 实现方的共享句柄。
pub type SharedHostApi = Arc<dyn HostApi>;

/// 插件侧 SDK 的 `host.progress({ token, message, percent? })` 在宿主这边的等价位：
/// 拼一帧通知参数（**不含 `pluginId`**，那个由宿主盖）。
///
/// SDK 层就把「进度里能带什么」收窄成白名单：只有这三个键会进参数对象，
/// `None` 的键干脆不出现。插件作者即便想顺手捎一段文件内容也没有入口——
/// 想传别的东西得先改这里的代码，改不了就等于传不出去。
pub fn progress_params(token: Option<&str>, message: Option<&str>, percent: Option<f64>) -> Value {
    let mut params = serde_json::Map::new();
    if let Some(token) = token {
        params.insert("token".into(), json!(token));
    }
    if let Some(message) = message {
        params.insert("message".into(), json!(message));
    }
    if let Some(percent) = percent {
        params.insert("percent".into(), json!(percent));
    }
    Value::Object(params)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SDK 封装只产出白名单键：`pluginId` 不能由插件自带（否则可冒充别人）。
    #[test]
    fn progress_params_only_carries_whitelisted_keys() {
        let full = progress_params(Some("42"), Some("扫描 src"), Some(30.0));
        assert_eq!(
            full,
            json!({ "token": "42", "message": "扫描 src", "percent": 30.0 })
        );
        assert!(full.get("pluginId").is_none(), "pluginId 由宿主盖");

        let minimal = progress_params(None, Some("扫描 src"), None);
        assert_eq!(minimal, json!({ "message": "扫描 src" }));

        assert_eq!(progress_params(None, None, None), json!({}));
    }

    /// 默认实现不认遥测：换实现方时不会「悄悄接受」一个没被处理的通知。
    #[test]
    fn default_notify_rejects_everything() {
        struct Bare;
        impl HostApi for Bare {
            fn call(&self, _: &str, _: &str, _: &Value) -> Result<Value, HostApiError> {
                Err(HostApiError::method_not_found("x"))
            }
        }
        let error = Bare
            .notify("demo", PROGRESS_METHOD, &json!({ "message": "x" }))
            .expect_err("默认不接");
        assert_eq!(error.code, HostApiError::METHOD_NOT_FOUND);
    }
}
