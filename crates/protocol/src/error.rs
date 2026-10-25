//! JSON-RPC 2.0 错误对象与错误码表。
//!
//! 错误码分区（见 `docs/03-内核接口协议规范.md` §1.3）：
//!
//! | 区间 | 归属 |
//! |---|---|
//! | `-327xx` | JSON-RPC 2.0 标准错误 |
//! | `1xxx` | 工作区与路径 |
//! | `2xxx` | Provider / 模型 |
//! | `3xxx` | 工具与 Agent 执行 |
//! | `4xxx` | 运行时（取消等） |
//! | `5xxx` | 会话持久化 |
//! | `6001`–`6099` | 终端会话 |
//! | `6101`–`6199` | 插件宿主 |
//! | `7xxx` | Git 操作 |
//! | `8xxx` | MCP 连接与工具 |

use serde::{Deserialize, Serialize};
use std::fmt;
use ts_rs::TS;

/// 错误码。使用 newtype 包住 `i32`，既保住类型安全，又按数字序列化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export)]
pub struct ErrorCode(pub i32);

impl ErrorCode {
    // ---------- JSON-RPC 2.0 标准 ----------
    pub const PARSE_ERROR: Self = Self(-32700);
    pub const INVALID_REQUEST: Self = Self(-32600);
    pub const METHOD_NOT_FOUND: Self = Self(-32601);
    pub const INVALID_PARAMS: Self = Self(-32602);
    pub const INTERNAL_ERROR: Self = Self(-32603);

    // ---------- 1xxx 工作区 ----------
    pub const WORKSPACE_NOT_OPENED: Self = Self(1001);
    pub const PATH_OUT_OF_WORKSPACE: Self = Self(1002);
    pub const PATH_DENIED: Self = Self(1003);
    pub const FILE_NOT_FOUND: Self = Self(1004);
    pub const IO_ERROR: Self = Self(1005);
    pub const INVALID_PATH: Self = Self(1006);

    // ---------- 2xxx 模型 ----------
    pub const PROVIDER_NOT_CONFIGURED: Self = Self(2001);
    pub const PROVIDER_AUTH_FAILED: Self = Self(2002);
    pub const PROVIDER_RATE_LIMITED: Self = Self(2003);
    pub const CONTEXT_OVERFLOW: Self = Self(2004);
    /// 上游不可达、超时或返回了无法解释的响应体（含本地未启用 HTTP 传输的骨架态）。
    pub const PROVIDER_UNAVAILABLE: Self = Self(2005);

    // ---------- 3xxx 工具 ----------
    pub const TOOL_NOT_ALLOWED: Self = Self(3001);
    pub const TOOL_TIMEOUT: Self = Self(3002);
    pub const DIFF_APPLY_CONFLICT: Self = Self(3003);

    // ---------- 4xxx 运行时 ----------
    pub const CANCELLED: Self = Self(4001);
    pub const PROTOCOL_VERSION_MISMATCH: Self = Self(4002);

    // ---------- 5xxx 会话持久化 ----------
    /// 会话不存在：读取 / 追加消息 / 改名 / 删除都可能命中。
    pub const SESSION_NOT_FOUND: Self = Self(5001);
    /// 会话库未就绪：迁移失败或落盘目录不可写。
    pub const STORE_UNAVAILABLE: Self = Self(5002);
    /// 会话库读写失败（SQLite 或 JSON 序列化报错）。
    pub const STORE_ERROR: Self = Self(5003);
    /// 导出落盘失败：路径不可写、父目录建不出来、磁盘满等文件系统侧原因。
    pub const EXPORT_FAILED: Self = Self(5004);
    /// 会话过大：导出 / 分叉单次只处理 10000 条消息，超过就明确报错而不是静默截断。
    pub const SESSION_TOO_LARGE: Self = Self(5005);
    /// 检查点不存在：回滚 / 删除都会命中。
    pub const CHECKPOINT_NOT_FOUND: Self = Self(5006);
    /// 检查点元数据损坏或回滚后回读校验不一致。
    pub const CHECKPOINT_CORRUPT: Self = Self(5007);

    // ---------- 6xxx 终端 ----------
    /// 终端会话不存在：已经退出、或被 `terminal.kill` 收掉了。
    pub const TERMINAL_NOT_FOUND: Self = Self(6001);
    /// PTY 起不来：shell 不存在、cwd 不可用、ConPTY 建不出来。
    pub const TERMINAL_SPAWN_FAILED: Self = Self(6002);
    /// 会话在但这次操作失败：写管道被对端关闭、resize 被拒。
    pub const TERMINAL_IO_ERROR: Self = Self(6003);
    /// 参数不可用：`data` 不是合法 base64、尺寸无法夹取等。
    pub const TERMINAL_INVALID: Self = Self(6004);

