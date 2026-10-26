/**
 * Monaco 四个语言服务 provider 的测试。
 *
 * 不加载 `monaco-editor` 运行时（单测环境没有 DOM / worker）：`registerLspProviders`
 * 的 Monaco 面收在 `MonacoProviderApi` 这个窄接口里，这里用几个假类顶替
 * `Uri` / `Range` / `Position`，把「注册了什么、回调被调时返回什么」全部抓下来断言。
 *
 * 钉的几条都是**看着对、其实错**的地方：
 *
 * - **1-based ↔ 0-based 只换算一次**：协议面与 Monaco 都是 1-based，中间形态是
 *   0-based；漏了或重了这一步，跳转会整体偏一格，补全还会把已输入的前缀吃掉；
 * - **未连接 / 请求失败回空**：绝不能抛给 Monaco，否则控制台刷红、编辑器像坏了；
 * - **重复注册返回同一份撤销函数**：Monaco 的 provider 是全局的，注册两遍会让
 *   每次查询跑两遍、定义列表出现重复项；
 * - **补全必须带 `insertText`**：Monaco 的类型里它是必填，缺省时才回落到 `label`；
 * - **`uri.path` 是百分号编码的**：`/e%3A/...` 不还原成 `e:/...`，内核会认不出盘符。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { LspRequestResult } from '@ai-ide/shared-types'

import {
  LSP_PROVIDER_LANGUAGES,
  caretRange,
  createLspProviders,
  pathFromUriPath,
  registerLspProviders,
  resetLspProviderRegistrationForTests,
  toCompletionHints,
  toHoverHint,
  toLocationHints,
  type LspProviderHost,
  type LspProviders,
  type MonacoProviderApi,
  type ZeroBasedRange
} from './lsp-providers'

/** Monaco 的 `Range` 替身：只保留 provider 真正读的四个字段。 */
class FakeRange {
  constructor(
    readonly startLineNumber: number,
    readonly startColumn: number,
    readonly endLineNumber: number,
    readonly endColumn: number
  ) {}
}

const TS_PATH = 'e:/ai_ide/apps/a.ts'

/** 一个最小的 `LspRequestResult`（只有 `locations` 非空，其余按协议面留默认）。 */
function result(overrides: Partial<LspRequestResult> = {}): LspRequestResult {
  return {
    server_id: 'srv-1',
    method: 'definition',
    path: TS_PATH,
    locations: [],
    ...overrides
  }
}

interface Registration {
  kind: 'definition' | 'reference' | 'hover' | 'completion'
  language: string
  provider: Record<string, (...args: never[]) => unknown>
  disposed: boolean
  dispose: () => void
}

/** 假的 Monaco 面：把每次注册连同 provider 实现一起收进数组。 */
function fakeMonaco(): { api: MonacoProviderApi; registrations: Registration[] } {
  const registrations: Registration[] = []

  const record =
    (kind: Registration['kind']) =>
    (language: string, provider: Registration['provider']): { dispose: () => void } => {
      const entry: Registration = {
        kind,
        language,
        provider,
        disposed: false,
        dispose: () => {
          entry.disposed = true
        }
      }
      registrations.push(entry)
      return { dispose: entry.dispose }
    }

  const api = {
    languages: {
      registerDefinitionProvider: record('definition'),
      registerReferenceProvider: record('reference'),
      registerHoverProvider: record('hover'),
      registerCompletionItemProvider: record('completion')
    },
    // `parse(uriFromPath(...))` 走的就是这里：一律只回显收到的字符串，断言好写。
    Uri: {
      file: (path: string) => ({ path, toString: () => `file:///${path}` }),
      parse: (value: string) => ({ path: value, toString: () => value })
    },
    Range: FakeRange,
    Position: class {}
  } as unknown as MonacoProviderApi

  return { api, registrations }
}

/** 假模型：`uri.path` 用 Windows 上真实的百分号编码形态，`getWordUntilPosition` 给 1-based 列。 */
function fakeModel(uriPath = '/e%3A/ai_ide/apps/a.ts') {
  return {
    uri: { path: uriPath },
    getWordUntilPosition: () => ({ startColumn: 5, endColumn: 8 })
  }
}

