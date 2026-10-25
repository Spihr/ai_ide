/**
 * 符号视图纯逻辑的测试。
 *
 * 重点不是分组本身好不好看，而是**浮层与搜索面板是否看到同一个顺序**：两处都调
 * `groupSymbols` / `visibleSymbols`，一旦组间顺序或组内顺序跟扁平列表不一致，用户按
 * ↓ 选到第 3 行、回车却跳到第 5 个符号，这种错位只在键盘上才暴露。
 */

import { describe, expect, it } from 'vitest'

import type { Symbol, SymbolKind } from '@ai-ide/shared-types'

import {
  SYMBOL_KIND_ORDER,
  groupSymbols,
  symbolKey,
  symbolKindGlyph,
  symbolKindLabel,
  symbolKindRank,
  symbolLineLabel,
  symbolLocationLabel,
  visibleSymbols
} from './symbol-search'

function sym(name: string, kind: SymbolKind, path = 'src/a.ts', line = 1): Symbol {
  return { name, kind, path, range: { start_line: line, end_line: line + 1 } }
}

describe('kind 文案与图标', () => {
  it('八种 kind 都有中文名与单字符图标', () => {
    for (const kind of SYMBOL_KIND_ORDER) {
      expect(symbolKindLabel(kind), kind).not.toBe('')
      expect(symbolKindGlyph(kind), kind).toHaveLength(1)
    }
  })

  it('未知 kind 不渲染成空白', () => {
    expect(symbolKindLabel('macro')).toBe('macro')
    expect(symbolKindGlyph('macro')).toBe('·')
  })

  it('未知 kind 排在已知 kind 之后', () => {
    expect(symbolKindRank('macro')).toBe(SYMBOL_KIND_ORDER.length)
    expect(symbolKindRank('function')).toBe(0)
  })
})

describe('分组', () => {
  it('组间按固定顺序，组内保持内核给的相关度顺序', () => {
    const groups = groupSymbols([
      sym('MAX', 'const', 'src/a.ts', 3),
      sym('load', 'function', 'src/a.ts', 10),
      sym('Loader', 'class', 'src/b.ts', 2),
      sym('fetch', 'function', 'src/a.ts', 20)
    ])

    expect(groups.map((group) => group.kind)).toEqual(['function', 'class', 'const'])
    // 组内不重排：fetch 在源码里靠后，但内核认为它更相关就保持在前。
    expect(groups[0]?.symbols.map((item) => item.name)).toEqual(['load', 'fetch'])
    expect(groups[0]?.label).toBe('函数')
    expect(groups[0]?.glyph).toBe('ƒ')
  })

  it('空结果得到空分组，不产出空壳组', () => {
    expect(groupSymbols([])).toEqual([])
  })

  it('扁平顺序与分组视觉顺序一致', () => {
    const symbols = [
      sym('MAX', 'const', 'src/a.ts', 3),
      sym('load', 'function', 'src/a.ts', 10),
      sym('Loader', 'class', 'src/b.ts', 2)
    ]

    const flattened = visibleSymbols(symbols)
    const fromGroups = groupSymbols(symbols).flatMap((group) => group.symbols)

    expect(flattened.map((item) => item.name)).toEqual(fromGroups.map((item) => item.name))
    expect(flattened.map((item) => item.name)).toEqual(['load', 'Loader', 'MAX'])
  })
})

describe('行标识与位置文案', () => {
  it('位置文案是 path:line', () => {
    expect(symbolLocationLabel(sym('load', 'function', 'src/a.ts', 42))).toBe('src/a.ts:42')
    expect(symbolLineLabel(sym('load', 'function', 'src/a.ts', 42))).toBe(':42')
  })

  it('键含路径与行号，同名符号不撞键', () => {
    const first = symbolKey(sym('new', 'method', 'src/a.ts', 10))
    const second = symbolKey(sym('new', 'method', 'src/a.ts', 30))

    expect(first).not.toBe(second)
    expect(first).toBe('src/a.ts:10:new')
  })
})
