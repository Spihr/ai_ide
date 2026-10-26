/**
 * Inline Edit（`Ctrl+K`）的状态机。
 *
 * 流程本身就是四个状态，所以不做成「一堆布尔」：
 *
 * `input`（浮层打开、等指令）→ `loading`（等内核回建议）→ `preview`（diff 预览、
 * 等接受/拒绝）→ `saving`（正在写盘）→ 关。
 *
 * 刻意的几个选择：
 *
 * - **拒绝就是不写**：`reject` 只清状态，不碰文件。写盘只有 `accept` 一条路径，
 *   且走 `workspace.writeFile`（带预期 hash，中途被别人改过就撞 CAS 失败而不是覆盖）。
 * - **发起时就记住 `baseContent`**：内核不写盘，写回要靠外壳自己把 `replacement`
 *   贴回原文。预览期间用户可能又去改了文件，所以接受时先比一次 `content`——
 *   不一致就要求重来，绝不把基于旧内容算出来的补丁打上去。
 * - **错误码进状态、不弹 notice**：9001 / 9003 是「改一下指令再来一次」的即时反馈，
 *   留在浮层里让用户边看边改；notice 是几秒后飘走的，用来报「已写回」这类结果。
 * - **不校验选区本身**：越界判定属于内核（它回 9001），外壳重复判断只会让两边
 *   对「越界」的定义慢慢分叉；这里只判断「有没有选区」。
 */

import { useSyncExternalStore } from 'react'

import type { InlineEditProposeResult, TextSelection } from '@ai-ide/shared-types'

import { decodeRpcErrorPayload } from '../../shared/ipc'
import {
  describeInlineEditError,
  spliceSelection,
  toTextSelection,
  type InlineEditErrorView
} from './inline-edit'
import {
  applyWrittenContent,
  getEditorState,
  setNotice,
  workspaceRelativePath
} from './store'
import { workspaceApi } from './workspace-api'

export type InlineEditPhase = 'input' | 'loading' | 'preview' | 'saving'

export interface InlineEditState {
  open: boolean
  phase: InlineEditPhase
  /** 目标文件的绝对路径；浮层关闭时为 `null`。 */
  path: string | null
  /** 给内核用的相对路径（`/` 分隔），也用来在浮层里显示。 */
  relativePath: string | null
  instruction: string
  /** 发起请求时算出来的选区（协议口径）。 */
  selection: TextSelection | null
  /** 内核回的建议；`preview` / `saving` 阶段才有。 */
  proposal: InlineEditProposeResult | null
  /** 可读错误：既包含 9001/9003 的映射，也包含「没选区」这类本地提示。 */
  error: string | null
  /** 发起请求时的文件全文，接受时用来拼接并检测并发改动。 */
  baseContent: string | null
}

function initialState(): InlineEditState {
  return {
    open: false,
    phase: 'input',
    path: null,
    relativePath: null,
    instruction: '',
    selection: null,
    proposal: null,
    error: null,
    baseContent: null
  }
}

let state: InlineEditState = initialState()

const listeners = new Set<() => void>()

function setState(patch: Partial<InlineEditState>): void {
  state = { ...state, ...patch }
  for (const listener of listeners) listener()
}

export function getInlineEditState(): InlineEditState {
  return state
}

