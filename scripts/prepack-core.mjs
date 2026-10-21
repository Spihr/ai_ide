/**
 * prepack:core —— 构建 release 内核并把它放到打包器的 extraResources 源目录。
 *
 * 流程：
 *   1. `cargo build --release -p ai-ide-core`
 *   2. 校验 target/release/<exe> 存在且非 0 字节
 *   3. 校验二进制比 Rust 源码 / Cargo 清单更新（避免「新代码 + 旧内核」的静默不一致）
 *   4. 复制到 apps/desktop/build/core/<exe>（extraResources 的 from 路径）
 *
 * 平台规则：Windows 为 `ai-ide-core.exe`，其余平台无后缀，
 * 与 apps/desktop/src/main/index.ts 的 resolveCoreBinary() 判定保持一致。
 */

import { execFileSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync, readdirSync, statSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'

const repoRoot = resolve(import.meta.dirname, '..')
const exe = process.platform === 'win32' ? 'ai-ide-core.exe' : 'ai-ide-core'
const source = join(repoRoot, 'target', 'release', exe)
const targetDir = join(repoRoot, 'apps', 'desktop', 'build', 'core')
const target = join(targetDir, exe)

const skipBuild = process.argv.includes('--no-build')

function fail(message) {
  console.error(`[prepack:core] ${message}`)
  process.exit(1)
}

if (!skipBuild) {
  console.log('[prepack:core] cargo build --release -p ai-ide-core')
  execFileSync('cargo', ['build', '--release', '-p', 'ai-ide-core'], { cwd: repoRoot, stdio: 'inherit' })
}

if (!existsSync(source)) fail(`release 内核不存在：${source}`)

const binaryStat = statSync(source)
if (binaryStat.size === 0) fail(`release 内核大小为 0：${source}`)

/** Rust 源码与清单里最新的修改时间。 */
function latestRustMtime() {
  let latest = 0
  let latestPath = ''
  const visit = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (entry.name === 'target' || entry.name === 'node_modules' || entry.name === '.git') continue
      const full = join(dir, entry.name)
      if (entry.isDirectory()) {
        visit(full)
        continue
      }
      if (!/\.(rs|toml|lock)$/.test(entry.name)) continue
      const mtime = statSync(full).mtimeMs
      if (mtime > latest) {
        latest = mtime
        latestPath = full
      }
    }
  }
  visit(join(repoRoot, 'crates'))
  for (const name of ['Cargo.toml', 'Cargo.lock']) {
    const full = join(repoRoot, name)
    if (!existsSync(full)) continue
    const mtime = statSync(full).mtimeMs
    if (mtime > latest) {
      latest = mtime
      latestPath = full
    }
  }
  return { mtime: latest, path: latestPath }
}

const rust = latestRustMtime()
if (rust.mtime > binaryStat.mtimeMs) {
  fail(
    `内核二进制早于 Rust 源码，拒绝复制（新代码 + 旧内核）：\n` +
      `  二进制 ${source} @ ${new Date(binaryStat.mtimeMs).toISOString()}\n` +
      `  源码   ${rust.path} @ ${new Date(rust.mtime).toISOString()}`
  )
}

mkdirSync(targetDir, { recursive: true })
copyFileSync(source, target)

const copied = statSync(target)
const summary = {
  platform: process.platform,
  built: !skipBuild,
  from: source,
  to: target,
  bytes: copied.size,
  sizeMB: Number((copied.size / 1024 / 1024).toFixed(2)),
  binaryMtime: new Date(copied.mtimeMs).toISOString(),
  newestRustSource: rust.path
}

writeFileSync(join(targetDir, 'core-manifest.json'), `${JSON.stringify(summary, null, 2)}\n`)
console.log(JSON.stringify(summary, null, 2))
