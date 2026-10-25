/**
 * Monaco 的四个语言服务 provider（定义 / 引用 / 悬浮 / 补全）。
 *
 * 这是「协议面 → 编辑器」的最后一段：`lsp.request` 的四种只读查询在这里翻成
 * Monaco 认识的形状。分三层写，是为了让纯逻辑能在 vitest 里跑（单测环境没有
 * DOM、没有 worker、更没有 `monaco-editor` 运行时）：
 *
 * 1. **转换函数**（`toLocationHints` / `toHoverHint` / `toCompletionHints`）：
 *    纯函数，只吃 `LspRequestResult`，吐出**0-based** 的中间形态；
 * 2. **`createLspProviders`**：把「取连接 → 发请求 → 转换 → 失败吞掉」串起来，
 *    依赖（`activeServerId` / `request`）由外部注入，所以没有连接时可以单测
 *    「回空结果」；
 * 3. **`registerLspProviders`**：唯一一处接触 `monaco-editor` 的地方，把中间形态
 *    补回 Monaco 的 **1-based** `Range` / `Position` 并注册。
 *
 * **坐标系要说清，否则一定差一**：本协议面（内核 DTO）行列 1-based；LSP 线格式
 * 0-based；Monaco 的 `IRange`（`lineNumber` / `column`）**也是 1-based**。于是
 * 「1-based 转 0-based」发生在协议面 → 中间形态这一步（做运算、夹取、排序用 0-based
 * 更不容易错），注册时再加回去。补全项尤其要注意：Monaco 拿 `range` 决定「替换哪段
 * 前缀」，给错了会把用户已输入的单词也吃掉或留重复。
 *
 * **未连接一律回空集合/`null`，绝不抛错**：语言服务器是增强，没装
 * `typescript-language-server` 是常态；抛错会让 Monaco 在控制台刷一片红，还可能
 * 弹「provider 出错」的提示，看起来像 IDE 坏了。
 *
 * **查询以磁盘文本为准**：内核 `lsp.request` 读的是文件内容（见 `lsp_api.rs`），
 * 未保存的缓冲不参与——要查最新代码得先保存。这是协议面刻意的取舍（查询不该隐式
 * 写盘，也不该把整篇缓冲每次都传一遍），在此记录以免误判成 bug。
 */

import type * as Monaco from 'monaco-editor'

import type {
  LspCompletionList,
  LspPosition,
  LspRequestMethod,
  LspRequestResult
} from '@ai-ide/shared-types'

import { positionToZeroBased, rangeToZeroBased, type ZeroBasedPosition } from './lsp-api'
import { activeServerId as currentServerId } from './lsp-store'
import { lspApi } from './lsp-api'

/** 0-based 半开区间：注册时统一 `+1` 变成 Monaco 的 1-based `Range`。 */
export interface ZeroBasedRange {
  start: ZeroBasedPosition
  end: ZeroBasedPosition
}

/** 一个可跳转位置（定义 / 引用）。`path` 缺失表示非文件 scheme（`git:` 等）。 */
export interface LocationHint {
  uri: string
  path?: string
  range: ZeroBasedRange
}

/** 悬浮内容。`null` 由调用方解释成「没有可展示内容」（不是错误）。 */
export interface HoverHint {
  contents: string
  range?: ZeroBasedRange
}

/** 一条补全候选；`range` 是「这次补全会替换掉的区间」（0-based）。 */
export interface CompletionHint {
  label: string
  kind?: number
  detail?: string
  documentation?: string
  insertText?: string
  sortText?: string
  range: ZeroBasedRange
}

/** 查询目标：文件绝对路径 + 1-based 位置（与协议面一致，注册时无需换算）。 */
export interface LspQueryTarget {
  path: string
  position: LspPosition
}