function hintsProviders(overrides: Partial<Record<keyof LspProviders, unknown>> = {}): LspProviders {
  return {
    definition: vi.fn(async () => []),
    references: vi.fn(async () => []),
    hover: vi.fn(async () => null),
    completion: vi.fn(async () => []),
    ...overrides
  } as unknown as LspProviders
}

beforeEach(() => {
  resetLspProviderRegistrationForTests()
})

describe('协议面 → 中间形态的转换', () => {
  it('定义 / 引用：行列转 0-based，非文件 scheme 保留 uri', () => {
    const hints = toLocationHints(
      result({
        locations: [
          {
            uri: 'file:///e%3A/ai_ide/apps/a.ts',
            path: TS_PATH,
            range: { start: { line: 3, character: 5 }, end: { line: 3, character: 9 } }
          },
          {
            uri: 'git:/repo/a.ts?ref',
            range: { start: { line: 1, character: 1 }, end: { line: 1, character: 1 } }
          }
        ]
      })
    )

    expect(hints).toHaveLength(2)
    expect(hints[0]).toMatchObject({
      path: TS_PATH,
      range: { start: { line: 2, character: 4 }, end: { line: 2, character: 8 } }
    })
    // 非文件 scheme：`path` 缺失但 `uri` 必须留着，否则 Monaco 少一条可跳转结果。
    expect(hints[1]?.path).toBeUndefined()
    expect(hints[1]?.uri).toBe('git:/repo/a.ts?ref')
  })

  it('反向区间夹成正向（服务器偶尔给 end < start）', () => {
    const hints = toLocationHints(
      result({
        locations: [
          {
            uri: 'file:///a.ts',
            path: TS_PATH,
            range: { start: { line: 5, character: 8 }, end: { line: 5, character: 2 } }
          }
        ]
      })
    )

    expect(hints[0]?.range.start).toEqual({ line: 4, character: 7 })
    expect(hints[0]?.range.end).toEqual({ line: 4, character: 7 })
  })

  it('悬浮：缺字段 / 空白内容都按「没有内容」处理，有内容则去掉首尾空白', () => {
    expect(toHoverHint(result())).toBeNull()
    expect(toHoverHint(result({ hover: { contents: '   \n ' } }))).toBeNull()

    const hint = toHoverHint(
      result({
        hover: {
          contents: '  const a: number  ',
          range: { start: { line: 2, character: 1 }, end: { line: 2, character: 6 } }
        }
      })
    )
    expect(hint?.contents).toBe('const a: number')
    expect(hint?.range?.start).toEqual({ line: 1, character: 0 })
  })

  it('补全：snake_case 字段拍平成 Monaco 口径，缺省替换区间是「插入点」', () => {
    const hints = toCompletionHints(
      result({
        completions: {
          is_incomplete: false,
          items: [
            {
              label: 'log',
              kind: 2,
              detail: '(method) console.log',
              documentation: '打印',
              insert_text: 'log()',
              sort_text: '01'
            },
            { label: 'length' }
          ]
        }
      })
    )

    expect(hints[0]).toMatchObject({
      label: 'log',
      kind: 2,
      detail: '(method) console.log',
      documentation: '打印',
      insertText: 'log()',
      sortText: '01'
    })
    // 不传替换区间时退化成「第一行第一列的空区间」：纯插入，不覆盖前缀。
    expect(hints[0]?.range).toEqual({ start: { line: 0, character: 0 }, end: { line: 0, character: 0 } })
    expect(hints[1]?.insertText).toBeUndefined()
    expect(toCompletionHints(result())).toEqual([])
  })

  it('caretRange 与 pathFromUriPath：0-based 空区间 / 百分号编码还原盘符', () => {
    expect(caretRange({ line: 4, character: 7 })).toEqual({
      start: { line: 3, character: 6 },
      end: { line: 3, character: 6 }
    })

    expect(pathFromUriPath('/e%3A/ai_ide/apps/a.ts')).toBe('e:/ai_ide/apps/a.ts')
    expect(pathFromUriPath('/home/user/a.ts')).toBe('/home/user/a.ts')
    expect(pathFromUriPath('/c%3A/ws/a.ts')).toBe('c:/ws/a.ts')
  })
})

