/**
 * 缺口第三批：Git 面板外壳接线（分支 / fetch·pull·push / stash）的回归测试。
 *
 * 与 `git-store.test.ts` 同一套路：不渲染 React，用假的 `window.aiide` 顶替 IPC，
 * 直接驱动动作函数，断言「发了哪个方法、带的参数长什么样、失败后界面落什么文案」。
 *
 * 这一批最容易「看着有按钮、其实没接上」，所以钉住这几条：
 *
 * - **方法名与参数形状**：`git.checkout` 的 `create` / `start_point`、`git.push` 的
 *   `set_upstream` / `force_with_lease`——传错字段不会报错，只会安静地做出另一件事；
 * - **不可逆动作必须过确认**：`stash_drop` 与强推在「只挂起、还没确认」时**一条请求都不许发**，
 *   否则弹窗就成了装饰；
 * - **失败码各自说人话**：7006 要能列出被拦下的文件、7007/7008/7009 要点名下一步动作、
 *   7009 要能一键带 `-u` 修复；
 * - **7010 时界面一条不丢**：stash 冲突后不回拉列表，靠内核「条目仍在」的事实让列表保持原样，
 *   重拉会把「UI 自己丢了条目」这类 bug 掩盖掉。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  Methods,
  type GitBranchesResult,
  type GitCheckoutResult,
  type GitFetchResult,
  type GitPullResult,
  type GitPushResult,
  type GitStashApplyResult,
  type GitStashDropResult,
  type GitStashEntry,
  type GitStashListResult,
  type GitStashResult,
  type GitStatusEntry,
  type GitStatusResult,
  type WorkspaceChangedEvent
} from '@ai-ide/shared-types'

import type { AiIdeApi } from '../../shared/ipc'
import { RPC_ERROR_PAYLOAD_MARKER, encodeRpcErrorPayload } from '../../shared/ipc'
import { describeGitError, describeGitFailure, gitHintForError } from './git-api'
import {
  applyStash,
  cancelGitAction,
  checkoutBranch,
  confirmGitAction,
  createBranch,
  fetchRemote,
  getGitState,
  popStash,
  pullRemote,
  pushRemote,
  pushWithUpstream,
  refreshGit,
  refreshStashes,
  requestDropStash,
  requestForcePush,
  resetGitState,
  saveStash
} from './git-store'
import { closeTab, closeWorkspace, getEditorState, openWorkspace } from './store'

const ROOT = 'C:\\ws'

interface Call {
  method: string
  params: Record<string, unknown> | undefined
}

interface Harness {
  calls: Call[]
  /** 让 `git.stash_list` 返回这份列表。 */
  setStashes: (entries: GitStashEntry[]) => void
  /** 模拟「工作区本来就干净」：`git.stash` 回 `stashed: false`。 */
  setStashClean: (clean: boolean) => void
  /** 让某个方法的下一次调用抛这个错（只抛一次）。 */
  failOnce: (method: string, error: Error) => void
}

/** 按真实形态构造内核错误：主进程把 `data` 编在 message 尾巴上。 */
function rpcError(code: number, message: string, data?: unknown): Error {
  return new Error(encodeRpcErrorPayload({ code, message, data }))
}

function stashEntry(index: number, message: string): GitStashEntry {
  return {
    index,
    oid: String(index).repeat(40),
    short_oid: String(index).repeat(7),
    message
  }
}

/** stash 序号在真实 git 里是连续的：删掉中间一条，后面的要往前顶。 */
function reindex(entries: GitStashEntry[]): GitStashEntry[] {
  return entries.map((entry, index) => ({ ...entry, index }))
}

function statusOf(entries: GitStatusEntry[] = []): GitStatusResult {
  return {
    workspace_id: 'ws-1',
    branch: 'main',
    detached: false,
    head_oid: 'a'.repeat(40),
    upstream: 'origin/main',
    ahead: 1,
    behind: 0,
    clean: entries.length === 0,
    entries
  }
}

