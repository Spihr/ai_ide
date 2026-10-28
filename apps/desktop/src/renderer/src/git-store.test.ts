/**
 * M6 Git 面板（外壳侧）的状态与动作测试。
 *
 * 与其它 store 测试同一套路：不渲染 React，用假的 `window.aiide` 顶替 IPC，
 * 直接驱动动作函数，断言「发了什么请求」「状态怎么变」。
 *
 * 这里钉的是 Git 面板最容易「看着对、其实错」的几条：
 *
 * - **分组不是二选一**：`index` 与 `worktree` 两列独立判定，改完又暂存又改的文件必须
 *   同时出现在两组里，否则「取消暂存」这个按钮会落在错误的组上；
 * - **diff 基准跟着分组走**：点已暂存条目必须发 `staged: true`，混了基准就会让用户
 *   对着 A 内容点提交、提交进去的是 B；
 * - **未跟踪文件不发 diff**（两侧都没有基准版本），由 UI 提示「暂存后才能看」；
 * - **连点两条变更时，慢回来的旧结果不许覆盖新选中**；
 * - **提交前置在本地就拦掉**（空信息 / 空暂存区），不让用户白等一次往返。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  Methods,
  type GitBranchesResult,
  type GitCommitResult,
  type GitConflictReadResult,
  type GitConflictResolveResult,
  type GitDiffResult,
  type GitHunk,
  type GitStageHunkResult,
  type GitStageResult,
  type GitStatusEntry,
  type GitStatusResult,
  type GitUnstageHunkResult,
  type WorkspaceChangedEvent
} from '@ai-ide/shared-types'

import type { AiIdeApi } from '../../shared/ipc'
import type { GitDiffHunk } from './git-api'
import {
  GIT_REFRESH_DELAY_MS,
  changeCount,
  clearGitSelection,
  commitStaged,
  disposeGitEvents,
  entryStatusLabel,
  getGitState,
  groupEntries,
  initGit,
  loadConflict,
  refreshGit,
  reloadConflict,
  resetGitState,
  resolveConflict,
  selectGitEntry,
  setCommitMessage,
  stageAll,
  stageEntry,
  stageHunk,
  stagedCount,
  unstageAll,
  unstageEntry,
  unstageHunk
} from './git-store'
import { closeTab, closeWorkspace, getEditorState, openWorkspace } from './store'

const ROOT = 'C:\\ws'

interface Call {
  method: string
  params: Record<string, unknown> | undefined
}

interface Harness {
  calls: Call[]
  /** 下一次 `git.status` 返回这份变更清单。 */
  setEntries: (entries: GitStatusEntry[]) => void
  /** 下一次 `git.status` 抛错（模拟不是仓库）。 */
  failStatus: (message: string) => void
  /** 下一次 `git.diff` 抛错。 */
  failDiff: (message: string) => void
  /** 让下一条 `git.diff` 挂起，直到 `release` 放行（测竞态用）。 */
  deferDiff: () => { release: (diff: GitDiffResult) => void }
  /** 模拟内核推进一条 `workspace.changed`。 */
  pushWorkspaceChanged: (workspaceId: string) => void
  /** 让下一次 `git.conflict_resolve` 回执带上「还剩这些冲突文件」。 */
  setRemaining: (paths: string[]) => void
  /** `initGit` 之后才会被赋值。 */
  hasWorkspaceListener: () => boolean
}

function entry(overrides: Partial<GitStatusEntry> & Pick<GitStatusEntry, 'path'>): GitStatusEntry {
  return {
    index: '.',
    worktree: '.',
    kind: 'changed',
    ...overrides
  }
}

function statusOf(entries: GitStatusEntry[], overrides: Partial<GitStatusResult> = {}): GitStatusResult {
  return {
    workspace_id: 'ws-1',
    branch: 'main',
    detached: false,
    head_oid: 'a'.repeat(40),
    upstream: 'origin/main',
    ahead: 1,
    behind: 0,
    clean: entries.length === 0,
    entries,
    ...overrides
  }
}

