/**
 * 命令面板的「命令注册表」纯逻辑（FR-1.7）。
 *
 * 面板的难点不在样式，而在**两条来源不同的命令要长得一样、查得一样、分组一样**：
 *
 * - **内置命令**：外壳自己的动作（打开工作区、保存、切侧边栏、开关终端…）。动作
 *   就在渲染进程里，注册表直接把 `run()` 挂上。
 * - **插件命令**：清单里的 `commands[]`，行为在子进程。注册表**只放元数据**（插件 id、
 *   命令名、风险级别、说明），`run` 留给调用方注入——渲染进程不可能自己执行插件
 *   代码，这一层刻意保持「无 IO」，好让过滤 / 导航 / 分发的单测不碰 IPC。
 *
 * 因此本模块是**纯函数 + 常量**：没有 React、没有 `window.aiide`。副作用集中在
 * `command-palette-store.ts`（取数、执行、错误提示、开关状态）。
 */

import type { PluginCommandSummary, PluginState } from '@ai-ide/shared-types'

/** 命令所属分组。顺序即面板上的展示顺序。 */
export type PaletteSection = 'builtin' | 'plugin'

export const PALETTE_SECTION_ORDER: readonly PaletteSection[] = ['builtin', 'plugin']

export const PALETTE_SECTION_LABELS: Record<PaletteSection, string> = {
  builtin: '内置命令',
  plugin: '插件命令'
}

/** 插件命令的风险级别在面板上的中文名（与插件面板同一套口径）。 */
export const PLUGIN_COMMAND_RISK_LABELS: Record<PluginCommandSummary['risk'], string> = {
  read: '只读',
  write: '写入',
  exec: '执行'
}

/** 风险级别中文名；未知级别原样回显（内核将来加级别时不至于显示 `undefined`）。 */
export function commandRiskLabel(risk: string): string {
  return (PLUGIN_COMMAND_RISK_LABELS as Record<string, string>)[risk] ?? risk
}

/**
 * 面板里的一行。
 *
 * `run` 只对内置命令有值；插件命令走 `dispatchCommand` 的 IPC 分支，不看这个字段。
 */
export interface PaletteCommand {
  /** 全局唯一键：内置为 `builtin:<id>`，插件为 `plugin:<pluginId>:<command>`。 */
  id: string
  section: PaletteSection
  title: string
  /** 副标题：内置给「一句话说明」，插件给「插件名 · 风险」。 */
  description: string
  /** 参与模糊匹配的别名（中文 + 英文 + 拼音首字母）。 */
  keywords: readonly string[]
  pluginId?: string
  command?: string
  risk?: PluginCommandSummary['risk']
  run?: () => void | Promise<void>
}

/** 内置命令需要的动作集。测试里传假实现，生产里接 store。 */
export interface BuiltinCommandActions {
  openWorkspace: () => void | Promise<void>
  saveActiveTab: () => void | Promise<void>
  /** 切到某个活动栏视图（资源管理器 / 搜索 / 插件 …）。 */
  showView: (view: 'explorer' | 'search' | 'ai' | 'git' | 'checkpoints' | 'permissions' | 'mcp' | 'plugin') => void
  toggleTerminal: () => void | Promise<void>
}

export interface BuiltinCommandSpec {
  id: string
  title: string
  description: string
  keywords: readonly string[]
  run: (actions: BuiltinCommandActions) => void | Promise<void>
}

/**
 * 内置命令清单。
 *
 * 只放**外壳真的做得到**的动作：每一条都对应一个已存在的 store 动作，不做「先占位
 * 后实现」——面板里点了没反应，比面板里少一行更糟。
 */
