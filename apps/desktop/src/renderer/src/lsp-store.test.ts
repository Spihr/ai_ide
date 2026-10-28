/**
 * 外壳语言服务数据层测试。
 *
 * 不渲染 React、不加载 monaco：用假的 `window.aiide` 顶替 IPC，标记写入器用
 * `setMarkerWriter` 注入一个数组收集器，这样「诊断到底画成什么样」可以直接断言。
 *
 * 钉的几条都是**看着对、其实错**的地方：
 *
 * - **降级不抛错**：`10001`（没装服务器）必须变成 `unavailable` 状态，`openFile`
 *   返回 `false` 而不是 reject——否则打开一个 `.py` 文件就弹一次错误框；
 * - **`sync` 带的是编辑器缓冲**：只让内核读磁盘的话，改了一半的代码跳转会跳到
 *   已删除的行上；
 * - **行列要 +1 回写**：内部转 0-based 做运算，交给 Monaco 的必须是 1-based，
 *   少这一步所有标记整体左上偏一格；
 * - **空诊断要清标记**：`diagnostics: []` 表示问题全消失，丢掉了红点就永久残留；
 * - **`stop` 清标记 + 清摘要**：内核不再推诊断，UI 不能还挂着「2 个错误」。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import {
  Methods,
  type LspDiagnosticsEvent,
  type LspServersResult,
  type LspStartResult,
  type LspSyncResult
} from '@ai-ide/shared-types'

import { RPC_ERROR_PAYLOAD_MARKER, type AiIdeApi } from '../../shared/ipc'
import {
  LSP_ERROR_NOT_INSTALLED,
  LSP_ERROR_SERVER_UNKNOWN,
  MarkerSeverity,
  countProblems,
  diagnosticToMarker,
  diagnosticsToMarkers,
  isDegradeCode,
  lspErrorCode,
  positionToZeroBased,
  severityToMarkerSeverity,
  type LspMarkerData
} from './lsp-api'
import {
  currentFileDiagnostics,
  dispose,
  getLspState,
  languageLabel,
  openFile,
  refreshServers,
  resetLspStoreForTests,
  setMarkerWriter,
  statusLabel,
  stop,
  stopAll,
  syncFile
} from './lsp-store'

const TS_PATH = 'C:\\ws\\src\\a.ts'
const TS_PATH_2 = 'C:\\ws\\src\\c.ts'
const PY_PATH = 'C:\\ws\\src\\b.py'

interface Call {
  method: string
  params: Record<string, unknown> | undefined
}

interface Harness {
  calls: Call[]
  /** 让下一条 `lsp.start` 抛错（降级 / 失败路径）。 */
  failStart: (error: Error) => void
  /** 让下一条 `lsp.sync` 抛错。 */
  failSync: (error: Error) => void
  /** 让下一条 `lsp.stop` 抛错。 */
  failStop: (error: Error) => void
  /** 推进一条 `lsp.diagnostics`（模拟内核推送）。 */
  pushDiagnostics: (event: LspDiagnosticsEvent) => void
  /** 已注册的诊断监听器数量（用来验「只挂一次」）。 */
  listenerCount: () => number
}

/** 拼一条外壳认得的 RPC 错误（信封藏在 message 尾部，与主进程一致）。 */
function rpcError(code: number, message: string): Error {
  return new Error(`${message}${RPC_ERROR_PAYLOAD_MARKER}${JSON.stringify({ code })}`)
}

function startResult(languageId: string, serverId = `srv-${languageId}`): LspStartResult {
  return {
    server: {
      server_id: serverId,
      language_id: languageId,
      command: `${languageId}-langserver`,
      status: 'ready',
      restarts: 0
    },
    reused: false
  }
}

