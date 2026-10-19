/**
 * M3 外壳侧引用逻辑的单测。
 *
 * 这一层刻意做成纯函数（不碰 IPC、不碰 React），所以边界可以直接钉死：什么样的
 * `@` 算触发、候选怎么筛、chip 怎么变成 `context.refs`、超限那句话怎么说。
 * IPC 那一侧的行为在 `ai-store.test.ts` 里验。
 */

import { describe, expect, it } from 'vitest'

import {
  MAX_COMPLETIONS,
  buildChatContext,
  buildContextRefs,
  chipLabel,
  contextKey,
  estimateExceeded,
  findTrigger,
  formatTokenHint,
  makeTokenEstimate,
  normalizePath,
  rankCompletions,
  stripTrigger,
  toRelativePath,
  type CompletionItem
} from './context-completion'

const ITEMS: CompletionItem[] = [
  { kind: 'file', path: 'src/app.ts', label: 'app.ts' },
  { kind: 'folder', path: 'src', label: 'src' },
  { kind: 'file', path: 'docs/app-notes.md', label: 'app-notes.md' },
  { kind: 'file', path: 'README.md', label: 'README.md' }
]

describe('@ 触发解析', () => {
  it('行首或空白后的 @ 才算触发，并带上已敲入的查询串', () => {
    expect(findTrigger('@ma', 3)).toEqual({ start: 0, query: 'ma' })
    expect(findTrigger('看看 @src/ap', 10)).toEqual({ start: 3, query: 'src/ap' })
    // 刚敲下 `@` 还没输字：查询串为空，此时应该列全部候选。
    expect(findTrigger('@', 1)).toEqual({ start: 0, query: '' })
  })

  it('紧跟在非空白字符后面的 @ 不算触发（邮箱、`a@b` 这类别弹菜单）', () => {
    expect(findTrigger('someone@example.com', 18)).toBeNull()
    expect(findTrigger('a@b', 3)).toBeNull()
  })

  it('@ 与光标之间出现空白或第二个 @ 就不触发：用户已经在写句子了', () => {
    expect(findTrigger('@main.ts 帮我看下', 15)).toBeNull()
    expect(findTrigger('@a@b', 4)).toBeNull()
  })

  it('没有 @ 时不触发', () => {
    expect(findTrigger('随便写点什么', 6)).toBeNull()
  })

  it('摘掉触发片段只删 `@查询`，前后文原样保留', () => {
    const trigger = findTrigger('请参考 @src/ap 这一行', 11)
    expect(trigger).not.toBeNull()
    expect(stripTrigger('请参考 @src/ap 这一行', trigger!, 11)).toBe('请参考  这一行')
  })
})

describe('候选过滤与排序', () => {
  it('末段名完全匹配优于路径中包含，不相关的直接丢掉', () => {
    expect(rankCompletions(ITEMS, 'app.ts').map((item) => item.path)).toEqual(['src/app.ts'])
  })

  it('文件夹的末段名命中时排在文件前（引用整个目录通常是更粗的第一步）', () => {
    const ranked = rankCompletions(ITEMS, 'src')
    expect(ranked.map((item) => item.path)).toEqual(['src', 'src/app.ts'])
  })

  it('查询为空时列出全部候选，文件夹仍排在最前', () => {
    const ranked = rankCompletions(ITEMS, '')
    expect(ranked).toHaveLength(ITEMS.length)
    expect(ranked[0]?.kind).toBe('folder')
  })

  it('已经选过的引用不再出现在候选里，重复路径大小写无关去重', () => {
    const ranked = rankCompletions([...ITEMS, { kind: 'file', path: 'SRC/App.ts', label: 'App.ts' }], '', {
      exclude: [contextKey('file', 'src/app.ts')]
    })
    const paths = ranked.map((item) => item.path.toLowerCase())
    expect(paths).not.toContain('src/app.ts')
    expect(ranked).toHaveLength(ITEMS.length - 1)
  })

  it('候选有数量上限：菜单太长会盖住输入框', () => {
    const many = Array.from({ length: MAX_COMPLETIONS + 6 }, (_, index) => ({
      kind: 'file' as const,
      path: `src/f${index}.ts`,
      label: `f${index}.ts`
    }))
    expect(rankCompletions(many, '')).toHaveLength(MAX_COMPLETIONS)
  })
})

