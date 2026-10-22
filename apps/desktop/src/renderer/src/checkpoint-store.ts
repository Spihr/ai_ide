/**
 * M5 渲染侧状态：检查点列表与回滚回执。
 *
 * 与 `session-store` 同一套路（单例 + `useSyncExternalStore`，动作返回布尔、失败原因落
 * `hint` / `error`），区别在于这里有一条**破坏性动作**——`restore` 会覆盖工作区文件。
 *
 * 因此回滚这条路径刻意多做了三件事，都在 store 里而不是 UI 里：
 *
 * 1. 有未保存的标签页就**拒绝**回滚：磁盘会被快照覆盖，编辑器里的脏内容一旦被
 *    `reloadTab` 刷掉就找不回来了。让用户先处理脏文件，比替他决定「丢掉哪一个」安全。
 * 2. 回滚后刷新文件树、把已打开的标签页重新读一遍：不回读的话编辑器还显示旧内容，
 *    用户会以为「回滚没生效」，然后在旧内容上继续编辑、再保存回去——把回滚成果盖掉。
 * 3. 回执如实反映 `verified`：内核逐字节校验失败时不能显示「回滚成功」。
 */

import { useSyncExternalStore } from 'react'

import type { CheckpointRestoreResult, CheckpointView } from '@ai-ide/shared-types'

import { checkpointApi, DEFAULT_CHECKPOINT_PAGE } from './checkpoint-api'
import { getEditorState, refreshTree, reloadTab, tabIsDirty } from './store'

export interface CheckpointState {
  checkpoints: CheckpointView[]
  total: number
  /** 列表加载中。 */
  loading: boolean
  /** 某个动作（拍快照 / 回滚 / 删除）进行中：UI 据此禁用按钮。 */
  busy: boolean
  hint: string | null
  error: string | null
  /** 上一次回滚的逐文件报告；没回滚过为 `null`。 */
  lastRestore: CheckpointRestoreResult | null
}

function initialState(): CheckpointState {
  return {
    checkpoints: [],
    total: 0,
    loading: false,
    busy: false,
    hint: null,
    error: null,
    lastRestore: null
  }
}

let state: CheckpointState = initialState()
const listeners = new Set<() => void>()

function setState(patch: Partial<CheckpointState>): void {
  state = { ...state, ...patch }
  for (const listener of listeners) listener()
}

export function getCheckpointState(): CheckpointState {
  return state
}