function installHarness(): Harness {
  const calls: Call[] = []
  let startError: Error | null = null
  let syncError: Error | null = null
  let stopError: Error | null = null
  const listeners = new Set<(event: LspDiagnosticsEvent) => void>()

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
    request: async (method: string, params?: unknown) => {
      const typed = params as Record<string, unknown> | undefined
      calls.push({ method, params: typed })

      switch (method) {
        case Methods.lsp.start: {
          if (startError !== null) {
            const error = startError
            startError = null
            throw error
          }
          // 语言按路径后缀判：`.py` 走 python，其余按 typescript。
          const language = String(typed?.path ?? '').toLowerCase().endsWith('.py')
            ? 'python'
            : 'typescript'
          return startResult(language)
        }
        case Methods.lsp.sync: {
          if (syncError !== null) {
            const error = syncError
            syncError = null
            throw error
          }
          return {
            server_id: String(typed?.server_id ?? ''),
            path: String(typed?.path ?? ''),
            uri: `file:///${String(typed?.path ?? '').replace(/\\/g, '/')}`,
            version: 1,
            opened: true,
            changed: false
          } satisfies LspSyncResult
        }
        case Methods.lsp.stop: {
          if (stopError !== null) {
            const error = stopError
            stopError = null
            throw error
          }
          return { server_id: String(typed?.server_id ?? ''), stopped: true }
        }
        case Methods.lsp.servers:
          return { servers: [] } satisfies LspServersResult
        default:
          throw new Error(`测试未预期的请求：${method}`)
      }
    },
    onCoreStatus: () => () => undefined,
    onCoreLog: () => () => undefined,
    onWorkspaceChanged: () => () => undefined,
    onWorkspaceIndexed: () => () => undefined,
    getCredentialStatus: async () => {
      throw new Error('测试未使用 getCredentialStatus')
    },
    forgetCredential: async () => {
      throw new Error('测试未使用 forgetCredential')
    },
    onAiEvent: () => () => undefined,
    onCredentialStatus: () => () => undefined,
    onTerminalData: () => () => undefined,
    onTerminalExit: () => () => undefined,
    onPluginLog: () => () => undefined,
    onPluginProgress: () => () => undefined,
    onPluginStateChanged: () => () => undefined,
    onPluginView: () => () => undefined,
    onViewportState: () => () => undefined,
    openExternalViewport: () => Promise.resolve({ reused: false }),

    onLspDiagnostics: (listener) => {
      listeners.add(listener)
      return () => {
        listeners.delete(listener)
      }
    }
  }

  vi.stubGlobal('window', { aiide: api })

  return {
    calls,
    failStart: (error) => {
      startError = error
    },
    failSync: (error) => {
      syncError = error
    },
    failStop: (error) => {
      stopError = error
    },
    pushDiagnostics: (event) => {
      for (const listener of listeners) listener(event)
    },
    listenerCount: () => listeners.size
  }
}

/** 收集标记写入，避免真的碰 monaco。 */
function installMarkerSink(): Array<{ path: string; markers: LspMarkerData[] }> {
  const writes: Array<{ path: string; markers: LspMarkerData[] }> = []
  setMarkerWriter((path, markers) => {
    writes.push({ path, markers })
  })
  return writes
}

function diagnosticsEvent(overrides: Partial<LspDiagnosticsEvent> = {}): LspDiagnosticsEvent {
  return {
    server_id: 'srv-typescript',
    path: TS_PATH,
    version: 1,
    diagnostics: [],
    ...overrides
  }
}

function errorDiagnostic(line: number, message = 'e'): LspDiagnosticsEvent['diagnostics'][number] {
  return {
    range: { start: { line, character: 1 }, end: { line, character: 2 } },
    severity: 'error',
    message
  }
}

beforeEach(() => {
  resetLspStoreForTests()
})

afterEach(() => {
  dispose()
  vi.unstubAllGlobals()
})

