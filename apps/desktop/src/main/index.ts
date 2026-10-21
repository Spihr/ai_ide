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
import { CoreSupervisor } from './core-supervisor'
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

/** M2 事件：`provider.changed` 与 `llm.*` 全家。 */
function isAiEvent(method: string): boolean {
  return method === Methods.event.providerChanged || method.startsWith('llm.')
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
    // 工作区事件走各自专用通道，M2 的 `provider.changed` / `llm.*` 与 M4 的
    // `agent.*` 走统一 AI 事件通道；其余通知只留在内核日志里。
    const channel =
      method === Methods.event.workspaceChanged
        ? IpcChannels.workspaceChanged
        : method === Methods.event.workspaceIndexed
          ? IpcChannels.workspaceIndexed
          : isAiEvent(method)
            ? IpcChannels.aiEvent
            : null

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
      const result = await requireSupervisor().request(method, params)

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
