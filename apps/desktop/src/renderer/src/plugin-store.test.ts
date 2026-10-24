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
 *
 * P2 加的装载三条（`load` / `unload` / `reload`）钉的是另一类错：**失败也要刷新**。
 * 内核先把 `crashed` / `needs_grant` 与 `last_error` 写进账本再返回错误，store 若
 * 只记 error 不重拉清单，面板就会停在旧状态——用户看到「加载失败」却还是「已停用」，
 * 崩溃原因和重启次数全都读不到。
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
  cancelRevoke,
  dismissPluginError,
  getPluginState,
  grantCapabilities,
  initPlugins,
  installPlugin,
  loadPlugin,
  refreshPlugins,
  reloadPlugin,
  resetPluginState,
  revokeCapability,
  selectPlugin,
  setInstallPath,
  uninstallPlugin,
  unloadPlugin
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
  /** 下一次 load/reload 的结果：成功起进程，还是崩了（内核落 crashed + last_error）。 */
  setLoadOutcome: (value: 'ready' | 'crashed') => void
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
    restarts: 0,
    ...patch
  }
}

function installHarness(): Harness {
  const calls: Call[] = []
  let failure: { message: string; method: string | null } | null = null
  let plugins: PluginSummary[] = [plugin('hello.world', { name: 'Hello' })]
  let removedDir = true
  let loadOutcome: 'ready' | 'crashed' = 'ready'

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
        case Methods.plugin.load:
        case Methods.plugin.reload: {
          const id = String(p.plugin_id)
          const index = plugins.findIndex((item) => item.id === id)
          if (index < 0) throw new Error('[6101] plugin not found')
          const current = plugins[index] as PluginSummary
          if (loadOutcome === 'crashed') {
            // 内核的真实顺序：先落账本（crashed + last_error + restarts），再返回 6102
            plugins[index] = {
              ...current,
              state: 'crashed',
              last_error: '子进程退出码 1：入口文件不存在',
              restarts: current.restarts + 1
            }
            throw new Error('[6102] 插件已崩溃，工具已下架')
          }
          plugins[index] = { ...current, state: 'ready', last_error: undefined }
          return {
            plugin: { ...current, state: 'ready', last_error: undefined },
            tools: [
              {
                name: 'read',
                namespaced_name: `plugin__${id}__read`,
                description: '读文件',
                risk: 'read',
                input_schema: { type: 'object' }
              }
            ]
          }
        }
        case Methods.plugin.unload: {
          const id = String(p.plugin_id)
          const index = plugins.findIndex((item) => item.id === id)
          if (index < 0) throw new Error('[6101] plugin not found')
          plugins[index] = {
            ...(plugins[index] as PluginSummary),
            state: 'disabled',
            last_error: undefined
          }
          return { plugin_id: id, removed_tools: 1 }
        }
        case Methods.plugin.grant: {
          const id = String(p.plugin_id)
          const index = plugins.findIndex((item) => item.id === id)
          if (index < 0) throw new Error('[6101] plugin not found')
          const current = plugins[index] as PluginSummary
          const asked = (p.capabilities ?? []) as string[]
          const wanted = asked.length > 0 ? asked : current.capabilities
          const merged = [
            ...current.granted_capabilities,
            ...wanted.filter((item) => !current.granted_capabilities.includes(item))
          ]
          const complete = current.capabilities.every((item) => merged.includes(item))
          plugins[index] = {
            ...current,
            granted_capabilities: merged,
            state: complete ? 'disabled' : current.state
          }
          return { plugin: { ...plugins[index] } }
        }
        case Methods.plugin.revoke: {
          const id = String(p.plugin_id)
          const index = plugins.findIndex((item) => item.id === id)
          if (index < 0) throw new Error('[6101] plugin not found')
          const current = plugins[index] as PluginSummary
          const asked = (p.capabilities ?? []) as string[]
          const granted = current.granted_capabilities.filter((item) => !asked.includes(item))
          // 内核判定：撤掉关键能力必然连带卸载，进程停 + 工具下架 + 退回 needs_grant
          const unloaded = asked.some((item) => ['fs.write', 'net.fetch', 'llm.chat'].includes(item))
          plugins[index] = {
            ...current,
            granted_capabilities: granted,
            state: unloaded ? 'needs_grant' : current.state,
            tool_count: unloaded ? 0 : current.tool_count
          }
          return { plugin: { ...plugins[index] }, unloaded }
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
    setLoadOutcome: (value) => {
      loadOutcome = value
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
    // 边界要在提示里说清楚：装完没有进程，下一步是点「加载」
    expect(state.hint).toContain('进程未启动')
    expect(state.hint).toContain('加载')
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

describe('装载与停止', () => {
  it('加载成功后重拉清单与详情，状态变 ready', async () => {
    const harness = installHarness()
    harness.setLoadOutcome('ready')
    await refreshPlugins()
    // 选中它，详情才会跟着刷新（fetchPluginDetail 只更新当前选中行）
    await selectPlugin('hello.world')

    const ok = await loadPlugin('hello.world')
    expect(ok).toBe(true)

    expect(harness.calls.map((call) => call.method)).toEqual([
      Methods.plugin.list, // 前置 refresh
      Methods.plugin.status, // 前置选中
      Methods.plugin.load,
      Methods.plugin.list,
      Methods.plugin.status
    ])
    expect(harness.calls[2]?.params).toEqual({ plugin_id: 'hello.world' })

    const state = getPluginState()
    expect(state.busyPlugin).toBeNull()
    expect(state.busyAction).toBeNull()
    expect(state.plugins[0]?.state).toBe('ready')
    // 工具数用返回值里的真实长度，不猜
    expect(state.hint).toContain('已装载')
    expect(state.hint).toContain('1 个工具')
  })

  it('加载崩溃：错误落下、清单刷新到 crashed、原因与重启次数读得到', async () => {
    const harness = installHarness()
    harness.setLoadOutcome('crashed')
    await refreshPlugins()
    await selectPlugin('hello.world')

    const ok = await loadPlugin('hello.world')
    expect(ok).toBe(false)

    const state = getPluginState()
    expect(state.busyPlugin).toBeNull()
    expect(state.busyAction).toBeNull()
    // 报错文案带协议码和内核原话，用户能直接贴给我们
    expect(state.error).toContain('[6102]')
    expect(state.error).toContain('工具已下架')
    // 关键：失败分支也必须重拉清单，否则面板停在 needs_grant，看不到崩溃
    expect(harness.count(Methods.plugin.list)).toBe(2)
    const crashed = state.plugins[0]
    expect(crashed?.state).toBe('crashed')
    expect(crashed?.last_error).toContain('入口文件不存在')
    expect(crashed?.restarts).toBe(1)
    // 详情也要跟上：明细里是同一个账本状态
    expect(state.detail?.plugin.state).toBe('crashed')
  })

  it('停止：保留账本与安装目录，只说下架了几个工具', async () => {
    const harness = installHarness()
    harness.setPlugins([plugin('hello.world', { state: 'ready' })])
    await refreshPlugins()

    const ok = await unloadPlugin('hello.world')
    expect(ok).toBe(true)
    expect(harness.calls.map((call) => call.method)).toContain(Methods.plugin.unload)

    const state = getPluginState()
    expect(state.busyPlugin).toBeNull()
    expect(state.plugins[0]?.state).toBe('disabled')
    expect(state.hint).toContain('已停止')
    expect(state.hint).toContain('1 个工具')
    expect(state.hint).toContain('保留')
    // 停止不是卸载：不该调 uninstall
    expect(harness.count(Methods.plugin.uninstall)).toBe(0)
  })

  it('重载走 reload，不先手动 unload（内核内部自己停）', async () => {
    const harness = installHarness()
    harness.setPlugins([plugin('hello.world', { state: 'ready' })])
    await refreshPlugins()

    const ok = await reloadPlugin('hello.world')
    expect(ok).toBe(true)

    expect(harness.count(Methods.plugin.reload)).toBe(1)
    expect(harness.count(Methods.plugin.unload)).toBe(0)
    expect(getPluginState().hint).toContain('已重载')
  })

  it('装载动作互斥：正在忙时再点不发第二个请求', async () => {
    const harness = installHarness()
    await refreshPlugins()

    // 不 await 第一下，让它停在「忙」的状态里
    const first = loadPlugin('hello.world')
    const second = await loadPlugin('hello.world')
    expect(second).toBe(false)
    await first

    expect(harness.count(Methods.plugin.load)).toBe(1)
  })
})

/**
 * P4 授权/撤销。
 *
 * 这一组的重点是**撤销的两段式**：第一次点只上膛、绝不发请求。内核撤关键能力会
 * 连带卸载插件（进程停、工具下架），点错没法反悔，所以「问一声」必须发生在
 * 请求之前，而且上膛态要跟着 `(插件, 能力)` 走。
 */
describe('能力授权与撤销', () => {
  function credPlugin(): PluginSummary {
    return plugin('demo.hello', {
      name: 'Demo Hello',
      capabilities: ['fs.read', 'fs.write'],
      granted_capabilities: [],
      state: 'needs_grant'
    })
  }

  it('全部授权：只传 plugin_id，成功后拉清单与详情、状态离开 needs_grant', async () => {
    const harness = installHarness()
    harness.setPlugins([credPlugin()])
    await refreshPlugins()
    await selectPlugin('demo.hello')

    const ok = await grantCapabilities('demo.hello')
    expect(ok).toBe(true)

    // 空数组就是内核的「全授」语义（`wanted.is_empty()` → 取清单声明），不要自己展开成清单
    const grant = harness.calls.find((call) => call.method === Methods.plugin.grant)
    expect(grant?.params).toEqual({ plugin_id: 'demo.hello', capabilities: [] })

    const state = getPluginState()
    expect(state.busyPlugin).toBeNull()
    expect(state.busyAction).toBeNull()
    expect(state.plugins[0]?.granted_capabilities).toEqual(['fs.read', 'fs.write'])
    expect(state.plugins[0]?.state).toBe('disabled')
    expect(state.detail?.plugin.granted_capabilities).toEqual(['fs.read', 'fs.write'])
    // 授权不拉起进程：提示必须把下一步指到「加载」
    expect(state.hint).toContain('加载')
  })

  it('逐项授权：只授予点名那一项，剩下没授的要明说还差几项', async () => {
    const harness = installHarness()
    harness.setPlugins([credPlugin()])
    await refreshPlugins()

    const ok = await grantCapabilities('demo.hello', ['fs.read'])
    expect(ok).toBe(true)

    const grant = harness.calls.find((call) => call.method === Methods.plugin.grant)
    expect(grant?.params).toEqual({ plugin_id: 'demo.hello', capabilities: ['fs.read'] })

    const state = getPluginState()
    expect(state.plugins[0]?.granted_capabilities).toEqual(['fs.read'])
    expect(state.plugins[0]?.state).toBe('needs_grant')
    expect(state.hint).toContain('还差 1 项')
  })

  it('全空白的授权请求不发出去：错误落在本地', async () => {
    const harness = installHarness()
    harness.setPlugins([credPlugin()])
    await refreshPlugins()

    const ok = await grantCapabilities('demo.hello', ['  ', ''])
    expect(ok).toBe(false)
    expect(harness.count(Methods.plugin.grant)).toBe(0)
    expect(getPluginState().error).toContain('不能是空字符串')
  })

  it('授权被拒：busy 复位、错误留下，并重拉清单对齐账本', async () => {
    const harness = installHarness()
    harness.setPlugins([credPlugin()])
    await refreshPlugins()
    harness.failNext('[6106] capability not requested', { method: Methods.plugin.grant })

    const ok = await grantCapabilities('demo.hello', ['net.fetch'])
    expect(ok).toBe(false)

    const state = getPluginState()
    expect(state.busyPlugin).toBeNull()
    expect(state.busyAction).toBeNull()
    expect(state.error).toContain('[6106]')
    // 失败分支也刷新：别的窗口可能刚改过账本
    expect(harness.count(Methods.plugin.list)).toBe(2)
  })

  it('撤销第一下只上膛：不发请求、不动账本', async () => {
    const harness = installHarness()
    harness.setPlugins([credPlugin()])
    await refreshPlugins()

    const ok = await revokeCapability('demo.hello', 'fs.write')
    expect(ok).toBe(false)
    expect(harness.count(Methods.plugin.revoke)).toBe(0)

    const state = getPluginState()
    expect(state.pendingRevoke).toEqual({ pluginId: 'demo.hello', capability: 'fs.write' })
    expect(state.error).toBeNull()
  })

  it('撤销第二下才发请求；关键能力连带卸载后提示要说清后果', async () => {
    const harness = installHarness()
    harness.setPlugins([
      plugin('demo.hello', {
        name: 'Demo Hello',
        capabilities: ['fs.read', 'fs.write'],
        granted_capabilities: ['fs.read', 'fs.write'],
        state: 'disabled'
      })
    ])
    await refreshPlugins()
    await selectPlugin('demo.hello')

    await revokeCapability('demo.hello', 'fs.write')
    const ok = await revokeCapability('demo.hello', 'fs.write')
    expect(ok).toBe(true)

    const revoke = harness.calls.find((call) => call.method === Methods.plugin.revoke)
    expect(revoke?.params).toEqual({ plugin_id: 'demo.hello', capabilities: ['fs.write'] })

    const state = getPluginState()
    expect(state.pendingRevoke).toBeNull()
    expect(state.busyPlugin).toBeNull()
    expect(state.plugins[0]?.granted_capabilities).toEqual(['fs.read'])
    expect(state.plugins[0]?.state).toBe('needs_grant')
    expect(state.hint).toContain('连带卸载')
    expect(state.hint).toContain('待授权')
  })

  it('撤非关键能力不说卸载：插件仍在跑，只是下一次调用被拒', async () => {
    const harness = installHarness()
    harness.setPlugins([
      plugin('demo.hello', {
        capabilities: ['fs.read', 'fs.write'],
        granted_capabilities: ['fs.read', 'fs.write'],
        state: 'ready'
      })
    ])
    await refreshPlugins()

    await revokeCapability('demo.hello', 'fs.read')
    const ok = await revokeCapability('demo.hello', 'fs.read')
    expect(ok).toBe(true)

    const state = getPluginState()
    expect(state.plugins[0]?.state).toBe('ready')
    expect(state.hint).not.toContain('连带卸载')
    expect(state.hint).toContain('拒绝')
  })

  it('上膛态跟着能力走：点了 A 再点 B，确认撤的是 B', async () => {
    const harness = installHarness()
    harness.setPlugins([
      plugin('demo.hello', {
        capabilities: ['fs.read', 'fs.write'],
        granted_capabilities: ['fs.read', 'fs.write'],
        state: 'ready'
      })
    ])
    await refreshPlugins()

    await revokeCapability('demo.hello', 'fs.write')
    await revokeCapability('demo.hello', 'fs.read')
    expect(getPluginState().pendingRevoke).toEqual({
      pluginId: 'demo.hello',
      capability: 'fs.read'
    })

    // 这一下才是真撤，撤的是 fs.read；fs.write 仍在上膛位置之外，没被动过
    const ok = await revokeCapability('demo.hello', 'fs.read')
    expect(ok).toBe(true)
    expect(harness.count(Methods.plugin.revoke)).toBe(1)
    expect(getPluginState().plugins[0]?.granted_capabilities).toEqual(['fs.write'])
  })

  it('cancelRevoke 收起上膛态，之后再点仍需重新确认', async () => {
    const harness = installHarness()
    harness.setPlugins([
      plugin('demo.hello', {
        capabilities: ['fs.write'],
        granted_capabilities: ['fs.write'],
        state: 'ready'
      })
    ])
    await refreshPlugins()

    await revokeCapability('demo.hello', 'fs.write')
    cancelRevoke()
    expect(getPluginState().pendingRevoke).toBeNull()

    const ok = await revokeCapability('demo.hello', 'fs.write')
    expect(ok).toBe(false)
    expect(harness.count(Methods.plugin.revoke)).toBe(0)
  })

  it('换选中插件会清掉上膛态：别让用户在 A 上的确认落到 B 上', async () => {
    const harness = installHarness()
    harness.setPlugins([plugin('demo.hello', { capabilities: ['fs.write'] }), plugin('other.one')])
    await refreshPlugins()

    await revokeCapability('demo.hello', 'fs.write')
    await selectPlugin('other.one')

    expect(getPluginState().pendingRevoke).toBeNull()
  })

  it('撤销失败：上膛态清掉、错误留下，不让用户停在一个假确认里', async () => {
    const harness = installHarness()
    harness.setPlugins([
      plugin('demo.hello', { capabilities: ['fs.write'], granted_capabilities: ['fs.write'] })
    ])
    await refreshPlugins()
    harness.failNext('[6106] capability not granted', { method: Methods.plugin.revoke })

    await revokeCapability('demo.hello', 'fs.write')
    const ok = await revokeCapability('demo.hello', 'fs.write')
    expect(ok).toBe(false)

    const state = getPluginState()
    expect(state.pendingRevoke).toBeNull()
    expect(state.busyPlugin).toBeNull()
    expect(state.error).toContain('[6106]')
  })
})
