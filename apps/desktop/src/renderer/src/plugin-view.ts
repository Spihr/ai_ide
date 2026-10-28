/**
 * 插件视图描述树的**解析、限幅与动作裁决**（docs/24 §4.2–§4.4）。
 *
 * 纯函数、零 React 依赖：渲染层只负责把这里吐出来的 `ViewNode` 画成控件，
 * 所有「能不能渲染、要不要截断、这条命令有没有声明」的判断都收敛在本文件。
 * 这样 vitest（node 环境、无 DOM）就能覆盖 V-4 ~ V-7 的全部判定，
 * 不必为了测限幅而搭一套渲染测试栈。
 *
 * 三条边界（刻意的）：
 * - **不抛异常**。插件给的是外部数据，任何形状错误都降级为「占位 + 诊断」，
 *   面板不能因为一个节点写错就整块白屏。
 * - **不改写内容语义**。字符串超限只截断，不做 HTML 转义——转义是渲染层的事，
 *   本文件产出的永远是纯数据，从不拼 HTML。
 * - **不判断权限**。视图动作能不能真的执行，由 `plugin.command` 网关裁决；
 *   这里只回答「按钮该不该是禁用态」（`actionState`）。
 */

/** 视图描述协议版本（docs/24 §4.2）。 */
export const VIEW_SCHEMA = 'aiide.view/1'

/** 节点总数上限（超出截断）。 */
export const MAX_VIEW_NODES = 2000
/** 树深度上限（超深子树降级为占位）。 */
export const MAX_VIEW_DEPTH = 16
/** 序列化体积上限（超出整棵树拒渲染）。 */
export const MAX_VIEW_BYTES = 256 * 1024
/** 单字符串上限（超出截断）。 */
export const MAX_VIEW_STRING = 4 * 1024
/** `list` / `table` / `tree` 行数上限（超出截断）。 */
export const MAX_VIEW_ROWS = 500
/** 诊断条数上限：插件若整棵树都是错的，没必要把 2000 条错误都收下来。 */
export const MAX_VIEW_ISSUES = 32

/** 白名单控件（docs/24 §4.3）。`color` / `button` 与 §4.3 的表述一致。 */
export const VIEW_CONTROLS = [
  'row',
  'column',
  'group',
  'tabs',
  'text',
  'stat',
  'table',
  'list',
  'tree',
  'progress',
  'toggle',
  'slider',
  'select',
  'color',
  'button'
] as const

/** 其中一个白名单控件名。 */
export type ViewControl = (typeof VIEW_CONTROLS)[number]

const CONTROL_SET: ReadonlySet<string> = new Set<string>(VIEW_CONTROLS)

/**
 * 危险协议探测（docs/24 §4.4.2）。
 *
 * 命中即把该节点标为 `tainted`：渲染层仍显示原文，但不生成任何可点击元素。
 * 白名单里确实没有 `link` 控件，这条是给「未来加了控件但忘了过安全」留的兜底。
 */
const UNSAFE_URL = /(?:https?:\/\/|data:|javascript:|file:\/\/|vbscript:|about:)/i

/** 诊断码。前端只做展示与开发排查，不参与审计（审计在 Rust 侧）。 */
export type ViewIssueCode =
  | 'schema_mismatch'
  | 'bad_shape'
  | 'too_large'
  | 'too_many_nodes'
  | 'too_deep'
  | 'unknown_type'
  | 'string_truncated'
  | 'row_truncated'
  | 'unsafe_url'
  | 'command_not_declared'

/** 一条渲染诊断。 */
export interface ViewIssue {
  code: ViewIssueCode
  /** 出错节点在树里的路径，如 `root.children[2].items`。 */
  path: string
  message: string
}

/** 视图动作：只可能是本插件的命令，走 `plugin.command` 审批链。 */
export interface ViewAction {
  command: string
  args?: unknown
}

/** `list` / `tree` / `tabs` 的一项。 */
export interface ViewItem {
  key: string
  label: string
  /** `tree` 的子项。 */
  children?: ViewItem[]
  /** `tabs` 每页的内容。 */
  content?: ViewNode[]
}

