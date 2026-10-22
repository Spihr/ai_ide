/**
 * M5 渲染侧 API 封装：把 `window.aiide.request` 收窄成权限表动作。
 *
 * 与 `session-api.ts` 同一套路——组件不拼方法名，也不自己拼参数。
 *
 * 权限表只有「看」和「撤」两个方向：授免问的唯一入口是 `agent.decide` 的
 * `allow_always`（用户在裁决卡片上亲手点的），协议里没有 `permission.allow`。
 * 因此这里的两个动作都不会让闸门变松，撤销之后同类工具会重新回到「问一次」。
 */

import {
  Methods,
  type PermissionListParams,
  type PermissionListResult,
  type PermissionRevokeParams,
  type PermissionRevokeResult,
  type PermissionRuleView
} from '@ai-ide/shared-types'

/** 一次拉多少条：规则本来就少（几十条算多），200 是内核的硬上限。 */
export const PERMISSION_PAGE = 200

function request<T>(method: string, params: unknown): Promise<T> {
  return window.aiide.request(method, params) as Promise<T>
}

export const permissionApi = {
  list: (params: Partial<PermissionListParams> = {}): Promise<PermissionListResult> =>
    request<PermissionListResult>(Methods.permission.list, {
      workspace_id: params.workspace_id,
      limit: params.limit ?? PERMISSION_PAGE
    } satisfies PermissionListParams),

  /** 按范围键撤销单条。`removed === 0` 表示它已经不在了，不算错。 */
  revoke: (id: string): Promise<PermissionRevokeResult> =>
    request<PermissionRevokeResult>(Methods.permission.revoke, {
      id
    } satisfies PermissionRevokeParams),

  /** 按工具名整批撤销：同一工具在不同工作区/范围下的规则一次清掉。 */
  revokeTool: (tool: string): Promise<PermissionRevokeResult> =>
    request<PermissionRevokeResult>(Methods.permission.revoke, {
      tool
    } satisfies PermissionRevokeParams)
}

/**
 * 范围键 → 人话。
 *
 * 键的形状由内核定义：`tool:<名字>` / `command:<命令>` / `path:<动作>:<相对路径>`，
 * 也可能是 `global` 级别的裸工具名。认不出的形状原样显示——宁可露出一个生键，
 * 也不要把用户看不懂的那条规则藏起来。
 */
export function scopeParts(rule: PermissionRuleView): { kind: string; detail: string } {
  const [head = '', ...rest] = rule.id.split(':')
  const detail = rest.join(':')
  switch (head) {
    case 'tool':
      return { kind: '工具', detail }
    case 'command':
      return { kind: '命令', detail }
    case 'path':
      return { kind: '路径', detail }
    default:
      return { kind: '规则', detail: rule.id }
  }
}

/** 生效范围的人话：`global` 是「所有工作区」，其余显示目录名。 */
export function scopeLabel(scope: string): string {
  if (scope === 'global' || scope.length === 0) return '所有工作区'
  const parts = scope.split(/[\\/]/).filter((part) => part.length > 0)
  return parts[parts.length - 1] ?? scope
}

/** 权限记录的相对时间：一个月内按天数/小时数，再远直接给日期。 */
export function formatRuleTime(createdAtMs: number): string {
  const delta = Date.now() - createdAtMs
  if (delta < 0) return '刚刚'
  const minute = 60_000
  const hour = 60 * minute
  const day = 24 * hour
  if (delta < minute) return '刚刚'
  if (delta < hour) return `${Math.floor(delta / minute)} 分钟前`
  if (delta < day) return `${Math.floor(delta / hour)} 小时前`
  if (delta < 30 * day) return `${Math.floor(delta / day)} 天前`
  const date = new Date(createdAtMs)
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(
    date.getDate()
  ).padStart(2, '0')}`
}
