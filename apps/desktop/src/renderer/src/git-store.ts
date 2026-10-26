/**
 * M6 渲染侧状态：Git 面板（变更清单 / diff / 暂存 / 提交 / 分支 / 远端 / stash）。
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
 *
 * 缺口第二批再钉四条：
 *
 * 4. **不可逆动作必须两道门**：`stash_drop` 与「强制推送」先在 `confirm` 里挂起，用户
 *    点确认才真的发请求。确认文案写在 store 而不是组件里，这样「弹窗说了什么」也能被单测。
 * 5. **7010 之后条目不许消失**：stash pop 撞冲突时内核**保留**条目，界面上的列表也必须
 *    保留——否则用户会以为丢掉了一条改动，去重做一遍 stash。
 * 6. **7006 的清单必须露出来**：脏工作区拒绝切分支时，`data.files` 要变成 `errorFiles`
 *    摆给用户看，而不是只说一句「切换失败」。
 * 7. **7009 的修复入口就地在场**：没有上游时 `data.branch` 落进 `errorBranch`，面板据此
 *    给一个「以它作上游推送」的按钮，用户不必自己敲 `git push -u`。
 */

import { useSyncExternalStore } from 'react'

import type {
  GitBranch,
  GitCommitResult,
  GitDiffResult,
  GitPushResult,
  GitStashDropResult,
  GitStashEntry,
  GitStatusEntry,
  GitStatusResult
} from '@ai-ide/shared-types'

import { describeGitError, describeGitFailure, gitApi } from './git-api'
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

/**
 * 一个挂起的不可逆动作：用户点了按钮，但还差一次确认。
 *
 * 文案（`title` / `detail` / `warning`）放在这里而不是组件里，是为了让「弹窗到底说了
 * 什么」可以被单测钉住——「不可逆」这句话丢掉一个字都算事故。
 */
export interface GitConfirm {
  kind: 'stash_drop' | 'force_push'
  title: string
  /** 具体对象（哪一条 stash / 哪个分支推到哪里）。 */
  detail: string
  /** 不可逆说明，逐字显示在弹窗里。 */
  warning: string
  /** `stash_drop` 的目标序号；`force_push` 为 `null`。 */
  stashIndex: number | null
  /**
   * `force_push` 的目标分支 / 远端。
   *
   * 必须存下来：确认时若改推「当前分支」，用户在对某条分支发起强推后切了分支，
   * 就会覆盖掉另一个分支——弹窗里写的和实际做的必须是同一件事。
   */
  targetBranch: string | null
  targetRemote: string | null
}

export interface GitState {
  /** 状态所属工作区；换工作区时选中项与 diff 一并作废。 */
  workspaceId: string | null
  status: GitStatusResult | null
  branches: GitBranch[]
  /** `git.status` / `git.branches` / `git.stash_list` 加载中。 */
  loading: boolean
  /** 暂存 / 提交 / 远端 / stash 动作进行中：UI 据此禁用按钮。 */
  busy: boolean
  error: string | null
  /** 失败的结构化附加信息：7006 / 7010 的冲突文件清单（其它失败为空表）。 */
  errorFiles: string[]
  /** 失败码（`null` 表示文本里没有协议码）。 */
  errorCode: number | null
  /** 7009 缺上游时的分支名：面板据此给「以它作上游推送」的修复按钮。 */
  errorBranch: string | null
  /**
   * git 自己的原始报错（`data.detail`）。
   *
   * 7007 / 7008 的价值全在这一行：`non-fast-forward`、`Authentication failed` 是用户
   * 去搜索、去贴给同事的唯一凭据；只留一句人话等于把可复现的证据删了。
   */
  errorDetail: string | null
  /** 7010 时为真：stash 条目被内核保留，界面不得把它从列表里删掉。 */
  stashKept: boolean
  hint: string | null
  selected: GitSelection | null
  diff: GitDiffResult | null
  diffLoading: boolean
  /** 提交信息草稿（面板输入框直接绑这里，切视图不丢）。 */
  message: string
  /** 上一次提交的回执。 */
  lastCommit: GitCommitResult | null
  /** stash 列表（`index` 0 最新）。 */
  stashes: GitStashEntry[]
  /** stash 区域单独的加载态：保存/取出时不遮住变更清单。 */
  stashLoading: boolean
  /** 上一次 stash 删除的回执（被删条目的 oid / message 是事后唯一证据）。 */
  lastStashDrop: GitStashDropResult | null
  /** 挂起待确认的不可逆动作；`null` 表示没有弹窗。 */
  confirm: GitConfirm | null
  /** 最后一次成功的推送回执（面板据此展示「推到了哪里」）。 */
  lastPush: GitPushResult | null
}

