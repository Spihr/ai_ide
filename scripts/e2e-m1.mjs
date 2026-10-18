/**
 * M1 端到端冒烟（Playwright + Electron）。
 *
 * 与 smoke-m1.mjs 的分工：
 *   - smoke-m1.mjs：绕过 Electron，直接对内核二进制打协议，验证内核行为；
 *   - 本脚本：**看真界面**。拉起真实的 Electron 应用，用真实点击驱动渲染进程，
 *     验证「UI → preload → 策略闸门 → 主进程 → 内核」整条链路，最后落到磁盘上核对。
 *
 * 前置：`pnpm --filter @ai-ide/desktop build` 已产出 out/main 与 out/renderer。
 * 用法：`node scripts/e2e-m1.mjs`
 */

import { execSync, spawn } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { performance } from 'node:perf_hooks'

const require = createRequire(import.meta.url)
const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const desktopDir = join(repoRoot, 'apps', 'desktop')
const wsRoot = join(repoRoot, 'target', 'e2e-m1-workspace')

const results = []
const check = (name, ok, detail = '') => {
  results.push({ name, ok })
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  ${detail}` : ''}`)
}
const sleep = (ms) => new Promise((done) => setTimeout(done, ms))

/** 轮询直到条件成立或超时；返回耗时（ms）或 -1（超时）。 */
async function waitUntil(fn, timeoutMs, intervalMs = 50) {
  const started = performance.now()
  for (;;) {
    try {
      if (await fn()) return performance.now() - started
    } catch {
      /* 未就绪，继续等 */
    }
    if (performance.now() - started > timeoutMs) return -1
    await sleep(intervalMs)
  }
}

// ---------------------------------------------------------------------------
// 0. 前置检查与工作区准备
// ---------------------------------------------------------------------------

const mainEntry = join(desktopDir, 'out', 'main', 'index.js')
const rendererEntry = join(desktopDir, 'out', 'renderer', 'index.html')
if (!existsSync(mainEntry) || !existsSync(rendererEntry)) {
  console.error('缺少构建产物，请先执行：pnpm --filter @ai-ide/desktop build')
  process.exit(1)
}

const coreExe = join(repoRoot, 'target', 'debug', process.platform === 'win32' ? 'ai-ide-core.exe' : 'ai-ide-core')
if (!existsSync(coreExe)) {
  console.error(`找不到内核可执行文件：${coreExe}（先执行 cargo build -p ai-ide-core）`)
  process.exit(1)
}

const desktopRequire = createRequire(join(desktopDir, 'package.json'))
const electronExe = desktopRequire('electron')
const { _electron: electron } = require('playwright-core')

rmSync(wsRoot, { recursive: true, force: true })
mkdirSync(join(wsRoot, 'src'), { recursive: true })
const mainTs = join(wsRoot, 'src', 'main.ts')
const readmeMd = join(wsRoot, 'notes.md')
const mainTsInitial = 'export function main(): string {\n  return "hello"\n}\n'
writeFileSync(mainTs, mainTsInitial, 'utf8')
writeFileSync(readmeMd, '# notes\n\n第一版内容\n', 'utf8')

console.log(`工作区：${wsRoot}`)
console.log(`Electron：${electronExe}\n`)

const coreCountBefore = countCoreProcesses()
let app
let page

