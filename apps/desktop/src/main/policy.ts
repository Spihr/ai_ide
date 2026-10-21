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
  Methods.search.content,

  // M2：模型接入。放行的理由与 M1 一致——这些都要用户显式动作才会发起：
  // 配置/删除 Provider 与模型、点「测试连接」、发一条对话、按「停止」。
  // 其中 `provider.test` 会向 Provider 的 base_url 发起真实请求、`llm.chat_stream`
  // 会产生真实调用与计费，所以它们**只能**由用户在设置界面或对话面板亲手触发，
  // 不存在任何「渲染进程自动重试」的路径。
  Methods.provider.upsert,
  Methods.provider.list,
  Methods.provider.delete,
  Methods.provider.test,
  Methods.model.upsert,
  Methods.model.list,
  Methods.model.delete,
  Methods.llm.chatStream,
  Methods.llm.cancel,
  Methods.llm.listModels,
  Methods.llm.countTokens,

  // M4：Agent 运行。三条方法都是「用户显式动作」的产物，没有自动发起的路径：
  // `agent.start` 由按下「启动」触发，`agent.stop` 由按下「停止」触发，
  // `agent.decide` 只能由用户对某一次 `agent.approval_required` 按下裁决触发。
  //
  // 注意这里放行的是**发起**，不是**放行工具**：具体某个工具调用要不要执行，由内核
  // 的权限闸门按策略判定，闸门要问用户时走 `agent.approval_required` 通知。外壳
  // 因此不存在「绕过闸门」的入口——`permissions_override` 在 UI 上根本没有控件。
  Methods.agent.start,
  Methods.agent.stop,
  Methods.agent.decide
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
