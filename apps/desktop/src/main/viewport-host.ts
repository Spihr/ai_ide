/**
 * 外部视口进程的**外壳侧**管辖（docs/24 §5.4）。
 *
 * 只做三件事，都发生在主进程：把清单里的相对 `program` 解析成绝对路径并围栏校验、
 * 用数组参数 `spawn`（不经 shell）、探通命名管道确认视口真的起来了。
 *
 * 刻意**不做**的事：
 *
 * 1. 不解析会话帧。`open` / `state` / `selection` 这些帧走插件宿主的管道客户端，
 *    外壳只在启动期探一次「管道活着吗」，探完就断开——两个客户端抢同一条管道
 *    会把 `ready` 首帧谁消费变成竞态。
 * 2. 不杀视口进程。IDE 退出时用户可能正看着模型（§5.3），断管道即可；子进程用
 *    `detached: false` + `stdio: ignore` 是因为它**不该继承**父进程的句柄，而不是
 *    为了「父死子亡」。
 * 3. 不由渲染进程提供路径。渲染侧只给 `pluginId + viewId`，安装目录与 `program`
 *    一律回内核查（`plugin.status`）。否则「传一个 installDir 进来」就等于给了
 *    渲染进程一个「执行任意 exe」的口子，再严的围栏校验也白搭。
 */

import { spawn as spawnChild, type ChildProcess } from 'node:child_process'
import { randomBytes } from 'node:crypto'
import { existsSync, realpathSync, statSync } from 'node:fs'
import { connect } from 'node:net'
import { extname, isAbsolute, resolve, sep } from 'node:path'

/**
 * 外壳认识的外部视口协议。
 *
 * 白名单而不是「非空即可」：协议名是两侧唯一的兼容性开关，清单里写个
 * `asset3d.session/2` 而外壳还按 1 去探通，等于把版本不匹配伪装成「视口起不来」。
 */
export const VIEWPORT_PROTOCOLS: readonly string[] = ['asset3d.session/1']

/** 管道名前缀。视口侧手动双击时用的默认名也在这个命名空间下，避免与别的软件撞名。 */
export const PIPE_PREFIX = 'aiide.viewport.'

/** 探通重试：100 次 × 100 ms = 10 s，与 §5.3 的 `ready` 超时一致。 */
const PROBE_ATTEMPTS = 100
const PROBE_INTERVAL_MS = 100

/** 失败原因码。渲染侧据此给不同文案（「重新安装插件」vs「查看插件日志」）。 */
export type ViewportSpawnErrorCode =
  | 'plugin_invalid'
  | 'install_missing'
  | 'program_invalid'
  | 'program_escape'
  | 'program_ext'
  | 'program_missing'
  | 'protocol_unsupported'
  | 'spawn_failed'
  | 'pipe_timeout'

export class ViewportSpawnError extends Error {
  constructor(
    readonly code: ViewportSpawnErrorCode,
    message: string
  ) {
    super(message)
    this.name = 'ViewportSpawnError'
  }
}

/** 与 §4.1 的视图 id 规则一致：`^[a-z][a-z0-9_-]{0,31}$`。 */
const VIEW_ID_PATTERN = /^[a-z][a-z0-9_-]{0,31}$/
/** 插件 id 允许点号（`hello.world`），管道名允许点号与连字符。 */
const PLUGIN_ID_PATTERN = /^[a-z0-9][a-z0-9._-]{0,63}$/i

function isWithin(root: string, candidate: string): boolean {
  // Windows 路径大小写不敏感，比较统一折小写；`root` 末尾补分隔符，
  // 否则 `C:\a\bc` 会被判成 `C:\a\b` 的子路径。
  const normalizedRoot = root.endsWith(sep) ? root : `${root}${sep}`
  const left = candidate.toLowerCase()
  const right = normalizedRoot.toLowerCase()
  return left === root.toLowerCase() || left.startsWith(right)
}

/**
 * 清单里的 `external.program` → 可执行文件的绝对路径。
 *
 * 四道关，顺序有讲究：**先判形状再判存在**。反过来的话，一个 `..\..\Windows\System32\cmd.exe`
 * 会先因为「文件存在」通过，再靠围栏拦下——中间那一步已经把「目标在哪」泄露给调用方了。
 *
 * 符号链接单独再判一次：`resolve` 只做字符串归并，盘内建一个
 * `plugin/render.exe -> C:\Windows\System32\cmd.exe` 的链接，字符串围栏是过得去的。
 */