/**
 * `definition` / `references`：服务端给的 `locations` 直接翻成可跳转位置。
 *
 * 没有 `path` 的位置也保留（`uri` 仍在）：`git:` 这类 URI Monaco 自己认识，丢了
 * 反而少一条结果。
 */
export function toLocationHints(result: LspRequestResult): LocationHint[] {
  const locations = result.locations ?? []
  const hints: LocationHint[] = []
  for (const location of locations) {
    const range = rangeToZeroBased(location.range)
    hints.push(
      location.path === undefined || location.path.length === 0
        ? { uri: location.uri, range }
        : { uri: location.uri, path: location.path, range }
    )
  }
  return hints
}

/** `hover`：空白内容按「没有悬浮」处理（服务端常回 `""` 表示「这里没东西」）。 */
export function toHoverHint(result: LspRequestResult): HoverHint | null {
  const hover = result.hover
  if (hover === undefined) return null
  const contents = hover.contents.trim()
  if (contents.length === 0) return null
  return hover.range === undefined
    ? { contents }
    : { contents, range: rangeToZeroBased(hover.range) }
}

/**
 * 光标的 0-based 位置当替换区间（无单词信息时的兜底：纯插入，不覆盖前缀）。
 */
export function caretRange(position: LspPosition): ZeroBasedRange {
  const caret = positionToZeroBased(position)
  return { start: caret, end: { ...caret } }
}

/**
 * `completion`：`items` 翻成 Monaco 的 `CompletionItem`。
 *
 * `replaceRange` 由调用方按「光标处的单词边界」算好传进来（Monaco 侧用
 * `model.getWordUntilPosition`，那里的列是 1-based，转 0-based 后交过来）；不传
 * 就退化成「第一行第一列的空区间」，也只在不关心替换范围时才有意义。
 * `is_incomplete` 不进返回值：这里给的是数组，编辑器自己会按已输入前缀再过滤一遍。
 */
export function toCompletionHints(
  result: LspRequestResult,
  replaceRange: ZeroBasedRange = caretRange({ line: 1, character: 1 })
): CompletionHint[] {
  const list: LspCompletionList | undefined = result.completions
  const items = list?.items ?? []
  return items.map((item) => {
    const hint: CompletionHint = { label: item.label, range: replaceRange }
    if (item.kind !== undefined) hint.kind = item.kind
    if (item.detail !== undefined) hint.detail = item.detail
    if (item.documentation !== undefined) hint.documentation = item.documentation
    if (item.insert_text !== undefined) hint.insertText = item.insert_text
    if (item.sort_text !== undefined) hint.sortText = item.sort_text
    return hint
  })
}

/** provider 需要的两个依赖；生产里接 `lsp-store` + `lsp-api`，测试里传假的。 */
export interface LspProviderHost {
  /** 当前文件可用的连接句柄；没有（未启动 / 降级 / 崩溃）时回 `null`。 */
  activeServerId: () => string | null
  request: (input: {
    serverId: string
    method: LspRequestMethod
    path: string
    position: LspPosition
  }) => Promise<LspRequestResult>
}

/** 四个 provider 的纯逻辑形态（返回中间形态，不碰 Monaco）。 */
export interface LspProviders {
  definition: (target: LspQueryTarget) => Promise<LocationHint[]>
  references: (target: LspQueryTarget) => Promise<LocationHint[]>
  hover: (target: LspQueryTarget) => Promise<HoverHint | null>
  completion: (
    target: LspQueryTarget,
    replaceRange?: ZeroBasedRange
  ) => Promise<CompletionHint[]>
}

/**
 * 装配四个 provider。
 *
 * 统一的三条纪律写在 `query` 里：没连接直接回 `null`（不请求）、请求失败回 `null`
 * （不抛给 Monaco）、正常结果原样往下传。
 */
