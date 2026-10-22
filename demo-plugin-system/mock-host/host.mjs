/**
 * PluginHost —— 插件系统的宿主（demo 版）。
 *
 * 真实实现落 `crates/plugin-host`（Rust），本文件是同一套状态机与闸门顺序的
 * Node 可执行版本，目的是证明协议自洽，不是生产代码。
 *
 * 闸门顺序（顺序不可交换，每一步都可能直接终结请求）：
 *   ① 插件状态（必须 Ready）        → 6001 / 6002
 *   ② 方法是否属于该插件            → 6001
 *   ③ 能力授权（capability）         → 6003
 *   ④ 路径闸门（PathGuard）          → 1002 / 1003
 *   ⑤ 审批（write / exec）           → 3001
 *   ⑥ 资源限制（帧大小 / 超时 / 结果截断）
 */

import fs from 'node:fs'
import path from 'node:path'
import { EventEmitter } from 'node:events'
import {
  ErrorCodes,
  MAX_HOST_CALLS_PER_INVOKE,
  MAX_TOOL_RESULT_BYTES,
  Methods,
  PLUGIN_API_VERSION,
  PluginError,
  isCompatible
} from './protocol.mjs'
import { PathGuard } from './path-guard.mjs'
import { validateManifest } from './manifest.mjs'
import { ApprovalQueue, GrantStore } from './permission.mjs'
import { PluginProcess } from './supervisor.mjs'

/** 插件状态机：list → installing → granted → loading → ready → (crashed) → disabled */
export const PLUGIN_STATE = {
  INSTALLED: 'installed',
  NEEDS_GRANT: 'needs_grant',
  READY: 'ready',
  CRASHED: 'crashed',
  DISABLED: 'disabled'
}

export class PluginHost extends EventEmitter {
  /**
   * @param {object} opts
   * @param {string} opts.workspaceRoot
   * @param {string} opts.workspaceId
   * @param {(req: object) => Promise<object>} opts.approver
   */
  constructor(opts) {
    super()
    this.workspaceRoot = path.resolve(opts.workspaceRoot)
    this.workspaceId = opts.workspaceId ?? 'ws-demo'
    this.workspaceName = path.basename(this.workspaceRoot)
    this.guard = new PathGuard(this.workspaceRoot)
    this.grants = new GrantStore()
    this.approvals = new ApprovalQueue(opts.approver)
    this.plugins = new Map()
    /** 全名（plugin.<id>.<tool>）→ 归属信息，等价于 ToolRegistry 的 name → Tool 映射 */
    this.toolIndex = new Map()
    this.audit = []
    /** 当前这次工具调用已经发起的宿主 API 次数（每次 callTool 归零） */
    this.hostCalls = 0
  }

  // ── 生命周期 ────────────────────────────────────────────────────────────

  /** 只读扫描：不启动进程，只读清单并给出可加载性判断。 */
  inspect(pluginDir) {
    const manifestPath = path.join(pluginDir, 'plugin.json')
    if (!fs.existsSync(manifestPath)) {
      throw new PluginError(ErrorCodes.PLUGIN_NOT_FOUND, `缺少 plugin.json：${pluginDir}`)
    }
    const raw = JSON.parse(fs.readFileSync(manifestPath, 'utf8'))
    const { manifest, errors, warnings } = validateManifest(raw)
    return { pluginDir, manifest, errors, warnings }
  }

  grant(pluginId, capabilities) {
    return this.grants.grant(pluginId, capabilities)
  }

  async load(pluginDir) {
    const { manifest, errors, warnings } = this.inspect(pluginDir)
    if (!manifest) {
      throw new PluginError(ErrorCodes.PLUGIN_MANIFEST_INVALID, errors.join('; '), { errors })
    }

    const entry = {
      id: manifest.id,
      manifest,
      dir: pluginDir,
      state: PLUGIN_STATE.INSTALLED,
      warnings,
      proc: null,
      tools: [], // 握手后插件上报的工具
      restarts: 0,
      lastError: null
    }
    this.plugins.set(manifest.id, entry)

    // ② 能力闸门在启动之前：没授权就不给进程，避免「先跑起来再要权限」的谈判空间。
    const missing = this.grants.missingFor(manifest.id, manifest.capabilities)
    if (missing.length > 0) {
      entry.state = PLUGIN_STATE.NEEDS_GRANT
      this._emitState(entry.id, 'plugin.needs_grant', { missing })
      throw new PluginError(
        ErrorCodes.CAPABILITY_DENIED,
        `加载前需授予能力：${missing.join(', ')}`,
        { missing }
      )
    }

    await this._spawn(entry)
    return entry
  }