/** 解析后的节点：字段全部已归一、已限幅，渲染层可直接用。 */
export interface ViewNode {
  /** 原始 `type`（未知类型也原样保留，便于诊断文案）。 */
  type: string
  /** 是否命中白名单。`false` → 渲染占位块。 */
  known: boolean
  id?: string
  label?: string
  title?: string
  value?: string
  /** `progress` 的 0–100 百分比（从 `value` 或 `props.value` 归一）。 */
  progress?: number
  /** `toggle` 的初始状态。 */
  checked?: boolean
  /** `slider` 的三段。 */
  min?: number
  max?: number
  step?: number
  /** `slider` / `select` / `color` 的当前值（字符串形态，避免 number/string 之争）。 */
  current?: string
  /** `select` 的选项。 */
  options?: ViewItem[]
  /** `table` 的表头与行；单元格已截断、已过危险协议探测。 */
  columns?: string[]
  rows?: ViewRow[]
  /** `list` / `tree` / `tabs` 的项。 */
  items?: ViewItem[]
  /** 子节点（布局控件）。 */
  children?: ViewNode[]
  /** 动作（`button` 必填，输入控件可选）。 */
  action?: ViewAction
  /** 任一文本字段命中危险协议 → 渲染为纯文本且不产生点击元素。 */
  tainted: boolean
}

/** `table` 的一行。`tainted` 按行聚合，便于整行降级。 */
export interface ViewRow {
  cells: string[]
  tainted: boolean
}

/** 解析结果。 */
export interface ParsedView {
  /** `rejected` 表示整棵树不可渲染（schema 不符 / 体积超限 / 根节点非法）。 */
  status: 'ok' | 'rejected'
  /** 插件给的修订号，用于「同一 rev 不重复拉取」；缺省 0。 */
  rev: number
  root: ViewNode | null
  issues: ViewIssue[]
  /** 实际收下的节点数（含被降级的）。 */
  nodeCount: number
  /** 是否有任何截断发生。 */
  truncated: boolean
  /** `status === 'rejected'` 时的人话原因。 */
  error: string | null
}

interface ParseContext {
  issues: ViewIssue[]
  nodeCount: number
  truncated: boolean
  /** 已报过的未知类型，避免同一种控件写错刷满诊断。 */
  unknownTypes: Set<string>
  /** 节点配额是否已耗尽：耗尽后后续兄弟节点全部丢弃。 */
  exhausted: boolean
}

function createContext(): ParseContext {
  return {
    issues: [],
    nodeCount: 0,
    truncated: false,
    unknownTypes: new Set<string>(),
    exhausted: false
  }
}

function record(ctx: ParseContext, code: ViewIssueCode, path: string, message: string): void {
  if (ctx.issues.length >= MAX_VIEW_ISSUES) return
  ctx.issues.push({ code, path, message })
  if (TRUNCATING_ISSUES.has(code)) ctx.truncated = true
}

/** 这几类诊断意味着「内容被砍过」，需要在面板底部显式提示，而不是只躺在诊断里。 */
const TRUNCATING_ISSUES: ReadonlySet<ViewIssueCode> = new Set<ViewIssueCode>([
  'string_truncated',
  'row_truncated',
  'too_deep',
  'too_many_nodes'
])

/** 判断一段文本是否含危险协议。导出供渲染层与测试共用。 */
export function isUnsafeText(value: string): boolean {
  return UNSAFE_URL.test(value)
}

/** 序列化字节数：用 `TextEncoder` 而不是 `length`，中文按 UTF-8 算。 */
function byteLength(value: unknown): number {
  try {
    const text = JSON.stringify(value)
    if (text === undefined) return 0
    return new TextEncoder().encode(text).length
  } catch {
    // 循环引用等：当作超限处理比放过去安全。
    return Number.POSITIVE_INFINITY
  }
}

/** 取一个受限字符串字段。非字符串当缺省；超 4 KiB 截断并记一条诊断。 */
function takeText(value: unknown, path: string, ctx: ParseContext): string | undefined {
  if (typeof value !== 'string') return undefined
  let text = value
  if (text.length > MAX_VIEW_STRING) {
    text = text.slice(0, MAX_VIEW_STRING)
    record(ctx, 'string_truncated', path, `字符串超过 ${MAX_VIEW_STRING} 字符，已截断`)
  }
  if (isUnsafeText(text)) {
    record(ctx, 'unsafe_url', path, '含 http(s)/data/javascript/file 协议，按纯文本显示')
  }
  return text
}

