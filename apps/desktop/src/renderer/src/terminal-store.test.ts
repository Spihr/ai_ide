/**
 * M6 终端面板（外壳侧）的状态与编解码行为测试。
 *
 * 与其它 store 测试同一套路：不渲染 React，直接驱动动作函数，用假的
 * `window.aiide` 顶替 IPC，断言「发了什么请求」「状态怎么变」。
 *
 * 这里钉的是终端最容易「看起来能用、其实丢数据」的几条：
 *
 * - 输出必须在**没人接**的时候先缓冲：标签刚建好、xterm 还没挂上时，首屏 banner
 *   就已经推来了，丢了就是一片空白；
 * - 关标签后**不许**再为它攒输出（否则后台一直涨内存，且用户永远看不到）；
 * - 退出后标签留在原地（用户要看最后一屏），重复的 `terminal.exit` 不该刷状态；
 * - 重启要**占原坑位**，不能让标签顺序跳来跳去；
 * - base64 双向转换不能把 UTF-8 与 `\r` / `ESC` 弄坏——这是终端能不能用的底线。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'

import { Methods, type TerminalDataEvent, type TerminalExitEvent, type TerminalView } from '@ai-ide/shared-types'

import type { AiIdeApi } from '../../shared/ipc'
import { decodeBase64, encodeBase64, encodeInput } from './terminal-api'
import {
  closeTerminalPanel,
  closeTerminalTab,
  getTerminalState,
  initTerminalEvents,
  newTerminalTab,
  receiveTerminalData,
  receiveTerminalExit,
  resetTerminalState,
  restartTerminalTab,
  setActiveTerminal,
  subscribeOutput,
  syncTerminalSize,
  toggleTerminalPanel
} from './terminal-store'
import { closeTab, closeWorkspace, getEditorState, openWorkspace } from './store'

const ROOT = 'C:\\ws'

interface Call {
  method: string
  params: Record<string, unknown> | undefined
}

interface Harness {
  calls: Call[]
  /** 手动推一条 `terminal.data` 给 store（模拟内核推送）。 */
  pushData: (terminalId: string, text: string) => void
  /** 手动推一条 `terminal.exit`。 */
  pushExit: (terminalId: string, code: number | null) => void
  failNext: (message: string, method?: string) => void
  /** 让 `terminal.list` 返回的存活会话。 */
  setAlive: (ids: string[]) => void
}

function view(id: string, shell = 'C:\\WINDOWS\\System32\\WindowsPowerShell\\v1.0\\powershell.exe'): TerminalView {
  return {
    id,
    shell,
    cwd: ROOT,
    cols: 80,
    rows: 24,
    created_at_ms: 1000
  }
}