  async _spawn(entry) {
    entry.state = PLUGIN_STATE.INSTALLED
    const proc = new PluginProcess({
      command: process.execPath,
      args: [entry.manifest.main],
      cwd: entry.dir,
      // 环境变量只给最小集：不给 home、不给 provider 密钥，插件没有理由知道。
      env: {
        PATH: process.env.PATH ?? '',
        NODE_ENV: 'production',
        AIIDE_WORKSPACE_ID: this.workspaceId
      }
    })

    proc.onRequest = (method, params) => this._handlePluginRequest(entry, method, params)
    proc.on('stderr', (line) => {
      this.emit('log', { pluginId: entry.id, level: 'stderr', message: line })
    })
    proc.on('violation', (err) => {
      this.lastViolation = err
      this.emit('log', { pluginId: entry.id, level: 'error', message: `协议违规：${err.message}` })
    })
    proc.on('exit', ({ code, signal }) => {
      if (entry.state === PLUGIN_STATE.DISABLED) return
      entry.state = PLUGIN_STATE.CRASHED
      entry.lastError = `进程退出 code=${code} signal=${signal}`
      // 崩溃的工具必须立刻下架，否则模型会继续调到已死的插件。
      this._unregisterTools(entry.id)
      this._emitState(entry.id, 'plugin.crashed', { code, signal })
      this.emit('log', { pluginId: entry.id, level: 'error', message: entry.lastError })
    })

    entry.proc = proc.start()

    const result = await entry.proc.request(Methods.rpc.INITIALIZE, {
      pluginApi: PLUGIN_API_VERSION,
      host: { product: 'ai-ide', version: '0.0.0-demo' },
      plugin: { id: entry.manifest.id, version: entry.manifest.version },
      // 刻意不给 rootPath：插件用 host/fs.* 的相对路径工作，越界由 PathGuard 兜住。
      workspace: { id: this.workspaceId, name: this.workspaceName },
      capabilities: this.grants.list(entry.manifest.id),
      limits: { maxFrameBytes: 1024 * 1024, requestTimeoutMs: 30000 },
      subscriptions: entry.manifest.subscriptions
    })

    this._assertHandshake(entry, result)
    this._registerTools(entry)
    entry.state = PLUGIN_STATE.READY
    this._emitState(entry.id, 'plugin.ready', { tools: entry.tools.map((t) => t.name) })
    return entry
  }

  /**
   * 握手结果必须与清单**双向**一致：
   * 少报 = 清单骗人（工具不可用却写进文档），多报 = 运行时偷偷扩权。
   * 两种情况都判协议违规，宁可拒载。
   */
  _assertHandshake(entry, result) {
    if (!result || typeof result !== 'object') {
      throw new PluginError(ErrorCodes.PLUGIN_PROTOCOL_VIOLATION, 'initialize 未返回对象')
    }
    if (!isCompatible(result.pluginApi, PLUGIN_API_VERSION)) {
      throw new PluginError(ErrorCodes.PLUGIN_API_MISMATCH, '初始化返回的插件 API 版本不兼容', {
        got: result.pluginApi
      })
    }
    const declared = new Map(entry.manifest.tools.map((t) => [t.name, t]))
    const reported = Array.isArray(result.tools) ? result.tools : []

    for (const tool of reported) {
      if (!declared.has(tool?.name)) {
        throw new PluginError(
          ErrorCodes.PLUGIN_PROTOCOL_VIOLATION,
          `插件上报了清单未声明的工具：${tool?.name}`
        )
      }
    }
    const reportedNames = new Set(reported.map((t) => t?.name))
    for (const name of declared.keys()) {
      if (!reportedNames.has(name)) {
        throw new PluginError(
          ErrorCodes.PLUGIN_PROTOCOL_VIOLATION,
          `清单声明了工具但插件未实现：${name}`
        )
      }
    }
    // 工具的定义以清单为准（risk / pathField 只能由清单说了算），插件上报仅作确认。
    entry.tools = entry.manifest.tools.map((t) => ({ ...t }))
    entry.commands = Array.isArray(result.commands) ? result.commands : entry.manifest.commands
  }

