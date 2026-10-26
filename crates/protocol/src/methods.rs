//! 方法名常量表。
//!
//! 命名规范：`<域>.<动作>`，域与动作均小写下划线（见 `docs/03-内核接口协议规范.md` §1.2）。
//! 事件（Core → Main 的单向通知）与请求同名空间，以 `methods::event` 分区区分。

/// 内核生命周期。
pub mod core {
    pub const HELLO: &str = "core.hello";
    pub const PING: &str = "core.ping";
    pub const STATS: &str = "core.stats";
    pub const SHUTDOWN: &str = "core.shutdown";
}

/// 取消机制。
pub mod cancel {
    /// 取消某个在途请求（其 `params.id` 指向被取消请求的 id）。
    pub const REQUEST: &str = "$/cancelRequest";
}

/// 工作区与文件系统（M1）。
pub mod workspace {
    pub const OPEN: &str = "workspace.open";
    pub const CLOSE: &str = "workspace.close";
    pub const TREE: &str = "workspace.tree";
    pub const READ_FILE: &str = "workspace.read_file";
    pub const WRITE_FILE: &str = "workspace.write_file";
    pub const CREATE: &str = "workspace.create";
    pub const RENAME: &str = "workspace.rename";
    pub const DELETE: &str = "workspace.delete";
    pub const REVEAL: &str = "workspace.reveal";
}

/// 检索（M1：文件名模糊 + 内容检索；M3 切片一补符号检索）。
pub mod search {
    pub const FILES: &str = "search.files";
    pub const CONTENT: &str = "search.content";
    /// 符号检索：按名字在源码里找函数 / 结构体 / 类 / 枚举 / 接口 / trait / 常量 / 方法。
    ///
    /// 与 [`CONTENT`] 的区别是**语义层级**：内容检索给的是「哪一行的文本里有这个词」，
    /// 符号检索给的是「这个声明定义在哪、覆盖到哪一行」。前端据此做「跳到定义」，
    /// 而不是把命中行当文本展示。
    pub const SYMBOLS: &str = "search.symbols";
}

/// Provider 与模型管理（M2，已在 `crates/core` 接线）。
///
/// 密钥本身不在这层：`provider.upsert` 的 `api_key` 只进内存保管（见 docs/02 §6.3），
/// `provider.list` 只回 `has_key` / `key_hint` 这类脱敏信息。
pub mod provider {
    pub const UPSERT: &str = "provider.upsert";
    pub const LIST: &str = "provider.list";
    pub const DELETE: &str = "provider.delete";
    pub const TEST: &str = "provider.test";
}

/// 模型目录（M2，已在 `crates/core` 接线）。模型挂在 Provider 下，把「厂商」与「具体模型」解耦。
pub mod model {
    pub const UPSERT: &str = "model.upsert";
    pub const LIST: &str = "model.list";
    pub const DELETE: &str = "model.delete";
}

/// 模型接入（M2：流式对话、取消、模型枚举、token 计数）。
pub mod llm {
    pub const CHAT_STREAM: &str = "llm.chat_stream";
    pub const CANCEL: &str = "llm.cancel";
    pub const LIST_MODELS: &str = "llm.list_models";
    pub const COUNT_TOKENS: &str = "llm.count_tokens";
}

/// Inline Edit（M3 切片三：选区改写）。
///
/// 与 `llm.chat_stream` 的分工：那条是「对话」，流式、多轮、由 UI 拼消息；
/// 这条是「编辑器里的一个动作」——给一段选区与一句指令，要回一段可直接落盘的替换文本。
/// 因此它一次调用即返回完整结果，不走 `llm.*` 事件族，也不产生会话消息。
pub mod inline_edit {
    /// 依据指令改写文件中的一段选区，返回替换文本与 unified diff。
    ///
    /// 内核**不写盘**：结果只描述「应该改成什么」，落盘由外壳走 `workspace.write_file`
    /// （带 `expected_hash` 乐观锁）。这样用户按 Esc 放弃时不需要任何撤销逻辑。
    pub const PROPOSE: &str = "inline_edit.propose";
}

/// Agent 运行时（M4：协议面先定稿，`crates/core` 的接线随后落地）。
pub mod agent {
    pub const START: &str = "agent.start";
    pub const STOP: &str = "agent.stop";
    pub const DECIDE: &str = "agent.decide";
}