function takeFiniteNumber(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined
}

function takeAction(value: unknown, path: string, ctx: ParseContext): ViewAction | undefined {
  if (value === null || typeof value !== 'object') return undefined
  const raw = value as Record<string, unknown>
  const command = raw['command']
  if (typeof command !== 'string' || command.trim() === '') {
    record(ctx, 'bad_shape', path, 'action 缺 command，已忽略该动作')
    return undefined
  }
  return { command, args: raw['args'] }
}

/** `props` 对象浅取值。协议里输入类控件的值都放这里。 */
function propsOf(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === 'object' ? (value as Record<string, unknown>) : {}
}

/** 归一 `list` / `tree` / `tabs` 的 items；行数超限截断。 */
function takeItems(value: unknown, path: string, ctx: ParseContext, withContent: boolean): ViewItem[] | undefined {
  if (!Array.isArray(value)) return undefined
  const items: ViewItem[] = []
  const limit = Math.min(value.length, MAX_VIEW_ROWS)
  if (value.length > MAX_VIEW_ROWS) {
    record(ctx, 'row_truncated', path, `条目 ${value.length} 条，仅显示前 ${MAX_VIEW_ROWS} 条`)
  }
  for (let index = 0; index < limit; index += 1) {
    const entry = value[index]
    const itemPath = `${path}[${index}]`
    if (entry === null || typeof entry !== 'object') {
      record(ctx, 'bad_shape', itemPath, '条目不是对象，已跳过')
      continue
    }
    const raw = entry as Record<string, unknown>
    const key = takeText(raw['key'], `${itemPath}.key`, ctx)
    const label = takeText(raw['label'], `${itemPath}.label`, ctx)
    if (key === undefined && label === undefined) {
      record(ctx, 'bad_shape', itemPath, '条目缺 key 与 label，已跳过')
      continue
    }
    const item: ViewItem = { key: key ?? label ?? '', label: label ?? key ?? '' }
    // `tabs` 的 `children` 是内容节点，`tree` 的 `children` 是子项，两者的形状不同。
    if (withContent) {
      if (raw['children'] !== undefined) {
        const content = takeChildren(raw['children'], `${itemPath}.children`, ctx, 0)
        if (content !== undefined) item.content = content
      }
    } else if (Array.isArray(raw['children'])) {
      const children = takeItems(raw['children'], `${itemPath}.children`, ctx, false)
      if (children !== undefined) item.children = children
    }
    items.push(item)
  }
  return items
}

/** `table` 归一：表头 + 行，单元格一律转字符串。 */
function takeTable(value: unknown, path: string, ctx: ParseContext): { columns?: string[]; rows?: ViewRow[] } {
  if (value === null || typeof value !== 'object') return {}
  const raw = value as Record<string, unknown>
  const result: { columns?: string[]; rows?: ViewRow[] } = {}

  if (Array.isArray(raw['columns'])) {
    result.columns = raw['columns'].map((column, index) =>
      takeText(column, `${path}.columns[${index}]`, ctx) ?? ''
    )
  }

  const rows = raw['rows']
  if (Array.isArray(rows)) {
    if (rows.length > MAX_VIEW_ROWS) {
      record(ctx, 'row_truncated', `${path}.rows`, `行数 ${rows.length}，仅显示前 ${MAX_VIEW_ROWS} 行`)
    }
    const limit = Math.min(rows.length, MAX_VIEW_ROWS)
    const taken: ViewRow[] = []
    for (let index = 0; index < limit; index += 1) {
      const row = rows[index]
      if (!Array.isArray(row)) {
        record(ctx, 'bad_shape', `${path}.rows[${index}]`, '行不是数组，已跳过')
        continue
      }
      const cells: string[] = []
      let tainted = false
      for (let cellIndex = 0; cellIndex < row.length; cellIndex += 1) {
        const cell = row[cellIndex]
        const text =
          typeof cell === 'string'
            ? cell
            : typeof cell === 'number' || typeof cell === 'boolean'
              ? String(cell)
              : ''
        const clipped = takeText(text, `${path}.rows[${index}][${cellIndex}]`, ctx) ?? ''
        if (isUnsafeText(clipped)) tainted = true
        cells.push(clipped)
      }
      taken.push({ cells, tainted })
    }
    result.rows = taken
  }

  return result
}

