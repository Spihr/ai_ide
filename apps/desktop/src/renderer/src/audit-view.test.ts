/**
 * 审计回执展示层测试。
 *
 * `audit-view.ts` 里全是纯函数，这里钉的是三件容易「看起来没问题、排查时却读不出东西」的事：
 * 未知 kind 不能变成空白行、插件明细缺字段不能编占位值、被拒裁决只认内核写下的 `decision`。
 */

import { describe, expect, it } from 'vitest'

import type { AuditEventView } from '@ai-ide/shared-types'

import {
  AUDIT_KIND_FILTERS,
  auditKindLabel,
  isDeniedDecision,
  pluginAuditFacts,
  pluginAuditLine
} from './audit-view'

function event(kind: string, detail: unknown, summary = kind): AuditEventView {
  return {
    event_id: 1,
    kind,
    session_id: null,
    actor: 'core',
    summary,
    detail: detail as AuditEventView['detail'],
    created_at_ms: 1
  }
}

describe('kind 标签', () => {
  it('插件与会话的已知 kind 都有中文短标签', () => {
    expect(auditKindLabel('plugin.capability_changed')).toBe('能力变更')
    expect(auditKindLabel('plugin.call')).toBe('插件调用')
    expect(auditKindLabel('plugin.lifecycle')).toBe('插件生命周期')
    expect(auditKindLabel('plugin.host_call')).toBe('插件宿主调用')
    expect(auditKindLabel('session.created')).toBe('会话创建')
  })

  it('未收录的 kind 原样显示：新 kind 不该在界面上变成空白行', () => {
    expect(auditKindLabel('plugin.future_kind')).toBe('plugin.future_kind')
    expect(auditKindLabel('')).toBe('')
  })
})

describe('插件审计字段', () => {
  it('五个基础字段齐全时逐个抠出来', () => {
    const facts = pluginAuditFacts(
      event('plugin.call', {
        pluginId: 'demo',
        action: 'invoke',
        target: 'tool:fs.read',
        decision: 'allow',
        callId: 'call-1',
        // 内核多写的字段不该被带出去：面板只认这五个。
        extra: 'ignored'
      })
    )

    expect(facts).toEqual({
      pluginId: 'demo',
      action: 'invoke',
      target: 'tool:fs.read',
      decision: 'allow',
      callId: 'call-1'
    })
  })

  it('非插件事件返回 null：调用方继续用 summary', () => {
    expect(pluginAuditFacts(event('session.created', { session_id: 's-1' }))).toBeNull()
    expect(pluginAuditFacts(event('message.appended', null))).toBeNull()
  })

  it('缺字段、空串、纯空白、非字符串一律 null，不编占位值', () => {
    const facts = pluginAuditFacts(
      event('plugin.capability_changed', {
        pluginId: '  demo  ',
        action: '   ',
        target: null,
        decision: 42,
        callId: undefined
      })
    )

    // 有值的保留（去掉两端空白），其余留空。
    expect(facts).toEqual({
      pluginId: 'demo',
      action: null,
      target: null,
      decision: null,
      callId: null
    })
  })

  it('detail 为 null 时五个字段全空', () => {
    expect(pluginAuditFacts(event('plugin.lifecycle', null))).toEqual({
      pluginId: null,
      action: null,
      target: null,
      decision: null,
      callId: null
    })
  })

  it('字段是嵌套对象时不算文本，归 null', () => {
    const facts = pluginAuditFacts(
      event('plugin.call', { pluginId: { name: 'demo' }, action: ['invoke'] })
    )

    expect(facts?.pluginId).toBeNull()
    expect(facts?.action).toBeNull()
  })
})

describe('审计第二行', () => {
  it('按「插件 · 动作 · 目标 · → 裁决」拼，callId 放括号里', () => {
    expect(
      pluginAuditLine(
        event('plugin.call', {
          pluginId: 'demo',
          action: 'invoke',
          target: 'tool:fs.read',
          decision: 'allow',
          callId: 'call-1'
        })
      )
    ).toBe('demo · invoke · tool:fs.read · → allow（call-1）')
  })

  it('缺字段时只拼存在的部分：宁可短也不写「未知」', () => {
    expect(pluginAuditLine(event('plugin.capability_changed', { pluginId: 'demo' }))).toBe('demo')
    expect(
      pluginAuditLine(event('plugin.capability_changed', { pluginId: 'demo', decision: 'deny' }))
    ).toBe('demo · → deny')
    // 只有 callId 时不拼行：括号里孤零零一个号读不出「谁干了什么」，退回 summary 更清楚。
    expect(pluginAuditLine(event('plugin.call', { callId: 'call-9' }))).toBeNull()
  })

  it('一个字段都没有时返回 null：让调用方退回 summary，而不是渲染空行', () => {
    expect(pluginAuditLine(event('plugin.call', {}))).toBeNull()
    expect(pluginAuditLine(event('session.created', { session_id: 's-1' }))).toBeNull()
  })
})

describe('被拒裁决', () => {
  it('闸门拒 deny 与写侧审批被驳回 reject 都算被拒', () => {
    expect(isDeniedDecision(event('plugin.call', { decision: 'deny' }))).toBe(true)
    expect(isDeniedDecision(event('plugin.host_call', { decision: 'reject' }))).toBe(true)
  })

  it('大小写不敏感，并认 *_denied / *_rejected 历史写法', () => {
    // `denied` 不含 `deny` 子串，旧的 includes('deny') 会漏掉这类值。
    expect(isDeniedDecision(event('plugin.call', { decision: 'DENY' }))).toBe(true)
    expect(isDeniedDecision(event('plugin.call', { decision: 'update_denied' }))).toBe(true)
    expect(isDeniedDecision(event('plugin.call', { decision: 'approval_rejected' }))).toBe(true)
  })

  it('放行、配额超限、执行失败、缺 decision、非插件事件都不算被拒', () => {
    expect(isDeniedDecision(event('plugin.call', { decision: 'allow' }))).toBe(false)
    expect(isDeniedDecision(event('plugin.call', { decision: 'approve' }))).toBe(false)
    // 这两个是「放行后没跑完」，不是裁决拒绝，不该跟着标红。
    expect(isDeniedDecision(event('plugin.call', { decision: 'quota_exceeded' }))).toBe(false)
    expect(isDeniedDecision(event('plugin.call', { decision: 'error' }))).toBe(false)
    expect(isDeniedDecision(event('plugin.call', { pluginId: 'demo' }))).toBe(false)
    expect(isDeniedDecision(event('session.created', { decision: 'deny' }))).toBe(false)
  })
})

describe('类别过滤器', () => {
  it('首项是不筛（kind 为 null），并覆盖能力变更与调用两类', () => {
    expect(AUDIT_KIND_FILTERS[0]).toEqual({ kind: null, label: '全部' })
    expect(AUDIT_KIND_FILTERS.map((item) => item.kind)).toEqual([
      null,
      'plugin.capability_changed',
      'plugin.call'
    ])
  })
})
