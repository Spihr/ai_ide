/**
 * Tab 补全纯逻辑测试。
 *
 * 覆盖两件事，且都要能在这个文件里单独断言：
 * 1. **候选怎么来**：LSP 与符号索引两路合并、去重、本文件优先、截断；
 * 2. **这次 Tab 动不动编辑器**：无候选必须 `pass`（缩进照旧）、单候选直补、
 *    多候选先补公共前缀；以及来源抛错时的降级。
 *
 * 不测 `CodeEditor.tsx` 的 Monaco 接线（需要真实编辑器实例），那部分靠「`runTabCompletion`
 * 收到什么就返回什么」这一层把风险压到最小。
 */

import { describe, expect, it } from 'vitest'

import type { Symbol, SymbolKind } from '@ai-ide/shared-types'

import type { CompletionHint } from './lsp-providers'
import {
  MAX_TAB_CANDIDATES,
  MIN_TAB_PREFIX,
  decideTabAction,
  longestCommonPrefix,
  replaceRangeFor,
  runTabCompletion,
  toTabCandidates,
  type TabRunnerHost
} from './tab-completion'

const RANGE = { start: { line: 0, character: 0 }, end: { line: 0, character: 0 } }

function hint(label: string, insertText?: string, detail?: string): CompletionHint {
  const value: CompletionHint = { label, range: RANGE }
  if (insertText !== undefined) value.insertText = insertText
  if (detail !== undefined) value.detail = detail
  return value
}

function symbol(name: string, path = 'src/other.rs', kind: SymbolKind = 'function'): Symbol {
  return { name, kind, path, range: { start_line: 1, end_line: 2 } }
}

describe('候选合并', () => {
  it('前缀短于下限时不下发候选', () => {
    expect(MIN_TAB_PREFIX).toBeGreaterThan(1)
    const candidates = toTabCandidates({
      prefix: 'f'.slice(0, MIN_TAB_PREFIX - 1),
      completions: [hint('foobar')],
      symbols: [symbol('foobar')]
    })
    expect(candidates).toEqual([])
  })

  it('两路来源合并且按 insertText 去重，LSP 优先', () => {
    const candidates = toTabCandidates({
      prefix: 'fo',
      completions: [hint('format', 'format(...)')],
      symbols: [symbol('format'), symbol('fold')]
    })
    // `format` 只留 LSP 那份：它的插入文本带括号占位，更准。
    expect(candidates.map((item) => [item.label, item.source, item.insertText])).toEqual([
      ['format', 'lsp', 'format(...)'],
      ['fold', 'symbol', 'fold']
    ])
  })

  it('本文件的符号排在 LSP 与其它文件之前', () => {
    const candidates = toTabCandidates({
      prefix: 'ru',
      completions: [hint('runForever')],
      symbols: [symbol('runTask', 'src/other.rs'), symbol('runLocal', 'src\\a.ts')],
      currentPath: 'src/a.ts'
    })
    expect(candidates.map((item) => item.label)).toEqual(['runLocal', 'runForever', 'runTask'])
  })

  it('补不出新字符的候选被丢掉（否则 Tab 会变哑键）', () => {
    const candidates = toTabCandidates({
      prefix: 'run',
      completions: [hint('run', 'run')],
      symbols: [symbol('run')]
    })
    expect(candidates).toEqual([])
  })

  it('按上限截断', () => {
    const many = Array.from({ length: MAX_TAB_CANDIDATES + 10 }, (_, index) =>
      symbol(`fn_${String(index).padStart(3, '0')}`)
    )
    expect(toTabCandidates({ prefix: 'fn', symbols: many })).toHaveLength(MAX_TAB_CANDIDATES)
  })
})

describe('决策', () => {
  it('无候选 → pass', () => {
    expect(decideTabAction([], 'ru')).toEqual({ kind: 'pass', reason: 'no-candidate' })
  })

  it('单候选 → 直接补', () => {
    const candidate = { label: 'runTask', insertText: 'runTask', source: 'symbol' as const }
    expect(decideTabAction([candidate], 'run')).toEqual({
      kind: 'insert',
      insertText: 'runTask',
      candidate,
      extended: true
    })
  })

  it('多候选 → 只补公共前缀', () => {
    const candidates = [
      { label: 'runTask', insertText: 'runTask', source: 'symbol' as const },
      { label: 'runTest', insertText: 'runTest', source: 'symbol' as const }
    ]
    expect(decideTabAction(candidates, 'ru')).toEqual({
      kind: 'insert',
      insertText: 'runT',
      candidate: candidates[0],
      extended: true
    })
  })

  it('公共前缀已用尽 → 取第一条（LSP 顺序即相关度）', () => {
    const candidates = [
      { label: 'runTask', insertText: 'runTask', source: 'lsp' as const },
      { label: 'runTest', insertText: 'runTest', source: 'lsp' as const }
    ]
    expect(decideTabAction(candidates, 'runT')).toEqual({
      kind: 'insert',
      insertText: 'runTask',
      candidate: candidates[0],
      extended: false
    })
  })

  it('最长公共前缀', () => {
    expect(longestCommonPrefix(['runner', 'run', 'rune'])).toBe('run')
    expect(longestCommonPrefix([])).toBe('')
  })
})