function branchesOf(): GitBranchesResult {
  return {
    workspace_id: 'ws-1',
    current: 'main',
    detached: false,
    branches: [
      {
        name: 'dev',
        oid: 'b'.repeat(40),
        short_oid: 'bbbbbbb',
        is_current: false,
        is_remote: false,
        upstream: null
      },
      {
        name: 'main',
        oid: 'a'.repeat(40),
        short_oid: 'aaaaaaa',
        is_current: true,
        is_remote: false,
        upstream: 'origin/main'
      },
      {
        name: 'origin/main',
        oid: 'a'.repeat(40),
        short_oid: 'aaaaaaa',
        is_current: false,
        is_remote: true,
        upstream: null
      }
    ]
  }
}

function installHarness(): Harness {
  const calls: Call[] = []
  let stashes: GitStashEntry[] = []
  let stashIsClean = false
  const failures = new Map<string, Error[]>()

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

      const queued = failures.get(method)
      if (queued !== undefined && queued.length > 0) {
        const next = queued.shift()
        if (next !== undefined) throw next
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
        case Methods.git.status:
          return statusOf()
        case Methods.git.branches:
          return branchesOf()
        case Methods.git.stashList: {
          const result: GitStashListResult = { workspace_id: 'ws-1', entries: stashes }
          return result
        }
        case Methods.git.checkout: {
          const create = typed?.create === true
          const result: GitCheckoutResult = {
            workspace_id: 'ws-1',
            branch: String(typed?.branch ?? ''),
            previous: 'main',
            created: create,
            oid: 'd'.repeat(40),
            short_oid: 'ddddddd'
          }
          return result
        }
        case Methods.git.fetch: {
          const result: GitFetchResult = {
            workspace_id: 'ws-1',
            remote: String(typed?.remote ?? 'origin'),
            updated: [
              { ref_name: 'origin/main', old_short: '1111111', new_short: '2222222', forced: false },
              { ref_name: 'refs/tags/v1', old_short: null, new_short: '3333333', forced: false }
            ],
            up_to_date: false
          }
          return result
        }
        case Methods.git.pull: {
          const result: GitPullResult = {
            workspace_id: 'ws-1',
            branch: 'main',
            upstream: 'origin/main',
            previous_oid: '1'.repeat(40),
            oid: '2'.repeat(40),
            short_oid: '2222222',
            updated: true,
            commits: 2,
            files: 3,
            changed_paths: ['src/a.ts', 'src/b.ts', 'src/c.ts']
          }
          return result
        }
        case Methods.git.push: {
          const result: GitPushResult = {
            workspace_id: 'ws-1',
            branch: String(typed?.branch ?? 'main'),
            remote: String(typed?.remote ?? 'origin'),
            remote_branch: String(typed?.branch ?? 'main'),
            forced: typed?.force_with_lease === true,
            up_to_date: false,
            oid: 'e'.repeat(40),
            short_oid: 'eeeeeee'
          }
          return result
        }
        case Methods.git.stash: {
          if (stashIsClean) {
            const empty: GitStashResult = {
              workspace_id: 'ws-1',
              stashed: false,
              entry: null,
              total: stashes.length
            }
            return empty
          }
          const created = stashEntry(0, String(typed?.message ?? 'WIP on main'))
          stashes = reindex([created, ...stashes])
          const result: GitStashResult = {
            workspace_id: 'ws-1',
            stashed: true,
            entry: created,
            total: stashes.length
          }
          return result
        }
        case Methods.git.stashPop:
        case Methods.git.stashApply: {
          const action = method === Methods.git.stashPop ? 'pop' : 'apply'
          const index = Number(typed?.index ?? 0)
          if (action === 'pop') stashes = reindex(stashes.filter((item) => item.index !== index))
          const result: GitStashApplyResult = {
            workspace_id: 'ws-1',
            action,
            index,
            dropped: action === 'pop',
            total: stashes.length
          }
          return result
        }
        case Methods.git.stashDrop: {
          const index = Number(typed?.index ?? 0)
          const target = stashes.find((item) => item.index === index) ?? stashEntry(index, 'WIP')
          stashes = reindex(stashes.filter((item) => item.index !== index))
          const result: GitStashDropResult = {
            workspace_id: 'ws-1',
            index,
            oid: target.oid,
            message: target.message,
            total: stashes.length
          }
          return result
        }
        default:
          return { ok: true }
      }
    },
    onCoreStatus: () => () => undefined,
    onCoreLog: () => () => undefined,
    onWorkspaceChanged: (_listener: (event: WorkspaceChangedEvent) => void) => () => undefined,
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
    setStashes: (next) => {
      stashes = next
    },
    setStashClean: (clean) => {
      stashIsClean = clean
    },
    failOnce: (method, error) => {
      const list = failures.get(method) ?? []
      list.push(error)
      failures.set(method, list)
    }
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
  resetGitState()
  h = installHarness()
})

