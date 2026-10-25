/**
 * 协议类型的唯一入口。
 *
 * `generated/` 下的文件由 Rust 侧 `crates/protocol` 通过 `ts-rs` 自动导出：
 *
 * ```bash
 * pnpm types:gen        # 等价于 cargo test -p ai-ide-protocol export_bindings
 * ```
 *
 * **禁止手工修改 `generated/` 下的任何文件**——它们是构建产物，
 * 下次导出会被覆盖。要改类型，改 `crates/protocol/src/**`。
 *
 * 新增可导出类型时，除在 Rust 侧加 `#[ts(export)]` 外，
 * 还需在下方的 re-export 列表里补一行。
 */

// ---- 帧与错误 ----
export type { ErrorCode } from "./generated/ErrorCode";
export type { ErrorObject } from "./generated/ErrorObject";
export type { ProtocolVersion } from "./generated/ProtocolVersion";
export type { Request } from "./generated/Request";
export type { Notification } from "./generated/Notification";
export type { Response } from "./generated/Response";
export type { ErrorResponse } from "./generated/ErrorResponse";

// ---- core.* 方法 ----
export type { HelloParams } from "./generated/HelloParams";
export type { HelloResult } from "./generated/HelloResult";
export type { PingParams } from "./generated/PingParams";
export type { PingResult } from "./generated/PingResult";
export type { StatsParams } from "./generated/StatsParams";
export type { StatsResult } from "./generated/StatsResult";
export type { ShutdownParams } from "./generated/ShutdownParams";
export type { ShutdownResult } from "./generated/ShutdownResult";
export type { CancelParams } from "./generated/CancelParams";
export type { LogLevel } from "./generated/LogLevel";
export type { LogEvent } from "./generated/LogEvent";

// ---- workspace.* 方法（M1）----
export type { WorkspaceOpenParams } from "./generated/WorkspaceOpenParams";
export type { WorkspaceStats } from "./generated/WorkspaceStats";
export type { WorkspaceOpenResult } from "./generated/WorkspaceOpenResult";
export type { WorkspaceIndexedEvent } from "./generated/WorkspaceIndexedEvent";
export type { WorkspaceCloseParams } from "./generated/WorkspaceCloseParams";
export type { OkResult } from "./generated/OkResult";
export type { EntryKind } from "./generated/EntryKind";
export type { TreeEntry } from "./generated/TreeEntry";
export type { WorkspaceTreeParams } from "./generated/WorkspaceTreeParams";
export type { WorkspaceTreeResult } from "./generated/WorkspaceTreeResult";
export type { Eol } from "./generated/Eol";
export type { WorkspaceReadFileParams } from "./generated/WorkspaceReadFileParams";
export type { WorkspaceReadFileResult } from "./generated/WorkspaceReadFileResult";
export type { WorkspaceWriteFileParams } from "./generated/WorkspaceWriteFileParams";
export type { WorkspaceWriteFileResult } from "./generated/WorkspaceWriteFileResult";
export type { CreateKind } from "./generated/CreateKind";
export type { WorkspaceCreateParams } from "./generated/WorkspaceCreateParams";
export type { WorkspaceRenameParams } from "./generated/WorkspaceRenameParams";
export type { WorkspaceDeleteParams } from "./generated/WorkspaceDeleteParams";
export type { WorkspaceDeleteResult } from "./generated/WorkspaceDeleteResult";
export type { WorkspaceRevealParams } from "./generated/WorkspaceRevealParams";
export type { PathChangeKind } from "./generated/PathChangeKind";
export type { PathChange } from "./generated/PathChange";
export type { WorkspaceChangedEvent } from "./generated/WorkspaceChangedEvent";

