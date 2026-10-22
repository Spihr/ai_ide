/**
 * M6 渲染侧状态：Git 面板（变更清单 / diff / 暂存 / 提交 / 分支）。
 *
 * 与 `checkpoint-store` 同一套路（模块级单例 + `useSyncExternalStore`，动作返回布尔、
 * 失败原因落 `error` / `hint`）。这里额外钉住三件容易「看起来能用、其实不对」的事：
 *
 * 1. **分组是视图层的职责**：内核只回一张按路径升序的清单，`index` / `worktree` 两列状态
 *    字母才是真相。一个文件可以同时出现在「已暂存」和「未暂存」（改了 → 暂存 → 又改），
 *    所以分组**不是二选一**，两个组各自独立判定。合成一个 `isStaged` 就再也分不开了。
 * 2. **diff 的基准跟着分组走**：点「已暂存」里的条目要看 `git diff --cached`（暂存区相对
 *    HEAD），点「未暂存」里的要看工作区相对暂存区。两条基准混了，用户会对着看到的
 *    增删去点提交，而提交进去的是另一份内容。
 * 3. **未跟踪文件没有基准版本**：`git diff` 两侧都不会出现它（协议 §2.8 明确），所以这里
 *    直接不发请求，由面板提示「新文件，暂存后才能看 diff」，避免一次空 diff 被误读成「没改动」。
 */

import { useSyncExternalStore } from 'react'

import type { GitBranch, GitCommitResult, GitDiffResult, GitStatusEntry, GitStatusResult } from '@ai-ide/shared-types'

import { describeGitError, gitApi } from './git-api'
import { getEditorState } from './store'

/** `workspace.changed` 之后延迟多久重拉状态。watcher 一次保存会推多条，去抖省进程。 */
export const GIT_REFRESH_DELAY_MS = 400

/** 当前选中的 diff 目标；`staged` 是基准（true = 暂存区相对 HEAD）。 */
export interface GitSelection {
  path: string
  staged: boolean
  /** 条目类别，用来识别「未跟踪 = 没有基准版本」。 */
  kind: string
  /** 该侧的状态字母（已暂存组取 `index`，未暂存组取 `worktree`）。 */
  letter: string
}

export interface GitState {
  /** 状态所属工作区；换工作区时选中项与 diff 一并作废。 */
  workspaceId: string | null
  status: GitStatusResult | null
  branches: GitBranch[]
  /** `git.status` / `git.branches` 加载中。 */
  loading: boolean
  /** 暂存 / 提交进行中：UI 据此禁用按钮。 */
  busy: boolean
  error: string | null
  hint: string | null
  selected: GitSelection | null
  diff: GitDiffResult | null
  diffLoading: boolean
  /** 提交信息草稿（面板输入框直接绑这里，切视图不丢）。 */
  message: string
  /** 上一次提交的回执。 */
  lastCommit: GitCommitResult | null
}

function initialState(): GitState {
  return {
    workspaceId: null,
    status: null,
    branches: [],
    loading: false,
    busy: false,
    error: null,
    hint: null,
    selected: null,
    diff: null,
    diffLoading: false,
    message: '',
    lastCommit: null
  }
}

let state: GitState = initialState()
const listeners = new Set<() => void>()

function setState(patch: Partial<GitState>): void {
  state = { ...state, ...patch }
  for (const listener of listeners) listener()
}

export function getGitState(): GitState {
  return state
}

