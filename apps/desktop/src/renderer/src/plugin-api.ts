/**
 * P1/P2/P4/P5 渲染侧 API 封装：把 `window.aiide.request` 收窄成 `plugin.*` 十个动作。
 *
 * 与 `mcp-api.ts` / `git-api.ts` 同一套路——组件不拼方法名、不自己拼参数，
 * 错误码翻译也收在这里。内核给插件单开了一段错误码（docs/14 §6.6，
 * 实现见 `crates/core/src/plugin_api.rs`）：
 *
 * - `6101` 插件不存在 / 安装目录已消失（卸载/加载/看详情时目标没了）；
 * - `6102` 插件进程崩了或起不来（工具已下架；偶发崩溃由内核自动重启，走到这一步
 *   说明没救回来；面板要显示 `last_error`）；
 * - `6103` 清单申请的能力没授齐（**进程没起**；P4 起面板可以直接补授权）；
 * - `6104` 清单里的 `pluginApi` 与宿主不兼容——安装时**只会出现在 6107 的 details 里**，
 *   加载时也会单独返回（目录里的清单被改过）；
 * - `6105` 握手超时（进程被内核杀掉，可重试）；
 * - `6106` 帧非法 / 工具集合与清单不一致（重试无意义，要作者修）；
 * - `6107` 清单不合法，`data.details[]` 一次列全所有问题；
 * - `6108` 插件工具与核心工具撞名（同样只出现在 details 里）；
 * - `6109` 宿主 API 调用超配额（P3 起才会见到，先给话术免得显示裸码）；
 * - `3001` 工具 / 命令未被放行（P5 命令面板见得到：`risk=write` 的插件命令在
 *   执行**前**要走一次审批，用户拒了就是这一条——不是插件坏了）；
 * - `5003` 账本/磁盘读写失败（库坏了就说库坏了，与 `session.*` 同口径）。
 *
 * 6107 是 P1 的验收点：「一次列全」意味着面板**不能只显示第一条**，也不能把
 * 内核拼的「（N 条问题）」当正文。所以这里除了翻译人话，还把 `details[]` 解析
 * 成结构化的 `PluginIssue[]`，让面板逐条渲染 `code + path + message`。
 *
 * P2 的验收点在 6102/6103/6105/6106：装载失败时内核**同时**把状态与原因写进账本，
 * 面板要能说清「为什么没起来」以及「下一步该做什么」（重新加载 / 去授权 / 改清单），
 * 不能只甩一个码。
 *
 * `details[]` 不是 `message` 的一部分：它走的是内核 `ErrorObject.data`，跨 IPC
 * 会被剥掉，主进程对 `plugin.*` 把载荷编进 message 尾部（见 `shared/ipc.ts`），
 * 这里负责拆回来。
 */

import {
  Methods,
  type PluginCommandParams,
  type PluginCommandResult,
  type PluginGrantParams,
  type PluginGrantResult,
  type PluginInstallParams,
  type PluginInstallResult,
  type PluginListParams,
  type PluginListResult,
  type PluginLoadParams,
  type PluginLoadResult,
  type PluginReloadParams,
  type PluginReloadResult,
  type PluginRevokeParams,
  type PluginRevokeResult,
  type PluginState,
  type PluginStatusParams,
  type PluginStatusResult,
  type PluginUninstallParams,
  type PluginUninstallResult,
  type PluginUnloadParams,
  type PluginUnloadResult
} from '@ai-ide/shared-types'
import { decodeRpcErrorPayload, stripRpcErrorPayload } from '../../shared/ipc'