describe('createLspProviders', () => {
  const target = { path: TS_PATH, position: { line: 3, character: 5 } }

  it('没有可用连接时一律回空，且不发请求', async () => {
    const host: LspProviderHost = { activeServerId: () => null, request: vi.fn() }
    const providers = createLspProviders(host)

    expect(await providers.definition(target)).toEqual([])
    expect(await providers.references(target)).toEqual([])
    expect(await providers.hover(target)).toBeNull()
    expect(await providers.completion(target)).toEqual([])
    expect(host.request).not.toHaveBeenCalled()
  })

  it('请求失败（句柄过期 / 服务器崩）吞成空结果，不抛给 Monaco', async () => {
    const host: LspProviderHost = {
      activeServerId: () => 'srv-1',
      request: vi.fn(async () => {
        throw new Error('10003: server unknown')
      })
    }
    const providers = createLspProviders(host)

    await expect(providers.definition(target)).resolves.toEqual([])
    await expect(providers.hover(target)).resolves.toBeNull()
    await expect(providers.completion(target)).resolves.toEqual([])
  })

  it('有连接时把 method / path / position 原样交给 lsp.request', async () => {
    const request = vi.fn(async () => result({ locations: [] }))
    const providers = createLspProviders({ activeServerId: () => 'srv-1', request })

    await providers.references(target)
    expect(request).toHaveBeenCalledWith({
      serverId: 'srv-1',
      method: 'references',
      path: TS_PATH,
      position: { line: 3, character: 5 }
    })
  })

  it('补全没给替换区间时用光标位置兜底（纯插入）', async () => {
    const request = vi.fn(async () =>
      result({
        method: 'completion',
        completions: { is_incomplete: false, items: [{ label: 'log' }] }
      })
    )
    const providers = createLspProviders({ activeServerId: () => 'srv-1', request })

    const hints = await providers.completion(target)
    expect(hints[0]?.range).toEqual({ start: { line: 2, character: 4 }, end: { line: 2, character: 4 } })
  })
})

