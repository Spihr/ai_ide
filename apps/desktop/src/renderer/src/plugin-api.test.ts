/**
 * P1 渲染侧插件纯函数 + API 封装测试。
 *
 * 与 `mcp-api.test.ts` 同一刀口：这里钉的是「错误码 → 用户能看懂的话」以及
 * **6107 的 details 能不能被完整拆出来**——那是 P1 的验收点（一次列全所有清单
 * 问题），面板只负责渲染，拆不出来就等于没实现。
 *
 * P2 补的是另外两件事：装载三条（`load` / `unload` / `reload`）的参数形状，以及
 * 6102/6103/6105/6106 这几个「装载期错误」也得有人说人话——用户在面板上点「加载」
 * 失败时，看到的不能是裸码。
 *
 * 样式一律不测。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'

import { Methods } from '@ai-ide/shared-types'

import type { AiIdeApi } from '../../shared/ipc'
import { encodeRpcErrorPayload } from '../../shared/ipc'
import {
  describePluginError,
  pluginApi,
  pluginCapabilityLabel,
  pluginErrorCode,
  pluginErrorIssues,
  pluginErrorMessage,
  pluginHintForError,
  pluginIssueKindLabel,
  pluginStateLabel,
  pluginStateTone
} from './plugin-api'

interface Call {
  method: string
  params: Record<string, unknown> | undefined
}

function installHarness(): Call[] {
  const calls: Call[] = []
  const api = {
    request: async (method: string, params?: unknown) => {
      calls.push({ method, params: params as Record<string, unknown> | undefined })
      if (method === Methods.plugin.list) return { plugins: [] }
      if (method === Methods.plugin.install) {
        return { plugin: {}, replaced: false, installed_dir: 'C:\\ud\\plugins\\x' }
      }
      if (method === Methods.plugin.status) return { plugin: {}, tools: [], commands: [] }
      if (method === Methods.plugin.uninstall) return { plugin_id: 'x', removed_dir: true }
      if (method === Methods.plugin.load) return { plugin: { name: 'x' }, tools: [] }
      if (method === Methods.plugin.reload) return { plugin: { name: 'x' }, tools: [] }
      if (method === Methods.plugin.unload) return { plugin_id: 'x', removed_tools: 0 }
      throw new Error(`测试收到了意外方法：${method}`)
    }
  } as unknown as AiIdeApi

  vi.stubGlobal('window', { aiide: api })
  return calls
}

beforeEach(() => {
  vi.unstubAllGlobals()
})

describe('错误码解析', () => {
  it('从主进程文案里抠出协议码', () => {
    expect(pluginErrorCode('[6107] 清单不合法')).toBe(6107)
    expect(pluginErrorCode('[6101] plugin not found')).toBe(6101)
    expect(pluginErrorCode('没有码的普通错误')).toBeNull()
    expect(pluginErrorCode('[abc] 未知')).toBeNull()
  })

  it('P1/P2 相关错误码都有对应的人话', () => {
    for (const code of [6101, 6102, 6103, 6104, 6105, 6106, 6107, 6108, 6109, 5003, -32602]) {
      expect(pluginHintForError(code).length, `${code} 缺少提示`).toBeGreaterThan(0)
    }
    // 未知码返回空串：调用方据此回落到原文，而不是硬凑一句解释
    expect(pluginHintForError(9999)).toBe('')
    expect(pluginHintForError(null)).toBe('')
  })

  it('装载期错误各自指向不同的补救动作', () => {
    // 三条最容易混的：没授权 / 起不来 / 超时。文案必须能区分「去找授权」
    // 「看清单和路径」「这台机器太慢或插件卡住」。
    expect(pluginHintForError(6103)).toContain('授权')
    expect(pluginHintForError(6105)).toContain('超时')
    expect(pluginHintForError(6106)).toContain('协议')
    const hints = new Set([6103, 6105, 6106].map((code) => pluginHintForError(code)))
    expect(hints.size).toBe(3)
  })

  it('describePluginError 保留原文并拼上人话', () => {
    const text = describePluginError(new Error('[6107] manifest invalid'))
    expect(text).toContain('[6107]')
    expect(text).toContain('清单不合法')

    expect(describePluginError(new Error('boom'))).toBe('boom')
    expect(describePluginError('plain')).toBe('plain')
  })
})

describe('6107 的 details 明细', () => {
  /** 内核 6107 的真实形态：message + `data.details[]`，跨 IPC 后编码进 message 尾部。 */
  function manifestError(details: unknown): Error {
    const message = encodeRpcErrorPayload({
      code: 6107,
      message: '[6107] manifest invalid',
      data: { details }
    })
    return new Error(message)
  }

  it('一次拆出全部问题，不截断', () => {
    const error = manifestError([
      { code: 6107, path: '$.id', message: 'id 必须是小写字母开头' },
      { code: 6107, path: '$.version', message: '缺少 version' },
      { code: 6104, path: '$.pluginApi', message: '宿主 API 不兼容' },
      { code: 6108, path: '$.tools[1].name', message: '与核心工具撞名' }
    ])

    const issues = pluginErrorIssues(error)
    expect(issues).toHaveLength(4)
    expect(issues.map((issue) => issue.code)).toEqual([6107, 6107, 6104, 6108])
    expect(issues[3]?.path).toBe('$.tools[1].name')
    expect(issues[3]?.message).toContain('撞名')
  })

  it('信封尾巴不会漏进用户看到的文案', () => {
    const text = pluginErrorMessage(manifestError([{ code: 6107, path: '$.id', message: 'x' }]))
    expect(text).toBe('[6107] manifest invalid')
    expect(text).not.toContain('details')
  })

  it('三种「拿不到明细」都回空数组，交给调用方回落', () => {
    // 不是信封：老路径 / 非内核错误
    expect(pluginErrorIssues(new Error('[6107] manifest invalid'))).toEqual([])
    // 信封里没有 data：6101 这类单点错误
    expect(pluginErrorIssues(new Error(encodeRpcErrorPayload({ code: 6101, message: 'gone' }))))
      .toEqual([])
    // details 不是数组：形状不认识时宁可少列，也不塞半截 JSON
    expect(pluginErrorIssues(new Error(encodeRpcErrorPayload({ code: 6107, message: 'x', data: { details: 'oops' } }))))
      .toEqual([])
  })

  it('单条 issue 字段缺失时给保守默认值', () => {
    const issues = pluginErrorIssues(manifestError([{ message: '只有说明' }, 'not-an-object']))
    expect(issues).toHaveLength(1)
    expect(issues[0]).toEqual({ code: 6107, path: '', message: '只有说明' })
  })

  it('细分码翻成人话标签，认不出的原样显示', () => {
    expect(pluginIssueKindLabel(6107)).toBe('清单')
    expect(pluginIssueKindLabel(6104)).toBe('API 不兼容')
    expect(pluginIssueKindLabel(6108)).toBe('工具撞名')
    expect(pluginIssueKindLabel(6200)).toBe('错误 6200')
  })
})