  _registerTools(entry) {
    for (const tool of entry.tools) {
      const fullName = `plugin.${entry.id}.${tool.name}`
      if (this.toolIndex.has(fullName)) {
        throw new PluginError(ErrorCodes.TOOL_CONFLICT, `工具名冲突：${fullName}`)
      }
      this.toolIndex.set(fullName, { pluginId: entry.id, tool })
    }
  }

  _unregisterTools(pluginId) {
    for (const [fullName, ref] of [...this.toolIndex]) {
      if (ref.pluginId === pluginId) this.toolIndex.delete(fullName)
    }
  }

  /** 崩溃后重启。失败次数超阈值就停手，避免「崩溃-重启」死循环烧 CPU。 */
  async restart(pluginId, { maxRestarts = 3 } = {}) {
    const entry = this.plugins.get(pluginId)
    if (!entry) throw new PluginError(ErrorCodes.PLUGIN_NOT_FOUND, pluginId)
    if (entry.restarts >= maxRestarts) {
      entry.state = PLUGIN_STATE.DISABLED
      throw new PluginError(ErrorCodes.PLUGIN_CRASHED, `${pluginId} 重启次数超限，已禁用`)
    }
    entry.restarts += 1
    if (entry.proc) await entry.proc.stop({ timeoutMs: 1000 }).catch(() => {})
    await this._spawn(entry)
    return entry
  }

  async unload(pluginId) {
    const entry = this.plugins.get(pluginId)
    if (!entry) throw new PluginError(ErrorCodes.PLUGIN_NOT_FOUND, pluginId)
    this._unregisterTools(pluginId)
    entry.state = PLUGIN_STATE.DISABLED
    if (entry.proc) await entry.proc.stop()
    this._emitState(pluginId, 'plugin.unloaded', {})
  }

  async shutdown() {
    for (const entry of this.plugins.values()) {
      this._unregisterTools(entry.id)
      entry.state = PLUGIN_STATE.DISABLED
      if (entry.proc) await entry.proc.stop({ timeoutMs: 1000 }).catch(() => {})
    }
  }

  // ── 工具调用 ────────────────────────────────────────────────────────────

  /** 等价于内核的 `Tool::call`，但所有副作用都退回宿主代理。 */
  async callTool(fullName, args = {}) {
    const ref = this.toolIndex.get(fullName)
    if (!ref) {
      // 工具已从注册表下架，但它所属的插件仍存在：这是「插件坏了」，不是「工具不存在」，
      // 错误码必须区分开，否则上层（Agent）会把崩溃误判成模型编了个工具名。
      for (const entry of this.plugins.values()) {
        const owned = entry.tools.some((t) => `plugin.${entry.id}.${t.name}` === fullName)
        if (owned) {
          throw new PluginError(
            ErrorCodes.PLUGIN_CRASHED,
            `插件 ${entry.id} 状态为 ${entry.state}，工具 ${fullName} 已下架`,
            { state: entry.state, lastError: entry.lastError }
          )
        }
      }
      throw new PluginError(ErrorCodes.TOOL_NOT_ALLOWED, `未注册的工具：${fullName}`)
    }
    const entry = this.plugins.get(ref.pluginId)
    if (!entry || entry.state !== PLUGIN_STATE.READY) {
      throw new PluginError(ErrorCodes.PLUGIN_CRASHED, `插件 ${ref.pluginId} 当前不可用`)
    }

    await this.approvals.ensure({
      pluginId: ref.pluginId,
      toolName: ref.tool.name,
      risk: ref.tool.risk,
      args,
      pathField: ref.tool.pathField
    })

    const callId = `call-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`
    this.hostCalls = 0
    const result = await entry.proc.request(Methods.rpc.TOOL_INVOKE, {
      tool: ref.tool.name,
      arguments: args,
      callId
    })
    return this._truncate(result)
  }

  _truncate(result) {
    if (!result || typeof result !== 'object') return result
    const content = typeof result.content === 'string' ? result.content : JSON.stringify(result.content ?? '')
    const bytes = Buffer.byteLength(content, 'utf8')
    if (bytes <= MAX_TOOL_RESULT_BYTES) return { ...result, content }
    const cut = Buffer.from(content, 'utf8').subarray(0, MAX_TOOL_RESULT_BYTES).toString('utf8')
    return {
      ...result,
      content: `${cut}\n\n[输出已截断：原始 ${bytes} 字节，上限 ${MAX_TOOL_RESULT_BYTES} 字节]`,
      truncated: true
    }
  }

