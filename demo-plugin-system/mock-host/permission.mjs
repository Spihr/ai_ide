/**
 * 能力授权表 + 工具调用审批。
 *
 * 这里实现的是「第二道闸门（能力）」和「第四道闸门（资源/审批）」，
 * 对应内核的 `PermissionGate`。两条规则与 docs/02 §8 保持一致：
 *
 * - 能力必须在**加载前**授予，运行中调未授权能力直接 6003，不弹窗、不降级；
 * - 工具按 risk 分档：read 直接过，write / exec 必须审批。
 *   read 不审批不是偷懒，而是「工作区内只读」本身就是基线权限。
 *
 * demo 里的审批人是脚本化的 Decider，真实实现走 `ask_user` 卡片。
 */

import { CAPABILITIES } from './manifest.mjs'
import { ErrorCodes, PluginError } from './protocol.mjs'

/** 工具 risk → 审批策略 */
const RISK_POLICY = {
  read: { approval: 'none', reason: '工作区只读属基线权限' },
  write: { approval: 'ask', reason: '写入会改变工作区内容' },
  exec: { approval: 'ask', reason: '执行外部进程会突破路径/能力闸门' }
}

export class GrantStore {
  constructor() {
    /** @type {Map<string, Set<string>>} */
    this.grants = new Map()
    /** @type {Array<object>} 审计行：谁、什么时候、因为什么拿到能力 */
    this.audit = []
  }

  grant(pluginId, capabilities, { by = 'user', reason = 'install' } = {}) {
    const accepted = []
    const rejected = []
    for (const cap of capabilities) {
      if (!CAPABILITIES.has(cap)) {
        rejected.push(cap)
        continue
      }
      const set = this.grants.get(pluginId) ?? new Set()
      set.add(cap)
      this.grants.set(pluginId, set)
      accepted.push(cap)
    }
    if (accepted.length > 0) {
      this.audit.push({
        at: new Date().toISOString(),
        action: 'grant',
        pluginId,
        capabilities: accepted,
        by,
        reason
      })
    }
    if (rejected.length > 0) {
      throw new PluginError(ErrorCodes.INVALID_PARAMS, `未知能力：${rejected.join(', ')}`)
    }
    return accepted
  }

  revoke(pluginId, capabilities) {
    const set = this.grants.get(pluginId)
    if (!set) return []
    const removed = []
    for (const cap of capabilities) {
      if (set.delete(cap)) removed.push(cap)
    }
    this.audit.push({
      at: new Date().toISOString(),
      action: 'revoke',
      pluginId,
      capabilities: removed,
      by: 'user'
    })
    return removed
  }

  has(pluginId, capability) {
    return this.grants.get(pluginId)?.has(capability) ?? false
  }

  list(pluginId) {
    return [...(this.grants.get(pluginId) ?? [])].sort()
  }

  /** 加载后校验：清单声明的每个能力是否都已授予，缺一个都拒载。 */
  missingFor(pluginId, capabilities) {
    return capabilities.filter((cap) => !this.has(pluginId, cap))
  }
}

/**
 * 审批队列。key 用 `pluginId::toolName::scope`，
 * scope 取工具声明的 pathField（通常是 `path`），
 * 这样「批准一次」的作用域是具体文件，而不是整个插件——
 * 与内核 `scope_key` 的思路一致，避免一次授权覆盖全工作区。
 */
export class ApprovalQueue {
  /**
   * @param {(req: object) => Promise<{approved: boolean, remember?: boolean, note?: string}>} decider
   */
  constructor(decider, { timeoutMs = 30000 } = {}) {
    this.decider = decider
    this.timeoutMs = timeoutMs
    this.remembered = new Set()
    this.history = []
  }

  static scopeKey(pluginId, toolName, args, pathField) {
    const scope = pathField && args ? String(args[pathField] ?? '') : ''
    return `${pluginId}::${toolName}::${scope}`
  }

  async ensure({ pluginId, toolName, risk, args, pathField }) {
    const policy = RISK_POLICY[risk] ?? RISK_POLICY.write
    if (policy.approval === 'none') {
      this.history.push({ pluginId, toolName, decision: 'allow', why: policy.reason })
      return { approved: true, decision: 'allow', why: policy.reason }
    }

    const key = ApprovalQueue.scopeKey(pluginId, toolName, args, pathField)
    if (this.remembered.has(key)) {
      this.history.push({ pluginId, toolName, decision: 'allow', why: '本次会话已记住' })
      return { approved: true, decision: 'allow-remembered', why: '本次会话已记住' }
    }

    const request = { pluginId, toolName, risk, args, scope: pathField && args?.[pathField], reason: policy.reason }

    let timer
    const timeout = new Promise((resolve) => {
      timer = setTimeout(() => resolve({ approved: false, note: '审批超时，按拒绝处理' }), this.timeoutMs)
      if (typeof timer.unref === 'function') timer.unref()
    })
    const answer = await Promise.race([this.decider(request), timeout])
    clearTimeout(timer)

    const approved = answer?.approved === true
    if (approved && answer.remember) this.remembered.add(key)
    this.history.push({
      pluginId,
      toolName,
      decision: approved ? 'allow' : 'deny',
      why: answer?.note ?? policy.reason
    })
    if (!approved) {
      throw new PluginError(
        ErrorCodes.TOOL_NOT_ALLOWED,
        `用户拒绝执行插件工具 ${pluginId}.${toolName}`,
        { note: answer?.note }
      )
    }
    return { approved: true, decision: 'allow', why: answer?.note ?? policy.reason }
  }
}

export { RISK_POLICY }