const PLUGIN_ERROR_HINTS: Record<number, string> = {
  6101: '插件不存在：可能已被卸载，点「刷新」看最新清单。',
  6102: '插件进程崩了或起不来：工具已下架。偶发崩溃内核会自动重启（同一次装载最多 3 次），到这里说明没救回来——详情里是内核记的原因，修好后再点「加载」。',
  6103: '插件申请的能力还没授齐，进程没有启动：到详情里把能力逐项授权（或「全部授权」）后再点「加载」。',
  6104: '插件声明的 `pluginApi` 与宿主不兼容：需要插件作者按当前宿主 API 改版本号。',
  6105: '插件握手超时：进程已被停掉，可以点「加载」重试一次；连续超时要查插件启动逻辑。',
  6106: '插件违反协议（帧格式不对，或上报的工具与清单不一致）：重试没用，需要插件作者修。',
  6107: '插件清单不合法：下面列出了全部问题，改完清单再装一次。',
  6108: '插件工具与核心工具撞名：给工具换个名字（插件工具会加命名空间，撞的是插件内原名与核心工具）。',
  6109: '插件调用宿主 API 过于频繁，本次已中止：等一会儿再试，或让作者减少批量调用。',
  3001: '这次命令 / 工具调用没有被放行：写操作要先过审批，被拒绝就不会执行（不是插件坏了）。需要的话再执行一次并在提示里同意。',
  5003: '本地账本或安装目录读写失败：检查磁盘空间与目录权限后重试。',
  [-32602]: '参数不合法：安装要给包目录，加载/卸载/重载/看详情/执行命令要给插件 id（执行命令还要给命令名与合法入参）。'
}

/** 一条清单问题（对应内核 `Violation`）。 */
export interface PluginIssue {
  /** 细分错误码：`6107` 形状/语义、`6104` API 不兼容、`6108` 工具撞名。 */
  code: number
  /** 清单内位置，如 `$.tools[1].name`——面板可以直接把它当路标高亮。 */
  path: string
  message: string
}

/** 从主进程抛上来的错误文本里抠出 `[6107]` 这类协议码。 */
export function pluginErrorCode(message: string): number | null {
  const matched = /\[(-?\d+)\]/.exec(message)
  if (!matched) return null
  const code = Number(matched[1])
  return Number.isFinite(code) ? code : null
}

/** 错误码 → 人话；未知码返回空串，由调用方回落到原始 message。 */
export function pluginHintForError(code: number | null): string {
  if (code === null) return ''
  return PLUGIN_ERROR_HINTS[code] ?? ''
}

/** 错误原文（剥掉信封尾巴，面板与日志里只该看到人话）。 */
export function pluginErrorMessage(error: unknown): string {
  const raw = error instanceof Error ? error.message : String(error)
  return stripRpcErrorPayload(raw)
}

/**
 * 从异常里拆出 `details[]`。
 *
 * 三种情况都返回空数组，交给调用方回落成「只有一句话」的错误：不是信封（老路径 /
 * 非内核错误）、信封里没有 `data`（6101 这类单点错误）、`details` 形状不认识。
 * 宁可少列几行，也不要把半截 JSON 塞给用户。
 */
export function pluginErrorIssues(error: unknown): PluginIssue[] {
  const raw = error instanceof Error ? error.message : String(error)
  const payload = decodeRpcErrorPayload(raw)
  if (payload === null) return []

  const data = payload.data
  if (data === null || typeof data !== 'object') return []
  const details = (data as Record<string, unknown>)['details']
  if (!Array.isArray(details)) return []

  const issues: PluginIssue[] = []
  for (const item of details) {
    if (item === null || typeof item !== 'object') continue
    const record = item as Record<string, unknown>
    const code = record['code']
    const path = record['path']
    const message = record['message']
    issues.push({
      code: typeof code === 'number' && Number.isFinite(code) ? code : 6107,
      path: typeof path === 'string' ? path : '',
      message: typeof message === 'string' ? message : String(message ?? '')
    })
  }
  return issues
}

/** 把异常整理成「原文｜人话」；原文始终保留，否则用户贴不出可复现的报错。 */
export function describePluginError(error: unknown): string {
  const message = pluginErrorMessage(error)
  const hint = pluginHintForError(pluginErrorCode(message))
  return hint.length > 0 && !message.includes(hint) ? `${message}｜${hint}` : message
}

/** 清单问题的细分码 → 人话标签；认不出的码原样显示，不藏。 */
export function pluginIssueKindLabel(code: number): string {
  switch (code) {
    case 6107:
      return '清单'
    case 6104:
      return 'API 不兼容'
    case 6108:
      return '工具撞名'
    default:
      return `错误 ${code}`
  }
}