// ---- search.* 方法（M1）----
export type { SearchFilesParams } from "./generated/SearchFilesParams";
export type { FileMatch } from "./generated/FileMatch";
export type { SearchFilesResult } from "./generated/SearchFilesResult";
export type { SearchContentParams } from "./generated/SearchContentParams";
export type { ContentMatch } from "./generated/ContentMatch";
export type { SearchContentResult } from "./generated/SearchContentResult";
// M3 切片一：符号检索（`search.symbols`）的请求 / 响应与符号视图类型。
export type { SearchSymbolsParams } from "./generated/SearchSymbolsParams";
export type { SearchSymbolsResult } from "./generated/SearchSymbolsResult";
export type { SymbolKind } from "./generated/SymbolKind";
export type { SymbolRange } from "./generated/SymbolRange";
export type { Symbol } from "./generated/Symbol";

// ---- provider.* 方法（M2）----
export type { ProviderChangedEvent } from "./generated/ProviderChangedEvent";
export type { ProviderDeleteParams } from "./generated/ProviderDeleteParams";
export type { ProviderListParams } from "./generated/ProviderListParams";
export type { ProviderListResult } from "./generated/ProviderListResult";
export type { ProviderProtocol } from "./generated/ProviderProtocol";
export type { ProviderTestOverride } from "./generated/ProviderTestOverride";
export type { ProviderTestParams } from "./generated/ProviderTestParams";
export type { ProviderTestResult } from "./generated/ProviderTestResult";
export type { ProviderUpsertParams } from "./generated/ProviderUpsertParams";
export type { ProviderUpsertResult } from "./generated/ProviderUpsertResult";
export type { ProviderView } from "./generated/ProviderView";

// ---- model.* 方法（M2）----
export type { ModelCaps } from "./generated/ModelCaps";
export type { ModelDefaultParams } from "./generated/ModelDefaultParams";
export type { ModelDeleteParams } from "./generated/ModelDeleteParams";
export type { ModelInfo } from "./generated/ModelInfo";
export type { ModelListParams } from "./generated/ModelListParams";
export type { ModelListResult } from "./generated/ModelListResult";
export type { ModelRef } from "./generated/ModelRef";
export type { ModelUpsertParams } from "./generated/ModelUpsertParams";
export type { ModelUpsertResult } from "./generated/ModelUpsertResult";
export type { ModelView } from "./generated/ModelView";

// ---- llm.* 方法（M2）----
export type { ChatAttachment } from "./generated/ChatAttachment";
export type { ChatMessage } from "./generated/ChatMessage";
export type { ChatParams } from "./generated/ChatParams";
export type { ChatRole } from "./generated/ChatRole";
export type { ChatToolCall } from "./generated/ChatToolCall";
export type { FinishReason } from "./generated/FinishReason";
export type { LlmCancelParams } from "./generated/LlmCancelParams";
export type { LlmChatStreamParams } from "./generated/LlmChatStreamParams";
export type { LlmChatStreamResult } from "./generated/LlmChatStreamResult";
export type { LlmCountTokensParams } from "./generated/LlmCountTokensParams";
export type { LlmCountTokensResult } from "./generated/LlmCountTokensResult";
export type { LlmDeltaEvent } from "./generated/LlmDeltaEvent";
export type { LlmErrorEvent } from "./generated/LlmErrorEvent";
export type { LlmFinishedEvent } from "./generated/LlmFinishedEvent";
export type { LlmListModelsParams } from "./generated/LlmListModelsParams";
export type { LlmListModelsResult } from "./generated/LlmListModelsResult";
export type { LlmReasoningEvent } from "./generated/LlmReasoningEvent";
export type { LlmStartedEvent } from "./generated/LlmStartedEvent";
export type { LlmToolCallEvent } from "./generated/LlmToolCallEvent";
export type { LlmUsageEvent } from "./generated/LlmUsageEvent";
export type { ToolChoice } from "./generated/ToolChoice";
export type { ToolSpec } from "./generated/ToolSpec";

// ---- inline_edit.* 方法（M3 切片三：选区改写）----
export type { InlineEditProposeParams } from "./generated/InlineEditProposeParams";
export type { InlineEditProposeResult } from "./generated/InlineEditProposeResult";
export type { InlineEditUsage } from "./generated/InlineEditUsage";
export type { TextSelection } from "./generated/TextSelection";

