/**
 * store 行为测试。
 *
 * 直接驱动导出的动作函数、用 `getEditorState()` 读快照——不渲染 React。
 * 所有内核调用都走假的 `window.aiide`，断言的是「发了什么请求」与「状态怎么变」。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  Methods,
  type SearchContentResult,
  type SearchFilesResult,
  type TreeEntry,
  type WorkspaceOpenResult,
  type WorkspaceReadFileResult,
  type WorkspaceTreeResult,
  type WorkspaceWriteFileResult
} from '@ai-ide/shared-types'

import type { AiIdeApi } from '../../shared/ipc'
import {
  closeTab,
  getEditorState,
  handleWorkspaceChanged,
  handleWorkspaceIndexed,
  openFile,
  openMatch,
  openWorkspace,
  runContentSearch,
  runFileSearch,
  saveTab,
  tabIsDirty,
  updateContent
} from './store'

const ROOT = 'C:\\ws'
const FILE_A = `${ROOT}\\a.ts`
const FILE_B = `${ROOT}\\b.ts`

function entry(name: string, path: string, kind: 'file' | 'dir'): TreeEntry {
  return { name, path, kind, size: 0, mtime: 0, ignored: false }
}

interface Call {
  method: string
  params: Record<string, unknown> | undefined
}

interface Harness {
  calls: Call[]
  confirm: ReturnType<typeof vi.fn>
  setReadResult: (patch: Partial<WorkspaceReadFileResult>) => void
  setTreeEntries: (entries: TreeEntry[]) => void
  failNextRequest: (message: string) => void
}

function installHarness(): Harness {
  const calls: Call[] = []

  let readResult: WorkspaceReadFileResult = {
    content: 'hello\n',
    encoding: 'utf-8',
    eol: 'lf',
    size: 6,
    truncated: false,
    hash: 'h1'
  }
  let treeEntries: TreeEntry[] = [entry('src', `${ROOT}\\src`, 'dir'), entry('a.ts', FILE_A, 'file')]
  let failure: string | null = null

  const openResult: WorkspaceOpenResult = {
    workspace_id: 'ws-1',
    root: ROOT,
    name: 'ws',
    stats: { files: 2, dirs: 1, ignored: 0 },
    indexing: false
  }

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
    // M5：导出会话的落点由主进程的系统对话框决定，渲染侧只拿返回的路径。
    pickSavePath: async () => null,
    pickWorkspaceDirectory: async () => ROOT,
    request: async (method: string, params?: unknown) => {
      calls.push({ method, params: params as Record<string, unknown> | undefined })
      if (failure) {
        const message = failure
        failure = null
        throw new Error(message)
      }
      switch (method) {
        case Methods.workspace.open:
          return openResult
        case Methods.workspace.tree:
          return { entries: treeEntries } satisfies WorkspaceTreeResult
        case Methods.workspace.readFile:
          return readResult
        case Methods.workspace.writeFile:
          return { ok: true, hash: 'h2' } satisfies WorkspaceWriteFileResult
        case Methods.search.files:
          return { matches: [{ path: 'a.ts', score: 1 }] } satisfies SearchFilesResult
        case Methods.search.content:
          return {
            matches: [{ path: 'a.ts', line: 1, column: 1, preview: 'hello' }],
            truncated: false
          } satisfies SearchContentResult
        default:
          return { ok: true }
      }
    },
    onCoreStatus: () => () => undefined,
    onCoreLog: () => () => undefined,
    onWorkspaceChanged: () => () => undefined,
    onWorkspaceIndexed: () => () => undefined,
    getCredentialStatus: async () => ({
      encryptionAvailable: false,
      vaultPath: 'C:\\ud\\credentials.enc',
      managedProviderIds: [],
      lastError: null
    }),
    forgetCredential: async () => ({
      encryptionAvailable: false,
      vaultPath: 'C:\\ud\\credentials.enc',
      managedProviderIds: [],
      lastError: null
    }),
    onAiEvent: () => () => undefined,
    onCredentialStatus: () => () => undefined,
    onTerminalData: () => () => undefined,
    onTerminalExit: () => () => undefined
  }

  const confirm = vi.fn(() => true)
  vi.stubGlobal('window', { aiide: api, confirm })

  return {
    calls,
    confirm,
    setReadResult: (patch) => {
      readResult = { ...readResult, ...patch }
    },
    setTreeEntries: (entries) => {
      treeEntries = entries
    },
    failNextRequest: (message) => {
      failure = message
    }
  }
}

let h: Harness

function callsOf(method: string): Call[] {
  return h.calls.filter((call) => call.method === method)
}

beforeEach(async () => {
  h = installHarness()
  await openWorkspace(ROOT)
})

describe('工作区', () => {
  it('打开后载入根目录，目录排在文件之前', () => {
    const state = getEditorState()
    expect(state.workspace?.root).toBe(ROOT)
    expect(state.opening).toBe(false)
    expect(state.tree['']?.map((item) => item.name)).toEqual(['src', 'a.ts'])
  })

  it('打开失败时记录 workspaceError，不写入工作区', async () => {
    h.failNextRequest('内核对不上')
    await openWorkspace('C:\\bad')
    const state = getEditorState()
    expect(state.opening).toBe(false)
    expect(state.workspaceError).toContain('内核对不上')
  })

  it('索引完成事件把最终统计写回工作区', () => {
    handleWorkspaceIndexed({
      workspace_id: 'ws-1',
      stats: { files: 9, dirs: 3, ignored: 1 }
    })
    expect(getEditorState().workspace?.stats.files).toBe(9)
  })
})

describe('标签', () => {
  it('打开文件后填充内容且未标脏', async () => {
    await openFile(FILE_A)
    const state = getEditorState()
    expect(state.tabs).toHaveLength(1)
    expect(state.activePath).toBe(FILE_A)
    expect(state.tabs[0]?.content).toBe('hello\n')
    expect(tabIsDirty(state.tabs[0]!)).toBe(false)
  })

  it('重复打开同一文件只切换激活，不再读盘', async () => {
    await openFile(FILE_A)
    await openFile(FILE_B)
    await openFile(FILE_A)
    const state = getEditorState()
    expect(state.tabs).toHaveLength(2)
    expect(state.activePath).toBe(FILE_A)
    expect(callsOf(Methods.workspace.readFile)).toHaveLength(2)
  })

  it('编辑后标脏，保存后回写并刷新哈希', async () => {
    await openFile(FILE_A)
    updateContent(FILE_A, 'hello world\n')
    expect(tabIsDirty(getEditorState().tabs[0]!)).toBe(true)

    const saved = await saveTab()
    expect(saved).toBe(true)

    const tab = getEditorState().tabs[0]!
    expect(tabIsDirty(tab)).toBe(false)
    expect(tab.hash).toBe('h2')
    expect(callsOf(Methods.workspace.writeFile)[0]?.params).toMatchObject({
      path: FILE_A,
      content: 'hello world\n',
      expected_hash: 'h1'
    })
  })

  it('被截断的文件拒绝写回，避免损坏原文件', async () => {
    h.setReadResult({ truncated: true })
    await openFile(`${ROOT}\\big.bin`)
    updateContent(`${ROOT}\\big.bin`, 'x')

    expect(await saveTab()).toBe(false)
    expect(callsOf(Methods.workspace.writeFile)).toHaveLength(0)
    expect(getEditorState().tabs[0]?.error).toContain('截断')
  })

  it('关闭脏标签需要确认；取消则保留', async () => {
    await openFile(FILE_A)
    updateContent(FILE_A, 'changed')

    h.confirm.mockReturnValueOnce(false)
    closeTab(FILE_A)
    expect(getEditorState().tabs).toHaveLength(1)

    h.confirm.mockReturnValueOnce(true)
    closeTab(FILE_A)
    expect(getEditorState().tabs).toHaveLength(0)
    expect(getEditorState().activePath).toBeNull()
  })
})

describe('检索', () => {
  it('空查询清空结果，非空查询写入结果', async () => {
    await runFileSearch('a')
    expect(getEditorState().fileMatches).toHaveLength(1)

    await runFileSearch('   ')
    expect(getEditorState().fileMatches).toHaveLength(0)

    await runContentSearch('hello')
    const state = getEditorState()
    expect(state.contentMatches[0]?.line).toBe(1)
    expect(state.contentTruncated).toBe(false)
  })

  it('从命中跳转：拼出与文件树一致的绝对路径并写入 reveal', async () => {
    await openMatch('a.ts', 3, 7)
    const state = getEditorState()
    expect(state.activePath).toBe(FILE_A)
    expect(state.tabs[0]?.path).toBe(FILE_A)
    expect(state.reveal).toMatchObject({ path: FILE_A, line: 3, column: 7 })
  })
})

describe('外部改动', () => {
  // 注意：内核 watcher 上报的是**相对工作区根、以 `/` 分隔**的路径
  // （见 crates/workspace/src/watcher.rs 的 to_relative）。早期单测直接用了绝对路径，
  // 与真实协议不符，导致「事件到了但 UI 不响应」这类缺陷在单测里被漏掉。
  it('干净标签收到相对路径 modified 事件后自动重载', async () => {
    await openFile(FILE_A)
    handleWorkspaceChanged({
      workspace_id: 'ws-1',
      changes: [{ path: 'a.ts', kind: 'modified', old_path: null }]
    })

    await vi.waitFor(() => {
      expect(callsOf(Methods.workspace.readFile)).toHaveLength(2)
    })
  })

  it('绝对路径形式的事件同样兼容（协议演进防回退）', async () => {
    await openFile(FILE_A)
    handleWorkspaceChanged({
      workspace_id: 'ws-1',
      changes: [{ path: FILE_A, kind: 'modified', old_path: null }]
    })

    await vi.waitFor(() => {
      expect(callsOf(Methods.workspace.readFile)).toHaveLength(2)
    })
  })

  it('不相干的相对路径不会误触发重载', async () => {
    await openFile(FILE_A)
    handleWorkspaceChanged({
      workspace_id: 'ws-1',
      changes: [{ path: 'src/other.ts', kind: 'modified', old_path: null }]
    })

    expect(callsOf(Methods.workspace.readFile)).toHaveLength(1)
  })

  it('脏标签收到 modified 事件时保留本地内容并提示', async () => {
    await openFile(FILE_A)
    updateContent(FILE_A, 'local edit')
    handleWorkspaceChanged({
      workspace_id: 'ws-1',
      changes: [{ path: 'a.ts', kind: 'modified', old_path: null }]
    })

    const state = getEditorState()
    expect(state.tabs[0]?.content).toBe('local edit')
    expect(state.notice?.kind).toBe('warn')
  })

  it('标签收到 deleted 事件后直接关闭', async () => {
    await openFile(FILE_A)
    handleWorkspaceChanged({
      workspace_id: 'ws-1',
      changes: [{ path: 'a.ts', kind: 'deleted', old_path: null }]
    })

    expect(getEditorState().tabs).toHaveLength(0)
  })

  it('其它工作区的事件被忽略', async () => {
    await openFile(FILE_A)
    handleWorkspaceChanged({
      workspace_id: 'ws-other',
      changes: [{ path: 'a.ts', kind: 'deleted', old_path: null }]
    })

    expect(getEditorState().tabs).toHaveLength(1)
  })
})