export function createLspProviders(host: LspProviderHost): LspProviders {
  const query = async (
    method: LspRequestMethod,
    target: LspQueryTarget
  ): Promise<LspRequestResult | null> => {
    const serverId = host.activeServerId()
    if (serverId === null) return null
    try {
      return await host.request({
        serverId,
        method,
        path: target.path,
        position: target.position
      })
    } catch {
      // 10003（句柄过期）/ 协议错 / 服务器崩：对编辑器来说都是「这次没有结果」。
      return null
    }
  }

  return {
    definition: async (target) => {
      const result = await query('definition', target)
      return result === null ? [] : toLocationHints(result)
    },
    references: async (target) => {
      const result = await query('references', target)
      return result === null ? [] : toLocationHints(result)
    },
    hover: async (target) => {
      const result = await query('hover', target)
      return result === null ? null : toHoverHint(result)
    },
    completion: async (target, replaceRange) => {
      const result = await query('completion', target)
      if (result === null) return []
      return toCompletionHints(
        result,
        replaceRange ?? caretRange(target.position)
      )
    }
  }
}

/** 生产环境的默认依赖：连接取 `lsp-store` 的当前可用句柄，查询走 `lsp.request`。 */
export const lspProviders: LspProviders = createLspProviders({
  activeServerId: currentServerId,
  request: (input) => lspApi.request(input)
})

/**
 * 注册 provider 的语言。
 *
 * Monaco 的 selector 不接受通配语言（`'*'` 只有在「按语言字符串精确匹配」的那条
 * 路径上才生效），所以只能逐个语言注册。这份清单与 `lsp-store` 的
 * `LANGUAGE_LABELS` 保持同一套语言 id——那边能显示名字的，这边就注册 provider。
 */
export const LSP_PROVIDER_LANGUAGES: readonly string[] = [
  'typescript',
  'typescriptreact',
  'javascript',
  'javascriptreact',
  'python',
  'rust',
  'go',
  'java',
  'c',
  'cpp',
  'csharp',
  'json',
  'yaml',
  'html',
  'css',
  'scss',
  'php',
  'ruby',
  'vue',
  'svelte'
]

/** 注册所需的 Monaco 面（只取用到的几项，便于测试替换）。 */
export interface MonacoProviderApi {
  languages: {
    registerDefinitionProvider: typeof Monaco.languages.registerDefinitionProvider
    registerReferenceProvider: typeof Monaco.languages.registerReferenceProvider
    registerHoverProvider: typeof Monaco.languages.registerHoverProvider
    registerCompletionItemProvider: typeof Monaco.languages.registerCompletionItemProvider
  }
  Uri: typeof Monaco.Uri
  Range: typeof Monaco.Range
  Position: typeof Monaco.Position
}

/**
 * URI → 绝对路径。
 *
 * Monaco 的 `uri.path` 是正斜杠且经过百分号编码（Windows 上是 `/e%3A/...`），
 * 送进内核前要还原成 `e:/...` 这种带盘符的形式。非 `file` scheme 原样回
 * `uri.toString()`：内核那边会自己判「不是本地文件」，不必在这里猜。
 */
export function pathFromUriPath(uriPath: string): string {
  const decoded = decodeURIComponent(uriPath).replace(/\\/g, '/')
  return decoded.replace(/^\/+(?=[A-Za-z]:)/, '')
}

function hintToRange(Range: MonacoProviderApi['Range'], range: ZeroBasedRange): InstanceType<
  MonacoProviderApi['Range']
> {
  // 0-based → Monaco 的 1-based（`lineNumber` / `column` 从 1 起）。
  return new Range(
    range.start.line + 1,
    range.start.character + 1,
    range.end.line + 1,
    range.end.character + 1
  )
}

function hintToLocation(api: MonacoProviderApi, hint: LocationHint) {
  return {
    uri: hint.path === undefined ? api.Uri.parse(hint.uri) : api.Uri.parse(uriFromPath(api, hint.path, hint.uri)),
    range: hintToRange(api.Range, hint.range)
  }
}

