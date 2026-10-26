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
  pluginHintForError,
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

/**
 * 批量装载里**单个**插件的失败。
 *
 * 内核把「谁没起来、为什么」放在 `items[].error` 里（`code` + `message`），而不是
 * 让整批变成一次 JSON-RPC 错误——5 个里挂 1 个时，调用方要的是另外 4 个的结果
 * 加上这 1 个的原因。所以这里也逐条留，不折叠成一句话。
 */
export interface PluginBatchFailure {
  pluginId: string
  code: number
  message: string
  /** 由 `code` 翻出来的人话（认不出的码为空串）。 */
  hint: string
}

/**
 * 「全部启用」一次批量的进度与结局。
 *
 * `done` 的推进有两条路：装载期间靠 `plugin.state_changed`（内核每落一次账本就广播
 * 一次，`ready` / `crashed` / `needs_grant` 都是终态），请求返回后按 `items` 一次性
 * 补齐。两条路都走 `counted` 去重，否则同一次装载会把进度算两遍——面板上的
 * 「3 / 5」突然跳到「6 / 5」比不显示进度更糟。
 */
export interface PluginBatchState {
  /** 本次请求的插件 id（去重保序，与内核返回的 `items` 同序）。 */
  requested: string[]
  total: number
  done: number
  /** 已经计入 `done` 的插件 id。 */
  counted: string[]
  succeeded: string[]
  failures: PluginBatchFailure[]
  /** 内核归一后的实际并发度；请求还没回来时为 `null`。 */
  maxConcurrent: number | null
  running: boolean
}

/** 装载后能到达的终态：出现其中之一就代表这个插件已经结算了。 */
const BATCH_TERMINAL_STATES: readonly string[] = ['ready', 'crashed', 'needs_grant']

/** 可以被「全部启用」挑中的状态：已经跑着的（`ready`）不必再起，待授权的不许起。 */
const BATCH_CANDIDATE_STATES: readonly string[] = ['disabled', 'crashed']

/**
 * 「全部启用」的候选：已停用 + 已崩溃。
 *
 * `ready` 排除在外——它本来就在跑，再 `load` 一次只会被内核按 `alive()` 复用，
 * 白白多一次握手往返；`needs_grant` 也排除，那会必然拿到 `6103`（内核不 spawn），
 * 批量里塞一堆注定失败的行，只会把真正的失败淹掉。
 */
export function loadablePluginIds(plugins: readonly PluginSummary[]): string[] {
  return plugins
    .filter((plugin) => BATCH_CANDIDATE_STATES.includes(plugin.state))
    .map((plugin) => plugin.id)
}

