/**
 * P1/P2/P4 渲染侧状态：已安装插件清单、选中项详情、装卸载结果，以及能力授权/撤销。
 *
 * 与 `mcp-store` / `permission-store` 同一形态：单例 + `useSyncExternalStore`，
 * 动作返回布尔表示「是否真的做成了」，失败原因落到 `error`（外加 6107 的
 * `errorDetails`）。
 *
 * 三个刻意的取舍：
 *
 * 1. **清单只按动作拉**。插件没有推送通道（装/卸/装载都由本面板发起），多一条轮询
 *    只是让内核白跑路由；进面板、点刷新、每个动作做完各拉一次就够。
 * 2. **刷新不清错误**。6107 往往一次列好几条清单问题，用户得逐条对着改清单；如果
 *    刷新顺手把错误擦了，他就再也没法把那些 `path` 抄下来。错误只由「下一次动作」
 *    或用户主动「知道了」清掉。
 * 3. **装载失败也要刷新**。P2 的失败路径是「内核先写账本（`crashed` /
 *    `needs_grant` + `last_error`）再返回错误」，所以 `catch` 里必须补一次
 *    `refreshPlugins()`，否则面板会停在「已停用」这种旧状态上，用户看不出崩溃原因。
 *    刷新不清 error，于是「码 + 人话」与「行上的红色已崩溃」能同时看到。
 */

import { useSyncExternalStore } from 'react'

import type { PluginStatusResult, PluginSummary } from '@ai-ide/shared-types'

import {
  describePluginError,
  pluginApi,
  pluginErrorIssues,
  type PluginIssue
} from './plugin-api'

/** 一次只跑一个的装卸/装载/授权动作（`installing` 单独一位，安装不针对某一行）。 */
export type PluginBusyAction =
  | 'install'
  | 'uninstall'
  | 'load'
  | 'unload'
  | 'reload'
  | 'grant'
  | 'revoke'

/**
 * 待二次确认的撤销（P4）。
 *
 * **为什么确认态放在 store 而不是组件里**：撤销关键能力会连带卸载插件（进程停、
 * 工具下架），属于「点错了没法反悔」的动作。把「第一次点击只是上膛」这件事放进
 * 状态机，动作函数自己就能保证「没上膛不发请求」——组件漏写一个 if、或者别处
 * 直接调 `revokeCapability`，也不会绕过确认。
 */
export interface PendingRevoke {
  pluginId: string
  capability: string
}

export interface PluginStoreState {
  /** 已安装插件（内核按安装时间排序）。 */
  plugins: PluginSummary[]
  loading: boolean
  /** 正在安装（安装按钮禁用；一次只允许一个包，避免并发覆盖同一个 id）。 */
  installing: boolean
  /**
   * 正在装卸/装载的插件 id（UI 只禁用那一行）。
   *
   * 五个动作共用一个槽位：同一时刻只允许一个动作在飞，否则「点加载又点卸载」
   * 这种乱序请求会把账本状态和面板显示搅在一起。
   */
  busyPlugin: string | null
  /**
   * 正在跑的是哪个动作。与 `busyPlugin` 一起用：只知道「这一行忙着」的话，
   * 五个按钮会同时显示「处理中…」，用户看不出自己点的是加载还是卸载。
   */
  busyAction: PluginBusyAction | null
  /** 安装路径输入框的原文：故意保留用户写坏的样子，方便改。 */
  installPath: string
  /** 当前选中的插件 id（详情从 `plugin.status` 拉）。 */
  selectedId: string | null
  /** 选中插件详情；`null` 表示没选或还没拉回来。 */
  detail: PluginStatusResult | null
  detailLoading: boolean
  hint: string | null
  error: string | null
  /** 6107 的 `details[]`；其他错误为空数组。 */
  errorDetails: PluginIssue[]
  /** 正在等第二次点击确认的撤销；`null` 表示没有上膛的撤销。 */
  pendingRevoke: PendingRevoke | null
}

function initialState(): PluginStoreState {
  return {
    plugins: [],
    loading: false,
    installing: false,
    busyPlugin: null,
    busyAction: null,
    installPath: '',
    selectedId: null,
    detail: null,
    detailLoading: false,
    hint: null,
    error: null,
    errorDetails: [],
    pendingRevoke: null
  }
}

let state: PluginStoreState = initialState()

const listeners = new Set<() => void>()

function setState(patch: Partial<PluginStoreState>): void {
  state = { ...state, ...patch }
  for (const listener of listeners) listener()
}

export function getPluginState(): PluginStoreState {
  return state
}