/** 有 `path` 就按本地文件建 URI（内核给的 `uri` 可能是 `file://` 的另一种编码）。 */
function uriFromPath(api: MonacoProviderApi, path: string, fallback: string): string {
  const normalized = path.replace(/\\/g, '/')
  return /^[A-Za-z]:\//.test(normalized) || normalized.startsWith('/')
    ? api.Uri.file(normalized).toString()
    : fallback
}

/**
 * 注册四个 provider。返回「撤销注册」函数。
 *
 * 幂等：Monaco 的 provider 注册是全局的，重复注册会让每次查询跑两遍、定义列表
 * 出现重复项。模块级 `installed` 记住当前那批 disposable，重复调用直接返回上一次的
 * 撤销函数。
 */
let installed: { dispose: () => void } | null = null

export function registerLspProviders(
  api: MonacoProviderApi,
  providers: LspProviders = lspProviders
): () => void {
  if (installed !== null) return installed.dispose

  const disposables: Array<{ dispose: () => void }> = []

  const targetOf = (model: Monaco.editor.ITextModel, position: Monaco.Position): LspQueryTarget => ({
    path: pathFromUriPath(model.uri.path),
    position: { line: position.lineNumber, character: position.column }
  })

  for (const language of LSP_PROVIDER_LANGUAGES) {
    disposables.push(
      api.languages.registerDefinitionProvider(language, {
        provideDefinition: async (model, position) =>
          (await providers.definition(targetOf(model, position))).map((hint) =>
            hintToLocation(api, hint)
          )
      }),
      api.languages.registerReferenceProvider(language, {
        provideReferences: async (model, position) =>
          (await providers.references(targetOf(model, position))).map((hint) =>
            hintToLocation(api, hint)
          )
      }),
      api.languages.registerHoverProvider(language, {
        provideHover: async (model, position) => {
          const hint = await providers.hover(targetOf(model, position))
          if (hint === null) return null
          // `contents` 是服务端给的 Markdown 原文，Monaco 的 `MarkdownString` 直接吃。
          return hint.range === undefined
            ? { contents: [{ value: hint.contents }] }
            : { contents: [{ value: hint.contents }], range: hintToRange(api.Range, hint.range) }
        }
      }),
      api.languages.registerCompletionItemProvider(language, {
        provideCompletionItems: async (model, position) => {
          const word = model.getWordUntilPosition(position)
          // Monaco 的单词信息是 1-based 列，先落成 0-based 再交给转换层。
          const replaceRange: ZeroBasedRange = {
            start: { line: position.lineNumber - 1, character: word.startColumn - 1 },
            end: { line: position.lineNumber - 1, character: word.endColumn - 1 }
          }
          const hints = await providers.completion(targetOf(model, position), replaceRange)
          if (hints.length === 0) return { suggestions: [] }
          return {
            suggestions: hints.map((hint) => {
              // `insertText` 在 Monaco 的类型里是**必填**（缺省时才回落到 `label`，
              // 所以这里必须显式补上，不能留空让编辑器自己猜）。
              const item: Monaco.languages.CompletionItem = {
                label: hint.label,
                kind: hint.kind ?? 0,
                insertText: hint.insertText ?? hint.label,
                range: hintToRange(api.Range, hint.range)
              }
              if (hint.detail !== undefined) item.detail = hint.detail
              if (hint.documentation !== undefined) {
                item.documentation = { value: hint.documentation }
              }
              if (hint.sortText !== undefined) item.sortText = hint.sortText
              return item
            })
          }
        }
      })
    )
  }

  const dispose = (): void => {
    for (const disposable of disposables) disposable.dispose()
    if (installed !== null && installed.dispose === dispose) installed = null
  }
  installed = { dispose }
  return dispose
}

/** 仅供单测：清掉「已注册」标记（真注册过的 disposable 由调用方自己撤）。 */
export function resetLspProviderRegistrationForTests(): void {
  installed = null
}