/// 会话与消息（M5：落盘在 `crates/store`，`crates/core` 只做协议翻译）。
pub mod session {
    pub const CREATE: &str = "session.create";
    pub const LIST: &str = "session.list";
    pub const GET: &str = "session.get";
    pub const UPDATE: &str = "session.update";
    pub const DELETE: &str = "session.delete";
    pub const APPEND_MESSAGE: &str = "session.append_message";
    pub const MESSAGES: &str = "session.messages";
    /// 把会话导出成 Markdown / JSON 并写到指定路径（M5 切片三）。
    pub const EXPORT: &str = "session.export";
    /// 复制会话及其消息生成一个新会话，可截断到指定消息（M5 切片三）。
    pub const FORK: &str = "session.fork";
}

/// 检查点与回滚（M5 切片五）。
///
/// 与 `session.*` 的分工：会话记「说过什么」，检查点记「磁盘上原本是什么」。
/// 快照内容落在 `<data_dir>/checkpoints/` 下的普通文件里，元数据是同目录的
/// `meta.json`——回滚要逐字节还原，把内容塞进 SQLite 只会让「同一份字节被
/// 存两遍」。
pub mod checkpoint {
    /// 给当前工作区拍一个检查点（可指定文件子集）。
    pub const CREATE: &str = "checkpoint.create";
    /// 列出某工作区的检查点（按创建时间倒序）。
    pub const LIST: &str = "checkpoint.list";
    /// 回滚到某个检查点，并回读校验。
    pub const RESTORE: &str = "checkpoint.restore";
    /// 删掉一个检查点（释放快照占用的磁盘）。
    pub const DELETE: &str = "checkpoint.delete";
}

/// 审计日志（M5 切片四：落盘在 `crates/store` 的 `audit_event` 表）。
///
/// 只有查询入口：写入是内核在关键操作处的**旁路动作**，客户端无权伪造一条
/// 「某会话被导出到某路径」的记录——审计记录一旦能让外部写，就不再是证据。
pub mod audit {
    pub const LIST: &str = "audit.list";
}

/// 权限免问规则（M5 切片六：落盘在 `crates/store` 的 `permission_rule` 表）。
///
/// 与 `agent.decide` 的分工：`decide` 是「这一次怎么办」，这里管的是
/// 「点过『总是允许』之后，那些免问范围还在不在、要不要收回」。
///
/// 只有读与撤销两个动作，**没有 `permission.allow`**：能凭空写一条免问规则的
/// 客户端等于拿到了一把万能钥匙——新增免问必须走 `agent.decide` 的
/// `allow_always`，让用户当场看着点。
pub mod permission {
    /// 列出免问范围（按时间倒序）。
    pub const LIST: &str = "permission.list";
    /// 撤销免问范围：按规则的 `id`，或按工具名整批。
    pub const REVOKE: &str = "permission.revoke";
}

/// 终端会话（M6 切片一：PTY 在 `crates/terminal`，`crates/core` 只做协议翻译）。
///
/// 五个动作覆盖一个终端的完整生命周期：`create` 建、`write` 喂输入、`resize`
/// 跟着窗口走、`kill` 主动收、`list` 自查（客户端掉线重连后靠它捡回会话）。
/// 输出不经请求-响应，而是走 [`event::TERMINAL_DATA`] 的单向推送——终端的输出
/// 是持续的字节流，用请求去「拉」只会让谁先谁后全乱。
pub mod terminal {
    /// 新建一个终端会话（返回会话视图）。
    pub const CREATE: &str = "terminal.create";
    /// 往终端写输入（`data` 为 base64 原始字节，客户端据此可发任意控制序列）。
    pub const WRITE: &str = "terminal.write";
    /// 改终端尺寸（列 × 行）。
    pub const RESIZE: &str = "terminal.resize";
    /// 杀掉终端会话（先礼后兵：先请求进程退出，超时再强杀）。
    pub const KILL: &str = "terminal.kill";
    /// 列出当前存活的终端会话。
    pub const LIST: &str = "terminal.list";
}

/// Git 工作区能力（M6 切片三）：状态 / diff / 暂存 / 提交 / 分支。
///
/// 五个方法都以「工作区根」为操作对象，路径参数一律相对工作区根；
/// 事件侧不新增通知——状态变化由客户端在写操作后自行重查，避免内核猜测外部改动。
pub mod git {
    /// 工作区变更清单与分支头信息。
    pub const STATUS: &str = "git.status";
    /// unified diff（单文件或全量、工作区或暂存区）。
    pub const DIFF: &str = "git.diff";
    /// 暂存 / 取消暂存。
    pub const STAGE: &str = "git.stage";
    /// 提交暂存区并返回新提交哈希。
    pub const COMMIT: &str = "git.commit";
    /// 当前分支与分支列表。
    pub const BRANCHES: &str = "git.branches";
}

