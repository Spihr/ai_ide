import { defineConfig } from 'electron-vite'
import react from '@vitejs/plugin-react'

/**
 * 三段式构建：main / preload / renderer。
 *
 * 入口全部走 electron-vite 的默认约定
 * （`src/main/index.ts`、`src/preload/index.ts`、`src/renderer/index.html`）。
 *
 * 打包态（electron-builder + asar）要求 main / preload 产物**自包含**：
 * `packages/shared-types` 是 workspace 内部包，`exports` 指向 `src/index.ts`
 * （TS 源码，仓库内无 dist 产物）。electron-vite 6 默认会把 `dependencies`
 * 里的包 externalize 成运行期 `require()`，一旦被装进 asar，Node 解析到的
 * 是 `.ts` 源码入口，能否加载取决于 Electron 内置 Node 的类型剥离能力——
 * 不可依赖。故显式把它从 externalize 名单中排除，使其内联进 bundle。
 *
 * `electron` 与 Node 内建模块仍保持 external（由 Electron 运行时提供），
 * 这部分是 electron-vite 的默认 external，不受此处 exclude 影响。
 */
const bundleInternalPackages = {
  build: {
    externalizeDeps: { exclude: ['@ai-ide/shared-types'] }
  }
}

export default defineConfig({
  main: bundleInternalPackages,
  preload: bundleInternalPackages,
  renderer: {
    plugins: [react()]
  }
})
