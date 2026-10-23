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
  Methods.permission.revoke,

  // M6：终端。五条同样是「用户显式动作」的后果——点「新建终端」才 `create`、
  // 敲键盘才 `write`、拖动面板才 `resize`、点关闭才 `kill`、面板展开时对齐
  // 内核真实的会话表才 `list`。这里放行的是**在 PTY 里跑用户自己敲的命令**，
  // 它不构成「渲染进程能执行任意命令」的新入口：`terminal.create` 不能指定
  // 任意可执行文件以外的东西，也拿不到内核的 workspace 权限提升——命令的
  // 破坏力受制于开终端的那个人（用户），而不是某个脚本自动拼出来的。
  Methods.terminal.create,
  Methods.terminal.write,
  Methods.terminal.resize,
  Methods.terminal.kill,
  Methods.terminal.list,

  // M6：Git。五条同样是「用户显式动作」的后果——打开 Git 视图才 `status` / `branches`、
  // 点某个文件才 `diff`、点「暂存 / 取消暂存」才 `stage`、按「提交」才 `commit`。
  //
  // 这里要交代清楚它比终端弱在哪：`git.stage` / `git.commit` 会**写仓库对象库**，
  // 但两者都不能指定任意命令——参数是路径清单与提交信息，动作集合在协议里是封闭的
  // （没有 `git.checkout` / `git.reset --hard` / `git.push`），所以渲染进程即使被
  // 注入脚本，也拿不到「丢工作区改动」或「往远端推」的能力。工作区之外的路径由内核
  // 侧做越界校验（错误码 7004），不依赖白名单兜底。
  Methods.git.status,
  Methods.git.diff,
  Methods.git.stage,
  Methods.git.commit,
  Methods.git.branches,

  // M7：MCP。五条同样是「用户显式动作」的后果——打开 MCP 视图才 `servers` / `tools`、
  // 点某个 Server 的「连接 / 断开」才 `connect` / `disconnect`、在面板里手写一条参数
  // 并按「执行」才 `call`。
  //
  // 这里要交代清楚它的边界：放行的是**外壳能发起的动作**，不是「绕过权限闸门」的入口。
  // `mcp.call` 与 `agent.start` 的关系是并列而非替代——它只调到「已连接 Server 已注册
  // 的工具」，没有 `mcp.register` / `mcp.config` 之类能自己塞 Server 的方法，Server 清单
  // 始终来自磁盘上的 `mcp.json`（用户自己写的文件）。工具调用的风险判定也不在这里：
  // 同一批工具被注入 agent 注册表后走的是同一条权限闸门，经 UI 直接调用只是把
  // 「这一次」显式落到用户手上，且面板强制行内二次确认（第一次点变「确认执行」）。
  Methods.mcp.servers,
  Methods.mcp.connect,
  Methods.mcp.disconnect,
  Methods.mcp.tools,
  Methods.mcp.call,

  // P1：插件清单与安装。四条同样是「用户显式动作」的后果——打开插件视图才 `list`、
  // 点某个插件看详情才 `status`、填好本地包目录按「安装」才 `install`、按「卸载」才
  // `uninstall`。这四条拿不到「执行插件代码」的能力：`install` 只复制包目录与写账本
  // （内核侧 `plugin_api::install` 全程 `spawn_blocking` 拷文件，**不启动任何进程**），
  // `uninstall` 只删账本与安装目录。
  //
  // P2：装载与握手。三条同样是「用户显式动作」的后果——点某行的「加载」才 `load`、
  // 点「停止」才 `unload`、点「重载」才 `reload`。内核侧（`core/src/plugin_api.rs`）
  // 已把三条换成真实现：起子进程、stdio 帧握手、工具注册/下架、崩溃巡检落账本。
  //
  // 放行的边界要写清楚，否则「白名单里有 load」看起来像「UI 能自己跑任意代码」：
  //
  // - `load` 跑的是**用户自己装进来的包**，路径来自账本里的 `install_dir`；渲染进程
  //   不能指定可执行文件、不能传命令行参数，协议参数只有一个 `plugin_id`。
  // - 能力闸门在内核：清单申请的能力没授齐就返回 `6103` 且**不 spawn 进程**
  //  （docs/14 §4.2 第 4 条）。外壳不参与授权判定，也没有「跳过授权」的入参。
  // - 崩溃后的自动重启也由内核按上限（3 次）自己控；UI 上这两个按钮只是把「用户想
  //   重来一次」这个意思传下去，没有轮询重试的路径。
  //
  // 仍然**没放行的三个**：`call`（P3 的调用网关）、`grant` / `revoke`（P4 的授权面）。
  // 它们在协议方法表里、也还留在 `IMPLEMENTED_METHODS` 中，但内核侧仍是 `6101` 空分支。
  // 白名单一旦提前放行，UI 就能发起一个必然失败、还会被用户当成「插件坏了」的调用。
  // 这正是设计文档 §9 要求的「能力不虚报」：**实现对齐之后再加进来**。
  Methods.plugin.install,
  Methods.plugin.list,
  Methods.plugin.status,
  Methods.plugin.uninstall,
  Methods.plugin.load,
  Methods.plugin.unload,
  Methods.plugin.reload
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
