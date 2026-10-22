/**
 * 渲染进程的终端状态（M6 切片二）。
 *
 * 设计上刻意把 xterm 实例留在组件里，store 只管「有哪些会话、谁在前台、活没活」：
 *
 * - 输出不走 store 的 `state`：一段命令能刷出几千个 chunk，每次 `set()` 都会让
 *   整棵树重渲染。这里用**按会话 id 的订阅表**（`subscribeOutput`）把字节直接
 *   递到那个 xterm 实例的 `write()`，store 只在「标签增删 / 状态切换」时变。
 * - 还没挂载的标签（例如后台标签刚创建）先落到 `pending` 缓冲，挂载时再灌回去，
 *   否则「建会话 → 首屏 banner 输出」这一段落会丢。
 * - 面板关掉不杀进程：标签页与 PTY 的生命周期由用户点「×」决定，隐藏面板只是
 *   不看它——这跟 VS Code 一致，也避免「手滑收起面板，跑着的构建就没了」。
 *
 * 所有动作自己吞异常并落到 `error` / 标签的 `status`，不向调用方抛。
 */

import { useSyncExternalStore } from 'react'

import type { TerminalDataEvent, TerminalExitEvent } from '@ai-ide/shared-types'

import { getEditorState } from './store'
import {
  createTerminal,
  decodeBase64,
  describeTerminalError,
  killTerminal,
  listTerminals,
  resizeTerminal
} from './terminal-api'

export type TerminalStatus = 'starting' | 'running' | 'exited' | 'failed'

/** 一个终端标签。`id` 就是内核的 `terminal_id`，也是 xterm 实例的键。 */
export interface TerminalTab {
  id: string
  /** 显示名，如 `PowerShell 1`。 */
  title: string
  /** 内核实际启动的可执行文件。 */
  shell: string
  /** 起始工作目录（内核回报的）。 */
  cwd: string | null
  status: TerminalStatus
  /** 子进程退出码；未退出或拿不到时为 `null`。 */
  exitCode: number | null
  /** 终止信号（Unix）；Windows 恒为 `null`。 */
  signal: string | null
  /** 建会话失败的原因。 */
  error: string | null
}

export interface TerminalState {
  /** 底部面板是否展开。 */
  open: boolean
  tabs: TerminalTab[]
  /** 前台标签的 id。 */
  activeId: string | null
  /** 正在创建会话。 */
  busy: boolean
  /** 面板级错误（建会话失败等）。 */
  error: string | null
}

/** 未挂载标签的待灌输出上限：超过就丢最旧的，避免后台标签吃光内存。 */
const PENDING_LIMIT_BYTES = 512 * 1024

const initialState: TerminalState = {
  open: false,
  tabs: [],
  activeId: null,
  busy: false,
  error: null
}

let state: TerminalState = initialState
const listeners = new Set<() => void>()

/** 会话 id → 输出订阅者（每个挂载的 xterm 一个）。 */
const outputListeners = new Map<string, Set<(bytes: Uint8Array) => void>>()
/** 会话 id → 还没人接的输出块。 */
const pending = new Map<string, Uint8Array[]>()
let pendingBytes = 0
/** 终端标题的全局序号（关掉再开不复用，避免两个「PowerShell 1」）。 */
let titleSeq = 0

function emit(): void {
  for (const listener of listeners) listener()
}

