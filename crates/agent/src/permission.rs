//! 工具调用的权限闸门（M4 第三切片）。
//!
//! ## 判定输入与三态输出
//!
//! 判定输入只有三样：**工具名 + 目标路径 + 命令内容**；输出也是三态：
//!
//! - [`Ruling::Allow`]：默认策略直接放行（工作区内的读写工具）。
//! - [`Ruling::Deny`]：默认策略直接拒——越界路径 `1002`、敏感路径 `1003`、
//!   高危命令 `3001`，都复用既有错误语义。与 [`AgentError::is_fatal`] 同口径：
//!   命中就终止循环，不给模型「换个写法再来」的机会。
//! - [`Ruling::Ask`]：其余情况（外部命令、未登记的工具、缺目标路径的写工具）
//!   向上层要一次审批。
//!
//! ## 裁决者与四种动作
//!
//! 上层实现 [`Decider`]——把 [`ApprovalRequest`] 送到审批 UI，拿回一个
//! [`ApprovalReply`]。动作复用协议面的 [`ApprovalDecision`]，**不另立枚举**，
//! 免得两侧漂移成「协议说 allow_once、内核说 allow」：
//!
//! | 动作 | 语义 |
//! |---|---|
//! | `allow_once` | 只放行这一次，同类调用下次还得问 |
//! | `allow_always` | 同工具同范围记一笔，本次进程内不再问；装了 [`AlwaysRecorder`] 则同时落盘，重启仍有效 |
//! | `deny` | 拒绝，理由由闸门补齐 |
//! | `deny_with_reason` | 拒绝，并把原因原文留在错误里回灌给模型自纠 |
//!
//! ## 缺省判拒，绝不默认放行
//!
//! 裁决者缺失、裁决者没答复（`None`）、等待超时——三种情况**一律判拒**。
//! 「审批通道坏了」不等于「用户同意了」，这条默认值写在 [`PermissionGate::authorize`] 里。
//!
//! 本模块只做判定与审批，不自己派发工具：接线点在
//! [`AgentLoop::with_gate`](crate::runner::AgentLoop::with_gate)——循环每次派发前先过
//! 这里的判定，`Deny` 终止循环，`Ask` 被拒 / 超时折成工具错误回灌。

use crate::error::AgentError;
use crate::registry::ToolContext;
use ai_ide_protocol::dto::ApprovalDecision;
use ai_ide_workspace::PathError;
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

/// 等待上层裁决的默认时长。
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(30);

/// 只读工具：目标在工作区内一律放行。
const READ_TOOLS: &[&str] = &["read", "list", "grep"];

/// 写工具：工作区内放行，越界 / 敏感路径照拒。
const WRITE_TOOLS: &[&str] = &["write", "edit", "create", "apply_diff"];

/// 该工具是否会改动磁盘。
///
/// 唯一的判断依据：循环的写盘留痕（`AgentLoop::with_write_hook`）与闸门的
/// 「写工具放行」用的是同一份名单，免得一处新增工具、另一处忘了留快照。
pub(crate) fn is_write_tool(name: &str) -> bool {
    WRITE_TOOLS.contains(&name)
}

/// 跑外部命令的工具：非高危命令一律走审批。
const COMMAND_TOOL: &str = "run_command";

// ---------------------------------------------------------------- 高危命令
//
// 这份黑名单是「危险命令」的唯一事实来源：`tools::run_command` 的起进程前闸门
// 也从这里取（`crate::permission::{dangerous_program, dangerous_pattern}`），
// 免得两处各存一份、改一处漏一处。

/// 危险可执行程序（比对去掉扩展名后的**全等**匹配）。
pub(crate) const BLOCKED_PROGRAMS: &[&str] = &[
    "format", "diskpart", "fdisk", "mkfs", "shutdown", "bcdedit", "cipher", "vssadmin",
];

/// 危险命令行特征（比对「程序名 + 全部参数」拼成的文本的子串）。
pub(crate) const BLOCKED_PATTERNS: &[&str] = &[
    "reg delete",
    "reg add",
    "net user",
    "net localgroup",
    "rm -rf",
    "rm -fr",
    "del /",
    "rd /s",
    "rmdir /s",
    "takeown",
    "icacls",
];

/// 可执行程序名：取路径末段、去掉扩展名、转小写。
pub(crate) fn exe_stem(program: &str) -> String {
    std::path::Path::new(program)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_else(|| program.to_ascii_lowercase())
}

/// 命中危险程序时返回它，否则 `None`。
pub(crate) fn dangerous_program(program: &str) -> Option<&'static str> {
    let name = exe_stem(program);
    BLOCKED_PROGRAMS
        .iter()
        .copied()
        .find(|blocked| name == *blocked)
}

/// 命中危险命令行特征时返回它，否则 `None`。
pub(crate) fn dangerous_pattern(rendered: &str) -> Option<&'static str> {
    let text = rendered.to_ascii_lowercase();
    BLOCKED_PATTERNS
        .iter()
        .copied()
        .find(|pattern| text.contains(*pattern))
}

// ---------------------------------------------------------------- 判定输入

/// 一次待裁决的工具调用。
///
/// 只装判定需要的东西：工具名、目标路径、命令内容——判定不读磁盘、不看工具实现。
#[derive(Debug, Clone)]
pub struct ToolCall<'a> {
    tool: &'a str,
    path: Option<&'a str>,
    program: Option<&'a str>,
    args: Vec<String>,
}