    // ---------- 61xx 插件宿主 ----------
    //
    // 这一段**不是** 6xxx 的开头：终端已占住 6001–6004，`crates/protocol` 的测试
    // 有一条撞号断言盯着它。docs/14 §6.6 原先按「6xxx 整段空缺」写成 6001–6009，
    // 那个前提在 M6 接终端时就失效了，因此插件改占 6101–6109。
    //
    // 码值与 `PLUGIN_` 前缀同步：`CAPABILITY_DENIED` / `TOOL_CONFLICT` 这类通用名
    // 在内核里已有近似含义（3001 `TOOL_NOT_ALLOWED`、1003 `PATH_DENIED`），
    // 加前缀是为了让「这是插件闸门拒的」在日志里一眼可辨。

    /// 插件未安装 / 未加载，或宿主尚未接线（P0 的空分支就回这一条）。
    pub const PLUGIN_NOT_FOUND: Self = Self(6101);
    /// 进程不在或刚崩溃：工具已下架，等重启。
    ///
    /// 与 [`Self::PLUGIN_NOT_FOUND`] 必须分开：前者让模型「别编工具名了」，
    /// 后者让它「等它重启」。合成一个码会让模型反复重试同一个不存在的工具。
    pub const PLUGIN_CRASHED: Self = Self(6102);
    /// 能力未授予：清单申请了，但用户没批。
    pub const PLUGIN_CAPABILITY_DENIED: Self = Self(6103);
    /// `pluginApi` 主版本不兼容。
    pub const PLUGIN_API_MISMATCH: Self = Self(6104);
    /// 调用超时（默认 30 s，`timeoutMs` 可放宽到 120 s）；超时即杀进程。
    pub const PLUGIN_TIMEOUT: Self = Self(6105);
    /// 帧非法 / 方法越权 / 握手工具集合不一致。
    pub const PLUGIN_PROTOCOL_VIOLATION: Self = Self(6106);
    /// 清单非法，`data.details[]` 一次报全所有错误。
    pub const PLUGIN_MANIFEST_INVALID: Self = Self(6107);
    /// 工具名与已有工具冲突（含与核心工具冲突）。
    pub const PLUGIN_TOOL_CONFLICT: Self = Self(6108);
    /// 单次调用宿主 API 超过配额（默认 200 次）。
    pub const PLUGIN_QUOTA_EXCEEDED: Self = Self(6109);

    // ---------- 7xxx Git ----------
    /// 机器上没有可用的 `git`：未安装、或 `AI_IDE_GIT` 指向的文件不可执行。
    pub const GIT_UNAVAILABLE: Self = Self(7001);
    /// 工作区不是 Git 仓库（或不在工作树之内）。
    pub const GIT_NOT_A_REPOSITORY: Self = Self(7002);
    /// git 命令本身失败：非零退出，`data.stderr` 带原文。
    pub const GIT_COMMAND_FAILED: Self = Self(7003);
    /// 参数不可用：路径越界 / 绝对路径、上下文行数越界、空提交信息。
    pub const GIT_INVALID_PARAMS: Self = Self(7004);
    /// 没有可提交的变更（暂存区为空且未 `--allow-empty`）。
    pub const GIT_NOTHING_TO_COMMIT: Self = Self(7005);

    // ---------- 8xxx MCP ----------
    /// Server 名不在 `mcp.json` 里：写错了名字，或配置文件根本没装载。
    ///
    /// 与 [`Self::MCP_CONNECT_FAILED`] 分开是因为处置完全不同：这一条要用户改配置，
    /// 重连多少次都一样；那一条往往重试一次就能好。
    pub const MCP_SERVER_UNKNOWN: Self = Self(8001);
    /// 连接建立失败：子进程起不来、握手被拒、HTTP 非 2xx、协议版本不兼容。
    pub const MCP_CONNECT_FAILED: Self = Self(8002);
    /// Server 未连接（从未连、已断开、已被 `mcp.disconnect` 下线）就发起调用。
    pub const MCP_NOT_CONNECTED: Self = Self(8003);
    /// 工具不在该 Server 最近一次 `tools/list` 的结果里——模型编了个不存在的工具名。
    pub const MCP_TOOL_NOT_FOUND: Self = Self(8004);
    /// 调用失败：传输中断、Server 返回 JSON-RPC error、响应不合契约。
    ///
    /// 注意「工具自己执行失败」不走这里：那是 `result.is_error = true` 的正常结果。
    pub const MCP_CALL_FAILED: Self = Self(8005);
    /// 单次请求超时（连接或调用），`data.timeout_ms` 带预算。
    pub const MCP_TIMEOUT: Self = Self(8006);
    /// 参数不可用：命名空间拆不开、`arguments` 不是对象、Server 名非法。
    pub const MCP_INVALID_PARAMS: Self = Self(8007);