/** 打开工作区并先拉一轮，让「动作后重拉」的计数有干净起点。 */
async function ready(): Promise<void> {
  await openWorkspace(ROOT)
  await refreshGit()
}

describe('分支：切换 / 新建', () => {
  it('切换已有分支：发 git.checkout 且 create 为 false，成功后整份重拉清单', async () => {
    await ready()
    const before = callsOf(Methods.git.status).length

    const ok = await checkoutBranch('dev')

    expect(ok).toBe(true)
    const checkout = callsOf(Methods.git.checkout)
    expect(checkout).toHaveLength(1)
    expect(checkout[0]?.params).toEqual({ workspace_id: 'ws-1', branch: 'dev', create: false })
    expect(getGitState().hint).toContain('已从 main 切换到 dev')
    // 换分支意味着清单与暂存区全变，必须重拉（不是只改个文案）。
    expect(callsOf(Methods.git.status).length).toBeGreaterThan(before)
    expect(getGitState().busy).toBe(false)
  })

  it('脏工作区被 7006 拒绝：把被拦下的文件逐条摊开、不偷偷 stash、不静默失败', async () => {
    await ready()
    h.failOnce(
      Methods.git.checkout,
      rpcError(7006, '工作区有未提交改动，拒绝切换分支', {
        files: ['src/a.ts', 'src/app/b.ts']
      })
    )

    const ok = await checkoutBranch('dev')

    expect(ok).toBe(false)
    const state = getGitState()
    expect(state.errorCode).toBe(7006)
    // 面板据这两条逐行渲染成 <li data-testid="git-error-file">。
    expect(state.errorFiles).toEqual(['src/a.ts', 'src/app/b.ts'])
    expect(state.error).toContain('未提交改动')
    expect(state.error).toContain('stash')
    // 用户必须看得到原文，但看不到 IPC 信封里的 JSON。
    expect(state.error).not.toContain(RPC_ERROR_PAYLOAD_MARKER)
    expect(state.busy).toBe(false)
    // 关键：不能替他 stash 再重试一次——那会把改动藏进他没听说过的条目里。
    expect(callsOf(Methods.git.checkout)).toHaveLength(1)
    expect(callsOf(Methods.git.stash)).toHaveLength(0)
  })

  it('从远端分支检出：create 为 true 且 start_point 指向远端引用', async () => {
    await ready()

    const ok = await createBranch('feat/x', 'origin/feat/x')

    expect(ok).toBe(true)
    const checkout = callsOf(Methods.git.checkout)
    expect(checkout).toHaveLength(1)
    expect(checkout[0]?.params).toEqual({
      workspace_id: 'ws-1',
      branch: 'feat/x',
      create: true,
      start_point: 'origin/feat/x'
    })
    expect(getGitState().hint).toContain('已新建并切换到 feat/x')
  })

  it('空名 / 带空格在本地就拦掉，省一次必然失败的往返', async () => {
    await ready()

    expect(await checkoutBranch('   ')).toBe(false)
    expect(getGitState().hint).toBe('分支名不能为空。')

    expect(await createBranch('a b')).toBe(false)
    expect(getGitState().hint).toBe('分支名不能包含空格。')

    expect(callsOf(Methods.git.checkout)).toHaveLength(0)
  })
})

