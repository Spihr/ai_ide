/**
 * 命令面板（FR-1.7）的渲染侧状态机：开关、查询、选中项、插件命令取数与执行分发。
 *
 * 分工：`command-registry.ts` 只管纯逻辑（过滤 / 分组 / 导航 / 文案），本文件只做
 * **副作用**——拉插件命令、把「用户按了回车」翻成一次 store 动作或一次
 * `plugin.command` IPC，并把失败原因做成可见提示。
 *
 * 为什么不复用 `plugin-store` 的清单：面板要的是**全部插件的命令**（而不是当前选中
 * 那一个），拿 `plugin-store.plugins` 还得逐个补 `status`，不如自己拉一遍干净；
 * 代价是打开面板时多几个 `plugin.status`，都是本地账本读，量级可忽略。
 */

import { useSyncExternalStore } from 'react'

import {
  builtinCommands,
  describeCommandError,
  filterCommands,
  groupCommands,
  moveActiveIndex,
  normalizeActiveIndex,
  pluginCommands,
  type BuiltinCommandActions,
  type PaletteCommand,
  type PaletteCommandGroup,
  type PluginCommandSource
} from './command-registry'
import { pluginApi, pluginErrorCode, pluginErrorMessage } from './plugin-api'
import {
  getEditorState,
  pickAndOpenWorkspace,
  saveTab,
  setActivity,
  setNotice
} from './store'
import { toggleTerminalPanel } from './terminal-store'

export interface CommandPaletteState {
  open: boolean
  query: string
  /** 在整个（已过滤的）扁平列表里的下标；-1 表示没有候选项。 */
  activeIndex: number
  /** 内置命令（常量，装配一次）。 */
  builtin: PaletteCommand[]
  /** 插件命令（随清单刷新）。 */
  plugin: PaletteCommand[]
  /** 插件命令是否正在拉取。 */
  loading: boolean
  /** 拉取插件命令失败时的说明（面板仍可用，只是没有插件那一组）。 */
  loadError: string | null
  /** 正在执行的行 id，防止连按回车重复触发。 */
  pendingId: string | null
}

/** 内置命令的真实动作。抽出来是为了让 `initialState` 与测试都能一眼看清接线。 */
export const builtinActions: BuiltinCommandActions = {
  openWorkspace: () => pickAndOpenWorkspace(),
  saveActiveTab: () => saveActiveTabQuietly(),
  showView: (view) => setActivity(view),
  toggleTerminal: () => toggleTerminalPanel()
}

/**
 * 保存活动标签。
 *
 * 没有活动标签时 store 自己会返回 false 且什么都不做——这里补一句提示，否则用户
 * 按了回车只看到面板关掉，会以为按键没生效。
 */
async function saveActiveTabQuietly(): Promise<void> {
  const activePath = getEditorState().activePath
  if (!activePath) {
    setNotice({ kind: 'warn', text: '没有可保存的文件：先打开一个文件再保存。' })
    return
  }
  await saveTab(activePath)
}

function initialState(): CommandPaletteState {
  return {
    open: false,
    query: '',
    activeIndex: 0,
    builtin: builtinCommands(builtinActions),
    plugin: [],
    loading: false,
    loadError: null,
    pendingId: null
  }
}

let state: CommandPaletteState = initialState()

const listeners = new Set<() => void>()

function setState(patch: Partial<CommandPaletteState>): void {
  state = { ...state, ...patch }
  for (const listener of listeners) listener()
}

export function getPaletteState(): CommandPaletteState {
  return state
}

