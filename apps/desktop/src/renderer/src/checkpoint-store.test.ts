/**
 * M5 渲染侧检查点状态行为测试。
 *
 * 与 `session-store.test.ts` 同一套路：不渲染 React，直接驱动动作函数，用假的
 * `window.aiide` 顶替 IPC，断言「发了什么请求」与「状态怎么变」。
 *
 * 这里钉的是检查点最容易出人命、出错了却看起来像「内核没生效」的几条：
 *
 * - 没有工作区时不许发请求（`checkpoint.*` 缺省取活动工作区，没工作区等于打到空气上）；
 * - 有未保存的标签页时**不许**回滚（回滚会覆盖磁盘，脏内容刷掉就找不回来）；
 * - 回滚之后必须回读文件树与已打开标签页（否则编辑器显示的还是回滚前的内容）；
 * - `verified = false` 时如实报「校验未通过」，不能显示回滚成功。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  Methods,
  type CheckpointRestoreResult,
  type CheckpointView,
  type TreeEntry,
  type WorkspaceOpenResult,
  type WorkspaceReadFileResult,
  type WorkspaceTreeResult
} from '@ai-ide/shared-types'

import type { AiIdeApi } from '../../shared/ipc'
import { formatBytes, formatCheckpointTime, originText } from './checkpoint-api'
import {
  createCheckpoint,
  deleteCheckpoint,
  dismissCheckpointHint,
  getCheckpointState,
  refreshCheckpoints,
  resetCheckpointState,
  restoreCheckpoint
} from './checkpoint-store'
import {
  closeTab,
  closeWorkspace,
  getEditorState,
  openFile,
  openWorkspace,
  updateContent
} from './store'

const ROOT = 'C:\\ws'
const FILE_A = `${ROOT}\\a.ts`

interface Call {
  method: string
  params: Record<string, unknown> | undefined
}

interface Harness {
  calls: Call[]
  /** 让下一次请求失败；给了 `method` 就只让该方法的请求失败。 */
  failNext: (message: string, method?: string) => void
  /** 下一次 `checkpoint.restore` 的返回（改 `verified` 用）。 */
  setRestoreResult: (patch: Partial<CheckpointRestoreResult>) => void
}

function entry(name: string, path: string, kind: 'file' | 'dir'): TreeEntry {
  return { name, path, kind, size: 0, mtime: 0, ignored: false }
}

function checkpoint(id: string, label: string, createdAt = 1000): CheckpointView {
  return {
    checkpoint_id: id,
    label,
    workspace_root: ROOT,
    origin: 'manual',
    created_at_ms: createdAt,
    files: [
      { path: 'a.ts', kind: 'modified', before_hash: 'h0', size: 12 },
      { path: 'b.ts', kind: 'created', size: 8 }
    ],
    total_bytes: 20,
    skipped: 0
  }
}

