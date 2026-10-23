/**
 * 渲染进程的编辑器状态。
 *
 * 一个极简的「单例 store + useSyncExternalStore」实现：状态是普通对象，
 * 变更走 `set()` 一次性替换引用并通知订阅者。没有引入状态库——M1 的状态
 * 形状还很简单，import 一个 reducer 框架只会让阅读成本变高。
 *
 * 约定：
 * - `state` 只在 `set()` / `state =` + `emit()` 里被替换，绝不原地修改；
 * - 所有异步动作自己吞掉异常并落到 `workspaceError` / `notice` / 标签 `error`；
 * - 树按目录懒加载，缓存键是**相对工作区根**的路径（根为 `''`）。
 */

import { useSyncExternalStore } from 'react'

import type {
  ContentMatch,
  Eol,
  FileMatch,
  TreeEntry,
  WorkspaceChangedEvent,
  WorkspaceIndexedEvent,
  WorkspaceOpenResult
} from '@ai-ide/shared-types'

import { workspaceApi, type ContentSearchOptions } from './workspace-api'

export type ActivityView =
  | 'explorer'
  | 'search'
  | 'ai'
  | 'checkpoints'
  | 'permissions'
  | 'git'
  | 'mcp'

/** 一个打开的编辑器标签。 */
export interface Tab {
  /** 绝对路径，同时作为标签的唯一键。 */
  path: string
  name: string
  /** 编辑器中的当前内容（换行统一为 `\n`）。 */
  content: string
  /** 最后一次从磁盘读到的内容，用于判定脏标记。 */
  savedContent: string
  /** 磁盘内容的 SHA-256，写回时的乐观锁。 */
  hash: string | null
  eol: Eol
  encoding: string
  /** 读取时被 `max_bytes` 截断——此时禁止保存，避免把半截内容写回。 */
  truncated: boolean
  loading: boolean
  saving: boolean
  error: string | null
}

export interface Notice {
  kind: 'info' | 'warn' | 'error'
  text: string
}

/**
 * 编辑器里的选区（M3 下一切片）。
 *
 * 只有**真的选中了内容**才算选区：光标停在某处不动是「没有选区」，此时不能拿它去
 * 构造 `selection` 引用——否则用户会把整行、甚至当前位置之后的内容当成自己选中的东西送出去。
 * 行号一律 1-based 且含端点，与内核 `ContextRef.start_line` / `end_line` 同一口径。
 */
export interface EditorSelection {
  /** 绝对路径。 */
  path: string
  /** 起始行（1-based，含）。 */
  startLine: number
  /** 结束行（1-based，含）。 */
  endLine: number
}

/** 内容搜索点击后要在编辑器里定位的位置。 */
export interface RevealTarget {
  /** 绝对路径。 */
  path: string
  /** 行号，从 1 开始。 */
  line: number
  /** 列号，从 1 开始。 */
  column: number
  /** 自增序号：让「再跳同一位置」也能触发编辑器。 */
  nonce: number
}

export interface EditorState {
  workspace: WorkspaceOpenResult | null
  workspaceError: string | null
  opening: boolean
  activity: ActivityView
  /** 目录 → 条目（键为相对根路径，`''` 表示根）。 */
  tree: Record<string, TreeEntry[]>
  /** 已展开的目录（相对根路径）。 */
  expanded: string[]
  /** 正在加载的目录。 */
  loadingDirs: string[]
  tabs: Tab[]
  activePath: string | null
  /** 活动编辑器上报的选区；没有活动文件或没选中内容时为 `null`。 */
  selection: EditorSelection | null
  fileQuery: string
  fileMatches: FileMatch[]
  contentQuery: string
  contentMatches: ContentMatch[]
  contentTruncated: boolean
  searching: boolean
  searchError: string | null
  notice: Notice | null
  /** 待执行的编辑器跳转。 */
  reveal: RevealTarget | null
}