/** 递归归一子节点。`depth` 用于深度闸门。 */
function takeChildren(value: unknown, path: string, ctx: ParseContext, depth: number): ViewNode[] | undefined {
  if (!Array.isArray(value)) return undefined
  const children: ViewNode[] = []
  for (let index = 0; index < value.length; index += 1) {
    if (ctx.exhausted) {
      record(ctx, 'too_many_nodes', path, `节点总数超过 ${MAX_VIEW_NODES}，其余已丢弃`)
      break
    }
    const child = normalizeNode(value[index], `${path}[${index}]`, ctx, depth + 1)
    if (child !== null) children.push(child)
  }
  return children
}

/** 单个节点归一。返回 `null` 表示该节点被整块丢弃（超深 / 配额耗尽）。 */
function normalizeNode(value: unknown, path: string, ctx: ParseContext, depth: number): ViewNode | null {
  if (depth > MAX_VIEW_DEPTH) {
    record(ctx, 'too_deep', path, `树深度超过 ${MAX_VIEW_DEPTH}，该子树不再渲染`)
    return null
  }
  if (value === null || typeof value !== 'object') {
    record(ctx, 'bad_shape', path, '节点不是对象，已跳过')
    return null
  }
  if (ctx.nodeCount >= MAX_VIEW_NODES) {
    ctx.exhausted = true
    record(ctx, 'too_many_nodes', path, `节点总数超过 ${MAX_VIEW_NODES}，其余已丢弃`)
    return null
  }
  ctx.nodeCount += 1

  const raw = value as Record<string, unknown>
  const type = typeof raw['type'] === 'string' ? raw['type'] : ''
  if (type === '') {
    record(ctx, 'bad_shape', path, '节点缺 type，已跳过')
    return null
  }

  const known = CONTROL_SET.has(type)
  if (!known && !ctx.unknownTypes.has(type)) {
    ctx.unknownTypes.add(type)
    record(ctx, 'unknown_type', path, `未知控件 ${type}，渲染为占位块`)
  }

  const props = propsOf(raw['props'])
  const node: ViewNode = { type, known, tainted: false }

  const id = takeText(raw['id'], `${path}.id`, ctx)
  if (id !== undefined) node.id = id
  const label = takeText(raw['label'], `${path}.label`, ctx)
  if (label !== undefined) node.label = label
  const title = takeText(raw['title'], `${path}.title`, ctx)
  if (title !== undefined) node.title = title
  const value_ = takeText(raw['value'], `${path}.value`, ctx)
  if (value_ !== undefined) node.value = value_

  for (const field of [id, label, title, value_]) {
    if (field !== undefined && isUnsafeText(field)) node.tainted = true
  }

  const action = takeAction(raw['action'], `${path}.action`, ctx)
  if (action !== undefined) node.action = action

  switch (type) {
    case 'progress': {
      const percent = takeFiniteNumber(raw['value']) ?? takeFiniteNumber(props['value'])
      if (percent !== undefined) node.progress = Math.max(0, Math.min(100, percent))
      break
    }
    case 'toggle': {
      const checked = props['checked'] ?? raw['value']
      node.checked = checked === true || checked === 'true'
      break
    }
    case 'slider': {
      node.min = takeFiniteNumber(props['min']) ?? 0
      node.max = takeFiniteNumber(props['max']) ?? 100
      node.step = takeFiniteNumber(props['step']) ?? 1
      const current = takeFiniteNumber(props['value']) ?? node.min
      node.current = String(current)
      break
    }
    case 'select': {
      const options = takeItems(raw['items'], `${path}.items`, ctx, false)
      if (options !== undefined) node.options = options
      const current = takeText(props['value'], `${path}.props.value`, ctx)
      if (current !== undefined) node.current = current
      break
    }
    case 'color': {
      const current = takeText(props['value'], `${path}.props.value`, ctx)
      // 只认 `#rgb` / `#rrggbb`：`color` 的输入框不该接受任意字符串。
      if (current !== undefined && /^#(?:[0-9a-f]{3}|[0-9a-f]{6})$/i.test(current)) node.current = current
      break
    }
    case 'table': {
      const table = takeTable(raw, path, ctx)
      if (table.columns !== undefined) node.columns = table.columns
      if (table.rows !== undefined) node.rows = table.rows
      if ((table.rows ?? []).some((row) => row.tainted)) node.tainted = true
      break
    }
    case 'list':
    case 'tree':
    case 'tabs': {
      const items = takeItems(raw['items'], `${path}.items`, ctx, type === 'tabs')
      if (items !== undefined) node.items = items
      break
    }
    default: {
      const children = takeChildren(raw['children'], `${path}.children`, ctx, depth)
      if (children !== undefined) node.children = children
      break
    }
  }

  // `tabs` 的每一页也可能带 `children`（协议允许 `items[].children` 与直挂并存）。
  if (type === 'tabs' && raw['children'] !== undefined && node.items === undefined) {
    const children = takeChildren(raw['children'], `${path}.children`, ctx, depth)
    if (children !== undefined) node.children = children
  }

  return node
}

/**
 * 解析 `plugin.call` 返回的 `ui` 字段（docs/24 §4.2）。
 *
 * 返回 `status: 'rejected'` 的三种情况都是「继续渲染只会更糟」：schema 不对、
 * 体积超限、根节点不是对象。其余问题一律降级渲染 + 诊断。
 */
export function parseView(payload: unknown): ParsedView {
  const ctx = createContext()

  if (payload === null || typeof payload !== 'object') {
    return {
      status: 'rejected',
      rev: 0,
      root: null,
      issues: [{ code: 'bad_shape', path: 'root', message: '视图描述不是对象' }],
      nodeCount: 0,
      truncated: false,
      error: '视图描述不是对象'
    }
  }

  const raw = payload as Record<string, unknown>
  const schema = raw['schema']
  if (schema !== VIEW_SCHEMA) {
    const actual = typeof schema === 'string' ? schema : '（缺失）'
    return {
      status: 'rejected',
      rev: 0,
      root: null,
      issues: [
        {
          code: 'schema_mismatch',
          path: 'root.schema',
          message: `协议版本不符：期望 ${VIEW_SCHEMA}，实际 ${actual}`
        }
      ],
      nodeCount: 0,
      truncated: false,
      error: `协议版本不符：期望 ${VIEW_SCHEMA}，实际 ${actual}`
    }
  }

  const bytes = byteLength(payload)
  if (bytes > MAX_VIEW_BYTES) {
    const message = `视图描述 ${Math.round(bytes / 1024)} KiB，超过 ${Math.round(MAX_VIEW_BYTES / 1024)} KiB 上限`
    return {
      status: 'rejected',
      rev: 0,
      root: null,
      issues: [{ code: 'too_large', path: 'root', message }],
      nodeCount: 0,
      truncated: false,
      error: message
    }
  }

  const revValue = raw['rev']
  const rev = typeof revValue === 'number' && Number.isFinite(revValue) ? revValue : 0

  const root = normalizeNode(raw['root'], 'root', ctx, 0)
  if (root === null) {
    return {
      status: 'rejected',
      rev,
      root: null,
      issues: ctx.issues.length > 0 ? ctx.issues : [{ code: 'bad_shape', path: 'root', message: '根节点不可用' }],
      nodeCount: ctx.nodeCount,
      truncated: ctx.truncated,
      error: '根节点不可用'
    }
  }

  return {
    status: 'ok',
    rev,
    root,
    issues: ctx.issues,
    nodeCount: ctx.nodeCount,
    truncated: ctx.truncated,
    error: null
  }
}

/**
 * 按钮该不该是禁用态（docs/24 §4.4.1）。
 *
 * 只判「命令是否在本插件 `commands[]` 里」——真正的权限/风险判定在
 * `plugin.command` 网关，前端再判一遍只会出现两套结论。
 */
export function actionState(
  node: ViewNode,
  commands: readonly string[]
): { enabled: boolean; reason: string | null } {
  if (node.action === undefined) return { enabled: true, reason: null }
  if (commands.length === 0) {
    // 清单没读到（安装目录不可读）时不应把按钮全禁掉：交给网关报错更诚实。
    return { enabled: true, reason: null }
  }
  if (!commands.includes(node.action.command)) {
    return { enabled: false, reason: `命令 ${node.action.command} 未在本插件声明` }
  }
  return { enabled: true, reason: null }
}