export function resolveViewportProgram(installDir: string, program: string): string {
  if (typeof installDir !== 'string' || installDir.trim().length === 0) {
    throw new ViewportSpawnError('install_missing', '插件安装目录未知，请重新安装插件')
  }
  if (typeof program !== 'string' || program.trim().length === 0) {
    throw new ViewportSpawnError('program_invalid', '视图没有声明外部视口程序')
  }
  if (program.includes('\0')) {
    throw new ViewportSpawnError('program_invalid', '外部视口程序名含非法字符')
  }

  const relative = program.trim().replace(/\\/g, '/')
  if (relative.startsWith('/') || isAbsolute(relative) || /^[a-zA-Z]:/.test(relative)) {
    throw new ViewportSpawnError('program_invalid', '外部视口程序必须是插件包内的相对路径')
  }

  let root: string
  try {
    root = realpathSync(installDir)
  } catch {
    throw new ViewportSpawnError('install_missing', `插件安装目录已不存在：${installDir}`)
  }

  const target = resolve(root, relative)
  if (!isWithin(root, target)) {
    throw new ViewportSpawnError('program_escape', '外部视口程序越出了插件安装目录')
  }
  if (extname(target).toLowerCase() !== '.exe') {
    throw new ViewportSpawnError('program_ext', '外部视口程序必须是 .exe')
  }
  if (!existsSync(target)) {
    throw new ViewportSpawnError('program_missing', `插件包内找不到外部视口程序：${relative}`)
  }

  let real: string
  try {
    real = realpathSync(target)
  } catch {
    throw new ViewportSpawnError('program_missing', `外部视口程序不可访问：${relative}`)
  }
  if (!isWithin(root, real)) {
    throw new ViewportSpawnError('program_escape', '外部视口程序经符号链接指向了安装目录之外')
  }
  if (!statSync(real).isFile()) {
    throw new ViewportSpawnError('program_missing', `外部视口程序不是普通文件：${relative}`)
  }
  return real
}

/**
 * 生成本次托管的管道名。
 *
 * 每次 `spawn` 都用新名字（带 4 字节随机尾巴）：视口崩溃后旧管道可能残留在
 * 「半关闭」状态，复用同名会让新进程的 `CreateNamedPipe` 与旧句柄打架，
 * 表现是「进程起来了但连不上」，排查成本极高。
 */
export function buildViewportPipeName(pluginId: string, token?: string): string {
  if (typeof pluginId !== 'string' || !PLUGIN_ID_PATTERN.test(pluginId)) {
    throw new ViewportSpawnError('plugin_invalid', `插件 id 不合法：${String(pluginId)}`)
  }
  const suffix = token ?? randomBytes(4).toString('hex')
  return `${PIPE_PREFIX}${pluginId}.${suffix}`
}

/**
 * 默认管道名：**不带随机尾巴**的那一条（docs/24 §5.3「管道已存在则不重复 spawn」）。
 *
 * 存在的理由只有一个——让外壳能发现「不是自己起的」视口实例：用户双击
 * `asset3d-gui.exe` 时视口自己创建并监听这条默认管道，IDE 再点「打开视口」
 * 应当连上去（并转发 `open`），而不是在默认管道之外另起一个窗口。
 *
 * 与之相对，**自己 spawn 的**进程一律用随机名：残留管道与新进程撞名会让
 * `CreateNamedPipe` 失败，表现为「进程起来了但连不上」，比多一个窗口难查得多。
 */
export function defaultViewportPipeName(pluginId: string): string {
  if (typeof pluginId !== 'string' || !PLUGIN_ID_PATTERN.test(pluginId)) {
    throw new ViewportSpawnError('plugin_invalid', `插件 id 不合法：${String(pluginId)}`)
  }
  return `${PIPE_PREFIX}${pluginId}.default`
}

/** 命名管道的 Windows 全名。 */
export function viewportPipePath(pipeName: string): string {
  return `\\\\.\\pipe\\${pipeName}`
}

/**
 * 子进程环境变量：只给 Windows 起图形进程的**必需项**。
 *
 * `SystemRoot` / `ComSpec` 看着不像「应用该有的变量」，但缺了它们，
 * DXGI / D3D 编译着色器时会直接失败，报错还停留在「设备创建失败」这一层，
 * 与真正的原因（没有系统目录）隔着十万八千里。密钥类变量（`AI_IDE_*`、
 * `*_API_KEY`）一个都不传。
 */
const PASSTHROUGH_ENV_KEYS = [
  'PATH',
  'Path',
  'TEMP',
  'TMP',
  'USERPROFILE',
  'SystemRoot',
  'SYSTEMROOT',
  'windir',
  'ComSpec',
  'NUMBER_OF_PROCESSORS',
  'PROCESSOR_ARCHITECTURE'
] as const

