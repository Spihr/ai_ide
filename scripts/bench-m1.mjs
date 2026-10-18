/**
 * M1 性能基准：把 05-里程碑与验证计划.md 里 M1 的两条硬指标落成可复跑的实测。
 *
 * 验收指标（文档原文）：
 *   - 10 万文件目录从打开到树首屏 < 3s，全量索引 < 30s；
 *   - 编辑大文件（1 万行）输入延迟 < 16ms/frame（这里用「读写往返」做代理指标）。
 *
 * 用法：
 *   node scripts/bench-m1.mjs                # 默认 10 万文件，工作区建在 target/bench-m1-workspace
 *   BENCH_FILES=20000 node scripts/bench-m1.mjs
 *
 * 工作区生成一次后复用（存在 .bench-ready 标记即跳过生成）。
 */

import { spawn } from 'node:child_process'
import { createInterface } from 'node:readline'
import { randomUUID } from 'node:crypto'
import { existsSync, mkdirSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { performance } from 'node:perf_hooks'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const EXE = join(repoRoot, 'target', 'debug', process.platform === 'win32' ? 'ai-ide-core.exe' : 'ai-ide-core')
if (!existsSync(EXE)) {
  console.error(`找不到内核可执行文件：${EXE}（先执行 cargo build -p ai-ide-core）`)
  process.exit(1)
}

const FILE_COUNT = Number(process.env.BENCH_FILES ?? 100_000)
const PER_DIR = 100
const benchRoot = join(repoRoot, 'target', 'bench-m1-workspace')
const marker = join(benchRoot, '.bench-ready')
const fmt = (value) => `${value.toFixed(1)} ms`

// ---------------------------------------------------------------------------
// 1. 生成工作区（只生成一次）
// ---------------------------------------------------------------------------

if (existsSync(marker) && readdirSync(benchRoot).length > 1) {
  console.log(`复用已有基准工作区：${benchRoot}`)
} else {
  console.log(`生成基准工作区：${benchRoot}（${FILE_COUNT} 个文件）`)
  rmSync(benchRoot, { recursive: true, force: true })
  mkdirSync(benchRoot, { recursive: true })
  const payload = `// bench payload\n${'const value = 1\n'.repeat(8)}`
  const genStart = performance.now()
  const made = new Set()
  for (let i = 0; i < FILE_COUNT; i += 1) {
    const dir = join(benchRoot, `d${Math.floor(i / PER_DIR)}`)
    if (!made.has(dir)) {
      mkdirSync(dir, { recursive: true })
      made.add(dir)
    }
    writeFileSync(join(dir, `f${i}.ts`), `${payload}// ${i}\n`, 'utf8')
    if (i > 0 && i % 20_000 === 0) console.log(`  ...已生成 ${i}`)
  }
  writeFileSync(join(benchRoot, 'README.md'), '# bench\n', 'utf8')
  const total = statSync(benchRoot)
  console.log(`生成完成，用时 ${fmt(performance.now() - genStart)}（挂载点 ${total.isDirectory() ? 'ok' : '?'}）\n`)
}

// ---------------------------------------------------------------------------
// 2. 起内核，跑基准
// ---------------------------------------------------------------------------

const pending = new Map()
let stderrText = ''
const child = spawn(EXE, [], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true })
const rl = createInterface({ input: child.stdout, crlfDelay: Infinity })
child.stderr.setEncoding('utf8')
child.stderr.on('data', (chunk) => {
  stderrText += chunk
})

rl.on('line', (line) => {
  const text = line.trim()
  if (text.length === 0) return
  let frame
  try {
    frame = JSON.parse(text)
  } catch {
    return
  }
  if (frame.id && ('result' in frame || 'error' in frame)) {
    const slot = pending.get(frame.id)
    if (!slot) return
    pending.delete(frame.id)
    if (frame.error) slot.reject(new Error(`[${frame.error.code}] ${frame.error.message}`))
    else slot.resolve(frame.result)
    return
  }
  console.log('[通知]', frame.method)
})

function request(method, params = {}) {
  const id = randomUUID()
  return new Promise((resolveRequest, rejectRequest) => {
    pending.set(id, { resolve: resolveRequest, reject: rejectRequest })
    child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
  })
}

