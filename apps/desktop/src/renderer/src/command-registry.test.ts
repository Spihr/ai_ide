/**
 * 命令面板的纯逻辑测试（FR-1.7）。
 *
 * 钉四件事：模糊过滤的召回、分组顺序、键盘导航的回绕、以及**错误码到中文文案**的
 * 映射。这四块都不碰 IPC / DOM，所以能拿假数据直接断言，跑得快也不会因为
 * `window.aiide` 没装而挂掉。
 *
 * 样式与滚动行为不测。
 */

import { describe, expect, it, vi } from 'vitest'

import type { PluginCommandSummary } from '@ai-ide/shared-types'

import {
  COMMAND_ERROR_TEXT,
  PALETTE_SECTION_LABELS,
  builtinCommands,
  describeCommandError,
  filterCommands,
  fuzzyScore,
  groupCommands,
  moveActiveIndex,
  normalizeActiveIndex,
  pluginCommands,
  type BuiltinCommandActions,
  type PaletteCommand,
  type PluginCommandSource
} from './command-registry'

function fakeActions(): BuiltinCommandActions {
  return {
    openWorkspace: vi.fn(),
    saveActiveTab: vi.fn(),
    showView: vi.fn(),
    toggleTerminal: vi.fn(),
    gotoSymbol: vi.fn()
  }
}

function commandSummary(
  name: string,
  title: string,
  risk: PluginCommandSummary['risk'] = 'read'
): PluginCommandSummary {
  return { name, title, risk }
}

function source(
  pluginId: string,
  pluginName: string,
  commands: PluginCommandSummary[]
): PluginCommandSource {
  return { pluginId, pluginName, state: 'ready', commands }
}

describe('内置命令注册表', () => {
  it('覆盖 FR-1.7 要求的四类动作，且 id 不重复', () => {
    const rows = builtinCommands(fakeActions())
    const ids = rows.map((row) => row.id)

    expect(new Set(ids).size, 'id 必须唯一').toBe(ids.length)
    expect(ids).toContain('builtin:workspace.open')
    expect(ids).toContain('builtin:file.save')
    expect(ids).toContain('builtin:view.explorer')
    expect(ids).toContain('builtin:view.plugin')
    expect(ids).toContain('builtin:terminal.toggle')
    for (const row of rows) {
      expect(row.section).toBe('builtin')
      expect(row.title.length).toBeGreaterThan(0)
      expect(row.keywords.length).toBeGreaterThan(0)
    }
  })

  it('执行内置命令时调用对应动作', async () => {
    const actions = fakeActions()
    const rows = builtinCommands(actions)

    await rows.find((row) => row.id === 'builtin:view.explorer')?.run?.()
    expect(actions.showView).toHaveBeenCalledWith('explorer')

    await rows.find((row) => row.id === 'builtin:view.plugin')?.run?.()
    expect(actions.showView).toHaveBeenCalledWith('plugin')

    await rows.find((row) => row.id === 'builtin:file.save')?.run?.()
    expect(actions.saveActiveTab).toHaveBeenCalledTimes(1)

    await rows.find((row) => row.id === 'builtin:terminal.toggle')?.run?.()
    expect(actions.toggleTerminal).toHaveBeenCalledTimes(1)

    await rows.find((row) => row.id === 'builtin:workspace.open')?.run?.()
    expect(actions.openWorkspace).toHaveBeenCalledTimes(1)
  })

  it('「转到符号」是内建命令，走 gotoSymbol 动作', async () => {
    const actions = fakeActions()
    const row = builtinCommands(actions).find((entry) => entry.id === 'builtin:symbol.goto')

    expect(row, '命令面板必须能搜到「转到符号」').toBeDefined()
    // 描述里带快捷键：用户在面板里看到这一行就知道下次可以直接按。
    expect(row?.description).toContain('Ctrl+T')

    await row?.run?.()
    expect(actions.gotoSymbol).toHaveBeenCalledTimes(1)
  })

  it('「转到符号」能被中文与 symbol 两种词召回', () => {
    const rows = builtinCommands(fakeActions())

    const byChinese = filterCommands(rows, '符号').map((row) => row.id)
    expect(byChinese).toContain('builtin:symbol.goto')

    const byEnglish = filterCommands(rows, 'symbol').map((row) => row.id)
    expect(byEnglish).toContain('builtin:symbol.goto')
  })
})

