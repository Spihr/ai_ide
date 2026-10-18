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
  event: {
    log: "core.log",
    workspaceChanged: "workspace.changed",
    workspaceIndexed: "workspace.indexed",
  },
} as const;

/** M0 + M1 已实现的方法全集。对应 Rust `methods::IMPLEMENTED`。 */
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
] as const;
