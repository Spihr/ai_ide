/**
 * M1 端到端冒烟：绕开 Electron，直接对内核跑「打开工作区 → 文件树 → 读写 → 搜索 → 外部改动」。
 *
 * 与 smoke-core.mjs 分工：
 *   - smoke-core.mjs 只验协议骨架（握手 / ping / 退出）；
 *   - 本脚本验 M1 的功能面：M1 的每条验收标准在这里都有一条 PASS/FAIL。
 *
 * 用法：
 *   node scripts/smoke-m1.mjs                     # 默认打 target/debug 的内核
 *   node scripts/smoke-m1.mjs <内核可执行文件路径>  # 指定二进制（如 release 产物）
 *
 * 工作区固定建在 target/smoke-m1-workspace（每次运行前重建），不碰仓库源码。
 */

import { spawn } from 'node:child_process'
import { createInterface } from 'node:readline'
import { randomUUID } from 'node:crypto'
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const defaultExe = join(
  repoRoot,
  'target',
  'debug',
  process.platform === 'win32' ? 'ai-ide-core.exe' : 'ai-ide-core'
)
const EXE = process.argv[2] ?? defaultExe

if (!existsSync(EXE)) {
  console.error(`找不到内核可执行文件：${EXE}`)
  console.error('请先执行：cargo build -p ai-ide-core')
  process.exit(1)
}

// ---------------------------------------------------------------------------
// 内核连接
// ---------------------------------------------------------------------------

const pending = new Map()
const received = []
let stderrText = ''

const child = spawn(EXE, [], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true })
const rl = createInterface({ input: child.stdout, crlfDelay: Infinity })
child.stderr.setEncoding('utf8')
child.stderr.on('data', (chunk) => {
  stderrText += chunk
})

const exitPromise = new Promise((resolveExit) => {
  child.on('exit', (code, signal) => resolveExit({ code, signal }))
})

rl.on('line', (line) => {
  const text = line.trim()
  if (text.length === 0) return

  let frame
  try {
    frame = JSON.parse(text)
  } catch {
    console.log('[非 JSON 报文]', text.slice(0, 160))
    return
  }

  if (frame.id && ('result' in frame || 'error' in frame)) {
    const slot = pending.get(frame.id)
    if (!slot) {
      console.log('[响应没有对应在途请求]', frame.id)
      return
    }
    pending.delete(frame.id)
    if (frame.error) slot.reject(new Error(`[${frame.error.code}] ${frame.error.message}`))
    else slot.resolve(frame.result)
    return
  }

  received.push({ method: frame.method, params: frame.params })
  console.log('[通知]', frame.method, JSON.stringify(frame.params).slice(0, 200))
})

function request(method, params = {}) {
  const id = randomUUID()
  const line = `${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`
  return new Promise((resolveRequest, rejectRequest) => {
    pending.set(id, { resolve: resolveRequest, reject: rejectRequest })
    child.stdin.write(line)
  })
}

/** 等一条满足条件的通知；超时返回 null。 */
function waitForNotification(method, predicate = () => true, timeoutMs = 5_000) {
  const hit = received.find((item) => item.method === method && predicate(item.params))
  if (hit) return Promise.resolve(hit)
  return new Promise((resolveWait) => {
    const started = Date.now()
    const timer = setInterval(() => {
      const found = received.find((item) => item.method === method && predicate(item.params))
      if (found) {
        clearInterval(timer)
        resolveWait(found)
        return
      }
      if (Date.now() - started > timeoutMs) {
        clearInterval(timer)
        resolveWait(null)
      }
    }, 25)
  })
}

const results = []
function check(name, ok, detail) {
  results.push({ name, ok })
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  ${detail}` : ''}`)
}

const slashed = (value) => String(value).replace(/\\/g, '/')
const sleep = (ms) => new Promise((resolveSleep) => setTimeout(resolveSleep, ms))

// ---------------------------------------------------------------------------
// 准备临时工作区
// ---------------------------------------------------------------------------