// ---- 上下文引用（M3）----
export type { ChatContext } from "./generated/ChatContext";
export type { CompressOptions } from "./generated/CompressOptions";
export type { CompressStrategy } from "./generated/CompressStrategy";
export type { ContextRef } from "./generated/ContextRef";
export type { ContextRefKind } from "./generated/ContextRefKind";

// ---- lsp.* 方法（语言服务，切片一：协议面先行，内核未接线）----
// 坐标一律 1-based 行列：LSP 原生协议用 0-based，但本协议面给外壳用——两者换算
// 只在内核做一次（`crates/lsp`），外壳与 DTO 都拿 1-based，避免「第一行是 0 还是 1」
// 这类错误在三个进程间反复传播。
export type { LspPosition } from "./generated/LspPosition";
export type { LspRange } from "./generated/LspRange";
export type { LspServerStatus } from "./generated/LspServerStatus";
export type { LspServerInfo } from "./generated/LspServerInfo";
export type { LspStartParams } from "./generated/LspStartParams";
export type { LspStartResult } from "./generated/LspStartResult";
export type { LspStopParams } from "./generated/LspStopParams";
export type { LspStopResult } from "./generated/LspStopResult";
export type { LspServersParams } from "./generated/LspServersParams";
export type { LspServersResult } from "./generated/LspServersResult";
export type { LspRequestMethod } from "./generated/LspRequestMethod";
export type { LspRequestParams } from "./generated/LspRequestParams";
export type { LspRequestResult } from "./generated/LspRequestResult";
export type { LspLocation } from "./generated/LspLocation";
export type { LspHover } from "./generated/LspHover";
export type { LspCompletionItem } from "./generated/LspCompletionItem";
export type { LspCompletionList } from "./generated/LspCompletionList";
export type { LspSyncParams } from "./generated/LspSyncParams";
export type { LspSyncResult } from "./generated/LspSyncResult";
export type { LspDiagnosticSeverity } from "./generated/LspDiagnosticSeverity";
export type { LspDiagnostic } from "./generated/LspDiagnostic";
export type { LspDiagnosticsEvent } from "./generated/LspDiagnosticsEvent";

// ---- agent.* 方法（M4）----
export type { AgentMode } from "./generated/AgentMode";
export type { AgentRunStatus } from "./generated/AgentRunStatus";
export type { ApprovalDecision } from "./generated/ApprovalDecision";
export type { ApprovalRisk } from "./generated/ApprovalRisk";
export type { AgentPermissionOverride } from "./generated/AgentPermissionOverride";
export type { AgentStartParams } from "./generated/AgentStartParams";
export type { AgentStartResult } from "./generated/AgentStartResult";
export type { AgentStopParams } from "./generated/AgentStopParams";
export type { AgentStopResult } from "./generated/AgentStopResult";
export type { AgentDecideParams } from "./generated/AgentDecideParams";
export type { AgentDecideResult } from "./generated/AgentDecideResult";
export type { AgentApprovalRequiredEvent } from "./generated/AgentApprovalRequiredEvent";
export type { AgentFinishedEvent } from "./generated/AgentFinishedEvent";

// ---- session.* 方法（M5）----
export type { SessionView } from "./generated/SessionView";
export type { MessageView } from "./generated/MessageView";
// 消息的 usage 是「上游原始用量，原样保存」，形状由上游决定，故用 ts-rs 导出的
// JSON 值类型而非为它另造结构（造了就得跟着上游字段变而改协议）。
export type { JsonValue } from "./generated/serde_json/JsonValue";
export type { SessionCreateParams } from "./generated/SessionCreateParams";
export type { SessionCreateResult } from "./generated/SessionCreateResult";
export type { SessionListParams } from "./generated/SessionListParams";
export type { SessionListResult } from "./generated/SessionListResult";
export type { SessionGetParams } from "./generated/SessionGetParams";
export type { SessionGetResult } from "./generated/SessionGetResult";
export type { SessionUpdateParams } from "./generated/SessionUpdateParams";
export type { SessionDeleteParams } from "./generated/SessionDeleteParams";
export type { SessionDeleteResult } from "./generated/SessionDeleteResult";
export type { SessionAppendMessageParams } from "./generated/SessionAppendMessageParams";
export type { SessionAppendMessageResult } from "./generated/SessionAppendMessageResult";
export type { SessionMessagesParams } from "./generated/SessionMessagesParams";
export type { SessionMessagesResult } from "./generated/SessionMessagesResult";
export type { SessionExportFormat } from "./generated/SessionExportFormat";
export type { SessionExportParams } from "./generated/SessionExportParams";
export type { SessionExportResult } from "./generated/SessionExportResult";
export type { SessionExportDocument } from "./generated/SessionExportDocument";
export type { SessionForkParams } from "./generated/SessionForkParams";
export type { SessionForkResult } from "./generated/SessionForkResult";

