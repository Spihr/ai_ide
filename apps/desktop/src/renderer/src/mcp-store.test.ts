/**
 * M7 渲染侧 MCP 状态测试。
 *
 * 与 `checkpoint-store.test.ts` 同一套路：不渲染 React，直接驱动动作函数，用假的
 * `window.aiide` 顶替 IPC，断言「发了什么请求」与「状态怎么变」。
 *
 * 钉住的是这个面板最容易出错、错了却像「内核没生效」的几条：
 *
 * - 一个 Server 都没连时不该去打 `mcp.tools`（白白多一次请求）；
 * - 选中态是「跟着工具清单活」的：工具没了必须一起清掉，否则点执行拿到 `8004`；
 * - 参数写坏时**不发请求**，错误必须落在本地；
 * - 断开是幂等的：`closed = false` 要如实说「本来就是断开状态」，不能当失败；
 * - 失败时 `busyServer` 必须复位，否则那一行按钮永久禁用。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  Methods,
  type McpCallResult,
  type McpConnectResult,
  type McpDisconnectResult,
  type McpServerInfo,
  type McpToolInfo
} from '@ai-ide/shared-types'

import type { AiIdeApi } from '../../shared/ipc'
import { callResultText } from './mcp-api'
import {
  callSelectedTool,
  connectServer,
  disconnectServer,
  getMcpState,
  initMcp,
  refreshMcp,
  resetMcpState,
  selectTool,
  setArgsText
} from './mcp-store'

interface Call {
  method: string
  params: Record<string, unknown> | undefined
}

interface Harness {
  calls: Call[]
  /** 让下一次请求失败，并支持指定方法。 */
  failNext: (message: string, method?: string) => void
  /** 改写 `mcp.servers` 的返回。 */
  setServers: (servers: McpServerInfo[]) => void
  /** 改写 `mcp.tools` 的返回。 */
  setTools: (tools: McpToolInfo[]) => void
  /** 改写下一次 `mcp.call` 的返回。 */
  setCallResult: (result: McpCallResult) => void
}

function server(name: string, patch: Partial<McpServerInfo> = {}): McpServerInfo {
  return {
    name,
    transport: 'stdio',
    configured: true,
    connected: false,
    state: 'configured',
    tool_count: 0,
    last_error: undefined,
    ...patch
  }
}

function tool(serverName: string, name: string, description?: string): McpToolInfo {
  return {
    name,
    namespaced_name: `mcp__${serverName}__${name}`,
    server: serverName,
    description,
    input_schema: { type: 'object' }
  }
}

const CALL_OK: McpCallResult = {
  server: 'fs',
  tool: 'read',
  namespaced_name: 'mcp__fs__read',
  content: [{ kind: 'text', text: 'file body' }],
  text: 'file body',
  is_error: false
}