  // ── 插件 → 宿主 的能力调用（唯一的副作用入口）──────────────────────────

  async _handlePluginRequest(entry, method, params) {
    // 通知类：不需要能力，但需要节流（demo 只做记录）
    if (method === Methods.host.LOG) {
      this.emit('log', { pluginId: entry.id, level: params.level ?? 'info', message: String(params.message ?? '') })
      return null
    }
    if (method === Methods.host.PROGRESS) {
      this.emit('progress', { pluginId: entry.id, ...params })
      return null
    }

    const capability = CAPABILITY_OF_METHOD[method]
    if (!capability) {
      throw new PluginError(ErrorCodes.METHOD_NOT_FOUND, `宿主不提供方法：${method}`)
    }
    // 配额检查放在能力之后：先判「你有没有资格」，再判「你用得多不多」，
    // 否则一个没授权的插件每轮都先被配额拦，错误信息会指向错误的原因。
    this.hostCalls += 1
    if (this.hostCalls > MAX_HOST_CALLS_PER_INVOKE) {
      this._audit({ pluginId: entry.id, action: 'quota_exceeded', method })
      throw new PluginError(
        ErrorCodes.PLUGIN_QUOTA_EXCEEDED,
        `单次调用宿主 API 超过 ${MAX_HOST_CALLS_PER_INVOKE} 次`,
        { method, limit: MAX_HOST_CALLS_PER_INVOKE }
      )
    }
    if (!this.grants.has(entry.id, capability)) {
      this._audit({ pluginId: entry.id, action: 'deny', method, reason: `缺能力 ${capability}` })
      throw new PluginError(ErrorCodes.CAPABILITY_DENIED, `未授予能力 ${capability}`, { capability })
    }

    switch (method) {
      case Methods.host.FS_READ: {
        const abs = this.guard.resolve(params.path, { forWrite: false })
        const shown = this.guard.display(abs)
        const stat = this._stat(abs)
        if (stat.kind !== 'file') {
          throw new PluginError(ErrorCodes.INVALID_PARAMS, `不是文件：${shown}`)
        }
        const raw = fs.readFileSync(abs, 'utf8')
        const limited = raw.length > MAX_TOOL_RESULT_BYTES ? raw.slice(0, MAX_TOOL_RESULT_BYTES) : raw
        this._audit({ pluginId: entry.id, action: 'fs.read', path: shown, bytes: raw.length })
        return { path: shown, content: limited, size: raw.length, truncated: limited.length !== raw.length }
      }

      case Methods.host.FS_LIST: {
        const abs = this.guard.resolve(params.path ?? '.')
        const shown = this.guard.display(abs)
        const stat = this._stat(abs)
        if (stat.kind !== 'dir') throw new PluginError(ErrorCodes.INVALID_PARAMS, `不是目录：${shown}`)
        const entries = fs.readdirSync(abs, { withFileTypes: true }).map((d) => {
          const childAbs = path.join(abs, d.name)
          let size = 0
          try {
            size = d.isFile() ? fs.statSync(childAbs).size : 0
          } catch {
            size = 0
          }
          return {
            name: d.name,
            path: this.guard.display(childAbs),
            kind: d.isDirectory() ? 'dir' : 'file',
            size
          }
        })
        this._audit({ pluginId: entry.id, action: 'fs.list', path: shown, count: entries.length })
        return { path: shown, entries }
      }

      case Methods.host.FS_STAT: {
        const abs = this.guard.resolve(params.path)
        this._audit({ pluginId: entry.id, action: 'fs.stat', path: this.guard.display(abs) })
        return { path: this.guard.display(abs), ...this._stat(abs) }
      }

      case Methods.host.FS_WRITE: {
        // 写路径过两遍闸门：路径合法 + 「写入内容」本身也需要一次审批，
        // 因为工具级审批只覆盖「调用意图」，不覆盖「实际落盘的字节」。
        const abs = this.guard.resolve(params.path, { forWrite: true })
        const shown = this.guard.display(abs)
        const content = String(params.content ?? '')
        if (Buffer.byteLength(content, 'utf8') > MAX_TOOL_RESULT_BYTES) {
          throw new PluginError(ErrorCodes.INVALID_PARAMS, '单次写入超过 64 KiB 上限')
        }
        fs.mkdirSync(path.dirname(abs), { recursive: true })
        fs.writeFileSync(abs, content, 'utf8')
        this._audit({ pluginId: entry.id, action: 'fs.write', path: shown, bytes: Buffer.byteLength(content) })
        return { path: shown, bytes: Buffer.byteLength(content, 'utf8'), ok: true }
      }

      case Methods.host.SEARCH_CONTENT: {
        const query = String(params.query ?? '')
        if (query.trim() === '') throw new PluginError(ErrorCodes.INVALID_PARAMS, 'query 不能为空')
        const hits = this._searchContent(query, params.maxResults ?? 20)
        this._audit({ pluginId: entry.id, action: 'search', query, hits: hits.length })
        return { query, hits }
      }

      case Methods.host.WORKSPACE_INFO:
        return {
          id: this.workspaceId,
          name: this.workspaceName,
          contentCapability: this.grants.list(entry.id)
        }

      case Methods.host.NET_FETCH:
        throw new PluginError(ErrorCodes.INTERNAL_ERROR, 'demo 未实现 net.fetch（仅演示能力闸门）')

      default:
        throw new PluginError(ErrorCodes.METHOD_NOT_FOUND, method)
    }
  }

