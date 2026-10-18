/**
 * 策略闸门的单元测试。
 *
 * 这是渲染进程与内核之间的那道门，写错的后果是「UI 能直接触发本该被拦下的操作」，
 * 所以这里逐条钉死放行与拦截的行为。
 */

import { describe, expect, it } from 'vitest'

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

  it('拦截白名单之外的方法，并给出原因', () => {
    // workspace.chmod / fs.delete 之类内核根本没实现，也不该被 UI 调用
    const methods = ['core.hello', 'core.shutdown', 'llm.chat', 'agent.run', 'workspace.chmod']

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
})