// ---- checkpoint.* 方法（M5）----
export type { CheckpointView } from "./generated/CheckpointView";
export type { CheckpointFileView } from "./generated/CheckpointFileView";
export type { CheckpointFileKind } from "./generated/CheckpointFileKind";
export type { CheckpointOrigin } from "./generated/CheckpointOrigin";
export type { CheckpointCreateParams } from "./generated/CheckpointCreateParams";
export type { CheckpointCreateResult } from "./generated/CheckpointCreateResult";
export type { CheckpointListParams } from "./generated/CheckpointListParams";
export type { CheckpointListResult } from "./generated/CheckpointListResult";
export type { CheckpointRestoreParams } from "./generated/CheckpointRestoreParams";
export type { CheckpointRestoreResult } from "./generated/CheckpointRestoreResult";
export type { CheckpointRestoreFileView } from "./generated/CheckpointRestoreFileView";
export type { CheckpointRestoreStatus } from "./generated/CheckpointRestoreStatus";
export type { CheckpointDeleteParams } from "./generated/CheckpointDeleteParams";
export type { CheckpointDeleteResult } from "./generated/CheckpointDeleteResult";

// ---- permission.* 方法（M5）----
export type { PermissionRuleView } from "./generated/PermissionRuleView";
export type { PermissionListParams } from "./generated/PermissionListParams";
export type { PermissionListResult } from "./generated/PermissionListResult";
export type { PermissionRevokeParams } from "./generated/PermissionRevokeParams";
export type { PermissionRevokeResult } from "./generated/PermissionRevokeResult";

// ---- audit.* 方法（M5）----
export type { AuditEventView } from "./generated/AuditEventView";
export type { AuditListParams } from "./generated/AuditListParams";
export type { AuditListResult } from "./generated/AuditListResult";

// ---- terminal.* 方法（M6）----
// 终端输出走 `terminal.data` 的单向推送，`data` 是**原始字节的 base64**：
// 控制字符（`\r`、`Ctrl-C`、方向键的 `ESC [ A`）过字符串会在 JSON 转义里丢层，
// 因此协议两侧都以 base64 承载，由渲染侧解码成 `Uint8Array` 再喂给 xterm。
export type { TerminalView } from "./generated/TerminalView";
export type { TerminalCreateParams } from "./generated/TerminalCreateParams";
export type { TerminalCreateResult } from "./generated/TerminalCreateResult";
export type { TerminalWriteParams } from "./generated/TerminalWriteParams";
export type { TerminalWriteResult } from "./generated/TerminalWriteResult";
export type { TerminalResizeParams } from "./generated/TerminalResizeParams";
export type { TerminalResizeResult } from "./generated/TerminalResizeResult";
export type { TerminalKillParams } from "./generated/TerminalKillParams";
export type { TerminalListResult } from "./generated/TerminalListResult";
export type { TerminalDataEvent } from "./generated/TerminalDataEvent";
export type { TerminalExitEvent } from "./generated/TerminalExitEvent";

