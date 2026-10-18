/// <reference types="vite/client" />

import type { AiIdeApi } from '../../shared/ipc'

declare global {
  interface Window {
    /** 由 preload 注入；渲染进程访问主进程能力的唯一入口。 */
    aiide: AiIdeApi
  }
}

export {}