function installHarness(): Harness {
  const calls: Call[] = []
  let failure: { message: string; method: string | null } | null = null
  let servers: McpServerInfo[] = [
    server('fs'),
    server('git', { transport: 'http', connected: true, state: 'ready', tool_count: 1 })
  ]
  let tools: McpToolInfo[] = [tool('git', 'status', '看工作区状态')]
  let callResult: McpCallResult = CALL_OK

  const api: AiIdeApi = {
    getCoreStatus: async () => {
      throw new Error('测试未使用 getCoreStatus')
    },
    restartCore: async () => {
      throw new Error('测试未使用 restartCore')
    },
    ping: async () => {
      throw new Error('测试未使用 ping')
    },
    pickWorkspaceDirectory: async () => null,
    pickSavePath: async () => null,
    // MCP 面板不碰事件流与密钥托管：给空订阅，把接口凑齐即可。
    onCoreStatus: () => () => undefined,
    onCoreLog: () => () => undefined,
    onWorkspaceChanged: () => () => undefined,
    onWorkspaceIndexed: () => () => undefined,
    onAiEvent: () => () => undefined,
    onCredentialStatus: () => () => undefined,
    onTerminalData: () => () => undefined,
    onTerminalExit: () => () => undefined,
    getCredentialStatus: async () => ({
      encryptionAvailable: true,
      vaultPath: 'C:\\ud\\credentials.enc',
      managedProviderIds: [],
      lastError: null
    }),
    forgetCredential: async () => ({
      encryptionAvailable: true,
      vaultPath: 'C:\\ud\\credentials.enc',
      managedProviderIds: [],
      lastError: null
    }),
    request: async (method: string, params?: unknown) => {
      calls.push({ method, params: params as Record<string, unknown> | undefined })
      if (failure && (failure.method === null || failure.method === method)) {
        const text = failure.message
        failure = null
        throw new Error(text)
      }
      const p = (params ?? {}) as Record<string, unknown>
      switch (method) {
        case Methods.mcp.servers:
          return { servers }
        case Methods.mcp.tools:
          return { tools }
        case Methods.mcp.connect: {
          const name = String(p.server)
          // 连上之后清单里多一个 ready 的 Server，并把它的工具交出去
          servers = servers.map((item) =>
            item.name === name ? { ...item, connected: true, state: 'ready', tool_count: 2 } : item
          )
          tools = [...tools, tool(name, 'read'), tool(name, 'write')]
          const result: McpConnectResult = {
            server: name,
            transport: 'stdio',
            protocol_version: '2025-06-18',
            tool_count: 2,
            tools: tools.filter((item) => item.server === name)
          }
          return result
        }
        case Methods.mcp.disconnect: {
          const name = String(p.server)
          const wasConnected = servers.some((item) => item.name === name && item.connected)
          servers = servers.map((item) =>
            item.name === name ? { ...item, connected: false, state: 'closed', tool_count: 0 } : item
          )
          if (wasConnected) tools = tools.filter((item) => item.server !== name)
          const result: McpDisconnectResult = {
            server: name,
            closed: wasConnected,
            removed_tools: wasConnected ? 2 : 0
          }
          return result
        }
        case Methods.mcp.call: {
          // 回包里的 Server / 工具名按请求回填：这样断言「结果对应的是刚调的那个
          // 工具」才有意义，否则测试里的固定回包会把这类错配掩盖掉。
          const serverName = String(p.server)
          const toolName = String(p.tool)
          return {
            ...callResult,
            server: serverName,
            tool: toolName,
            namespaced_name: `mcp__${serverName}__${toolName}`
          }
        }
        default:
          throw new Error(`测试收到了意外方法：${method}`)
      }
    },
    // P5 切片七：插件遥测三条与终端输出同一范式，这些用例不驱动它们，但接口要全。
    onPluginLog: () => () => undefined,
    onPluginProgress: () => () => undefined,
    onPluginStateChanged: () => () => undefined,
    onPluginView: () => () => undefined,
    onViewportState: () => () => undefined,
    openExternalViewport: () => Promise.resolve({ reused: false }),

    onLspDiagnostics: () => () => undefined
  }

  vi.stubGlobal('window', { aiide: api })

  return {
    calls,
    failNext: (message, method) => {
      failure = { message, method: method ?? null }
    },
    setServers: (next) => {
      servers = next
    },
    setTools: (next) => {
      tools = next
    },
    setCallResult: (next) => {
      callResult = next
    }
  }
}

function methodsCalled(calls: Call[], method: string): number {
  return calls.filter((call) => call.method === method).length
}

beforeEach(() => {
  resetMcpState()
})

describe('拉清单', () => {
  it('没有已连接的 Server 时不打 mcp.tools', async () => {
    const harness = installHarness()
    harness.setServers([server('fs'), server('git', { transport: 'http' })])

    const ok = await refreshMcp()
    expect(ok).toBe(true)
    expect(methodsCalled(harness.calls, Methods.mcp.servers)).toBe(1)
    expect(methodsCalled(harness.calls, Methods.mcp.tools)).toBe(0)

    const state = getMcpState()
    expect(state.servers).toHaveLength(2)
    expect(state.tools).toEqual([])
    expect(state.loading).toBe(false)
  })

  it('有已连接 Server 时连工具一起拉', async () => {
    const harness = installHarness()
    await refreshMcp()

    expect(methodsCalled(harness.calls, Methods.mcp.tools)).toBe(1)
    expect(getMcpState().tools.map((item) => item.namespaced_name)).toEqual(['mcp__git__status'])
  })

  it('工具消失后选中态与试跑结果一起清掉', async () => {
    const harness = installHarness()
    await refreshMcp()
    selectTool('mcp__git__status')
    expect(getMcpState().selectedTool).toBe('mcp__git__status')

    // Server 被外部断开：工具清单变空
    harness.setTools([])
    await refreshMcp()

    expect(getMcpState().selectedTool).toBeNull()
    expect(getMcpState().callResult).toBeNull()
  })

  it('请求失败时落错误、清空清单、loading 复位', async () => {
    const harness = installHarness()
    harness.failNext('[8002] 连接被拒', Methods.mcp.servers)

    const ok = await refreshMcp()
    expect(ok).toBe(false)
    const state = getMcpState()
    expect(state.loading).toBe(false)
    expect(state.servers).toEqual([])
    expect(state.error).toContain('[8002]')
  })

  it('initMcp 正在拉时不叠第二次请求', async () => {
    const harness = installHarness()
    initMcp()
    initMcp()
    await new Promise((resolve) => setTimeout(resolve, 0))

    expect(methodsCalled(harness.calls, Methods.mcp.servers)).toBe(1)
  })
})

