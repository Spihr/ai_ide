/**
 * M8 干净环境安装验收（Playwright + 打包产物）。
 *
 * 与 e2e-m1.mjs 的分工：
 *   - e2e-m1.mjs：从源码目录 `electron .` 拉起，验证开发态链路；
 *   - 本脚本：拉起 **electron-builder 的 win-unpacked 产物**（`ai-ide.exe`），
 *     并把 `USERPROFILE` / `APPDATA` / `LOCALAPPDATA` / `TEMP` 全部指到临时目录，
 *     模拟「这台机器从没装过本应用」的干净用户配置，验证：
 *       1. 包内 `resources/core/ai-ide-core.exe` 能被真实拉起（不是开发树 target/debug）；
 *       2. 用户数据落在隔离 profile 下（不污染真机 %APPDATA%）；
 *       3. 打开工作区 → 文件树 → Monaco 打开文件 → 读到磁盘正文（打包内核真的能干活）；
 *       4. MCP 面板可从活动栏进入并如实报「还没配置」（M7 的能力确实进了包）；
 *       5. 退出后无遗留 ai-ide-core 进程。
 *
 * 前置：`pnpm dist`（或 `pnpm package:dir`）+ `pnpm verify:package`。
 * 用法：`node scripts/e2e-m8.mjs`
 *   --dir <路径>   指定 win-unpacked 目录（默认 apps/desktop/release/win-unpacked）
 *   --keep         保留临时 profile 与工作区（排查用）
 */

import { execSync } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { performance } from 'node:perf_hooks'

const require = createRequire(import.meta.url)
const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')

const args = process.argv.slice(2)
const dirArgIndex = args.findIndex((a) => a === '--dir' || a === '-d')
const keep = args.includes('--keep')
const isWin = process.platform === 'win32'
const exeName = isWin ? 'ai-ide.exe' : 'ai-ide'

const unpackedDir =
  dirArgIndex >= 0 && args[dirArgIndex + 1]
    ? resolve(args[dirArgIndex + 1])
    : join(repoRoot, 'apps', 'desktop', 'release', 'win-unpacked')