const initialState: EditorState = {
  workspace: null,
  workspaceError: null,
  opening: false,
  activity: 'explorer',
  tree: {},
  expanded: [],
  loadingDirs: [],
  tabs: [],
  activePath: null,
  selection: null,
  fileQuery: '',
  fileMatches: [],
  contentQuery: '',
  contentMatches: [],
  contentTruncated: false,
  searching: false,
  searchError: null,
  notice: null,
  reveal: null
}

let state: EditorState = initialState
const listeners = new Set<() => void>()

function emit(): void {
  for (const listener of listeners) listener()
}

function set(patch: Partial<EditorState>): void {
  state = { ...state, ...patch }
  emit()
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

/** 订阅整个状态。快照是稳定引用，不会引发额外重渲染。 */
export function useEditor(): EditorState {
  return useSyncExternalStore(subscribe, () => state)
}

/** 非 React 场景（快捷键、测试、事件回调）读取当前快照。 */
export function getEditorState(): EditorState {
  return state
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

/** 剥掉 Electron IPC 包装的前缀，只留下内核给的原因。 */
function cleanMessage(error: unknown): string {
  const raw = error instanceof Error ? error.message : String(error)
  return raw.replace(/^Error invoking remote method '[^']*':\s*/, '').replace(/^Error:\s*/, '')
}

function normalize(path: string): string {
  return path.replace(/\\/g, '/')
}

/** 绝对路径 → 相对工作区根的路径（根自身为 `''`）。 */
function relPath(absPath: string): string {
  const root = state.workspace?.root
  if (!root) return absPath
  const target = normalize(absPath)
  const base = normalize(root).replace(/\/+$/, '')
  if (target === base) return ''
  return target.startsWith(`${base}/`) ? target.slice(base.length + 1) : absPath
}

function parentRel(rel: string): string {
  const index = rel.lastIndexOf('/')
  return index < 0 ? '' : rel.slice(0, index)
}

function baseName(path: string): string {
  const norm = normalize(path)
  return norm.slice(norm.lastIndexOf('/') + 1)
}

function isDirty(tab: Tab): boolean {
  return tab.content !== tab.savedContent
}

export function tabIsDirty(tab: Tab): boolean {
  return isDirty(tab)
}

function sortEntries(entries: TreeEntry[]): TreeEntry[] {
  return [...entries].sort((a, b) => {
    const aRank = a.kind === 'dir' ? 0 : 1
    const bRank = b.kind === 'dir' ? 0 : 1
    if (aRank !== bRank) return aRank - bRank
    return a.name.localeCompare(b.name, 'zh-Hans-CN')
  })
}

function patchTab(path: string, patch: Partial<Tab>): void {
  set({ tabs: state.tabs.map((tab) => (tab.path === path ? { ...tab, ...patch } : tab)) })
}

function findTab(path: string | null): Tab | undefined {
  if (!path) return undefined
  return state.tabs.find((tab) => tab.path === path)
}

// ---------------------------------------------------------------------------
// 工作区
// ---------------------------------------------------------------------------

export function setActivity(activity: ActivityView): void {
  set({ activity })
}

export function dismissNotice(): void {
  set({ notice: null })
}

export async function pickAndOpenWorkspace(): Promise<void> {
  try {
    const picked = await window.aiide.pickWorkspaceDirectory()
    if (!picked) return
    await openWorkspace(picked)
  } catch (error) {
    set({ workspaceError: cleanMessage(error) })
  }
}

export async function openWorkspace(path: string): Promise<void> {
  set({ opening: true, workspaceError: null })
  try {
    const workspace = await workspaceApi.open(path, true)
    // 换工作区 = 换世界：标签、树、搜索结果全部重置。
    state = {
      ...initialState,
      workspace,
      activity: state.activity,
      opening: false
    }
    emit()
    await loadDir('')
  } catch (error) {
    set({ opening: false, workspaceError: cleanMessage(error) })
  }
}

export async function closeWorkspace(): Promise<void> {
  const workspace = state.workspace
  if (!workspace) return
  const dirty = state.tabs.filter(isDirty)
  if (dirty.length > 0 && !window.confirm(`有 ${dirty.length} 个文件未保存，确定关闭工作区？`)) {
    return
  }
  try {
    await workspaceApi.close(workspace.workspace_id)
  } catch {
    // 关闭失败不影响本地状态复位：内核那边会话消失是幂等的
  }
  state = { ...initialState, activity: state.activity }
  emit()
}

/** 加载（或刷新）某个目录的一层。 */
export async function loadDir(rel: string): Promise<void> {
  const workspace = state.workspace
  if (!workspace) return
  if (!state.loadingDirs.includes(rel)) {
    set({ loadingDirs: [...state.loadingDirs, rel] })
  }
  try {
    const result = await workspaceApi.tree(workspace.workspace_id, rel === '' ? undefined : rel, 1)
    set({
      tree: { ...state.tree, [rel]: sortEntries(result.entries) },
      loadingDirs: state.loadingDirs.filter((dir) => dir !== rel)
    })
  } catch (error) {
    set({
      loadingDirs: state.loadingDirs.filter((dir) => dir !== rel),
      workspaceError: cleanMessage(error)
    })
  }
}

/** 展开 / 收起目录；首次展开时按需拉取。 */
export async function toggleDir(rel: string): Promise<void> {
  if (state.expanded.includes(rel)) {
    set({ expanded: state.expanded.filter((dir) => dir !== rel) })
    return
  }
  set({ expanded: [...state.expanded, rel] })
  if (!state.tree[rel]) {
    await loadDir(rel)
  }
}

/** 刷新整个可见树：根 + 所有已展开目录。 */
export async function refreshTree(): Promise<void> {
  const dirs = ['', ...state.expanded]
  await Promise.all(dirs.map((dir) => loadDir(dir)))
}

// ---------------------------------------------------------------------------
// 标签
// ---------------------------------------------------------------------------

export async function openFile(absPath: string): Promise<void> {
  const existing = state.tabs.find((tab) => tab.path === absPath)
  if (existing) {
    activateTab(absPath)
    return
  }

  const tab: Tab = {
    path: absPath,
    name: baseName(absPath),
    content: '',
    savedContent: '',
    hash: null,
    eol: 'lf',
    encoding: 'utf-8',
    truncated: false,
    loading: true,
    saving: false,
    error: null
  }
  set({ tabs: [...state.tabs, tab], activePath: absPath, selection: null })

  try {
    const result = await workspaceApi.readFile(absPath)
    patchTab(absPath, {
      content: result.content,
      savedContent: result.content,
      hash: result.hash,
      eol: result.eol,
      encoding: result.encoding,
      truncated: result.truncated,
      loading: false,
      error: result.truncated ? '文件过大，已截断显示；该标签不可保存' : null
    })
  } catch (error) {
    patchTab(absPath, { loading: false, error: cleanMessage(error) })
  }
}

/** 重新从磁盘读取并覆盖标签内容（外部改动、或用户手动刷新时用）。 */
export async function reloadTab(path: string): Promise<void> {
  try {
    const result = await workspaceApi.readFile(path)
    patchTab(path, {
      content: result.content,
      savedContent: result.content,
      hash: result.hash,
      eol: result.eol,
      encoding: result.encoding,
      truncated: result.truncated,
      error: null
    })
  } catch (error) {
    patchTab(path, { error: cleanMessage(error) })
  }
}

export function activateTab(path: string): void {
  if (state.activePath === path) return
  // 换活动编辑器 = 换选区：上一个文件的选区行号套到新文件上就是一条错引用。
  // 这里先清掉，随后由编辑器把新模型的选区重新报上来。
  set({ activePath: path, selection: null })
}

export function closeTab(path: string, force = false): void {
  const tab = findTab(path)
  if (!tab) return
  if (!force && isDirty(tab) && !window.confirm(`「${tab.name}」有未保存的改动，确定关闭？`)) {
    return
  }
  const tabs = state.tabs.filter((item) => item.path !== path)
  const fallback = tabs.length > 0 ? (tabs[tabs.length - 1]?.path ?? null) : null
  const activePath = state.activePath === path ? fallback : state.activePath
  set({
    tabs,
    activePath,
    selection: activePath === state.activePath ? state.selection : null
  })
}

export function updateContent(path: string, content: string): void {
  patchTab(path, { content })
}

/** 保存标签；返回是否真的写盘。 */
export async function saveTab(path?: string): Promise<boolean> {
  const target = path ?? state.activePath
  const tab = findTab(target)
  if (!tab || tab.loading || tab.saving) return false
  if (tab.truncated) {
    patchTab(tab.path, { error: '文件被截断显示，拒绝写回（避免损坏原文件）' })
    return false
  }

  patchTab(tab.path, { saving: true, error: null })
  try {
    const result = await workspaceApi.writeFile(tab.path, tab.content, tab.hash)
    patchTab(tab.path, { saving: false, hash: result.hash, savedContent: tab.content })
    return true
  } catch (error) {
    patchTab(tab.path, { saving: false, error: cleanMessage(error) })
    return false
  }
}

// ---------------------------------------------------------------------------
// 编辑器选区与对话面板可用的引用（M3 下一切片）
// ---------------------------------------------------------------------------

function sameSelection(a: EditorSelection | null, b: EditorSelection | null): boolean {
  if (a === b) return true
  if (!a || !b) return false
  return a.path === b.path && a.startLine === b.startLine && a.endLine === b.endLine
}

/**
 * 编辑器上报选区，`null` 表示「没有选区」（光标停在某处不算选中内容）。
 *
 * 编辑器是唯一知道选区的角色，外壳只负责把真实值存下来：行号不是 ≥1 的整数就整条忽略
 * （不往状态里塞一个可能错的行号），上报的文件与当前活动文件不一致则等同于没有选区。
 */
export function setEditorSelection(selection: EditorSelection | null): void {
  if (!selection || selection.path !== state.activePath) {
    if (state.selection !== null) set({ selection: null })
    return
  }
  const { startLine, endLine } = selection
  if (!Number.isInteger(startLine) || !Number.isInteger(endLine) || startLine < 1 || endLine < 1) {
    return
  }
  const next: EditorSelection = {
    path: selection.path,
    startLine: Math.min(startLine, endLine),
    endLine: Math.max(startLine, endLine)
  }
  if (sameSelection(state.selection, next)) return
  set({ selection: next })
}

/** 对话面板用得到的编辑器侧引用信息：活动文件 + 选区。 */
export interface EditorReference {
  /** 活动文件绝对路径；没有活动文件时为 `null`。 */
  path: string | null
  /** 选区；没有活动文件或没选中内容时为 `null`。 */
  selection: EditorSelection | null
}

const emptyReference: EditorReference = { path: null, selection: null }
let referenceCache: EditorReference = emptyReference

/** `useSyncExternalStore` 的快照必须是稳定引用：只有真的变了才换新对象。 */
function referenceSnapshot(): EditorReference {
  if (
    referenceCache.path === state.activePath &&
    sameSelection(referenceCache.selection, state.selection)
  ) {
    return referenceCache
  }
  referenceCache = { path: state.activePath, selection: state.selection }
  return referenceCache
}

/**
 * 订阅「当前文件 + 选区」这一小块状态。
 *
 * 不订阅整个编辑器状态：打字会不断改 `tabs`，对话面板没必要跟着重渲染。
 */
export function useEditorReference(): EditorReference {
  return useSyncExternalStore(subscribe, referenceSnapshot)
}

// ---------------------------------------------------------------------------
// 检索
// ---------------------------------------------------------------------------

export async function runFileSearch(query: string): Promise<void> {
  set({ fileQuery: query })
  const workspace = state.workspace
  if (!workspace) return
  if (query.trim() === '') {
    set({ fileMatches: [], searchError: null })
    return
  }
  set({ searching: true, searchError: null })
  try {
    const result = await workspaceApi.searchFiles(workspace.workspace_id, query)
    set({ fileMatches: result.matches, searching: false })
  } catch (error) {
    set({ searching: false, searchError: cleanMessage(error) })
  }
}

export async function runContentSearch(
  query: string,
  options: ContentSearchOptions = {}
): Promise<void> {
  set({ contentQuery: query })
  const workspace = state.workspace
  if (!workspace) return
  if (query.trim() === '') {
    set({ contentMatches: [], contentTruncated: false, searchError: null })
    return
  }
  set({ searching: true, searchError: null })
  try {
    const result = await workspaceApi.searchContent(workspace.workspace_id, query, options)
    set({
      contentMatches: result.matches,
      contentTruncated: result.truncated,
      searching: false
    })
  } catch (error) {
    set({ searching: false, searchError: cleanMessage(error) })
  }
}

/** 搜索结果点击：打开文件并请求编辑器定位到行列。 */
let revealNonce = 0

export async function openMatch(
  relPathFromRoot: string,
  line: number,
  column: number
): Promise<void> {
  const workspace = state.workspace
  if (!workspace) return
  // 拼接必须与文件树里 `TreeEntry.path` 同形，否则同一文件会被开成两个标签。
  const sep = workspace.root.includes('\\') ? '\\' : '/'
  const rel = relPathFromRoot.replace(/[\\/]+/g, sep).replace(/^[\\/]+/, '')
  const absPath = `${workspace.root.replace(/[\\/]+$/, '')}${sep}${rel}`
  await openFile(absPath)
  revealNonce += 1
  set({ reveal: { path: absPath, line, column, nonce: revealNonce } })
}

/** 编辑器消费掉跳转请求后调用。 */
export function clearReveal(): void {
  if (state.reveal) set({ reveal: null })
}

// ---------------------------------------------------------------------------
// 内核事件
// ---------------------------------------------------------------------------

export function handleWorkspaceChanged(event: WorkspaceChangedEvent): void {
  const workspace = state.workspace
  if (!workspace || event.workspace_id !== workspace.workspace_id) return

  const root = normalize(workspace.root).replace(/\/+$/, '')
  /**
   * 内核事件里的 path 是**相对工作区根、以 `/` 分隔**的路径（见 crates/workspace/src/watcher.rs），
   * 而标签里存的是绝对路径——两边形式不同，必须先归一化再比对，否则永远匹配不上。
   */
  const toAbs = (path: string): string => {
    const norm = normalize(path)
    const isAbsolute = norm.startsWith('/') || /^[a-zA-Z]:\//.test(norm)
    return isAbsolute ? norm : `${root}/${norm}`
  }

  const dirsToRefresh = new Set<string>()

  for (const change of event.changes) {
    const abs = toAbs(change.path)
    const rel = relPath(abs)
    const dir = parentRel(rel)
    if (dir in state.tree) dirsToRefresh.add(dir)

    const tab = state.tabs.find((item) => normalize(item.path) === abs)
    if (!tab) continue

    if (change.kind === 'deleted') {
      closeTab(tab.path, true)
      continue
    }
    if (change.kind === 'modified' || change.kind === 'created') {
      if (isDirty(tab)) {
        set({
          notice: { kind: 'warn', text: `「${tab.name}」在磁盘上被改动，本地还有未保存内容` }
        })
      } else {
        void reloadTab(tab.path)
      }
    }
  }

  for (const dir of dirsToRefresh) {
    void loadDir(dir)
  }
}

export function handleWorkspaceIndexed(event: WorkspaceIndexedEvent): void {
  const workspace = state.workspace
  if (!workspace || event.workspace_id !== workspace.workspace_id) return
  set({ workspace: { ...workspace, stats: event.stats, indexing: false } })
}

export function initWorkspaceEvents(): () => void {
  const offChanged = window.aiide.onWorkspaceChanged(handleWorkspaceChanged)
  const offIndexed = window.aiide.onWorkspaceIndexed(handleWorkspaceIndexed)
  return () => {
    offChanged()
    offIndexed()
  }
}