export function minimalViewportEnv(source: NodeJS.ProcessEnv = process.env): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {}
  for (const key of PASSTHROUGH_ENV_KEYS) {
    const value = source[key]
    if (typeof value === 'string' && value.length > 0) env[key] = value
  }
  return env
}

export interface SpawnViewportInput {
  pluginId: string
  viewId: string
  /** 插件安装目录（由内核 `plugin.status` 给出，不接受渲染进程输入）。 */
  installDir: string
  /** 清单里的相对路径（`.exe`）。 */
  program: string
  /** 清单里的 `external.protocol`。 */
  protocol: string
}

export interface SpawnViewportResult {
  /** 本次管道名（不含 `\\.\pipe\` 前缀），供插件宿主连接。 */
  pipeName: string
  /** 解析后的绝对路径（已过围栏）。 */
  program: string
  /** `true` 表示复用了上一轮仍然活着的进程，没有新起。 */
  reused: boolean
}

/** 视口进程退出（外壳自己起的那些）。 */
export interface ViewportExitInfo {
  pluginId: string
  viewId: string
  exitCode: number | null
}

export interface ViewportHostDeps {
  spawn?: typeof spawnChild
  /** 探通一次管道；连不上时 reject。 */
  probe?: (pipePath: string) => Promise<void>
  sleep?: (ms: number) => Promise<void>
  attempts?: number
  intervalMs?: number
  /**
   * 视口进程退出时回调（docs/24 §5.3）。
   *
   * 只有外壳自己 `spawn` 的进程会回调：接管别人起的实例（默认管道）时手里没有
   * 句柄，监听不到对方退出——那一侧不归外壳管，面板也没法变出「已关闭」。
   */
  onExit?: (info: ViewportExitInfo) => void
}

function defaultProbe(pipePath: string): Promise<void> {
  return new Promise<void>((resolvePromise, rejectPromise) => {
    const socket = connect(pipePath)
    const finish = (error?: Error): void => {
      socket.removeAllListeners()
      socket.destroy()
      if (error) rejectPromise(error)
      else resolvePromise()
    }
    socket.once('connect', () => finish())
    socket.once('error', (error: Error) => finish(error))
  })
}

const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms))

interface LiveViewport {
  child: ChildProcess
  pipeName: string
  program: string
}

/**
 * 外部视口进程表：同一 `pluginId + viewId` 只托管一个进程。
 *
 * 注意这里**不是**单实例互斥（那是视口侧 `Local\` 互斥体的职责，§5.3）：
 * 用户手动双击开的窗口与 IDE 并行存在是合法的，两个进程靠视口侧互斥体
 * 决定「谁监听、谁转发」。外壳这张表只回答「我上次起的那个还活着吗」。
 */
export class ViewportHost {
  private readonly live = new Map<string, LiveViewport>()
  private readonly spawnFn: typeof spawnChild
  private readonly probeFn: (pipePath: string) => Promise<void>
  private readonly sleepFn: (ms: number) => Promise<void>
  private readonly attempts: number
  private readonly intervalMs: number
  private readonly onExitFn: ((info: ViewportExitInfo) => void) | undefined

  constructor(deps: ViewportHostDeps = {}) {
    this.spawnFn = deps.spawn ?? spawnChild
    this.probeFn = deps.probe ?? defaultProbe
    this.sleepFn = deps.sleep ?? sleep
    this.attempts = deps.attempts ?? PROBE_ATTEMPTS
    this.intervalMs = deps.intervalMs ?? PROBE_INTERVAL_MS
    this.onExitFn = deps.onExit
  }

  private static key(pluginId: string, viewId: string): string {
    return `${pluginId}\u0000${viewId}`
  }