/// MCP 连接与工具（M7 切片二）。
///
/// 这一域把事情分成「连接管理」与「调用」两半：`servers` / `connect` / `disconnect` /
/// `tools` 面向外壳的服务器面板，`call` 面向要执行工具的一方。注入 agent 的
/// `mcp__<server>__<tool>` 工具最终也落到同一条 `tools/call` 路径上，不另开方法。
///
/// 不新增事件：连接状态变化由发起方（面板）在 `connect` / `disconnect` 的响应里获得。
pub mod mcp {
    /// 已配置 ∪ 已连接的 Server 清单与连接状态。
    pub const SERVERS: &str = "mcp.servers";
    /// 连接一个 `mcp.json` 里的 Server，并拉取它的工具清单。
    pub const CONNECT: &str = "mcp.connect";
    /// 断开一个 Server（幂等），并把它的工具从 agent 注册表下架。
    pub const DISCONNECT: &str = "mcp.disconnect";
    /// 列出已连接 Server 的工具（带 `mcp__<server>__<tool>` 全名）。
    pub const TOOLS: &str = "mcp.tools";
    /// 调用一个 MCP 工具（原始名或全名都接受）。
    pub const CALL: &str = "mcp.call";
}

/// 插件宿主（P0 协议骨架）。
///
/// 只有这 12 个方法属于**内核协议**：它们进 [`IMPLEMENTED`]、进 `core.hello`
/// 的能力清单、进外壳 `policy.ts` 白名单、由 ts-rs 导出类型。目录里的
/// `plugin/*`（宿主 → 插件）与 `host/*`（插件 → 宿主）是插件包内部协议，
/// **不进**这张表——混进来会让能力清单不再是「静态可审计」的（见 docs/14 §6.1）。
///
/// P0 阶段宿主尚未接线：`crates/core/src/dispatch.rs` 有 10 个分支，但每个分支都
/// 如实返回 [`ai_ide_protocol::ErrorCode::PLUGIN_NOT_FOUND`]（6101）。先定义方法名
/// 而不是等实现完再补，是为了让「未实现」表现为明确的业务错误，而不是 `-32601`。
pub mod plugin {
    /// 列已安装插件与状态。
    pub const LIST: &str = "plugin.list";
    /// 单个插件详情（含工具 / 命令清单与最近错误）。
    pub const STATUS: &str = "plugin.status";
    /// 校验并写入插件目录，不启动进程。
    pub const INSTALL: &str = "plugin.install";
    /// 卸载运行中的插件并删目录。
    pub const UNINSTALL: &str = "plugin.uninstall";
    /// 授权齐备则起子进程并握手。
    pub const LOAD: &str = "plugin.load";
    /// 批量并发装载（P5 切片八）：一次请求装多个插件，单个失败不影响其他。
    ///
    /// 与 [`LOAD`] 分开而不是让外壳循环调 `load`：并发上限、部分失败的结果归集、
    /// 「同 id 只装一次」这三件事需要一个**可观测**的执行器来保证，散在渲染端
    /// 循环里既测不到也说不清。单插件语义与 [`LOAD`] 完全一致（共用同一段装载
    /// 实现），差别只在调度。
    pub const LOAD_MANY: &str = "plugin.load_many";
    /// 停进程并把工具从 agent 注册表下架。
    pub const UNLOAD: &str = "plugin.unload";
    /// 停 + 起，用于开发调试。
    pub const RELOAD: &str = "plugin.reload";
    /// 调用网关：按 `plugin_id` + `method` 转发到插件进程。
    pub const CALL: &str = "plugin.call";
    /// 命令网关：按 `plugin_id` + `command` 执行插件声明的命令（命令面板入口）。
    ///
    /// 与 [`CALL`] 分开而不是复用它：命令带 `args`（清单 schema 校验）、有独立
    /// 的审计 kind（`plugin.command`）、`risk=write` 时还要在执行**前**审批，
    /// 语义与「调一个工具」并不相同。两者共用同一套闸门判定（见 core 的
    /// `gate_base` / `gate_declared`），差别只在业务步骤。
    pub const COMMAND: &str = "plugin.command";
    /// 授予能力。
    pub const GRANT: &str = "plugin.grant";
    /// 撤销能力（关键能力则连带 unload）。
    pub const REVOKE: &str = "plugin.revoke";
}