// 可执行文件名跟 electron-builder 的 productName 走（本仓库是「AI IDE」），
// 不写死 ai-ide.exe：productName 改过一次就再也找不到产物了。
function productName() {
  try {
    const text = readFileSync(join(repoRoot, 'apps', 'desktop', 'electron-builder.yml'), 'utf8')
    const match = text.match(/^productName:\s*(.+)$/m)
    return match ? match[1].trim().replace(/^["']|["']$/g, '') : null
  } catch {
    return null
  }
}
const name = productName()
const exeCandidates = [
  name ? `${name}${isWin ? '.exe' : ''}` : null,
  exeName,
  isWin ? 'ai-ide.exe' : 'ai-ide'
].filter(Boolean)
const appExe = exeCandidates.map((candidate) => join(unpackedDir, candidate)).find((path) => existsSync(path))
const exeLabel = appExe ? appExe.slice(unpackedDir.length + 1) : exeCandidates[0]
const packagedCore = join(unpackedDir, 'resources', 'core', isWin ? 'ai-ide-core.exe' : 'ai-ide-core')

const profileRoot = join(repoRoot, 'target', 'e2e-m8-profile')
const wsRoot = join(repoRoot, 'target', 'e2e-m8-workspace')

const results = []
const check = (name, ok, detail = '') => {
  results.push({ name, ok })
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  ${detail}` : ''}`)
}
const sleep = (ms) => new Promise((done) => setTimeout(done, ms))

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

function countCoreProcesses() {
  if (!isWin) return 0
  try {
    const out = execSync('tasklist /FI "IMAGENAME eq ai-ide-core.exe" /NH', { encoding: 'utf8' })
    return out.split('\n').filter((line) => line.includes('ai-ide-core.exe')).length
  } catch {
    return -1
  }
}

// ---------------------------------------------------------------------------
// 0. 产物前置检查
// ---------------------------------------------------------------------------

if (!appExe) {
  console.error(`找不到打包产物：${join(unpackedDir, exeCandidates[0])}\n请先执行 pnpm dist（或 pnpm package:dir）`)
  process.exit(1)
}
check(
  '打包产物存在',
  true,
  `${exeLabel} · ${(statSync(appExe).size / 1024 / 1024).toFixed(1)} MB`
)

const coreInPackage = existsSync(packagedCore) && statSync(packagedCore).size > 0
check(
  '包内 resources/core 内核就位',
  coreInPackage,
  coreInPackage ? `${(statSync(packagedCore).size / 1024 / 1024).toFixed(1)} MB` : packagedCore
)
if (!coreInPackage) {
  console.error('[e2e:m8] 包内缺内核二进制，后续断言无法进行')
  process.exit(1)
}

// 干净 profile：把 Windows 的四处用户目录都挪进临时目录
rmSync(profileRoot, { recursive: true, force: true })
const env = {
  ...process.env,
  USERPROFILE: profileRoot,
  APPDATA: join(profileRoot, 'AppData', 'Roaming'),
  LOCALAPPDATA: join(profileRoot, 'AppData', 'Local'),
  TEMP: join(profileRoot, 'Temp'),
  TMP: join(profileRoot, 'Temp')
}
// 不允许开发态的路径覆盖渗进来
for (const key of ['AI_IDE_CORE_PATH', 'AI_IDE_MCP_CONFIG', 'AI_IDE_DATA_DIR', 'AI_IDE_CORE_PROFILE']) {
  delete env[key]
}
for (const dir of [env.APPDATA, env.LOCALAPPDATA, env.TEMP]) mkdirSync(dir, { recursive: true })

// 干净工作区
rmSync(wsRoot, { recursive: true, force: true })
mkdirSync(join(wsRoot, 'src'), { recursive: true })
const mainTs = join(wsRoot, 'src', 'main.ts')
writeFileSync(mainTs, 'export function main(): string {\n  return "packaged-hello"\n}\n', 'utf8')
writeFileSync(join(wsRoot, 'notes.md'), '# notes\n\n干净环境验收\n', 'utf8')

console.log(`产物：${unpackedDir}`)
console.log(`隔离 profile：${profileRoot}`)
console.log(`工作区：${wsRoot}\n`)

const { _electron: electron } = require('playwright-core')
const coreCountBefore = countCoreProcesses()
let app

try {
  app = await electron.launch({ executablePath: appExe, env, timeout: 60_000 })
  const page = await app.firstWindow()
  page.on('dialog', (dialog) => {
    void dialog.accept()
  })
  await page.waitForSelector('.titlebar', { timeout: 30_000 })
  check('打包应用启动并渲染出外壳', true, `title=${await page.locator('.titlebar__title').innerText()}`)

  const version = await app.evaluate(({ app: electronApp }) => electronApp.getVersion())
  const pkg = JSON.parse(readFileSync(join(repoRoot, 'package.json'), 'utf8')).version
  check('应用版本与仓库一致', String(version) === String(pkg), `app=${version} repo=${pkg}`)

  const coreReady = await waitUntil(
    async () => (await page.evaluate(() => window.aiide.getCoreStatus())).state === 'ready',
    30_000
  )
  const status = await page.evaluate(() => window.aiide.getCoreStatus())
  check(
    '包内内核被拉起并就绪',
    coreReady >= 0,
    `${coreReady.toFixed(0)} ms, core=${status.hello?.core_version}`
  )
  check(
    '内核版本与包版本一致',
    String(status.hello?.core_version ?? '') === String(version),
    `core=${status.hello?.core_version}`
  )

  // 用户数据必须落在隔离 profile 内
  const userData = await app.evaluate(({ app: electronApp }) => electronApp.getPath('userData'))
  check(
    '用户数据落在隔离 profile（不污染真机 %APPDATA%）',
    userData.toLowerCase().startsWith(profileRoot.toLowerCase()),
    userData
  )

  // 内核落盘位置由主进程通过 AI_IDE_DATA_DIR 指定（当前即 userData），也可能回落到
  // %APPDATA%\ai-ide。两处都算，只看「干净 profile 下真的建出了内核文件」。
  const kernelHomes = [userData, join(env.APPDATA, 'ai-ide')]
  const findArtifacts = () =>
    kernelHomes
      .filter((dir) => existsSync(dir))
      .flatMap((dir) =>
        readdirSync(dir)
          .filter((entry) => /^(sessions\.db|providers\.json|logs|checkpoints|mcp)/.test(entry))
          .map((entry) => `${dirname(dir) === userData ? 'userData' : 'appdata'}/${entry}`)
      )
  const artifactsReady = await waitUntil(async () => findArtifacts().length > 0, 15_000)
  const artifacts = findArtifacts()
  check(
    '内核在干净 profile 下自建数据/日志',
    artifactsReady >= 0,
    `${artifactsReady.toFixed(0)} ms, ${artifacts.join(', ') || kernelHomes.join(' | ')}`
  )

  // ---- 打开工作区（stub 原生目录框，点击仍走真实 UI 链路）----
  await app.evaluate(({ dialog }, path) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [path] })
  }, wsRoot)
  await page.getByRole('button', { name: '打开目录', exact: true }).click()

  const treeAppeared = await waitUntil(async () => (await page.locator('.tree__row').count()) >= 2, 20_000)
  const rows = await page.locator('.tree__row').allInnerTexts()
  check(
    '打开工作区后文件树渲染',
    treeAppeared >= 0 && rows.some((row) => row.includes('src')),
    `${treeAppeared.toFixed(0)} ms, rows=${JSON.stringify(rows)}`
  )

  await page.locator('.tree__row', { hasText: 'src' }).first().click()
  await waitUntil(async () => (await page.locator('.tree__row', { hasText: 'main.ts' }).count()) >= 1, 10_000)
  await page.locator('.tree__row', { hasText: 'main.ts' }).first().click()
  await page.waitForSelector('[role=tab]', { timeout: 10_000 })
  const editorLoaded = await waitUntil(
    async () => (await page.locator('.view-lines').innerText()).includes('packaged-hello'),
    15_000
  )
  check(
    '点击文件在 Monaco 中加载磁盘正文',
    editorLoaded >= 0,
    `${editorLoaded.toFixed(0)} ms, tab=${await page.locator('.tab__name').first().innerText()}`
  )

  // ---- MCP 面板（M7 能力是否真进了包）----
  await page.getByRole('button', { name: 'MCP Server', exact: true }).click()
  const mcpOpened = await waitUntil(
    async () => (await page.locator('[data-testid=mcp-panel]').count()) >= 1,
    10_000
  )
  const emptyHint = mcpOpened >= 0 ? (await page.locator('[data-testid=mcp-panel]').innerText()) : ''
  check(
    'MCP 面板可进入且如实报「还没配置」',
    mcpOpened >= 0 && emptyHint.includes('MCP Server'),
    `${mcpOpened.toFixed(0)} ms`
  )
  check(
    'MCP 视图不因缺 mcp.json 而报错',
    mcpOpened >= 0 && !emptyHint.includes('读取失败'),
    emptyHint.replace(/\s+/g, ' ').slice(0, 80)
  )

  // ---- 关闭工作区后退出 ----
  await page.getByRole('button', { name: '关闭', exact: true }).click()
  await waitUntil(async () => (await page.locator('.welcome').count()) >= 1, 10_000)

  await app.close()
  app = null
  await sleep(2_000)
  const coreCountAfter = countCoreProcesses()
  check(
    '退出后无遗留内核进程',
    coreCountAfter <= coreCountBefore,
    `before=${coreCountBefore} after=${coreCountAfter}`
  )
} catch (err) {
  check(`干净环境验收未抛异常（${err instanceof Error ? err.message : String(err)}）`, false)
} finally {
  if (app) await app.close().catch(() => undefined)
  if (!keep) {
    rmSync(profileRoot, { recursive: true, force: true })
    rmSync(wsRoot, { recursive: true, force: true })
  }
}

const passed = results.filter((item) => item.ok).length
console.log(`\nM8 干净环境验收: ${passed}/${results.length} 项通过`)
if (passed !== results.length) {
  console.log('未通过项：')
  for (const item of results.filter((entry) => !entry.ok)) console.log(`  - ${item.name}`)
}
process.exit(passed === results.length ? 0 : 1)
