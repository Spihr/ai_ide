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

// ---- 上下文引用（M3）----
export type { ChatContext } from "./generated/ChatContext";
export type { CompressOptions } from "./generated/CompressOptions";
export type { CompressStrategy } from "./generated/CompressStrategy";
export type { ContextRef } from "./generated/ContextRef";
export type { ContextRefKind } from "./generated/ContextRefKind";

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
 * 共 61 项，与 Rust `methods::IMPLEMENTED` 逐项对齐。
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
] as const;

/**
 * 协议面已定、内核尚未接线的方法。对应 Rust `methods::DECLARED_NOT_IMPLEMENTED`。
 *
 * M2 收口后 `llm.*` 已接进 `crates/core` 的 dispatch，本清单为空——空表是给后续
 * 里程碑（会话 / Agent / 终端…）留的位置：协议先行定义、实现后补，谁没接线谁待在这里。
 *
 * 它**不能**与 `IMPLEMENTED_METHODS` 有交集：`core.hello` 会把已实现清单当能力
 * 回给外壳，一旦虚报，外壳照单调用只会拿到一片 `-32601`。
 */
export const DECLARED_NOT_IMPLEMENTED_METHODS: readonly string[] = [];