// ---- git.* 方法（M6 切片三）----
// 变更清单的每一侧都用 git 的两列状态字母（`index` / `worktree`），不合成布尔量：
// 「已暂存的新增 A」与「未暂存的新增 ?」必须分得开，否则 UI 没法决定要不要显示「取消暂存」。
export type { GitStatusParams } from "./generated/GitStatusParams";
export type { GitStatusResult } from "./generated/GitStatusResult";
export type { GitStatusEntry } from "./generated/GitStatusEntry";
export type { GitDiffParams } from "./generated/GitDiffParams";
export type { GitDiffResult } from "./generated/GitDiffResult";
export type { GitStageParams } from "./generated/GitStageParams";
export type { GitStageResult } from "./generated/GitStageResult";
export type { GitCommitParams } from "./generated/GitCommitParams";
export type { GitCommitResult } from "./generated/GitCommitResult";
export type { GitBranchesParams } from "./generated/GitBranchesParams";
export type { GitBranchesResult } from "./generated/GitBranchesResult";
export type { GitBranch } from "./generated/GitBranch";
export type { McpServerInfo } from "./generated/McpServerInfo";
export type { McpServersParams } from "./generated/McpServersParams";
export type { McpServersResult } from "./generated/McpServersResult";
export type { McpConnectParams } from "./generated/McpConnectParams";
export type { McpConnectResult } from "./generated/McpConnectResult";
export type { McpDisconnectParams } from "./generated/McpDisconnectParams";
export type { McpDisconnectResult } from "./generated/McpDisconnectResult";
export type { McpToolInfo } from "./generated/McpToolInfo";
export type { McpToolsParams } from "./generated/McpToolsParams";
export type { McpToolsResult } from "./generated/McpToolsResult";
export type { McpCallParams } from "./generated/McpCallParams";
export type { McpCallResult } from "./generated/McpCallResult";
export type { McpContentBlock } from "./generated/McpContentBlock";

// ---- plugin.* 方法（P0 协议骨架）----
export type { PluginState } from "./generated/PluginState";
export type { PluginToolRisk } from "./generated/PluginToolRisk";
export type { PluginSummary } from "./generated/PluginSummary";
export type { PluginToolSummary } from "./generated/PluginToolSummary";
export type { PluginCommandSummary } from "./generated/PluginCommandSummary";
export type { PluginCommandParams } from "./generated/PluginCommandParams";
export type { PluginCommandResult } from "./generated/PluginCommandResult";
export type { PluginListParams } from "./generated/PluginListParams";
export type { PluginListResult } from "./generated/PluginListResult";
export type { PluginStatusParams } from "./generated/PluginStatusParams";
export type { PluginStatusResult } from "./generated/PluginStatusResult";
export type { PluginInstallParams } from "./generated/PluginInstallParams";
export type { PluginInstallResult } from "./generated/PluginInstallResult";
export type { PluginUninstallParams } from "./generated/PluginUninstallParams";
export type { PluginUninstallResult } from "./generated/PluginUninstallResult";
export type { PluginLoadParams } from "./generated/PluginLoadParams";
export type { PluginLoadResult } from "./generated/PluginLoadResult";
export type { PluginUnloadParams } from "./generated/PluginUnloadParams";
export type { PluginUnloadResult } from "./generated/PluginUnloadResult";
export type { PluginReloadParams } from "./generated/PluginReloadParams";
export type { PluginReloadResult } from "./generated/PluginReloadResult";
export type { PluginCallParams } from "./generated/PluginCallParams";
export type { PluginCallResult } from "./generated/PluginCallResult";
export type { PluginGrantParams } from "./generated/PluginGrantParams";
export type { PluginGrantResult } from "./generated/PluginGrantResult";
export type { PluginRevokeParams } from "./generated/PluginRevokeParams";
export type { PluginRevokeResult } from "./generated/PluginRevokeResult";

// ---- 插件可观测事件（P5 切片七）----
//
// 三个事件的载荷都由 Rust 侧用 `json!` 手工构造（`crates/plugin/src/events.rs`
// 与 `crates/core/src/plugin_host.rs`），因此**没有** ts-rs 生成的 DTO。这里按
// 内核真正投递的键逐字排列，字段一律 camelCase（与 Rust 侧一致，不是协议的
// snake_case——事件载荷不是 RPC 参数）。

/** 插件日志级别。内核侧只做原样透传，未知级别按 `info` 处理。 */
export type PluginLogLevel = "debug" | "info" | "warn" | "error";