describe('连接 / 断开', () => {
  it('连上之后重拉清单，并把工具数写进提示', async () => {
    const harness = installHarness()
    await refreshMcp()

    const ok = await connectServer('fs')
    expect(ok).toBe(true)

    // connect → servers → tools：连完必须重拉，否则列表里还是「未连接」
    const order = harness.calls.map((call) => call.method)
    expect(order.indexOf(Methods.mcp.connect)).toBeLessThan(order.lastIndexOf(Methods.mcp.servers))
    expect(methodsCalled(harness.calls, Methods.mcp.tools)).toBeGreaterThan(0)

    const state = getMcpState()
    expect(state.busyServer).toBeNull()
    expect(state.hint).toContain('已连接「fs」')
    expect(state.hint).toContain('2 个工具')
    expect(state.tools.some((item) => item.namespaced_name === 'mcp__fs__read')).toBe(true)
  })

  it('连不上时错误落到面板，busy 复位', async () => {
    const harness = installHarness()
    await refreshMcp()
    harness.failNext('[8002] spawn ENOENT', Methods.mcp.connect)

    const ok = await connectServer('fs')
    expect(ok).toBe(false)
    const state = getMcpState()
    expect(state.busyServer).toBeNull()
    expect(state.error).toContain('连接失败')
  })

  it('断开真的断掉时说下架了几个工具', async () => {
    installHarness()
    await refreshMcp()

    const ok = await disconnectServer('git')
    expect(ok).toBe(true)
    expect(getMcpState().hint).toContain('从注册表下架 2 个工具')
    expect(getMcpState().tools).toEqual([])
  })

  it('断开本来就未连接的 Server 不算失败', async () => {
    const harness = installHarness()
    harness.setServers([server('fs')])
    await refreshMcp()

    const ok = await disconnectServer('fs')
    expect(ok).toBe(true)
    const state = getMcpState()
    expect(state.error).toBeNull()
    expect(state.hint).toContain('本来就是断开状态')
  })
})

describe('试跑', () => {
  it('没选工具时只报错，不发请求', async () => {
    const harness = installHarness()
    await refreshMcp()

    const ok = await callSelectedTool()
    expect(ok).toBe(false)
    expect(methodsCalled(harness.calls, Methods.mcp.call)).toBe(0)
    expect(getMcpState().error).toContain('先选一个工具')
  })

  it('参数不是 JSON 对象时拦在本地', async () => {
    const harness = installHarness()
    await refreshMcp()
    selectTool('mcp__git__status')
    setArgsText('{"a":}')

    const ok = await callSelectedTool()
    expect(ok).toBe(false)
    expect(methodsCalled(harness.calls, Methods.mcp.call)).toBe(0)
    expect(getMcpState().error).toContain('不是合法 JSON')
  })

  it('成功时把正文与 is_error 落到结果里，并按 Server + 原始名发请求', async () => {
    const harness = installHarness()
    await refreshMcp()
    selectTool('mcp__git__status')
    setArgsText('{"verbose":true}')

    const ok = await callSelectedTool()
    expect(ok).toBe(true)

    const call = harness.calls.find((item) => item.method === Methods.mcp.call)
    expect(call?.params).toEqual({ server: 'git', tool: 'status', arguments: { verbose: true } })

    const state = getMcpState()
    expect(state.calling).toBe(false)
    expect(state.callResult).toEqual({
      tool: 'mcp__git__status',
      text: callResultText(CALL_OK),
      isError: false
    })
  })

  it('Server 报 is_error 时如实标红，但仍算协议成功', async () => {
    const harness = installHarness()
    await refreshMcp()
    selectTool('mcp__git__status')
    harness.setCallResult({ ...CALL_OK, text: 'not a git repo', content: [], is_error: true })

    const ok = await callSelectedTool()
    expect(ok).toBe(true)
    const state = getMcpState()
    expect(state.error).toBeNull()
    expect(state.callResult?.isError).toBe(true)
    expect(state.callResult?.text).toBe('not a git repo')
    expect(state.hint).toContain('失败结果')
  })

  it('切换工具会清掉上一次的试跑结果与参数', () => {
    selectTool('mcp__git__status')
    setArgsText('{"a":1}')
    selectTool('mcp__git__diff')

    const state = getMcpState()
    expect(state.argsText).toBe('{}')
    expect(state.callResult).toBeNull()
  })
})