/// 语言服务（LSP，切片二：内核已接线）。
///
/// 五个动作覆盖一条语言服务器连接的完整生命周期：`start` 起进程并握手、`sync`
/// 把编辑缓冲推过去、`request` 发一次查询、`servers` 自查存活、`stop` 收掉。
/// 形状与 [`terminal`] 同构，但有两点不同：
///
/// 1. **没有字节流**：语言服务器的输出是结构化结果（诊断 / 补全 / 悬停），
///    诊断走 [`event::LSP_DIAGNOSTICS`] 单向推送，不需要 `lsp.data` 那种基座。
/// 2. **`stop` 只认 `server_id`**：终端杀会话同理，但这里没有 `pid` 这类入参——
///    进程归内核管，客户端能决定的只有「关哪一条连接」。
///
/// 五个方法整组坐在 [`IMPLEMENTED`] 里（能力数 74 → 79）：`crates/core` 的
/// `dispatch.rs` 有五个真分支，实现在 `lsp_api.rs`，连接池按
/// `workspace_id → server_id` 复用。诊断不走请求-应答，是
/// [`event::LSP_DIAGNOSTICS`] 单向推送。
pub mod lsp {
    /// 为某个文件（或显式 `language_id`）起一条语言服务器连接。
    pub const START: &str = "lsp.start";
    /// 关掉一条连接（`shutdown` → `exit`，超时强杀兜底）。幂等：已关再关不报错。
    pub const STOP: &str = "lsp.stop";
    /// 列当前活着的连接（掉线重连后靠它捡回 `server_id`）。
    pub const SERVERS: &str = "lsp.servers";
    /// 发一次查询：定义 / 引用 / 悬停 / 补全（方法由枚举限定，不收自由字符串）。
    pub const REQUEST: &str = "lsp.request";
    /// 把编辑器里的文本同步给服务器（必要时自动 `didOpen` / `didChange`）。
    pub const SYNC: &str = "lsp.sync";
}

/// Core → Main 的通知（事件）。
pub mod event {
    /// 内核日志转发，便于 Main 侧统一落盘。
    pub const LOG: &str = "core.log";
    /// 工作区内的文件被外部程序改动。
    pub const WORKSPACE_CHANGED: &str = "workspace.changed";
    /// 后台全量索引完成，附带最终规模统计。
    pub const WORKSPACE_INDEXED: &str = "workspace.indexed";
    /// 流式对话开始（M2）。每次 `llm.chat_stream` 恰好一条。
    pub const LLM_STARTED: &str = "llm.started";
    /// 正文增量（M2）。
    pub const LLM_DELTA: &str = "llm.delta";
    /// 思维链增量（M2，仅推理模型有）。
    pub const LLM_REASONING: &str = "llm.reasoning";
    /// 工具调用增量（M2，参数按片段拼接）。
    pub const LLM_TOOL_CALL: &str = "llm.tool_call";
    /// 用量统计（M2）。
    pub const LLM_USAGE: &str = "llm.usage";
    /// 流正常结束（M2）。与 [`LLM_ERROR`] 互斥。
    pub const LLM_FINISHED: &str = "llm.finished";
    /// 流异常终止（M2）。与 [`LLM_FINISHED`] 互斥。
    pub const LLM_ERROR: &str = "llm.error";
    /// Provider 目录发生变化（M2），供设置界面刷新。
    pub const PROVIDER_CHANGED: &str = "provider.changed";

    /// Agent 需要用户裁决某次工具调用（M4）。
    pub const AGENT_APPROVAL_REQUIRED: &str = "agent.approval_required";
    /// Agent 一次运行结束（M4），成功 / 失败 / 被取消都走它。
    pub const AGENT_FINISHED: &str = "agent.finished";

    /// 终端有输出可读（M6），`data` 为 base64 原始字节。
    pub const TERMINAL_DATA: &str = "terminal.data";
    /// 终端子进程退出（M6）：自然结束、崩溃、被 `terminal.kill` 杀掉都走它。
    pub const TERMINAL_EXIT: &str = "terminal.exit";