describe('状态与能力标记', () => {
  it('认得内核那五个状态', () => {
    expect(pluginStateLabel('disabled')).toBe('已停用')
    expect(pluginStateLabel('needs_grant')).toBe('待授权')
    expect(pluginStateLabel('loading')).toBe('加载中')
    expect(pluginStateLabel('ready')).toBe('运行中')
    expect(pluginStateLabel('crashed')).toBe('已崩溃')
    expect(pluginStateLabel('handshaking')).toBe('handshaking')
  })

  it('配色只有 ready 是绿的，crashed 红', () => {
    expect(pluginStateTone('ready')).toBe('ok')
    expect(pluginStateTone('needs_grant')).toBe('warn')
    expect(pluginStateTone('crashed')).toBe('bad')
    expect(pluginStateTone('disabled')).toBe('idle')
    expect(pluginStateTone('loading')).toBe('idle')
  })

  it('未授予的能力加问号，已授予的原样', () => {
    expect(pluginCapabilityLabel('fs:read', ['fs:read'])).toBe('fs:read')
    expect(pluginCapabilityLabel('net:http', ['fs:read'])).toBe('net:http?')
    expect(pluginCapabilityLabel('net:http', [])).toBe('net:http?')
  })
})

describe('API 封装', () => {
  it('七个方法各发各的协议名与参数', async () => {
    const calls = installHarness()

    await pluginApi.list()
    await pluginApi.status('hello.world')
    await pluginApi.install('E:\\my-plugins\\hello')
    await pluginApi.uninstall('hello.world')
    await pluginApi.load('hello.world')
    await pluginApi.unload('hello.world')
    await pluginApi.reload('hello.world')

    expect(calls.map((call) => call.method)).toEqual([
      Methods.plugin.list,
      Methods.plugin.status,
      Methods.plugin.install,
      Methods.plugin.uninstall,
      Methods.plugin.load,
      Methods.plugin.unload,
      Methods.plugin.reload
    ])
    // 参数用协议里的字段名（snake_case），组件不自己拼
    expect(calls[1]?.params).toEqual({ plugin_id: 'hello.world' })
    expect(calls[2]?.params).toEqual({ path: 'E:\\my-plugins\\hello' })
    expect(calls[3]?.params).toEqual({ plugin_id: 'hello.world' })
    // 装载三条只认 plugin_id：可执行文件与参数由账本里的 install_dir 决定，
    // 渲染进程没有机会指向别的东西。
    expect(calls[4]?.params).toEqual({ plugin_id: 'hello.world' })
    expect(calls[5]?.params).toEqual({ plugin_id: 'hello.world' })
    expect(calls[6]?.params).toEqual({ plugin_id: 'hello.world' })
  })

  it('不碰 call / grant / revoke', async () => {
    const calls = installHarness()
    const allowed = new Set<string>([
      Methods.plugin.install,
      Methods.plugin.list,
      Methods.plugin.status,
      Methods.plugin.uninstall,
      Methods.plugin.load,
      Methods.plugin.unload,
      Methods.plugin.reload
    ])

    await pluginApi.list()
    await pluginApi.status('x.y')
    await pluginApi.install('E:\\p')
    await pluginApi.uninstall('x.y')
    await pluginApi.load('x.y')
    await pluginApi.unload('x.y')
    await pluginApi.reload('x.y')

    for (const call of calls) {
      expect(allowed.has(call.method), `${call.method} 不该由当前面板发起`).toBe(true)
    }
  })
})