export const BUILTIN_COMMAND_SPECS: readonly BuiltinCommandSpec[] = [
  {
    id: 'workspace.open',
    title: '打开工作区…',
    description: '选一个本地目录作为工作区',
    keywords: ['open workspace', 'dkgzq', '文件夹', 'workspace'],
    run: (actions) => actions.openWorkspace()
  },
  {
    id: 'file.save',
    title: '保存当前文件',
    description: '把活动标签写回磁盘（Ctrl+S）',
    keywords: ['save', 'bc', '保存文件', 'write'],
    run: (actions) => actions.saveActiveTab()
  },
  {
    id: 'view.explorer',
    title: '切换到资源管理器',
    description: '左侧活动栏 · 文件树',
    keywords: ['explorer', 'files', 'zyglq', '文件', '目录'],
    run: (actions) => actions.showView('explorer')
  },
  {
    id: 'view.search',
    title: '切换到搜索',
    description: '左侧活动栏 · 全工作区搜索',
    keywords: ['search', 'ss', '查找', 'grep'],
    run: (actions) => actions.showView('search')
  },
  {
    id: 'view.ai',
    title: '切换到 AI 对话',
    description: '左侧活动栏 · 与模型对话',
    keywords: ['ai', 'chat', 'dh', '对话', '助手'],
    run: (actions) => actions.showView('ai')
  },
  {
    id: 'view.git',
    title: '切换到源代码管理',
    description: '左侧活动栏 · Git 状态与提交',
    keywords: ['git', 'scm', 'ydmgl', '源代码', '提交'],
    run: (actions) => actions.showView('git')
  },
  {
    id: 'view.plugin',
    title: '打开插件管理',
    description: '左侧活动栏 · 安装 / 加载 / 授权 / 命令',
    keywords: ['plugin', 'plugins', 'cj', '插件', '扩展'],
    run: (actions) => actions.showView('plugin')
  },
  {
    id: 'view.mcp',
    title: '切换到 MCP Server',
    description: '左侧活动栏 · MCP 连接与工具',
    keywords: ['mcp', 'server', 'fw', '服务'],
    run: (actions) => actions.showView('mcp')
  },
  {
    id: 'terminal.toggle',
    title: '显示 / 隐藏终端',
    description: '底部终端面板（Ctrl+`）',
    keywords: ['terminal', 'shell', 'zd', '终端', '命令行'],
    run: (actions) => actions.toggleTerminal()
  }
]

/** 把内置命令清单接上具体动作，产出面板可用的行。 */
export function builtinCommands(actions: BuiltinCommandActions): PaletteCommand[] {
  return BUILTIN_COMMAND_SPECS.map((spec) => ({
    id: `builtin:${spec.id}`,
    section: 'builtin' as const,
    title: spec.title,
    description: spec.description,
    keywords: spec.keywords,
    run: () => spec.run(actions)
  }))
}

/** 一个已安装插件的命令来源（`plugin.status` 的回包 + 插件名）。 */
export interface PluginCommandSource {
  pluginId: string
  pluginName: string
  state: PluginState
  commands: readonly PluginCommandSummary[]
}

/**
 * 把插件的命令清单拍平成面板行。
 *
 * 说明里带「插件名 · 风险」，因为面板上同名命令（比如两个插件都有 `format`）
 * 必须一眼能分清是谁的；`path_field` 之类的细节留给插件面板，塞进面板只会挤掉标题。
 */
export function pluginCommands(sources: readonly PluginCommandSource[]): PaletteCommand[] {
  const rows: PaletteCommand[] = []
  for (const source of sources) {
    for (const command of source.commands) {
      rows.push({
        id: `plugin:${source.pluginId}:${command.name}`,
        section: 'plugin',
        title: command.title || command.name,
        description: `${source.pluginName} · ${commandRiskLabel(command.risk)}`,
        keywords: [command.name, source.pluginName, source.pluginId, source.state],
        pluginId: source.pluginId,
        command: command.name,
        risk: command.risk
      })
    }
  }
  return rows
}

/**
 * 子序列模糊匹配，返回分数（越大越靠前），不匹配回 `null`。
 *
 * 规则刻意简单：按 query 的字符顺序在 text 里找，命中位置相邻加分、开头命中额外
 * 加分。不引入第三方模糊库——面板的候选只有几十行，手写 30 行比多一个依赖划算。
 */
export function fuzzyScore(query: string, text: string): number | null {
  const needle = query.trim().toLowerCase()
  if (needle.length === 0) return 0
  const haystack = text.toLowerCase()
  if (haystack.length === 0) return null

  let score = 0
  let cursor = 0
  let streak = 0
  for (const ch of needle) {
    if (ch === ' ') {
      streak = 0
      continue
    }
    const found = haystack.indexOf(ch, cursor)
    if (found < 0) return null
    // 相邻命中（gap 为 0）比跨字符命中更像「用户想要的那条」。
    streak = found === cursor && streak > 0 ? streak + 1 : found === cursor ? 1 : 0
    score += 2 + streak
    cursor = found + 1
  }
  // 命中落在开头（标题前缀）再加一点，避免「插件」永远压过「插件管理」。
  if (haystack.startsWith(needle)) score += 4
  return score
}

