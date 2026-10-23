/**
 * M7 渲染侧 MCP 纯函数测试。
 *
 * 这些函数是「错误码 → 用户能看懂的话」的唯一出口，也是参数输入的唯一校验点，
 * 所以单独钉住它们，而不是全部塞进 store 测试里间接验证。
 *
 * 反过来，样式与排版一律不测——那些靠人眼。
 */

import { describe, expect, it } from 'vitest'

import type { McpCallResult } from '@ai-ide/shared-types'

import {
  callResultText,
  describeMcpError,
  formatBytes,
  isConfigError,
  mcpErrorCode,
  mcpHintForError,
  parseToolArguments,
  serverStateLabel
} from './mcp-api'

describe('错误码解析', () => {
  it('从主进程文案里抠出协议码', () => {
    expect(mcpErrorCode('请求失败：[8002] 连接被拒')).toBe(8002)
    expect(mcpErrorCode('[8001] 未配置')).toBe(8001)
    expect(mcpErrorCode('没有码的普通错误')).toBeNull()
    // 不是数字的不算；宁可当作未知错误，也不猜一个码出来。
    expect(mcpErrorCode('[abc] 未知')).toBeNull()
  })

  it('每个 M7 错误码都有对应的人话', () => {
    for (const code of [8001, 8002, 8003, 8004, 8005, 8006, 8007]) {
      expect(mcpHintForError(code).length).toBeGreaterThan(0)
    }
    // 未知码返回空串：调用方据此回落到原始 message，而不是编一句话。
    expect(mcpHintForError(9999)).toBe('')
  })

  it('把「要用户动手」和「再点一次就行」分开', () => {
    // 配置 / 参数问题：重试没用
    expect(isConfigError(8001)).toBe(true)
    expect(isConfigError(8002)).toBe(true)
    expect(isConfigError(8007)).toBe(true)
    // 未连接 / 工具不存在 / 执行失败 / 超时：可以靠交互恢复
    expect(isConfigError(8003)).toBe(false)
    expect(isConfigError(8004)).toBe(false)
    expect(isConfigError(8005)).toBe(false)
    expect(isConfigError(8006)).toBe(false)
    expect(isConfigError(null)).toBe(false)
  })

  it('describeMcpError 保留原文并拼上人话', () => {
    const text = describeMcpError(new Error('[8003] server not connected'))
    expect(text).toContain('[8003]')
    expect(text).toContain('还没连接')

    // 未知码：只有原文，不硬凑解释
    expect(describeMcpError(new Error('boom'))).toBe('boom')
    // 非 Error 也要能处理（IPC 层偶尔抛字符串）
    expect(describeMcpError('plain')).toBe('plain')
  })
})

describe('状态标记', () => {
  it('认得内核那五个状态', () => {
    expect(serverStateLabel('configured')).toBe('未连接')
    expect(serverStateLabel('connecting')).toBe('连接中')
    expect(serverStateLabel('ready')).toBe('已连接')
    expect(serverStateLabel('stale')).toBe('连接过期')
    expect(serverStateLabel('closed')).toBe('已断开')
  })

  it('认不出的状态原样显示，不藏', () => {
    expect(serverStateLabel('handshaking')).toBe('handshaking')
  })
})

describe('参数解析', () => {
  it('空输入当空对象', () => {
    expect(parseToolArguments('')).toEqual({ ok: true, value: {} })
    expect(parseToolArguments('   \n ')).toEqual({ ok: true, value: {} })
  })

  it('接受 JSON 对象', () => {
    const parsed = parseToolArguments('{"path":"README.md","limit":3}')
    expect(parsed.ok).toBe(true)
    if (parsed.ok) expect(parsed.value).toEqual({ path: 'README.md', limit: 3 })
  })

  it('语法错误就说语法错误', () => {
    const parsed = parseToolArguments('{"path":}')
    expect(parsed.ok).toBe(false)
    if (!parsed.ok) expect(parsed.reason).toContain('不是合法 JSON')
  })

  it('非对象一律拒绝：数组 / null / 字符串', () => {
    for (const text of ['[1,2]', 'null', '"x"', '42']) {
      const parsed = parseToolArguments(text)
      expect(parsed.ok).toBe(false)
      if (!parsed.ok) expect(parsed.reason).toContain('必须是 JSON 对象')
    }
  })
})

describe('调用结果正文', () => {
  const base: McpCallResult = {
    server: 'fs',
    tool: 'read',
    namespaced_name: 'mcp__fs__read',
    content: [],
    text: 'hello',
    is_error: false
  }

  it('优先用内核拼好的 text', () => {
    expect(callResultText(base)).toBe('hello')
  })

  it('text 为空时按内容块回落', () => {
    const result: McpCallResult = {
      ...base,
      text: '',
      content: [
        { kind: 'text', text: 'a' },
        { kind: 'text', text: 'b' }
      ]
    }
    expect(callResultText(result)).toBe('a\nb')
  })

  it('二进制块只说类别与量级，不塞 base64', () => {
    const data = 'A'.repeat(4096) // base64 长度 → 3/4 字节
    const result: McpCallResult = {
      ...base,
      text: '',
      content: [{ kind: 'image', data, mime_type: 'image/png' }]
    }
    const text = callResultText(result)
    expect(text).toContain('image')
    expect(text).toContain('image/png')
    expect(text).toContain('3.0 KB')
    expect(text).not.toContain(data)
  })

  it('资源链接带上名字与 URI', () => {
    const result: McpCallResult = {
      ...base,
      text: '',
      content: [{ kind: 'resource_link', name: 'a.txt', uri: 'file:///a.txt' }]
    }
    expect(callResultText(result)).toBe('[资源链接 a.txt file:///a.txt]')
  })

  it('彻底空的结果也要说一句', () => {
    expect(callResultText({ ...base, text: '', content: [] })).toBe('（Server 返回了空结果）')
  })
})

describe('字节格式化', () => {
  it('三个量级', () => {
    expect(formatBytes(12)).toBe('12 B')
    expect(formatBytes(2048)).toBe('2.0 KB')
    expect(formatBytes(3 * 1024 * 1024)).toBe('3.0 MB')
  })
})