    /// 语言服务器推来一批诊断（切片一）。
    ///
    /// **整篇替换，不做增量**：服务器每次 `publishDiagnostics` 给的都是某文件的
    /// 全量清单，按增量下发会让外壳自己维护「上一条还在不在」，一旦漏掉一条
    /// 就永久残留一个红点。空数组表示「这个文件现在没毛病了」，必须转发。
    pub const LSP_DIAGNOSTICS: &str = "lsp.diagnostics";
}

/// 内核**真正接线并可路由**的方法全集，用于单测与能力声明。
///
/// 注意：事件（通知）不计入能力清单——能力清单描述的是「可被请求的方法」。
///
/// 这里的「已实现」有严格含义：`crates/core` 的 `dispatch.rs` 里有对应分支，
/// 调用不会返回 `-32601`。协议里**定义了但还没接线**的方法见
/// [`DECLARED_NOT_IMPLEMENTED`]。
///
/// M4 第三切片的 `agent.*` 曾按「先登记、后接线」暂时留在这里：协议与 DTO 先行
/// 定稿，`crates/core` 的 dispatch 分支随后落地（见 `crates/core/src/dispatch.rs`
/// 的三处 `methods::agent::*` 分支与 `crates/core/src/agent_api.rs`）。接线已完成，
/// 这三项与其余方法同等——报成能力就能真路由。
pub const IMPLEMENTED: &[&str] = &[
    core::HELLO,
    core::PING,
    core::STATS,
    core::SHUTDOWN,
    cancel::REQUEST,
    workspace::OPEN,
    workspace::CLOSE,
    workspace::TREE,
    workspace::READ_FILE,
    workspace::WRITE_FILE,
    workspace::CREATE,
    workspace::RENAME,
    workspace::DELETE,
    workspace::REVEAL,
    search::FILES,
    search::CONTENT,
    search::SYMBOLS,
    provider::UPSERT,
    provider::LIST,
    provider::DELETE,
    provider::TEST,
    model::UPSERT,
    model::LIST,
    model::DELETE,
    llm::CHAT_STREAM,
    llm::CANCEL,
    llm::LIST_MODELS,
    llm::COUNT_TOKENS,
    // M3 切片三：Inline Edit 内核面（一次调用返回替换文本 + unified diff，不写盘）。
    inline_edit::PROPOSE,
    // M4 第三切片：agent 运行时（协议面与 core 接线均已落地）。
    agent::START,
    agent::STOP,
    agent::DECIDE,
    // M5 切片二 / 三：会话与消息、导出与分叉。
    session::CREATE,
    session::LIST,
    session::GET,
    session::UPDATE,
    session::DELETE,
    session::APPEND_MESSAGE,
    session::MESSAGES,
    // M5 切片三：导出与分叉（`crates/core` 已有 dispatch 分支）。
    session::EXPORT,
    session::FORK,
    // M5 切片四：审计查询（写入由内核在会话关键操作处旁路完成）。
    audit::LIST,
    // M5 切片五：检查点与回滚（快照落文件、元数据落 meta.json）。
    checkpoint::CREATE,
    checkpoint::LIST,
    checkpoint::RESTORE,
    checkpoint::DELETE,
    // M5 切片六：权限免问规则（读取与撤销由 `crates/core` 接线）。
    permission::LIST,
    permission::REVOKE,
    // M6 切片一：终端会话（PTY 在 `crates/terminal`，dispatch 已接线）。
    terminal::CREATE,
    terminal::WRITE,
    terminal::RESIZE,
    terminal::KILL,
    terminal::LIST,
    // M6 切片三：Git 工作区能力（实现在 `crates/git`，dispatch 已接线）。
    git::STATUS,
    git::DIFF,
    git::STAGE,
    git::COMMIT,
    git::BRANCHES,
    // M7 切片二：MCP 连接与工具（实现在 `crates/mcp`，dispatch 已接线）。
    mcp::SERVERS,
    mcp::CONNECT,
    mcp::DISCONNECT,
    mcp::TOOLS,
    mcp::CALL,
    // P0 插件协议骨架：方法名与 DTO 先行，dispatch 分支如实返回 6101。
    // 这不算虚报——能力清单的语义是「可路由」，不是「业务已实现」。
    plugin::LIST,
    plugin::STATUS,
    plugin::INSTALL,
    plugin::UNINSTALL,
    plugin::LOAD,
    // P5 切片八：批量并发装载（调度器在 `crates/core/src/plugin_api.rs`，能力数 79 → 80）。
    plugin::LOAD_MANY,
    plugin::UNLOAD,
    plugin::RELOAD,
    plugin::CALL,
    // P5 切片二：命令网关接线，闸门与 `plugin.call` 同源。
    plugin::COMMAND,
    plugin::GRANT,
    plugin::REVOKE,
    // LSP 切片二：语言服务五个方法接线完成（实现在 `crates/core/src/lsp_api.rs`，
    // 端到端用例在 `crates/core/tests/lsp_flow.rs`）。能力数 74 → 79。
    lsp::START,
    lsp::STOP,
    lsp::SERVERS,
    lsp::REQUEST,
    lsp::SYNC,
];