impl<'a> ToolCall<'a> {
    /// 新建：至少要有工具名。
    pub fn new(tool: &'a str) -> Self {
        Self {
            tool,
            path: None,
            program: None,
            args: Vec::new(),
        }
    }

    /// 带上目标路径（读写工具的 `path` 参数）。
    #[must_use]
    pub fn path(mut self, path: &'a str) -> Self {
        self.path = Some(path);
        self
    }

    /// 带上命令内容（`run_command` 的 `cmd` + `args`）。
    #[must_use]
    pub fn command(mut self, program: &'a str, args: &[String]) -> Self {
        self.program = Some(program);
        self.args = args.to_vec();
        self
    }

    /// 工具名。
    pub fn tool_name(&self) -> &'a str {
        self.tool
    }

    /// 目标路径（若有）。
    pub fn path_arg(&self) -> Option<&'a str> {
        self.path
    }

    /// 命令的可执行程序（若有）。
    pub fn program(&self) -> Option<&'a str> {
        self.program
    }

    /// 「程序名 + 全部参数」拼成一行，与 `run_command` 的回显口径一致。
    pub fn command_line(&self) -> Option<String> {
        let program = self.program?;
        Some(if self.args.is_empty() {
            program.to_string()
        } else {
            format!("{program} {}", self.args.join(" "))
        })
    }
}

// ---------------------------------------------------------------- 裁决者

/// 向上层请求审批的输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    /// 工具名。
    pub tool: String,
    /// 目标路径（展示口径，与工具参数一致）。
    pub path: Option<String>,
    /// 命令内容（程序 + 参数拼行）。
    pub command: Option<String>,
    /// 为什么要问：给审批界面显示的一句话。
    pub reason: String,
}

/// 裁决者的答复：动作 + 可选原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalReply {
    /// 四个动作之一（复用协议面枚举）。
    pub decision: ApprovalDecision,
    /// 原因原文；`deny_with_reason` 时带，供回灌模型自纠。
    pub reason: Option<String>,
}

impl ApprovalReply {
    /// 只给动作。
    pub fn new(decision: ApprovalDecision) -> Self {
        Self {
            decision,
            reason: None,
        }
    }

    /// 动作 + 原因。
    pub fn with_reason(decision: ApprovalDecision, reason: impl Into<String>) -> Self {
        Self {
            decision,
            reason: Some(reason.into()),
        }
    }
}

/// 可注入的裁决者：外部实现，负责把请求送到上层并拿回一个动作。
///
/// 返回 `None` 表示「没有答复」（通道断了 / 上层放弃）——一律判拒，绝不默认放行。
#[async_trait]
pub trait Decider: Send + Sync {
    /// 请上层裁决一次。
    async fn decide(&self, request: &ApprovalRequest) -> Option<ApprovalReply>;
}

// ---------------------------------------------------------------- 判定结果

/// 默认策略的三态判定结果。
#[derive(Debug)]
pub enum Ruling {
    /// 策略直接放行。
    Allow,
    /// 策略直接拒（越界 `1002` / 敏感 `1003` / 高危命令 `3001`）。
    Deny(AgentError),
    /// 需要一次审批，附带理由。
    Ask(String),
}

/// 本次调用被允许的依据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grant {
    /// 默认策略直接放行。
    Policy,
    /// 命中同工具同范围的 `allow_always` 记录，本次不再询问。
    Cached,
    /// 本次拿到了裁决者的答复。
    Approved(ApprovalReply),
}

// ---------------------------------------------------------------- 免问落盘

/// 一条「总是允许」记录：**范围键 + 工具名 + 作用域**。
///
/// 范围键由 [`scope_key`] 算出，键本身就是去重粒度（`command:cargo` 而非整个
/// `run_command`），落盘与内存用同一把钥匙，重启后不会放大成「全放行」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlwaysRecord {
    /// 范围键，形如 `command:cargo` / `path:edit:src` / `tool:foo`。
    pub key: String,
    /// 工具名，供列表展示与「按工具撤销」。
    pub tool: String,
    /// 作用域：工作区绝对路径或 `global`。
    pub scope: String,
}

/// 免问范围的持久化落点（由上层注入，agent crate 不依赖存储层）。
///
/// 实现方**必须自己保证不阻塞判定**：这里在 `authorize` 的热路径上同步调用，
/// 写库应当转交后台任务（先落内存、再异步写盘），失败只记日志即可——
/// 「记不住」不该让用户当场被拦下。
pub trait AlwaysRecorder: Send + Sync {
    /// 记下一条免问范围（同键覆盖，幂等）。
    fn remember(&self, record: AlwaysRecord);
}

/// 某次调用对应的免问范围键：与 `allow_always` 的去重粒度逐字一致。
///
/// 命令按「程序」、路径按「工具 + 所在目录」、其余按「工具」——与
/// [`scope_of`] 共享同一个函数，杜绝「内存用一个键、落盘用另一个键」。
pub fn scope_key(call: &ToolCall<'_>) -> String {
    scope_of(call)
}

// ---------------------------------------------------------------- 闸门

