/**
 * 主进程入口：进程编排 + 策略闸门 + UI 宿主。
 *
 * 启动顺序：
 *   1. 注册 IPC 处理器（先于建窗，避免渲染进程抢先调用时拿到空处理器）
 *   2. 创建窗口
 *   3. 异步拉起 Rust 内核并握手，结果通过 `core:status-changed` 推给 UI
 *
 * 内核启动失败**不阻断**窗口显示——UI 需要活着把错误呈现给用户。
 */

import { app, BrowserWindow, dialog, ipcMain, safeStorage } from 'electron'
import { dirname, join } from 'node:path'

import { Methods, type HelloResult } from '@ai-ide/shared-types'
import { CoreRpcError, CoreSupervisor } from './core-supervisor'
import { CredentialVault } from './credentials'
import { checkRendererCall } from './policy'
import {
  extractDeletedProviderId,
  extractUpsertedKey,
  rememberKeyAfterUpsert,
  replayCredentials
} from './provider-keys'
import {
  IpcChannels,
  encodeRpcErrorPayload,
  type AiEvent,
  type CoreLogChunk,
  type CoreStatus,
  type CredentialStatus,
  type PingBenchmarkResult
} from '../shared/ipc'

const isDev = !app.isPackaged

/**
 * 用户数据目录覆盖（测试与多实例隔离用）。
 *
 * 必须在 `app.whenReady()` 之前设置：Electron 一旦就绪就会按默认目录
 * 建好各种路径，事后再 `setPath` 只会得到半新半旧的混合状态。
 */
const userDataOverride = process.env.AI_IDE_USER_DATA_DIR
if (userDataOverride && userDataOverride.trim().length > 0) {
  app.setPath('userData', userDataOverride.trim())
}

let supervisor: CoreSupervisor | null = null
let mainWindow: BrowserWindow | null = null
let vault: CredentialVault | null = null

let currentStatus: CoreStatus = {
  state: 'idle',
  binaryPath: '',
  pid: null,
  hello: null,
  error: null,
  exitCode: null,
  exitSignal: null
}

// ---------------------------------------------------------------------------
// 内核生命周期
// ---------------------------------------------------------------------------

/**
 * 解析内核可执行文件位置。
 *
 * - 开发期：`<repo>/target/{debug,release}/ai-ide-core.exe`
 * - 打包后：`<resources>/core/ai-ide-core.exe`
 */
function resolveCoreBinary(): string {
  const exe = process.platform === 'win32' ? 'ai-ide-core.exe' : 'ai-ide-core'
  if (app.isPackaged) {
    return join(process.resourcesPath, 'core', exe)
  }
  const repoRoot = join(app.getAppPath(), '..', '..')
  const profile = process.env.AI_IDE_CORE_PROFILE === 'release' ? 'release' : 'debug'
  return join(repoRoot, 'target', profile, exe)
}

function setStatus(next: CoreStatus): void {
  currentStatus = next
  if (mainWindow && !mainWindow.isDestroyed()) {
    mainWindow.webContents.send(IpcChannels.coreStatusChanged, next)
  }
}

function pushLog(text: string): void {
  if (!mainWindow || mainWindow.isDestroyed()) return
  const chunk: CoreLogChunk = { stream: 'stderr', text }
  mainWindow.webContents.send(IpcChannels.coreLog, chunk)
}

// ---------------------------------------------------------------------------
// 密钥托管（docs/02 §6.3）
// ---------------------------------------------------------------------------

/** 插件可观测事件（P5 切片七）：插件遥测专用的三条通道。 */
function pluginEventChannel(method: string): string | null {
  if (method === Methods.event.pluginLog) return IpcChannels.pluginLog
  if (method === Methods.event.pluginProgress) return IpcChannels.pluginProgress
  if (method === Methods.event.pluginStateChanged) return IpcChannels.pluginStateChanged
  return null
}

/** M2 事件：`provider.changed` 与 `llm.*` 全家。 */
function isAiEvent(method: string): boolean {
  return method === Methods.event.providerChanged || method.startsWith('llm.')
}

/**
 * 内核通知 → IPC 通道。
 *
 * 工作区事件各走专用通道，M2 的 `provider.changed` / `llm.*` 与 M4 的 `agent.*`
 * 走统一 AI 事件通道，M6 的 `terminal.data` / `terminal.exit`、插件的三条遥测
 * 与语言服务的 `lsp.diagnostics` 各走专用通道——它们要么是高频流（终端字节、
 * 进度帧），要么是「服务器主动推、一次编辑一批」（诊断），混进 `ai:event`
 * 只会让对话面板无谓唤醒。认不出的通知返回 `null`，由调用方只写内核日志。
 *
 * 写成 `switch` 而不是嵌套三元：这一跳每帧通知都要走，可读性比行数重要，
 * 之前那串三元已经加到「读不出哪个方法落哪条通道」了。
 */