async function timed(label, fn) {
  const started = performance.now()
  const value = await fn()
  const took = performance.now() - started
  console.log(`  ${label.padEnd(38)} ${fmt(took)}`)
  return { took, value }
}

await request('core.hello', {
  client_version: '0.1.0',
  protocol_version: { major: 1, minor: 0 },
  platform: process.platform,
  locale: 'zh-CN',
  capabilities: []
})

console.log('--- 打开与索引 ---')
const opened = await timed('workspace.open（含索引）', () =>
  request('workspace.open', { path: benchRoot, watch: false })
)
const ws = opened.value
console.log(`  stats=${JSON.stringify(ws.stats)} indexing=${ws.indexing}`)

const firstTree = await timed('workspace.tree（根目录首屏）', () =>
  request('workspace.tree', { workspace_id: ws.workspace_id, path: null, depth: 1, include_ignored: false })
)
const subTree = await timed('workspace.tree（单个子目录）', () =>
  request('workspace.tree', { workspace_id: ws.workspace_id, path: 'd0', depth: 1, include_ignored: false })
)

console.log('\n--- 检索 ---')
const fileSearch = await timed('search.files（模糊查询 f9999）', () =>
  request('search.files', { workspace_id: ws.workspace_id, query: 'f9999', glob: null, limit: null })
)
const contentSearch = await timed('search.content（字面量 bench payload）', () =>
  request('search.content', {
    workspace_id: ws.workspace_id,
    query: 'const value = 1',
    is_regex: false,
    case_sensitive: false,
    includes: [],
    excludes: [],
    max_results: 200
  })
)

console.log('\n--- 大文件读写（1 万行） ---')
const bigLines = 10_000
const bigPath = join(benchRoot, 'big.txt')
const bigContent = Array.from({ length: bigLines }, (_, i) => `line ${i + 1} of the big file`).join('\n') + '\n'
writeFileSync(bigPath, bigContent, 'utf8')
const bigRead = await timed('workspace.read_file（1 万行）', () =>
  request('workspace.read_file', { path: bigPath })
)
const bigWrite = await timed('workspace.write_file（1 万行）', () =>
  request('workspace.write_file', {
    path: bigPath,
    content: bigRead.value.content.replace('line 1 of', 'line one of'),
    expected_hash: bigRead.value.hash,
    eol: null
  })
)
rmSync(bigPath, { force: true })

await request('workspace.close', { workspace_id: ws.workspace_id })
await request('core.shutdown', { graceful: true })
await new Promise((resolveExit) => child.on('exit', resolveExit))

// ---------------------------------------------------------------------------
// 3. 结论
// ---------------------------------------------------------------------------

const firstPaint = opened.took + firstTree.took
console.log('\n================ M1 性能结论 ================')
console.log(`文件规模              : ${ws.stats.files} 个文件 / ${ws.stats.dirs} 个目录`)
console.log(`树首屏（open + tree） : ${fmt(firstPaint)}     门槛 < 3000 ms   ${firstPaint < 3000 ? 'PASS' : 'FAIL'}`)
console.log(`全量索引（open 内）   : ${fmt(opened.took)}     门槛 < 30000 ms  ${opened.took < 30000 ? 'PASS' : 'FAIL'}`)
console.log(`模糊检索              : ${fmt(fileSearch.took)}（命中 ${fileSearch.value.matches.length}）`)
console.log(`内容检索              : ${fmt(contentSearch.took)}（命中 ${contentSearch.value.matches.length}，上限 200）`)
console.log(`读 1 万行             : ${fmt(bigRead.took)}`)
console.log(`写 1 万行             : ${fmt(bigWrite.took)}`)

if (stderrText.trim().length > 0) {
  const lines = stderrText.trim().split('\n')
  console.log(`\n内核 stderr 末 5 行：\n${lines.slice(-5).join('\n')}`)
}

const ok = firstPaint < 3000 && opened.took < 30_000 && ws.stats.files >= FILE_COUNT
console.log(`\n基准结论: ${ok ? 'PASS' : 'FAIL'}`)
process.exit(ok ? 0 : 1)
