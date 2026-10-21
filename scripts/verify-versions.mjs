/**
 * verify:versions —— 校验外壳版本与内核（Cargo workspace）版本一致。
 *
 * 不一致会导致「外壳 vX / 内核 vY」的静默漂移：UI 上的 clientVersion 来自
 * apps/desktop/package.json，而 core.hello 上报的 coreVersion 来自 Cargo workspace。
 */

import { readFileSync } from 'node:fs'
import { join, resolve } from 'node:path'

const repoRoot = resolve(import.meta.dirname, '..')

const desktopPkg = JSON.parse(readFileSync(join(repoRoot, 'apps', 'desktop', 'package.json'), 'utf8'))

const cargoToml = readFileSync(join(repoRoot, 'Cargo.toml'), 'utf8')
const workspaceSection = cargoToml.match(/\[workspace\.package\]([\s\S]*?)(\n\[|$)/)
if (!workspaceSection) {
  console.error('[verify:versions] 未在 Cargo.toml 中找到 [workspace.package] 段')
  process.exit(1)
}
const versionMatch = workspaceSection[1].match(/^\s*version\s*=\s*"([^"]+)"/m)
if (!versionMatch) {
  console.error('[verify:versions] 未在 [workspace.package] 中找到 version')
  process.exit(1)
}

const desktopVersion = desktopPkg.version
const coreVersion = versionMatch[1]

const summary = {
  'apps/desktop/package.json version': desktopVersion,
  'Cargo.toml [workspace.package] version': coreVersion,
  consistent: desktopVersion === coreVersion
}
console.log(JSON.stringify(summary, null, 2))

if (!summary.consistent) {
  console.error(
    `[verify:versions] 版本不一致：外壳 ${desktopVersion} vs 内核 ${coreVersion}。` +
      '请先对齐两处版本号再打包。'
  )
  process.exit(1)
}