/** 批量结果的一句话小结（面板与提示共用，便于单测钉住措辞里的数字）。 */
export function describePluginBatch(batch: PluginBatchState): string {
  const concurrency = batch.maxConcurrent === null ? '' : `（并发 ${batch.maxConcurrent}）`
  if (batch.running) {
    return `正在并发装载 ${batch.total} 个插件${concurrency}：已完成 ${batch.done} / ${batch.total}。`
  }
  const failed = batch.failures.length
  const succeeded = batch.succeeded.length
  if (failed === 0) {
    return `已装载 ${succeeded} 个插件${concurrency}，全部成功。`
  }
  if (succeeded === 0) {
    return `${batch.total} 个插件都没能装载${concurrency}：失败原因见下方清单，修好后可单独重试。`
  }
  return `已装载 ${batch.total} 个插件${concurrency}：成功 ${succeeded} 个、失败 ${failed} 个。失败的那几个不影响已成功的插件，它们已在运行。`
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
  /** 最近一次「全部启用」的进度与结局；`null` 表示这次会话还没跑过批量。 */
  batch: PluginBatchState | null
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
    pendingRevoke: null,
    batch: null
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
 * 「全部启用」（P5 切片八）：把当前所有「已停用 / 已崩溃」的插件一次性交给
 * `plugin.load_many` 并发装载。
 *
 * ## 为什么面板要这条，而不是循环调 `load`
 *
 * 循环 `load` 是**串行**的：每个插件都要握手一轮，10 个插件就是 10 次往返叠着
 * 等，中途一个超时还会把后面全堵住。`load_many` 在内核侧用 `buffer_unordered`
 * 并发起进程，并且**单个失败只落在它自己那一项上**——这正是面板需要的语义：
 * 5 个里挂 1 个，另外 4 个照常起来。
 *
 * ## 进度从哪来
 *
 * 请求是一次性的，`onProgress` 这类回调协议里没有。所以进度取自**内核本来就会
 * 广播的 `plugin.state_changed`**（每次落账本都发一次），由
 * [`notePluginBatchProgress`] 折算成「已完成 N / 总数」；请求返回后再按 `items`
 * 一次性补齐（两者共用 `counted` 去重）。
 *
 * 失败项**不会被丢掉**：逐条进 `batch.failures`，连带 `hint`（6102/6103 的人话），
 * 面板按行列出插件名与原因。成功项不受影响，`hint` 里明确说清这一点。
 */
export async function loadPluginsMany(pluginIds?: string[]): Promise<boolean> {
  if (state.batch?.running === true) return false
  if (state.busyPlugin !== null || state.installing) {
    // 单插件动作与批量同时进行会让 `busyPlugin` 指向一个「正在被批量装」的插件，
    // 面板上的按钮状态就说不清了。宁可让用户等这一次。
    setState({ error: '正在处理另一个装载请求，等它结束后再批量启用。', errorDetails: [] })
    return false
  }

  const requested = dedupeIds(pluginIds ?? loadablePluginIds(state.plugins))
  if (requested.length === 0) {
    setState({
      hint: '没有可批量启用的插件：已经在运行的不用再启用，待授权的需要先在详情里授权。',
      error: null,
      errorDetails: []
    })
    return false
  }
  if (requested.length > PLUGIN_BATCH_MAX) {
    setState({
      error: `一次最多并发启用 ${PLUGIN_BATCH_MAX} 个插件，当前选中 ${requested.length} 个，请分批。`,
      errorDetails: []
    })
    return false
  }

  setState({
    batch: {
      requested,
      total: requested.length,
      done: 0,
      counted: [],
      succeeded: [],
      failures: [],
      maxConcurrent: null,
      running: true
    },
    error: null,
    errorDetails: [],
    hint: null
  })

  try {
    const result = await pluginApi.loadMany(requested)
    const succeeded: string[] = []
    const failures: PluginBatchFailure[] = []
    for (const item of result.items) {
      if (item.error === null) {
        succeeded.push(item.plugin_id)
      } else {
        failures.push({
          pluginId: item.plugin_id,
          code: item.error.code,
          message: item.error.message,
          hint: pluginHintForError(item.error.code)
        })
      }
    }
    // 请求返回即「全部结算」：内核按入参顺序回 `items`，缺项不该出现，但真缺了
    // 也不能让进度条永远卡在 N-1，所以这里直接按总数收口。
    setState({
      batch: {
        requested,
        total: requested.length,
        done: requested.length,
        counted: [...requested],
        succeeded,
        failures,
        maxConcurrent: result.max_concurrent,
        running: false
      }
    })
    await refreshPlugins()
    if (state.selectedId !== null) await fetchPluginDetail(state.selectedId)
    return failures.length === 0
  } catch (error) {
    setState({
      batch: {
        requested,
        total: requested.length,
        done: 0,
        counted: [],
        succeeded: [],
        failures: [],
        maxConcurrent: null,
        running: false
      },
      error: describePluginError(error),
      errorDetails: pluginErrorIssues(error)
    })
    await refreshPlugins()
    return false
  }
}

/** 一次批量最多多少个插件：与内核 `plugin.load_many` 的单次上限一致。 */
export const PLUGIN_BATCH_MAX = 32

function dedupeIds(pluginIds: readonly string[]): string[] {
  const seen = new Set<string>()
  const out: string[] = []
  for (const raw of pluginIds) {
    const id = raw.trim()
    if (id.length === 0 || seen.has(id)) continue
    seen.add(id)
    out.push(id)
  }
  return out
}

/**
 * 批量装载期间，按 `plugin.state_changed` 推进「已完成」计数。
 *
 * 只认本次 `requested` 里的插件、只在 `running` 期间生效、每个 id 只计一次：
 * 内核装载一个插件会广播多次状态（`loading` → `ready`），把 `loading` 也计数
 * 就会让进度条跑得比实际快。
 */
export function notePluginBatchProgress(pluginId: string, pluginState: string): void {
  const batch = state.batch
  if (batch === null || !batch.running) return
  if (!batch.requested.includes(pluginId)) return
  if (batch.counted.includes(pluginId)) return
  if (!BATCH_TERMINAL_STATES.includes(pluginState)) return
  setState({
    batch: {
      ...batch,
      done: Math.min(batch.total, batch.done + 1),
      counted: [...batch.counted, pluginId]
    }
  })
}

/** 关掉批量结果条（用户看完失败清单后）。 */
export function dismissPluginBatch(): void {
  setState({ batch: null })
}

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