/**
 * `plugin.log` 载荷。
 *
 * `fields` 是插件自报的结构化附加信息（内核只放行对象形态），UI 不解析它，
 * 仅原样展示在详情里。
 */
export interface PluginLogEvent {
  pluginId: string;
  level: PluginLogLevel;
  message: string;
  fields?: Record<string, unknown>;
}

/**
 * `plugin.progress` 载荷。
 *
 * `token` 把同一次长任务的多次上报串起来（渲染侧靠 `pluginId + token` 归并）；
 * `percent` 已被内核夹到 0~100 且非有限值一律为 `null`；`truncated` 表示
 * `message` / `token` 被按字符截断过。
 */
export interface PluginProgressEvent {
  pluginId: string;
  token: string | null;
  message: string | null;
  percent: number | null;
  truncated: boolean;
}

// `PluginState` 只在上面的 re-export 列表里出现过，re-export 不进本文件作用域，
// 于是 `PluginStateChangedEvent.state` 得单独 import 一次。
import type { PluginState } from "./generated/PluginState";

/** `plugin.state_changed` 载荷：`state` 与 `PluginState` 同集合。 */
export interface PluginStateChangedEvent {
  pluginId: string;
  state: PluginState;
}

// ---- 协议常量（与 Rust 侧保持一致的运行时镜像）----

/** JSON-RPC 版本字面量。对应 Rust `ai_ide_protocol::JSONRPC_VERSION`。 */
export const JSONRPC_VERSION = "2.0" as const;

/** 当前协议版本字符串。对应 Rust `ai_ide_protocol::PROTOCOL_VERSION`。 */
export const PROTOCOL_VERSION = "1.0" as const;

/**
 * 方法名常量。对应 Rust `ai_ide_protocol::methods`。
 *
 * 这些是**运行时值**，无法由 ts-rs 导出，因此在此手工维护；
 * 由 `apps/desktop/src/shared/methods-contract.test.ts` 负责逐字比对。
 */