function set(patch: Partial<TerminalState>): void {
  state = { ...state, ...patch }
  emit()
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

/** 订阅整个终端状态。 */
export function useTerminal(): TerminalState {
  return useSyncExternalStore(subscribe, () => state)
}

/** 非 React 场景（事件回调、测试）读取快照。 */
export function getTerminalState(): TerminalState {
  return state
}

/** 测试用：清空状态与所有缓冲。 */
export function resetTerminalState(): void {
  state = initialState
  outputListeners.clear()
  pending.clear()
  pendingBytes = 0
  titleSeq = 0
  emit()
}

/** 从可执行文件路径里取一个像名字的东西：`C:\...\powershell.exe` → `powershell`。 */
export function shellName(shell: string): string {
  const base = shell.split(/[\\/]/).pop() ?? shell
  return base.replace(/\.exe$/i, '') || base
}

/** 标题带上序号，多开时能区分。 */
function nextTitle(shell: string): string {
  titleSeq += 1
  return `${shellName(shell)} ${titleSeq}`
}

// ---------------------------------------------------------------------------
// 输出分发
// ---------------------------------------------------------------------------

function pushPending(id: string, bytes: Uint8Array): void {
  const queue = pending.get(id) ?? []
  queue.push(bytes)
  pendingBytes += bytes.length
  pending.set(id, queue)

  // 丢最旧的整块，直到回到上限内。半块不切——切了会把转义序列劈成两半。
  while (pendingBytes > PENDING_LIMIT_BYTES && queue.length > 1) {
    const dropped = queue.shift()
    pendingBytes -= dropped?.length ?? 0
  }
}

/**
 * 订阅某个会话的输出，返回取消订阅函数。
 *
 * 已有缓冲会**同步**灌给这个订阅者：xterm 挂载时先收历史、再收增量，
 * 顺序不会乱。
 */
export function subscribeOutput(id: string, listener: (bytes: Uint8Array) => void): () => void {
  const targets = outputListeners.get(id) ?? new Set()
  outputListeners.set(id, targets)
  targets.add(listener)

  const queued = pending.get(id)
  if (queued && queued.length > 0) {
    pending.delete(id)
    for (const chunk of queued) pendingBytes -= chunk.length
    for (const chunk of queued) listener(chunk)
  }

  return () => {
    const current = outputListeners.get(id)
    if (!current) return
    current.delete(listener)
    if (current.size === 0) outputListeners.delete(id)
  }
}

/** 内核推来一段输出：有人接就直接给，没人接先存着。 */
function dispatchOutput(id: string, bytes: Uint8Array): void {
  const targets = outputListeners.get(id)
  if (targets && targets.size > 0) {
    for (const target of targets) target(bytes)
    return
  }
  pushPending(id, bytes)
}

// ---------------------------------------------------------------------------
// 内核事件入口
// ---------------------------------------------------------------------------

/** `terminal.data`：base64 解成字节后分发。 */
export function receiveTerminalData(event: TerminalDataEvent): void {
  // 已经关掉的标签不必再攒缓冲（用户不会再看到它）。
  if (!state.tabs.some((tab) => tab.id === event.terminal_id)) return
  dispatchOutput(event.terminal_id, decodeBase64(event.data))
}

/** `terminal.exit`：把标签标成已退出，保留输出给用户看最后一屏。 */
export function receiveTerminalExit(event: TerminalExitEvent): void {
  const tab = state.tabs.find((item) => item.id === event.terminal_id)
  if (!tab) return
  if (tab.status === 'exited') return

  set({
    tabs: state.tabs.map((item) =>
      item.id === event.terminal_id
        ? {
            ...item,
            status: 'exited',
            exitCode: event.code,
            signal: event.signal,
            error: null
          }
        : item
    )
  })
}

/**
 * 订阅内核的终端事件。返回取消订阅函数。
 *
 * 只在进程级绑一次（`App` 挂载时）：面板收起时也要收事件，否则重新展开的标签
 * 会缺掉「后台跑完的那段输出」。
 */
export function initTerminalEvents(): () => void {
  const offData = window.aiide.onTerminalData((event) => receiveTerminalData(event))
  const offExit = window.aiide.onTerminalExit((event) => receiveTerminalExit(event))
  return () => {
    offData()
    offExit()
  }
}

// ---------------------------------------------------------------------------
// 动作
// ---------------------------------------------------------------------------

/** 新建一个会话并切到它。`cwd` 缺省取当前工作区根。 */
export async function newTerminalTab(cwd?: string | null): Promise<string | null> {
  if (state.busy) return null
  set({ busy: true, error: null })

  const targetCwd = cwd ?? getEditorState().workspace?.root ?? null

  try {
    const view = await createTerminal({ cwd: targetCwd })
    const tab: TerminalTab = {
      id: view.id,
      title: nextTitle(view.shell),
      shell: view.shell,
      cwd: view.cwd ?? null,
      // 会话已经建出来了就是活的；被立刻杀掉的极端情况由 exit 事件纠正。
      status: 'running',
      exitCode: null,
      signal: null,
      error: null
    }
    set({
      tabs: [...getTerminalState().tabs, tab],
      activeId: tab.id,
      busy: false,
      error: null
    })
    return tab.id
  } catch (error) {
    set({ busy: false, error: describeTerminalError(error) })
    return null
  }
}

/**
 * 关闭标签：**先从 UI 摘掉，再异步杀进程**。
 *
 * 反过来的话，`terminal.kill` 慢一点（Windows 上要等子进程收尾）用户就会觉得
 * 「点了 × 没反应」，而重复点会打死同一个 id。杀失败只记日志式错误——
 * 标签已经不在了，报错也不该把面板顶起来。
 */
export async function closeTerminalTab(id: string): Promise<void> {
  const tabs = getTerminalState().tabs
  const index = tabs.findIndex((tab) => tab.id === id)
  if (index < 0) return

  const rest = tabs.filter((tab) => tab.id !== id)
  const nextActive =
    getTerminalState().activeId === id
      ? (rest[Math.min(index, rest.length - 1)]?.id ?? null)
      : getTerminalState().activeId

  pending.delete(id)
  outputListeners.delete(id)
  set({ tabs: rest, activeId: nextActive })

  try {
    await killTerminal(id)
  } catch (error) {
    set({ error: describeTerminalError(error) })
  }
}

/**
 * 重启一个已退出的标签：在**原位置**换一个同 cwd 的新会话。
 *
 * 不新建到末尾，是因为「重启」的语义是「这个坑位继续用」；标签顺序跳来跳去
 * 会让人找不到刚才那个。
 */
export async function restartTerminalTab(id: string): Promise<string | null> {
  const tabs = getTerminalState().tabs
  const index = tabs.findIndex((tab) => tab.id === id)
  if (index < 0) return null
  const old = tabs[index]
  if (!old) return null
  if (state.busy) return null

  const rest = tabs.filter((tab) => tab.id !== id)
  set({ tabs: rest, activeId: rest[Math.max(index - 1, 0)]?.id ?? null, busy: true, error: null })

  // 旧会话可能已经自己退了，kill 失败属于正常路径。
  await killTerminal(id).catch(() => undefined)
  pending.delete(id)
  outputListeners.delete(id)

  try {
    const view = await createTerminal({ cwd: old.cwd })
    const tab: TerminalTab = {
      id: view.id,
      title: nextTitle(view.shell),
      shell: view.shell,
      cwd: view.cwd ?? null,
      status: 'running',
      exitCode: null,
      signal: null,
      error: null
    }
    const current = [...getTerminalState().tabs]
    current.splice(Math.min(index, current.length), 0, tab)
    set({ tabs: current, activeId: tab.id, busy: false, error: null })
    return tab.id
  } catch (error) {
    set({ busy: false, error: describeTerminalError(error) })
    return null
  }
}

/** 切前台标签。 */
export function setActiveTerminal(id: string): void {
  if (!getTerminalState().tabs.some((tab) => tab.id === id)) return
  if (getTerminalState().activeId === id) return
  set({ activeId: id })
}

/** 展开 / 收起面板。展开时若一个标签都没有，顺手开一个。 */
export async function toggleTerminalPanel(force?: boolean): Promise<void> {
  const next = force ?? !getTerminalState().open
  set({ open: next })
  if (!next) return
  if (getTerminalState().tabs.length === 0) {
    if (!getTerminalState().busy) await newTerminalTab()
    return
  }
  // 已有标签：对齐一次内核真实会话表——内核重启后旧标签其实都死了，
  // 不校正的话用户点到的是一个再也写不进字的空壳。
  void reconcileTerminals()
}

/** 收起面板（保留会话）。 */
export function closeTerminalPanel(): void {
  if (!getTerminalState().open) return
  set({ open: false })
}

/** 面板级错误手动清掉。 */
export function dismissTerminalError(): void {
  if (!getTerminalState().error) return
  set({ error: null })
}

/** 把内核真实的会话表对齐到 UI（例如内核重启后，旧标签其实都死了）。 */
export async function reconcileTerminals(): Promise<void> {
  try {
    const alive = await listTerminals()
    const aliveIds = new Set(alive.map((view) => view.id))
    const tabs = getTerminalState().tabs
    const kept = tabs.filter((tab) => tab.status !== 'running' || aliveIds.has(tab.id))
    if (kept.length === tabs.length) return
    const activeId = getTerminalState().activeId
    set({
      tabs: kept,
      activeId: kept.some((tab) => tab.id === activeId) ? activeId : (kept[0]?.id ?? null)
    })
  } catch (error) {
    set({ error: describeTerminalError(error) })
  }
}

/** 把本地尺寸同步给内核；由 xterm 的 `onResize` 触发。 */
export async function syncTerminalSize(id: string, cols: number, rows: number): Promise<void> {
  // 面板还没布局完时 xterm 会报 0×0，这种请求只会被内核夹取，白跑一趟。
  if (cols <= 0 || rows <= 0) return
  try {
    await resizeTerminal(id, cols, rows)
  } catch (error) {
    set({ error: describeTerminalError(error) })
  }
}