/**
 * 插件状态 → 面板上那枚小标记的文案。
 *
 * `needs_grant` 单独成态而不是「未加载」：清单合法、只差用户点授权。P2 里它意味着
 * 「装好了、但内核按 §4.2 拒绝 spawn」，要等 P4 的授权面板接上才能变成 `ready`。
 * 认不出的状态原样显示。
 */
export function pluginStateLabel(state: PluginState | string): string {
  switch (state) {
    case 'disabled':
      return '已停用'
    case 'needs_grant':
      return '待授权'
    case 'loading':
      return '加载中'
    case 'ready':
      return '运行中'
    case 'crashed':
      return '已崩溃'
    default:
      return state
  }
}

/** 状态 → 标记配色：只有 `ready` 是绿的，`crashed` 红，其余中性或警示。 */
export function pluginStateTone(state: PluginState | string): 'ok' | 'warn' | 'bad' | 'idle' {
  switch (state) {
    case 'ready':
      return 'ok'
    case 'needs_grant':
      return 'warn'
    case 'crashed':
      return 'bad'
    default:
      return 'idle'
  }
}

/** 面板上的能力列表：申请了但没授予的加 `?`，一眼看出「差哪一项」。 */
export function pluginCapabilityLabel(capability: string, granted: string[]): string {
  return granted.includes(capability) ? capability : `${capability}?`
}

/**
 * 「关键能力」白名单，与内核 `crates/core/src/plugin_api.rs::KEY_CAPABILITIES` 对齐。
 *
 * 关键与否决定**撤销的后果**：撤这三项会让内核当场把插件 `unload`（工具下架、进程停），
 * 所以 UI 必须先说清「会连带卸载」再让用户点第二下。名单写死在前端是有意的——
 * 它只用于「提前告知」，真正的判定在 `plugin.revoke` 返回的 `unloaded` 字段上，
 * 前端不拿它当授权门槛（授权与否只认 `granted_capabilities`）。
 */
export const PLUGIN_KEY_CAPABILITIES: readonly string[] = ['fs.write', 'net.fetch', 'llm.chat']

/** 是否关键能力（撤销会连带卸载）。 */
export function isKeyCapability(capability: string): boolean {
  return PLUGIN_KEY_CAPABILITIES.includes(capability)
}

/**
 * 撤销一项能力会发生什么——给二次确认用的那句实话。
 *
 * 关键能力：撤了内核立刻 `unload`，工具从模型工具表消失，正在跑的调用拿 6103/3001；
 * 普通能力：插件还在跑，只是下次碰这项能力被拒（网关先查能力再查路径/审批）。
 */
export function describeRevokeImpact(capability: string): string {
  return isKeyCapability(capability)
    ? `撤销 ${capability}（关键能力）：内核会立即卸载该插件——进程停掉、工具从工具表下架，正在进行的调用会被拒。撤销后要重新授权并「加载」才能再用。`
    : `撤销 ${capability}：插件继续运行，但下一次调用该能力会被拒绝（能力闸门在路径检查与审批之前）。`
}

