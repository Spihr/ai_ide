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
  Methods.agent.decide,

  // M5：会话与审计。九条 `session.*` 全部是「用户显式动作」的直接后果：
  // 新建 / 切换 / 改名 / 删除 / 导出 / 从某条消息分叉，以及对话面板发消息时
  // 落盘的那一条（用户按了「发送」）。没有任何自动重试或后台批量改写的路径。
  //
  // `session.export` 会写盘，所以它不能拿到任意路径：`out_path` 一律由主进程的
  // 系统保存对话框（`dialog:pick-save-path`）产生，渲染进程没有自行拼路径的入口。
  // `audit.list` 是只读查询，且返回内容已在存储层脱敏（不含密钥、不含完整消息体）。
  Methods.session.create,
  Methods.session.list,
  Methods.session.get,
  Methods.session.update,
  Methods.session.delete,
  Methods.session.appendMessage,
  Methods.session.messages,
  Methods.session.export,
  Methods.session.fork,

  // M5：检查点。四条同样是「用户显式动作」：手动拍快照、打开面板看列表、
  // 点「回滚」、删掉某个检查点。没有定时拍快照或自动回滚的路径
  // （Agent 运行前后的自动检查点由内核自己建，不经渲染进程）。
  //
  // 其中 `checkpoint.restore` 会**覆盖工作区文件**：它是本白名单里破坏性最强的一条。
  // 因此外壳侧强制「先列清单、再二次确认」——`CheckpointPanel` 在调用前必须让用户
  // 敲一次确认，且回滚是内核按快照逐字节校验的（`verified` 如实回报），
  // 不存在「点了就悄悄改一堆文件、用户不知道改了什么」的余地。
  Methods.checkpoint.create,
  Methods.checkpoint.list,
  Methods.checkpoint.restore,
  Methods.checkpoint.delete,

  Methods.audit.list,

  // M5：权限表（免问范围）。这两条回的是「用户自己点过的『总是允许』」的清单，
  // 以及把它撤销掉——**都是显式动作的产物，且只有收窄权限的方向**：
  // 协议里没有 `permission.allow`（授免问只能经 `agent.decide` 的 `allow_always`），
  // 所以外壳不存在「悄悄给自己开权限」的入口，`revoke` 只会让闸门更严。
  Methods.permission.list,
  Methods.permission.revoke
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