function diffOf(path: string, staged: boolean, text = '@@ -1,1 +1,1 @@\n-a\n+b\n'): GitDiffResult {
  return { path, staged, diff: text, bytes: text.length, truncated: false }
}

function installHarness(): Harness {
  const calls: Call[] = []
  let entries: GitStatusEntry[] = []
  let failure: string | null = null
  let diffFailure: string | null = null
  let deferNextDiff = false
  let pendingDiff: { resolve: (diff: GitDiffResult) => void } | null = null
  let workspaceListener: ((event: WorkspaceChangedEvent) => void) | null = null
  /** 下一次 `git.conflict_resolve` 回执里「还剩哪些冲突文件」。 */
  let remaining: string[] = []

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
      const typed = params as Record<string, unknown> | undefined
      calls.push({ method, params: typed })

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
        case Methods.git.status: {
          if (failure !== null) {
            const message = failure
            failure = null
            throw new Error(message)
          }
          return statusOf(entries)
        }
        case Methods.git.branches: {
          const branches: GitBranchesResult = {
            workspace_id: 'ws-1',
            current: 'main',
            detached: false,
            branches: [
              { name: 'dev', oid: 'b'.repeat(40), short_oid: 'bbbbbbb', is_current: false, is_remote: false, upstream: null },
              { name: 'main', oid: 'a'.repeat(40), short_oid: 'aaaaaaa', is_current: true, is_remote: false, upstream: 'origin/main' },
              { name: 'origin/main', oid: 'a'.repeat(40), short_oid: 'aaaaaaa', is_current: false, is_remote: true, upstream: null }
            ]
          }
          return branches
        }
        case Methods.git.diff: {
          const path = String(typed?.path ?? '')
          const staged = typed?.staged === true
          if (deferNextDiff) {
            deferNextDiff = false
            return new Promise<GitDiffResult>((resolve) => {
              pendingDiff = { resolve }
            })
          }
          if (diffFailure !== null) {
            const message = diffFailure
            diffFailure = null
            throw new Error(message)
          }
          return diffOf(path, staged)
        }
        case Methods.git.stage: {
          const result: GitStageResult = {
            paths: (typed?.paths as string[] | undefined) ?? [],
            all: typed?.all === true,
            unstage: typed?.unstage === true,
            staged_total: 1
          }
          return result
        }
        // 单 hunk 与冲突两组：回显就用请求里的四个行号，外壳不该自己再算一遍。
        case Methods.git.stageHunk:
        case Methods.git.unstageHunk: {
          const hunk: GitHunk = {
            path: String(typed?.path ?? ''),
            index: Number(typed?.index ?? 0),
            old_start: Number(typed?.old_start ?? 1),
            old_lines: Number(typed?.old_lines ?? 1),
            new_start: Number(typed?.new_start ?? 1),
            new_lines: Number(typed?.new_lines ?? 1),
            header: '',
            lines: []
          }
          if (method === Methods.git.stageHunk) {
            const staged: GitStageHunkResult = { hunk, staged_total: 3 }
            return staged
          }
          const unstaged: GitUnstageHunkResult = { hunk, staged_total: 2 }
          return unstaged
        }
        case Methods.git.conflictRead: {
          const read: GitConflictReadResult = {
            path: String(typed?.path ?? ''),
            kind: 'both_modified',
            xy: 'UU',
            present: true,
            blocks: [
              {
                index: 0,
                start_line: 3,
                end_line: 7,
                ours_label: 'HEAD',
                theirs_label: 'feature',
                ours: ['ours'],
                theirs: ['theirs']
              }
            ]
          }
          return read
        }
        case Methods.git.conflictResolve: {
          const resolved: GitConflictResolveResult = {
            path: String(typed?.path ?? ''),
            kind: 'both_modified',
            resolved: true,
            still_conflicted: false,
            resolved_blocks: 1,
            remaining
          }
          return resolved
        }
        case Methods.git.commit: {
          const result: GitCommitResult = {
            oid: 'c'.repeat(40),
            short_oid: 'ccccccc',
            summary: 'feat: 提交',
            branch: 'main',
            files: 2,
            author: 'Test <test@example.com>'
          }
          return result
        }
        default:
          return { ok: true }
      }
    },
    onCoreStatus: () => () => undefined,
    onCoreLog: () => () => undefined,
    onWorkspaceChanged: (listener) => {
      workspaceListener = listener
      return () => {
        workspaceListener = null
      }
    },
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
    setEntries: (next) => {
      entries = next
    },
    failStatus: (message) => {
      failure = message
    },
    failDiff: (message) => {
      diffFailure = message
    },
    deferDiff: () => {
      deferNextDiff = true
      return {
        release: (diff: GitDiffResult) => {
          const pending = pendingDiff
          pendingDiff = null
          pending?.resolve(diff)
        }
      }
    },
    pushWorkspaceChanged: (workspaceId) => {
      workspaceListener?.({ workspace_id: workspaceId, changes: [] })
    },
    setRemaining: (paths) => {
      remaining = paths
    },
    hasWorkspaceListener: () => workspaceListener !== null
  }
}