export const Methods = {
  core: {
    hello: "core.hello",
    ping: "core.ping",
    stats: "core.stats",
    shutdown: "core.shutdown",
  },
  cancel: {
    request: "$/cancelRequest",
  },
  workspace: {
    open: "workspace.open",
    close: "workspace.close",
    tree: "workspace.tree",
    readFile: "workspace.read_file",
    writeFile: "workspace.write_file",
    create: "workspace.create",
    rename: "workspace.rename",
    delete: "workspace.delete",
    reveal: "workspace.reveal",
  },
  search: {
    files: "search.files",
    content: "search.content",
    symbols: "search.symbols",
  },
  provider: {
    upsert: "provider.upsert",
    list: "provider.list",
    delete: "provider.delete",
    test: "provider.test",
  },
  model: {
    upsert: "model.upsert",
    list: "model.list",
    delete: "model.delete",
  },
  llm: {
    chatStream: "llm.chat_stream",
    cancel: "llm.cancel",
    listModels: "llm.list_models",
    countTokens: "llm.count_tokens",
  },
  // M3 切片三：选区改写。语义上贴着 llm.*，但它是「编辑器动作」而非「一轮对话」，
  // 因此单起一个命名空间——外壳以后要按这个前缀决定「不进会话、不写盘」。
  inlineEdit: {
    propose: "inline_edit.propose",
  },
  // 语言服务（切片一：协议面先行）。`request` 的 `method` 用枚举字符串而不是裸字符串，
  // 因为外壳要按它选渲染组件（定义跳转 / 悬浮 / 补全），拼错一个字母就得到空结果，
  // 那种失败最难查。`stop` 只带 `server_id`——被关的是连接，不是整个语言。
  lsp: {
    start: "lsp.start",
    stop: "lsp.stop",
    servers: "lsp.servers",
    request: "lsp.request",
    sync: "lsp.sync",
  },
  agent: {
    start: "agent.start",
    stop: "agent.stop",
    decide: "agent.decide",
  },
  session: {
    create: "session.create",
    list: "session.list",
    get: "session.get",
    update: "session.update",
    delete: "session.delete",
    appendMessage: "session.append_message",
    messages: "session.messages",
    export: "session.export",
    fork: "session.fork",
  },
  checkpoint: {
    create: "checkpoint.create",
    list: "checkpoint.list",
    restore: "checkpoint.restore",
    delete: "checkpoint.delete",
  },
  permission: {
    list: "permission.list",
    revoke: "permission.revoke",
  },
  audit: {
    list: "audit.list",
  },
  terminal: {
    create: "terminal.create",
    write: "terminal.write",
    resize: "terminal.resize",
    kill: "terminal.kill",
    list: "terminal.list",
  },
  git: {
    status: "git.status",
    diff: "git.diff",
    stage: "git.stage",
    commit: "git.commit",
    branches: "git.branches",
  },
  mcp: {
    servers: "mcp.servers",
    connect: "mcp.connect",
    disconnect: "mcp.disconnect",
    tools: "mcp.tools",
    call: "mcp.call",
  },
  plugin: {
    list: "plugin.list",
    status: "plugin.status",
    install: "plugin.install",
    uninstall: "plugin.uninstall",
    load: "plugin.load",
    unload: "plugin.unload",
    reload: "plugin.reload",
    call: "plugin.call",
    command: "plugin.command",
    grant: "plugin.grant",
    revoke: "plugin.revoke",
  },
  event: {
    log: "core.log",
    workspaceChanged: "workspace.changed",
    workspaceIndexed: "workspace.indexed",
    providerChanged: "provider.changed",
    llmStarted: "llm.started",
    llmDelta: "llm.delta",
    llmReasoning: "llm.reasoning",
    llmToolCall: "llm.tool_call",
    llmUsage: "llm.usage",
    llmFinished: "llm.finished",
    llmError: "llm.error",
    agentApprovalRequired: "agent.approval_required",
    agentFinished: "agent.finished",
    terminalData: "terminal.data",
    terminalExit: "terminal.exit",
    // 诊断是**推送**而非拉取：打开文件后语言服务器随时可能重算，外壳不能靠轮询
    // （补全型服务器算一次诊断要几十毫秒，轮询会把编辑器拖死）。载荷见
    // `LspDiagnosticsEvent`，带 `version` 供外壳丢弃过期批次。
    lspDiagnostics: "lsp.diagnostics",
    // 插件可观测事件（P5 切片七）。名字取自 Rust `crates/plugin/src/events.rs`
    // 的常量（`PLUGIN_LOG` / `PLUGIN_PROGRESS` / `PLUGIN_STATE_CHANGED`）——
    // 这三个不是 `ai_ide_protocol::methods::event` 的成员，因为它们是宿主
    // 内部通道：内核 → 外壳窗口，不参与插件协议握手。
    pluginLog: "plugin.log",
    pluginProgress: "plugin.progress",
    pluginStateChanged: "plugin.state_changed",
  },
} as const;

/**
 * 已实现的方法全集。对应 Rust `methods::IMPLEMENTED`。
 *
 * M0 + M1（协议内核 / 工作区 / 检索）+ M2 的 `provider.*` / `model.*` / `llm.*`
 * （Provider 与模型目录，含密钥内存保管与脱敏视图；流式对话、取消与用量统计），
 * 再加 M4 第三切片的 `agent.*` 三项、M5 的 `session.*` 九项（会话 / 消息落盘，
 * 含导出与分叉）、`checkpoint.*` 四项（快照 / 列表 / 回滚 / 删除）、
 * `permission.*` 两项（免问规则列表 / 撤销）与 `audit.list`（审计账本只读查询），
 * 再加 M6 切片一的 `terminal.*` 五项（PTY 会话的建 / 写 / 改尺寸 / 杀 / 列），
 * M7 的 `mcp.*` 五项（Server 清单 / 连接 / 断开 / 工具 / 调用），
 * 以及插件系统 P0 的 `plugin.*` 十项（协议骨架：能路由，但统一回 6101）——
 * P5 切片二把其中第 11 项 `plugin.command`（命令网关）补成真实现，
 * 再加 M3 切片一的 `search.symbols`（符号检索内核面：七种语言的声明抽取，
 * 外壳入口留待下一片），以及 M3 切片三的 `inline_edit.propose`（选区改写内核面：
 * 一次补全 + 结构化 diff，内核不写盘），共 74 项，与 Rust `methods::IMPLEMENTED` 逐项对齐。
 */
