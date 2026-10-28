/**
 * `plugin-view-store` 的单元测试（docs/24 §9.1 第 3/4 刀）。
 *
 * 覆盖四条不变量：按 rev 去重、切视图让旧响应作废、失败不留旧树、
 * 以及 `plugin:view` 事件只对「当前插件 + 当前视图」生效。
 *
 * 这里把 `pluginApi.call` 换成一个可手动 resolve 的桩：竞态类断言必须能
 * 控制「谁先回来」，靠真实时间等待的测试在 CI 上必然抖。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type { PluginViewRefreshEvent, PluginViewSummary } from '@ai-ide/shared-types'

import { VIEW_SCHEMA } from './plugin-view'

const callMock = vi.fn<(pluginId: string, method: string, params?: unknown) => Promise<unknown>>()

vi.mock('./plugin-api', () => ({
  pluginApi: {
    call: (pluginId: string, method: string, params?: unknown) => callMock(pluginId, method, params)
  }
}))

const {
  getPluginViewState,
  handlePluginViewRefresh,
  initPluginViewEvents,
  refreshPluginView,
  resetPluginViewState,
  selectPluginView,
  syncPluginViews
} = await import('./plugin-view-store')

function view(id: string, entry = `tool.${id}`): PluginViewSummary {
  return { id, title: id, location: 'panel', entry, refresh: false, external: null } as unknown as PluginViewSummary
}

function payload(root: unknown = { type: 'text', value: 'hello' }, rev = 1): { content: string; truncated: boolean } {
  return { content: JSON.stringify({ schema: VIEW_SCHEMA, rev, root }), truncated: false }
}

beforeEach(() => {
  callMock.mockReset()
  resetPluginViewState()
})

describe('plugin-view-store · 拉取与解析', () => {
  it('选中视图后把正文解析成 ParsedView', async () => {
    callMock.mockResolvedValue(payload())
    syncPluginViews('asset3d', [view('main')])
    await selectPluginView('asset3d', view('main'))

    const state = getPluginViewState()
    expect(state.status).toBe('ready')
    expect(state.view?.status).toBe('ok')
    expect(state.rev).toBe(1)
    expect(callMock).toHaveBeenCalledWith('asset3d', 'tool.main', {})
  })

  it('正文超 64 KB 被截断时拒渲染，不当成半棵树显示', async () => {
    callMock.mockResolvedValue({ content: '{"schema":', truncated: true })
    syncPluginViews('asset3d', [view('main')])
    await selectPluginView('asset3d', view('main'))

    const state = getPluginViewState()
    expect(state.status).toBe('error')
    expect(state.view).toBeNull()
    expect(state.error).toContain('64 KB')
  })

  it('正文不是 JSON 时给出可读原因', async () => {
    callMock.mockResolvedValue({ content: '<html>oops</html>', truncated: false })
    syncPluginViews('asset3d', [view('main')])
    await selectPluginView('asset3d', view('main'))

    const state = getPluginViewState()
    expect(state.status).toBe('error')
    expect(state.error).toContain('合法 JSON')
  })

  it('内核报错时清掉旧树，并保留原文', async () => {
    callMock.mockResolvedValueOnce(payload())
    syncPluginViews('asset3d', [view('main')])
    await selectPluginView('asset3d', view('main'))
    expect(getPluginViewState().view).not.toBeNull()

    callMock.mockRejectedValueOnce(new Error('插件进程未就绪（6102）'))
    await refreshPluginView()

    const state = getPluginViewState()
    expect(state.status).toBe('error')
    expect(state.view).toBeNull()
    expect(state.error).toContain('6102')
  })
})

describe('plugin-view-store · 声明同步', () => {
  it('切换插件时清空内容，避免上个插件的树留在界面上', () => {
    syncPluginViews('a', [view('main')])
    syncPluginViews('b', [view('other')])

    const state = getPluginViewState()
    expect(state.pluginId).toBe('b')
    expect(state.activeViewId).toBeNull()
    expect(state.view).toBeNull()
  })

  it('声明没变时保留已渲染结果（不触发无谓重拉）', () => {
    syncPluginViews('a', [view('main')])
    const before = getPluginViewState()
    syncPluginViews('a', [view('main')])
    expect(getPluginViewState()).toBe(before)
  })

  it('当前视图从声明里消失时清空选中', async () => {
    callMock.mockResolvedValue(payload())
    syncPluginViews('a', [view('main'), view('extra')])
    await selectPluginView('a', view('extra'))
    syncPluginViews('a', [view('main')])
    expect(getPluginViewState().activeViewId).toBeNull()
  })
})

describe('plugin-view-store · 刷新事件去重', () => {
  it('只对当前插件 + 当前视图生效', async () => {
    callMock.mockResolvedValue(payload())
    syncPluginViews('asset3d', [view('main')])
    await selectPluginView('asset3d', view('main'))
    callMock.mockClear()

    expect(handlePluginViewRefresh({ pluginId: 'other', viewId: 'main', rev: 9 })).toBe(false)
    expect(handlePluginViewRefresh({ pluginId: 'asset3d', viewId: 'side', rev: 9 })).toBe(false)
    expect(callMock).not.toHaveBeenCalled()
  })

  it('同一 rev 不重复拉，更大的 rev 才重拉', async () => {
    callMock.mockResolvedValue(payload(undefined, 3))
    syncPluginViews('asset3d', [view('main')])
    await selectPluginView('asset3d', view('main'))
    expect(getPluginViewState().rev).toBe(3)
    callMock.mockClear()

    expect(handlePluginViewRefresh({ pluginId: 'asset3d', viewId: 'main', rev: 3 })).toBe(false)
    expect(handlePluginViewRefresh({ pluginId: 'asset3d', viewId: 'main', rev: 2 })).toBe(false)
    expect(callMock).not.toHaveBeenCalled()

    expect(handlePluginViewRefresh({ pluginId: 'asset3d', viewId: 'main', rev: 4 })).toBe(true)
    expect(callMock).toHaveBeenCalledTimes(1)
  })
})

describe('plugin-view-store · 事件订阅', () => {
  /** vitest 跑在 node 环境，没有 window；订阅入口靠这里装上再验。 */
  function stubPreload(
    onPluginView: ((cb: (event: PluginViewRefreshEvent) => void) => () => void) | null
  ): void {
    ;(globalThis as { window?: unknown }).window = {
      aiide: onPluginView === null ? {} : { onPluginView }
    }
  }

  afterEach(() => {
    delete (globalThis as { window?: unknown }).window
  })

  it('收到命中当前视图的新 rev 时重拉，退订后不再响应', async () => {
    callMock.mockResolvedValue(payload(undefined, 1))
    syncPluginViews('asset3d', [view('main')])
    await selectPluginView('asset3d', view('main'))
    callMock.mockClear()

    let listener: ((event: PluginViewRefreshEvent) => void) | undefined
    const off = vi.fn()
    stubPreload((cb) => {
      listener = cb
      return off
    })

    const dispose = initPluginViewEvents()
    expect(listener).toBeTypeOf('function')

    listener?.({ pluginId: 'asset3d', viewId: 'main', rev: 2 })
    await vi.waitFor(() => expect(callMock).toHaveBeenCalledTimes(1))

    dispose()
    expect(off).toHaveBeenCalledTimes(1)
  })

  it('旧版 preload 没有 onPluginView 时退化为手动刷新，不抛错', () => {
    stubPreload(null)
    const dispose = initPluginViewEvents()
    expect(() => dispose()).not.toThrow()
  })

  it('没有 window 的安全返回空退订', () => {
    const dispose = initPluginViewEvents()
    expect(() => dispose()).not.toThrow()
  })
})

describe('plugin-view-store · 竞态', () => {
  it('后点的视图先回来时，先点的迟到响应被丢弃', async () => {
    // 用数组持有 resolve：赋值发生在 Promise 执行器里，TS 的控制流分析
    // 看不见它，直接写 `let resolveFirst` 会被收窄成 never。
    const pending: ((value: unknown) => void)[] = []
    callMock.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          pending.push(resolve)
        })
    )
    callMock.mockResolvedValueOnce(payload({ type: 'text', value: 'second' }))

    syncPluginViews('asset3d', [view('main'), view('side')])
    const first = selectPluginView('asset3d', view('main'))
    await selectPluginView('asset3d', view('side'))

    expect(getPluginViewState().activeViewId).toBe('side')
    expect(getPluginViewState().view?.root?.value).toBe('second')

    pending[0]?.(payload({ type: 'text', value: 'first' }))
    await first

    // 迟到的 main 不能把 side 的内容顶掉。
    expect(getPluginViewState().activeViewId).toBe('side')
    expect(getPluginViewState().view?.root?.value).toBe('second')
  })
})