export function subscribePlugins(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function usePlugins(): PluginStoreState {
  return useSyncExternalStore(subscribePlugins, getPluginState, getPluginState)
}

/** 测试用：模块级单例跨用例不清就会串状态。 */
export function resetPluginState(): void {
  state = initialState()
  for (const listener of listeners) listener()
}

/**
 * 拉清单。
 *
 * 装完/卸完都会走这里，所以必须**保住选中项**：如果选中的插件还在，详情不动；
 * 已经被卸载（或换了 id）就把它连同详情一起清掉，免得面板继续显示一个不存在的插件。
 */
export async function refreshPlugins(): Promise<boolean> {
  setState({ loading: true })
  try {
    const result = await pluginApi.list()
    const stillThere =
      state.selectedId !== null && result.plugins.some((plugin) => plugin.id === state.selectedId)
    // 上膛的撤销也要跟着清单校验：插件被别的窗口卸掉了，确认按钮就无处可点。
    const armed = state.pendingRevoke
    const armedStillThere =
      armed !== null && result.plugins.some((plugin) => plugin.id === armed.pluginId)

    setState({
      loading: false,
      plugins: result.plugins,
      selectedId: stillThere ? state.selectedId : null,
      detail: stillThere ? state.detail : null,
      detailLoading: stillThere ? state.detailLoading : false,
      pendingRevoke: armedStillThere ? armed : null
    })
    return true
  } catch (error) {
    // 清单拉不回来时保留上一次的清单：空白列表 + 一句报错会让人以为插件全丢了。
    setState({
      loading: false,
      error: describePluginError(error),
      errorDetails: pluginErrorIssues(error)
    })
    return false
  }
}

/** 面板首次出现时拉一次；正在拉就不再叠一次。 */
export function initPlugins(): void {
  if (state.loading) return
  void refreshPlugins()
}

export function setInstallPath(path: string): void {
  setState({ installPath: path })
}

/**
 * 按本地包目录安装。
 *
 * 路径为空时**不发请求**：`-32602` 说的是同一件事，但让用户白等一次 IPC 往返、
 * 再读一句协议码，不如就地提示。
 */
export async function installPlugin(): Promise<boolean> {
  if (state.installing) return false
  const path = state.installPath.trim()
  if (path.length === 0) {
    setState({ error: '先填插件包目录的绝对路径（含 plugin.json 的那一层）。', errorDetails: [] })
    return false
  }

  setState({ installing: true, error: null, errorDetails: [], hint: null })
  try {
    const result = await pluginApi.install(path)
    const plugin = result.plugin
    setState({
      installing: false,
      installPath: '',
      hint: result.replaced
        ? `已更新「${plugin.name}」到 v${plugin.version}（覆盖了同 id 的旧版本）。`
        : `已安装「${plugin.name}」v${plugin.version}，进程未启动；点「加载」才会握手并注册工具。`
    })
    await refreshPlugins()
    // 装完直接把新插件选中：用户下一步大概率就是看它的能力与工具。
    // 覆盖安装时它可能本来就是选中的，先清掉选中态，否则 `selectPlugin` 会按
    // 「同一个 id 不重复拉」直接返回，详情还停在上一个版本上。
    if (state.selectedId === plugin.id) setState({ selectedId: null, detail: null })
    await selectPlugin(plugin.id)
    return true
  } catch (error) {
    setState({
      installing: false,
      error: describePluginError(error),
      errorDetails: pluginErrorIssues(error)
    })
    return false
  }
}

/**
 * 卸载：删账本 + 删安装目录。
 *
 * `removed_dir = false` 不算失败——账本已经清了，只是目录被占用（比如编辑器开着
 * 插件里的文件）。这时要如实告诉用户「条目没了、目录还在」，而不是报一个红错。
 */
export async function uninstallPlugin(pluginId: string): Promise<boolean> {
  if (state.busyPlugin !== null) return false
  setState({
    busyPlugin: pluginId,
    busyAction: 'uninstall',
    error: null,
    errorDetails: [],
    hint: null
  })
  try {
    const result = await pluginApi.uninstall(pluginId)
    const wasSelected = state.selectedId === pluginId
    setState({
      busyPlugin: null,
      busyAction: null,
      hint: result.removed_dir
        ? `已卸载「${result.plugin_id}」，安装目录已删除。`
        : `「${result.plugin_id}」已从账本移除，但安装目录没能删掉（可能被占用），可手工清理。`,
      ...(wasSelected ? { selectedId: null, detail: null, detailLoading: false } : {})
    })
    await refreshPlugins()
    return true
  } catch (error) {
    setState({
      busyPlugin: null,
      busyAction: null,
      error: describePluginError(error),
      errorDetails: pluginErrorIssues(error)
    })
    return false
  }
}

/**
 * 装载：起子进程 + 握手。成功后工具进 agent 注册表，失败时内核已把
 * `crashed` / `needs_grant` 与 `last_error` 写进账本（P2 验收点）。
 */
export async function loadPlugin(pluginId: string): Promise<boolean> {
  return runRuntimeAction(pluginId, 'load')
}

/** 停止：停进程 + 下架工具，保留账本与安装目录。 */
export async function unloadPlugin(pluginId: string): Promise<boolean> {
  return runRuntimeAction(pluginId, 'unload')
}

/** 重载：停 → 重读清单 → 再装载（改完插件代码让它生效）。 */
export async function reloadPlugin(pluginId: string): Promise<boolean> {
  return runRuntimeAction(pluginId, 'reload')
}

type RuntimeAction = 'load' | 'unload' | 'reload'

/**
 * 三个装载动作的公共骨架：置忙 → 调内核 → 拉清单 → 拉详情 → 收忙。
 *
 * 失败分支里 `refreshPlugins()` 必须在 `setState(error)` **之后**：刷新不清 error，
 * 而它自己失败时（`5003`）会覆盖 error，后者才是用户需要看到的最后一句真话。
 * 成功分支只提示「做了什么」，不猜内核内部细节（工具数用返回值里的真实长度）。
 */
async function runRuntimeAction(pluginId: string, action: RuntimeAction): Promise<boolean> {
  if (state.busyPlugin !== null) return false
  setState({
    busyPlugin: pluginId,
    busyAction: action,
    error: null,
    errorDetails: [],
    hint: null
  })
  try {
    if (action === 'unload') {
      const result = await pluginApi.unload(pluginId)
      setState({
        busyPlugin: null,
        busyAction: null,
        hint:
          result.removed_tools > 0
            ? `已停止「${result.plugin_id}」，下架 ${result.removed_tools} 个工具（安装目录与账本保留）。`
            : `「${result.plugin_id}」本来就没在运行，账本已同步为未加载。`
      })
    } else {
      const result =
        action === 'load' ? await pluginApi.load(pluginId) : await pluginApi.reload(pluginId)
      const verb = action === 'load' ? '已装载' : '已重载'
      setState({
        busyPlugin: null,
        busyAction: null,
        hint:
          result.tools.length > 0
            ? `${verb}「${result.plugin.name}」，注册 ${result.tools.length} 个工具。`
            : `${verb}「${result.plugin.name}」，但它没有声明任何工具。`
      })
    }
  } catch (error) {
    setState({
      busyPlugin: null,
      busyAction: null,
      error: describePluginError(error),
      errorDetails: pluginErrorIssues(error)
    })
    // 失败也要刷新：状态与 last_error 是内核写进账本的，不拉就看不到崩溃原因。
    await refreshPlugins()
    await fetchPluginDetail(pluginId)
    return false
  }

  await refreshPlugins()
  await fetchPluginDetail(pluginId)
  return true
}

/**
 * 授权（P4）：把清单申请到的能力写进账本，并当场灌进运行态。
 *
 * `capabilities` 留空表示「清单申请的全部能力」——面板的「全部授权」走这条。授权
 * **不会**把插件拉起来（内核只记账本 + 运行态快照），所以提示里要说清下一步是
 * 「加载」，否则用户会以为授完就该在工具表里看到它。
 */
export async function grantCapabilities(
  pluginId: string,
  capabilities: string[] = []
): Promise<boolean> {
  if (state.busyPlugin !== null) return false
  const wanted = capabilities.map((item) => item.trim()).filter((item) => item.length > 0)
  if (capabilities.length > 0 && wanted.length === 0) {
    setState({ error: '要授予的能力不能是空字符串：逐项授权就传具体能力名，全部授权传空数组。', errorDetails: [] })
    return false
  }

  setState({
    busyPlugin: pluginId,
    busyAction: 'grant',
    error: null,
    errorDetails: [],
    hint: null,
    pendingRevoke: null
  })
  try {
    const result = await pluginApi.grant(pluginId, wanted)
    const granted = result.plugin.granted_capabilities
    const requested = result.plugin.capabilities
    const added = wanted.length > 0 ? wanted.join('、') : `清单申请的全部能力（${requested.join('、')}）`
    const complete = requested.every((capability) => granted.includes(capability))
    setState({
      busyPlugin: null,
      busyAction: null,
      hint: complete
        ? `已授予「${result.plugin.name}」：${added}。授权只记账本，进程还没起——点「加载」启动它。`
        : `已授予「${result.plugin.name}」：${added}。还差 ${requested.length - granted.length} 项没授，授齐后才能加载。`
    })
  } catch (error) {
    setState({
      busyPlugin: null,
      busyAction: null,
      error: describePluginError(error),
      errorDetails: pluginErrorIssues(error)
    })
    // 被拒的授权内核也会落一行审计（`decision = deny`），但账本没变；拉一次只为对齐
    // 「别的窗口刚改过」这类偏差，失败信息本身仍以 error 为准。
    await refreshPlugins()
    await fetchPluginDetail(pluginId)
    return false
  }

  await refreshPlugins()
  await fetchPluginDetail(pluginId)
  return true
}

/**
 * 撤销（P4）：**两段式**——第一次调用只上膛，第二次才发请求。
 *
 * 关键能力（`fs.write` / `net.fetch` / `llm.chat`）一撤，内核立刻卸载插件：进程停、
 * 工具下架、状态退回 `needs_grant`。这种「点错没法反悔」的动作必须问一声，而且
 * 问的话得在**发请求之前**说清楚（`describeRevokeImpact`）。
 *
 * 确认态按 `(插件, 能力)` 配对：点了 A 又点 B，等于把准星移到 B 上，A 自动撤回
 * 上膛状态——否则用户以为自己在确认 B，实际撤的是 A。
 *
 * 返回值：`true` 只在真正发出去并成功时为真；只上膛或失败都返回 `false`。
 */
export async function revokeCapability(pluginId: string, capability: string): Promise<boolean> {
  if (state.busyPlugin !== null) return false
  const name = capability.trim()
  if (name.length === 0) {
    setState({ error: '要撤销的能力不能为空。', errorDetails: [] })
    return false
  }

  const armed =
    state.pendingRevoke !== null &&
    state.pendingRevoke.pluginId === pluginId &&
    state.pendingRevoke.capability === name
  if (!armed) {
    setState({ pendingRevoke: { pluginId, capability: name }, error: null, errorDetails: [] })
    return false
  }

  setState({
    busyPlugin: pluginId,
    busyAction: 'revoke',
    error: null,
    errorDetails: [],
    hint: null,
    pendingRevoke: null
  })
  try {
    const result = await pluginApi.revoke(pluginId, [name])
    // `unloaded` 是内核的权威结论，不是外壳按名单猜的：关键能力被撤时它才为真。
    setState({
      busyPlugin: null,
      busyAction: null,
      hint: result.unloaded
        ? `已撤销「${name}」：这是关键能力，插件已连带卸载——进程已停、工具已下架、状态退回「待授权」。要再用请重新授权后点「加载」。`
        : `已撤销「${name}」：插件仍在运行，但下一次调用这项能力会被内核拒绝。要再用请重新授权。`
    })
  } catch (error) {
    setState({
      busyPlugin: null,
      busyAction: null,
      pendingRevoke: null,
      error: describePluginError(error),
      errorDetails: pluginErrorIssues(error)
    })
    await refreshPlugins()
    await fetchPluginDetail(pluginId)
    return false
  }

  await refreshPlugins()
  await fetchPluginDetail(pluginId)
  return true
}

/** 收起「上膛」状态（用户点「取消」或换了选中项）。 */
export function cancelRevoke(): void {
  if (state.pendingRevoke === null) return
  setState({ pendingRevoke: null })
}

/**
 * 选中一个插件并拉详情；传 `null` 收起详情。
 *
 * 详情是「工具/命令清单」的唯一来源——账本只存计数，`plugin.status` 才回读安装
 * 目录里的清单。拉不到详情不影响「插件存在」这个事实，所以失败只落 error，列表照旧。
 */
export async function selectPlugin(pluginId: string | null): Promise<void> {
  if (pluginId === state.selectedId) return
  if (pluginId === null) {
    setState({ selectedId: null, detail: null, detailLoading: false, pendingRevoke: null })
    return
  }

  // 换选中项时把上膛的撤销撤回：准星换了，用户以为自己确认的是新插件的能力。
  setState({ selectedId: pluginId, detail: null, detailLoading: true, pendingRevoke: null })
  await fetchPluginDetail(pluginId)
}

/**
 * 重新拉一次某个插件的详情，**允许 id 与当前选中项相同**。
 *
 * `selectPlugin` 对同一个 id 直接返回（避免重复点同一行时白拉），但装载/停止/重载
 * 之后状态与工具都变了，同一个 id 也必须重拉，所以单独留这个内部入口。
 * 只在它仍是当前选中项时落地，避免慢响应覆盖用户新点的行。
 */
async function fetchPluginDetail(pluginId: string): Promise<void> {
  if (state.selectedId !== pluginId) return
  setState({ detailLoading: true })
  try {
    const detail = await pluginApi.status(pluginId)
    if (state.selectedId !== pluginId) return
    setState({ detail, detailLoading: false })
  } catch (error) {
    if (state.selectedId !== pluginId) return
    setState({
      detailLoading: false,
      error: describePluginError(error),
      errorDetails: pluginErrorIssues(error)
    })
  }
}

/** 收起错误块（6107 的明细一起清）。 */
export function dismissPluginError(): void {
  setState({ error: null, errorDetails: [] })
}

export function dismissPluginHint(): void {
  setState({ hint: null })
}
