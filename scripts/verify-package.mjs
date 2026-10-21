/**
 * verify:package —— 打包产物冒烟校验（静态部分，不需要启动应用）。
 *
 * 校验项：
 *   1. 产物目录存在（默认 apps/desktop/release/win-unpacked，可用 --dir 覆盖）
 *   2. resources/core/<exe> 存在、非 0 字节，且大小与 target/release/<exe> 一致
 *   3. app.asar 内**不含** *.exe / ai-ide-core（二进制误入 asar 会导致 spawn 失败）
 *   4. app.asar 内存在 out/main/index.js，且其内部不再出现 require("@ai-ide/shared-types")
 *   5. app.asar 内 package.json 的 main 指向 out/main/index.js
 *   6. 可选（--installer）：release/ 下存在 NSIS 安装包
 *
 * asar 头部布局：| uint32=4 | uint32 headerSize | uint32 jsonSize | json | padding | files… |
 * 因此文件数据区起点 = 8 + headerSize，单文件绝对偏移 = 起点 + entry.offset。
 */

import { closeSync, existsSync, openSync, readSync, readdirSync, statSync } from 'node:fs'
import { join, resolve } from 'node:path'

const repoRoot = resolve(import.meta.dirname, '..')
const args = process.argv.slice(2)
const dirArgIndex = args.findIndex((a) => a === '--dir' || a === '-d')
const checkInstaller = args.includes('--installer')
const exe = process.platform === 'win32' ? 'ai-ide-core.exe' : 'ai-ide-core'

const unpackedDir =
  dirArgIndex >= 0 && args[dirArgIndex + 1]
    ? resolve(args[dirArgIndex + 1])
    : join(repoRoot, 'apps', 'desktop', 'release', 'win-unpacked')