let h: Harness

function callsOf(method: string): Call[] {
  return h.calls.filter((call) => call.method === method)
}

beforeEach(async () => {
  for (const tab of [...getEditorState().tabs]) closeTab(tab.path, true)
  if (getEditorState().workspace !== null) await closeWorkspace()
  vi.unstubAllGlobals()
  vi.useRealTimers()
  disposeGitEvents()
  h = installHarness()
  resetGitState()
})

describe('分组与计数', () => {
  it('两列字母独立判定：又改又暂存的文件同时出现在两组', () => {
    const groups = groupEntries([
      entry({ path: 'a.ts', index: 'M', worktree: 'M', kind: 'changed' }),
      entry({ path: 'b.ts', index: 'M', kind: 'changed' }),
      entry({ path: 'c.ts', worktree: 'M', kind: 'changed' }),
      entry({ path: 'new.ts', worktree: '?', kind: 'untracked' }),
      entry({ path: 'ignored.ts', worktree: '!', kind: 'ignored' })
    ])

    expect(groups.staged.map((item) => item.path)).toEqual(['a.ts', 'b.ts'])
    expect(groups.unstaged.map((item) => item.path)).toEqual(['a.ts', 'c.ts', 'new.ts'])
  })

  it('忽略文件不进任何一组（摆上提交界面就是误导）', () => {
    const groups = groupEntries([
      entry({ path: 'dist/', worktree: '!', kind: 'ignored' }),
      entry({ path: 'a.ts', worktree: 'M', kind: 'changed' })
    ])

    expect(groups.staged).toHaveLength(0)
    expect(groups.unstaged.map((item) => item.path)).toEqual(['a.ts'])
  })

  it('未跟踪只进「未暂存」，即使两列都是 `?`', () => {
    const groups = groupEntries([entry({ path: 'new.ts', index: '?', worktree: '?', kind: 'untracked' })])

    expect(groups.staged).toHaveLength(0)
    expect(groups.unstaged).toHaveLength(1)
  })

  it('stagedCount 只数暂存侧，changeCount 数全部（含同时出现在两组的那一个）', () => {
    const status = statusOf([
      entry({ path: 'a.ts', index: 'M', worktree: 'M', kind: 'changed' }),
      entry({ path: 'b.ts', index: 'M', kind: 'changed' }),
      entry({ path: 'c.ts', worktree: 'M', kind: 'changed' }),
      entry({ path: 'ignored.ts', worktree: '!', kind: 'ignored' })
    ])

    expect(stagedCount(status)).toBe(2)
    expect(changeCount(status)).toBe(3)
    expect(stagedCount(null)).toBe(0)
    expect(changeCount(null)).toBe(0)
  })

  it('状态字母有人话，未知字母原样返回', () => {
    expect(entryStatusLabel('M')).toBe('修改')
    expect(entryStatusLabel('?')).toBe('未跟踪')
    expect(entryStatusLabel('X')).toBe('X')
  })
})