describe('行列与严重度换算', () => {
  it('1-based 转 0-based 时减一，越界夹到 0', () => {
    expect(positionToZeroBased({ line: 3, character: 5 })).toEqual({ line: 2, character: 4 })
    expect(positionToZeroBased({ line: 0, character: 0 })).toEqual({ line: 0, character: 0 })
  })

  it('severity 枚举与线格式数字都映射到 Monaco 的数值', () => {
    expect(severityToMarkerSeverity('error')).toBe(MarkerSeverity.Error)
    expect(severityToMarkerSeverity('warning')).toBe(MarkerSeverity.Warning)
    expect(severityToMarkerSeverity('information')).toBe(MarkerSeverity.Info)
    expect(severityToMarkerSeverity('hint')).toBe(MarkerSeverity.Hint)
    // LSP 线格式 1..4
    expect(severityToMarkerSeverity(1)).toBe(MarkerSeverity.Error)
    expect(severityToMarkerSeverity(2)).toBe(MarkerSeverity.Warning)
    expect(severityToMarkerSeverity(3)).toBe(MarkerSeverity.Info)
    expect(severityToMarkerSeverity(4)).toBe(MarkerSeverity.Hint)
    // unknown / 越界归 info，不猜成 error
    expect(severityToMarkerSeverity('unknown')).toBe(MarkerSeverity.Info)
    expect(severityToMarkerSeverity(0)).toBe(MarkerSeverity.Info)
  })

  it('标记回写用 1-based（内部 0-based 运算后 +1）', () => {
    const marker = diagnosticToMarker({
      range: { start: { line: 2, character: 3 }, end: { line: 2, character: 7 } },
      severity: 'error',
      message: '找不到名称',
      source: 'ts',
      code: 'TS2304'
    })

    expect(marker).toEqual({
      severity: MarkerSeverity.Error,
      message: '找不到名称',
      source: 'ts',
      code: 'TS2304',
      startLineNumber: 2,
      startColumn: 3,
      endLineNumber: 2,
      endColumn: 7
    })
  })

  it('反向区间夹成正向，不产生负长度标记', () => {
    const marker = diagnosticToMarker({
      range: { start: { line: 4, character: 9 }, end: { line: 4, character: 2 } },
      severity: 'warning',
      message: 'x'
    })
    expect(marker.startColumn).toBe(9)
    expect(marker.endColumn).toBe(9)
  })

  it('批量转换保持顺序，countProblems 只数 error / warning', () => {
    const markers = diagnosticsToMarkers([
      {
        range: { start: { line: 1, character: 1 }, end: { line: 1, character: 2 } },
        severity: 'hint',
        message: 'a'
      },
      errorDiagnostic(2, 'b')
    ])
    expect(markers).toHaveLength(2)
    expect(markers[0]?.severity).toBe(MarkerSeverity.Hint)
    expect(
      countProblems([
        errorDiagnostic(1, 'a'),
        {
          range: { start: { line: 1, character: 1 }, end: { line: 1, character: 2 } },
          severity: 'warning',
          message: 'b'
        },
        {
          range: { start: { line: 1, character: 1 }, end: { line: 1, character: 2 } },
          severity: 'information',
          message: 'c'
        }
      ])
    ).toBe(2)
  })

  it('错误码优先认结构化信封，其次认 [code] 裸写法', () => {
    expect(lspErrorCode(rpcError(LSP_ERROR_NOT_INSTALLED, '未安装').message)).toBe(
      LSP_ERROR_NOT_INSTALLED
    )
    expect(lspErrorCode('语言服务器未安装 [10001]')).toBe(LSP_ERROR_NOT_INSTALLED)
    expect(lspErrorCode('纯文案，没有码')).toBeNull()
  })

  it('只有 10001 / 10002 算降级，10003 只是连接过期', () => {
    expect(isDegradeCode(LSP_ERROR_NOT_INSTALLED)).toBe(true)
    expect(isDegradeCode(10002)).toBe(true)
    expect(isDegradeCode(LSP_ERROR_SERVER_UNKNOWN)).toBe(false)
    expect(isDegradeCode(null)).toBe(false)
  })
})

describe('启动 / 降级', () => {
  it('命令缺失时静默降级：状态变 unavailable，openFile 返回 false 且不抛错', async () => {
    const harness = installHarness()
    installMarkerSink()
    harness.failStart(rpcError(LSP_ERROR_NOT_INSTALLED, 'lsp.start 失败：找不到命令'))

    await expect(
      openFile({ path: PY_PATH, languageId: 'python', content: 'print(1)' })
    ).resolves.toBe(false)

    const state = getLspState()
    expect(state.status).toBe('unavailable')
    expect(state.serverId).toBeNull()
    // 状态栏文案必须带语言名，且不是错误态
    expect(statusLabel(state)).toBe('Python 语言服务器不可用')
    expect(state.notice).toContain('未安装')
    // 降级不重试：start 只调了一次，也没去 sync
    expect(harness.calls.filter((call) => call.method === Methods.lsp.start)).toHaveLength(1)
    expect(harness.calls.some((call) => call.method === Methods.lsp.sync)).toBe(false)
  })

  it('启动成功后按当前缓冲 sync，且 start 不传 command', async () => {
    const harness = installHarness()
    installMarkerSink()

    await expect(
      openFile({ path: TS_PATH, languageId: 'typescript', content: 'const a = 1' })
    ).resolves.toBe(true)

    const start = harness.calls.find((call) => call.method === Methods.lsp.start)
    expect(start?.params).toMatchObject({ path: TS_PATH, args: [] })
    expect(start?.params).not.toHaveProperty('command')

    const sync = harness.calls.find((call) => call.method === Methods.lsp.sync)
    expect(sync?.params).toMatchObject({
      server_id: 'srv-typescript',
      path: TS_PATH,
      content: 'const a = 1'
    })

    const state = getLspState()
    expect(state.status).toBe('ready')
    expect(state.serverId).toBe('srv-typescript')
    expect(state.languageId).toBe('typescript')
    expect(statusLabel(state)).toBe('TypeScript 语言服务已就绪')
  })

  it('同语言换文件复用连接：只 start 一次，sync 两次；切语言另起一条', async () => {
    const harness = installHarness()
    installMarkerSink()

    await openFile({ path: TS_PATH, languageId: 'typescript', content: 'a' })
    await openFile({ path: TS_PATH_2, languageId: 'typescript', content: 'b' })
    await openFile({ path: PY_PATH, languageId: 'python', content: 'c' })

    expect(harness.calls.filter((call) => call.method === Methods.lsp.start)).toHaveLength(2)
    expect(harness.calls.filter((call) => call.method === Methods.lsp.sync)).toHaveLength(3)

    const state = getLspState()
    expect(state.status).toBe('ready')
    expect(state.serverId).toBe('srv-python')
    expect(Object.keys(state.connections).sort()).toEqual(['python', 'typescript'])
    // 诊断通道只挂一次，不随每次开文件重复注册
    expect(harness.listenerCount()).toBe(1)
  })

  it('认不出的错误进 error 态并保留原文，不当成降级', async () => {
    const harness = installHarness()
    installMarkerSink()
    harness.failStart(new Error('内核进程已退出'))

    await expect(
      openFile({ path: TS_PATH, languageId: 'typescript', content: 'a' })
    ).resolves.toBe(false)

    const state = getLspState()
    expect(state.status).toBe('error')
    expect(statusLabel(state)).toBe('语言服务出错')
    expect(state.notice).toContain('内核进程已退出')
  })
})