export function subscribePalette(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function usePalette(): CommandPaletteState {
  return useSyncExternalStore(subscribePalette, getPaletteState, getPaletteState)
}

/** 测试用：模块级单例跨用例不清就会串状态。 */
export function resetPaletteState(): void {
  state = initialState()
  for (const listener of listeners) listener()
}

/** 全部候选项：内置在前、插件在后（`filterCommands` 同分时按这个顺序稳定输出）。 */
export function paletteCommands(current: CommandPaletteState = state): PaletteCommand[] {
  return [...current.builtin, ...current.plugin]
}

/** 面板要渲染的分组（内置 / 插件）。 */
export function paletteGroups(current: CommandPaletteState = state): PaletteCommandGroup[] {
  return groupCommands(filterCommands(paletteCommands(current), current.query))
}

/**
 * 键盘导航用的扁平列表。
 *
 * 这里刻意**按分组后的顺序**拍平，而不是直接用 `filterCommands` 的输出：分组会把
 * 内置命令整体提到插件命令前面，若导航仍用未分组的排序，「第 N 行高亮」与「最终
 * 执行的那条」就会对不上——按 ↓ 跳到的行和回车跑的命令不是同一个，是面板最难查的
 * 一类 bug。组内仍是相关性排序，所以「更相关的排前面」不受影响。
 */
export function visibleCommands(current: CommandPaletteState = state): PaletteCommand[] {
  return paletteGroups(current).flatMap((group) => group.commands)
}

/** 当前高亮的那一行（没有候选时为 null）。 */
export function activeCommand(current: CommandPaletteState = state): PaletteCommand | null {
  const visible = visibleCommands(current)
  const index = normalizeActiveIndex(current.activeIndex, visible.length)
  return index < 0 ? null : visible[index] ?? null
}

/**
 * 唤出面板。
 *
 * 每次唤出都重置 query 与选中项（上次搜过的词留着，下次打开会让人莫名其妙），
 * 并顺手刷新插件命令——用户刚装完插件就按快捷键是常见路径，缓存不能太长寿。
 */
export function openPalette(): void {
  setState({ open: true, query: '', activeIndex: 0, loadError: null })
  void refreshPluginCommands()
}

export function closePalette(): void {
  setState({ open: false, query: '', activeIndex: 0 })
}

export function togglePalette(): void {
  if (state.open) closePalette()
  else openPalette()
}

/** Esc：先清掉查询词，词已空才真正关面板（两级退出，和编辑器里的习惯一致）。 */
export function escapePalette(): void {
  if (state.query.length > 0) {
    setState({ query: '', activeIndex: 0 })
    return
  }
  closePalette()
}

export function setPaletteQuery(query: string): void {
  // 查询一变，原来的下标没有意义：直接回到第一行。
  setState({ query, activeIndex: 0 })
}

export function setPaletteActive(index: number): void {
  const total = visibleCommands().length
  setState({ activeIndex: normalizeActiveIndex(index, total) })
}

/** ↑ / ↓：越界回绕，空列表时回到 0。 */
export function movePaletteActive(delta: number): void {
  const total = visibleCommands().length
  const next = moveActiveIndex(state.activeIndex, delta, total)
  setState({ activeIndex: next < 0 ? 0 : next })
}

/**
 * 拉取所有已安装插件的命令清单。
 *
 * 单个插件取详情失败**不**让整组消失：这只说明该插件这一行读不到，其它插件的命令
 * 仍然该出现在面板里；失败信息合并进 `loadError` 供界面上给一句轻提示。
 */
export async function refreshPluginCommands(): Promise<void> {
  if (state.loading) return
  setState({ loading: true })
  const sources: PluginCommandSource[] = []
  const failures: string[] = []
  try {
    const listed = await pluginApi.list()
    const statuses = await Promise.all(
      listed.plugins.map(async (plugin) => {
        try {
          const status = await pluginApi.status(plugin.id)
          return { plugin, status }
        } catch (error) {
          failures.push(`${plugin.name}：${pluginErrorMessage(error)}`)
          return null
        }
      })
    )
    for (const entry of statuses) {
      if (!entry) continue
      sources.push({
        pluginId: entry.plugin.id,
        pluginName: entry.plugin.name,
        state: entry.status.plugin.state,
        commands: entry.status.commands
      })
    }
    setState({
      loading: false,
      plugin: pluginCommands(sources),
      loadError:
        failures.length === 0
          ? null
          : `有 ${failures.length} 个插件的命令没读到：${failures.join('；')}`
    })
  } catch (error) {
    // 清单都拉不回来：保留上一次的插件命令，只提示一句，别把面板清空。
    setState({
      loading: false,
      loadError: `插件命令暂时读不到：${pluginErrorMessage(error)}`
    })
  }
}

/** 执行某个命令（面板上的回车 / 点击都走这里）。 */
export async function executeCommand(command: PaletteCommand): Promise<void> {
  if (state.pendingId !== null) return
  if (command.section === 'builtin') {
    closePalette()
    try {
      await command.run?.()
    } catch (error) {
      setNotice({
        kind: 'error',
        text: `「${command.title}」执行失败：${
          error instanceof Error ? error.message : String(error)
        }`
      })
    }
    return
  }

  const pluginId = command.pluginId
  const name = command.command
  if (!pluginId || !name) {
    setNotice({ kind: 'error', text: `「${command.title}」缺少插件或命令标识，未执行。` })
    return
  }

  setState({ pendingId: command.id })
  try {
    const result = await pluginApi.command(pluginId, name)
    closePalette()
    const preview = result.content.trim()
    setNotice({
      kind: 'info',
      text:
        preview.length === 0
          ? `已执行「${command.title}」。`
          : `已执行「${command.title}」：${result.truncated ? `${preview}…（输出已截断）` : preview}`
    })
  } catch (error) {
    // 失败时**不**关面板：用户通常要换一条命令或再试一次，关掉等于把上下文弄丢。
    const described = describeCommandError(
      error,
      { commandTitle: command.title },
      {
        // `pluginErrorCode` 只吃字符串，这里统一先过 `pluginErrorMessage` 做归一
        // （跨 IPC 的 Error / 裸字符串都能拿到 message）。
        code: (error) => pluginErrorCode(pluginErrorMessage(error)),
        message: pluginErrorMessage
      }
    )
    setNotice({ kind: 'error', text: described.text })
  } finally {
    setState({ pendingId: null })
  }
}

/** 回车：执行当前高亮行。 */
export async function executeActiveCommand(): Promise<void> {
  const command = activeCommand()
  if (!command) return
  await executeCommand(command)
}

/** 面板内联错误条（取数失败提示）关掉。 */
export function dismissPaletteLoadError(): void {
  setState({ loadError: null })
}

/** 供组件直接调用的动作集合，避免组件到处 import 模块级函数。 */
export const paletteActions = {
  open: openPalette,
  close: closePalette,
  toggle: togglePalette,
  escape: escapePalette,
  setQuery: setPaletteQuery,
  setActive: setPaletteActive,
  moveActive: movePaletteActive,
  execute: executeCommand,
  executeActive: executeActiveCommand,
  refresh: refreshPluginCommands,
  dismissLoadError: dismissPaletteLoadError
}