describe('registerLspProviders', () => {
  it('四种 provider × 每个语言各注册一次', () => {
    const { api, registrations } = fakeMonaco()
    registerLspProviders(api, hintsProviders())

    expect(registrations).toHaveLength(LSP_PROVIDER_LANGUAGES.length * 4)
    for (const language of LSP_PROVIDER_LANGUAGES) {
      const kinds = registrations.filter((entry) => entry.language === language).map((e) => e.kind)
      expect(kinds.sort()).toEqual(['completion', 'definition', 'hover', 'reference'])
    }
  })

  it('重复注册返回同一份撤销函数，不再注册第二遍', () => {
    const { api, registrations } = fakeMonaco()
    const first = registerLspProviders(api, hintsProviders())
    const second = registerLspProviders(api, hintsProviders())

    expect(second).toBe(first)
    expect(registrations).toHaveLength(LSP_PROVIDER_LANGUAGES.length * 4)
  })

  it('撤销后全部 disposable 被 dispose，且可以重新注册（HMR 场景）', () => {
    const { api, registrations } = fakeMonaco()
    const dispose = registerLspProviders(api, hintsProviders())

    dispose()
    expect(registrations.every((entry) => entry.disposed)).toBe(true)

    registerLspProviders(api, hintsProviders())
    expect(registrations).toHaveLength(LSP_PROVIDER_LANGUAGES.length * 8)
  })

  it('定义 provider：把中间形态补回 1-based Range，模型路径经 uri 解码后交给上游', async () => {
    const definition = vi.fn(async () => [
      {
        uri: 'file:///e%3A/ai_ide/apps/a.ts',
        path: TS_PATH,
        range: { start: { line: 2, character: 4 }, end: { line: 2, character: 9 } }
      }
    ])
    const { api, registrations } = fakeMonaco()
    registerLspProviders(api, hintsProviders({ definition }))

    const entry = registrations.find((item) => item.kind === 'definition' && item.language === 'typescript')
    expect(entry).toBeDefined()

    const model = fakeModel()
    const locations = (await entry?.provider.provideDefinition?.(model as never, {
      lineNumber: 3,
      column: 5
    } as never)) as Array<{ uri: { toString: () => string }; range: FakeRange }>

    // 上游拿到的是「解码后的绝对路径 + 1-based 位置」，与协议面同一套坐标。
    expect(definition).toHaveBeenCalledWith({
      path: TS_PATH,
      position: { line: 3, character: 5 }
    })
    expect(locations).toHaveLength(1)
    // 有 `path` 时按本地文件**重建** URI（内核给的 `uri` 可能是另一种 `file://` 编码），
    // 所以断言的是重建后的形态，而不是原样透传服务器那个字符串。
    expect(locations[0]?.uri.toString()).toBe('file:///e:/ai_ide/apps/a.ts')
    expect(locations[0]?.range).toBeInstanceOf(FakeRange)
    expect(locations[0]?.range).toMatchObject({
      startLineNumber: 3,
      startColumn: 5,
      endLineNumber: 3,
      endColumn: 10
    })
  })

  it('悬浮 provider：没有内容回 null，有内容带上 Markdown 正文与区间', async () => {
    const hover = vi
      .fn()
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce({
        contents: 'const a: number',
        range: { start: { line: 0, character: 0 }, end: { line: 0, character: 5 } }
      })
    const { api, registrations } = fakeMonaco()
    registerLspProviders(api, hintsProviders({ hover }))

    const entry = registrations.find((item) => item.kind === 'hover' && item.language === 'typescript')
    const model = fakeModel()

    expect(await entry?.provider.provideHover?.(model as never, { lineNumber: 1, column: 1 } as never)).toBeNull()

    const shown = (await entry?.provider.provideHover?.(model as never, { lineNumber: 1, column: 1 } as never)) as {
      contents: Array<{ value: string }>
      range: FakeRange
    }
    expect(shown.contents).toEqual([{ value: 'const a: number' }])
    expect(shown.range).toMatchObject({ startLineNumber: 1, startColumn: 1, endLineNumber: 1, endColumn: 6 })
  })

  it('补全 provider：用光标处的单词算替换区间，缺 insertText 时回落 label', async () => {
    const completion = vi.fn(async (_target: unknown, replaceRange?: ZeroBasedRange) => {
      const range = replaceRange ?? caretRange({ line: 3, character: 5 })
      return [
        { label: 'log', kind: 2, insertText: 'log()', range },
        { label: 'length', range }
      ]
    })
    const { api, registrations } = fakeMonaco()
    registerLspProviders(api, hintsProviders({ completion }))

    const entry = registrations.find((item) => item.kind === 'completion' && item.language === 'typescript')
    const model = fakeModel()
    const provided = (await entry?.provider.provideCompletionItems?.(model as never, {
      lineNumber: 3,
      column: 8
    } as never)) as {
      suggestions: Array<{
        label: string
        kind: number
        insertText: string
        range: FakeRange
      }>
    }

    // 单词区间（列 5..8，1-based）落成 0-based 后交给转换层，回写时再加回 1。
    expect(completion).toHaveBeenCalledWith(
      { path: TS_PATH, position: { line: 3, character: 8 } },
      { start: { line: 2, character: 4 }, end: { line: 2, character: 7 } }
    )
    expect(provided.suggestions).toHaveLength(2)
    expect(provided.suggestions[0]).toMatchObject({ label: 'log', kind: 2, insertText: 'log()' })
    expect(provided.suggestions[0]?.range).toMatchObject({
      startLineNumber: 3,
      startColumn: 5,
      endLineNumber: 3,
      endColumn: 8
    })
    // 没给 kind 的候选回落 0（Monaco 的 `Text`），没给 insertText 的回落 label。
    expect(provided.suggestions[1]).toMatchObject({ label: 'length', kind: 0, insertText: 'length' })
  })

  it('补全 provider：上游回空时给空列表，不构造任何候选', async () => {
    const { api, registrations } = fakeMonaco()
    registerLspProviders(api, hintsProviders({ completion: vi.fn(async () => []) }))

    const entry = registrations.find((item) => item.kind === 'completion' && item.language === 'rust')
    const provided = (await entry?.provider.provideCompletionItems?.(fakeModel() as never, {
      lineNumber: 1,
      column: 1
    } as never)) as { suggestions: unknown[] }

    expect(provided.suggestions).toEqual([])
  })
})