const checks = []
function check(name, ok, detail) {
  checks.push({ name, ok, detail })
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? ` — ${detail}` : ''}`)
}

function fail(message) {
  console.error(`\n[verify:package] 校验失败：${message}`)
  console.error(JSON.stringify({ unpackedDir, checks }, null, 2))
  process.exit(1)
}

// --- 1. 产物目录 ------------------------------------------------------------
check('产物目录存在', existsSync(unpackedDir), unpackedDir)
if (!existsSync(unpackedDir)) fail('产物目录不存在，请先执行 pnpm package:dir')

// --- 2. 内核二进制在 asar 之外 ----------------------------------------------
const packagedCore = join(unpackedDir, 'resources', 'core', exe)
const sourceCore = join(repoRoot, 'target', 'release', exe)
check('resources/core 内核存在', existsSync(packagedCore), packagedCore)
if (!existsSync(packagedCore)) fail(`产物内缺少内核二进制：${packagedCore}`)

const packagedSize = statSync(packagedCore).size
check('内核非 0 字节', packagedSize > 0, `${packagedSize} bytes`)
if (packagedSize === 0) fail('产物内内核为 0 字节')

if (existsSync(sourceCore)) {
  const sourceSize = statSync(sourceCore).size
  check(
    '内核与 target/release 一致',
    sourceSize === packagedSize,
    `target=${sourceSize} packaged=${packagedSize}`
  )
  if (sourceSize !== packagedSize) fail('产物内内核与 target/release 体积不一致')
} else {
  check('target/release 内核可对照', false, `未找到 ${sourceCore}（跳过体积一致性核对）`)
}

// --- 3/4/5. asar 内容 -------------------------------------------------------
const asarPath = join(unpackedDir, 'resources', 'app.asar')
check('app.asar 存在', existsSync(asarPath), asarPath)
if (!existsSync(asarPath)) fail(`产物内缺少 app.asar：${asarPath}`)

/**
 * asar 头部布局（Electron 44 / @electron/asar 实测）：
 *   | uint32 | uint32 headerSize | uint32 jsonSize | uint32 (保留) | json… | padding | files… |
 * 不同版本前缀长度存在差异，因此这里不硬编码偏移：先用 `{"files"` 标记定位 JSON 起点，
 * 再用括号平衡法求 JSON 结束位置，最后用 package.json 的真实内容反推「文件数据区起点」。
 */
function findJsonEnd(buf, start) {
  let depth = 0
  let inStr = false
  let esc = false
  for (let i = start; i < buf.length; i += 1) {
    const c = buf[i]
    if (inStr) {
      if (esc) esc = false
      else if (c === 0x5c) esc = true
      else if (c === 0x22) inStr = false
      continue
    }
    if (c === 0x22) inStr = true
    else if (c === 0x7b) depth += 1
    else if (c === 0x7d) {
      depth -= 1
      if (depth === 0) return i + 1
    }
  }
  throw new Error('asar header JSON 未闭合')
}

function readRange(fd, offset, size) {
  const buf = Buffer.alloc(size)
  readSync(fd, buf, 0, size, offset)
  return buf
}

function readAsar(asarPath) {
  const fd = openSync(asarPath, 'r')
  try {
    const probeSize = Math.min(statSync(asarPath).size, 8 * 1024 * 1024)
    const probe = readRange(fd, 0, probeSize)
    const jsonStart = probe.indexOf('{"files"')
    if (jsonStart < 0) throw new Error('未在 asar 头部找到 {"files" 标记')
    const jsonEnd = findJsonEnd(probe, jsonStart)
    const json = JSON.parse(probe.subarray(jsonStart, jsonEnd).toString('utf8'))

    const headerSize = probe.readUInt32LE(4)
    const declaredJsonSize = probe.readUInt32LE(8)
    const candidates = [
      12 + headerSize,
      8 + headerSize,
      jsonEnd,
      jsonStart + declaredJsonSize
    ].filter((v) => Number.isFinite(v) && v > 0 && v < probeSize)

    let dataStart = candidates[0]
    const pkg = findEntry(json, 'package.json')
    if (pkg) {
      for (const base of candidates) {
        try {
          const text = readRange(fd, base + Number(pkg.offset), Number(pkg.size)).toString('utf8')
          if (JSON.parse(text).name === '@ai-ide/desktop') {
            dataStart = base
            break
          }
        } catch {
          /* 该候选起点不正确，继续尝试下一个 */
        }
      }
    }
    return { json, jsonStart, jsonSize: jsonEnd - jsonStart, headerSize, dataStart }
  } finally {
    closeSync(fd)
  }
}

function findEntry(root, path) {
  const parts = path.split('/').filter(Boolean)
  let node = root
  for (const part of parts) {
    if (!node.files || !node.files[part]) return null
    node = node.files[part]
  }
  return node
}

function readAsarFile(asarPath, dataStart, entry) {
  const size = Number(entry.size)
  const offset = dataStart + Number(entry.offset)
  const fd = openSync(asarPath, 'r')
  try {
    return readRange(fd, offset, size)
  } finally {
    closeSync(fd)
  }
}

function walk(node, prefix, out) {
  for (const [name, value] of Object.entries(node.files ?? {})) {
    const path = prefix ? `${prefix}/${name}` : name
    if (value.files) {
      out.dirs.push(path)
      walk(value, path, out)
    } else {
      out.files.push(path)
    }
  }
}

let asar
try {
  asar = readAsar(asarPath)
} catch (error) {
  check('app.asar 头部可解析', false, String(error))
  fail('app.asar 头部解析失败')
}

const tree = { files: [], dirs: [] }
walk(asar.json, '', tree)

const exeEntries = tree.files.filter((f) => /\.exe$/i.test(f) || /ai-ide-core/i.test(f))
check('asar 内无 *.exe / ai-ide-core', exeEntries.length === 0, exeEntries.join(', ') || '无命中')
if (exeEntries.length > 0) fail(`asar 内误含可执行文件：${exeEntries.join(', ')}`)

const mainEntry = findEntry(asar.json, 'out/main/index.js')
check('asar 内存在 out/main/index.js', Boolean(mainEntry), mainEntry ? `${mainEntry.size} bytes` : '缺失')
if (!mainEntry) fail('asar 内缺少 out/main/index.js')

if (mainEntry && !mainEntry.unpacked) {
  const mainCode = readAsarFile(asarPath, asar.dataStart, mainEntry).toString('utf8')
  const leftover = mainCode.includes('require("@ai-ide/shared-types")')
  check('main 产物未残留 require("@ai-ide/shared-types")', !leftover, leftover ? '仍存在外置 require' : '已内联')
  if (leftover) fail('main 产物仍 externalize @ai-ide/shared-types，打包态将无法解析')
}

const pkgEntry = findEntry(asar.json, 'package.json')
if (pkgEntry && !pkgEntry.unpacked) {
  const pkg = JSON.parse(readAsarFile(asarPath, asar.dataStart, pkgEntry).toString('utf8'))
  const mainOk = pkg.main === './out/main/index.js'
  check('asar 内 package.json.main 指向 out/main/index.js', mainOk, String(pkg.main))
  if (!mainOk) fail(`asar 内 package.json.main 异常：${pkg.main}`)
} else {
  check('asar 内存在 package.json', false, '缺失')
  fail('asar 内缺少 package.json')
}

const nodeModulesInAsar = tree.dirs.filter((d) => d.startsWith('node_modules'))
check(
  'asar 内无 node_modules（白名单外内容）',
  nodeModulesInAsar.length === 0,
  nodeModulesInAsar.slice(0, 5).join(', ') || '无'
)

// --- 6. 安装包 --------------------------------------------------------------
const releaseDir = join(repoRoot, 'apps', 'desktop', 'release')
if (checkInstaller) {
  const installers = existsSync(releaseDir)
    ? readdirSync(releaseDir).filter((f) => f.toLowerCase().endsWith('.exe') && !f.endsWith('.blockmap'))
    : []
  check('release/ 下存在 NSIS 安装包', installers.length > 0, installers.join(', ') || '未找到')
  if (installers.length === 0) fail('未在 release/ 下找到安装包')
}

const summary = {
  unpackedDir,
  asarBytes: statSync(asarPath).size,
  asarFileCount: tree.files.length,
  coreBytes: packagedSize,
  asarTopLevel: Object.keys(asar.json.files ?? {}),
  checks,
  passed: checks.every((c) => c.ok)
}
console.log(JSON.stringify(summary, null, 2))

if (!summary.passed) fail('存在未通过的校验项')
console.log('\n[verify:package] 全部静态校验通过')
