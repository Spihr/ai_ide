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
  }
}

contextBridge.exposeInMainWorld('aiide', api)