describe('插件命令拍平', () => {
  it('带上插件名与风险级别，且不挂 run（执行只能走 IPC）', () => {
    const rows = pluginCommands([
      source('acme.tools', 'Acme 工具', [
        commandSummary('format', '格式化文档', 'write'),
        commandSummary('ping', '探活')
      ])
    ])

    expect(rows).toHaveLength(2)
    expect(rows[0]?.id).toBe('plugin:acme.tools:format')
    expect(rows[0]?.title).toBe('格式化文档')
    expect(rows[0]?.description).toContain('Acme 工具')
    expect(rows[0]?.description).toContain('写入')
    expect(rows[0]?.risk).toBe('write')
    expect(rows[0]?.run).toBeUndefined()
    expect(rows[1]?.description).toContain('只读')
  })

  it('标题为空时退回命令名，别渲染出一行空白', () => {
    const rows = pluginCommands([source('p', 'P', [commandSummary('noop', '')])])
    expect(rows[0]?.title).toBe('noop')
  })

  it('同一命令名分属不同插件时 id 不撞车', () => {
    const rows = pluginCommands([
      source('a', 'A', [commandSummary('format', '格式化')]),
      source('b', 'B', [commandSummary('format', '格式化')])
    ])
    expect(new Set(rows.map((row) => row.id)).size).toBe(2)
  })
})

describe('模糊过滤', () => {
  const rows: PaletteCommand[] = [
    ...builtinCommands(fakeActions()),
    ...pluginCommands([
      source('acme.tools', 'Acme 工具', [
        commandSummary('format_json', '格式化 JSON', 'write'),
        commandSummary('lint', '检查代码风格')
      ])
    ])
  ]

  it('空查询保留全部，且保持原始顺序', () => {
    expect(filterCommands(rows, '').map((row) => row.id)).toEqual(rows.map((row) => row.id))
    expect(filterCommands(rows, '   ')).toHaveLength(rows.length)
  })

  it('按标题子序列命中', () => {
    const hits = filterCommands(rows, '保存')
    expect(hits.length).toBeGreaterThan(0)
    expect(hits[0]?.id).toBe('builtin:file.save')
  })

  it('按英文关键词与命令名命中插件命令', () => {
    const byKeyword = filterCommands(rows, 'json').map((row) => row.id)
    expect(byKeyword).toContain('plugin:acme.tools:format_json')

    const byPluginName = filterCommands(rows, 'acme').map((row) => row.id)
    expect(byPluginName).toEqual(
      expect.arrayContaining(['plugin:acme.tools:format_json', 'plugin:acme.tools:lint'])
    )
  })

  it('多词查询按 AND 语义收窄', () => {
    const both = filterCommands(rows, 'acme format').map((row) => row.id)
    expect(both).toEqual(['plugin:acme.tools:format_json'])

    // 一个词不命中就整条掉出，不当成 OR 放大结果
    expect(filterCommands(rows, 'acme 不存在的词')).toHaveLength(0)
  })

  it('完全不匹配时返回空数组而不是原样返回', () => {
    expect(filterCommands(rows, 'zzzzz')).toEqual([])
  })

  it('标题前缀命中排在子序列命中之前', () => {
    const candidates: PaletteCommand[] = [
      {
        id: 'spread',
        section: 'builtin',
        title: '打开临时目录',
        description: '',
        keywords: []
      },
      {
        id: 'prefix',
        section: 'builtin',
        title: '打开目录',
        description: '',
        keywords: []
      }
    ]
    expect(filterCommands(candidates, '打开目').map((row) => row.id)).toEqual(['prefix', 'spread'])
  })

  it('fuzzyScore 对空查询回 0，不匹配回 null', () => {
    expect(fuzzyScore('', '保存')).toBe(0)
    expect(fuzzyScore('保存', '')).toBeNull()
    expect(fuzzyScore('保存', '保存文件')).not.toBeNull()
    expect(fuzzyScore('保x存', '保存文件')).toBeNull()
  })
})