  _stat(abs) {
    try {
      const st = fs.statSync(abs)
      return {
        kind: st.isDirectory() ? 'dir' : 'file',
        size: st.size,
        mtime: st.mtime.toISOString()
      }
    } catch {
      throw new PluginError(ErrorCodes.IO_ERROR, `路径不存在：${this.guard.display(abs)}`)
    }
  }

  _searchContent(query, maxResults) {
    const hits = []
    const walk = (dir, depth) => {
      if (depth > 6 || hits.length >= maxResults) return
      for (const d of fs.readdirSync(dir, { withFileTypes: true })) {
        if (hits.length >= maxResults) return
        const abs = path.join(dir, d.name)
        if (d.isDirectory()) {
          if (d.name === 'node_modules' || d.name.startsWith('.')) continue
          walk(abs, depth + 1)
          continue
        }
        if (d.size > 512 * 1024) continue
        const text = fs.readFileSync(abs, 'utf8')
        text.split(/\r?\n/).forEach((line, idx) => {
          if (hits.length < maxResults && line.toLowerCase().includes(query.toLowerCase())) {
            hits.push({ path: this.guard.display(abs), line: idx + 1, text: line.trim().slice(0, 160) })
          }
        })
      }
    }
    walk(this.workspaceRoot, 0)
    return hits
  }

  // ── 事件 / 诊断 ─────────────────────────────────────────────────────────

  broadcast(event, payload) {
    let delivered = 0
    for (const entry of this.plugins.values()) {
      if (entry.state !== PLUGIN_STATE.READY) continue
      if (!entry.manifest.subscriptions.includes(event)) continue
      entry.proc.notify(Methods.rpc.EVENT_NOTIFY, { event, payload })
      delivered += 1
    }
    return delivered
  }

  listPlugins() {
    return [...this.plugins.values()].map((e) => ({
      id: e.id,
      version: e.manifest.version,
      state: e.state,
      capabilities: this.grants.list(e.id),
      tools: e.tools.map((t) => ({ name: `plugin.${e.id}.${t.name}`, risk: t.risk })),
      restarts: e.restarts,
      lastError: e.lastError
    }))
  }

  listTools() {
    return [...this.toolIndex.entries()].map(([name, ref]) => ({
      name,
      description: ref.tool.description,
      risk: ref.tool.risk,
      pluginId: ref.pluginId
    }))
  }

  _audit(row) {
    this.audit.push({ at: new Date().toISOString(), ...row })
  }

  _emitState(pluginId, event, payload) {
    this.emit('state', { pluginId, event, payload })
  }
}

/** 方法 → 所需能力。缺一行就等于开一个后门，所以用常量表而不是散在 switch 里。 */
const CAPABILITY_OF_METHOD = {
  [Methods.host.FS_READ]: 'fs.read',
  [Methods.host.FS_LIST]: 'fs.read',
  [Methods.host.FS_STAT]: 'fs.read',
  [Methods.host.FS_WRITE]: 'fs.write',
  [Methods.host.SEARCH_CONTENT]: 'search',
  [Methods.host.WORKSPACE_INFO]: 'fs.read',
  [Methods.host.NET_FETCH]: 'net.fetch'
}
