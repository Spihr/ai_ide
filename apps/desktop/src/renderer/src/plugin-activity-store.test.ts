/**
 * P5 切片七：插件活动（`plugin.log` / `plugin.progress` / `plugin.state_changed`）的
 * 渲染侧归并与筛选测试。
 *
 * 不渲染 React，直接驱动 `receive*` 与纯函数 `mergePluginActivity`，只断言「列表长
 * 什么样」与「有没有去重拉清单」。钉住的是这个面板最容易错、错了却像「插件没在跑」
 * 的五条：
 *
 * - **倒序**：最新的一条在最上面（按 `seq` 而不是时间戳——同毫秒的批量事件也要有
 *   稳定顺序）；
 * - **进度合并**：同插件同 token 只占一行，百分比被后来者覆盖，而不是刷出一堆行；
 * - **上限裁剪**：超过 500 条丢尾部，并把丢了多少单独计数（不能默默少一截）；
 * - **`state_changed` 触发刷新**：状态变了清单就过期了，只追加一行会让「活动里
 *   写着 crashed、列表里还写着已就绪」自相矛盾；
 * - **级别筛选只作用于日志**：进度与状态被门槛挡掉，等于把「在跑什么 / 崩没崩」
 *   一起藏了。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'

import { Methods, type PluginLogLevel } from '@ai-ide/shared-types'

import type { AiIdeApi } from '../../shared/ipc'
import {
  MAX_PLUGIN_ACTIVITY,
  activeProgressCount,
  clearPluginActivity,
  getPluginActivityState,
  mergePluginActivity,
  receivePluginLog,
  receivePluginProgress,
  receivePluginStateChanged,
  resetPluginActivity,
  setPluginActivityLevel,
  setPluginActivityOpen,
  visiblePluginActivity,
  type PluginActivityEntry
} from './plugin-activity-store'
import { resetPluginState } from './plugin-store'

function log(pluginId: string, message: string, level: PluginLogLevel = 'info'): void {
  receivePluginLog({ pluginId, level, message })
}

function progress(
  pluginId: string,
  token: string | null,
  percent: number | null,
  message = ''
): void {
  receivePluginProgress({ pluginId, token, percent, message, truncated: false })
}

/** 当前列表快照。 */
function entries(): PluginActivityEntry[] {
  return getPluginActivityState().entries
}

/**
 * 取第 `index` 条并断言存在。
 *
 * `noUncheckedIndexedAccess` 下裸写 `entries[0]` 过不了 typecheck，而这里每次
 * 取的都是「刚写进去的那条」，越界就是用例本身写错了，直接炸比 `!` 更诚实。
 */
function at(index: number, list: PluginActivityEntry[] = entries()): PluginActivityEntry {
  const entry = list[index]
  if (!entry) throw new Error(`活动列表没有第 ${index} 条（共 ${list.length} 条）`)
  return entry
}

/** 假 IPC：只认 `plugin.list`，返回空清单（`state_changed` 的刷新会走到这里）。 */
function stubIpc(): { calls: string[] } {
  const calls: string[] = []
  const api = {
    request: async (method: string) => {
      calls.push(method)
      if (method === Methods.plugin.list) return { plugins: [] }
      throw new Error(`测试收到了意外方法：${method}`)
    }
  } as unknown as AiIdeApi
  vi.stubGlobal('window', { aiide: api })
  return { calls }
}

/** 等 `state_changed` 里那条 `void refreshPlugins()` 落地。 */
async function flush(): Promise<void> {
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
}

beforeEach(() => {
  resetPluginActivity()
  resetPluginState()
  vi.unstubAllGlobals()
})