function initialState(): GitState {
  return {
    workspaceId: null,
    status: null,
    branches: [],
    loading: false,
    busy: false,
    error: null,
    errorFiles: [],
    errorCode: null,
    errorBranch: null,
    errorDetail: null,
    stashKept: false,
    hint: null,
    selected: null,
    diff: null,
    diffLoading: false,
    message: '',
    lastCommit: null,
    stashes: [],
    stashLoading: false,
    lastStashDrop: null,
    confirm: null,
    lastPush: null
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
 * 重拉状态 + 分支 + stash 列表。三条并行发：它们互不依赖，串行只会让面板多转半圈。
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
    const [status, branches, stashes] = await Promise.all([
      gitApi.status(workspaceId),
      gitApi.branches(workspaceId),
      gitApi.stashList(workspaceId)
    ])
    // 慢回来的旧结果不许覆盖新工作区的状态。
    if (state.workspaceId !== workspaceId) return false

    const selected = state.selected
    const keep = selected !== null && stillSelected(status.entries, selected)
    setState({
      loading: false,
      status,
      branches: branches.branches,
      stashes: stashes.entries,
      selected: keep ? selected : null,
      diff: keep ? state.diff : null
    })
    return true
  } catch (error) {
    if (state.workspaceId !== workspaceId) return false
    // 不是仓库（7002）会走到这里：清掉旧清单，别让上一个工作区的变更继续留在界面上。
    const failure = describeGitFailure(error)
    setState({
      loading: false,
      status: null,
      branches: [],
      stashes: [],
      selected: null,
      diff: null,
      error: describeGitError(error),
      errorCode: failure.code,
      errorFiles: failure.files,
      errorBranch: failure.branch,
      stashKept: failure.stashKept
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

  startAction()
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
    failWith(error)
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

  startAction()
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
    failWith(error)
    return false
  }
}

// ---------------------------------------------------------------------------
// 失败收口
// ---------------------------------------------------------------------------

/**
 * 统一失败收口：文案 + 结构化附加信息一起落。
 *
 * `errorFiles` / `errorBranch` / `stashKept` 每次动作前都会被清空（`startAction`），
 * 所以这里不需要先置空——读到的就是本次失败自己的数据。
 */
function failWith(error: unknown): void {
  const failure = describeGitFailure(error)
  setState({
    busy: false,
    stashLoading: false,
    error: describeGitError(error),
    errorCode: failure.code,
    errorFiles: failure.files,
    errorBranch: failure.branch,
    errorDetail: failure.detail,
    stashKept: failure.stashKept
  })
}

/** 动作起手：把上一次的失败痕迹清干净（否则 7006 的文件清单会一直挂着）。 */
function startAction(): void {
  setState({
    busy: true,
    error: null,
    errorFiles: [],
    errorCode: null,
    errorBranch: null,
    errorDetail: null,
    stashKept: false,
    hint: null
  })
}

export function dismissGitError(): void {
  if (state.error === null) return
  setState({
    error: null,
    errorFiles: [],
    errorCode: null,
    errorBranch: null,
    errorDetail: null,
    stashKept: false
  })
}

// ---------------------------------------------------------------------------
// 分支：切换 / 新建
// ---------------------------------------------------------------------------

/**
 * 切换（或新建）分支。
 *
 * 脏工作区会被内核以 `7006` 拒绝——这里**不做**任何「自动 stash 再切换」的补偿：
 * 那会让用户以为自己切成功了，而改动被藏进一个他没听说过的 stash 里。
 */
async function switchBranch(input: {
  branch: string
  create: boolean
  startPoint?: string
}): Promise<boolean> {
  const workspaceId = activeWorkspaceId()
  if (workspaceId === null) {
    setState({ hint: 'Git 面板需要先打开工作区。' })
    return false
  }

  const branch = input.branch.trim()
  if (branch.length === 0) {
    setState({ hint: '分支名不能为空。' })
    return false
  }
  // 分支名带空格在 git 里非法，本地先拦掉，省一次往返。
  if (/\s/.test(branch)) {
    setState({ hint: '分支名不能包含空格。' })
    return false
  }

  const startPoint = input.startPoint?.trim() ?? ''
  startAction()
  try {
    const result = await gitApi.checkout({
      workspaceId,
      branch,
      create: input.create,
      startPoint: startPoint.length === 0 ? undefined : startPoint
    })
    const from = result.previous === null ? '分离头指针' : result.previous
    setState({
      busy: false,
      hint: result.created
        ? `已新建并切换到 ${result.branch}（起点 ${startPoint.length === 0 ? 'HEAD' : startPoint}，原 ${from}）`
        : `已从 ${from} 切换到 ${result.branch}（${result.short_oid}）`
    })
    // 换分支意味着整个清单/暂存区都变了，必须整份重拉。
    await refreshGit()
    return true
  } catch (error) {
    failWith(error)
    return false
  }
}

/** 切换到已有分支。 */
export function checkoutBranch(branch: string): Promise<boolean> {
  return switchBranch({ branch, create: false })
}

/** 新建分支并切过去（`start_point` 缺省 = 当前 HEAD）。 */
export function createBranch(name: string, startPoint?: string): Promise<boolean> {
  return switchBranch({ branch: name, create: true, startPoint })
}

// ---------------------------------------------------------------------------
// 远端：fetch / pull / push
// ---------------------------------------------------------------------------

export async function fetchRemote(options: { remote?: string; prune?: boolean } = {}): Promise<boolean> {
  const workspaceId = activeWorkspaceId()
  if (workspaceId === null) {
    setState({ hint: 'Git 面板需要先打开工作区。' })
    return false
  }

  startAction()
  try {
    const result = await gitApi.fetch({
      workspaceId,
      remote: options.remote,
      prune: options.prune ?? false
    })
    setState({
      busy: false,
      hint: result.up_to_date
        ? `已从 ${result.remote} 获取，远端没有新提交`
        : `已从 ${result.remote} 获取，更新 ${result.updated.length} 个引用`
    })
    // fetch 只动远端跟踪引用，本地清单不变；但仍重拉一次，让「落后/领先」的展示跟上。
    await refreshGit()
    return true
  } catch (error) {
    failWith(error)
    return false
  }
}

/** 只做快进拉取；非快进由内核回 `7007`，面板据此提示先处理分叉。 */
export async function pullRemote(options: { remote?: string; branch?: string } = {}): Promise<boolean> {
  const workspaceId = activeWorkspaceId()
  if (workspaceId === null) {
    setState({ hint: 'Git 面板需要先打开工作区。' })
    return false
  }

  startAction()
  try {
    const result = await gitApi.pull({
      workspaceId,
      remote: options.remote,
      branch: options.branch
    })
    setState({
      busy: false,
      hint: result.updated
        ? `已从 ${result.upstream} 快进到 ${result.short_oid}（${result.commits} 个提交 / ${result.files} 个文件）`
        : `${result.upstream} 没有新提交，已在最新`
    })
    await refreshGit()
    return true
  } catch (error) {
    failWith(error)
    return false
  }
}

async function push(input: {
  remote?: string
  branch?: string
  setUpstream: boolean
  forceWithLease: boolean
}): Promise<boolean> {
  const workspaceId = activeWorkspaceId()
  if (workspaceId === null) {
    setState({ hint: 'Git 面板需要先打开工作区。' })
    return false
  }

  startAction()
  try {
    const result = await gitApi.push({
      workspaceId,
      remote: input.remote,
      branch: input.branch,
      setUpstream: input.setUpstream,
      forceWithLease: input.forceWithLease
    })
    setState({
      busy: false,
      lastPush: result,
      hint: result.up_to_date
        ? `${result.remote}/${result.remote_branch} 已是最新，无需推送`
        : `已推送 ${result.branch} → ${result.remote}/${result.remote_branch}（${result.short_oid}）` +
          (result.forced ? '，使用了 --force-with-lease' : '')
    })
    await refreshGit()
    return true
  } catch (error) {
    failWith(error)
    return false
  }
}

/** 普通推送（从不强推）。 */
export function pushRemote(options: { remote?: string; branch?: string } = {}): Promise<boolean> {
  return push({ ...options, setUpstream: false, forceWithLease: false })
}

/**
 * 7009 的修复入口：用报错里带的分支名当上游再推一次。
 *
 * 分支名来自 `data.branch`，不是让用户重填——重填最容易填错成 `origin/xxx`。
 */
export function pushWithUpstream(branch: string, remote?: string): Promise<boolean> {
  return push({ remote, branch, setUpstream: true, forceWithLease: false })
}

/**
 * 请求强制推送：**不发请求**，只挂起确认。
 *
 * 想强推的调用方必须先经过 `confirmGitAction`，这就是那道不可绕过的门。
 */
export function requestForcePush(options: { remote?: string; branch?: string } = {}): void {
  const explicit = options.branch ?? null
  const target = explicit ?? state.branches.find((item) => item.is_current)?.name ?? '当前分支'
  const where = options.remote ?? 'origin'
  setState({
    confirm: {
      kind: 'force_push',
      title: '确认强制推送？',
      detail: `将把本地 ${target} 强制推送到 ${where}/${target}（--force-with-lease）。`,
      warning: '远端上你没有的提交会被丢弃，且无法从本地恢复（只能靠远端 reflog）。此操作不可逆。',
      stashIndex: null,
      // 只有显式指定了分支才记下来；否则让确认时按当下的当前分支解析（用户没点具体分支）。
      targetBranch: explicit,
      targetRemote: options.remote ?? null
    }
  })
}

// ---------------------------------------------------------------------------
// stash：保存 / 列表 / pop / apply / drop
// ---------------------------------------------------------------------------

/** 重拉 stash 列表。失败不覆盖变更清单的错误，只落文案。 */
export async function refreshStashes(): Promise<boolean> {
  const workspaceId = activeWorkspaceId()
  if (workspaceId === null) {
    setState({ stashes: [] })
    return false
  }

  setState({ stashLoading: true })
  try {
    const result = await gitApi.stashList(workspaceId)
    if (state.workspaceId !== null && state.workspaceId !== workspaceId) return false
    setState({ stashLoading: false, stashes: result.entries })
    return true
  } catch (error) {
    setState({ stashLoading: false })
    failWith(error)
    return false
  }
}

/** 保存当前改动到 stash（可带说明、可含未跟踪文件）。 */
export async function saveStash(options: { message?: string; includeUntracked?: boolean } = {}): Promise<boolean> {
  const workspaceId = activeWorkspaceId()
  if (workspaceId === null) {
    setState({ hint: 'Git 面板需要先打开工作区。' })
    return false
  }

  const message = options.message?.trim() ?? ''
  startAction()
  try {
    const result = await gitApi.stash({
      workspaceId,
      message: message.length === 0 ? undefined : message,
      includeUntracked: options.includeUntracked ?? false
    })
    if (!result.stashed) {
      // 工作区本来干净：不是错误，但也不该假装存了一条。
      setState({ busy: false, hint: '工作区没有可保存的改动，未生成 stash。', stashes: state.stashes })
      await refreshGit()
      return false
    }
    const saved = result.entry
    setState({
      busy: false,
      hint: `已保存 stash@{${saved?.index ?? 0}}${saved ? `：${saved.message}` : ''}（共 ${result.total} 条）`
    })
    await Promise.all([refreshGit(), refreshStashes()])
    return true
  } catch (error) {
    failWith(error)
    return false
  }
}

/** pop / apply 的共同实现：差别只在成功后删不删条目（由内核决定）。 */
async function takeStash(action: 'pop' | 'apply', index: number): Promise<boolean> {
  const workspaceId = activeWorkspaceId()
  if (workspaceId === null) {
    setState({ hint: 'Git 面板需要先打开工作区。' })
    return false
  }
  if (!Number.isInteger(index) || index < 0) {
    setState({ hint: 'stash 序号非法。' })
    return false
  }

  startAction()
  try {
    const result =
      action === 'pop'
        ? await gitApi.stashPop({ workspaceId, index })
        : await gitApi.stashApply({ workspaceId, index })
    setState({
      busy: false,
      hint: result.dropped
        ? `已 pop stash@{${result.index}} 并删除该条（剩 ${result.total} 条）`
        : `已 apply stash@{${result.index}}，条目保留（共 ${result.total} 条）`
    })
    await Promise.all([refreshGit(), refreshStashes()])
    return true
  } catch (error) {
    const failure = describeGitFailure(error)
    failWith(error)
    // 7010：冲突时条目还在，界面列表**一个字都不许动**。同时提示它还在，避免用户
    // 以为要重新 stash 一遍。刷新列表在这里是错的：内核保留条目，重拉只会掩盖
    // 「UI 自己把条目丢了」这类 bug，也让断言无从下手。
    if (failure.code === 7010) {
      setState({
        hint: `stash@{${index}} 未被删除，冲突解决后仍可 pop / apply / drop。`
      })
    }
    return false
  }
}

/** 取出并删除该条。冲突（7010）时条目保留。 */
export function popStash(index: number): Promise<boolean> {
  return takeStash('pop', index)
}

/** 取出但保留该条。 */
export function applyStash(index: number): Promise<boolean> {
  return takeStash('apply', index)
}

/**
 * 请求删除一条 stash：**不发请求**，只挂起确认（不可逆）。
 */
export function requestDropStash(index: number): void {
  const entry = state.stashes.find((item) => item.index === index) ?? null
  const label = entry === null ? `stash@{${index}}` : `stash@{${entry.index}}（${entry.message}）`
  setState({
    confirm: {
      kind: 'stash_drop',
      title: '确认删除这条 stash？',
      detail: `将删除 ${label}${entry === null ? '' : `，其提交为 ${entry.short_oid}`}。`,
      warning: 'stash 一旦删除，其中的改动将无法从界面上找回（除非你记得 oid 并手动 git stash apply）。此操作不可逆。',
      stashIndex: index,
      targetBranch: null,
      targetRemote: null
    }
  })
}

/** 真正执行删除：只由 `confirmGitAction` 调用，避免有人绕过确认弹窗。 */
async function dropStash(index: number): Promise<boolean> {
  const workspaceId = activeWorkspaceId()
  if (workspaceId === null) {
    setState({ hint: 'Git 面板需要先打开工作区。' })
    return false
  }

  startAction()
  try {
    const result = await gitApi.stashDrop({ workspaceId, index })
    setState({
      busy: false,
      lastStashDrop: result,
      hint: `已删除 stash@{${result.index}}（${result.message}，${result.oid.slice(0, 7)}），剩 ${result.total} 条`
    })
    await refreshStashes()
    return true
  } catch (error) {
    failWith(error)
    return false
  }
}

/** 确认挂起的不可逆动作。没有挂起时返回 `false`（幂等：双击不会推两次）。 */
export async function confirmGitAction(): Promise<boolean> {
  const pending = state.confirm
  if (pending === null) return false

  // 先关弹窗再发请求：失败信息由 `error` 承载，弹窗不该一直挡着。
  setState({ confirm: null })

  if (pending.kind === 'stash_drop') {
    return pending.stashIndex === null ? false : dropStash(pending.stashIndex)
  }
  // 按挂起时记下的目标推，而不是「确认那一刻的当前分支」。
  return push({
    remote: pending.targetRemote ?? undefined,
    branch: pending.targetBranch ?? undefined,
    setUpstream: false,
    forceWithLease: true
  })
}

/** 取消挂起的不可逆动作：什么都不发。 */
export function cancelGitAction(): void {
  if (state.confirm !== null) setState({ confirm: null })
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