/// 协议面**已定义、但内核尚未接线**的方法。
///
/// M2 收尾时本表清空过一次；LSP 切片一曾临时装入 `lsp.*` 五项，切片二接线后
/// 再次清空。保留这张表而不是删掉，是因为「先定义方法名、后接线」是这套协议的
/// 既定做法：让「未实现」在协议层就有一等公民的位置，而不是让人误以为名字写错了。
///
/// 表里的名字**不能**出现在 [`IMPLEMENTED`] 里：`IMPLEMENTED` 会经 `core.hello`
/// 回报给 Main 作为能力清单，一旦虚报，Main 会照单调用、然后拿到一片 `-32601`。
/// 接线时把对应常量从本表移入 [`IMPLEMENTED`]，两张表始终互斥。
pub const DECLARED_NOT_IMPLEMENTED: &[&str] = &[];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implemented_list_covers_core_m1_m2_agent_domains() {
        assert_eq!(IMPLEMENTED.len(), 80);
        assert!(IMPLEMENTED.contains(&"core.ping"));
        assert!(IMPLEMENTED.contains(&"$/cancelRequest"));
        assert!(IMPLEMENTED.contains(&"workspace.read_file"));
        assert!(IMPLEMENTED.contains(&"search.content"));
        // M3 切片一：符号检索进清单，与 `search.files` / `search.content` 同族。
        assert!(IMPLEMENTED.contains(&search::SYMBOLS));
        assert!(IMPLEMENTED.contains(&provider::UPSERT));
        assert!(IMPLEMENTED.contains(&model::DELETE));
        // M2 收尾：`llm.*` 四个方法进清单，能力声明与 dispatch 分支一致。
        assert!(IMPLEMENTED.contains(&llm::CHAT_STREAM));
        assert!(IMPLEMENTED.contains(&llm::CANCEL));
        assert!(IMPLEMENTED.contains(&llm::LIST_MODELS));
        assert!(IMPLEMENTED.contains(&llm::COUNT_TOKENS));
        // M3 切片三：Inline Edit 内核面进清单，dispatch 有真分支（非 6101 式占位）。
        assert!(IMPLEMENTED.contains(&inline_edit::PROPOSE));
        assert_eq!(inline_edit::PROPOSE, "inline_edit.propose");
        // M4 第三切片：`agent.*` 三个方法进清单（协议面与 core 接线均已落地）。
        assert!(IMPLEMENTED.contains(&agent::START));
        assert!(IMPLEMENTED.contains(&agent::STOP));
        assert!(IMPLEMENTED.contains(&agent::DECIDE));
        // M5 切片二：`session.*` 七个方法进清单（`crates/store` 之上已接线）。
        assert!(IMPLEMENTED.contains(&session::CREATE));
        assert!(IMPLEMENTED.contains(&session::LIST));
        assert!(IMPLEMENTED.contains(&session::GET));
        assert!(IMPLEMENTED.contains(&session::UPDATE));
        assert!(IMPLEMENTED.contains(&session::DELETE));
        assert!(IMPLEMENTED.contains(&session::APPEND_MESSAGE));
        assert!(IMPLEMENTED.contains(&session::MESSAGES));
        // M5 切片三：`session.export` / `session.fork` 进清单，别只写在文档里。
        assert!(IMPLEMENTED.contains(&session::EXPORT));
        assert!(IMPLEMENTED.contains(&session::FORK));
        assert_eq!(session::EXPORT, "session.export");
        assert_eq!(session::FORK, "session.fork");
        // M5 切片五：检查点四个方法进清单。
        assert!(IMPLEMENTED.contains(&checkpoint::CREATE));
        assert!(IMPLEMENTED.contains(&checkpoint::LIST));
        assert!(IMPLEMENTED.contains(&checkpoint::RESTORE));
        assert!(IMPLEMENTED.contains(&checkpoint::DELETE));
        assert_eq!(checkpoint::RESTORE, "checkpoint.restore");
        // M6 切片一：终端五个方法进清单，事件不进。
        for name in [
            terminal::CREATE,
            terminal::WRITE,
            terminal::RESIZE,
            terminal::KILL,
            terminal::LIST,
        ] {
            assert!(IMPLEMENTED.contains(&name), "{name} 应已接线");
        }
        assert_eq!(terminal::CREATE, "terminal.create");
        assert_eq!(terminal::RESIZE, "terminal.resize");
        // M6 切片三：Git 五个方法进清单，事件侧不新增任何通知。
        for name in [
            git::STATUS,
            git::DIFF,
            git::STAGE,
            git::COMMIT,
            git::BRANCHES,
        ] {
            assert!(IMPLEMENTED.contains(&name), "{name} 应已接线");
        }
        assert_eq!(git::STATUS, "git.status");
        assert_eq!(git::BRANCHES, "git.branches");
        // M7 切片二：MCP 五个方法进清单，且不复用终端的 6xxx 段。
        for name in [
            mcp::SERVERS,
            mcp::CONNECT,
            mcp::DISCONNECT,
            mcp::TOOLS,
            mcp::CALL,
        ] {
            assert!(IMPLEMENTED.contains(&name), "{name} 应已接线");
        }
        assert_eq!(mcp::SERVERS, "mcp.servers");
        assert_eq!(mcp::CALL, "mcp.call");
        // P0 插件协议骨架：10 个方法进清单（dispatch 分支统一回 6101），
        // 而 `plugin/*` / `host/*` 这类插件包内部方法一律不进。
        // P5 切片二补第 11 个：`plugin.command`（命令网关，真实实现）。
        // P5 切片八补第 12 个：`plugin.load_many`（批量并发装载）。
        for name in [
            plugin::LIST,
            plugin::STATUS,
            plugin::INSTALL,
            plugin::UNINSTALL,
            plugin::LOAD,
            plugin::LOAD_MANY,
            plugin::UNLOAD,
            plugin::RELOAD,
            plugin::CALL,
            plugin::COMMAND,
            plugin::GRANT,
            plugin::REVOKE,
        ] {
            assert!(IMPLEMENTED.contains(&name), "{name} 应已接线");
        }
        assert_eq!(plugin::LIST, "plugin.list");
        assert_eq!(plugin::CALL, "plugin.call");
        assert_eq!(plugin::COMMAND, "plugin.command");
        assert_eq!(plugin::LOAD_MANY, "plugin.load_many");
        assert!(!IMPLEMENTED.iter().any(|name| name.starts_with("host/")));
        assert!(!IMPLEMENTED.iter().any(|name| name.starts_with("plugin/")));
    }

    /// LSP 切片二：五个方法接线后**进**能力清单。
    ///
    /// 这条测试钉住两件事：字面量（外壳按这些字符串发请求）与落表位置
    /// （在 `IMPLEMENTED` 而非待接线表）。能力数从 74 涨到 79 也是在这里
    /// 定死的——虚报能力比不报更糟，漏报则让外壳以为功能不存在。
    #[test]
    fn lsp_methods_are_wired_into_the_capability_surface() {
        assert_eq!(lsp::START, "lsp.start");
        assert_eq!(lsp::STOP, "lsp.stop");
        assert_eq!(lsp::SERVERS, "lsp.servers");
        assert_eq!(lsp::REQUEST, "lsp.request");
        assert_eq!(lsp::SYNC, "lsp.sync");
        for name in [lsp::START, lsp::STOP, lsp::SERVERS, lsp::REQUEST, lsp::SYNC] {
            assert!(IMPLEMENTED.contains(&name), "{name} 应已进能力清单");
            assert!(
                !DECLARED_NOT_IMPLEMENTED.contains(&name),
                "{name} 已接线，不该留在待接线表里"
            );
        }
        // 诊断是单向通知，永远不进能力清单（见 events_are_not_part_of_the_capability_surface）。
        assert_eq!(event::LSP_DIAGNOSTICS, "lsp.diagnostics");
        assert!(!DECLARED_NOT_IMPLEMENTED.contains(&event::LSP_DIAGNOSTICS));
    }

    #[test]
    fn declared_table_is_empty_after_lsp_wiring() {
        // M2 收尾时本表为空；LSP 切片一临时装过五个方法，切片二接线后再次清空。
        // 一旦有人新增协议方法却忘了接线、或接线后忘了挪表，这里会先响——
        // 比 Main 侧收到一片 `-32601` 才发现要好得多。
        assert!(
            DECLARED_NOT_IMPLEMENTED.is_empty(),
            "待接线表应为空：{:?}",
            DECLARED_NOT_IMPLEMENTED
        );
        for name in [
            llm::CHAT_STREAM,
            llm::CANCEL,
            llm::LIST_MODELS,
            llm::COUNT_TOKENS,
        ] {
            assert!(IMPLEMENTED.contains(&name), "{name} 应已接线");
        }
    }

    #[test]
    fn capability_tables_are_disjoint_and_cover_every_request_method() {
        let mut all: Vec<&str> = IMPLEMENTED.to_vec();
        all.extend_from_slice(DECLARED_NOT_IMPLEMENTED);
        let total = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), total, "两张表不得有重叠");
        // LSP 五个接线后已接线 79；P5 切片八补 `plugin.load_many` 后为 80、待接线 0。
        assert_eq!(IMPLEMENTED.len(), 80, "P5 切片八后应为 80 个已接线方法");
        assert_eq!(DECLARED_NOT_IMPLEMENTED.len(), 0);
        assert_eq!(
            total, 80,
            "应恰好覆盖协议里定义的全部 80 个请求方法（含 LSP 五个与 plugin.load_many）"
        );
    }

    #[test]
    fn audit_list_is_the_only_audit_method_and_is_wired() {
        assert_eq!(audit::LIST, "audit.list");
        assert!(IMPLEMENTED.contains(&audit::LIST));
        // 审计写入必须留在内核侧：协议面只暴露一个只读方法。
        assert!(!IMPLEMENTED
            .iter()
            .any(|name| name.starts_with("audit.") && *name != audit::LIST));
    }

    #[test]
    fn events_are_not_part_of_the_capability_surface() {
        // 事件是单向通知，不能被请求，因此两张方法表里都不该出现。
        for event in [
            event::LOG,
            event::WORKSPACE_CHANGED,
            event::LLM_STARTED,
            event::LLM_DELTA,
            event::LLM_REASONING,
            event::LLM_TOOL_CALL,
            event::LLM_USAGE,
            event::LLM_FINISHED,
            event::LLM_ERROR,
            event::PROVIDER_CHANGED,
            event::AGENT_APPROVAL_REQUIRED,
            event::AGENT_FINISHED,
            event::TERMINAL_DATA,
            event::TERMINAL_EXIT,
            event::LSP_DIAGNOSTICS,
        ] {
            assert!(!IMPLEMENTED.contains(&event), "事件不该进能力清单: {event}");
            assert!(
                !DECLARED_NOT_IMPLEMENTED.contains(&event),
                "事件不该进待接线方法表: {event}"
            );
        }
    }

    #[test]
    fn llm_domain_matches_protocol_doc() {
        assert_eq!(provider::UPSERT, "provider.upsert");
        assert_eq!(model::LIST, "model.list");
        assert_eq!(llm::CHAT_STREAM, "llm.chat_stream");
        assert_eq!(llm::COUNT_TOKENS, "llm.count_tokens");
        assert_eq!(event::LLM_DELTA, "llm.delta");
        assert_eq!(event::PROVIDER_CHANGED, "provider.changed");
    }

    #[test]
    fn every_implemented_name_is_domain_dot_action() {
        for name in IMPLEMENTED {
            if *name == cancel::REQUEST {
                continue;
            }
            let (domain, action) = name.split_once('.').expect("方法名必须形如 <域>.<动作>");
            assert!(!domain.is_empty(), "域不能为空: {name}");
            assert!(!action.is_empty(), "动作不能为空: {name}");
            for ch in name.chars() {
                assert!(
                    ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '_',
                    "方法名含非法字符 {ch}: {name}"
                );
            }
        }
    }

    #[test]
    fn workspace_module_matches_protocol_doc() {
        assert_eq!(workspace::OPEN, "workspace.open");
        assert_eq!(workspace::READ_FILE, "workspace.read_file");
        assert_eq!(workspace::WRITE_FILE, "workspace.write_file");
        assert_eq!(search::FILES, "search.files");
        assert_eq!(search::SYMBOLS, "search.symbols");
        assert_eq!(event::WORKSPACE_CHANGED, "workspace.changed");
    }
}
