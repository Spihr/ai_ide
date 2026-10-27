/**
 * 插件协议常量与帧编解码。
 *
 * 三组命名空间必须分清，混用会让「谁调谁」变得不可读：
 *
 * 1. `Methods.plugin.*`  —— 内核对外暴露的方法（进 JSON-RPC 方法表，握手时上报）
 * 2. `Methods.rpc.*`     —— 宿主 → 插件 的调用（只在插件进程内有效，不进全局方法表）
 * 3. `Methods.host.*`    —— 插件 → 宿主 的能力调用（唯一能产生副作用的入口）
 *
 * 命名风格刻意与内核现有方法保持一致（`<域>.<动作>` 小写下划线），
 * 唯一例外是宿主↔插件之间的调用用 `/` 分隔——它们不是内核协议的一部分，
 * 用不同分隔符可以在日志里一眼区分帧的归属。
 */

/** 插件 API 版本。major 不一致即拒载，minor 取低版本能力集（同 docs/03 §5）。 */
export const PLUGIN_API_VERSION = { major: 1, minor: 0 }

/** 内核对外方法（会进 `methods::IMPLEMENTED`）。 */
export const Methods = {
  plugin: {
    LIST: 'plugin.list',
    STATUS: 'plugin.status',
    INSTALL: 'plugin.install',
    UNINSTALL: 'plugin.uninstall',
    LOAD: 'plugin.load',
    UNLOAD: 'plugin.unload',
    RELOAD: 'plugin.reload',
    CALL: 'plugin.call',
    GRANT: 'plugin.grant',
    REVOKE: 'plugin.revoke'
  },
  event: {
    LOG: 'plugin.log',
    STATE_CHANGED: 'plugin.state_changed',
    DIAGNOSTICS: 'plugin.diagnostics'
  },
  /**
   * 宿主 → 插件的调用。
   *
   * 这里的方法名以 `crates/plugin/src/host.rs` 的常量为准（`plugin/handshake`、
   * `plugin/tool-invoke`、`plugin/command-invoke`、`plugin/event`）。`LEGACY_*`
   * 是 P1 之前的旧口径，只用于兼容老 demo 包，新写插件不要用。
   */
  rpc: {
    HANDSHAKE: 'plugin/handshake',
    LEGACY_HANDSHAKE: 'plugin/initialize',
    SHUTDOWN: 'plugin/shutdown',
    PING: 'plugin/ping',
    TOOL_INVOKE: 'plugin/tool-invoke',
    LEGACY_TOOL_INVOKE: 'tool/invoke',
    COMMAND_INVOKE: 'plugin/command-invoke',
    LEGACY_COMMAND_INVOKE: 'command/invoke',
    EVENT_NOTIFY: 'plugin/event',
    LEGACY_EVENT_NOTIFY: 'event/notify'
  },
  /** 插件 → 宿主的能力调用 */
  host: {
    FS_READ: 'host/fs.read',
    FS_WRITE: 'host/fs.write',
    FS_LIST: 'host/fs.list',
    FS_STAT: 'host/fs.stat',
    SEARCH_CONTENT: 'host/search/content',
    WORKSPACE_INFO: 'host/workspace.info',
    NET_FETCH: 'host/net.fetch',
    LOG: 'host/log',
    PROGRESS: 'host/progress'
  }
}

/**
 * 错误码。1xxx–5xxx 段已被内核占用（workspace / provider / tool / 通用 / store），
 * 插件独占 6xxx，避免与既有分类混淆。
 */
export const ErrorCodes = {
  // 标准 JSON-RPC
  PARSE_ERROR: -32700,
  INVALID_REQUEST: -32600,
  METHOD_NOT_FOUND: -32601,
  INVALID_PARAMS: -32602,
  INTERNAL_ERROR: -32603,
  // 沿用内核既有码
  PATH_OUT_OF_WORKSPACE: 1002,
  PATH_DENIED: 1003,
  IO_ERROR: 1005,
  TOOL_NOT_ALLOWED: 3001,
  TOOL_TIMEOUT: 3002,
  CANCELLED: 4001,
  // 插件专属
  PLUGIN_NOT_FOUND: 6001,
  PLUGIN_CRASHED: 6002,
  CAPABILITY_DENIED: 6003,
  PLUGIN_API_MISMATCH: 6004,
  PLUGIN_TIMEOUT: 6005,
  PLUGIN_PROTOCOL_VIOLATION: 6006,
  PLUGIN_MANIFEST_INVALID: 6007,
  TOOL_CONFLICT: 6008,
  PLUGIN_QUOTA_EXCEEDED: 6009
}

