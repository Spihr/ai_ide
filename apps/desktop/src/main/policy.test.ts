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

  it('拦截白名单之外的方法，并给出原因', () => {
    // core.hello / core.shutdown 是主进程自用的握手与退出；workspace.chmod、agent.run
    // 之类内核根本没实现，也不该被 UI 调用
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
      'core.ping',
      'core.stats',
      'llm.cancel',
      'llm.chat_stream',
      'llm.count_tokens',
      'llm.list_models',
      'model.delete',
      'model.list',
      'model.upsert',
      'provider.delete',
      'provider.list',
      'provider.test',
      'provider.upsert',
      'search.content',
      'search.files',
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