try {
  app = await electron.launch({
    executablePath: electronExe,
    args: ['.'],
    cwd: desktopDir,
    env: { ...process.env, AI_IDE_CORE_PROFILE: 'debug' }
  })
  page = await app.firstWindow()
  // 关闭脏工作区会弹原生 confirm，测试里一律确认。
  page.on('dialog', (dialog) => {
    void dialog.accept()
  })
  await page.waitForSelector('.titlebar', { timeout: 15_000 })
  check('Electron 应用启动并渲染出外壳', true, `title=${await page.locator('.titlebar__title').innerText()}`)

  const coreReady = await waitUntil(
    async () =>
      (await page.evaluate(() => window.aiide.getCoreStatus())).state === 'ready',
    20_000
  )
  const hello = await page.evaluate(async () => (await window.aiide.getCoreStatus()).hello)
  check('内核就绪（UI 侧看到 ready）', coreReady >= 0, `${coreReady.toFixed(0)} ms, core=${hello?.core_version}`)

  // 原生目录选择框无法被自动化；stub 掉 dialog，点击仍是真实 UI 路径。
  await app.evaluate(({ dialog }, path) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [path] })
  }, wsRoot)

  await page.getByRole('button', { name: '打开目录', exact: true }).click()

  // 诊断探针：在 renderer 里独立记数「有没有收到内核事件」，
  // 用于区分「事件没到 renderer」与「store 没匹配/没渲染」。
  await page.evaluate(() => {
    window.__wsEvents = []
    window.aiide.onWorkspaceChanged((event) => window.__wsEvents.push(event))
  })

  const treeAppeared = await waitUntil(
    async () => (await page.locator('.tree__row').count()) >= 2,
    10_000
  )
  const rootRows = await page.locator('.tree__row').allInnerTexts()
  check(
    '点击「打开目录」后文件树渲染出工作区条目',
    treeAppeared >= 0 && rootRows.some((row) => row.includes('src')),
    `${treeAppeared.toFixed(0)} ms, rows=${JSON.stringify(rootRows)}`
  )

  check(
    '标题栏显示工作区名与根路径',
    (await page.locator('.titlebar__root').innerText()).toLowerCase() === wsRoot.toLowerCase()
  )

  // ---- 打开文件：树 → 标签 → Monaco ----
  await page.locator('.tree__row', { hasText: 'src' }).first().click()
  const fileRowAppeared = await waitUntil(
    async () => (await page.locator('.tree__row', { hasText: 'main.ts' }).count()) >= 1,
    10_000
  )
  check('展开目录懒加载出子条目', fileRowAppeared >= 0, `${fileRowAppeared.toFixed(0)} ms`)

  await page.locator('.tree__row', { hasText: 'main.ts' }).first().click()
  await page.waitForSelector('[role=tab]', { timeout: 10_000 })
  const tabName = await page.locator('.tab__name').first().innerText()
  const editorLoaded = await waitUntil(
    async () => (await page.locator('.view-lines').innerText()).includes('hello'),
    10_000
  )
  check('点击文件打开标签并在 Monaco 中加载内容', tabName === 'main.ts' && editorLoaded >= 0, `${editorLoaded.toFixed(0)} ms`)

  // ---- 编辑 → 保存（按钮路径）----
  await page.locator('.monaco-editor .view-lines').click()
  await page.keyboard.press('Control+End')
  await page.keyboard.press('Escape')
  await page.keyboard.type('// e2e-via-button\n')
  const dirtied = await waitUntil(async () => (await page.locator('.tab__dot').count()) >= 1, 5_000)
  check('编辑后标签出现未保存标记', dirtied >= 0, `${dirtied.toFixed(0)} ms`)

  await page.getByRole('button', { name: '保存', exact: true }).click()
  const savedByButton = await waitUntil(
    async () => readFileSync(mainTs, 'utf8').includes('// e2e-via-button'),
    10_000
  )
  check('点「保存」后磁盘内容已更新', savedByButton >= 0, `${savedByButton.toFixed(0)} ms`)

  const cleanAgain = await waitUntil(async () => (await page.locator('.tab__dot').count()) === 0, 5_000)
  check('保存后未保存标记消失', cleanAgain >= 0)

  // ---- 编辑 → 保存（快捷键路径）----
  await page.locator('.monaco-editor .view-lines').click()
  await page.keyboard.press('Control+End')
  await page.keyboard.press('Escape')
  await page.keyboard.type('// e2e-via-hotkey\n')
  await waitUntil(async () => (await page.locator('.tab__dot').count()) >= 1, 5_000)
  await page.keyboard.press('Control+s')
  const savedByHotkey = await waitUntil(
    async () => readFileSync(mainTs, 'utf8').includes('// e2e-via-hotkey'),
    10_000
  )
  check('Ctrl+S 保存到磁盘', savedByHotkey >= 0, `${savedByHotkey.toFixed(0)} ms`)

  // ---- 外部改动（干净标签）→ 自动重载 ----
  writeFileSync(mainTs, `${readFileSync(mainTs, 'utf8')}// external-clean\n`, 'utf8')
  const reloaded = await waitUntil(
    async () => (await page.locator('.view-lines').innerText()).includes('external-clean'),
    10_000
  )
  const rawEvents = await page.evaluate(() =>
    (window.__wsEvents ?? []).map((event) =>
      (event.changes ?? []).map((change) => `${change.kind}:${change.path}`).join('|')
    )
  )
  console.log(`   [诊断] renderer 收到事件批次：${JSON.stringify(rawEvents)}`)
  check('外部改动干净标签后 UI 自动重载', reloaded >= 0, `${reloaded.toFixed(0)} ms`)

  // ---- 外部改动（脏标签）→ 冲突提示 ----
  await page.locator('.monaco-editor .view-lines').click()
  await page.keyboard.press('Control+End')
  await page.keyboard.press('Escape')
  await page.keyboard.type('// local-unsaved\n')
  await waitUntil(async () => (await page.locator('.tab__dot').count()) >= 1, 5_000)

  const conflictStart = performance.now()
  writeFileSync(mainTs, `${readFileSync(mainTs, 'utf8')}// external-dirty\n`, 'utf8')
  const noticeShown = await waitUntil(
    async () => (await page.locator('.notice').count()) >= 1,
    10_000
  )
  const noticeText =
    noticeShown >= 0 ? ((await page.locator('.notice').first().textContent()) ?? '').replace(/\s+/g, ' ').trim() : ''
  check(
    '外部改动脏标签后弹出冲突提示',
    noticeShown >= 0,
    `${(performance.now() - conflictStart).toFixed(0)} ms, notice=${noticeText}`
  )
  check('冲突提示在 1s 内出现（M1 验收：1s 内提示）', noticeShown >= 0 && noticeShown < 1_000, `${noticeShown.toFixed(0)} ms`)

  // ---- 搜索：文件名模糊 + 内容检索 + 命中跳转 ----
  await page.getByRole('button', { name: '搜索' }).click()
  const searchOpened = await waitUntil(async () => (await page.locator('.search__input').count()) >= 1, 8_000)
  check('活动栏可切到搜索面板', searchOpened >= 0, `${searchOpened.toFixed(0)} ms`)

  await page.locator('.search__input').fill('notes')
  const fileHit = await waitUntil(async () => (await page.locator('.results__row').count()) >= 1, 10_000)
  const fileHitText = fileHit >= 0 ? ((await page.locator('.results__row').first().textContent()) ?? '').trim() : ''
  check(
    '文件名模糊搜索命中 notes.md',
    fileHit >= 0 && fileHitText.includes('notes.md'),
    `${fileHit.toFixed(0)} ms, hit=${fileHitText}`
  )

  await page.getByRole('tab', { name: '内容' }).click()
  await page.locator('.search__input').fill('hello')
  const contentHit = await waitUntil(async () => (await page.locator('.results__line').count()) >= 1, 10_000)
  const contentHitText =
    contentHit >= 0
      ? ((await page.locator('.results__row').first().textContent()) ?? '').replace(/\s+/g, ' ').trim()
      : ''
  check(
    '内容检索命中 main.ts 里的 hello',
    contentHit >= 0 && contentHitText.includes('main.ts'),
    `${contentHit.toFixed(0)} ms, hit=${contentHitText}`
  )

  await page.locator('.results__row').first().click()
  const jumped = await waitUntil(async () => (await page.locator('.view-lines').innerText()).includes('hello'), 8_000)
  check('点击内容命中可跳转到对应文件', jumped >= 0, `${jumped.toFixed(0)} ms`)

  // ---- 关闭工作区 ----
  await page.getByRole('button', { name: '关闭', exact: true }).click()
  const backToWelcome = await waitUntil(
    async () => (await page.locator('.welcome').count()) >= 1,
    10_000
  )
  check('「关闭」回到欢迎页', backToWelcome >= 0, `${backToWelcome.toFixed(0)} ms`)

  // ---- 退出：无孤儿进程 ----
  await app.close()
  app = null
  await sleep(1_500)
  const coreCountAfter = countCoreProcesses()
  check(
    '退出后无遗留内核进程',
    coreCountAfter <= coreCountBefore,
    `before=${coreCountBefore} after=${coreCountAfter}`
  )
} catch (err) {
  check(`端到端执行未抛异常（${err instanceof Error ? err.message : String(err)}）`, false)
} finally {
  if (app) await app.close().catch(() => undefined)
}

const passed = results.filter((item) => item.ok).length
console.log(`\n端到端结论: ${passed}/${results.length} 项通过`)
if (passed !== results.length) {
  console.log('未通过项：')
  for (const item of results.filter((entry) => !entry.ok)) console.log(`  - ${item.name}`)
}
process.exit(passed === results.length ? 0 : 1)

function countCoreProcesses() {
  if (process.platform !== 'win32') return 0
  try {
    const out = execSync('tasklist /FI "IMAGENAME eq ai-ide-core.exe" /NH', { encoding: 'utf8' })
    return out.split('\n').filter((line) => line.includes('ai-ide-core.exe')).length
  } catch {
    return -1
  }
}

// 供将来扩展：保留 spawn 引用避免 lint 抱怨
void spawn