const ERROR_TEXT = {
  [ErrorCodes.PATH_OUT_OF_WORKSPACE]: '路径越出工作区',
  [ErrorCodes.PATH_DENIED]: '命中敏感路径',
  [ErrorCodes.PLUGIN_NOT_FOUND]: '插件不存在',
  [ErrorCodes.PLUGIN_CRASHED]: '插件进程已崩溃',
  [ErrorCodes.CAPABILITY_DENIED]: '插件未获得该能力授权',
  [ErrorCodes.PLUGIN_API_MISMATCH]: '插件 API 版本不兼容',
  [ErrorCodes.PLUGIN_TIMEOUT]: '插件调用超时',
  [ErrorCodes.PLUGIN_PROTOCOL_VIOLATION]: '插件违反协议',
  [ErrorCodes.PLUGIN_MANIFEST_INVALID]: '插件清单非法',
  [ErrorCodes.TOOL_CONFLICT]: '工具名冲突',
  [ErrorCodes.PLUGIN_QUOTA_EXCEEDED]: '单次调用超出宿主 API 配额'
}

export class PluginError extends Error {
  constructor(code, message, data) {
    super(message || ERROR_TEXT[code] || `插件错误 ${code}`)
    this.name = 'PluginError'
    this.code = code
    this.data = data
  }

  toObject() {
    const out = { code: this.code, message: this.message }
    if (this.data !== undefined) out.data = this.data
    return out
  }
}

/** 一帧 = 一行 JSON（NDJSON），与内核 stdin/stdout 的约定完全一致。 */
export function encodeFrame(message) {
  return `${JSON.stringify(message)}\n`
}

export function requestFrame(id, method, params) {
  const frame = { jsonrpc: '2.0', id, method }
  if (params !== undefined) frame.params = params
  return frame
}

export function parseFrame(line) {
  try {
    const value = JSON.parse(line)
    if (!value || typeof value !== 'object' || Array.isArray(value)) {
      throw new PluginError(ErrorCodes.PARSE_ERROR, '帧必须是 JSON 对象')
    }
    return value
  } catch (err) {
    if (err instanceof PluginError) throw err
    throw new PluginError(ErrorCodes.PARSE_ERROR, `帧不是合法 JSON：${err.message}`)
  }
}

/** major 一致即可通信；minor 以较低者为能力集上限。 */
export function isCompatible(a, b) {
  return Boolean(a) && Boolean(b) && a.major === b.major
}

export function effectiveMinor(a, b) {
  return Math.min(a.minor, b.minor)
}

export function formatVersion(v) {
  return `${v.major}.${v.minor}`
}

/**
 * 单行上限。超限说明对端在吐垃圾或协议实现有误，
 * 继续读下去只会把宿主内存吃光（同 `run_command` 的 64 KiB 截断思路，
 * 但这里是「整帧」上限，取 1 MiB 留足工具结果的余量）。
 */
export const MAX_FRAME_BYTES = 1024 * 1024

/** 单次工具结果回灌上限，与 `run_command` 的 MAX_STREAM_BYTES 对齐。 */
export const MAX_TOOL_RESULT_BYTES = 64 * 1024

/**
 * 单次工具调用允许发起的宿主 API 次数。
 * 宿主只提供单层原语（fs.list 不递归），遍历策略归插件，代价是往返次数变多；
 * 配额就是给这条自由的边界——200 次足够扫几千个文件，又不至于让一个死循环
 * 把 Core 的 IPC 线程占满。
 */
export const MAX_HOST_CALLS_PER_INVOKE = 200