export const IMPLEMENTED_METHODS = [
  Methods.core.hello,
  Methods.core.ping,
  Methods.core.stats,
  Methods.core.shutdown,
  Methods.cancel.request,
  Methods.workspace.open,
  Methods.workspace.close,
  Methods.workspace.tree,
  Methods.workspace.readFile,
  Methods.workspace.writeFile,
  Methods.workspace.create,
  Methods.workspace.rename,
  Methods.workspace.delete,
  Methods.workspace.reveal,
  Methods.search.files,
  Methods.search.content,
  Methods.search.symbols,
  Methods.provider.upsert,
  Methods.provider.list,
  Methods.provider.delete,
  Methods.provider.test,
  Methods.model.upsert,
  Methods.model.list,
  Methods.model.delete,
  Methods.llm.chatStream,
  Methods.llm.cancel,
  Methods.llm.listModels,
  Methods.llm.countTokens,
  Methods.inlineEdit.propose,
  Methods.agent.start,
  Methods.agent.stop,
  Methods.agent.decide,
  Methods.session.create,
  Methods.session.list,
  Methods.session.get,
  Methods.session.update,
  Methods.session.delete,
  Methods.session.appendMessage,
  Methods.session.messages,
  Methods.session.export,
  Methods.session.fork,
  Methods.checkpoint.create,
  Methods.checkpoint.list,
  Methods.checkpoint.restore,
  Methods.checkpoint.delete,
  Methods.permission.list,
  Methods.permission.revoke,
  Methods.audit.list,
  Methods.terminal.create,
  Methods.terminal.write,
  Methods.terminal.resize,
  Methods.terminal.kill,
  Methods.terminal.list,
  Methods.git.status,
  Methods.git.diff,
  Methods.git.stage,
  Methods.git.commit,
  Methods.git.branches,
  Methods.mcp.servers,
  Methods.mcp.connect,
  Methods.mcp.disconnect,
  Methods.mcp.tools,
  Methods.mcp.call,
  Methods.plugin.list,
  Methods.plugin.status,
  Methods.plugin.install,
  Methods.plugin.uninstall,
  Methods.plugin.load,
  Methods.plugin.unload,
  Methods.plugin.reload,
  Methods.plugin.call,
  Methods.plugin.command,
  Methods.plugin.grant,
  Methods.plugin.revoke,
] as const;

/**
 * 协议面已定、内核尚未接线的方法。对应 Rust `methods::DECLARED_NOT_IMPLEMENTED`。
 *
 * M2 收口后 `llm.*` 已接进 `crates/core` 的 dispatch，本清单为空——空表是给后续
 * 里程碑（会话 / Agent / 终端…）留的位置：协议先行定义、实现后补，谁没接线谁待在这里。
 *
 * 它**不能**与 `IMPLEMENTED_METHODS` 有交集：`core.hello` 会把已实现清单当能力
 * 回给外壳，一旦虚报，外壳照单调用只会拿到一片 `-32601`。
 *
 * 现在这里有 `lsp.*` 五项：协议面（DTO / 常量 / 错误码）已定，`crates/lsp` 的客户端
 * 与注册表也已落地，但 `crates/core` 的 dispatch 还没接线，因此它们**是能路由之外的
 * 声明**。放进这张表而不是 `IMPLEMENTED_METHODS`，是为了让 `core.hello` 如实回「还没
 * 这一项」；接线那一片只需把五行从这张表移到上面那张，`capability` 计数随之 +5。
 */
export const DECLARED_NOT_IMPLEMENTED_METHODS: readonly string[] = [
  Methods.lsp.start,
  Methods.lsp.stop,
  Methods.lsp.servers,
  Methods.lsp.request,
  Methods.lsp.sync,
];
