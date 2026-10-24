/**
 * 预加载脚本：把一份**收窄的**API 注入渲染进程。
 *
 * 渲染进程拿不到 `ipcRenderer` 本体，只能调用这里显式暴露的方法。
 * 这是「策略闸门」的第一道，主进程侧的 `policy.ts` 是第二道。
 */

import { contextBridge, ipcRenderer, type IpcRendererEvent } from 'electron'

import {
  IpcChannels,
  type AiEvent,
  type AiIdeApi,
  type CoreLogChunk,
  type CoreStatus,
  type CredentialStatus,
  type PingBenchmarkResult,
  type PluginLogEvent,
  type PluginProgressEvent,
  type PluginStateChangedEvent,
  type TerminalDataEvent,
  type TerminalExitEvent,
  type WorkspaceChangedEvent,
  type WorkspaceIndexedEvent
} from '../shared/ipc'

const api: AiIdeApi = {
  getCoreStatus: (): Promise<CoreStatus> => ipcRenderer.invoke(IpcChannels.coreStatus),

  restartCore: (): Promise<CoreStatus> => ipcRenderer.invoke(IpcChannels.coreRestart),

  ping: (count = 20): Promise<PingBenchmarkResult> =>
    ipcRenderer.invoke(IpcChannels.corePingBenchmark, count),

  request: (method: string, params?: unknown): Promise<unknown> =>
    ipcRenderer.invoke(IpcChannels.coreRequest, method, params),

  onCoreStatus: (listener: (status: CoreStatus) => void): (() => void) => {
    const handler = (_event: IpcRendererEvent, status: CoreStatus): void => listener(status)
    ipcRenderer.on(IpcChannels.coreStatusChanged, handler)
    return () => {
      ipcRenderer.removeListener(IpcChannels.coreStatusChanged, handler)
    }
  },

  onCoreLog: (listener: (chunk: CoreLogChunk) => void): (() => void) => {
    const handler = (_event: IpcRendererEvent, chunk: CoreLogChunk): void => listener(chunk)
    ipcRenderer.on(IpcChannels.coreLog, handler)
    return () => {
      ipcRenderer.removeListener(IpcChannels.coreLog, handler)
    }
  },

  pickWorkspaceDirectory: (): Promise<string | null> =>
    ipcRenderer.invoke(IpcChannels.pickWorkspace),

  // M5：只提交默认名与扩展名，最终路径由系统对话框决定（渲染进程无法自选落点）。
  pickSavePath: (defaultName: string, extensions: string[]): Promise<string | null> =>
    ipcRenderer.invoke(IpcChannels.pickSavePath, defaultName, extensions),

  onWorkspaceChanged: (listener: (event: WorkspaceChangedEvent) => void): (() => void) => {
    const handler = (_event: IpcRendererEvent, payload: WorkspaceChangedEvent): void =>
      listener(payload)
    ipcRenderer.on(IpcChannels.workspaceChanged, handler)
    return () => {
      ipcRenderer.removeListener(IpcChannels.workspaceChanged, handler)
    }
  },

  onWorkspaceIndexed: (listener: (event: WorkspaceIndexedEvent) => void): (() => void) => {
    const handler = (_event: IpcRendererEvent, payload: WorkspaceIndexedEvent): void =>
      listener(payload)
    ipcRenderer.on(IpcChannels.workspaceIndexed, handler)
    return () => {
      ipcRenderer.removeListener(IpcChannels.workspaceIndexed, handler)
    }
  },

  // M2：密钥托管只暴露「状态」与「撤销」，没有任何取回明文的通道。
  getCredentialStatus: (): Promise<CredentialStatus> =>
    ipcRenderer.invoke(IpcChannels.credentialsStatus),

  forgetCredential: (providerId: string): Promise<CredentialStatus> =>
    ipcRenderer.invoke(IpcChannels.credentialsForget, providerId),

  onAiEvent: (listener: (event: AiEvent) => void): (() => void) => {
    const handler = (_event: IpcRendererEvent, payload: AiEvent): void => listener(payload)
    ipcRenderer.on(IpcChannels.aiEvent, handler)
    return () => {
      ipcRenderer.removeListener(IpcChannels.aiEvent, handler)
    }
  },

  onCredentialStatus: (listener: (status: CredentialStatus) => void): (() => void) => {
    const handler = (_event: IpcRendererEvent, status: CredentialStatus): void => listener(status)
    ipcRenderer.on(IpcChannels.credentialsStatusChanged, handler)
    return () => {
      ipcRenderer.removeListener(IpcChannels.credentialsStatusChanged, handler)
    }
  },

  // M6：终端输出与退出各走一条专用通道。这里只负责转交，base64 解码在渲染侧做
  // （preload 不该替 UI 决定二进制怎么表示）。
  onTerminalData: (listener: (event: TerminalDataEvent) => void): (() => void) => {
    const handler = (_event: IpcRendererEvent, payload: TerminalDataEvent): void => listener(payload)
    ipcRenderer.on(IpcChannels.terminalData, handler)
    return () => {
      ipcRenderer.removeListener(IpcChannels.terminalData, handler)
    }
  },

  onTerminalExit: (listener: (event: TerminalExitEvent) => void): (() => void) => {
    const handler = (_event: IpcRendererEvent, payload: TerminalExitEvent): void => listener(payload)
    ipcRenderer.on(IpcChannels.terminalExit, handler)
    return () => {
      ipcRenderer.removeListener(IpcChannels.terminalExit, handler)
    }
  },

  // P5 切片七：插件可观测事件。三条通道原样转交——归并、去抖、上限都在
  // 渲染侧的 store 里做，preload 一旦开始加工载荷，两侧就会各有一套口径。
  onPluginLog: (listener: (event: PluginLogEvent) => void): (() => void) => {
    const handler = (_event: IpcRendererEvent, payload: PluginLogEvent): void => listener(payload)
    ipcRenderer.on(IpcChannels.pluginLog, handler)
    return () => {
      ipcRenderer.removeListener(IpcChannels.pluginLog, handler)
    }
  },

  onPluginProgress: (listener: (event: PluginProgressEvent) => void): (() => void) => {
    const handler = (_event: IpcRendererEvent, payload: PluginProgressEvent): void => listener(payload)
    ipcRenderer.on(IpcChannels.pluginProgress, handler)
    return () => {
      ipcRenderer.removeListener(IpcChannels.pluginProgress, handler)
    }
  },

  onPluginStateChanged: (listener: (event: PluginStateChangedEvent) => void): (() => void) => {
    const handler = (_event: IpcRendererEvent, payload: PluginStateChangedEvent): void =>
      listener(payload)
    ipcRenderer.on(IpcChannels.pluginStateChanged, handler)
    return () => {
      ipcRenderer.removeListener(IpcChannels.pluginStateChanged, handler)
    }
  }
}

contextBridge.exposeInMainWorld('aiide', api)
