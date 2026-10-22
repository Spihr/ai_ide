/**
 * M5 渲染侧 API 封装：把 `window.aiide.request` 收窄成检查点业务动作。
 *
 * 与 `session-api.ts` 同一套路——组件不拼方法名与参数。所有调用仍要过主进程的
 * 策略闸门（四门 `checkpoint.*` 已在白名单内）。
 *
 * 协议里可选字段的类型是 `field?: T`（Rust 的 `Option<T>` 序列化成「缺省」而不是
 * `null`），所以这里用 `?? undefined` 让 `JSON.stringify` 把字段整体省掉——
 * 手写成 `null` 会撞上类型，也会在部分反序列化路径上被当成「显式给了个空值」。
 */

import {
  Methods,
  type CheckpointCreateParams,
  type CheckpointCreateResult,
  type CheckpointDeleteResult,
  type CheckpointListParams,
  type CheckpointListResult,
  type CheckpointRestoreResult,
  type CheckpointRestoreStatus,
  type CheckpointView
} from '@ai-ide/shared-types'

/** 列表默认分页：检查点数量级远小于消息，一屏够用。 */
export const DEFAULT_CHECKPOINT_PAGE = 20

function request<T>(method: string, params: unknown): Promise<T> {
  return window.aiide.request(method, params) as Promise<T>
}

export const checkpointApi = {
  /** 手动拍一次快照；`paths` 缺省表示按忽略规则遍历整个工作区。 */
  create: (params: Partial<CheckpointCreateParams> = {}): Promise<CheckpointCreateResult> =>
    request<CheckpointCreateResult>(Methods.checkpoint.create, {
      workspace_id: params.workspace_id ?? undefined,
      label: params.label ?? undefined,
      paths: params.paths ?? undefined,
      session_id: params.session_id ?? undefined
    } satisfies CheckpointCreateParams),

  list: (params: Partial<CheckpointListParams> = {}): Promise<CheckpointListResult> =>
    request<CheckpointListResult>(Methods.checkpoint.list, {
      workspace_id: params.workspace_id ?? undefined,
      limit: params.limit ?? DEFAULT_CHECKPOINT_PAGE,
      offset: params.offset ?? 0
    } satisfies CheckpointListParams),

  /** 回滚：内核按快照逐字节写回并回读校验，`verified` 是校验结论。 */
  restore: (checkpointId: string): Promise<CheckpointRestoreResult> =>
    request<CheckpointRestoreResult>(Methods.checkpoint.restore, { checkpoint_id: checkpointId }),

  remove: (checkpointId: string): Promise<CheckpointDeleteResult> =>
    request<CheckpointDeleteResult>(Methods.checkpoint.delete, { checkpoint_id: checkpointId })
}

/** 字节数 → 人可读（检查点列表里显示快照体积）。 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB']
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  const digits = unit === 0 ? 0 : value < 10 ? 1 : 0
  return `${value.toFixed(digits)} ${units[unit]}`
}

/** Unix 毫秒 → 本地 `MM-DD HH:mm`（列表够读，不引第三方时间库）。 */
export function formatCheckpointTime(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return '—'
  const date = new Date(ms)
  const pad = (value: number): string => String(value).padStart(2, '0')
  return `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`
}

/** 来源标签：Agent 自动拍的 vs 手动拍的，用户一眼要能分开。 */
export function originText(checkpoint: CheckpointView): string {
  return checkpoint.origin === 'agent' ? 'Agent 自动' : '手动'
}

/** 回滚报告里单个文件的状态文案。 */
export function restoreStatusText(status: CheckpointRestoreStatus): string {
  switch (status) {
    case 'restored':
      return '已还原'
    case 'recreated':
      return '已重建'
    case 'removed':
      return '已删除'
    case 'failed':
      return '失败'
    default:
      return String(status)
  }
}