function eventChannel(method: string): string | null {
  switch (method) {
    case Methods.event.workspaceChanged:
      return IpcChannels.workspaceChanged
    case Methods.event.workspaceIndexed:
      return IpcChannels.workspaceIndexed
    case Methods.event.terminalData:
      return IpcChannels.terminalData
    case Methods.event.terminalExit:
      return IpcChannels.terminalExit
    case Methods.event.lspDiagnostics:
      return IpcChannels.lspDiagnostics
    case Methods.event.pluginLog:
    case Methods.event.pluginProgress:
    case Methods.event.pluginStateChanged:
      return pluginEventChannel(method)
    default:
      return isAiEvent(method) ? IpcChannels.aiEvent : null
  }
}

function requireVault(): CredentialVault {
  if (!vault) throw new Error('密钥托管尚未初始化')
  return vault
}

function credentialStatus(): CredentialStatus {
  if (!vault) {
    return {
      encryptionAvailable: safeStorage.isEncryptionAvailable(),
      vaultPath: '',
      managedProviderIds: [],
      lastError: '密钥托管尚未初始化'
    }
  }
  return vault.status()
}

function broadcastCredentialStatus(): void {
  if (mainWindow && !mainWindow.isDestroyed()) {
    mainWindow.webContents.send(IpcChannels.credentialsStatusChanged, credentialStatus())
  }
}

/**
 * 内核就绪（含重启）后把托管密钥注回内核。
 *
 * 内核只在内存里保管密钥，所以每次拉起内核都要重放一次；否则用户在设置界面
 * 填过的 Key 会表现为「填了但连不上」。
 */
async function injectCredentialKeys(instance: CoreSupervisor): Promise<void> {
  if (!vault) return
  const report = await replayCredentials({
    request: (method, params) => instance.request(method, params),
    vault,
    log: pushLog
  })

  const touched = report.injected.length + report.missing.length + report.failed.length
  if (touched > 0) {
    pushLog(
      `[credentials] 密钥重放：注入 ${report.injected.length}，缺失 ${report.missing.length}，失败 ${report.failed.length}\n`
    )
  }
  for (const failure of report.failed) {
    pushLog(`[credentials] 注入失败 ${failure.providerId}：${failure.message}\n`)
  }
  broadcastCredentialStatus()
}

/** 拉起内核并完成握手；返回最终状态。 */
async function startCore(): Promise<CoreStatus> {
  const binaryPath = resolveCoreBinary()

  if (supervisor) {
    await supervisor.stop(false).catch(() => undefined)
    supervisor = null
  }

  setStatus({
    state: 'starting',
    binaryPath,
    pid: null,
    hello: null,
    error: null,
    exitCode: null,
    exitSignal: null
  })

  const instance = new CoreSupervisor({
    binaryPath,
    cwd: dirname(binaryPath),
    clientVersion: app.getVersion(),
    // 内核的 `providers.json` 与外壳的 `credentials.enc` 都属于「这个应用的数据」，
    // 必须同进同退：外壳的 userData 被覆盖（测试 / 多实例）时，内核也不能写到
    // 真实用户目录去。用户显式设过 `AI_IDE_DATA_DIR` 则尊重用户的选择。
    env: {
      AI_IDE_DATA_DIR: process.env.AI_IDE_DATA_DIR ?? app.getPath('userData')
    }
  })

  instance.on('stderr', (chunk) => pushLog(chunk))
  instance.on('notification', (method, params) => {
    // 通道映射统一收在 `eventChannel` 里（见上方），这里只负责投递：
    // 有专用通道就直接发，没有的只留在内核日志里。
    const channel = eventChannel(method)

    if (channel && mainWindow && !mainWindow.isDestroyed()) {
      const payload = channel === IpcChannels.aiEvent ? ({ method, params } satisfies AiEvent) : params
      mainWindow.webContents.send(channel, payload)
      return
    }
    pushLog(`[notify] ${method} ${JSON.stringify(params ?? {})}\n`)
  })
  instance.on('protocolError', (detail) => pushLog(`[protocol-error] ${detail}\n`))
  instance.on('exit', (code, signal) => {
    setStatus({
      state: 'failed',
      binaryPath,
      pid: null,
      hello: null,
      error: `内核进程意外退出（code=${code ?? 'null'}, signal=${signal ?? 'null'}）`,
      exitCode: code,
      exitSignal: signal
    })
  })

  supervisor = instance

  try {
    const hello: HelloResult = await instance.start()
    setStatus({
      state: 'ready',
      binaryPath,
      pid: instance.pid,
      hello,
      error: null,
      exitCode: null,
      exitSignal: null
    })
    // 就绪即重放密钥：内核重启后内存里是空的，不重放会导致「配置在、密钥不在」。
    await injectCredentialKeys(instance)
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err)
    await instance.stop(false).catch(() => undefined)
    supervisor = null
    setStatus({
      state: 'failed',
      binaryPath,
      pid: null,
      hello: null,
      error: message,
      exitCode: null,
      exitSignal: null
    })
  }

  return currentStatus
}