export const pluginApi = {
  /** 已安装插件清单。只读，随时可拉。 */
  list: (): Promise<PluginListResult> =>
    window.aiide.request(
      Methods.plugin.list,
      {} satisfies PluginListParams
    ) as Promise<PluginListResult>,

  /** 单个插件详情（含工具与命令）。目标不存在会抛 `6101`。 */
  status: (pluginId: string): Promise<PluginStatusResult> =>
    window.aiide.request(Methods.plugin.status, {
      plugin_id: pluginId
    } satisfies PluginStatusParams) as Promise<PluginStatusResult>,

  /**
   * 按本地包目录安装。
   *
   * 只复制目录 + 写账本，**不启动进程**（P1 验收点）。清单有问题会抛 `6107`，
   * 其 `details[]` 用 `pluginErrorIssues` 取。
   */
  install: (path: string): Promise<PluginInstallResult> =>
    window.aiide.request(Methods.plugin.install, {
      path
    } satisfies PluginInstallParams) as Promise<PluginInstallResult>,

  /** 卸载：删账本 + 删安装目录；目录被占用时 `removed_dir = false`。 */
  uninstall: (pluginId: string): Promise<PluginUninstallResult> =>
    window.aiide.request(Methods.plugin.uninstall, {
      plugin_id: pluginId
    } satisfies PluginUninstallParams) as Promise<PluginUninstallResult>,

  /**
   * 装载：起子进程 + 握手，成功后工具进 agent 注册表（P2 验收点）。
   *
   * 内核侧失败会**先把状态写进账本再返回**，所以这里抛错之后紧接着拉一次 `list`
   * 就能拿到 `crashed` / `needs_grant` 与 `last_error`——调用方不要自己猜状态。
   * 参数只有 `plugin_id`：可执行文件与参数都由账本里的 `install_dir` 决定，
   * 渲染进程指不了别的东西。
   */
  load: (pluginId: string): Promise<PluginLoadResult> =>
    window.aiide.request(Methods.plugin.load, {
      plugin_id: pluginId
    } satisfies PluginLoadParams) as Promise<PluginLoadResult>,

  /**
   * 卸载（停止）：停子进程 + 下架工具，**保留安装目录与账本**。
   *
   * 返回的是「下架了几个工具」，不是「删了几个文件」——真删文件是 `uninstall`。
   */
  unload: (pluginId: string): Promise<PluginUnloadResult> =>
    window.aiide.request(Methods.plugin.unload, {
      plugin_id: pluginId
    } satisfies PluginUnloadParams) as Promise<PluginUnloadResult>,

  /**
   * 重载：停进程 → 重新读清单 → 再装载。
   *
   * 用途是「改完插件代码想立即生效」。清单在重载时被改坏了会抛 `6107`，与安装同口径。
   */
  reload: (pluginId: string): Promise<PluginReloadResult> =>
    window.aiide.request(Methods.plugin.reload, {
      plugin_id: pluginId
    } satisfies PluginReloadParams) as Promise<PluginReloadResult>,

  /**
   * 授权（P4）。`capabilities` 传空数组表示「把清单申请的全部能力授齐」。
   *
   * 内核只接受清单里声明过的能力，且会**重新校验清单**：非法项或清单外项一律拒绝
   * （`-32602`）。成功后 `granted_capabilities` 更新，状态从 `needs_grant` 回到
   * `disabled`——此时才允许 `load`。
   */
  grant: (pluginId: string, capabilities: string[] = []): Promise<PluginGrantResult> =>
    window.aiide.request(Methods.plugin.grant, {
      plugin_id: pluginId,
      capabilities
    } satisfies PluginGrantParams) as Promise<PluginGrantResult>,

  /**
   * 撤销（P4）。注意后果不对等：
   *
   * - 撤普通能力：插件照跑，下次调用该能力被拒；
   * - 撤**关键能力**（`fs.write` / `net.fetch` / `llm.chat`）：内核当场 `unload`，
   *   返回的 `unloaded` 为 `true`——调用方必须把这件事回执给用户，不能只报「已撤销」。
   *
   * 所以 UI 侧是两段式确认，见 `plugin-store.revokeCapability`。
   */
  /**
   * 执行插件声明的命令（P5 命令面板的落地通道）。
   *
   * 与 `plugin.call` 分开：命令带 `args`（按清单 `commands[].args` 校验，非法
   * `-32602`）、有独立审计 kind，`risk=write` 的命令还会在执行**前**要一次审批
   * （拒了就是 `3001`）。越权（调别的插件 / 调没声明的命令）回 `6106`，
   * 进程不在回 `6102`，插件被卸了回 `6101`。
   *
   * 返回值里的 `content` 已经被内核按 64 KB 字符边界截断过，`truncated` 为真时
   * 调用方要把「还有更多」说出来，别让用户以为命令只吐了这么点。
   */
  command: (
    pluginId: string,
    command: string,
    args?: unknown
  ): Promise<PluginCommandResult> =>
    window.aiide.request(Methods.plugin.command, {
      plugin_id: pluginId,
      command,
      ...(args === undefined ? {} : { args })
    } satisfies PluginCommandParams) as Promise<PluginCommandResult>,

  revoke: (pluginId: string, capabilities: string[]): Promise<PluginRevokeResult> =>
    window.aiide.request(Methods.plugin.revoke, {
      plugin_id: pluginId,
      capabilities
    } satisfies PluginRevokeParams) as Promise<PluginRevokeResult>
}
