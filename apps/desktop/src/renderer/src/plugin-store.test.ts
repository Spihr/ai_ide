/**
 * P1 渲染侧插件状态测试。
 *
 * 与 `mcp-store.test.ts` 同一套路：不渲染 React，直接驱动动作函数，用假的
 * `window.aiide` 顶替 IPC，只断言「发了什么请求」与「状态怎么变」。
 *
 * 钉住的是这个面板最容易错、错了却像「内核没生效」的几条：
 *
 * - 路径为空时**不发请求**（错误必须落在本地）；
 * - 安装成功后必须重拉清单并选中新插件，否则用户看到的是旧清单；
 * - `6107` 的 details 必须原样留在 state 里，**不能被随后的刷新擦掉**；
 * - 卸载 `removed_dir = false` 不算失败（账本已经清了），不能报红；
 * - 卸载掉当前选中的插件后，选中态与详情要一起清掉。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  Methods,
  type PluginStatusResult,
  type PluginSummary
} from '@ai-ide/shared-types'

import type { AiIdeApi } from '../../shared/ipc'
import { encodeRpcErrorPayload } from '../../shared/ipc'
import {
  dismissPluginError,
  getPluginState,
  initPlugins,
  installPlugin,
  refreshPlugins,
  resetPluginState,
  selectPlugin,
  setInstallPath,
  uninstallPlugin
} from './plugin-store'

interface Call {
  method: string
  params: Record<string, unknown> | undefined
}

interface Harness {
  calls: Call[]
  /** 让下一次请求失败（可指定方法），按内核的形态带 data。 */
  failNext: (message: string, options?: { method?: string; data?: unknown }) => void
  setPlugins: (plugins: PluginSummary[]) => void
  /** 卸载时「安装目录删没删掉」：`false` 模拟目录被占用。 */
  setRemovedDir: (value: boolean) => void
  /** 计数：某个方法被调了几次。 */
  count: (method: string) => number
}

function plugin(id: string, patch: Partial<PluginSummary> = {}): PluginSummary {
  return {
    id,
    name: id,
    version: '0.1.0',
    description: '示例插件',
    state: 'needs_grant',
    capabilities: ['fs:read'],
    granted_capabilities: [],
    tool_count: 1,
    last_error: undefined,
    ...patch
  }
}

function installHarness(): Harness {
  const calls: Call[] = []
  let failure: { message: string; method: string | null } | null = null
  let plugins: PluginSummary[] = [plugin('hello.world', { name: 'Hello' })]
  let removedDir = true

  const api = {
    request: async (method: string, params?: unknown) => {
      calls.push({ method, params: params as Record<string, unknown> | undefined })
      if (failure && (failure.method === null || failure.method === method)) {
        const text = failure.message
        failure = null
        throw new Error(text)
      }
      const p = (params ?? {}) as Record<string, unknown>
      switch (method) {
        case Methods.plugin.list:
          // 回包是快照：调用方不能持有内部数组的引用
          return { plugins: plugins.map((item) => ({ ...item })) }
        case Methods.plugin.install: {
          const id = 'fresh.plugin'
          plugins = [...plugins, plugin(id, { name: 'Fresh', version: '1.0.0' })]
          return {
            plugin: plugins[plugins.length - 1],
            replaced: false,
            installed_dir: 'C:\\ud\\plugins\\fresh'
          }
        }
        case Methods.plugin.status: {
          const id = String(p.plugin_id)
          const found = plugins.find((item) => item.id === id)
          if (!found) throw new Error('[6101] plugin not found')
          const detail: PluginStatusResult = {
            plugin: { ...found },
            tools: [
              {
                name: 'read',
                namespaced_name: `plugin__${id}__read`,
                description: '读文件',
                risk: 'read',
                input_schema: { type: 'object' }
              }
            ],
            commands: [{ name: 'hello', title: '打招呼', description: '打个招呼' }]
          }
          return detail
        }
        case Methods.plugin.uninstall: {
          const id = String(p.plugin_id)
          const existed = plugins.some((item) => item.id === id)
          if (!existed) throw new Error('[6101] plugin not found')
          plugins = plugins.filter((item) => item.id !== id)
          return { plugin_id: id, removed_dir: removedDir }
        }
        default:
          throw new Error(`测试收到了意外方法：${method}`)
      }
    }
  } as unknown as AiIdeApi

  vi.stubGlobal('window', { aiide: api })

  return {
    calls,
    failNext: (message, options) => {
      failure = { message, method: options?.method ?? null }
    },
    setPlugins: (next) => {
      plugins = next
    },
    setRemovedDir: (value) => {
      removedDir = value
    },
    count: (method) => calls.filter((call) => call.method === method).length
  }
}

