/**
 * M7 渲染侧状态：MCP Server 清单、工具清单与一次手动的试跑。
 *
 * 与 `permission-store` / `checkpoint-store` 同一形态：单例 + `useSyncExternalStore`，
 * 动作返回布尔表示「是否真的做成了」，失败原因落到 `error`。
 *
 * 这里有一处刻意的取舍：**Server 清单不从「事件」推**。MCP 没有自己的推送通道
 * （`workspace.changed` 也不覆盖它——`mcp.json` 是配置不是工作区内容），所以清单
 * 只在「进面板 / 点刷新 / 连接或断开之后」拉一次。宁可让用户偶尔点一下刷新，
 * 也不要为了一个低频视图去开一条轮询。
 */

import { useSyncExternalStore } from 'react'

import type { JsonValue, McpServerInfo, McpToolInfo } from '@ai-ide/shared-types'

import { callResultText, describeMcpError, mcpApi, parseToolArguments } from './mcp-api'

/** 一次试跑的结果；`isError` 来自 Server（不是协议失败）。 */
export interface McpCallView {
  /** 带命名空间的全名，便于用户回填到别处。 */
  tool: string
  text: string
  isError: boolean
}

export interface McpState {
  servers: McpServerInfo[]
  /** 已连接 Server 的工具（内核按 Server 名、工具名排好序）。 */
  tools: McpToolInfo[]
  loading: boolean
  /** 正在连接 / 断开的 Server 名（UI 只禁用那一行）。 */
  busyServer: string | null
  hint: string | null
  error: string | null
  /** 当前选中的工具全名（`mcp__<server>__<tool>`）。 */
  selectedTool: string | null
  /** 试跑参数输入框的原文：故意保留用户写坏的样子，方便改。 */
  argsText: string
  calling: boolean
  callResult: McpCallView | null
}

function initialState(): McpState {
  return {
    servers: [],
    tools: [],
    loading: false,
    busyServer: null,
    hint: null,
    error: null,
    selectedTool: null,
    argsText: '{}',
    calling: false,
    callResult: null
  }
}

let state: McpState = initialState()

const listeners = new Set<() => void>()

function setState(patch: Partial<McpState>): void {
  state = { ...state, ...patch }
  for (const listener of listeners) listener()
}

export function getMcpState(): McpState {
  return state
}

export function subscribeMcp(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function useMcp(): McpState {
  return useSyncExternalStore(subscribeMcp, getMcpState, getMcpState)
}

/** 测试用：模块级单例跨用例不清就会串状态。 */
export function resetMcpState(): void {
  state = initialState()
  for (const listener of listeners) listener()
}

/**
 * 拉清单：`servers` 一定拉，`tools` 只在「有已连接 Server」时才拉。
 *
 * 后者是省一次无意义请求——全都没连的时候内核回的就是空数组，多打一次只是让它
 * 白跑一趟路由。
 */
export async function refreshMcp(): Promise<boolean> {
  setState({ loading: true, error: null })
  try {
    const servers = await mcpApi.servers()
    const connected = servers.servers.some((server) => server.connected)
    const tools = connected ? await mcpApi.tools() : { tools: [] }

    // 选中的工具可能已经被断开 / 摘掉了：这时要把它从选中态里清掉，否则试跑区
    // 会继续拿着一个不存在的工具名，点「执行」得到的是 `8004`。
    const stillThere =
      state.selectedTool !== null && tools.tools.some((tool) => tool.namespaced_name === state.selectedTool)

    setState({
      loading: false,
      servers: servers.servers,
      tools: tools.tools,
      selectedTool: stillThere ? state.selectedTool : null,
      callResult: stillThere ? state.callResult : null
    })
    return true
  } catch (error) {
    setState({ loading: false, servers: [], tools: [], error: describeMcpError(error) })
    return false
  }
}

/** 面板首次出现时拉一次；正在拉就不再叠一次。 */
export function initMcp(): void {
  if (state.loading) return
  void refreshMcp()
}

/** 连一个 Server，完成后重拉清单（`tool_count` / `state` 都变了）。 */
export async function connectServer(server: string): Promise<boolean> {
  if (state.busyServer !== null) return false
  setState({ busyServer: server, error: null, hint: null })
  try {
    const result = await mcpApi.connect(server)
    setState({
      busyServer: null,
      hint:
        result.tool_count > 0
          ? `已连接「${result.server}」（${result.transport}），发现 ${result.tool_count} 个工具。`
          : `已连接「${result.server}」（${result.transport}），它没有暴露任何工具。`
    })
    await refreshMcp()
    return true
  } catch (error) {
    setState({ busyServer: null, error: describeMcpError(error) })
    return false
  }
}

/** 断开一个 Server。幂等：本就没连时 `closed = false`，照实说一句。 */
export async function disconnectServer(server: string): Promise<boolean> {
  if (state.busyServer !== null) return false
  setState({ busyServer: server, error: null, hint: null })
  try {
    const result = await mcpApi.disconnect(server)
    setState({
      busyServer: null,
      hint: result.closed
        ? `已断开「${result.server}」，从注册表下架 ${result.removed_tools} 个工具。`
        : `「${result.server}」本来就是断开状态。`
    })
    await refreshMcp()
    return true
  } catch (error) {
    setState({ busyServer: null, error: describeMcpError(error) })
    return false
  }
}

/** 选中一个工具；切换时清掉上一次的试跑结果，免得看起来像是这次的。 */
export function selectTool(namespacedName: string | null): void {
  if (namespacedName === state.selectedTool) return
  setState({ selectedTool: namespacedName, callResult: null, argsText: '{}', error: null })
}

export function setArgsText(text: string): void {
  setState({ argsText: text })
}

/**
 * 试跑当前选中的工具。
 *
 * 参数文本就地解析、就地报错——不把非法 JSON 送去内核换一个 `8007`，用户看到的是
 * 「哪一行写坏了」而不是一句协议码。
 */
export async function callSelectedTool(): Promise<boolean> {
  const tool = state.tools.find((item) => item.namespaced_name === state.selectedTool)
  if (tool === undefined) {
    setState({ error: '先选一个工具，再执行。' })
    return false
  }
  if (state.calling) return false

  const parsed = parseToolArguments(state.argsText)
  if (!parsed.ok) {
    setState({ error: parsed.reason, callResult: null })
    return false
  }

  setState({ calling: true, error: null, hint: null, callResult: null })
  try {
    const result = await mcpApi.call({
      server: tool.server,
      tool: tool.name,
      args: parsed.value as JsonValue
    })
    setState({
      calling: false,
      callResult: {
        tool: result.namespaced_name,
        text: callResultText(result),
        isError: result.is_error
      },
      hint: result.is_error ? '工具返回了失败结果（协议本身是成功的）。' : '调用成功。'
    })
    return true
  } catch (error) {
    setState({ calling: false, error: describeMcpError(error) })
    return false
  }
}

export function dismissMcpHint(): void {
  setState({ hint: null })
}
