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

import { app, BrowserWindow, dialog, ipcMain } from 'electron'
import { dirname, join } from 'node:path'

import { Methods, type HelloResult } from '@ai-ide/shared-types'
import { CoreSupervisor } from './core-supervisor'
import { checkRendererCall } from './policy'
import {
  IpcChannels,
  type CoreLogChunk,
  type CoreStatus,
  type PingBenchmarkResult
} from '../shared/ipc'

const isDev = !app.isPackaged

let supervisor: CoreSupervisor | null = null
let mainWindow: BrowserWindow | null = null

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
    clientVersion: app.getVersion()
  })

  instance.on('stderr', (chunk) => pushLog(chunk))
  instance.on('notification', (method, params) => {
    // 工作区事件直接转发给 UI；其余通知只留在内核日志里。
    const channel =
      method === Methods.event.workspaceChanged
        ? IpcChannels.workspaceChanged
        : method === Methods.event.workspaceIndexed
          ? IpcChannels.workspaceIndexed
          : null

    if (channel && mainWindow && !mainWindow.isDestroyed()) {
      mainWindow.webContents.send(channel, params)
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
      return requireSupervisor().request(method, params)
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

void app.whenReady().then(() => {
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
