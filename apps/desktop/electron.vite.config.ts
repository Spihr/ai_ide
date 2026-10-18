import { defineConfig } from 'electron-vite'
import react from '@vitejs/plugin-react'

/**
 * 三段式构建：main / preload / renderer。
 *
 * M0 刻意保持配置最小化——入口全部走 electron-vite 的默认约定
 * （`src/main/index.ts`、`src/preload/index.ts`、`src/renderer/index.html`）。
 * 后续引入原生模块或第二个渲染进程时再显式展开。
 */
export default defineConfig({
  main: {},
  preload: {},
  renderer: {
    plugins: [react()]
  }
})