describe('refreshGit', () => {
  it('没有工作区时不发请求，给一句提示', async () => {
    const ok = await refreshGit()

    expect(ok).toBe(false)
    expect(h.calls).toHaveLength(0)
    expect(getGitState().hint).toContain('打开工作区')
  })

  it('并行拉 status 与 branches，带上工作区 id', async () => {
    await openWorkspace(ROOT)
    h.setEntries([entry({ path: 'a.ts', worktree: 'M', kind: 'changed' })])

    const ok = await refreshGit()

    expect(ok).toBe(true)
    expect(callsOf(Methods.git.status)).toEqual([
      { method: Methods.git.status, params: { workspace_id: 'ws-1' } }
    ])
    expect(callsOf(Methods.git.branches)).toEqual([
      { method: Methods.git.branches, params: { workspace_id: 'ws-1' } }
    ])
    const state = getGitState()
    expect(state.status?.branch).toBe('main')
    expect(state.branches.map((branch) => branch.name)).toEqual(['dev', 'main', 'origin/main'])
    expect(state.loading).toBe(false)
  })

  it('不是仓库（7002）时清空清单并给出人话，不留上一个工作区的残留', async () => {
    await openWorkspace(ROOT)
    h.setEntries([entry({ path: 'a.ts', worktree: 'M', kind: 'changed' })])
    await refreshGit()

    h.failStatus('git.status 失败 [7002]: not a git repository')
    const ok = await refreshGit()

    expect(ok).toBe(false)
    const state = getGitState()
    expect(state.status).toBeNull()
    expect(state.branches).toHaveLength(0)
    expect(state.error).toContain('不是 Git 仓库')
  })

  it('输入框草稿跨刷新保留（提交信息不该被一次 status 刷掉）', async () => {
    await openWorkspace(ROOT)
    await refreshGit()
    setCommitMessage('feat: 半路写一半的信息')

    await refreshGit()

    expect(getGitState().message).toBe('feat: 半路写一半的信息')
  })
})