describe('分组展示', () => {
  it('固定「内置命令 → 插件命令」顺序，空组不出现', () => {
    const builtin = builtinCommands(fakeActions())
    const plugin = pluginCommands([source('a', 'A', [commandSummary('x', 'X')])])

    const groups = groupCommands([...builtin, ...plugin])
    expect(groups.map((group) => group.section)).toEqual(['builtin', 'plugin'])
    expect(groups.map((group) => group.label)).toEqual([
      PALETTE_SECTION_LABELS.builtin,
      PALETTE_SECTION_LABELS.plugin
    ])

    // 只剩插件命令时，内置组不该留一个空标题
    expect(groupCommands(plugin).map((group) => group.section)).toEqual(['plugin'])
    expect(groupCommands([])).toEqual([])
  })

  it('组内保持传入顺序（即相关性排序）', () => {
    const builtin = builtinCommands(fakeActions())
    const groups = groupCommands([...builtin].reverse())
    expect(groups[0]?.commands.map((row) => row.id)).toEqual(
      [...builtin].reverse().map((row) => row.id)
    )
  })
})

describe('键盘导航', () => {
  it('↓ 越界回绕到首行，↑ 越界回绕到末行', () => {
    expect(moveActiveIndex(0, 1, 3)).toBe(1)
    expect(moveActiveIndex(2, 1, 3)).toBe(0)
    expect(moveActiveIndex(0, -1, 3)).toBe(2)
    expect(moveActiveIndex(1, -1, 3)).toBe(0)
  })

  it('空列表时回 -1，不出现 NaN 下标', () => {
    expect(moveActiveIndex(0, 1, 0)).toBe(-1)
    expect(normalizeActiveIndex(0, 0)).toBe(-1)
    expect(moveActiveIndex(Number.NaN, 1, 2)).toBe(1)
  })

  it('下标夹在有效区间内', () => {
    expect(normalizeActiveIndex(9, 3)).toBe(2)
    expect(normalizeActiveIndex(-4, 3)).toBe(0)
    expect(normalizeActiveIndex(1.7, 3)).toBe(1)
    expect(normalizeActiveIndex(0, 1)).toBe(0)
  })

  it('大步长跨多行也按回绕算', () => {
    expect(moveActiveIndex(1, 5, 3)).toBe(0)
    expect(moveActiveIndex(1, -5, 3)).toBe(2)
  })
})

describe('错误码文案', () => {
  const extract = {
    code: (error: unknown) => {
      const matched = /\[(\d{4})\]/.exec(error instanceof Error ? error.message : String(error))
      return matched ? Number(matched[1]) : null
    },
    message: (error: unknown) => (error instanceof Error ? error.message : String(error))
  }

  it('六个约定错误码都有专属中文文案，且互不相同', () => {
    const codes = [6101, 6102, 6106, 6103, 3001, 6109]
    const texts = codes.map((code) => COMMAND_ERROR_TEXT[code])
    for (const [index, code] of codes.entries()) {
      expect(texts[index], `${code} 缺少文案`).toBeTruthy()
      expect(texts[index]?.length ?? 0).toBeGreaterThan(8)
    }
    expect(new Set(texts).size).toBe(codes.length)
  })

  it('文案里点明「没执行」，用户不会以为命令生效了', () => {
    for (const code of [6101, 6102, 6103, 6106, 3001, 6109]) {
      expect(COMMAND_ERROR_TEXT[code], `${code} 应说明未执行`).toMatch(/没有执行|未执行|已拦截|已中止/)
    }
  })

  it('describeCommandError 把码翻成人话并带上命令标题', () => {
    const view = describeCommandError(
      new Error('[6102] plugin not ready'),
      { commandTitle: '格式化 JSON' },
      extract
    )
    expect(view.code).toBe(6102)
    expect(view.text).toContain('格式化 JSON')
    expect(view.text).toContain('插件没在运行')
  })

  it('未知码与无码错误回落到原文，不吞掉信息', () => {
    const unknown = describeCommandError(
      new Error('[9999] weird failure'),
      { commandTitle: 'X' },
      extract
    )
    expect(unknown.code).toBe(9999)
    expect(unknown.text).toContain('[9999] weird failure')

    const plain = describeCommandError(new Error('boom'), { commandTitle: 'X' }, extract)
    expect(plain.code).toBeNull()
    expect(plain.text).toContain('boom')

    const empty = describeCommandError(new Error(''), { commandTitle: 'X' }, extract)
    expect(empty.text).toContain('未知错误')
  })
})