/** 单词级匹配：query 按空格切片，每个词都必须在某个字段里命中。 */
export function scoreCommand(command: PaletteCommand, query: string): number | null {
  const trimmed = query.trim()
  if (trimmed.length === 0) return 0

  const fields: Array<{ text: string; weight: number }> = [
    { text: command.title, weight: 3 },
    { text: command.command ?? '', weight: 2 },
    ...command.keywords.map((keyword) => ({ text: keyword, weight: 2 })),
    { text: command.description, weight: 1 }
  ]

  let total = 0
  for (const token of trimmed.split(/\s+/)) {
    let best: number | null = null
    for (const field of fields) {
      if (field.text.length === 0) continue
      const raw = fuzzyScore(token, field.text)
      if (raw === null) continue
      const weighted = raw * field.weight
      if (best === null || weighted > best) best = weighted
    }
    if (best === null) return null
    total += best
  }
  return total
}

/**
 * 过滤 + 排序。
 *
 * 同分时保持输入顺序（内置在前、插件在后），这样空 query 打开面板就是一份稳定的
 * 清单，不会因为排序不稳定而每次打开顺序都不一样。
 */
export function filterCommands(
  commands: readonly PaletteCommand[],
  query: string
): PaletteCommand[] {
  const scored: Array<{ command: PaletteCommand; score: number; order: number }> = []
  commands.forEach((command, order) => {
    const score = scoreCommand(command, query)
    if (score === null) return
    scored.push({ command, score, order })
  })
  scored.sort((a, b) => (b.score - a.score) || (a.order - b.order))
  return scored.map((entry) => entry.command)
}

export interface PaletteCommandGroup {
  section: PaletteSection
  label: string
  commands: PaletteCommand[]
}

/**
 * 分组展示：分组顺序固定（内置 → 插件），空分组直接不出现。
 *
 * 组内顺序沿用 `filterCommands` 的相关性排序——分会话里「哪个更相关」比「哪组
 * 更重要」更影响找命令的速度。
 */
export function groupCommands(commands: readonly PaletteCommand[]): PaletteCommandGroup[] {
  const groups: PaletteCommandGroup[] = []
  for (const section of PALETTE_SECTION_ORDER) {
    const rows = commands.filter((command) => command.section === section)
    if (rows.length === 0) continue
    groups.push({ section, label: PALETTE_SECTION_LABELS[section], commands: rows })
  }
  return groups
}

/** 空列表时回 -1（没有可选项），否则把索引夹到有效区间。 */
export function normalizeActiveIndex(index: number, total: number): number {
  if (total <= 0) return -1
  if (!Number.isFinite(index)) return 0
  return Math.min(Math.max(Math.trunc(index), 0), total - 1)
}

/** 上下键移动：越界回绕（末行再按 ↓ 回到首行）。 */
export function moveActiveIndex(index: number, delta: number, total: number): number {
  if (total <= 0) return -1
  const base = normalizeActiveIndex(index, total)
  if (base < 0) return 0
  return ((base + delta) % total + total) % total
}

/**
 * 错误码 → 面板提示文案。
 *
 * 这份表只说「为什么没跑成 + 下一步干什么」，因为面板是个 300px 的浮层，塞不下
 * 插件面板那种逐条 issue 列表。码的语义与 `plugin-api.ts` 的 `PLUGIN_ERROR_HINTS`
 * 一致，但措辞按「命令」场景重写（比如 6102 要指向「去插件面板加载」）。
 */
export const COMMAND_ERROR_TEXT: Record<number, string> = {
  6101: '插件已不在本机（可能被卸载）：命令没有执行。装回插件后重试。',
  6102: '插件没在运行：这条命令没有执行，去插件面板把它加载起来再试。',
  6103: '插件缺少这条命令所需的能力：命令没有执行，去插件面板授权后重试。',
  6106: '这条命令不属于该插件，或没有在清单里声明：已拦截，未执行。',
  3001: '你拒绝了这次写操作的审批：命令没有执行。需要的话再执行一次并在提示里同意。',
  6109: '插件调用宿主 API 过于频繁：本次已中止，等一会儿再试。'
}

/** 面板里展示的一行错误：`code` 为 null 表示不是结构化错误（纯文本兜底）。 */
export interface CommandErrorView {
  code: number | null
  text: string
}

/**
 * 把执行插件命令的异常翻成一行可见提示。
 *
 * `code` 优先取结构化信封里的数字，取不到才退回正则解析——信封是 main 进程
 * `checkRendererCall` 之后统一加的，正常情况下一定有。
 */
export function describeCommandError(
  error: unknown,
  context: { commandTitle: string },
  extract: { code: (error: unknown) => number | null; message: (error: unknown) => string }
): CommandErrorView {
  const code = extract.code(error)
  const known = code === null ? undefined : COMMAND_ERROR_TEXT[code]
  if (known) return { code, text: `「${context.commandTitle}」没执行：${known}` }
  const message = extract.message(error).trim()
  return {
    code,
    text: `「${context.commandTitle}」执行失败：${message || '未知错误（没有可读的错误信息）'}`
  }
}
