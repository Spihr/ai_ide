/**
 * 策略闸门（Policy Gate）。
 *
 * 架构约定：**渲染进程永远不能直接把方法名透传给内核**。
 * 所有来自 renderer 的调用都必须先经过这里，由主进程判断「这个方法是否允许
 * 被 UI 直接触发」。
 *
 * M0 阶段只有一条规则：白名单之外一律拒绝。后续里程碑会在这里加入
 * 更细的判定（工作区路径校验、命令危险等级、工具审批级别等）。
 */

import { Methods } from '@ai-ide/shared-types'

/** 允许渲染进程直接调用的方法。 */
const RENDERER_ALLOWED = new Set<string>([
  Methods.core.ping,
  Methods.core.stats,

  // M1：工作区与文件系统。这些都是「用户显式动作」才能触发的操作：
  // 打开/关闭工作区、列目录、读文件、写文件（带乐观锁）、创建、重命名、
  // 删除（固定走回收站，内核侧 `to_trash` 恒为 true，无永久删除）、定位文件。
  Methods.workspace.open,
  Methods.workspace.close,
  Methods.workspace.tree,
  Methods.workspace.readFile,
  Methods.workspace.writeFile,
  Methods.workspace.create,
  Methods.workspace.rename,
  Methods.workspace.delete,
  Methods.workspace.reveal,

  // M1：检索。
  Methods.search.files,
  Methods.search.content
])

export interface PolicyDecision {
  allowed: boolean
  reason?: string
}

/** 判定渲染进程是否可以直接调用某个方法。 */
export function checkRendererCall(method: string): PolicyDecision {
  if (typeof method !== 'string' || method.length === 0) {
    return { allowed: false, reason: '方法名必须是非空字符串' }
  }
  if (method.startsWith('$/')) {
    return { allowed: false, reason: `保留方法不允许被渲染进程调用：${method}` }
  }
  if (!RENDERER_ALLOWED.has(method)) {
    return {
      allowed: false,
      reason: `渲染进程无权调用 ${method}；如需开放，请在 main/policy.ts 中显式加入白名单`
    }
  }
  return { allowed: true }
}

/** 当前白名单快照（供调试与测试）。 */
export function allowedRendererMethods(): string[] {
  return [...RENDERER_ALLOWED]
}