export function subscribeInlineEdit(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function useInlineEdit(): InlineEditState {
  return useSyncExternalStore(subscribeInlineEdit, getInlineEditState, getInlineEditState)
}

/** 测试用：模块级单例跨用例不清就会串状态。 */
export function resetInlineEditState(): void {
  state = initialState()
  for (const listener of listeners) listener()
}

/** 没有选区时的提示语。单独导出，浮层与单测都按同一句话断言。 */
export const NO_SELECTION_HINT = '先在编辑器里选中要改的代码，再按 Ctrl+K。'

/**
 * 异常 → 可读文案。
 *
 * `decodeRpcErrorPayload` 吃的是**消息字符串**（信封藏在 message 尾部），而这里手上
 * 是异常对象，所以先拆一次再交过去。直接把它当 `extract` 传会抛 `lastIndexOf is not
 * a function`——错误处理路径自己再抛错，用户就看到一片空白而不是 9001 的提示。
 */
function toErrorView(error: unknown): InlineEditErrorView {
  return describeInlineEditError(error, (raw) =>
    decodeRpcErrorPayload(raw instanceof Error ? raw.message : String(raw))
  )
}

/**
 * `Ctrl+K`：打开浮层。
 *
 * 无选区时**不打开**也不发请求——只给一句提示。理由：空浮层让人以为界面卡了，
 * 而带着「没有选中代码」的错误浮层还得再按一次 Esc 关掉，多两步。
 */
export function openInlineEdit(): void {
  const editor = getEditorState()
  const path = editor.activePath
  const tab = path === null ? undefined : editor.tabs.find((entry) => entry.path === path)
  if (path === null || tab === undefined) {
    setNotice({ kind: 'warn', text: '先在编辑器里打开一个文件，再按 Ctrl+K。' })
    return
  }
  const selection = editor.selection
  if (selection === null || selection.path !== path) {
    setNotice({ kind: 'warn', text: NO_SELECTION_HINT })
    return
  }
  const relativePath = workspaceRelativePath(path)
  setState({
    open: true,
    phase: 'input',
    path,
    relativePath,
    instruction: '',
    selection: toTextSelection(selection, tab.content),
    proposal: null,
    error: null,
    baseContent: tab.content
  })
}

/** 输入框里改指令；顺手清掉上一次的错误，免得改完还挂着旧提示。 */
export function setInlineEditInstruction(instruction: string): void {
  setState({ instruction, error: null })
}

/** 关闭浮层、丢弃建议。拒绝与 Esc 走同一条路：**不写盘**。 */
export function closeInlineEdit(): void {
  if (!state.open) return
  state = initialState()
  for (const listener of listeners) listener()
}

/** Esc / 「拒绝」：关掉，文件保持原样。 */
export function rejectInlineEdit(): void {
  if (!state.open) return
  const touched = state.proposal !== null
  closeInlineEdit()
  if (touched) setNotice({ kind: 'info', text: '已放弃这次内联编辑，文件未改动。' })
}

/** 提交指令，向内核要建议。 */
export async function submitInlineEdit(): Promise<void> {
  if (!state.open || state.phase === 'loading' || state.phase === 'saving') return
  const { path, relativePath, selection, baseContent } = state
  const instruction = state.instruction.trim()
  if (path === null || relativePath === null || selection === null || baseContent === null) return
  if (instruction === '') {
    setState({ error: '先写一句要改成什么（例如「改成早退返回」）。' })
    return
  }

  const editor = getEditorState()
  const workspaceId = editor.workspace?.workspace_id ?? null
  if (workspaceId === null) {
    setState({ error: '还没有打开工作区，无法发起内联编辑。' })
    return
  }

  setState({ phase: 'loading', error: null, instruction })
  try {
    const proposal = await workspaceApi.inlineEditPropose(workspaceId, {
      path: relativePath,
      instruction,
      selection
    })
    setState({ phase: 'preview', proposal, error: null })
  } catch (error) {
    // 回到 `input` 而不是关掉：9001/9003 都能靠改指令或重选一次解决，浮层留着最省事。
    const view = toErrorView(error)
    setState({ phase: 'input', error: view.text, proposal: null })
  }
}

/**
 * 接受建议：把 `replacement` 拼回原文，经 `workspace.write_file` 写盘。
 *
 * 三道闸门都在写之前：
 * 1. 只在 `preview` 阶段可点（`loading` 时没有 `proposal`）。
 * 2. 预览期间文件被改过（`tab.content !== baseContent`）就拒绝——补丁是按旧内容算的。
 * 3. 拼不出合法全文（`spliceSelection` 返回 `null`）就不写，交给用户重来。
 */
export async function acceptInlineEdit(): Promise<void> {
  if (!state.open || state.phase !== 'preview') return
  const { path, proposal, baseContent } = state
  if (path === null || proposal === null || baseContent === null) return

  const editor = getEditorState()
  const tab = editor.tabs.find((entry) => entry.path === path)
  if (tab === undefined) {
    setState({ error: '这个文件已经不在编辑器里了，建议已丢弃。' })
    return
  }
  if (tab.content !== baseContent) {
    setState({ error: '文件在预览期间被改动过，这份建议已失效。重新按 Ctrl+K 生成一次。' })
    return
  }
  const next = spliceSelection(baseContent, proposal.range, proposal.replacement)
  if (next === null) {
    setState({ error: '建议的替换位置与文件对不上（选区越界），没有写回。重新选中再试。' })
    return
  }

  setState({ phase: 'saving', error: null })
  try {
    const written = await workspaceApi.writeFile(path, next, tab.hash)
    applyWrittenContent(path, next, written.hash)
    const name = path.slice(Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\')) + 1)
    closeInlineEdit()
    setNotice({ kind: 'info', text: `已写回「${name}」。` })
  } catch (error) {
    const view = toErrorView(error)
    setState({ phase: 'preview', error: view.text })
  }
}

/** 给组件用的动作集合（与 `symbolPickerActions` 同一套路，避免组件里散落 import）。 */
export const inlineEditActions = {
  open: openInlineEdit,
  setInstruction: setInlineEditInstruction,
  submit: submitInlineEdit,
  accept: acceptInlineEdit,
  reject: rejectInlineEdit,
  close: closeInlineEdit
}