describe('selectGitEntry', () => {
  it('点已暂存条目发 staged: true，并落 diff', async () => {
    await openWorkspace(ROOT)
    await refreshGit()

    const ok = await selectGitEntry(entry({ path: 'a.ts', index: 'M', kind: 'changed' }), true)

    expect(ok).toBe(true)
    const [diffCall] = callsOf(Methods.git.diff)
    expect(diffCall?.params).toEqual({
      workspace_id: 'ws-1',
      path: 'a.ts',
      staged: true,
      context_lines: 3
    })
    expect(getGitState().diff?.staged).toBe(true)
    expect(getGitState().diffLoading).toBe(false)
  })

  it('点未暂存条目发 staged: false，选中项记住基准', async () => {
    await openWorkspace(ROOT)
    await refreshGit()

    await selectGitEntry(entry({ path: 'a.ts', worktree: 'M', kind: 'changed' }), false)

    expect(callsOf(Methods.git.diff)[0]?.params).toMatchObject({ path: 'a.ts', staged: false })
    expect(getGitState().selected).toEqual({
      path: 'a.ts',
      staged: false,
      kind: 'changed',
      letter: 'M'
    })
  })

  it('未跟踪文件不发 diff，也不把空 diff 当成「没改动」', async () => {
    await openWorkspace(ROOT)
    await refreshGit()

    await selectGitEntry(entry({ path: 'new.ts', worktree: '?', kind: 'untracked' }), false)

    expect(callsOf(Methods.git.diff)).toHaveLength(0)
    const state = getGitState()
    expect(state.diff).toBeNull()
    expect(state.diffLoading).toBe(false)
    expect(state.selected?.kind).toBe('untracked')
  })

  it('连点两条：先发的慢结果不许盖住后发的', async () => {
    await openWorkspace(ROOT)
    await refreshGit()

    const slow = h.deferDiff()
    const first = selectGitEntry(entry({ path: 'a.ts', worktree: 'M', kind: 'changed' }), false)
    const second = selectGitEntry(entry({ path: 'b.ts', worktree: 'M', kind: 'changed' }), false)

    await second
    slow.release(diffOf('a.ts', false, '@@ -1,1 +1,1 @@\n-old\n+new\n'))
    await first

    // 选中的是 b.ts，慢回来的 a.ts diff 必须被丢掉
    expect(getGitState().selected?.path).toBe('b.ts')
    expect(getGitState().diff?.path).toBe('b.ts')
  })

  it('diff 失败时保留选中项、结束 loading 并报错，不静默', async () => {
    await openWorkspace(ROOT)
    await refreshGit()
    h.failDiff('git.diff 失败 [7003]: exit 128')

    const ok = await selectGitEntry(entry({ path: 'a.ts', worktree: 'M', kind: 'changed' }), false)

    expect(ok).toBe(false)
    const state = getGitState()
    expect(state.selected?.path).toBe('a.ts')
    expect(state.diff).toBeNull()
    expect(state.diffLoading).toBe(false)
    expect(state.error).toContain('git.diff 失败')
  })

  it('刷新后条目已不在原组：选中与 diff 一起清掉', async () => {
    await openWorkspace(ROOT)
    h.setEntries([entry({ path: 'a.ts', worktree: 'M', kind: 'changed' })])
    await refreshGit()
    await selectGitEntry(entry({ path: 'a.ts', worktree: 'M', kind: 'changed' }), false)

    h.setEntries([])
    await refreshGit()

    expect(getGitState().selected).toBeNull()
    expect(getGitState().diff).toBeNull()
  })

  it('clearGitSelection 清空选中', async () => {
    await openWorkspace(ROOT)
    await refreshGit()
    await selectGitEntry(entry({ path: 'a.ts', worktree: 'M', kind: 'changed' }), false)

    clearGitSelection()

    expect(getGitState().selected).toBeNull()
    expect(getGitState().diff).toBeNull()
  })
})

describe('暂存 / 取消暂存', () => {
  it('暂存单个文件：paths 一条、all false、unstage false，随后重拉状态', async () => {
    await openWorkspace(ROOT)
    await refreshGit()

    const ok = await stageEntry('a.ts')

    expect(ok).toBe(true)
    expect(callsOf(Methods.git.stage)[0]?.params).toEqual({
      workspace_id: 'ws-1',
      paths: ['a.ts'],
      all: false,
      unstage: false
    })
    expect(getGitState().hint).toContain('已暂存')
    // 一次 refreshGit 里只有一条 status；这里多出来的是动作后的重拉
    expect(callsOf(Methods.git.status)).toHaveLength(2)
  })

  it('取消暂存：unstage true，提示文案跟着变', async () => {
    await openWorkspace(ROOT)
    await refreshGit()

    await unstageEntry('a.ts')

    expect(callsOf(Methods.git.stage)[0]?.params).toMatchObject({ paths: ['a.ts'], unstage: true })
    expect(getGitState().hint).toContain('已取消暂存')
  })

  it('全部暂存 / 全部取消：paths 为空、all true', async () => {
    await openWorkspace(ROOT)
    await refreshGit()

    await stageAll()
    await unstageAll()

    expect(callsOf(Methods.git.stage).map((call) => call.params)).toEqual([
      { workspace_id: 'ws-1', paths: [], all: true, unstage: false },
      { workspace_id: 'ws-1', paths: [], all: true, unstage: true }
    ])
  })
})