describe('路径归一', () => {
  it('反斜杠、重复斜杠与 `./` 前缀都归一成正斜杠相对路径', () => {
    expect(normalizePath('src\\app.ts')).toBe('src/app.ts')
    expect(normalizePath('a//b\\\\c')).toBe('a/b/c')
    expect(normalizePath('./src/app.ts')).toBe('src/app.ts')
  })

  it('绝对路径压成相对工作区根，大小写不敏感；工作区外原样保留交给内核报错', () => {
    expect(toRelativePath('E:\\proj\\src\\app.ts', 'E:\\proj')).toBe('src/app.ts')
    expect(toRelativePath('e:/PROJ/src/app.ts', 'E:\\proj')).toBe('src/app.ts')
    expect(toRelativePath('E:\\proj', 'E:\\proj')).toBe('')
    expect(toRelativePath('D:\\other\\x.ts', 'E:\\proj')).toBe('D:/other/x.ts')
  })

  it('chip 文案取末段名', () => {
    expect(chipLabel('src/app.ts')).toBe('app.ts')
    expect(chipLabel('src\\app.ts')).toBe('app.ts')
  })
})

describe('请求体构造', () => {
  const CHIPS = [
    { kind: 'file' as const, path: 'src\\app.ts', label: 'app.ts' },
    { kind: 'folder' as const, path: 'src', label: 'src' }
  ]

  it('chip 按顺序变成 refs；行区间与选区原文一律为 null（本轮是整文件注入）', () => {
    expect(buildContextRefs(CHIPS)).toEqual([
      {
        kind: 'file',
        path: 'src/app.ts',
        start_line: null,
        end_line: null,
        text: null,
        label: 'app.ts'
      },
      { kind: 'folder', path: 'src', start_line: null, end_line: null, text: null, label: 'src' }
    ])
  })

  it('没有引用时 context 为 null：M2 的行为不能被 M3 改掉', () => {
    expect(buildChatContext([], 'ws-1')).toBeNull()
  })

  it('有引用时带上 workspace_id，compress 保持 null（裁剪交给内核默认策略）', () => {
    expect(buildChatContext(CHIPS, 'ws-1')).toEqual({
      workspace_id: 'ws-1',
      refs: buildContextRefs(CHIPS),
      compress: null
    })
  })
})

describe('token 估算与超限提示', () => {
  it('模型没声明窗口时不判超限：无从判断就别打扰用户', () => {
    expect(estimateExceeded(999_999, null)).toBe(false)
    expect(makeTokenEstimate(999_999, null, 'Fake Mini').hint).toBeNull()
  })

  it('刚好等于窗口不算超限（提示只留给真的会被上游退回的情况）', () => {
    expect(estimateExceeded(1000, 1000)).toBe(false)
    expect(formatTokenHint({ tokens: 1000, limit: 1000, label: 'Fake Mini' })).toBeNull()
  })

  it('超限时给出带量级、模型名与下一步动作的整句提示', () => {
    const estimate = makeTokenEstimate(12_345, 8000, 'Fake Mini')
    expect(estimate.exceeded).toBe(true)
    expect(estimate.hint).toContain('12,345')
    expect(estimate.hint).toContain('8,000')
    expect(estimate.hint).toContain('Fake Mini')
    expect(estimate.hint).toContain('减少引用')
  })

  it('模型没给显示名时用「当前模型」兜底', () => {
    const hint = formatTokenHint({ tokens: 200, limit: 100, label: null })
    expect(hint).toContain('当前模型')
  })
})
