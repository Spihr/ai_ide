/**
 * 策略闸门的单元测试。
 *
 * 这是渲染进程与内核之间的那道门，写错的后果是「UI 能直接触发本该被拦下的操作」，
 * 所以这里逐条钉死放行与拦截的行为。
 */

import { describe, expect, it } from 'vitest'

import { IMPLEMENTED_METHODS, Methods } from '@ai-ide/shared-types'

import { allowedRendererMethods, checkRendererCall } from './policy'

describe('checkRendererCall', () => {
  it('放行白名单内的只读方法', () => {
    expect(checkRendererCall('core.ping').allowed).toBe(true)
    expect(checkRendererCall('core.stats').allowed).toBe(true)
  })

  it('放行 M1 的工作区与检索方法', () => {
    const methods = [
      'workspace.open',
      'workspace.tree',
      'workspace.read_file',
      'workspace.write_file',
      'workspace.delete',
      'search.files',
      'search.content',
      'search.symbols'
    ]

    for (const method of methods) {
      expect(checkRendererCall(method).allowed, `${method} 应被放行`).toBe(true)
    }
  })

  it('放行 M2 的 Provider / 模型 / 对话方法', () => {
    const methods = [
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
      Methods.llm.countTokens
    ]

    for (const method of methods) {
      expect(checkRendererCall(method).allowed, `${method} 应被放行`).toBe(true)
    }
  })

  it('放行 M4 的 Agent 运行方法', () => {
    const methods = [Methods.agent.start, Methods.agent.stop, Methods.agent.decide]

    for (const method of methods) {
      expect(checkRendererCall(method).allowed, `${method} 应被放行`).toBe(true)
    }
  })

  it('放行 M5 的会话与审计方法', () => {
    const methods = [
      Methods.session.create,
      Methods.session.list,
      Methods.session.get,
      Methods.session.update,
      Methods.session.delete,
      Methods.session.appendMessage,
      Methods.session.messages,
      Methods.session.export,
      Methods.session.fork,
      Methods.checkpoint.create,
      Methods.checkpoint.list,
      Methods.checkpoint.restore,
      Methods.checkpoint.delete,
      Methods.permission.list,
      Methods.permission.revoke,
      Methods.audit.list
    ]

    for (const method of methods) {
      expect(checkRendererCall(method).allowed, `${method} 应被放行`).toBe(true)
    }
  })

  it('放行 M6 的终端方法', () => {
    const methods = [
      Methods.terminal.create,
      Methods.terminal.write,
      Methods.terminal.resize,
      Methods.terminal.kill,
      Methods.terminal.list
    ]

    for (const method of methods) {
      expect(checkRendererCall(method).allowed, `${method} 应被放行`).toBe(true)
    }
  })

  it('放行 M6 的 Git 方法', () => {
    const methods = [
      Methods.git.status,
      Methods.git.diff,
      Methods.git.stage,
      Methods.git.commit,
      Methods.git.branches
    ]

    for (const method of methods) {
      expect(checkRendererCall(method).allowed, `${method} 应被放行`).toBe(true)
    }
  })

  it('放行 M7 的 MCP 方法', () => {
    const methods = [
      Methods.mcp.servers,
      Methods.mcp.connect,
      Methods.mcp.disconnect,
      Methods.mcp.tools,
      Methods.mcp.call
    ]

    for (const method of methods) {
      expect(checkRendererCall(method).allowed, `${method} 应被放行`).toBe(true)
    }
  })

  /*
   * P1 只放行「清单与安装」四条：装/卸/列/看详情都不执行插件代码。
   */
  it('放行 P1 的插件安装与清单方法', () => {
    const methods = [
      Methods.plugin.install,
      Methods.plugin.list,
      Methods.plugin.status,
      Methods.plugin.uninstall
    ]

    for (const method of methods) {
      expect(checkRendererCall(method).allowed, `${method} 应被放行`).toBe(true)
    }
  })

  /*
   * P2 放行「装载与握手」三条：内核侧已是真实现（起子进程 + stdio 帧握手）。
   * 这三条是白名单里唯一会「执行插件代码」的入口，所以放行的前提写在这里：
   * 参数只有 `plugin_id`（不能指定可执行文件/命令行），能力闸门与重启上限都在内核。
   */
  it('放行 P2 的插件装载方法', () => {
    const methods = [Methods.plugin.load, Methods.plugin.unload, Methods.plugin.reload]

    for (const method of methods) {
      expect(checkRendererCall(method).allowed, `${method} 应被放行`).toBe(true)
    }
  })

  /*
   * P3/P4 放行「调用网关」与「授权面」三条：内核侧三条都已从 `6101` 空分支换成
   * 真实现，白名单才跟着放行（设计文档 §9：实现对齐之后再放，不虚报能力）。
   * 放行边界：`call` 只能调插件自己声明过的方法（内核六步定序 + 64 KB 截断），
   * `grant` / `revoke` 只能改本机已装插件的授权位（能力名按清单校验），三者都没有
   * 「指定可执行文件 / 任意协议帧 / 给未安装插件开权限」的入参。
   */
  it('放行 P3/P4 的插件调用与授权方法', () => {
    const methods = [Methods.plugin.call, Methods.plugin.grant, Methods.plugin.revoke]

    for (const method of methods) {
      const decision = checkRendererCall(method)
      expect(decision.allowed, `${method} 应被放行`).toBe(true)
      expect(decision.reason, `${method} 放行时不应带拒绝原因`).toBeUndefined()
    }
  })

  /*
   * P5 放行命令面板的插件命令（`plugin.command`）：内核侧命令网关已实现（切片二），
   * 渲染进程只提交 `plugin_id + command + args`，命令归属、能力、路径、审批、配额
   * 五道闸门都在内核。放行的前提是「命令清单由内核回」，面板不自己编命令名。
   */
  it('放行 P5 的插件命令网关', () => {
    const decision = checkRendererCall(Methods.plugin.command)
    expect(decision.allowed, 'plugin.command 应被放行').toBe(true)
    expect(decision.reason).toBeUndefined()
  })

  /*
   * 反过来钉住「放行的都是已实现方法」：插件十一条协议方法全部进白名单之后，谁再往
   * 协议表里加 `plugin.*`（比如将来的 `plugin.logs`）却没有内核实现，这条会先炸。
   */
  it('插件协议方法已全部放行', () => {
    const allowed = new Set(allowedRendererMethods())
    const pluginMethods = IMPLEMENTED_METHODS.filter((method) => method.startsWith('plugin.'))

    expect(pluginMethods.length).toBe(11)
    for (const method of pluginMethods) {
      expect(allowed.has(method), `${method} 已在内核实现，应被放行`).toBe(true)
    }
  })

  /*
   * 语言服务五条放行：内核侧五个 handler 已接线（能力表 74 → 79），外壳才跟着放。
   * 放行边界写在 `policy.ts` 的注释里：`start` 拿不到「任意 shell 命令」，
   * `request` 的 `method` 是枚举，`sync` / `stop` 不写盘。
   */
  it('放行语言服务五条协议方法', () => {
    const methods = [
      Methods.lsp.start,
      Methods.lsp.stop,
      Methods.lsp.servers,
      Methods.lsp.request,
      Methods.lsp.sync
    ]

    for (const method of methods) {
      const decision = checkRendererCall(method)
      expect(decision.allowed, `${method} 应被放行`).toBe(true)
      expect(decision.reason, `${method} 放行时不应带拒绝原因`).toBeUndefined()
    }
  })

  /*
   * 反过来钉住「放行的都是内核已实现的方法」：五个 `lsp.*` 都在 `IMPLEMENTED` 里，
   * 谁把某个方法退回 `DECLARED` 却忘了收紧白名单，这条会先炸。
   */
  it('语言服务方法已全部实现', () => {
    const lspMethods = IMPLEMENTED_METHODS.filter((method) => method.startsWith('lsp.'))

    expect(lspMethods.length).toBe(5)
  })

  /*
   * MCP 的「配置面」不进白名单：面板只能连 `mcp.json` 里已有的 Server，不能自己
   * 塞一个进去。这条用例钉的就是「UI 拿不到改配置 / 绕过已连接清单」的能力。
   */
  it('不放行 MCP 的配置面方法（Server 清单只能来自 mcp.json）', () => {
    const methods = ['mcp.register', 'mcp.config', 'mcp.upsert', 'mcp.install']

    for (const method of methods) {
      expect(checkRendererCall(method).allowed, `${method} 不应被放行`).toBe(false)
    }
  })

  /*
   * Git 的「危险动作」目前一个都没进协议：推、切分支、`reset --hard`、强制检出。
   * 这条用例是防回归的闸门——将来谁加了 `git.push` 之类的方法，会被这里挡下并提醒
   * 「放行前先想清楚 UI 有没有二次确认」，而不是顺手加进白名单就完事。
   */
  it('不放行协议里不存在的 Git 危险动作', () => {
    const methods = ['git.push', 'git.checkout', 'git.reset', 'git.merge', 'git.rebase']

    for (const method of methods) {
      expect(checkRendererCall(method).allowed, `${method} 不应被放行`).toBe(false)
    }
  })

  it('拦截白名单之外的方法，并给出原因', () => {
    // core.hello / core.shutdown 是主进程自用的握手与退出；workspace.chmod、agent.run
    // 之类内核根本没实现（M4 的三条是 agent.start / stop / decide，没有 agent.run），
    // 也不该被 UI 调用
    const methods = [
      Methods.core.hello,
      Methods.core.shutdown,
      Methods.cancel.request,
      'agent.run',
      'workspace.chmod',
      'mcp.invoke'
    ]

    for (const method of methods) {
      const decision = checkRendererCall(method)
      expect(decision.allowed, `${method} 不应被放行`).toBe(false)
      expect(decision.reason, `${method} 应当给出拒绝原因`).toBeTruthy()
    }
  })

  it('不放行 $/ 前缀的保留方法', () => {
    const decision = checkRendererCall('$/cancelRequest')
    expect(decision.allowed).toBe(false)
    expect(decision.reason).toContain('保留方法')
  })

  it('不放行空方法名', () => {
    expect(checkRendererCall('').allowed).toBe(false)
  })

  it('白名单保持在最小集', () => {
    expect([...allowedRendererMethods()].sort()).toEqual([
      'agent.decide',
      'agent.start',
      'agent.stop',
      'audit.list',
      'checkpoint.create',
      'checkpoint.delete',
      'checkpoint.list',
      'checkpoint.restore',
      'core.ping',
      'core.stats',
      'git.branches',
      'git.commit',
      'git.diff',
      'git.stage',
      'git.status',
      // 切片三：内联编辑只放行 `propose`——写盘仍是 `workspace.write_file`，
      // 内核不越权改文件，外壳也就没必要给它开一个新口子。
      'inline_edit.propose',
      'llm.cancel',
      'llm.chat_stream',
      'llm.count_tokens',
      'llm.list_models',
      // 语言服务：五条都对应「编辑器状态变化」这类显式动作——开文件起服务、
      // 切/存文件同步正文、关掉才 stop、按下跳转才 request。
      'lsp.request',
      'lsp.servers',
      'lsp.start',
      'lsp.stop',
      'lsp.sync',
      'mcp.call',
      'mcp.connect',
      'mcp.disconnect',
      'mcp.servers',
      'mcp.tools',
      'model.delete',
      'model.list',
      'model.upsert',
      'permission.list',
      'permission.revoke',
      'plugin.call',
      'plugin.command',
      'plugin.grant',
      'plugin.install',
      'plugin.list',
      'plugin.load',
      'plugin.reload',
      'plugin.revoke',
      'plugin.status',
      'plugin.uninstall',
      'plugin.unload',
      'provider.delete',
      'provider.list',
      'provider.test',
      'provider.upsert',
      'search.content',
      'search.files',
      'search.symbols',
      'session.append_message',
      'session.create',
      'session.delete',
      'session.export',
      'session.fork',
      'session.get',
      'session.list',
      'session.messages',
      'session.update',
      'terminal.create',
      'terminal.kill',
      'terminal.list',
      'terminal.resize',
      'terminal.write',
      'workspace.close',
      'workspace.create',
      'workspace.delete',
      'workspace.open',
      'workspace.read_file',
      'workspace.rename',
      'workspace.reveal',
      'workspace.tree',
      'workspace.write_file'
    ])
  })

  it('白名单只能是已实现方法的子集（放行内核没实现的方法 = 一片 -32601）', () => {
    const implemented = new Set<string>(IMPLEMENTED_METHODS)

    for (const method of allowedRendererMethods()) {
      expect(implemented.has(method), `${method} 未列在 IMPLEMENTED_METHODS 中`).toBe(true)
    }
  })
})