describe('替换区间', () => {
  it('Monaco 的 1-based 行列换成 0-based 半开区间', () => {
    // 列 8 表示光标前有 7 个字符；前缀 4 个字符 → 替换 [3, 7)。
    expect(replaceRangeFor({ lineNumber: 3, column: 8 }, 'runT')).toEqual({
      start: { line: 2, character: 3 },
      end: { line: 2, character: 7 }
    })
  })

  it('行首按 Tab 不会算出负列', () => {
    expect(replaceRangeFor({ lineNumber: 1, column: 1 }, '')).toEqual({
      start: { line: 0, character: 0 },
      end: { line: 0, character: 0 }
    })
  })
})

interface Recorder {
  host: TabRunnerHost
  inserted: Array<{ text: string; start: number; end: number }>
  passed: number
  completionsAsked: number
  symbolsAsked: number
}

function recorder(
  prefix: string,
  sources: {
    completions?: () => Promise<CompletionHint[]>
    symbols?: () => Promise<Symbol[]>
    position?: { lineNumber: number; column: number }
  } = {}
): Recorder {
  const state: Recorder = {
    inserted: [],
    passed: 0,
    completionsAsked: 0,
    symbolsAsked: 0,
    host: {
      prefix,
      position: sources.position ?? { lineNumber: 1, column: prefix.length + 1 },
      currentPath: 'src/a.ts',
      completions: () => {
        state.completionsAsked += 1
        return sources.completions ? sources.completions() : Promise.resolve([])
      },
      symbols: () => {
        state.symbolsAsked += 1
        return sources.symbols ? sources.symbols() : Promise.resolve([])
      },
      insert: (text, range) => {
        state.inserted.push({ text, start: range.start.character, end: range.end.character })
      },
      pass: () => {
        state.passed += 1
      }
    }
  }
  return state
}

describe('Tab 键处理', () => {
  it('无候选：两路都问过，然后原样放过（缩进不受影响）', async () => {
    const rec = recorder('ru')
    const decision = await runTabCompletion(rec.host)
    expect(decision).toEqual({ kind: 'pass', reason: 'no-candidate' })
    expect(rec.completionsAsked).toBe(1)
    expect(rec.symbolsAsked).toBe(1)
    expect(rec.passed).toBe(1)
    expect(rec.inserted).toEqual([])
  })

  it('有候选：写入补全文本，替换区间是 0-based', async () => {
    const rec = recorder('ru', {
      symbols: () => Promise.resolve([symbol('runTask', 'src/a.ts')])
    })
    const decision = await runTabCompletion(rec.host)
    expect(decision.kind).toBe('insert')
    expect(rec.inserted).toEqual([{ text: 'runTask', start: 0, end: 2 }])
    expect(rec.passed).toBe(0)
  })

  it('一路来源抛错时另一路照常生效（LSP 挂了 Tab 还能用符号索引）', async () => {
    const rec = recorder('ru', {
      completions: () => Promise.reject(new Error('lsp 未连接')),
      symbols: () => Promise.resolve([symbol('runTask', 'src/a.ts')])
    })
    const decision = await runTabCompletion(rec.host)
    expect(decision.kind).toBe('insert')
    expect(rec.inserted.map((item) => item.text)).toEqual(['runTask'])
  })

  it('两路都抛错：仍然放过，不把异常抛给键盘', async () => {
    const rec = recorder('ru', {
      completions: () => Promise.reject(new Error('boom')),
      symbols: () => Promise.reject(new Error('boom'))
    })
    await expect(runTabCompletion(rec.host)).resolves.toEqual({
      kind: 'pass',
      reason: 'no-candidate'
    })
    expect(rec.passed).toBe(1)
  })

  it('前缀太短不查来源：一个字母按 Tab 一定是缩进', async () => {
    const rec = recorder('r'.slice(0, MIN_TAB_PREFIX - 1))
    await runTabCompletion(rec.host)
    expect(rec.completionsAsked).toBe(0)
    expect(rec.symbolsAsked).toBe(0)
    expect(rec.passed).toBe(1)
  })
})