describe('远端：fetch / pull / push', () => {
  it('fetch 成功回显更新了几个引用', async () => {
    await ready()

    const ok = await fetchRemote()

    expect(ok).toBe(true)
    const fetch = callsOf(Methods.git.fetch)
    expect(fetch).toHaveLength(1)
    expect(fetch[0]?.params).toMatchObject({ workspace_id: 'ws-1', prune: false })
    expect(getGitState().hint).toContain('更新 2 个引用')
  })

  it('pull 撞 7007 非快进：原文保留，并指名「先 pull／强推要二次确认」', async () => {
    await ready()
    h.failOnce(
      Methods.git.pull,
      rpcError(7007, '拉取被拒绝', { detail: '! [rejected] main -> main (non-fast-forward)' })
    )

    const ok = await pullRemote()

    expect(ok).toBe(false)
    const state = getGitState()
    expect(state.errorCode).toBe(7007)
    // 人话在 error（message｜hint），git 原文在 errorDetail——两者都不能丢。
    expect(state.error).toContain('非快进')
    expect(state.error).toContain('先 pull')
    expect(state.error).toContain('强制推送')
    expect(state.errorDetail).toBe('! [rejected] main -> main (non-fast-forward)')
    expect(state.errorFiles).toEqual([])
    expect(state.errorBranch).toBeNull()
  })

  it('push 撞 7008 认证失败：提示查凭据，明说重试无用', async () => {
    await ready()
    h.failOnce(
      Methods.git.push,
      rpcError(7008, '推送失败', {
        detail: 'fatal: Authentication failed for https://example/repo.git'
      })
    )

    const ok = await pushRemote()

    expect(ok).toBe(false)
    const state = getGitState()
    expect(state.errorCode).toBe(7008)
    expect(state.error).toContain('认证失败')
    expect(state.error).toContain('凭据')
    expect(state.errorDetail).toBe('fatal: Authentication failed for https://example/repo.git')
    // 7009 的修复按钮不该在这种错误上冒出来（会推一条没有 upstream 的分支）。
    expect(state.errorBranch).toBeNull()
  })

  it('push 撞 7009 无上游：曝出分支名并给一键 -u 修复，修好后错误清空', async () => {
    await ready()
    h.failOnce(Methods.git.push, rpcError(7009, '当前分支没有上游分支', { branch: 'feat/x' }))

    expect(await pushRemote({ branch: 'feat/x' })).toBe(false)
    const failed = getGitState()
    expect(failed.errorCode).toBe(7009)
    expect(failed.errorBranch).toBe('feat/x')
    expect(failed.error).toContain('push -u origin')

    // 面板上的「以 feat/x 为上游推送」按钮直接拿报错里的分支名再推一次。
    expect(await pushWithUpstream(failed.errorBranch ?? '')).toBe(true)
    const pushes = callsOf(Methods.git.push)
    expect(pushes).toHaveLength(2)
    expect(pushes[1]?.params).toMatchObject({
      workspace_id: 'ws-1',
      branch: 'feat/x',
      set_upstream: true,
      force_with_lease: false
    })
    expect(getGitState().error).toBeNull()
    expect(getGitState().errorBranch).toBeNull()
    expect(getGitState().lastPush?.remote_branch).toBe('feat/x')
  })

  it('强制推送必须先二次确认：没确认一条请求都不发，确认后才带 force_with_lease', async () => {
    await ready()

    requestForcePush({ branch: 'main', remote: 'origin' })
    const pending = getGitState().confirm
    expect(pending?.kind).toBe('force_push')
    expect(pending?.warning).toContain('不可逆')
    expect(pending?.detail).toContain('--force-with-lease')
    expect(callsOf(Methods.git.push)).toHaveLength(0)

    // 取消：弹窗关掉，仍然什么都没发。
    cancelGitAction()
    expect(getGitState().confirm).toBeNull()
    expect(callsOf(Methods.git.push)).toHaveLength(0)

    requestForcePush({ branch: 'main', remote: 'origin' })
    expect(await confirmGitAction()).toBe(true)

    const pushes = callsOf(Methods.git.push)
    expect(pushes).toHaveLength(1)
    expect(pushes[0]?.params).toMatchObject({
      workspace_id: 'ws-1',
      branch: 'main',
      remote: 'origin',
      set_upstream: false,
      force_with_lease: true
    })
    expect(getGitState().lastPush?.forced).toBe(true)
    expect(getGitState().hint).toContain('--force-with-lease')
  })
})