/// 权限闸门：默认策略 + 可注入裁决者 + `allow_always` 的进程内记忆。
pub struct PermissionGate {
    decider: Option<Box<dyn Decider>>,
    timeout: Duration,
    always: Mutex<HashSet<String>>,
    recorder: Option<Box<dyn AlwaysRecorder>>,
    /// 运行期登记的「只读工具」全名（P3：插件 `risk=read` 工具直通）。
    ///
    /// 与 [`READ_TOOLS`] 分开：那张表是编译期常量，这张表按每次运行的装载状态变化。
    /// 只装只读工具——写工具漏进这里，就等于给了它一条绕开审批的写盘路。
    extra_read: Mutex<HashSet<String>>,
}

impl PermissionGate {
    /// 不带裁决者：只做默认策略判定，凡是 `Ask` 的一律判拒。
    pub fn new() -> Self {
        Self {
            decider: None,
            timeout: DEFAULT_APPROVAL_TIMEOUT,
            always: Mutex::new(HashSet::new()),
            recorder: None,
            extra_read: Mutex::new(HashSet::new()),
        }
    }

    /// 注入裁决者。
    pub fn with_decider(decider: impl Decider + 'static) -> Self {
        Self {
            decider: Some(Box::new(decider)),
            ..Self::new()
        }
    }

    /// 注入免问落盘器：`allow_always` 命中时同步记一笔，重启后仍免问。
    #[must_use]
    pub fn with_recorder(mut self, recorder: impl AlwaysRecorder + 'static) -> Self {
        self.recorder = Some(Box::new(recorder));
        self
    }

    /// 登记一批「按只读处理」的工具全名（P3：插件 `risk=read` 工具）。
    ///
    /// 命中集合的调用走 [`ruling_for_path`] 的只读分支：工作区内放行、越界路径照拒
    /// （闸门的路径校验一步都不省）。调用方必须只传插件声明为 `risk=read` 的工具——`risk=write` 的
    /// 工具留在「未登记 → Ask」分支，审批该问还得问。
    #[must_use]
    pub fn with_extra_read_tools(self, tools: impl IntoIterator<Item = String>) -> Self {
        {
            let mut guard = self
                .extra_read
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            guard.extend(tools);
        }
        self
    }