export function subscribeGit(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function useGit(): GitState {
  return useSyncExternalStore(subscribeGit, getGitState, getGitState)
}

/** 单测与「关闭工作区」时重置。 */
export function resetGitState(): void {
  state = initialState()
  for (const listener of listeners) listener()
}

function activeWorkspaceId(): string | null {
  return getEditorState().workspace?.workspace_id ?? null
}

const LETTER_LABELS: Record<string, string> = {
  M: '修改',
  A: '新增',
  D: '删除',
  R: '重命名',
  C: '复制',
  T: '类型变更',
  U: '冲突',
  '?': '未跟踪',
  '.': '无变更'
}

/** 状态字母 → 人话（给 `title` / `aria-label` 用；徽标本身仍显示字母）。 */
export function entryStatusLabel(letter: string): string {
  return LETTER_LABELS[letter] ?? letter
}

/**
 * 按暂存状态分组。
 *
 * - `ignored` 不进任何一组（内核已过滤，这里再兜一次：把忽略文件摆上提交界面是误导）；
 * - `untracked` 只进「未暂存」；
 * - 其余按两列字母各自判定，因此「改了又暂存又改」会同时出现在两组里。
 */
export function groupEntries(entries: GitStatusEntry[]): {
  staged: GitStatusEntry[]
  unstaged: GitStatusEntry[]
} {
  const staged: GitStatusEntry[] = []
  const unstaged: GitStatusEntry[] = []

  for (const entry of entries) {
    if (entry.kind === 'ignored') continue
    if (entry.kind === 'untracked') {
      unstaged.push(entry)
      continue
    }
    if (entry.index !== '.') staged.push(entry)
    if (entry.worktree !== '.') unstaged.push(entry)
  }

  return { staged, unstaged }
}

/** 暂存区条目数（提交前置判断与角标都用它）。 */
export function stagedCount(status: GitStatusResult | null): number {
  if (status === null) return 0
  return groupEntries(status.entries).staged.length
}

/** 变更总数（活动栏角标）。 */
export function changeCount(status: GitStatusResult | null): number {
  if (status === null) return 0
  return status.entries.filter((entry) => entry.kind !== 'ignored').length
}

/** 某一组里是否还有这个路径；`refreshGit` 用它决定选中项要不要保留。 */
function stillSelected(entries: GitStatusEntry[], selected: GitSelection): boolean {
  const group = selected.staged ? groupEntries(entries).staged : groupEntries(entries).unstaged
  return group.some((entry) => entry.path === selected.path)
}

// ---------------------------------------------------------------------------
// 读取
// ---------------------------------------------------------------------------

/**
 * 重拉状态 + 分支。两条并行发：它们互不依赖，串行只会让面板多转半圈。
 */
export async function refreshGit(): Promise<boolean> {
  const workspaceId = activeWorkspaceId()
  if (workspaceId === null) {
    state = { ...initialState(), hint: 'Git 面板需要先打开工作区。' }
    for (const listener of listeners) listener()
    return false
  }

  const sameWorkspace = state.workspaceId === workspaceId
  state = { ...state, workspaceId, loading: true, error: null, hint: sameWorkspace ? state.hint : null }
  for (const listener of listeners) listener()

  try {
    const [status, branches] = await Promise.all([
      gitApi.status(workspaceId),
      gitApi.branches(workspaceId)
    ])
    // 慢回来的旧结果不许覆盖新工作区的状态。
    if (state.workspaceId !== workspaceId) return false

    const selected = state.selected
    const keep = selected !== null && stillSelected(status.entries, selected)
    setState({
      loading: false,
      status,
      branches: branches.branches,
      selected: keep ? selected : null,
      diff: keep ? state.diff : null
    })
    return true
  } catch (error) {
    if (state.workspaceId !== workspaceId) return false
    // 不是仓库（7002）会走到这里：清掉旧清单，别让上一个工作区的变更继续留在界面上。
    setState({
      loading: false,
      status: null,
      branches: [],
      selected: null,
      diff: null,
      error: describeGitError(error)
    })
    return false
  }
}

/** diff 请求序号：连点两条变更时，先发的慢结果不许盖住后发的。 */
let diffSeq = 0

/**
 * 选中一条变更并拉它的 diff。
 *
 * 未跟踪文件不发请求（两侧都没有基准版本）：直接给空 diff + 让面板据 `kind` 提示。
 */
export async function selectGitEntry(entry: GitStatusEntry, staged: boolean): Promise<boolean> {
  const workspaceId = activeWorkspaceId()
  const letter = staged ? entry.index : entry.worktree
  setState({
    selected: { path: entry.path, staged, kind: entry.kind, letter },
    diff: null,
    diffLoading: entry.kind !== 'untracked',
    error: null
  })
  if (entry.kind === 'untracked') return true

  diffSeq += 1
  const seq = diffSeq
  try {
    const diff = await gitApi.diff({ workspaceId, path: entry.path, staged, contextLines: 3 })
    if (seq !== diffSeq) return false
    setState({ diff, diffLoading: false })
    return true
  } catch (error) {
    if (seq !== diffSeq) return false
    setState({ diffLoading: false, error: describeGitError(error) })
    return false
  }
}

export function clearGitSelection(): void {
  diffSeq += 1
  setState({ selected: null, diff: null, diffLoading: false })
}

export function setCommitMessage(message: string): void {
  setState({ message })
}

export function dismissGitHint(): void {
  if (state.hint !== null) setState({ hint: null })
}

// ---------------------------------------------------------------------------
// 暂存 / 取消暂存
// ---------------------------------------------------------------------------

async function stage(input: {
  paths?: string[]
  all?: boolean
  unstage: boolean
}): Promise<boolean> {
  const workspaceId = activeWorkspaceId()
  if (workspaceId === null) {
    setState({ hint: 'Git 面板需要先打开工作区。' })
    return false
  }

  setState({ busy: true, error: null, hint: null })
  try {
    const result = await gitApi.stage({
      workspaceId,
      paths: input.paths ?? [],
      all: input.all ?? false,
      unstage: input.unstage
    })
    const what = result.all ? '全部变更' : `${result.paths.length} 个文件`
    setState({
      busy: false,
      hint: input.unstage
        ? `已取消暂存 ${what}（仍在暂存区 ${result.staged_total}）`
        : `已暂存 ${what}（暂存区共 ${result.staged_total}）`
    })
    // 回执里的 `staged_total` 只是数字，清单还得重拉——否则「暂存了哪几个」对不上。
    await refreshGit()
    return true
  } catch (error) {
    setState({ busy: false, error: describeGitError(error) })
    return false
  }
}

/** 暂存单个文件（`git add -- <path>`）。 */
export function stageEntry(path: string): Promise<boolean> {
  return stage({ paths: [path], unstage: false })
}

/** 取消暂存单个文件（`git reset -- <path>`）。 */
export function unstageEntry(path: string): Promise<boolean> {
  return stage({ paths: [path], unstage: true })
}

/** 暂存全部变更（含删除）。 */
export function stageAll(): Promise<boolean> {
  return stage({ all: true, unstage: false })
}

/** 取消暂存全部。 */
export function unstageAll(): Promise<boolean> {
  return stage({ all: true, unstage: true })
}

// ---------------------------------------------------------------------------
// 提交
// ---------------------------------------------------------------------------

export interface CommitOptions {
  /** `--amend`：改写 HEAD（改信息，或把刚暂存的改动并进上一次提交）。 */
  amend?: boolean
  /** `--allow-empty`：显式允许空提交。 */
  allowEmpty?: boolean
}

/**
 * 提交暂存区。
 *
 * 三道前置**在本地就拦掉**，不把可预期的用户状态推给内核：
 *
 * - 提交信息去空白后为空 → 直接提示（内核也会回 `7004`，但没必要往返一次）；
 * - 暂存区为空且既非 `amend` 也没勾 `allowEmpty` → 提示（内核回 `7005`）；
 * - `amend` 时不看暂存区：`git commit --amend` 只改提交信息是合法用法。
 */
export async function commitStaged(options: CommitOptions = {}): Promise<boolean> {
  const workspaceId = activeWorkspaceId()
  if (workspaceId === null) {
    setState({ hint: 'Git 面板需要先打开工作区。' })
    return false
  }

  const message = state.message.trim()
  if (message.length === 0) {
    setState({ hint: '提交信息不能为空。' })
    return false
  }

  const amend = options.amend ?? false
  const allowEmpty = options.allowEmpty ?? false
  if (stagedCount(state.status) === 0 && !amend && !allowEmpty) {
    setState({ hint: '暂存区为空：先暂存要提交的文件，或勾选「允许空提交」。' })
    return false
  }

  setState({ busy: true, error: null, hint: null })
  try {
    const result = await gitApi.commit({ workspaceId, message, amend, allowEmpty })
    setState({
      busy: false,
      message: '',
      lastCommit: result,
      hint: `已提交 ${result.short_oid}（${result.files} 个文件）到 ${result.branch ?? '分离头指针'}`
    })
    await refreshGit()
    return true
  } catch (error) {
    setState({ busy: false, error: describeGitError(error) })
    return false
  }
}

// ---------------------------------------------------------------------------
// 事件
// ---------------------------------------------------------------------------

let offWorkspaceChanged: (() => void) | null = null
let refreshTimer: ReturnType<typeof setTimeout> | null = null

function scheduleRefresh(): void {
  if (refreshTimer !== null) clearTimeout(refreshTimer)
  refreshTimer = setTimeout(() => {
    refreshTimer = null
    void refreshGit()
  }, GIT_REFRESH_DELAY_MS)
}

/**
 * 订阅工作区变更，去抖后重拉状态。
 *
 * 为什么不用 `git.*` 自己的事件：协议 §2.8 已明确 `git.*` 没有事件，Git 状态是工作区内容的
 * 一个视图，`workspace.changed` 就是那个事实来源。这里只在**事件属于当前工作区**时才刷新。
 */
export function initGit(): () => void {
  if (offWorkspaceChanged !== null) return disposeGitEvents

  offWorkspaceChanged = window.aiide.onWorkspaceChanged((event) => {
    // 以编辑器里的活动工作区为准而不是 `state.workspaceId`：面板还没打开过时后者是 null，
    // 那样「刚打开工作区就有外部改动」会被静默丢掉。
    if (event.workspace_id !== activeWorkspaceId()) return
    scheduleRefresh()
  })

  return disposeGitEvents
}

/** 取消订阅并清掉待触发的去抖。 */
export function disposeGitEvents(): void {
  if (refreshTimer !== null) {
    clearTimeout(refreshTimer)
    refreshTimer = null
  }
  offWorkspaceChanged?.()
  offWorkspaceChanged = null
}