const wsRoot = join(repoRoot, 'target', 'smoke-m1-workspace')
rmSync(wsRoot, { recursive: true, force: true })
mkdirSync(join(wsRoot, 'src'), { recursive: true })
const mainPath = join(wsRoot, 'src', 'main.rs')
const crlfPath = join(wsRoot, 'crlf.txt')
writeFileSync(mainPath, 'fn main() {\n    println!("hi");\n}\n', 'utf8')
writeFileSync(join(wsRoot, 'README.md'), '# smoke\n\nhello m1\n', 'utf8')
writeFileSync(crlfPath, 'line1\r\nline2\r\n', 'utf8')
console.log(`临时工作区: ${wsRoot}\n`)

// ---------------------------------------------------------------------------
// 1. 握手 + 打开工作区
// ---------------------------------------------------------------------------

await request('core.hello', {
  client_version: '0.1.0',
  protocol_version: { major: 1, minor: 0 },
  platform: process.platform,
  locale: 'zh-CN',
  capabilities: []
})

const ws = await request('workspace.open', { path: wsRoot, watch: true })
console.log('\nworkspace.open:', JSON.stringify(ws))
check('workspace.open 返回 workspace_id 与规范化根路径', typeof ws.workspace_id === 'string' && typeof ws.root === 'string' && ws.root.length > 0)
check('workspace.open 的根路径指向目标目录', slashed(ws.root).toLowerCase() === slashed(wsRoot).toLowerCase(), slashed(ws.root))
check('workspace.open 返回规模统计', typeof ws.stats?.files === 'number' && ws.stats.files >= 3, JSON.stringify(ws.stats))

// M1 的索引随 open 同步建完（返回体 indexing 恒为 false 且已带 stats），
// 因此这里没有「后台索引完成」这一刻需要通知；workspace.indexed 留给 M2 的后台/增量索引。
check('workspace.open 的 indexing 标记与同步索引策略一致（M1 不发 workspace.indexed）', ws.indexing === false)

// ---------------------------------------------------------------------------
// 2. 文件树
// ---------------------------------------------------------------------------

const rootTree = await request('workspace.tree', {
  workspace_id: ws.workspace_id,
  path: null,
  depth: 1,
  include_ignored: false
})
const rootNames = rootTree.entries.map((item) => item.name)
check(
  'workspace.tree 列出根目录条目',
  rootNames.includes('src') && rootNames.includes('README.md'),
  rootNames.join(', ')
)
check(
  '目录排在文件之前',
  rootTree.entries.findIndex((item) => item.kind === 'dir') <
    rootTree.entries.findIndex((item) => item.kind === 'file')
)

const srcTree = await request('workspace.tree', {
  workspace_id: ws.workspace_id,
  path: 'src',
  depth: 1,
  include_ignored: false
})
check(
  'workspace.tree 支持按相对路径下钻',
  srcTree.entries.some((item) => item.name === 'main.rs'),
  srcTree.entries.map((item) => item.name).join(', ')
)

// ---------------------------------------------------------------------------
// 3. 读文件 / 写文件（含乐观锁与 CRLF）
// ---------------------------------------------------------------------------

const read = await request('workspace.read_file', { path: mainPath })
check(
  'workspace.read_file 返回内容、编码、EOL 与哈希',
  read.content.includes('fn main') && read.encoding === 'utf-8' && read.eol === 'lf' && /^[0-9a-f]{64}$/.test(read.hash),
  `eol=${read.eol} hash=${String(read.hash).slice(0, 8)}…`
)
check('workspace.read_file 不截断小文件', read.truncated === false)

let staleRejected = false
try {
  await request('workspace.write_file', {
    path: mainPath,
    content: 'tampered\n',
    expected_hash: '0'.repeat(64),
    eol: null
  })
} catch (error) {
  staleRejected = true
  console.log('过期哈希被拒:', error.message)
}
check('workspace.write_file 用过期 expected_hash 会拒绝写入', staleRejected)
check('拒绝后磁盘内容未被改动', readFileSync(mainPath, 'utf8').includes('fn main'))

const written = await request('workspace.write_file', {
  path: mainPath,
  content: 'fn main() {}\n',
  expected_hash: read.hash,
  eol: null
})
check('workspace.write_file 成功并返回新哈希', written.ok === true && written.hash !== read.hash)
check('写回内容已落盘', readFileSync(mainPath, 'utf8') === 'fn main() {}\n')