    /// 当前登记的只读工具（测试与诊断用，按字典序稳定输出）。
    pub fn extra_read_tools(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .extra_read
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .iter()
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// 该工具是否被登记为「按只读处理」。
    fn is_extra_read(&self, tool: &str) -> bool {
        self.extra_read
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .contains(tool)
    }

    /// 灌入历史免问范围（进程启动时从库里读回），返回灌入条数。
    ///
    /// 只接受**范围键**，不重新推导：库里存的就是哪个键，重启后就是哪个键。
    pub fn seed_always(&self, keys: impl IntoIterator<Item = String>) -> usize {
        let mut guard = self.always.lock().unwrap_or_else(|err| err.into_inner());
        let before = guard.len();
        guard.extend(keys);
        guard.len() - before
    }

    /// 当前内存里的免问范围键（测试与诊断用，按字典序稳定输出）。
    pub fn always_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .always
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .iter()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    /// 改等待审批的时长（超时判拒）。
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// 默认策略判定：不发请求、不等答复，纯函数。
    pub fn ruling(&self, ctx: &ToolContext<'_>, call: &ToolCall<'_>) -> Ruling {
        let tool = call.tool_name();

        if tool == COMMAND_TOOL {
            return ruling_for_command(call);
        }
        if READ_TOOLS.contains(&tool) || self.is_extra_read(tool) {
            return ruling_for_path(ctx, call, false);
        }
        if WRITE_TOOLS.contains(&tool) {
            return ruling_for_path(ctx, call, true);
        }
        // 没登记过的工具不擅自放行：策略表里没有它，就让上层决定。
        Ruling::Ask(format!("工具 `{tool}` 不在默认策略表里，需要上层裁决"))
    }

    /// 判定 + 必要时审批。
    ///
    /// `Allow` 直接放行；`Deny` 直接返回致命错误；`Ask` 走「命中 `allow_always`
    /// 记录 → 否则问裁决者」——裁决者缺失或超时**一律判拒**。
    pub async fn authorize(
        &self,
        ctx: &ToolContext<'_>,
        call: &ToolCall<'_>,
    ) -> Result<Grant, AgentError> {
        match self.ruling(ctx, call) {
            Ruling::Allow => Ok(Grant::Policy),
            Ruling::Deny(err) => Err(err),
            Ruling::Ask(reason) => self.ask(ctx, call, reason).await,
        }
    }

    /// 向裁决者要一次答复，并把四种动作落到结果上。
    async fn ask(
        &self,
        ctx: &ToolContext<'_>,
        call: &ToolCall<'_>,
        reason: String,
    ) -> Result<Grant, AgentError> {
        let tool = call.tool_name();
        let scope = scope_of(call);

        // `allow_always` 只在本次进程内生效：命中就免问，重启即失效。
        if self
            .always
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .contains(&scope)
        {
            return Ok(Grant::Cached);
        }

        let request = ApprovalRequest {
            tool: tool.to_string(),
            path: call.path_arg().map(str::to_string),
            command: call.command_line(),
            reason,
        };

        let Some(decider) = self.decider.as_ref() else {
            return Err(refused(
                tool,
                "没有可用的审批通道（裁决者缺失），按拒绝处理".to_string(),
            ));
        };

        let answer = match tokio::time::timeout(self.timeout, decider.decide(&request)).await {
            Ok(answer) => answer,
            Err(_) => {
                return Err(refused(
                    tool,
                    format!(
                        "审批超时（{}ms 内未获答复），按拒绝处理",
                        self.timeout.as_millis()
                    ),
                ))
            }
        };

        match answer {
            // 没答复 = 没许可。
            None => Err(refused(tool, "审批未给出答复，按拒绝处理".to_string())),
            Some(reply) => match reply.decision {
                // 只放行这一次：不落任何记录，下次照样问。
                ApprovalDecision::AllowOnce => Ok(Grant::Approved(reply)),
                // 同工具同范围记住，进程内不再问。
                ApprovalDecision::AllowAlways => {
                    self.always
                        .lock()
                        .unwrap_or_else(|err| err.into_inner())
                        .insert(scope.clone());
                    // 落盘失败由实现方吞掉：记住不了只是「下次再问」，不该让这次调用失败。
                    if let Some(recorder) = self.recorder.as_ref() {
                        recorder.remember(AlwaysRecord {
                            key: scope,
                            tool: tool.to_string(),
                            scope: ctx.root.to_string_lossy().replace('\\', "/"),
                        });
                    }
                    Ok(Grant::Approved(reply))
                }
                ApprovalDecision::Deny => Err(refused(tool, "审批被拒绝（deny）".to_string())),
                // 原因原文留在错误里，回灌给模型自纠。
                ApprovalDecision::DenyWithReason => {
                    let reason = reply.reason.as_deref().unwrap_or("未附原因");
                    Err(refused(
                        tool,
                        format!("审批被拒绝（deny_with_reason）：{reason}"),
                    ))
                }
            },
        }
    }
}

impl Default for PermissionGate {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------- 策略细则

/// 路径类工具：解析目标路径，越界 / 敏感照拒，落在工作区内放行。
fn ruling_for_path(ctx: &ToolContext<'_>, call: &ToolCall<'_>, for_write: bool) -> Ruling {
    let tool = call.tool_name();
    // 空串按「没给路径」算：工具层本来就是这么读参数的（`Args::opt_str` 把空串当缺省，
    // 必填的 `Args::str` 把空串判成可回灌的「不能为空」）。闸门若照字面把 `""` 递给
    // `PathGuard::resolve`，会以「路径为空」判成 `1006` 致命拒绝——那本是给「指向文件的
    // 路径」定的规矩，结果模型一句 `list` 带 `path: ""`（真实模型表达「列一下工作区根」
    // 的常见写法）就能把整条运行打停，与工具自己声明的「缺省为根目录」也对不上。
    let Some(path) = call.path_arg().filter(|input| !input.trim().is_empty()) else {
        // 无目标路径：只读工具按工作区根算（放行），写工具缺目标只能问上层。
        return if for_write {
            Ruling::Ask(format!("`{tool}` 没给目标路径，需要上层裁决"))
        } else {
            Ruling::Allow
        };
    };

    let resolved = if for_write {
        ctx.guard.resolve_for_write(path)
    } else {
        ctx.guard.resolve(path)
    };

    match resolved {
        Ok(abs) if ctx.guard.contains(&abs) => Ruling::Allow,
        // `resolve` 已保证界内；真出现「解析成功却在界外」，保守判拒。
        Ok(abs) => Ruling::Deny(AgentError::Path(PathError::OutOfWorkspace {
            path: abs.display().to_string(),
        })),
        Err(err) => Ruling::Deny(AgentError::Path(err)),
    }
}

/// 命令类工具：高危命令直接拒（`3001`），其余交给上层审批。
fn ruling_for_command(call: &ToolCall<'_>) -> Ruling {
    let Some(program) = call.program() else {
        return Ruling::Ask("命令内容缺失（没有可执行程序名），需要上层裁决".to_string());
    };

    if let Some(blocked) = dangerous_program(program) {
        return Ruling::Deny(AgentError::CommandDenied {
            tool: COMMAND_TOOL.to_string(),
            reason: format!("`{blocked}` 属于禁止执行的程序"),
        });
    }

    let rendered = call.command_line().unwrap_or_default();
    if let Some(pattern) = dangerous_pattern(&rendered) {
        return Ruling::Deny(AgentError::CommandDenied {
            tool: COMMAND_TOOL.to_string(),
            reason: format!("命令行特征 `{pattern}` 属于禁止执行的操作"),
        });
    }

    Ruling::Ask(format!("命令 `{rendered}` 需要上层审批"))
}

/// `allow_always` 的粒度：命令按「程序」，路径按「工具 + 所在目录」，其余按「工具」。
///
/// 纯字符串处理、不碰磁盘：范围只是「免问的范围」，真正的边界仍由路径闸门判定。
fn scope_of(call: &ToolCall<'_>) -> String {
    if let Some(program) = call.program() {
        return format!("command:{}", exe_stem(program));
    }
    if let Some(path) = call.path_arg() {
        return format!("path:{}:{}", call.tool_name(), parent_dir(path));
    }
    format!("tool:{}", call.tool_name())
}

/// 取路径的父目录（`/` 归一，纯字符串）。
fn parent_dir(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    match normalized.rfind('/') {
        Some(0) => "/".to_string(),
        Some(index) => normalized[..index].to_string(),
        None => ".".to_string(),
    }
}

/// 统一的拒绝构造：走 `3001 TOOL_NOT_ALLOWED`（`docs/03` §1.3 记作「权限策略为 deny」）。
fn refused(tool: &str, reason: String) -> AgentError {
    AgentError::CommandDenied {
        tool: tool.to_string(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Sandbox;
    use ai_ide_protocol::ErrorCode;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// 会记账的假裁决者：记下问了几次、问了什么，按预设答复。
    #[derive(Clone)]
    struct StubDecider {
        calls: Arc<AtomicUsize>,
        seen: Arc<Mutex<Vec<ApprovalRequest>>>,
        reply: Arc<Mutex<Option<ApprovalReply>>>,
        delay: Duration,
    }

    impl StubDecider {
        fn answering(reply: ApprovalReply) -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                seen: Arc::new(Mutex::new(Vec::new())),
                reply: Arc::new(Mutex::new(Some(reply))),
                delay: Duration::ZERO,
            }
        }

        /// 有答复，但拖到超时之后。
        fn stalling(delay: Duration) -> Self {
            let mut stub = Self::answering(ApprovalReply::new(ApprovalDecision::AllowOnce));
            stub.delay = delay;
            stub
        }

        /// 压根不答复。
        fn silent() -> Self {
            let mut stub = Self::answering(ApprovalReply::new(ApprovalDecision::AllowOnce));
            stub.reply = Arc::new(Mutex::new(None));
            stub
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn last_request(&self) -> ApprovalRequest {
            self.seen
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .last()
                .cloned()
                .expect("至少问过一次")
        }
    }

    #[async_trait]
    impl Decider for StubDecider {
        async fn decide(&self, request: &ApprovalRequest) -> Option<ApprovalReply> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .push(request.clone());
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            self.reply
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .clone()
        }
    }

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    // ---------------------------------------------------------- 三态判定

    #[test]
    fn read_and_write_inside_the_workspace_are_allowed_by_policy() {
        let sandbox = Sandbox::new("perm-inside");
        let ctx = sandbox.ctx();
        let gate = PermissionGate::new();

        for call in [
            ToolCall::new("read").path("a.txt"),
            ToolCall::new("list").path("src"),
            ToolCall::new("grep").path("src/a.rs"),
            ToolCall::new("write").path("src/a.rs"),
            ToolCall::new("edit").path("src/a.rs"),
            ToolCall::new("create").path("src/new.rs"),
            ToolCall::new("apply_diff").path("src/a.rs"),
        ] {
            assert!(
                matches!(gate.ruling(&ctx, &call), Ruling::Allow),
                "工作区内的 `{}` 应放行",
                call.tool_name()
            );
        }

        // 只读工具不给路径 = 工作区根，照样放行。
        assert!(matches!(
            gate.ruling(&ctx, &ToolCall::new("list")),
            Ruling::Allow
        ));
    }

    #[test]
    fn blank_path_is_treated_as_no_path() {
        let sandbox = Sandbox::new("perm-blank");
        let ctx = sandbox.ctx();
        let gate = PermissionGate::new();

        // 真实模型表达「列一下工作区根」时常显式写 `path: ""`。空串按「没给路径」算：
        // 只读工具放行（= 工作区根），写工具照旧要上层裁决——两者都不该被判成致命
        // `1006`（那会把整条运行打停，而工具层其实把空串当缺省 / 参数错误）。
        for blank in ["", "   "] {
            assert!(
                matches!(
                    gate.ruling(&ctx, &ToolCall::new("list").path(blank)),
                    Ruling::Allow
                ),
                "只读工具的空路径应放行"
            );
            assert!(
                matches!(
                    gate.ruling(&ctx, &ToolCall::new("write").path(blank)),
                    Ruling::Ask(_)
                ),
                "写工具的空路径仍要问上层"
            );
        }
    }

    #[test]
    fn out_of_workspace_and_sensitive_paths_are_denied() {
        let sandbox = Sandbox::new("perm-escape");
        let ctx = sandbox.ctx();
        let gate = PermissionGate::new();

        let out = gate.ruling(
            &ctx,
            &ToolCall::new("write").path("../../Windows/System32/a.dll"),
        );
        let Ruling::Deny(err) = out else {
            panic!("越界写必须直接拒");
        };
        assert!(err.is_fatal());
        assert_eq!(err.code(), ErrorCode::PATH_OUT_OF_WORKSPACE);
        assert_eq!(err.code().as_i32(), 1002);

        std::fs::create_dir_all(sandbox.path(".git")).unwrap();
        let sensitive = gate.ruling(&ctx, &ToolCall::new("write").path(".git/config"));
        let Ruling::Deny(err) = sensitive else {
            panic!("敏感路径写必须直接拒");
        };
        assert!(err.is_fatal());
        assert_eq!(err.code(), ErrorCode::PATH_DENIED);
        assert_eq!(err.code().as_i32(), 1003);
    }

    #[test]
    fn extra_read_tools_from_plugins_are_allowed_and_others_still_ask() {
        let sandbox = Sandbox::new("perm-plugin-read");
        let ctx = sandbox.ctx();
        let read_tool = "plugin__demo__peek";
        let write_tool = "plugin__demo__poke";
        let gate = PermissionGate::new().with_extra_read_tools([read_tool.to_string()]);

        assert_eq!(gate.extra_read_tools(), vec![read_tool.to_string()]);
        // 登记过的只读工具：工作区内直接放行，与内置 `read` 同口径。
        assert!(matches!(
            gate.ruling(&ctx, &ToolCall::new(read_tool).path("src/a.rs")),
            Ruling::Allow
        ));
        // 没给路径的插件只读工具按工作区根处理，也放行。
        assert!(matches!(
            gate.ruling(&ctx, &ToolCall::new(read_tool)),
            Ruling::Allow
        ));

        // 写工具不进集合：还是既有「未登记 → Ask」，审批不能被插件绕过。
        assert!(matches!(
            gate.ruling(&ctx, &ToolCall::new(write_tool).path("src/a.rs")),
            Ruling::Ask(_)
        ));

        // 只读直通不等于免路径校验：越界路径照旧直接拒（与内置 `read` 同一套 guard）。
        let out = gate.ruling(&ctx, &ToolCall::new(read_tool).path("../../etc/passwd"));
        let Ruling::Deny(err) = out else {
            panic!("插件只读工具的越界路径必须直接拒");
        };
        assert_eq!(err.code(), ErrorCode::PATH_OUT_OF_WORKSPACE);
    }

    #[test]
    fn extra_read_tools_are_opt_in_per_run() {
        let sandbox = Sandbox::new("perm-plugin-read-off");
        let ctx = sandbox.ctx();
        let call = ToolCall::new("plugin__demo__peek").path("src/a.rs");

        // 没登记（例如本轮没有插件）：未登记工具照旧问上层，绝不默认放行。
        assert!(matches!(
            PermissionGate::new().ruling(&ctx, &call),
            Ruling::Ask(_)
        ));
        assert!(PermissionGate::new().extra_read_tools().is_empty());
    }

    #[test]
    fn dangerous_commands_are_denied_and_the_rest_asks() {
        let sandbox = Sandbox::new("perm-command");
        let ctx = sandbox.ctx();
        let gate = PermissionGate::new();

        let cases = [
            (
                ToolCall::new("run_command").command("format", &argv(&["C:"])),
                "禁止执行的程序",
            ),
            (
                ToolCall::new("run_command")
                    .command("reg", &argv(&["delete", "HKLM\\Software\\X", "/f"])),
                "reg delete",
            ),
            (
                ToolCall::new("run_command").command("rm", &argv(&["-rf", "/"])),
                "rm -rf",
            ),
        ];
        for (call, marker) in cases {
            let Ruling::Deny(err) = gate.ruling(&ctx, &call) else {
                panic!("高危命令必须直接拒");
            };
            assert!(err.is_fatal());
            assert_eq!(err.code(), ErrorCode::TOOL_NOT_ALLOWED);
            assert_eq!(err.code().as_i32(), 3001);
            assert!(err.to_string().contains(marker), "{err}");
        }

        let plain = ToolCall::new("run_command").command("cargo", &argv(&["test"]));
        match gate.ruling(&ctx, &plain) {
            Ruling::Ask(reason) => assert!(reason.contains("cargo test"), "{reason}"),
            other => panic!("普通命令应走审批，实际 {other:?}"),
        }
    }

    #[test]
    fn unregistered_tools_and_pathless_writes_ask() {
        let sandbox = Sandbox::new("perm-ask");
        let ctx = sandbox.ctx();
        let gate = PermissionGate::new();

        assert!(matches!(
            gate.ruling(&ctx, &ToolCall::new("web_search")),
            Ruling::Ask(_)
        ));
        assert!(matches!(
            gate.ruling(&ctx, &ToolCall::new("write")),
            Ruling::Ask(_)
        ));
        assert!(matches!(
            gate.ruling(&ctx, &ToolCall::new("run_command")),
            Ruling::Ask(_)
        ));
    }

    // ---------------------------------------------------------- 四种动作

    #[tokio::test]
    async fn allow_once_grants_only_this_call() {
        let sandbox = Sandbox::new("perm-once");
        let ctx = sandbox.ctx();
        let stub = StubDecider::answering(ApprovalReply::new(ApprovalDecision::AllowOnce));
        let gate = PermissionGate::with_decider(stub.clone());
        let call = ToolCall::new("run_command").command("cargo", &argv(&["test"]));

        assert_eq!(
            gate.authorize(&ctx, &call).await.unwrap(),
            Grant::Approved(ApprovalReply::new(ApprovalDecision::AllowOnce))
        );
        // 第二次照样问：allow_once 不落记录。
        assert!(gate.authorize(&ctx, &call).await.is_ok());
        assert_eq!(stub.calls(), 2, "allow_once 不该被复用");
    }

    /// 会记账的假落盘器：验证「同一把钥匙」与作用域，不碰真库。
    #[derive(Clone, Default)]
    struct StubRecorder {
        saved: Arc<Mutex<Vec<AlwaysRecord>>>,
    }

    impl StubRecorder {
        fn records(&self) -> Vec<AlwaysRecord> {
            self.saved
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .clone()
        }
    }

    impl AlwaysRecorder for StubRecorder {
        fn remember(&self, record: AlwaysRecord) {
            self.saved
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .push(record);
        }
    }

    #[tokio::test]
    async fn allow_always_is_persisted_with_the_same_key_as_memory() {
        let sandbox = Sandbox::new("perm-persist");
        let ctx = sandbox.ctx();
        let stub = StubDecider::answering(ApprovalReply::new(ApprovalDecision::AllowAlways));
        let recorder = StubRecorder::default();
        let gate = PermissionGate::with_decider(stub).with_recorder(recorder.clone());

        let call = ToolCall::new("run_command").command("cargo", &argv(&["test"]));
        gate.authorize(&ctx, &call).await.expect("放行");

        let records = recorder.records();
        assert_eq!(records.len(), 1, "allow_always 必须落一笔");
        assert_eq!(records[0].tool, "run_command");
        assert_eq!(
            records[0].key,
            gate.always_keys()[0],
            "落盘的键必须与内存里的免问键逐字一致"
        );
        assert_eq!(records[0].key, "command:cargo", "命令按程序记范围");
        assert!(!records[0].scope.is_empty(), "作用域要记工作区");
    }

    #[tokio::test]
    async fn allow_once_never_reaches_the_recorder() {
        let sandbox = Sandbox::new("perm-once");
        let ctx = sandbox.ctx();
        let stub = StubDecider::answering(ApprovalReply::new(ApprovalDecision::AllowOnce));
        let recorder = StubRecorder::default();
        let gate = PermissionGate::with_decider(stub).with_recorder(recorder.clone());

        let call = ToolCall::new("run_command").command("cargo", &argv(&["test"]));
        gate.authorize(&ctx, &call).await.expect("放行一次");
        assert!(recorder.records().is_empty(), "allow_once 不该落盘");
        assert!(gate.always_keys().is_empty(), "allow_once 不该进免问集合");
    }

    #[tokio::test]
    async fn seeded_rules_skip_the_question_after_restart() {
        let sandbox = Sandbox::new("perm-seed");
        let ctx = sandbox.ctx();
        let stub = StubDecider::answering(ApprovalReply::new(ApprovalDecision::Deny));
        let gate = PermissionGate::with_decider(stub.clone());

        let call = ToolCall::new("run_command").command("cargo", &argv(&["test"]));
        // 重启后：库里读回这把钥匙，灌进新闸门。
        assert_eq!(gate.seed_always(vec!["command:cargo".to_string()]), 1);
        gate.authorize(&ctx, &call)
            .await
            .expect("灌入的免问范围应生效");
        assert_eq!(stub.calls(), 0, "命中历史免问范围就不该再问");
        assert_eq!(gate.always_keys(), vec!["command:cargo".to_string()]);

        // 再灌一遍是幂等的，不重复计数。
        assert_eq!(gate.seed_always(vec!["command:cargo".to_string()]), 0);
        // 别的程序仍要问。
        let other = ToolCall::new("run_command").command("git", &argv(&["status"]));
        assert!(gate.authorize(&ctx, &other).await.is_err());
        assert_eq!(stub.calls(), 1);
    }

    #[tokio::test]
    async fn allow_always_is_reused_within_the_process() {
        let sandbox = Sandbox::new("perm-always");
        let ctx = sandbox.ctx();
        let stub = StubDecider::answering(ApprovalReply::new(ApprovalDecision::AllowAlways));
        let gate = PermissionGate::with_decider(stub.clone());

        // 命令类按「程序」记范围：工作区内的读写走默认策略、根本不会问，所以拿命令做样本。
        let first = ToolCall::new("run_command").command("cargo", &argv(&["test"]));
        assert_eq!(
            gate.authorize(&ctx, &first).await.unwrap(),
            Grant::Approved(ApprovalReply::new(ApprovalDecision::AllowAlways))
        );

        // 同工具同范围：免问。
        let same_scope = ToolCall::new("run_command").command("cargo", &argv(&["build"]));
        assert_eq!(
            gate.authorize(&ctx, &same_scope).await.unwrap(),
            Grant::Cached
        );
        assert_eq!(stub.calls(), 1, "同工具同范围不该重复问");

        // 换了程序就是另一个范围，还得问。
        let other_scope = ToolCall::new("run_command").command("git", &argv(&["status"]));
        assert!(matches!(
            gate.authorize(&ctx, &other_scope).await.unwrap(),
            Grant::Approved(_)
        ));
        assert_eq!(stub.calls(), 2, "换范围要重新问");

        // 路径类按「工具 + 所在目录」记范围。
        let first_path = ToolCall::new("web_search").path("src/a.rs");
        assert!(matches!(
            gate.authorize(&ctx, &first_path).await.unwrap(),
            Grant::Approved(_)
        ));
        let same_dir = ToolCall::new("web_search").path("src/b.rs");
        assert_eq!(
            gate.authorize(&ctx, &same_dir).await.unwrap(),
            Grant::Cached
        );
        assert_eq!(stub.calls(), 3, "同工具同目录不该重复问");

        let other_dir = ToolCall::new("web_search").path("tests/c.rs");
        assert!(matches!(
            gate.authorize(&ctx, &other_dir).await.unwrap(),
            Grant::Approved(_)
        ));
        assert_eq!(stub.calls(), 4, "换目录要重新问");
    }

    #[tokio::test]
    async fn deny_is_refused_with_3001() {
        let sandbox = Sandbox::new("perm-deny");
        let ctx = sandbox.ctx();
        let stub = StubDecider::answering(ApprovalReply::new(ApprovalDecision::Deny));
        let gate = PermissionGate::with_decider(stub);
        let call = ToolCall::new("run_command").command("cargo", &argv(&["test"]));

        let err = gate.authorize(&ctx, &call).await.unwrap_err();
        assert!(err.is_fatal(), "被拒就该终止循环，而不是回灌重试");
        assert_eq!(err.code(), ErrorCode::TOOL_NOT_ALLOWED);
        assert!(err.to_string().contains("deny"), "{err}");
    }

    #[tokio::test]
    async fn deny_with_reason_keeps_the_reason_for_feedback() {
        let sandbox = Sandbox::new("perm-deny-reason");
        let ctx = sandbox.ctx();
        let stub = StubDecider::answering(ApprovalReply::with_reason(
            ApprovalDecision::DenyWithReason,
            "这个脚本会删掉构建产物，先别跑",
        ));
        let gate = PermissionGate::with_decider(stub);
        let call = ToolCall::new("run_command").command("cargo", &argv(&["clean"]));

        let err = gate.authorize(&ctx, &call).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::TOOL_NOT_ALLOWED);
        assert!(
            err.to_string().contains("这个脚本会删掉构建产物，先别跑"),
            "原因原文要留着回灌：{err}"
        );
    }