export function subscribeCheckpoints(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function useCheckpoints(): CheckpointState {
  return useSyncExternalStore(subscribeCheckpoints, getCheckpointState, getCheckpointState)
}

/** 单测与「重启内核后重连」时重置。 */
export function resetCheckpointState(): void {
  state = initialState()
  for (const listener of listeners) listener()
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

/** 当前打开的工作区 id（`checkpoint.*` 缺省会取活动工作区，这里只做「有没有」判断）。 */
function activeWorkspaceId(): string | null {
  return getEditorState().workspace?.workspace_id ?? null
}

/**
 * 拉检查点列表。
 *
 * 不传 `workspace_id` 的语义是「当前活动工作区」——检查点按工作区根目录定位，
 * 跨工作区混列既无意义（路径不同），也会让「回滚到别人的项目」变成可能。
 */
export async function refreshCheckpoints(): Promise<boolean> {
  if (activeWorkspaceId() === null) {
    setState({ checkpoints: [], total: 0, loading: false, hint: '检查点需要先打开工作区。' })
    return false
  }
  setState({ loading: true, error: null })
  try {
    const result = await checkpointApi.list({ limit: DEFAULT_CHECKPOINT_PAGE, offset: 0 })
    const checkpoints = result.checkpoints ?? []
    setState({
      checkpoints,
      total: result.total ?? checkpoints.length,
      loading: false
    })
    return true
  } catch (error) {
    setState({ loading: false, error: describe(error) })
    return false
  }
}

/**
 * 手动拍一次快照。
 *
 * 标题交给内核生成（带时间戳的来源名），用户想区分可以之后靠列表里的时间读。
 */
export async function createCheckpoint(label: string | null = null): Promise<boolean> {
  if (state.busy) return false
  if (activeWorkspaceId() === null) {
    setState({ hint: '检查点需要先打开工作区。' })
    return false
  }
  setState({ busy: true, error: null })
  try {
    const result = await checkpointApi.create({ label: label?.trim() || undefined })
    const created = result.checkpoint
    setState({
      busy: false,
      hint: `已建立检查点「${created.label}」，覆盖 ${created.files.length} 个文件（${created.total_bytes} 字节）。`,
      checkpoints: [created, ...state.checkpoints.filter((item) => item.checkpoint_id !== created.checkpoint_id)],
      total: state.total + 1
    })
    return true
  } catch (error) {
    setState({ busy: false, error: describe(error) })
    return false
  }
}

/**
 * 回滚到某个检查点。
 *
 * 前置检查（脏标签页 / 未打开工作区）都在发请求之前做：回滚是破坏性的，
 * 不能先改了盘再告诉用户「其实你的未保存改动没了」。
 */
export async function restoreCheckpoint(checkpointId: string): Promise<boolean> {
  if (state.busy) return false
  if (activeWorkspaceId() === null) {
    setState({ hint: '回滚需要先打开工作区（内核按快照记录的绝对路径定位）。' })
    return false
  }
  const dirty = getEditorState().tabs.filter((tab) => tabIsDirty(tab))
  if (dirty.length > 0) {
    setState({
      hint: `有 ${dirty.length} 个未保存的文件，先保存或关闭再回滚：${dirty
        .map((tab) => tab.name)
        .slice(0, 3)
        .join('、')}${dirty.length > 3 ? ' 等' : ''}。回滚会覆盖磁盘，未保存的改动会丢失。`
    })
    return false
  }

  setState({ busy: true, error: null })
  try {
    const report = await checkpointApi.restore(checkpointId)
    const failed = report.files.filter((file) => file.status === 'failed').length
    setState({
      busy: false,
      lastRestore: report,
      hint: report.verified
        ? `回滚完成：${report.files.length} 个文件已按快照写回并逐字节校验通过。`
        : `回滚已执行，但校验未通过：${failed > 0 ? `${failed} 个文件写回失败` : '回读内容与快照记录不一致'}——请检查这些文件后再继续编辑。`
    })
    // 回滚改了盘：文件树与已打开标签页必须回读，否则编辑器显示的是回滚前的内容。
    await refreshTree()
    for (const tab of getEditorState().tabs) {
      await reloadTab(tab.path)
    }
    return report.verified
  } catch (error) {
    setState({ busy: false, error: describe(error) })
    return false
  }
}

/** 删掉一个检查点（快照本体也一起删，`freed_bytes` 是回收的体积）。 */
export async function deleteCheckpoint(checkpointId: string): Promise<boolean> {
  if (state.busy) return false
  setState({ busy: true, error: null })
  try {
    const result = await checkpointApi.remove(checkpointId)
    setState({
      busy: false,
      hint: result.removed
        ? `已删除检查点，释放 ${result.freed_bytes} 字节。`
        : '该检查点已不存在。',
      checkpoints: state.checkpoints.filter((item) => item.checkpoint_id !== checkpointId),
      total: Math.max(0, state.total - (result.removed ? 1 : 0))
    })
    return true
  } catch (error) {
    setState({ busy: false, error: describe(error) })
    return false
  }
}

/** 清掉一次性提示（UI 关闭提示条时调用）。 */
export function dismissCheckpointHint(): void {
  if (state.hint === null && state.error === null) return
  setState({ hint: null, error: null })
}

let initialized = false

/** 应用启动时调一次：有工作区就拉列表，没有就静静等着（打开工作区后由 UI 再拉）。 */
export function initCheckpoints(): void {
  if (initialized) return
  initialized = true
  if (activeWorkspaceId() !== null) void refreshCheckpoints()
}