describe('stash：保存 / 列表 / pop·apply / drop', () => {
  it('保存带 message：请求带 message 与 include_untracked，成功后回显并重拉列表', async () => {
    await ready()

    const ok = await saveStash({ message: 'WIP 临时改动' })

    expect(ok).toBe(true)
    const stash = callsOf(Methods.git.stash)
    expect(stash).toHaveLength(1)
    expect(stash[0]?.params).toEqual({
      workspace_id: 'ws-1',
      message: 'WIP 临时改动',
      include_untracked: false
    })
    expect(getGitState().hint).toBe('已保存 stash@{0}：WIP 临时改动（共 1 条）')
    expect(getGitState().stashes.map((item) => item.message)).toEqual(['WIP 临时改动'])
  })

  it('工作区本来干净：stashed 为 false 时明说没生成条目，不谎报成功', async () => {
    await ready()
    h.setStashClean(true)

    const ok = await saveStash({ message: '不该出现' })

    expect(ok).toBe(false)
    expect(getGitState().hint).toContain('没有可保存的改动')
    expect(getGitState().stashes).toHaveLength(0)
  })

  it('pop 成功：条目按内核结果消失（dropped 为 true）', async () => {
    await ready()
    h.setStashes([stashEntry(0, 'A'), stashEntry(1, 'B')])
    await refreshStashes()
    expect(getGitState().stashes).toHaveLength(2)

    const ok = await popStash(0)

    expect(ok).toBe(true)
    expect(getGitState().hint).toBe('已 pop stash@{0} 并删除该条（剩 1 条）')
    expect(getGitState().stashes.map((item) => item.message)).toEqual(['B'])
  })

  it('apply 成功：条目保留（dropped 为 false）', async () => {
    await ready()
    h.setStashes([stashEntry(0, 'A'), stashEntry(1, 'B')])
    await refreshStashes()

    const ok = await applyStash(0)

    expect(ok).toBe(true)
    expect(getGitState().hint).toBe('已 apply stash@{0}，条目保留（共 2 条）')
    expect(getGitState().stashes).toHaveLength(2)
    expect(callsOf(Methods.git.stashApply)).toHaveLength(1)
    expect(callsOf(Methods.git.stashPop)).toHaveLength(0)
  })

  it('pop 撞 7010 冲突：条目一条不丢、不回拉列表、错误里给冲突文件', async () => {
    await ready()
    h.setStashes([stashEntry(0, 'A'), stashEntry(1, 'B')])
    await refreshStashes()
    const listCalls = callsOf(Methods.git.stashList).length

    h.failOnce(
      Methods.git.stashPop,
      rpcError(7010, 'stash 取出时冲突', { files: ['src/a.ts', 'src/b.ts'], stash_kept: true })
    )

    const ok = await popStash(0)

    expect(ok).toBe(false)
    const state = getGitState()
    expect(state.errorCode).toBe(7010)
    expect(state.errorFiles).toEqual(['src/a.ts', 'src/b.ts'])
    expect(state.stashKept).toBe(true)
    expect(state.error).toContain('仍在')
    // 界面上的条目必须还在（面板据此提示「条目已保留」）。
    expect(state.stashes.map((item) => item.message)).toEqual(['A', 'B'])
    expect(state.hint).toContain('未被删除')
    // 冲突时不刷新 stash 列表：刷新会掩盖「UI 自己把条目丢了」这类 bug。
    expect(callsOf(Methods.git.stashList).length).toBe(listCalls)
  })

  it('drop 必须二次确认：取消不发请求，确认才删并回显被删条目（可事后核对）', async () => {
    await ready()
    h.setStashes([stashEntry(0, 'A'), stashEntry(1, 'B')])
    await refreshStashes()

    requestDropStash(0)
    const pending = getGitState().confirm
    expect(pending?.kind).toBe('stash_drop')
    expect(pending?.stashIndex).toBe(0)
    expect(pending?.warning).toContain('不可逆')
    expect(pending?.detail).toContain('A')
    expect(callsOf(Methods.git.stashDrop)).toHaveLength(0)

    cancelGitAction()
    expect(getGitState().confirm).toBeNull()
    expect(callsOf(Methods.git.stashDrop)).toHaveLength(0)

    requestDropStash(0)
    expect(await confirmGitAction()).toBe(true)

    const drops = callsOf(Methods.git.stashDrop)
    expect(drops).toHaveLength(1)
    expect(drops[0]?.params).toEqual({ workspace_id: 'ws-1', index: 0 })
    // 删掉的条目已不在列表里，oid / message 是用户唯一的「到底删了哪一条」凭据。
    expect(getGitState().lastStashDrop).toMatchObject({
      index: 0,
      message: 'A',
      oid: '0'.repeat(40)
    })
    // 逐字断言而不是 toContain('stash@')：被删的是哪一条要能核对到索引、message 和
    // oid 前七位，三者缺一就成了「删过东西但说不清删了啥」。
    expect(getGitState().hint).toBe('已删除 stash@{0}（A，0000000），剩 1 条')
    expect(getGitState().stashes.map((item) => item.message)).toEqual(['B'])
  })
})

