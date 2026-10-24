/**
 * 审计回执的展示层规则（纯函数，可直接单测）。
 *
 * 内核把所有审计行的明细压成同一套字段（`crates/core/src/plugin_audit.rs::BASE_FIELDS`）：
 * `pluginId` / `action` / `target` / `decision` / `callId`。所以这里只需要一份渲染
 * 逻辑就能覆盖能力变更、插件调用、生命周期三类事件，不必为每个 kind 写一个分支。
 *
 * 为什么要在外壳再翻一遍：`summary` 是给人读的一行（`插件 demo grant：allow`），
 * 但它把「撤的是哪项能力」「被拒的是哪个工具」压进了同一句，排查时看不出差别。
 * 明细里的 `target` / `decision` / `callId` 才是能对号入座的字段，缺了就留空——
 * **不要**自己编一个占位值，那会让人以为内核真写了这个字段。
 */

import type { AuditEventView } from '@ai-ide/shared-types'

/** 审计 kind 的中文短标签；未收录的原样显示，避免新 kind 变成空白行。 */
const KIND_LABELS: Record<string, string> = {
  'plugin.capability_changed': '能力变更',
  'plugin.call': '插件调用',
  'plugin.lifecycle': '插件生命周期',
  'plugin.host_call': '插件宿主调用',
  'session.created': '会话创建',
  'session.deleted': '会话删除',
  'session.exported': '会话导出',
  'session.forked': '会话分叉'
}

export function auditKindLabel(kind: string): string {
  return KIND_LABELS[kind] ?? kind
}

/** 一行插件审计里能被单独拎出来显示的三个字段。 */
export interface PluginAuditFacts {
  pluginId: string | null
  action: string | null
  target: string | null
  decision: string | null
  callId: string | null
}

function text(value: unknown): string | null {
  if (typeof value !== 'string') return null
  const trimmed = value.trim()
  return trimmed.length === 0 ? null : trimmed
}

/** 从 `detail` 里抠出插件审计的五个基础字段；缺字段一律 `null`。 */
export function pluginAuditFacts(event: AuditEventView): PluginAuditFacts | null {
  if (!event.kind.startsWith('plugin.')) return null
  const detail = (event.detail ?? {}) as Record<string, unknown>
  return {
    pluginId: text(detail.pluginId),
    action: text(detail.action),
    target: text(detail.target),
    decision: text(detail.decision),
    callId: text(detail.callId)
  }
}

/**
 * 审计行的第二行文字：把「谁 · 干了什么 · 对什么 · 结果」摊开。
 *
 * 非插件事件返回 `null`（调用方继续用 `summary`），插件事件缺字段时只拼存在的
 * 部分——宁可短，也不要写「未知」去冒充内核数据。
 */
export function pluginAuditLine(event: AuditEventView): string | null {
  const facts = pluginAuditFacts(event)
  if (facts === null) return null
  const parts: string[] = []
  if (facts.pluginId !== null) parts.push(facts.pluginId)
  if (facts.action !== null) parts.push(facts.action)
  if (facts.target !== null) parts.push(facts.target)
  if (facts.decision !== null) parts.push(`→ ${facts.decision}`)
  const callId = facts.callId
  return parts.length === 0 ? null : parts.join(' · ') + (callId ? `（${callId}）` : '')
}

/**
 * 内核 `decision` 里属于「被拒」的两种：闸门拒（`DECISION_DENY`）与写侧审批被驳回
 * （`DECISION_REJECT`，`crates/core/src/plugin_host.rs`）。`quota_exceeded` / `error`
 * 是「放行后没跑完」，不是裁决拒绝，不在这里标红。
 */
const DENIED_DECISIONS: ReadonlySet<string> = new Set(['deny', 'reject'])

/**
 * 被拒的裁决要能一眼看见。
 *
 * 只认内核写下的 `decision`：`deny` / `reject`。前端不拿错误码猜——3001 是「写侧
 * 审批被拒」的码，但它只出现在调用返回里，审计行上只有 `decision`。
 *
 * 注意不能用 `includes('deny')` 兜底：`denied` 里没有 `deny` 这个子串（`deni` 之后
 * 是 `ed`），历史写法 `update_denied` 会被漏掉，所以这里额外认 `*_denied` / `*_rejected`。
 */
export function isDeniedDecision(event: AuditEventView): boolean {
  const facts = pluginAuditFacts(event)
  if (facts === null || facts.decision === null) return false
  const decision = facts.decision.toLowerCase()
  return (
    DENIED_DECISIONS.has(decision) ||
    decision.endsWith('_denied') ||
    decision.endsWith('_rejected')
  )
}

/** 审计类别过滤器的可选项：`null` 表示不筛。 */
export const AUDIT_KIND_FILTERS: ReadonlyArray<{ kind: string | null; label: string }> = [
  { kind: null, label: '全部' },
  { kind: 'plugin.capability_changed', label: '能力变更' },
  { kind: 'plugin.call', label: '插件调用' }
]
