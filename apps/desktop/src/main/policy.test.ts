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
      'search.content'
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

  it('不放行 P3/P4 才落地的插件方法（白名单不虚报能力）', () => {
    const methods = [Methods.plugin.call, Methods.plugin.grant, Methods.plugin.revoke]

    for (const method of methods) {
      const decision = checkRendererCall(method)
      expect(decision.allowed, `${method} 不应被放行`).toBe(false)
      expect(decision.reason, `${method} 应当给出拒绝原因`).toBeTruthy()
    }
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
      'llm.cancel',
      'llm.chat_stream',
      'llm.count_tokens',
      'llm.list_models',
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
      'plugin.install',
      'plugin.list',
      'plugin.load',
      'plugin.reload',
      'plugin.status',
      'plugin.uninstall',
      'plugin.unload',
      'provider.delete',
      'provider.list',
      'provider.test',
      'provider.upsert',
      'search.content',
      'search.files',
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