beforeEach(() => {
  resetPluginState()
  vi.unstubAllGlobals()
})

describe('拉清单', () => {
  it('拉到清单后 loading 复位', async () => {
    const harness = installHarness()

    const ok = await refreshPlugins()
    expect(ok).toBe(true)
    expect(harness.count(Methods.plugin.list)).toBe(1)

    const state = getPluginState()
    expect(state.loading).toBe(false)
    expect(state.plugins.map((item) => item.id)).toEqual(['hello.world'])
    expect(state.error).toBeNull()
  })

  it('拉不到时保留上一次的清单（空白列表会让人以为插件全丢了）', async () => {
    const harness = installHarness()
    await refreshPlugins()

    harness.failNext('[5003] ledger io failed', { method: Methods.plugin.list })
    const ok = await refreshPlugins()

    expect(ok).toBe(false)
    const state = getPluginState()
    expect(state.loading).toBe(false)
    expect(state.plugins.map((item) => item.id)).toEqual(['hello.world'])
    expect(state.error).toContain('[5003]')
  })

  it('刷新不擦掉还没读的安装错误', async () => {
    const harness = installHarness()
    harness.failNext('[5003] boom', { method: Methods.plugin.list })
    await refreshPlugins()
    expect(getPluginState().error).toContain('[5003]')

    // 用户点「刷新」看清单：错误块还在，等他自己点「知道了」
    await refreshPlugins()
    expect(getPluginState().error).toContain('[5003]')
    dismissPluginError()
    expect(getPluginState().error).toBeNull()
  })

  it('initPlugins 只在没有进行中的请求时触发一次', async () => {
    const harness = installHarness()
    initPlugins()
    initPlugins()
    await Promise.resolve()
    await Promise.resolve()
    expect(harness.count(Methods.plugin.list)).toBe(1)
  })
})

describe('安装', () => {
  it('路径为空时不发请求，错误落在本地', async () => {
    const harness = installHarness()
    setInstallPath('   ')

    const ok = await installPlugin()
    expect(ok).toBe(false)
    expect(harness.calls).toHaveLength(0)

    const state = getPluginState()
    expect(state.installing).toBe(false)
    expect(state.error).toContain('绝对路径')
  })

  it('成功后重拉清单、选中新插件、清空输入框', async () => {
    const harness = installHarness()
    setInstallPath('E:\\my-plugins\\fresh')

    const ok = await installPlugin()
    expect(ok).toBe(true)

    // 顺序：install → list → status
    expect(harness.calls.map((call) => call.method)).toEqual([
      Methods.plugin.install,
      Methods.plugin.list,
      Methods.plugin.status
    ])
    expect(harness.calls[0]?.params).toEqual({ path: 'E:\\my-plugins\\fresh' })
    expect(harness.calls[2]?.params).toEqual({ plugin_id: 'fresh.plugin' })

    const state = getPluginState()
    expect(state.installing).toBe(false)
    expect(state.installPath).toBe('')
    expect(state.plugins.map((item) => item.id)).toEqual(['hello.world', 'fresh.plugin'])
    expect(state.selectedId).toBe('fresh.plugin')
    expect(state.detail?.tools).toHaveLength(1)
    // P1 的边界要在提示里说清楚：装完没有进程
    expect(state.hint).toContain('未启动进程')
  })

  it('6107 失败时把 details 逐条留在 state 里', async () => {
    const harness = installHarness()
    setInstallPath('E:\\bad')
    harness.failNext(
      encodeRpcErrorPayload({
        code: 6107,
        message: '[6107] manifest invalid',
        data: {
          details: [
            { code: 6107, path: '$.id', message: 'id 非法' },
            { code: 6107, path: '$.version', message: '缺少 version' },
            { code: 6104, path: '$.pluginApi', message: 'API 不兼容' },
            { code: 6108, path: '$.tools[0].name', message: '工具撞名' }
          ]
        }
      }),
      { method: Methods.plugin.install }
    )

    const ok = await installPlugin()
    expect(ok).toBe(false)

    const state = getPluginState()
    expect(state.installing).toBe(false)
    expect(state.error).toContain('[6107]')
    expect(state.errorDetails).toHaveLength(4)
    expect(state.errorDetails.map((issue) => issue.path)).toEqual([
      '$.id',
      '$.version',
      '$.pluginApi',
      '$.tools[0].name'
    ])
    // 失败不重拉清单：内核根本没装进去
    expect(harness.count(Methods.plugin.list)).toBe(0)
    // 输入框保留原文：用户得照着改
    expect(state.installPath).toBe('E:\\bad')
  })

  it('安装失败后 busy 标记必须复位（否则按钮永久禁用）', async () => {
    const harness = installHarness()
    setInstallPath('E:\\bad')
    harness.failNext('[5003] copy failed', { method: Methods.plugin.install })

    await installPlugin()
    expect(getPluginState().installing).toBe(false)
  })
})