    // ---------------------------------------------------------- 缺省判拒

    #[tokio::test]
    async fn missing_decider_is_refused() {
        let sandbox = Sandbox::new("perm-nodecider");
        let ctx = sandbox.ctx();
        let gate = PermissionGate::new();
        let call = ToolCall::new("run_command").command("cargo", &argv(&["test"]));

        let err = gate.authorize(&ctx, &call).await.unwrap_err();
        assert!(err.is_fatal());
        assert_eq!(err.code(), ErrorCode::TOOL_NOT_ALLOWED);
        assert!(err.to_string().contains("裁决者缺失"), "{err}");

        // 策略放行的路径不受影响：没有裁决者也照样能读。
        assert_eq!(
            gate.authorize(&ctx, &ToolCall::new("read").path("a.txt"))
                .await
                .unwrap(),
            Grant::Policy
        );
    }

    #[tokio::test]
    async fn silent_decider_is_refused() {
        let sandbox = Sandbox::new("perm-silent");
        let ctx = sandbox.ctx();
        let gate = PermissionGate::with_decider(StubDecider::silent());
        let call = ToolCall::new("run_command").command("cargo", &argv(&["test"]));

        let err = gate.authorize(&ctx, &call).await.unwrap_err();
        assert!(err.to_string().contains("未给出答复"), "{err}");
    }