const crlfRead = await request('workspace.read_file', { path: crlfPath })
check('read_file 识别 CRLF 并把内存换行统一为 LF', crlfRead.eol === 'crlf' && crlfRead.content === 'line1\nline2\n')

await request('workspace.write_file', {
  path: crlfPath,
  content: 'a\nb\n',
  expected_hash: crlfRead.hash,
  eol: 'crlf'
})
check('写回时按 CRLF 还原换行', readFileSync(crlfPath, 'utf8') === 'a\r\nb\r\n', JSON.stringify(readFileSync(crlfPath, 'utf8').slice(0, 20)))

// ---------------------------------------------------------------------------
// 4. 搜索
// ---------------------------------------------------------------------------

const fileMatches = await request('search.files', {
  workspace_id: ws.workspace_id,
  query: 'main',
  glob: null,
  limit: null
})
check(
  'search.files 命中 src/main.rs',
  fileMatches.matches.some((item) => slashed(item.path).endsWith('src/main.rs')),
  JSON.stringify(fileMatches.matches)
)

const contentMatches = await request('search.content', {
  workspace_id: ws.workspace_id,
  query: 'fn main',
  is_regex: false,
  case_sensitive: false,
  includes: [],
  excludes: [],
  max_results: null
})
check(
  'search.content 命中并返回行列',
  contentMatches.matches.length > 0 && contentMatches.matches[0].line >= 1 && contentMatches.matches[0].column >= 1,
  JSON.stringify(contentMatches.matches[0] ?? null)
)

const regexMatches = await request('search.content', {
  workspace_id: ws.workspace_id,
  query: 'fn\\s+main',
  is_regex: true,
  case_sensitive: false,
  includes: [],
  excludes: [],
  max_results: null
})
check('search.content 支持正则', regexMatches.matches.length > 0)

// ---------------------------------------------------------------------------
// 5. 外部改动事件（文件树与标签自动跟随）
// ---------------------------------------------------------------------------

const createdEvent = waitForNotification(
  'workspace.changed',
  (params) => Array.isArray(params?.changes) && params.changes.some((item) => slashed(item.path).endsWith('new-file.txt')),
  5_000
)
await sleep(150)
writeFileSync(join(wsRoot, 'new-file.txt'), 'created by smoke\n', 'utf8')
const created = await createdEvent
check('外部新建文件触发 workspace.changed', created !== null, created ? JSON.stringify(created.params).slice(0, 160) : '超时未收到')

await sleep(150)
const deletedEvent = waitForNotification(
  'workspace.changed',
  (params) => Array.isArray(params?.changes) && params.changes.some((item) => slashed(item.path).endsWith('new-file.txt') && item.kind === 'deleted'),
  5_000
)
rmSync(join(wsRoot, 'new-file.txt'), { force: true })
const deleted = await deletedEvent
check('外部删除文件触发 deleted 变更', deleted !== null, deleted ? JSON.stringify(deleted.params).slice(0, 160) : '超时未收到')

// ---------------------------------------------------------------------------
// 6. 关闭工作区与退出
// ---------------------------------------------------------------------------

const closed = await request('workspace.close', { workspace_id: ws.workspace_id })
check('workspace.close 返回 ok', closed.ok === true)

let unknownWorkspaceRejected = false
try {
  await request('workspace.tree', { workspace_id: ws.workspace_id, path: null, depth: 1, include_ignored: false })
} catch (error) {
  unknownWorkspaceRejected = true
  console.log('关闭后再用同一 workspace_id:', error.message)
}
check('关闭后的 workspace_id 不再可用', unknownWorkspaceRejected)

await request('core.shutdown', { graceful: true })
const exit = await Promise.race([
  exitPromise,
  new Promise((resolveTimeout) => setTimeout(() => resolveTimeout({ code: 'timeout', signal: null }), 3_000))
])
check('core.shutdown 后进程自行退出', exit.code === 0, JSON.stringify(exit))

if (stderrText.trim().length > 0) {
  console.log('--- 内核 stderr ---')
  console.log(stderrText.trim())
}

rmSync(wsRoot, { recursive: true, force: true })

const failed = results.filter((result) => !result.ok)
console.log(`\n冒烟结论: ${results.length - failed.length}/${results.length} 项通过`)
process.exit(failed.length === 0 ? 0 : 1)