function installHarness(): Harness {
  const calls: Call[] = []
  let failure: { message: string; method: string | null } | null = null
  let created = 0
  let alive = ['t-1']
  let dataListener: ((event: TerminalDataEvent) => void) | null = null
  let exitListener: ((event: TerminalExitEvent) => void) | null = null

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
    pickWorkspaceDirectory: async () => ROOT,
    pickSavePath: async () => null,
    request: async (method: string, params?: unknown) => {
      calls.push({ method, params: params as Record<string, unknown> | undefined })
      if (failure && (failure.method === null || failure.method === method)) {
        const text = failure.message
        failure = null
        throw new Error(text)
      }
      switch (method) {
        case Methods.workspace.open:
          return {
            workspace_id: 'ws-1',
            root: ROOT,
            name: 'ws',
            stats: { files: 0, dirs: 0, ignored: 0 },
            indexing: false
          }
        case Methods.workspace.tree:
          return { entries: [] }
        case Methods.terminal.create: {
          created += 1
          const id = `t-${created}`
          alive = [...alive, id]
          return { terminal: view(id) }
        }
        case Methods.terminal.write:
          return { bytes: String((params as { data: string }).data).length }
        case Methods.terminal.resize:
          return { terminal: view(String((params as { terminal_id: string }).terminal_id)) }
        case Methods.terminal.kill:
          return { ok: true }
        case Methods.terminal.list:
          return { terminals: alive.map((id) => view(id)) }
        default:
          return { ok: true }
      }
    },
    onCoreStatus: () => () => undefined,
    onCoreLog: () => () => undefined,
    onWorkspaceChanged: () => () => undefined,
    onWorkspaceIndexed: () => () => undefined,
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
    onAiEvent: () => () => undefined,
    onCredentialStatus: () => () => undefined,
    onTerminalData: (listener) => {
      dataListener = listener
      return () => {
        dataListener = null
      }
    },
    onTerminalExit: (listener) => {
      exitListener = listener
      return () => {
        exitListener = null
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
    pushData: (terminalId, text) => {
      const data = encodeBase64(new TextEncoder().encode(text))
      dataListener?.({ terminal_id: terminalId, data, bytes: text.length })
    },
    pushExit: (terminalId, code) => {
      exitListener?.({ terminal_id: terminalId, code, signal: null, at_ms: 2000 })
    },
    failNext: (message, method) => {
      failure = { message, method: method ?? null }
    },
    setAlive: (ids) => {
      alive = ids
    }
  }
}

let h: Harness

function callsOf(method: string): Call[] {
  return h.calls.filter((call) => call.method === method)
}

function tabTitles(): string[] {
  return getTerminalState().tabs.map((tab) => tab.title)
}

beforeEach(async () => {
  // 编辑器状态是模块级单例，跨用例不清就会让「无工作区」这类用例拿到上一个用例的工作区。
  for (const tab of [...getEditorState().tabs]) closeTab(tab.path, true)
  if (getEditorState().workspace !== null) await closeWorkspace()
  vi.unstubAllGlobals()
  h = installHarness()
  resetTerminalState()
  // 事件入口与 App 一致地绑一遍，否则 `pushData` / `pushExit` 没有订阅者。
  initTerminalEvents()
})

describe('base64 编解码', () => {
  it('对 ASCII 做标准 base64（含 `=` 填充的三种余数）', () => {
    const encoder = new TextEncoder()
    expect(encodeBase64(encoder.encode('a'))).toBe('YQ==')
    expect(encodeBase64(encoder.encode('ab'))).toBe('YWI=')
    expect(encodeBase64(encoder.encode('abc'))).toBe('YWJj')
    expect(encodeBase64(new Uint8Array([]))).toBe('')
  })

  it('保留控制字符：`\\r` 与方向键的 ESC 序列不长歪', () => {
    const input = 'ls\r\u001b[A'
    const round = decodeBase64(encodeBase64(new TextEncoder().encode(input)))

    expect(new TextDecoder().decode(round)).toBe(input)
  })

  it('UTF-8 多字节（中文 / emoji）往返不乱码', () => {
    const input = '中文🙂'
    const round = decodeBase64(encodeInput(input))

    expect(new TextDecoder().decode(round)).toBe(input)
  })

  it('解码时忽略非法字符与缺失的填充', () => {
    // 内核只会发标准 base64，这里的容错是给手抄日志留活路。
    expect(new TextDecoder().decode(decodeBase64('YWJj\n'))).toBe('abc')
    expect(new TextDecoder().decode(decodeBase64('YWJj'))).toBe('abc')
  })
})

describe('面板开关与建会话', () => {
  it('展开面板时自动建第一个会话，cwd 取工作区根', async () => {
    await openWorkspace(ROOT)

    await toggleTerminalPanel(true)

    expect(getTerminalState().open).toBe(true)
    const [create] = callsOf(Methods.terminal.create)
    expect(create?.params).toMatchObject({ cwd: ROOT })
    expect(getTerminalState().tabs).toHaveLength(1)
    expect(getTerminalState().activeId).toBe('t-1')
    expect(getTerminalState().busy).toBe(false)
  })

  it('已有会话时展开不再重复建', async () => {
    await toggleTerminalPanel(true)
    await toggleTerminalPanel(false)
    await toggleTerminalPanel(true)

    expect(callsOf(Methods.terminal.create)).toHaveLength(1)
  })

  it('标签标题取 shell 名并带序号', async () => {
    await newTerminalTab()
    await newTerminalTab()

    expect(tabTitles()).toEqual(['powershell 1', 'powershell 2'])
    expect(getTerminalState().activeId).toBe('t-2')
  })

  it('建会话失败时落 error 且不留空标签', async () => {
    h.failNext('pty 启动失败', Methods.terminal.create)

    const id = await newTerminalTab()

    expect(id).toBeNull()
    expect(getTerminalState().tabs).toHaveLength(0)
    expect(getTerminalState().error).toContain('pty 启动失败')
    expect(getTerminalState().busy).toBe(false)
  })
})

describe('输出分发', () => {
  it('没人订阅时先缓冲，挂载后按序补灌', async () => {
    await newTerminalTab()
    h.pushData('t-1', 'first\r\n')
    h.pushData('t-1', 'second\r\n')

    const received: string[] = []
    const off = subscribeOutput('t-1', (bytes) => received.push(new TextDecoder().decode(bytes)))

    expect(received).toEqual(['first\r\n', 'second\r\n'])

    // 之后的增量直接送达
    h.pushData('t-1', 'third\r\n')
    expect(received).toEqual(['first\r\n', 'second\r\n', 'third\r\n'])
    off()
  })

  it('取消订阅后不再收到，缓冲不会重复回放', async () => {
    await newTerminalTab()
    const received: string[] = []
    const off = subscribeOutput('t-1', (bytes) => received.push(new TextDecoder().decode(bytes)))
    off()

    h.pushData('t-1', 'after-off\r\n')

    expect(received).toHaveLength(0)
  })

  it('已关闭的标签不再攒输出', async () => {
    await newTerminalTab()
    await closeTerminalTab('t-1')

    h.pushData('t-1', 'ghost\r\n')

    // 关掉后重新订阅也拿不到任何东西（既没标签也没缓冲）
    const received: string[] = []
    subscribeOutput('t-1', (bytes) => received.push(new TextDecoder().decode(bytes)))
    expect(received).toHaveLength(0)
  })
})

describe('退出与清理', () => {
  it('退出后标签保留、状态为 exited 并记录退出码', async () => {
    await newTerminalTab()

    h.pushExit('t-1', 1)

    const [tab] = getTerminalState().tabs
    expect(tab?.status).toBe('exited')
    expect(tab?.exitCode).toBe(1)
    // 前台不动：用户还要看最后一屏
    expect(getTerminalState().activeId).toBe('t-1')
  })

  it('重复的退出事件不再刷状态（避免状态对象频繁换引用）', async () => {
    await newTerminalTab()
    h.pushExit('t-1', 0)
    const first = getTerminalState().tabs

    h.pushExit('t-1', 0)

    expect(getTerminalState().tabs).toBe(first)
  })

  it('未知会话的退出事件被忽略', () => {
    receiveTerminalExit({ terminal_id: 'nope', code: 0, signal: null, at_ms: 1 })
    expect(getTerminalState().tabs).toHaveLength(0)
  })

  it('未知会话的输出事件被忽略', () => {
    receiveTerminalData({ terminal_id: 'nope', data: 'YQ==', bytes: 1 })
    expect(getTerminalState().tabs).toHaveLength(0)
  })

  it('关标签先摘 UI 再发 kill，并把前台交给邻居', async () => {
    await newTerminalTab()
    await newTerminalTab()
    setActiveTerminal('t-1')

    await closeTerminalTab('t-1')

    expect(getTerminalState().tabs.map((tab) => tab.id)).toEqual(['t-2'])
    expect(getTerminalState().activeId).toBe('t-2')
    expect(callsOf(Methods.terminal.kill).map((call) => call.params)).toEqual([
      { terminal_id: 't-1' }
    ])
  })

  it('kill 失败不回滚标签（UI 已经摘掉），只落 error', async () => {
    await newTerminalTab()
    h.failNext('会话已不存在', Methods.terminal.kill)

    await closeTerminalTab('t-1')

    expect(getTerminalState().tabs).toHaveLength(0)
    expect(getTerminalState().error).toContain('会话已不存在')
  })

  it('关掉最后一个标签后前台为 null，面板仍展开', async () => {
    await toggleTerminalPanel(true)

    await closeTerminalTab('t-1')

    expect(getTerminalState().tabs).toHaveLength(0)
    expect(getTerminalState().activeId).toBeNull()
    // 关的是标签不是面板：面板还开着，用户可以接着点「+」
    expect(getTerminalState().open).toBe(true)
  })

  it('收起面板不杀会话', async () => {
    await newTerminalTab()
    closeTerminalPanel()

    expect(getTerminalState().open).toBe(false)
    expect(getTerminalState().tabs).toHaveLength(1)
    expect(callsOf(Methods.terminal.kill)).toHaveLength(0)
  })

  it('重启占原坑位并换新会话 id', async () => {
    await newTerminalTab()
    await newTerminalTab()
    h.pushExit('t-1', 0)

    const newId = await restartTerminalTab('t-1')

    expect(newId).toBe('t-3')
    expect(getTerminalState().tabs.map((tab) => tab.id)).toEqual(['t-3', 't-2'])
    expect(getTerminalState().tabs[0]?.status).toBe('running')
    expect(getTerminalState().activeId).toBe('t-3')
    // 旧会话要先杀，不能留个僵尸 PTY
    expect(callsOf(Methods.terminal.kill).map((call) => call.params)).toEqual([
      { terminal_id: 't-1' }
    ])
    // 新会话沿用旧标签的 cwd（不是「当前工作区」，用户可能早就切走了）
    expect(callsOf(Methods.terminal.create)[2]?.params).toMatchObject({ cwd: ROOT })
  })
  it('展开时对齐内核会话表，剔掉内核重启后已死的标签', async () => {
    await newTerminalTab()
    await newTerminalTab()
    closeTerminalPanel()
    // 内核重启：只有 t-2 还在
    h.setAlive(['t-2'])

    await toggleTerminalPanel(true)
    await new Promise((resolve) => setTimeout(resolve, 0))

    expect(callsOf(Methods.terminal.list)).toHaveLength(1)
    expect(getTerminalState().tabs.map((tab) => tab.id)).toEqual(['t-2'])
    expect(getTerminalState().activeId).toBe('t-2')
  })
})

describe('尺寸同步', () => {
  it('0 尺寸不发请求（面板未布局时的 0×0 无意义）', async () => {
    await syncTerminalSize('t-1', 0, 0)
    expect(callsOf(Methods.terminal.resize)).toHaveLength(0)
  })

  it('正常尺寸按协议发 resize', async () => {
    await syncTerminalSize('t-1', 120, 30)

    const [call] = callsOf(Methods.terminal.resize)
    expect(call?.params).toEqual({ terminal_id: 't-1', cols: 120, rows: 30 })
  })
})