describe('倒序', () => {
  it('最新的日志排在最前面', () => {
    log('hello', '第一条')
    log('hello', '第二条')
    log('hello', '第三条')

    expect(entries().map((entry) => entry.message)).toEqual(['第三条', '第二条', '第一条'])
  })

  it('同毫秒内仍按到达顺序倒序（靠自增 seq，不靠时间戳）', () => {
    log('a', 'a1')
    log('b', 'b1')
    log('a', 'a2')

    expect(entries().map((entry) => entry.message)).toEqual(['a2', 'b1', 'a1'])
    // seq 严格递增，倒序列表里必然是递减序列
    expect(at(0).seq).toBeGreaterThan(at(1).seq)
    expect(at(1).seq).toBeGreaterThan(at(2).seq)
  })

  it('日志 / 进度 / 状态混在一起也按到达顺序排', () => {
    log('a', '日志')
    progress('a', 'scan', 10)
    receivePluginStateChanged({ pluginId: 'a', state: 'ready' })

    expect(entries().map((entry) => entry.kind)).toEqual(['state', 'progress', 'log'])
  })
})

describe('进度合并', () => {
  it('同插件同 token 覆盖同一行，而不是新增一行', () => {
    progress('a', 'scan', 10, '开始扫描')
    progress('a', 'scan', 40, '扫描中')
    progress('a', 'scan', 100, '完成')

    expect(entries()).toHaveLength(1)
    expect(at(0).kind).toBe('progress')
    expect(at(0).percent).toBe(100)
    expect(at(0).message).toBe('完成')
    expect(at(0).key).toBe('progress:a:scan')
  })

  it('合并后的行会被抬到队首（它刚刚更新过）', () => {
    progress('a', 'scan', 10)
    log('a', '中间来一条日志')
    progress('a', 'scan', 60)

    expect(entries()).toHaveLength(2)
    expect(at(0).kind).toBe('progress')
    expect(at(0).percent).toBe(60)
    expect(at(1).message).toBe('中间来一条日志')
  })

  it('不同 token 或不同插件各占一行', () => {
    progress('a', 'scan', 10)
    progress('a', 'index', 10)
    progress('b', 'scan', 10)

    expect(entries()).toHaveLength(3)
    expect(activeProgressCount()).toBe(3)
  })

  it('token 为 null 时退化为独立行（宁可多一行也不合并两次无关任务）', () => {
    progress('a', null, 10)
    progress('a', null, 90)

    expect(entries()).toHaveLength(2)
    expect(entries().map((entry) => entry.percent)).toEqual([90, 10])
  })

  it('百分比被夹到 0~100，非有限值退化为 null', () => {
    progress('a', 't1', 180)
    progress('a', 't2', -20)
    progress('a', 't3', Number.NaN)

    expect(at(0).percent).toBeNull()
    expect(at(1).percent).toBe(0)
    expect(at(2).percent).toBe(100)
  })
})

describe('上限裁剪', () => {
  it('超过 500 条丢最早的，并累计被裁掉的条数', () => {
    for (let index = 0; index < MAX_PLUGIN_ACTIVITY + 12; index += 1) {
      log('a', `第 ${index} 条`)
    }

    const state = getPluginActivityState()
    expect(state.entries).toHaveLength(MAX_PLUGIN_ACTIVITY)
    expect(state.dropped).toBe(12)
    // 队首是最新的，队尾是「第 12 条」——第 0~11 条被裁掉
    expect(at(0).message).toBe(`第 ${MAX_PLUGIN_ACTIVITY + 11} 条`)
    expect(at(MAX_PLUGIN_ACTIVITY - 1).message).toBe('第 12 条')
  })

  it('清空后 dropped 归零', () => {
    for (let index = 0; index < MAX_PLUGIN_ACTIVITY + 3; index += 1) log('a', `${index}`)
    clearPluginActivity()

    const state = getPluginActivityState()
    expect(state.entries).toHaveLength(0)
    expect(state.dropped).toBe(0)
  })

  it('纯函数：同键替换后仍然只保留 limit 条', () => {
    const existing: PluginActivityEntry[] = [
      {
        key: 'progress:a:t',
        kind: 'progress',
        pluginId: 'a',
        seq: 2,
        at: 2,
        level: null,
        message: '旧',
        percent: 10,
        state: null
      },
      {
        key: 'log:1',
        kind: 'log',
        pluginId: 'a',
        seq: 1,
        at: 1,
        level: 'info',
        message: '日志',
        percent: null,
        state: null
      }
    ]
    const fresh: PluginActivityEntry = {
      key: 'progress:a:t',
      kind: 'progress',
      pluginId: 'a',
      seq: 3,
      at: 3,
      level: null,
      message: '新',
      percent: 80,
      state: null
    }

    const result = mergePluginActivity(existing, fresh, 2)
    expect(result.dropped).toBe(0)
    expect(result.entries.map((item) => item.key)).toEqual(['progress:a:t', 'log:1'])
    expect(result.entries[0]?.message).toBe('新')

    const clipped = mergePluginActivity(existing, fresh, 1)
    expect(clipped.entries).toHaveLength(1)
    expect(clipped.dropped).toBe(1)
  })
})