    // --- Inline Edit（9xxx）------------------------------------------------
    //
    // 这一族专指「一次选区改写没做成」，且**失败原因分得开**：用户按 Ctrl+K 之后
    // 看到的是弹窗里的一句话，把「选区不属于这个文件」和「模型没配」混成同一个码，
    // 只能让用户自己猜该改什么。上游本身的问题（非 200 / 限流 / 未配置 provider）
    // 不在这里另开码，沿用 2xxx 段——那是模型接入层的既有口径。

    /// 选区越界：行号或列号超出文件范围、`start` 在 `end` 之后、文件内容为空。
    ///
    /// `data` 带 `lines`（文件总行数）与 `selection`，供外壳把选区收拢到合法范围。
    pub const INLINE_EDIT_SELECTION_INVALID: Self = Self(9001);
    /// 无可用模型：`model_ref` 缺失、`model_id` 为空，或 provider 名下没有该模型。
    pub const INLINE_EDIT_MODEL_UNAVAILABLE: Self = Self(9002);
    /// 上游返回的文本里提取不出任何代码（空响应、只有解释没有代码块）。
    ///
    /// 这条**绝不**降级成「返回空替换」：空替换落盘等于悄悄删掉用户的代码。
    pub const INLINE_EDIT_EMPTY_REPLACEMENT: Self = Self(9003);
    /// 目标文件超过读取上限被截断，行号与内容都不再可信，无法安全地算补丁。
    pub const INLINE_EDIT_FILE_TOO_LARGE: Self = Self(9004);

    pub const fn as_i32(self) -> i32 {
        self.0
    }

    /// 是否为 JSON-RPC 标准错误码。
    pub const fn is_standard(self) -> bool {
        self.0 <= -32000
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<i32> for ErrorCode {
    fn from(value: i32) -> Self {
        Self(value)
    }
}

impl From<ErrorCode> for i32 {
    fn from(value: ErrorCode) -> Self {
        value.0
    }
}

/// JSON-RPC 错误对象。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ErrorObject {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub data: Option<serde_json::Value>,
}

impl ErrorObject {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// 附加结构化数据。协议约定常用字段：
    /// - `upstream`：上游模型的原始错误体
    /// - `retry_after_ms`：限流建议等待时长
    /// - `path` / `tokens`：路径越界、上下文超限的上下文信息
    #[must_use]
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::INVALID_PARAMS, message)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            ErrorCode::METHOD_NOT_FOUND,
            format!("未实现的方法: {method}"),
        )
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::INTERNAL_ERROR, message)
    }
}

impl fmt::Display for ErrorObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for ErrorObject {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_serializes_as_number() {
        let json = serde_json::to_string(&ErrorCode::METHOD_NOT_FOUND).unwrap();
        assert_eq!(json, "-32601");
    }

    #[test]
    fn error_object_omits_none_data() {
        let err = ErrorObject::new(ErrorCode::CANCELLED, "cancelled");
        let json = serde_json::to_string(&err).unwrap();
        assert!(!json.contains("data"), "None 的 data 不应被序列化: {json}");
    }