  /**
   * 确保视口进程可用，返回可连接的管道名。
   *
   * 复用的判据是「进程活着 **且** 管道能连上」：只判进程会让「窗口被用户关了但
   * 进程还挂着」（比如卡在对话框）被误判成可用，用户点第二次没有任何反应。
   */
  async spawnViewport(input: SpawnViewportInput): Promise<SpawnViewportResult> {
    if (!VIEW_ID_PATTERN.test(input.viewId)) {
      throw new ViewportSpawnError('plugin_invalid', `视图 id 不合法：${input.viewId}`)
    }
    if (!VIEWPORT_PROTOCOLS.includes(input.protocol)) {
      throw new ViewportSpawnError(
        'protocol_unsupported',
        `外壳不支持视口协议 ${input.protocol}，请升级 IDE`
      )
    }

    // 先解析程序：即使要走「接管已有实例」的分支，也要证明清单本身是合法的
    // （围栏、扩展名、存在性），否则一次「连上默认管道」会掩盖坏的 external.program。
    const program = resolveViewportProgram(input.installDir, input.program)

    const key = ViewportHost.key(input.pluginId, input.viewId)
    const existing = this.live.get(key)
    if (existing && existing.child.exitCode === null && !existing.child.killed) {
      try {
        await this.probeOnce(viewportPipePath(existing.pipeName))
        return { pipeName: existing.pipeName, program: existing.program, reused: true }
      } catch {
        // 进程还在但管道断了：按「视口已关闭」处理，往下走重新 spawn。
        this.live.delete(key)
      }
    } else if (existing) {
      // 自己起的那个已经退出（exit 事件可能还没派发）：顺手摘掉，别让它挡住下面的探测。
      this.live.delete(key)
    }

    // 走到这里说明「我手上没有可用实例」，但**不代表机器上没有**：
    // 用户可能手动双击了视口，它监听的是默认管道名。
    if (await this.probeDefaultPipe(input.pluginId)) {
      return { pipeName: defaultViewportPipeName(input.pluginId), program, reused: true }
    }

    const pipeName = buildViewportPipeName(input.pluginId)

    let child: ChildProcess
    try {
      child = this.spawnFn(program, ['--pipe', pipeName], {
        cwd: input.installDir,
        // `ignore` 而不是 `inherit`：视口的 stdout/stderr 一旦接上父进程，
        // 打包后的 Windows GUI 应用会拿到一份无用的控制台句柄。
        stdio: 'ignore',
        env: minimalViewportEnv(),
        windowsHide: false,
        detached: false
      })
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error)
      throw new ViewportSpawnError('spawn_failed', `外部视口启动失败：${message}`)
    }

    const record: LiveViewport = { child, pipeName, program }
    this.live.set(key, record)
    // 进程自己退出就把它从表里摘掉，否则下一次会永远走「复用」分支。
    //
    // `error` 与 `exit` 可能先后各来一次（spawn 失败时 Electron 只发 error），
    // 所以回调只走一遍：用户看到的「视口已关闭」出现两次比不出现更让人困惑。
    let reported = false
    const report = (exitCode: number | null): void => {
      if (reported) return
      reported = true
      if (this.live.get(key) === record) this.live.delete(key)
      this.onExitFn?.({ pluginId: input.pluginId, viewId: input.viewId, exitCode })
    }
    child.once('exit', (code: number | null) => report(code))
    child.once('error', () => report(null))

    try {
      await this.waitForPipe(viewportPipePath(pipeName), child)
    } catch (error) {
      if (this.live.get(key) === record) this.live.delete(key)
      // 已经用「起不来 / 没连上」的文案报过一次，就别再补一条「视口已关闭」：
      // 那时面板显示的是失败原因，再叠一条退出通知会让人以为是第二次失败。
      reported = true
      throw error
    }

    return { pipeName, program, reused: false }
  }

  private async probeOnce(pipePath: string): Promise<void> {
    await this.probeFn(pipePath)
  }

  /**
   * 默认管道上是否已有监听者。
   *
   * 失败一律当「没有」：这里只是**顺手看一眼**，探测出错（权限、瞬时
   * `ENOENT`）不该拦住用户打开视口的主流程。
   */
  private async probeDefaultPipe(pluginId: string): Promise<boolean> {
    try {
      await this.probeOnce(viewportPipePath(defaultViewportPipeName(pluginId)))
      return true
    } catch {
      return false
    }
  }

  private async waitForPipe(pipePath: string, child: ChildProcess): Promise<void> {
    let lastError: Error | undefined
    for (let attempt = 0; attempt < this.attempts; attempt += 1) {
      // 进程已退出就不必再等满 10 s：用户点了按钮，等十秒再报「起不来」的体验
      // 比立刻说「插件包里的 exe 启动就崩了」差得多。
      if (child.exitCode !== null) {
        throw new ViewportSpawnError(
          'spawn_failed',
          `外部视口启动后立即退出（退出码 ${child.exitCode}），请查看插件日志`
        )
      }
      try {
        await this.probeOnce(pipePath)
        return
      } catch (error) {
        lastError = error instanceof Error ? error : new Error(String(error))
        await this.sleepFn(this.intervalMs)
      }
    }
    throw new ViewportSpawnError(
      'pipe_timeout',
      `外部视口未就绪（等待 ${Math.round((this.attempts * this.intervalMs) / 1000)} s 未连上管道）${
        lastError ? `：${lastError.message}` : ''
      }`
    )
  }

  /** IDE 退出时调用：只清表，不杀子进程（§5.3）。 */
  dispose(): void {
    this.live.clear()
  }
}