describe('commitStaged', () => {
  it('空提交信息在本地就拦掉，不发请求', async () => {
    await openWorkspace(ROOT)
    h.setEntries([entry({ path: 'a.ts', index: 'M', kind: 'changed' })])
    await refreshGit()
    setCommitMessage('   ')

    const ok = await commitStaged()

    expect(ok).toBe(false)
    expect(callsOf(Methods.git.commit)).toHaveLength(0)
    expect(getGitState().hint).toContain('提交信息不能为空')
  })

  it('暂存区为空且未勾 allowEmpty：本地拦掉，不发请求', async () => {
    await openWorkspace(ROOT)
    h.setEntries([entry({ path: 'a.ts', worktree: 'M', kind: 'changed' })])
    await refreshGit()
    setCommitMessage('feat: x')

    const ok = await commitStaged()

    expect(ok).toBe(false)
    expect(callsOf(Methods.git.commit)).toHaveLength(0)
    expect(getGitState().hint).toContain('暂存区为空')
  })

  it('提交成功：先清空草稿、记下回执，再重拉状态', async () => {
    await openWorkspace(ROOT)
    h.setEntries([entry({ path: 'a.ts', index: 'M', kind: 'changed' })])
    await refreshGit()
    setCommitMessage('  feat: 提交  ')

    const ok = await commitStaged()

    expect(ok).toBe(true)
    expect(callsOf(Methods.git.commit)[0]?.params).toEqual({
      workspace_id: 'ws-1',
      message: 'feat: 提交',
      amend: false,
      allow_empty: false
    })
    const state = getGitState()
    expect(state.message).toBe('')
    expect(state.lastCommit?.short_oid).toBe('ccccccc')
    expect(state.hint).toContain('ccccccc')
  })

  it('暂存区为空但勾了 allowEmpty：放行，参数带 allow_empty', async () => {
    await openWorkspace(ROOT)
    await refreshGit()
    setCommitMessage('chore: 空提交')

    const ok = await commitStaged({ allowEmpty: true })

    expect(ok).toBe(true)
    expect(callsOf(Methods.git.commit)[0]?.params).toMatchObject({ allow_empty: true })
  })

  it('amend 时即使暂存区为空也放行（只改提交信息是合法用法）', async () => {
    await openWorkspace(ROOT)
    await refreshGit()
    setCommitMessage('fix: 改一下上次的说明')

    const ok = await commitStaged({ amend: true })

    expect(ok).toBe(true)
    expect(callsOf(Methods.git.commit)[0]?.params).toMatchObject({ amend: true })
  })

  it('内核报 7005 时保留草稿并给出提示（用户不必重打一遍信息）', async () => {
    await openWorkspace(ROOT)
    h.setEntries([entry({ path: 'a.ts', index: 'M', kind: 'changed' })])
    await refreshGit()
    setCommitMessage('feat: 提交')

    const request = vi.spyOn(window.aiide, 'request').mockRejectedValueOnce(
      new Error('git.commit 失败 [7005]: nothing to commit')
    )
    const ok = await commitStaged()

    expect(ok).toBe(false)
    expect(getGitState().message).toBe('feat: 提交')
    expect(getGitState().error).toContain('暂存区没有可提交的内容')
    request.mockRestore()
  })
})