describe('同步', () => {
  it('保存后 syncFile 用当前连接发当前文本', async () => {
    const harness = installHarness()
    installMarkerSink()
    await openFile({ path: TS_PATH, languageId: 'typescript', content: 'v1' })

    await expect(syncFile(TS_PATH, 'v2')).resolves.toBe(true)

    const syncs = harness.calls.filter((call) => call.method === Methods.lsp.sync)
    expect(syncs).toHaveLength(2)
    expect(syncs[1]?.params).toMatchObject({ server_id: 'srv-typescript', content: 'v2' })
  })

  it('没有连接时 sync 直接返回 false，不发请求', async () => {
    const harness = installHarness()
    installMarkerSink()

    await expect(syncFile(TS_PATH, 'x')).resolves.toBe(false)
    expect(harness.calls).toHaveLength(0)
  })

  it('句柄过期（10003）时丢掉本地连接，下次开文件重新 start', async () => {
    const harness = installHarness()
    installMarkerSink()
    await openFile({ path: TS_PATH, languageId: 'typescript', content: 'v1' })

    harness.failSync(rpcError(LSP_ERROR_SERVER_UNKNOWN, '句柄不存在'))
    await expect(syncFile(TS_PATH, 'v2')).resolves.toBe(false)

    let state = getLspState()
    expect(state.serverId).toBeNull()
    expect(state.status).toBe('stopped')
    expect(state.connections.typescript).toBeUndefined()

    await openFile({ path: TS_PATH, languageId: 'typescript', content: 'v3' })
    state = getLspState()
    expect(state.status).toBe('ready')
    expect(harness.calls.filter((call) => call.method === Methods.lsp.start)).toHaveLength(2)
  })
})

describe('诊断渲染', () => {
  it('订阅后收到的诊断转成标记写入，并记进状态栏摘要', async () => {
    const harness = installHarness()
    const writes = installMarkerSink()
    await openFile({ path: TS_PATH, languageId: 'typescript', content: 'a' })

    harness.pushDiagnostics(
      diagnosticsEvent({
        diagnostics: [
          {
            range: { start: { line: 1, character: 1 }, end: { line: 1, character: 4 } },
            severity: 'error',
            message: '找不到名称 a',
            source: 'ts',
            code: 'TS2304'
          },
          {
            range: { start: { line: 3, character: 2 }, end: { line: 3, character: 5 } },
            severity: 'warning',
            message: '未使用变量'
          }
        ]
      })
    )

    const last = writes.at(-1)
    expect(last?.path).toBe(TS_PATH)
    expect(last?.markers).toHaveLength(2)
    expect(last?.markers[0]).toEqual({
      severity: MarkerSeverity.Error,
      message: '找不到名称 a',
      source: 'ts',
      code: 'TS2304',
      startLineNumber: 1,
      startColumn: 1,
      endLineNumber: 1,
      endColumn: 4
    })
    expect(last?.markers[1]?.severity).toBe(MarkerSeverity.Warning)

    expect(currentFileDiagnostics(getLspState())).toMatchObject({
      path: TS_PATH,
      problems: 2,
      total: 2,
      worst: 'error'
    })
  })

  it('空诊断是有效载荷：照样写空标记，摘要归零', async () => {
    const harness = installHarness()
    const writes = installMarkerSink()
    await openFile({ path: TS_PATH, languageId: 'typescript', content: 'a' })

    harness.pushDiagnostics(diagnosticsEvent({ diagnostics: [errorDiagnostic(1)] }))
    harness.pushDiagnostics(diagnosticsEvent({ diagnostics: [] }))

    expect(writes.at(-1)?.markers).toEqual([])
    expect(currentFileDiagnostics(getLspState())).toMatchObject({ problems: 0, total: 0 })
  })

  it('后台文件（未打开）的诊断也照常记摘要，切回来时标记还在', async () => {
    const harness = installHarness()
    const writes = installMarkerSink()
    await openFile({ path: TS_PATH, languageId: 'typescript', content: 'a' })
    harness.pushDiagnostics(diagnosticsEvent({ diagnostics: [errorDiagnostic(1)] }))

    harness.pushDiagnostics(
      diagnosticsEvent({ path: TS_PATH_2, diagnostics: [errorDiagnostic(2, '后台文件')] })
    )

    expect(writes.at(-1)?.path).toBe(TS_PATH_2)
    expect(getLspState().diagnostics[TS_PATH_2]?.problems).toBe(1)
    // 当前文件摘要不受影响
    expect(currentFileDiagnostics(getLspState())).toMatchObject({ path: TS_PATH, problems: 1 })
  })
})