describe('选中与详情', () => {
  it('同一 id 不重复拉详情', async () => {
    const harness = installHarness()
    await refreshPlugins()

    await selectPlugin('hello.world')
    await selectPlugin('hello.world')

    expect(harness.count(Methods.plugin.status)).toBe(1)
    expect(getPluginState().detail?.commands[0]?.name).toBe('hello')
  })

  it('传 null 收起详情，不发请求', async () => {
    const harness = installHarness()
    await refreshPlugins()
    await selectPlugin('hello.world')
    await selectPlugin(null)

    const state = getPluginState()
    expect(state.selectedId).toBeNull()
    expect(state.detail).toBeNull()
    expect(harness.count(Methods.plugin.status)).toBe(1)
  })

  it('详情拉不到时选中态保留、错误落下', async () => {
    const harness = installHarness()
    await refreshPlugins()
    harness.failNext('[6101] plugin not found', { method: Methods.plugin.status })

    await selectPlugin('hello.world')

    const state = getPluginState()
    expect(state.selectedId).toBe('hello.world')
    expect(state.detail).toBeNull()
    expect(state.detailLoading).toBe(false)
    expect(state.error).toContain('[6101]')
  })
})

describe('卸载', () => {
  it('卸载后条目消失、选中态一起清掉', async () => {
    const harness = installHarness()
    await refreshPlugins()
    await selectPlugin('hello.world')

    const ok = await uninstallPlugin('hello.world')
    expect(ok).toBe(true)

    const state = getPluginState()
    expect(state.busyPlugin).toBeNull()
    expect(state.plugins).toEqual([])
    expect(state.selectedId).toBeNull()
    expect(state.detail).toBeNull()
    expect(state.hint).toContain('安装目录已删除')
    expect(harness.count(Methods.plugin.list)).toBe(2)
  })

  it('卸载其他插件时保留当前选中项', async () => {
    const harness = installHarness()
    harness.setPlugins([plugin('hello.world'), plugin('other.one')])
    await refreshPlugins()
    await selectPlugin('hello.world')

    await uninstallPlugin('other.one')

    const state = getPluginState()
    expect(state.selectedId).toBe('hello.world')
    expect(state.plugins.map((item) => item.id)).toEqual(['hello.world'])
  })

  it('removed_dir = false 不算失败：账本已清，只说目录没删', async () => {
    const harness = installHarness()
    await refreshPlugins()
    harness.setRemovedDir(false)

    const ok = await uninstallPlugin('hello.world')
    expect(ok).toBe(true)

    const state = getPluginState()
    expect(state.error).toBeNull()
    expect(state.hint).toContain('没能删掉')
    expect(state.hint).toContain('手工清理')
  })

  it('卸载失败时 busy 复位并落错误', async () => {
    const harness = installHarness()
    await refreshPlugins()
    harness.failNext('[6101] plugin not found', { method: Methods.plugin.uninstall })

    const ok = await uninstallPlugin('hello.world')
    expect(ok).toBe(false)

    const state = getPluginState()
    expect(state.busyPlugin).toBeNull()
    expect(state.error).toContain('[6101]')
    // 失败不重拉清单：内核没动过账本
    expect(harness.count(Methods.plugin.list)).toBe(1)
  })
})
