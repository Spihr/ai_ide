/**
 * 命令面板状态机测试（FR-1.7）：取数、执行分发、错误提示、开关与 Esc 语义。
 *
 * 这里测的是「按了回车之后发生了什么」，所以必须装 `window.aiide` 桩——与
 * `plugin-api.test.ts` 同一套做法。纯过滤 / 导航 / 文案的断言在
 * `command-registry.test.ts`，不在这里重复。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'

import { Methods } from '@ai-ide/shared-types'

import type { AiIdeApi } from '../../shared/ipc'
import {
  closePalette,
  escapePalette,
  executeCommand,
  getPaletteState,
  movePaletteActive,
  openPalette,
  paletteGroups,
  refreshPluginCommands,
  resetPaletteState,
  setPaletteQuery,
  visibleCommands
} from './command-palette-store'
import { getEditorState } from './store'

interface Call {
  method: string
  params: Record<string, unknown> | undefined
}

interface Harness {
  calls: Call[]
  /** 让 `plugin.status` 对某个插件抛错，用于验证「单个插件坏了不影响其它」。 */
  failStatusFor: Set<string>
  /** `plugin.command` 的行为，默认返回一段输出。 */
  commandResult: (pluginId: string, command: string) => unknown
}

function installHarness(): Harness {
  const calls: Call[] = []
  const harness: Harness = {
    calls,
    failStatusFor: new Set<string>(),
    commandResult: () => ({ content: 'done', truncated: false })
  }

  const api = {
    request: async (method: string, params?: unknown) => {
      calls.push({ method, params: params as Record<string, unknown> | undefined })
      if (method === Methods.plugin.list) {
        return {
          plugins: [
            { id: 'acme.tools', name: 'Acme 工具', version: '0.1.0', state: 'ready' },
            { id: 'beta.git', name: 'Beta Git', version: '0.2.0', state: 'ready' }
          ]
        }
      }
      if (method === Methods.plugin.status) {
        const pluginId = String((params as { plugin_id?: string } | undefined)?.plugin_id ?? '')
        if (harness.failStatusFor.has(pluginId)) {
          throw new Error(`[6102] plugin not ready: ${pluginId}`)
        }
        return {
          plugin: {
            id: pluginId,
            name: pluginId === 'acme.tools' ? 'Acme 工具' : 'Beta Git',
            version: '0.1.0',
            state: 'ready'
          },
          tools: [],
          commands:
            pluginId === 'acme.tools'
              ? [
                  { name: 'format_json', title: '格式化 JSON', risk: 'write' },
                  { name: 'lint', title: '检查代码风格', risk: 'read' }
                ]
              : [{ name: 'sync', title: '同步远端', risk: 'read' }]
        }
      }
      if (method === Methods.plugin.command) {
        const record = params as { plugin_id?: string; command?: string } | undefined
        return harness.commandResult(String(record?.plugin_id ?? ''), String(record?.command ?? ''))
      }
      throw new Error(`测试收到了意外方法：${method}`)
    }
  } as unknown as AiIdeApi

  vi.stubGlobal('window', { aiide: api })
  return harness
}

beforeEach(() => {
  vi.unstubAllGlobals()
  resetPaletteState()
})

describe('取数与分组', () => {
  it('打开面板时把每个插件的命令都拉进来，并按「内置 → 插件」分组', async () => {
    installHarness()
    await refreshPluginCommands()

    const groups = paletteGroups()
    expect(groups.map((group) => group.section)).toEqual(['builtin', 'plugin'])
    const pluginTitles = groups[1]?.commands.map((row) => row.title) ?? []
    expect(pluginTitles).toContain('格式化 JSON')
    expect(pluginTitles).toContain('同步远端')
    expect(groups[1]?.commands).toHaveLength(3)
  })

  it('单个插件取详情失败只少那一组命令，其余照常', async () => {
    const harness = installHarness()
    harness.failStatusFor.add('beta.git')
    await refreshPluginCommands()

    const titles = visibleCommands()
      .filter((row) => row.section === 'plugin')
      .map((row) => row.title)
    expect(titles).toContain('格式化 JSON')
    expect(titles).not.toContain('同步远端')
    expect(getPaletteState().loadError).toContain('1 个插件')
  })

  it('清单拉不回来时保留旧命令，只挂一条提示', async () => {
    const first = installHarness()
    await refreshPluginCommands()
    const before = getPaletteState().plugin.length
    expect(before).toBeGreaterThan(0)

    // 换一个连 list 都会炸的桩：面板不能因此清空插件组。
    vi.unstubAllGlobals()
    vi.stubGlobal('window', {
      aiide: {
        request: async () => {
          throw new Error('[1002] core unavailable')
        }
      } as unknown as AiIdeApi
    })
    expect(first.calls.length).toBeGreaterThan(0)
    await refreshPluginCommands()

    expect(getPaletteState().plugin.length).toBe(before)
    expect(getPaletteState().loadError).toContain('读不到')
  })
})