describe('停止与清理', () => {
  it('stop 关连接、清标记、清摘要', async () => {
    const harness = installHarness()
    const writes = installMarkerSink()
    await openFile({ path: TS_PATH, languageId: 'typescript', content: 'a' })
    harness.pushDiagnostics(diagnosticsEvent({ diagnostics: [errorDiagnostic(1)] }))
    expect(writes.at(-1)?.markers).toHaveLength(1)

    await stop()

    const stopCall = harness.calls.find((call) => call.method === Methods.lsp.stop)
    expect(stopCall?.params).toMatchObject({ server_id: 'srv-typescript' })
    expect(writes.at(-1)).toEqual({ path: TS_PATH, markers: [] })

    const state = getLspState()
    expect(state.status).toBe('stopped')
    expect(state.serverId).toBeNull()
    expect(state.connections.typescript).toBeUndefined()
    expect(currentFileDiagnostics(state)).toBeNull()
    expect(statusLabel(state)).toBe('TypeScript 语言服务已停止')
  })

  it('stop 时内核说已关闭（幂等）不抛错，本地照样清干净', async () => {
    const harness = installHarness()
    installMarkerSink()
    await openFile({ path: TS_PATH, languageId: 'typescript', content: 'a' })

    harness.failStop(rpcError(LSP_ERROR_SERVER_UNKNOWN, '句柄不存在'))
    await expect(stop()).resolves.toBeUndefined()
    expect(getLspState().serverId).toBeNull()
    expect(getLspState().status).toBe('stopped')
  })

  it('stopAll 关掉所有语言的连接并清空诊断', async () => {
    const harness = installHarness()
    installMarkerSink()
    await openFile({ path: TS_PATH, languageId: 'typescript', content: 'a' })
    await openFile({ path: PY_PATH, languageId: 'python', content: 'b' })

    await stopAll()

    const stopped = harness.calls
      .filter((call) => call.method === Methods.lsp.stop)
      .map((call) => call.params?.server_id)
    expect(stopped.sort()).toEqual(['srv-python', 'srv-typescript'])
    const state = getLspState()
    expect(state.connections).toEqual({})
    expect(state.diagnostics).toEqual({})
    expect(state.status).toBe('stopped')
  })

  it('refreshServers 按内核清单回填，内核说没有连接时清掉本地句柄', async () => {
    const harness = installHarness()
    installMarkerSink()
    await openFile({ path: TS_PATH, languageId: 'typescript', content: 'a' })

    expect(await refreshServers()).toEqual([])
    expect(harness.calls.some((call) => call.method === Methods.lsp.servers)).toBe(true)
    expect(getLspState().serverId).toBeNull()
  })
})

describe('状态文案', () => {
  it('语言名映射覆盖常见语言，认不出的原样显示', () => {
    expect(languageLabel('typescript')).toBe('TypeScript')
    expect(languageLabel('typescriptreact')).toBe('TypeScript React')
    expect(languageLabel('zig')).toBe('zig')
    expect(languageLabel(null)).toBe('')
  })

  it('idle 时状态栏不出字', () => {
    expect(statusLabel(getLspState())).toBe('')
    expect(getLspState().status).toBe('idle')
  })
})
