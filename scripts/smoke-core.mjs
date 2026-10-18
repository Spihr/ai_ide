/**
 * 内核协议冒烟：绕开 Electron，直接与内核二进制对话。
 *
 * 存在的意义是把「协议本身是否跑得通」和「Electron 侧是否配置正确」
 * 这两件事解耦——任何一端出问题，都能一眼看出是哪一端。
 *
 * 用法：
 *   node scripts/smoke-core.mjs                         # 默认打 target/debug 的内核
 *   node scripts/smoke-core.mjs <内核可执行文件路径>       # 指定二进制（如 release 产物）
 */

import { spawn } from 'node:child_process'
import { createInterface } from 'node:readline'
import { randomUUID } from 'node:crypto'
import { performance } from 'node:perf_hooks'
import { existsSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const defaultExe = join(repoRoot, 'target', 'debug', process.platform === 'win32' ? 'ai-ide-core.exe' : 'ai-ide-core')
const EXE = process.argv[2] ?? defaultExe

if (!existsSync(EXE)) {
  console.error(`找不到内核可执行文件：${EXE}`)
  console.error('请先执行：cargo build -p ai-ide-core')
  process.exit(1)
}

const pending = new Map()
let stderrText = ''
let notifications = 0

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

  notifications += 1
  console.log('[通知]', frame.method, JSON.stringify(frame.params))
})

function request(method, params = {}) {
  const id = randomUUID()
  const line = `${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`
  return new Promise((resolveRequest, rejectRequest) => {
    pending.set(id, { resolve: resolveRequest, reject: rejectRequest })
    child.stdin.write(line)
  })
}

const results = []
function check(name, ok, detail) {
  results.push({ name, ok })
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  ${detail}` : ''}`)
}

const fmt = (value) => value.toFixed(2)

// ---- 1. 握手 ----
const helloStarted = performance.now()
const hello = await request('core.hello', {
  client_version: '0.1.0',
  protocol_version: { major: 1, minor: 0 },
  platform: process.platform,
  locale: 'zh-CN',
  capabilities: [],
})
const helloTook = performance.now() - helloStarted
console.log('\ncore.hello 结果:', JSON.stringify(hello))
console.log(`core.hello 耗时: ${fmt(helloTook)} ms\n`)
check('core.hello 返回结果', hello !== null && typeof hello === 'object')
check(
  'core.hello 协商出协议版本 1.0',
  JSON.stringify(hello.protocol_version) === '{"major":1,"minor":0}',
  JSON.stringify(hello.protocol_version)
)

// ---- 2. ping 往返延迟 ----
const samples = []
for (let i = 0; i < 20; i += 1) {
  const started = performance.now()
  await request('core.ping', {})
  samples.push(performance.now() - started)
}
const sorted = [...samples].sort((a, b) => a - b)
const mean = sorted.reduce((acc, value) => acc + value, 0) / sorted.length
console.log(
  `core.ping x20  min=${fmt(sorted[0])}  mean=${fmt(mean)}  p95=${fmt(sorted[Math.floor(19 * 0.95)])}  max=${fmt(sorted[19])} ms\n`
)
check('core.ping 平均往返 < 5ms', mean < 5, `mean=${fmt(mean)} ms`)

// ---- 3. stats ----
const stats = await request('core.stats', {})
console.log('core.stats 结果:', JSON.stringify(stats), '\n')
check('core.stats 返回结果', stats !== null && typeof stats === 'object')

// ---- 4. 错误路径 ----
let unknownMethodErrored = false
try {
  await request('core.definitely_not_implemented', {})
} catch (error) {
  unknownMethodErrored = true
  console.log(`未知方法正确返回错误: ${error.message}\n`)
}
check('未知方法返回 JSON-RPC 错误', unknownMethodErrored)

// ---- 5. 优雅退出 ----
const shutdown = await request('core.shutdown', { graceful: true })
console.log('core.shutdown 结果:', JSON.stringify(shutdown))
const exit = await Promise.race([
  exitPromise,
  new Promise((resolveTimeout) => setTimeout(() => resolveTimeout({ code: 'timeout', signal: null }), 3_000)),
])
console.log('内核退出:', JSON.stringify(exit), '\n')
check('core.shutdown 后进程自行退出（exit code 0）', exit.code === 0, JSON.stringify(exit))

if (stderrText.trim().length > 0) {
  console.log('--- 内核 stderr ---')
  console.log(stderrText.trim())
}
console.log(`--- 通知条数: ${notifications} ---`)

const failed = results.filter((result) => !result.ok)
console.log(`\n冒烟结论: ${results.length - failed.length}/${results.length} 项通过`)
process.exit(failed.length === 0 ? 0 : 1)