describe('开合与导航', () => {
  it('openPalette 重置查询并把选中项归零', () => {
    openPalette()
    setPaletteQuery('保存')
    movePaletteActive(2)
    expect(getPaletteState().open).toBe(true)

    openPalette()
    expect(getPaletteState().query).toBe('')
    expect(getPaletteState().activeIndex).toBe(0)
  })

  it('Esc 两段式：先清词，词空了才关面板', () => {
    openPalette()
    setPaletteQuery('git')

    escapePalette()
    expect(getPaletteState().open).toBe(true)
    expect(getPaletteState().query).toBe('')

    escapePalette()
    expect(getPaletteState().open).toBe(false)
  })

  it('移到最后一行再往下回到第一行', () => {
    openPalette()
    const total = visibleCommands().length
    expect(total).toBeGreaterThan(2)

    movePaletteActive(total - 1)
    expect(getPaletteState().activeIndex).toBe(total - 1)
    movePaletteActive(1)
    expect(getPaletteState().activeIndex).toBe(0)
  })

  it('查询变化时选中项回到首行，避免高亮指向已消失的行', () => {
    openPalette()
    movePaletteActive(3)
    setPaletteQuery('插件')
    expect(getPaletteState().activeIndex).toBe(0)
  })

  it('closePalette 清空状态', () => {
    openPalette()
    setPaletteQuery('x')
    closePalette()
    expect(getPaletteState().open).toBe(false)
    expect(getPaletteState().query).toBe('')
    expect(getPaletteState().activeIndex).toBe(0)
  })
})

describe('执行分发', () => {
  it('内置命令走 store 动作，不发 IPC', async () => {
    const harness = installHarness()
    openPalette()
    const row = visibleCommands().find((candidate) => candidate.id === 'builtin:view.plugin')
    expect(row).toBeDefined()

    await executeCommand(row!)
    expect(getEditorState().activity).toBe('plugin')
    expect(harness.calls.some((call) => call.method === Methods.plugin.command)).toBe(false)
    // 执行内置命令后浮层收起：内置动作往往自己会打开别的东西。
    expect(getPaletteState().open).toBe(false)
  })

  it('插件命令走 plugin.command 通道，参数只带 plugin_id + command', async () => {
    const harness = installHarness()
    await refreshPluginCommands()
    const row = visibleCommands().find((candidate) => candidate.command === 'format_json')
    expect(row).toBeDefined()

    await executeCommand(row!)
    const call = harness.calls.find((entry) => entry.method === Methods.plugin.command)
    expect(call).toBeDefined()
    expect(call?.params).toEqual({ plugin_id: 'acme.tools', command: 'format_json' })

    // 成功也要有可见回执：只说「已执行」+ 插件输出。
    expect(getEditorState().notice?.kind).toBe('info')
    expect(getEditorState().notice?.text).toContain('格式化 JSON')
    expect(getEditorState().notice?.text).toContain('done')
  })

  it('输出被截断时提示里点明「已截断」', async () => {
    const harness = installHarness()
    harness.commandResult = () => ({ content: 'x'.repeat(10), truncated: true })
    await refreshPluginCommands()
    const row = visibleCommands().find((candidate) => candidate.command === 'lint')

    await executeCommand(row!)
    expect(getEditorState().notice?.text).toContain('已截断')
  })

  it('六个错误码各自给出专属中文提示，且不会静默失败', async () => {
    const cases: Array<{ code: number; expect: string }> = [
      { code: 6101, expect: '插件已不在本机' },
      { code: 6102, expect: '插件没在运行' },
      { code: 6103, expect: '缺少这条命令所需的能力' },
      { code: 6106, expect: '没有在清单里声明' },
      { code: 3001, expect: '拒绝了这次写操作的审批' },
      { code: 6109, expect: '过于频繁' }
    ]

    for (const { code, expect: expected } of cases) {
      vi.unstubAllGlobals()
      resetPaletteState()
      const harness = installHarness()
      harness.commandResult = () => {
        throw new Error(`[${code}] boom`)
      }
      await refreshPluginCommands()
      const row = visibleCommands().find((candidate) => candidate.command === 'lint')

      await executeCommand(row!)
      const notice = getEditorState().notice
      expect(notice?.kind, `${code} 应为错误提示`).toBe('error')
      expect(notice?.text, `${code} 文案`).toContain(expected)
      // 每次失败都要把 pending 放开，否则面板会卡在「执行中」再也不响应回车。
      expect(getPaletteState().pendingId).toBeNull()
    }
  })

  it('失败时不关浮层，用户可就地换一条命令重试', async () => {
    const harness = installHarness()
    harness.commandResult = () => {
      throw new Error('[3001] approval denied')
    }
    openPalette()
    await executeCommand({
      id: 'plugin:acme.tools:format_json',
      section: 'plugin',
      title: '格式化 JSON',
      description: 'Acme 工具 · 写入',
      keywords: ['format_json'],
      pluginId: 'acme.tools',
      command: 'format_json',
      risk: 'write'
    })
    expect(getPaletteState().open).toBe(true)
    expect(getEditorState().notice?.text).toContain('拒绝了这次写操作的审批')
  })

  it('没有插件标识的行不会发请求，只提示', async () => {
    const harness = installHarness()
    openPalette()
    await executeCommand({
      id: 'plugin:broken:ghost',
      section: 'plugin',
      title: '幽灵命令',
      description: '',
      keywords: []
    })
    expect(harness.calls.some((call) => call.method === Methods.plugin.command)).toBe(false)
    expect(getEditorState().notice?.kind).toBe('error')
    expect(getEditorState().notice?.text).toContain('缺少插件或命令标识')
  })
})