function installHarness(): Harness {
  const calls: Call[] = []
  let failure: { message: string; method: string | null } | null = null
  let checkpoints: CheckpointView[] = [checkpoint('c-1', '手动快照 1', 2000), checkpoint('c-2', '手动快照 2', 1000)]
  let created = 0

  const readResult: WorkspaceReadFileResult = {
    content: 'hello\n',
    encoding: 'utf-8',
    eol: 'lf',
    size: 6,
    truncated: false,
    hash: 'h1'
  }
  let restoreResult: CheckpointRestoreResult = {
    checkpoint_id: 'c-1',
    verified: true,
    files: [
      { path: 'a.ts', status: 'restored', hash: 'h0' },
      { path: 'b.ts', status: 'removed' }
    ]
  }

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
    pickWorkspaceDirectory: async () => ROOT,
    pickSavePath: async () => null,
    request: async (method: string, params?: unknown) => {
      calls.push({ method, params: params as Record<string, unknown> | undefined })
      if (failure && (failure.method === null || failure.method === method)) {
        const text = failure.message
        failure = null
        throw new Error(text)
      }
      const p = (params ?? {}) as Record<string, unknown>
      switch (method) {
        case Methods.workspace.open:
          return openResult
        case Methods.workspace.tree:
          return { entries: [entry('a.ts', FILE_A, 'file')] } satisfies WorkspaceTreeResult
        case Methods.workspace.readFile:
          return readResult
        case Methods.checkpoint.list:
          return { checkpoints, total: checkpoints.length }
        case Methods.checkpoint.create: {
          created += 1
          const next = checkpoint(`c-new-${created}`, p.label ? String(p.label) : '手动快照', 9000 + created)
          checkpoints = [next, ...checkpoints]
          return { checkpoint: next }
        }
        case Methods.checkpoint.restore:
          return restoreResult
        case Methods.checkpoint.delete: {
          const id = p.checkpoint_id as string
          const existed = checkpoints.some((item) => item.checkpoint_id === id)
          checkpoints = checkpoints.filter((item) => item.checkpoint_id !== id)
          return { checkpoint_id: id, removed: existed, freed_bytes: 4096 }
        }
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
    onTerminalData: () => () => undefined,
    onTerminalExit: () => () => undefined,
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
    failNext: (text, method) => {
      failure = { message: text, method: method ?? null }
    },
    setRestoreResult: (patch) => {
      restoreResult = { ...restoreResult, ...patch }
    }
  }
}

let h: Harness

function callsOf(method: string): Call[] {
  return h.calls.filter((call) => call.method === method)
}

beforeEach(async () => {
  // 编辑器状态是模块级单例，跨用例不清就会让「没工作区」这类用例拿到上一个用例的工作区。
  for (const tab of [...getEditorState().tabs]) closeTab(tab.path, true)
  if (getEditorState().workspace !== null) await closeWorkspace()
  vi.unstubAllGlobals()
  h = installHarness()
  resetCheckpointState()
})

describe('refreshCheckpoints', () => {
  it('没有工作区时不发请求，给出提示', async () => {
    const ok = await refreshCheckpoints()

    expect(ok).toBe(false)
    expect(callsOf(Methods.checkpoint.list)).toHaveLength(0)
    expect(getCheckpointState().hint).toContain('打开工作区')
  })

  it('有工作区时按分页拉取并填充列表', async () => {
    await openWorkspace(ROOT)

    const ok = await refreshCheckpoints()

    expect(ok).toBe(true)
    const [call] = callsOf(Methods.checkpoint.list)
    expect(call?.params).toMatchObject({ limit: 20, offset: 0 })
    // 不传 workspace_id = 让内核取活动工作区；外壳不自己拼一个可能过期的 id。
    expect(call?.params?.workspace_id).toBeUndefined()
    expect(getCheckpointState().checkpoints.map((item) => item.checkpoint_id)).toEqual(['c-1', 'c-2'])
    expect(getCheckpointState().total).toBe(2)
    expect(getCheckpointState().error).toBeNull()
  })

  it('失败时落 error 并复位 loading', async () => {
    await openWorkspace(ROOT)
    h.failNext('内核未响应', Methods.checkpoint.list)

    const ok = await refreshCheckpoints()

    expect(ok).toBe(false)
    expect(getCheckpointState().error).toBe('内核未响应')
    expect(getCheckpointState().loading).toBe(false)
  })
})

describe('createCheckpoint', () => {
  it('成功后插到列表首位并报出覆盖文件数', async () => {
    await openWorkspace(ROOT)
    await refreshCheckpoints()

    const ok = await createCheckpoint('  改动前  ')

    expect(ok).toBe(true)
    expect(callsOf(Methods.checkpoint.create)[0]?.params?.label).toBe('改动前')
    expect(getCheckpointState().checkpoints[0]?.checkpoint_id).toBe('c-new-1')
    expect(getCheckpointState().hint).toContain('2 个文件')
    expect(getCheckpointState().busy).toBe(false)
  })

  it('没工作区时直接拒绝', async () => {
    const ok = await createCheckpoint()

    expect(ok).toBe(false)
    expect(callsOf(Methods.checkpoint.create)).toHaveLength(0)
    expect(getCheckpointState().hint).toContain('打开工作区')
  })
})

describe('restoreCheckpoint', () => {
  it('回滚成功后回读文件树与已打开标签页', async () => {
    await openWorkspace(ROOT)
    await openFile(FILE_A)
    h.calls.length = 0

    const ok = await restoreCheckpoint('c-1')

    expect(ok).toBe(true)
    expect(callsOf(Methods.checkpoint.restore)[0]?.params?.checkpoint_id).toBe('c-1')
    // 回滚改了盘：树要重拉、打开的标签要重读，否则编辑器还显示旧内容。
    expect(callsOf(Methods.workspace.tree).length).toBeGreaterThan(0)
    expect(callsOf(Methods.workspace.readFile).length).toBeGreaterThan(0)
    expect(getEditorState().tabs[0]?.content).toBe('hello\n')
    expect(getCheckpointState().hint).toContain('校验通过')
    expect(getCheckpointState().lastRestore?.verified).toBe(true)
  })

  it('校验未通过时如实报失败，不显示成功', async () => {
    await openWorkspace(ROOT)
    h.setRestoreResult({
      verified: false,
      files: [
        { path: 'a.ts', status: 'failed', detail: '写回后哈希不一致' },
        { path: 'b.ts', status: 'restored', hash: 'h0' }
      ]
    })

    const ok = await restoreCheckpoint('c-1')

    expect(ok).toBe(false)
    expect(getCheckpointState().hint).toContain('校验未通过')
    expect(getCheckpointState().lastRestore?.files[0]?.status).toBe('failed')
  })

  it('有未保存的标签页时拒绝回滚，不发请求', async () => {
    await openWorkspace(ROOT)
    await openFile(FILE_A)
    updateContent(FILE_A, '改了一半\n')

    const ok = await restoreCheckpoint('c-1')

    expect(ok).toBe(false)
    expect(callsOf(Methods.checkpoint.restore)).toHaveLength(0)
    expect(getCheckpointState().hint).toContain('未保存')
    // 脏内容必须还在：拒绝回滚的整个意义就是不丢它。
    expect(getEditorState().tabs[0]?.content).toBe('改了一半\n')

    closeTab(FILE_A, true)
  })

  it('没工作区时拒绝', async () => {
    const ok = await restoreCheckpoint('c-1')

    expect(ok).toBe(false)
    expect(callsOf(Methods.checkpoint.restore)).toHaveLength(0)
  })

  it('内核报错时落 error', async () => {
    await openWorkspace(ROOT)
    h.failNext('检查点不存在：c-9', Methods.checkpoint.restore)

    const ok = await restoreCheckpoint('c-9')

    expect(ok).toBe(false)
    expect(getCheckpointState().error).toBe('检查点不存在：c-9')
    expect(getCheckpointState().busy).toBe(false)
  })
})

describe('deleteCheckpoint', () => {
  it('删掉后从列表移除并回收体积', async () => {
    await openWorkspace(ROOT)
    await refreshCheckpoints()

    const ok = await deleteCheckpoint('c-1')

    expect(ok).toBe(true)
    expect(getCheckpointState().checkpoints.map((item) => item.checkpoint_id)).toEqual(['c-2'])
    expect(getCheckpointState().total).toBe(1)
    expect(getCheckpointState().hint).toContain('4096')
  })
})

describe('dismissCheckpointHint', () => {
  it('清掉 hint 与 error', async () => {
    await openWorkspace(ROOT)
    h.failNext('炸了', Methods.checkpoint.list)
    await refreshCheckpoints()

    dismissCheckpointHint()

    expect(getCheckpointState().hint).toBeNull()
    expect(getCheckpointState().error).toBeNull()
  })
})

describe('checkpoint-api 展示辅助', () => {
  it('体积格式化按量级切换单位', () => {
    expect(formatBytes(0)).toBe('0 B')
    expect(formatBytes(900)).toBe('900 B')
    expect(formatBytes(2048)).toBe('2.0 KB')
    expect(formatBytes(5 * 1024 * 1024)).toBe('5.0 MB')
  })

  it('时间与来源标签', () => {
    expect(formatCheckpointTime(0)).toBe('—')
    expect(formatCheckpointTime(1_700_000_000_000)).toMatch(/^\d{2}-\d{2} \d{2}:\d{2}$/)
    expect(originText(checkpoint('c', 'x'))).toBe('手动')
    expect(originText({ ...checkpoint('c', 'x'), origin: 'agent' })).toBe('Agent 自动')
  })
})