describe('initGit（workspace.changed 去抖刷新）', () => {
  it('别的工区的事件不触发刷新', async () => {
    await openWorkspace(ROOT)
    await refreshGit()
    const before = callsOf(Methods.git.status).length
    initGit()
    expect(h.hasWorkspaceListener()).toBe(true)

    h.pushWorkspaceChanged('ws-other')
    await new Promise((resolve) => setTimeout(resolve, GIT_REFRESH_DELAY_MS + 50))

    expect(callsOf(Methods.git.status)).toHaveLength(before)
  })

  it('本工区的事件去抖后只刷新一次', async () => {
    await openWorkspace(ROOT)
    await refreshGit()
    const before = callsOf(Methods.git.status).length
    initGit()

    vi.useFakeTimers()
    h.pushWorkspaceChanged('ws-1')
    h.pushWorkspaceChanged('ws-1')
    h.pushWorkspaceChanged('ws-1')
    await vi.advanceTimersByTimeAsync(GIT_REFRESH_DELAY_MS + 10)

    expect(callsOf(Methods.git.status)).toHaveLength(before + 1)
  })

  it('disposeGitEvents 之后不再刷新，且待触发的去抖被取消', async () => {
    await openWorkspace(ROOT)
    await refreshGit()
    const before = callsOf(Methods.git.status).length
    initGit()

    vi.useFakeTimers()
    h.pushWorkspaceChanged('ws-1')
    disposeGitEvents()
    await vi.advanceTimersByTimeAsync(GIT_REFRESH_DELAY_MS + 10)

    expect(callsOf(Methods.git.status)).toHaveLength(before)
    expect(h.hasWorkspaceListener()).toBe(false)
  })
})

describe('单 hunk 暂存', () => {
  const HUNK: GitDiffHunk = {
    index: 0,
    path: 'a.ts',
    oldStart: 10,
    oldLines: 2,
    newStart: 10,
    newLines: 3,
    header: '',
    from: 1,
    to: 4
  }

  async function selectChanged(): Promise<void> {
    await openWorkspace(ROOT)
    h.setEntries([entry({ path: 'a.ts', worktree: 'M', kind: 'changed' })])
    await refreshGit()
    await selectGitEntry(entry({ path: 'a.ts', worktree: 'M', kind: 'changed' }), false)
  }

  it('暂存一块：发的是序号 + 四个行号，不是只发序号', async () => {
    await selectChanged()

    const ok = await stageHunk(HUNK)

    expect(ok).toBe(true)
    expect(callsOf(Methods.git.stageHunk)[0]?.params).toEqual({
      workspace_id: 'ws-1',
      path: 'a.ts',
      index: 0,
      old_start: 10,
      old_lines: 2,
      new_start: 10,
      new_lines: 3
    })
    const state = getGitState()
    expect(state.hunkBusy).toBeNull()
    expect(state.hint).toContain('第 1 块')
  })

  it('成功之后重拉状态与 diff（不然按钮落在了已经变过的块上）', async () => {
    await selectChanged()
    const statusBefore = callsOf(Methods.git.status).length
    const diffBefore = callsOf(Methods.git.diff).length

    await stageHunk(HUNK)

    expect(callsOf(Methods.git.status).length).toBe(statusBefore + 1)
    expect(callsOf(Methods.git.diff).length).toBe(diffBefore + 1)
  })

  it('撤出一块走 unstage_hunk，参数同构', async () => {
    await openWorkspace(ROOT)
    h.setEntries([entry({ path: 'a.ts', index: 'M', kind: 'changed' })])
    await refreshGit()
    await selectGitEntry(entry({ path: 'a.ts', index: 'M', kind: 'changed' }), true)

    const ok = await unstageHunk(HUNK)

    expect(ok).toBe(true)
    expect(callsOf(Methods.git.unstageHunk)[0]?.params).toMatchObject({
      path: 'a.ts',
      index: 0,
      old_start: 10,
      new_lines: 3
    })
    expect(getGitState().hunkBusy).toBeNull()
  })

  it('内核报 7011（块已变化）：不留锁、把错误码原样交给 UI', async () => {
    await selectChanged()

    const request = vi
      .spyOn(window.aiide, 'request')
      .mockRejectedValueOnce(new Error('git.stage_hunk 失败 [7011]: hunk 与当前文件不符'))
    const ok = await stageHunk(HUNK)

    expect(ok).toBe(false)
    const state = getGitState()
    expect(state.hunkBusy).toBeNull()
    expect(state.errorCode).toBe(7011)
    request.mockRestore()
  })

  it('没选中文件时本地拦掉，不发请求', async () => {
    await openWorkspace(ROOT)

    const ok = await stageHunk(HUNK)

    expect(ok).toBe(false)
    expect(callsOf(Methods.git.stageHunk)).toHaveLength(0)
    expect(getGitState().hint).toContain('文件')
  })
})

