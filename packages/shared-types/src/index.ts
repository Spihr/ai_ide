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
  },
} as const;

/**
 * 已实现的方法全集。对应 Rust `methods::IMPLEMENTED`。
 *
 * M0 + M1（协议内核 / 工作区 / 检索）+ M2 的 `provider.*` / `model.*` / `llm.*`
 * （Provider 与模型目录，含密钥内存保管与脱敏视图；流式对话、取消与用量统计），
 * 再加 M4 第三切片的 `agent.*` 三项（协议与 DTO 先行定义，内核 dispatch 接线在
 * 下一轮），共 30 项，与 Rust `methods::IMPLEMENTED` 逐项对齐。
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