function requireSupervisor(): CoreSupervisor {
  if (!supervisor || !supervisor.isRunning) {
    throw new Error('内核未运行，请先重启内核')
  }
  return supervisor
}

// ---------------------------------------------------------------------------
// 往返延迟基准
// ---------------------------------------------------------------------------

function percentile(sorted: number[], ratio: number): number {
  if (sorted.length === 0) return 0
  const index = Math.min(sorted.length - 1, Math.floor(sorted.length * ratio))
  return sorted[index] ?? 0
}

async function runPingBenchmark(sup: CoreSupervisor, count: number): Promise<PingBenchmarkResult> {
  const samplesMs: number[] = []
  for (let i = 0; i < count; i += 1) {
    const started = performance.now()
    await sup.request(Methods.core.ping, {})
    samplesMs.push(performance.now() - started)
  }

  const sorted = [...samplesMs].sort((a, b) => a - b)
  const sum = sorted.reduce((acc, value) => acc + value, 0)

  return {
    count,
    minMs: sorted[0] ?? 0,
    maxMs: sorted[sorted.length - 1] ?? 0,
    meanMs: sorted.length > 0 ? sum / sorted.length : 0,
    p50Ms: percentile(sorted, 0.5),
    p95Ms: percentile(sorted, 0.95),
    samplesMs
  }
}

// ---------------------------------------------------------------------------
// IPC
// ---------------------------------------------------------------------------

/**
 * 把内核错误转成「渲染进程能拿到全部信息」的 Error。
 *
 * `Error` 过 IPC 只保 `message`，`CoreRpcError.data` 会被剥掉。插件安装的 `6107`
 * 偏偏要靠 `data.details[]` 才能把几条清单问题一次列全，所以对 `plugin.*` 这几个
 * 方法（P1 目前唯一需要结构化数据的调用面）把载荷编进 message 尾部，渲染侧
 * 按 `shared/ipc.ts` 的分隔串还原。
 *
 * 只对 `plugin.` 前缀生效而不是全量启用：其他面板目前只消费文案，给它们塞一段
 * JSON 尾巴只会让错误提示变丑。等哪个面板真的需要 data，再把前缀加进来即可。
 */
function toRendererError(method: string, error: unknown): unknown {
  if (!(error instanceof CoreRpcError) || !method.startsWith('plugin.')) {
    return error
  }
  return new Error(
    encodeRpcErrorPayload({ code: error.code, message: error.message, data: error.data })
  )
}