describe('合并冲突', () => {
  const CONFLICT = entry({ path: 'c.ts', index: 'U', worktree: 'U', kind: 'unmerged' })

  async function selectConflict(): Promise<void> {
    await openWorkspace(ROOT)
    h.setEntries([CONFLICT])
    await refreshGit()
    await selectGitEntry(CONFLICT, false)
  }

  it('选中未合并文件时自动读冲突块（用户不用再点一次）', async () => {
    await selectConflict()

    expect(callsOf(Methods.git.conflictRead)).toHaveLength(1)
    const state = getGitState()
    expect(state.conflict?.path).toBe('c.ts')
    expect(state.conflict?.blocks).toHaveLength(1)
    expect(state.conflictLoading).toBe(false)
  })

  it('解决：带上乐观锁，成功后清空面板并留回执', async () => {
    await selectConflict()

    const ok = await resolveConflict({ side: 'ours' })

    expect(ok).toBe(true)
    expect(callsOf(Methods.git.conflictResolve)[0]?.params).toEqual({
      workspace_id: 'ws-1',
      path: 'c.ts',
      side: 'ours',
      theirs_first: false,
      expect_blocks: 1,
      expect_start_line: 3
    })
    const state = getGitState()
    expect(state.conflict).toBeNull()
    expect(state.conflictBusy).toBe(false)
    expect(state.lastConflictResolve?.remaining).toEqual([])
    expect(state.hint).toContain('c.ts')
  })

  it('还有别的冲突文件时自动切到下一个', async () => {
    h.setRemaining(['d.ts'])
    await selectConflict()

    await resolveConflict({ side: 'both', theirsFirst: true })

    expect(callsOf(Methods.git.conflictResolve)[0]?.params).toMatchObject({
      side: 'both',
      theirs_first: true
    })
    expect(callsOf(Methods.git.conflictRead).map((call) => call.params?.path)).toEqual([
      'c.ts',
      'd.ts'
    ])
    expect(getGitState().conflict?.path).toBe('d.ts')
  })

  it('内核报 7014（布局已变）：保留面板让用户重读，而不是硬切', async () => {
    await selectConflict()

    const request = vi
      .spyOn(window.aiide, 'request')
      .mockRejectedValueOnce(new Error('git.conflict_resolve 失败 [7014]: 冲突块已变化'))
    const ok = await resolveConflict({ side: 'ours' })

    expect(ok).toBe(false)
    const state = getGitState()
    expect(state.conflictBusy).toBe(false)
    expect(state.errorCode).toBe(7014)
    expect(state.conflict?.path).toBe('c.ts')
    request.mockRestore()
  })

  it('reloadConflict 用当前冲突路径重读', async () => {
    await selectConflict()

    await reloadConflict()

    expect(callsOf(Methods.git.conflictRead).map((call) => call.params?.path)).toEqual([
      'c.ts',
      'c.ts'
    ])
  })

  it('没有选中冲突文件时本地拦掉', async () => {
    await openWorkspace(ROOT)

    const ok = await resolveConflict({ side: 'ours' })

    expect(ok).toBe(false)
    expect(callsOf(Methods.git.conflictResolve)).toHaveLength(0)
    expect(getGitState().hint).toContain('冲突')
  })

  it('清掉选中后冲突面板一起收口', async () => {
    await selectConflict()

    clearGitSelection()

    expect(getGitState().conflict).toBeNull()
  })
})