describe('错误报文解析与文案（真实信封形态）', () => {
  it('信封里的 files / branch / stash_kept 都能取出来，且不外泄 JSON', () => {
    const failure = describeGitFailure(
      rpcError(7010, 'stash 取出时冲突', {
        files: ['src/a.ts', 42, null],
        stash_kept: true
      })
    )

    expect(failure.code).toBe(7010)
    expect(failure.message).toBe('stash 取出时冲突')
    // 非字符串项被丢掉，而不是把 42 渲染成文件名。
    expect(failure.files).toEqual(['src/a.ts'])
    expect(failure.stashKept).toBe(true)

    const text = describeGitError(
      rpcError(7010, 'stash 取出时冲突', { files: ['src/a.ts'], stash_kept: true })
    )
    expect(text).toContain('stash 取出时冲突')
    expect(text).toContain('仍在')
    expect(text).not.toContain(RPC_ERROR_PAYLOAD_MARKER)
    expect(text).not.toContain('"files"')
  })

  it('7009 从 data.branch 取分支名；没有该字段时为 null', () => {
    expect(describeGitFailure(rpcError(7009, '没有上游', { branch: 'feat/x' })).branch).toBe('feat/x')
    expect(describeGitFailure(rpcError(7009, '没有上游')).branch).toBeNull()
  })

  it('无信封但文本里带 [7002] 也能取码，老路径不至于退化成「未知错误」', () => {
    const failure = describeGitFailure(new Error('git.status 失败 [7002]: not a git repository'))
    expect(failure.code).toBe(7002)
    expect(failure.hint).toContain('git init')
  })

  it('五个新码各自说人话，且都指向下一步动作', () => {
    expect(gitHintForError(7006)).toContain('stash')
    expect(gitHintForError(7007)).toContain('先 pull')
    expect(gitHintForError(7008)).toContain('凭据')
    expect(gitHintForError(7009)).toContain('push -u origin')
    expect(gitHintForError(7010)).toContain('仍在')
    // 未知码回落空串，由调用方保留原文。
    expect(gitHintForError(9999)).toBe('')
  })
})