function registerIpc(): void {
  ipcMain.handle(IpcChannels.coreStatus, async (): Promise<CoreStatus> => currentStatus)

  ipcMain.handle(IpcChannels.coreRestart, async (): Promise<CoreStatus> => startCore())

  ipcMain.handle(
    IpcChannels.coreRequest,
    async (_event, method: unknown, params: unknown): Promise<unknown> => {
      if (typeof method !== 'string') {
        throw new Error('method 必须是字符串')
      }
      const decision = checkRendererCall(method)
      if (!decision.allowed) {
        throw new Error(decision.reason ?? `调用被拒绝：${method}`)
      }

      // 明文密钥只在「渲染进程 → 主进程 → 内核」这条链上短暂存在：
      // 进内核之前先截住一份给托管，内核那边拿到的是同一把、但只留在内存。
      const pendingKey = extractUpsertedKey(method, params)
      const result = await requireSupervisor()
        .request(method, params)
        .catch((error: unknown) => {
          throw toRendererError(method, error)
        })

      if (pendingKey) {
        try {
          const saved = await rememberKeyAfterUpsert(requireVault(), result, pendingKey)
          broadcastCredentialStatus()
          if (!saved.persisted) {
            pushLog('[credentials] 系统密钥链不可用，密钥仅保留在内存中（未写盘）\n')
          }
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err)
          pushLog(`[credentials] 托管密钥失败：${message}\n`)
        }
      }

      const deletedId = extractDeletedProviderId(method, params)
      if (deletedId) {
        // 级联清理：Provider 删了还留着密钥托管，等于留了一把无主的钥匙。
        await requireVault()
          .forget(deletedId)
          .catch(() => false)
        broadcastCredentialStatus()
      }

      return result
    }
  )

  ipcMain.handle(
    IpcChannels.corePingBenchmark,
    async (_event, count: unknown): Promise<PingBenchmarkResult> => {
      const normalized =
        typeof count === 'number' && Number.isFinite(count)
          ? Math.min(Math.max(Math.trunc(count), 1), 2_000)
          : 20
      return runPingBenchmark(requireSupervisor(), normalized)
    }
  )

  ipcMain.handle(IpcChannels.credentialsStatus, async (): Promise<CredentialStatus> => credentialStatus())

  ipcMain.handle(
    IpcChannels.credentialsForget,
    async (_event, providerId: unknown): Promise<CredentialStatus> => {
      if (typeof providerId !== 'string' || providerId.trim().length === 0) {
        throw new Error('providerId 必须是非空字符串')
      }
      const forgot = await requireVault().forget(providerId.trim())
      if (forgot) pushLog(`[credentials] 已撤销 ${providerId} 的密钥托管\n`)
      broadcastCredentialStatus()
      return credentialStatus()
    }
  )

  ipcMain.handle(IpcChannels.pickWorkspace, async (): Promise<string | null> => {
    const options: Electron.OpenDialogOptions = {
      title: '选择工作区目录',
      buttonLabel: '打开',
      properties: ['openDirectory', 'createDirectory']
    }
    const result =
      mainWindow && !mainWindow.isDestroyed()
        ? await dialog.showOpenDialog(mainWindow, options)
        : await dialog.showOpenDialog(options)

    if (result.canceled || result.filePaths.length === 0) return null
    return result.filePaths[0] ?? null
  })

  // M5：会话导出要落盘，落点只能来自用户亲手点的保存对话框。
  ipcMain.handle(
    IpcChannels.pickSavePath,
    async (_event, defaultName: unknown, extensions: unknown): Promise<string | null> => {
      const name =
        typeof defaultName === 'string' && defaultName.trim().length > 0
          ? defaultName.trim().slice(0, 200)
          : 'export.md'
      // 只接受「纯扩展名」：带路径分隔符或点号的输入一律丢弃，避免伪造过滤器。
      const exts = (Array.isArray(extensions) ? extensions : [])
        .filter((item): item is string => typeof item === 'string')
        .map((item) => item.replace(/^\./, '').trim())
        .filter((item) => /^[a-z0-9]{1,12}$/i.test(item))
        .slice(0, 5)
      const options: Electron.SaveDialogOptions = {
        title: '导出会话',
        buttonLabel: '导出',
        defaultPath: name,
        filters: exts.length > 0 ? [{ name: '导出文件', extensions: exts }] : undefined
      }
      const result =
        mainWindow && !mainWindow.isDestroyed()
          ? await dialog.showSaveDialog(mainWindow, options)
          : await dialog.showSaveDialog(options)

      if (result.canceled || !result.filePath) return null
      return result.filePath
    }
  )
}

// ---------------------------------------------------------------------------
// 窗口
// ---------------------------------------------------------------------------

function createWindow(): void {
  mainWindow = new BrowserWindow({
    width: 1180,
    height: 800,
    minWidth: 940,
    minHeight: 620,
    show: false,
    autoHideMenuBar: true,
    backgroundColor: '#14161a',
    title: 'AI IDE',
    webPreferences: {
      preload: join(__dirname, '../preload/index.js'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: false
    }
  })

  mainWindow.on('ready-to-show', () => {
    mainWindow?.show()
  })

  mainWindow.on('closed', () => {
    mainWindow = null
  })

  const devUrl = process.env.ELECTRON_RENDERER_URL
  if (isDev && devUrl) {
    void mainWindow.loadURL(devUrl)
  } else {
    void mainWindow.loadFile(join(__dirname, '../renderer/index.html'))
  }
}

// ---------------------------------------------------------------------------
// 应用生命周期
// ---------------------------------------------------------------------------

void app.whenReady().then(async () => {
  // 托管文件位置依赖 userData，必须在窗口与内核之前就绪。
  vault = new CredentialVault({
    vaultPath: join(app.getPath('userData'), 'credentials.enc'),
    safeStorage
  })
  await vault.load()

  registerIpc()
  createWindow()
  void startCore()
})

app.on('activate', () => {
  if (BrowserWindow.getAllWindows().length === 0) {
    createWindow()
  }
})

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') {
    app.quit()
  }
})

app.on('before-quit', (event) => {
  if (!supervisor?.isRunning) return
  event.preventDefault()
  const instance = supervisor
  supervisor = null
  void instance.stop().finally(() => {
    app.quit()
  })
})