describe('state_changed', () => {
  it('追加一条状态迁移条目，并带出目标状态', () => {
    receivePluginStateChanged({ pluginId: 'hello', state: 'crashed' })

    expect(entries()).toHaveLength(1)
    expect(at(0).kind).toBe('state')
    expect(at(0).pluginId).toBe('hello')
    expect(at(0).state).toBe('crashed')
    expect(at(0).message).toContain('crashed')
    expect(at(0).level).toBeNull()
  })

  it('触发一次插件清单刷新（列表不能停在旧状态）', async () => {
    const { calls } = stubIpc()

    receivePluginStateChanged({ pluginId: 'hello', state: 'ready' })
    await flush()

    expect(calls.filter((method) => method === Methods.plugin.list)).toHaveLength(1)
  })

  it('刷新失败不影响事件条目（活动照旧记下这次迁移）', async () => {
    const api = {
      request: async () => {
        throw new Error('[5003] ledger io failed')
      }
    } as unknown as AiIdeApi
    vi.stubGlobal('window', { aiide: api })

    receivePluginStateChanged({ pluginId: 'hello', state: 'crashed' })
    await flush()

    expect(entries()).toHaveLength(1)
    expect(at(0).state).toBe('crashed')
  })
})

describe('日志级别筛选', () => {
  it('门槛以上的日志可见，进度与状态始终可见', () => {
    log('a', '调试', 'debug')
    log('a', '信息', 'info')
    log('a', '警告', 'warn')
    log('a', '错误', 'error')
    progress('a', 'scan', 30)
    receivePluginStateChanged({ pluginId: 'a', state: 'ready' })

    setPluginActivityLevel('warn')
    const visible = visiblePluginActivity()

    expect(visible.map((entry) => entry.message)).toEqual([
      '状态变为 ready',
      '',
      '错误',
      '警告'
    ])
    expect(visible.some((entry) => entry.level === 'debug')).toBe(false)
    expect(visible.some((entry) => entry.level === 'info')).toBe(false)
  })

  it('全部级别不过滤', () => {
    log('a', '调试', 'debug')
    log('a', '错误', 'error')

    setPluginActivityLevel('all')
    expect(visiblePluginActivity()).toHaveLength(2)
  })

  it('未知级别按 info 归一（内核只透传，不保证枚举）', () => {
    receivePluginLog({
      pluginId: 'a',
      level: 'trace' as unknown as PluginLogLevel,
      message: '奇怪的级别'
    })

    expect(at(0).level).toBe('info')
    setPluginActivityLevel('warn')
    expect(visiblePluginActivity()).toHaveLength(0)
  })
})

describe('面板开关', () => {
  it('默认收起，可以开关', () => {
    expect(getPluginActivityState().open).toBe(false)
    setPluginActivityOpen(true)
    expect(getPluginActivityState().open).toBe(true)
    setPluginActivityOpen(false)
    expect(getPluginActivityState().open).toBe(false)
  })

  it('开关不影响已收集的条目（收起时也要收）', () => {
    log('a', '后台来的日志')
    setPluginActivityOpen(true)
    expect(entries()).toHaveLength(1)
  })
})
