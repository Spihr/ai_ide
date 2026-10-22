/**
 * M5 渲染侧状态：权限表（免问范围）。
 *
 * 与 `session-store` / `checkpoint-store` 同一形态：单例 + `useSyncExternalStore`，
 * 动作返回布尔表示「是否真的做成了」，失败原因落到 `error`，UI 只负责显示。
 *
 * 这一屏只有「看清单」和「撤销」两种动作。撤销之后同类工具会重新回到「问一次」，
 * 所以撤销是**安全方向**的改动——但仍是用户可感知的行为变化，UI 侧照样要给
 * 行内二次确认，避免手滑把一条『总是允许』清掉之后又一脸问号地发现工具开始弹窗。
 */

import { useSyncExternalStore } from 'react'

import type { PermissionRuleView } from '@ai-ide/shared-types'

import { permissionApi } from './permission-api'
import { getEditorState } from './store'

export interface PermissionState {
  rules: PermissionRuleView[]
  total: number
  /** 该工作区是否已知：没有工作区时只看到 `global` 规则。 */
  workspaceScope: string | null
  loading: boolean
  /** 某个撤销动作进行中（按规则 id 标记，UI 只禁用那一行）。 */
  revoking: string | null
  hint: string | null
  error: string | null
}

function initialState(): PermissionState {
  return {
    rules: [],
    total: 0,
    workspaceScope: null,
    loading: false,
    revoking: null,
    hint: null,
    error: null
  }
}

let state: PermissionState = initialState()

const listeners = new Set<() => void>()

function setState(patch: Partial<PermissionState>): void {
  state = { ...state, ...patch }
  for (const listener of listeners) listener()
}

export function getPermissionState(): PermissionState {
  return state
}

export function subscribePermissions(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function usePermissions(): PermissionState {
  return useSyncExternalStore(subscribePermissions, getPermissionState, getPermissionState)
}

function describe(error: unknown): string {
  if (error instanceof Error) return error.message
  return String(error)
}

/**
 * 拉当前工作区（含 `global`）的免问规则。
 *
 * 故意把 `workspace_id` 留给内核按「活动工作区」缺省——工作区是主进程的会话态，
 * 渲染进程只知道自己打开了哪个目录，报 id 反而可能在切目录的瞬间错位。
 */
export async function refreshPermissions(): Promise<boolean> {
  setState({ loading: true, error: null })
  try {
    const result = await permissionApi.list({})
    setState({
      loading: false,
      rules: result.rules ?? [],
      total: result.total ?? 0,
      workspaceScope: getEditorState().workspace?.root ?? null
    })
    return true
  } catch (error) {
    setState({ loading: false, rules: [], total: 0, error: describe(error) })
    return false
  }
}

/** 面板首次挂载时拉一次；已有数据就不再打一次空请求。 */
export function initPermissions(): void {
  if (state.loading) return
  void refreshPermissions()
}

/** 撤销一条规则（按范围键）。 */
export async function revokePermission(id: string): Promise<boolean> {
  if (state.revoking !== null) return false
  setState({ revoking: id, error: null, hint: null })
  try {
    const result = await permissionApi.revoke(id)
    // 本地直接摘掉这一行，再拉一次总数：撤销是幂等的，返还条数为 0 说明它早就没了。
    setState({
      revoking: null,
      rules: state.rules.filter((rule) => rule.id !== id),
      total: Math.max(0, state.total - (result.removed > 0 ? 1 : 0)),
      hint:
        result.removed > 0
          ? `已撤销 ${id}，之后同类操作会重新询问。剩余 ${result.remaining} 条免问规则。`
          : `${id} 已经不在了（可能是别处已撤销）。剩余 ${result.remaining} 条免问规则。`
    })
    return result.removed > 0
  } catch (error) {
    setState({ revoking: null, error: describe(error) })
    return false
  }
}

/** 撤销某个工具名下的全部规则。 */
export async function revokePermissionTool(tool: string): Promise<boolean> {
  if (state.revoking !== null) return false
  setState({ revoking: `tool:${tool}`, error: null, hint: null })
  try {
    const result = await permissionApi.revokeTool(tool)
    const kept = state.rules.filter((rule) => rule.tool_pattern !== tool)
    setState({
      revoking: null,
      rules: kept,
      total: Math.max(0, state.total - result.removed),
      hint: `已撤销「${tool}」的 ${result.removed} 条免问规则，之后会重新询问。`
    })
    return result.removed > 0
  } catch (error) {
    setState({ revoking: null, error: describe(error) })
    return false
  }
}

export function dismissPermissionHint(): void {
  setState({ hint: null })
}