    #[test]
    fn error_object_keeps_data_when_present() {
        let err = ErrorObject::new(ErrorCode::PATH_OUT_OF_WORKSPACE, "越界")
            .with_data(serde_json::json!({ "path": "..\\..\\Windows" }));
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("Windows"));
    }

    #[test]
    fn standard_code_detection() {
        assert!(ErrorCode::PARSE_ERROR.is_standard());
        assert!(!ErrorCode::CANCELLED.is_standard());
    }

    /// 5xxx 是业务码，不能被误判成 JSON-RPC 标准错误（否则客户端会按规范错误处理）。
    #[test]
    fn session_error_codes_are_not_standard() {
        assert!(!ErrorCode::SESSION_NOT_FOUND.is_standard());
        assert!(!ErrorCode::STORE_UNAVAILABLE.is_standard());
        assert!(!ErrorCode::STORE_ERROR.is_standard());
        assert_eq!(ErrorCode::SESSION_NOT_FOUND.as_i32(), 5001);
    }

    /// MCP 错误码独立占 8xxx 段：既不能落进 JSON-RPC 标准码区间，
    /// 也不能和终端占用的 6001-6004 撞号——前端按段分流，撞号会走错提示。
    #[test]
    fn mcp_error_codes_use_their_own_8xxx_band() {
        let codes = [
            ErrorCode::MCP_SERVER_UNKNOWN,
            ErrorCode::MCP_CONNECT_FAILED,
            ErrorCode::MCP_NOT_CONNECTED,
            ErrorCode::MCP_TOOL_NOT_FOUND,
            ErrorCode::MCP_CALL_FAILED,
            ErrorCode::MCP_TIMEOUT,
            ErrorCode::MCP_INVALID_PARAMS,
        ];
        for (index, code) in codes.iter().enumerate() {
            let raw = code.as_i32();
            assert!((8001..=8099).contains(&raw), "{raw} 不在 8xxx 段");
            assert!(!code.is_standard(), "{raw} 不该被当成标准码");
            assert!(
                !(6001..=6004).contains(&raw),
                "{raw} 与终端占用的 6001-6004 撞号"
            );
            assert!(
                !codes[..index].contains(code),
                "{raw} 重复定义，同段内码值必须唯一"
            );
        }
        assert_eq!(ErrorCode::MCP_SERVER_UNKNOWN.as_i32(), 8001);
        assert_eq!(ErrorCode::MCP_INVALID_PARAMS.as_i32(), 8007);
    }

    /// 插件错误码占 61xx：终端已占 6001-6004（上面那条撞号断言盯着），
    /// 所以 docs/14 §6.6 原写的 6001-6009 必须整体挪到 6101-6109。
    ///
    /// 这条测试同时钉住三件事：段位、唯一性、与终端段不重叠。
    #[test]
    fn plugin_error_codes_use_their_own_61xx_band() {
        let codes = [
            ErrorCode::PLUGIN_NOT_FOUND,
            ErrorCode::PLUGIN_CRASHED,
            ErrorCode::PLUGIN_CAPABILITY_DENIED,
            ErrorCode::PLUGIN_API_MISMATCH,
            ErrorCode::PLUGIN_TIMEOUT,
            ErrorCode::PLUGIN_PROTOCOL_VIOLATION,
            ErrorCode::PLUGIN_MANIFEST_INVALID,
            ErrorCode::PLUGIN_TOOL_CONFLICT,
            ErrorCode::PLUGIN_QUOTA_EXCEEDED,
        ];
        for (index, code) in codes.iter().enumerate() {
            let raw = code.as_i32();
            assert!((6101..=6199).contains(&raw), "{raw} 不在 61xx 段");
            assert!(!code.is_standard(), "{raw} 不该被当成标准码");
            assert!(
                !(6001..=6099).contains(&raw),
                "{raw} 落进了终端占用的 6001-6099 段"
            );
            assert!(
                !codes[..index].contains(code),
                "{raw} 重复定义，同段内码值必须唯一"
            );
        }
        assert_eq!(ErrorCode::PLUGIN_NOT_FOUND.as_i32(), 6101);
        assert_eq!(ErrorCode::PLUGIN_QUOTA_EXCEEDED.as_i32(), 6109);
        // 相邻两段不能重叠：终端段最高位 6004 仍在插件段起点 6101 之下。
        assert_eq!(ErrorCode::TERMINAL_NOT_FOUND.as_i32(), 6001);
        assert_ne!(
            ErrorCode::PLUGIN_NOT_FOUND.as_i32(),
            ErrorCode::TERMINAL_NOT_FOUND.as_i32()
        );
    }

    /// Inline Edit 错误码占 9001-9004：这四个码是外壳弹窗的判断依据，
    /// 一旦有人为了「省一个码」把它们合并，用户就再也分不清是选区画错了还是模型没配。
    #[test]
    fn inline_edit_error_codes_use_their_own_9xxx_band() {
        let codes = [
            ErrorCode::INLINE_EDIT_SELECTION_INVALID,
            ErrorCode::INLINE_EDIT_MODEL_UNAVAILABLE,
            ErrorCode::INLINE_EDIT_EMPTY_REPLACEMENT,
            ErrorCode::INLINE_EDIT_FILE_TOO_LARGE,
        ];
        for (index, code) in codes.iter().enumerate() {
            let raw = code.as_i32();
            assert!((9001..=9099).contains(&raw), "{raw} 不在 9xxx 段");
            assert!(!code.is_standard(), "{raw} 不该被当成标准码");
            assert!(
                !codes[..index].contains(code),
                "{raw} 重复定义，同段内码值必须唯一"
            );
        }
        assert_eq!(ErrorCode::INLINE_EDIT_SELECTION_INVALID.as_i32(), 9001);
        assert_eq!(ErrorCode::INLINE_EDIT_FILE_TOO_LARGE.as_i32(), 9004);
        // 9001 与「参数非法 1002」必须分开：一个是「选区画错了」（可修正后重试），
        // 一个是「报文不合协议」（属于调用方 bug），外壳的反应不一样。
        assert_ne!(
            ErrorCode::INLINE_EDIT_SELECTION_INVALID.as_i32(),
            ErrorCode::INVALID_PARAMS.as_i32()
        );
    }
}