    #[tokio::test]
    async fn timing_out_is_refused() {
        let sandbox = Sandbox::new("perm-timeout");
        let ctx = sandbox.ctx();
        let stub = StubDecider::stalling(Duration::from_millis(500));
        let gate = PermissionGate::with_decider(stub).with_timeout(Duration::from_millis(20));
        let call = ToolCall::new("run_command").command("cargo", &argv(&["test"]));

        let err = gate.authorize(&ctx, &call).await.unwrap_err();
        assert!(err.is_fatal());
        assert!(err.to_string().contains("超时"), "{err}");
    }

    // ---------------------------------------------------------- 请求内容

    #[tokio::test]
    async fn the_request_carries_tool_path_and_command() {
        let sandbox = Sandbox::new("perm-request");
        let ctx = sandbox.ctx();
        let stub = StubDecider::answering(ApprovalReply::new(ApprovalDecision::Deny));
        let gate = PermissionGate::with_decider(stub.clone());

        let call = ToolCall::new("web_search").path("src/a.rs");
        let _ = gate.authorize(&ctx, &call).await;
        let request = stub.last_request();
        assert_eq!(request.tool, "web_search");
        assert_eq!(request.path.as_deref(), Some("src/a.rs"));
        assert_eq!(request.command, None);
        assert!(
            request.reason.contains("不在默认策略表里"),
            "{}",
            request.reason
        );

        let call = ToolCall::new("run_command").command("git", &argv(&["status", "--short"]));
        let _ = gate.authorize(&ctx, &call).await;
        let request = stub.last_request();
        assert_eq!(request.tool, "run_command");
        assert_eq!(request.path, None);
        assert_eq!(request.command.as_deref(), Some("git status --short"));
    }

    #[test]
    fn command_line_matches_the_run_command_rendering() {
        let call =
            ToolCall::new("run_command").command("cargo", &argv(&["test", "-p", "ai-ide-agent"]));
        assert_eq!(
            call.command_line().as_deref(),
            Some("cargo test -p ai-ide-agent")
        );

        let bare = ToolCall::new("run_command").command("cargo", &[]);
        assert_eq!(bare.command_line().as_deref(), Some("cargo"));
        assert_eq!(ToolCall::new("read").command_line(), None);
    }

    #[test]
    fn scope_is_coarse_but_not_blind() {
        assert_eq!(
            scope_of(&ToolCall::new("write").path("src/a.rs")),
            "path:write:src"
        );
        assert_eq!(
            scope_of(&ToolCall::new("write").path("src\\deep\\a.rs")),
            "path:write:src/deep"
        );
        assert_eq!(
            scope_of(&ToolCall::new("write").path("a.rs")),
            "path:write:."
        );
        assert_eq!(
            scope_of(&ToolCall::new("run_command").command("C:\\tools\\Node.EXE", &argv(&["-v"]))),
            "command:node"
        );
        assert_eq!(scope_of(&ToolCall::new("list")), "tool:list");
    }
}
