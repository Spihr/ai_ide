/**
 * M7 真实 MCP Server 验收：拿真官方 Server 当作被连对象，跑完整链路。
 *
 * 与 `crates/core/tests/mcp_flow.rs` 的分工：
 * - 那边连的是本仓库自带的 `mcp-mock-server`（不联网、不装包），保证 CI 稳定；
 * - 这边连的是 npm 上真官方 Server（`@modelcontextprotocol/server-everything`
 *   与 `@modelcontextprotocol/server-filesystem`），验证「握手协商 / 工具发现 /
 *   真调用」在第三方实现上也成立——mock 只能证明我们自己写的两端自洽。
 *
 * 因为要联网装包，本脚本不进 `verify:ci`，单独跑：
 *   pnpm build:core && node scripts/e2e-m7.mjs
 *   node scripts/e2e-m7.mjs --only everything --keep
 *
 * 用法：
 *   node scripts/e2e-m7.mjs [--core <内核路径>] [--only <everything|filesystem>]
 *                           [--timeout <秒>] [--keep] [--verbose]
 */

import { spawn } from 'node:child_process'
import { createInterface } from 'node:readline'
import { randomUUID } from 'node:crypto'
import { existsSync, mkdtempSync, mkdirSync, rmSync, writeFileSync, readdirSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')

// ---------------------------------------------------------------------------
// 参数
// ---------------------------------------------------------------------------

const argv = process.argv.slice(2)
function flag(name) {
  const index = argv.indexOf(`--${name}`)
  if (index === -1) return undefined
  const value = argv[index + 1]
  if (value === undefined || value.startsWith('--')) return true
  return value
}

const EXE = resolve(
  flag('core') ??
    join(repoRoot, 'target', 'debug', process.platform === 'win32' ? 'ai-ide-core.exe' : 'ai-ide-core')
)
const ONLY = flag('only')
const REQUEST_TIMEOUT_MS = Number(flag('timeout') ?? 240) * 1000
const KEEP = flag('keep') === true
const VERBOSE = flag('verbose') === true

if (!existsSync(EXE)) {
  console.error(`找不到内核可执行文件：${EXE}`)
  console.error('请先执行：cargo build -p ai-ide-core')
  process.exit(1)
}

// ---------------------------------------------------------------------------
// 场地：一份临时 mcp.json + 一个只给 filesystem Server 看的目录
// ---------------------------------------------------------------------------

const sandbox = mkdtempSync(join(tmpdir(), 'ai-ide-e2e-m7-'))
const dataDir = join(sandbox, 'data')
const fsRoot = join(sandbox, 'fs-sandbox')
const configPath = join(sandbox, 'mcp.json')
mkdirSync(dataDir, { recursive: true })
mkdirSync(fsRoot, { recursive: true })
writeFileSync(join(fsRoot, 'hello.txt'), 'm7 real server e2e\n', 'utf8')
mkdirSync(join(fsRoot, 'nested'), { recursive: true })
writeFileSync(join(fsRoot, 'nested', 'deep.md'), '# nested\n', 'utf8')

/**
 * Windows 上 `npx` 是 `npx.cmd`，`CreateProcess` 不会自己套 `cmd.exe`，
 * 所以内核里写 `command: "npx"` 是起不来的——这也正是本脚本要顺手验证的一点：
 * 走 `cmd /C` 能不能把 npm 上的 Server 拉起来。
 */
const isWindows = process.platform === 'win32'
function npxServer(packageName, extraArgs = []) {
  const npxArgs = ['--yes', packageName, ...extraArgs]
  return isWindows
    ? { type: 'stdio', command: 'cmd', args: ['/C', 'npx', ...npxArgs] }
    : { type: 'stdio', command: 'npx', args: npxArgs }
}

const servers = {
  everything: npxServer('@modelcontextprotocol/server-everything'),
  filesystem: npxServer('@modelcontextprotocol/server-filesystem', [fsRoot]),
}

const selected = ONLY ? [String(ONLY)] : ['everything', 'filesystem']
for (const name of selected) {
  if (!servers[name]) {
    console.error(`未知的 --only 取值：${name}（可选 everything / filesystem）`)
    process.exit(1)
  }
}

writeFileSync(
  configPath,
  JSON.stringify({ mcpServers: Object.fromEntries(selected.map((name) => [name, servers[name]])) }, null, 2),
  'utf8'
)

console.log(`内核: ${EXE}`)
console.log(`配置: ${configPath}`)
console.log(`连接对象: ${selected.join(', ')}（首次运行需要 npx 下载，可能较慢）\n`)

// ---------------------------------------------------------------------------
// 内核进程
// ---------------------------------------------------------------------------

const pending = new Map()
let stderrText = ''

const child = spawn(EXE, [], {
  stdio: ['pipe', 'pipe', 'pipe'],
  windowsHide: true,
  env: {
    ...process.env,
    AI_IDE_DATA_DIR: dataDir,
    AI_IDE_MCP_CONFIG: configPath,
  },
})

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
    if (VERBOSE) console.log('[非 JSON 报文]', text.slice(0, 200))
    return
  }

  if (frame.id && ('result' in frame || 'error' in frame)) {
    const slot = pending.get(frame.id)
    if (!slot) return
    pending.delete(frame.id)
    clearTimeout(slot.timer)
    if (frame.error) slot.reject(new Error(`[${frame.error.code}] ${frame.error.message}`))
    else slot.resolve(frame.result)
    return
  }

  if (VERBOSE) console.log('[通知]', frame.method, JSON.stringify(frame.params))
})

function request(method, params = {}, timeoutMs = REQUEST_TIMEOUT_MS) {
  const id = randomUUID()
  const line = `${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`
  return new Promise((resolveRequest, rejectRequest) => {
    const timer = setTimeout(() => {
      pending.delete(id)
      rejectRequest(new Error(`超时 ${timeoutMs} ms：${method}`))
    }, timeoutMs)
    pending.set(id, { resolve: resolveRequest, reject: rejectRequest, timer })
    child.stdin.write(line)
  })
}

// ---------------------------------------------------------------------------
// 断言
// ---------------------------------------------------------------------------

const results = []
function check(name, ok, detail) {
  results.push({ name, ok })
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  ${detail}` : ''}`)
}

function toolNames(value) {
  return (value?.tools ?? []).map((tool) => tool.namespaced_name)
}

function findServer(list, name) {
  return (list?.servers ?? []).find((server) => server.name === name)
}

// ---------------------------------------------------------------------------
// 主流程
// ---------------------------------------------------------------------------

let failures = 0
async function main() {
  const hello = await request('core.hello', {
    client_version: '0.1.0',
    protocol_version: { major: 1, minor: 0 },
    platform: process.platform,
    locale: 'zh-CN',
    capabilities: [],
  })
  check('core.hello 握手成功', hello !== null && typeof hello === 'object')

  const capabilities = hello.capabilities ?? []
  const missingCapabilities = ['mcp.servers', 'mcp.connect', 'mcp.disconnect', 'mcp.tools', 'mcp.call'].filter(
    (method) => !capabilities.includes(method)
  )
  check('能力清单含 mcp.* 五方法', missingCapabilities.length === 0, missingCapabilities.join(','))

  const before = await request('mcp.servers', {})
  check(
    `mcp.servers 报出 ${selected.length} 个配置项且均未连接`,
    selected.every((name) => {
      const server = findServer(before, name)
      return server?.configured === true && server?.connected === false && server?.state === 'configured'
    }),
    JSON.stringify((before.servers ?? []).map((server) => [server.name, server.state]))
  )
  check('未连接时 mcp.tools 为空', toolNames(await request('mcp.tools', {})).length === 0)

  for (const name of selected) {
    const startedAt = Date.now()
    const connected = await request('mcp.connect', { server: name })
    const took = ((Date.now() - startedAt) / 1000).toFixed(1)

    check(
      `mcp.connect ${name} 协商出 MCP 协议版本`,
      typeof connected.protocol_version === 'string' && connected.protocol_version.length > 0,
      `protocol=${connected.protocol_version} tool_count=${connected.tool_count} 用时 ${took}s`
    )
    check(
      `mcp.connect ${name} 发现工具且带命名空间前缀`,
      connected.tool_count > 0 && connected.tools.every((tool) => tool.namespaced_name.startsWith(`mcp__${name}__`)),
      toolNames(connected).slice(0, 4).join(' ')
    )
    check(
      `mcp.connect ${name} 的工具都带 description 与 input_schema`,
      connected.tools.every((tool) => typeof tool.description === 'string' && typeof tool.input_schema === 'object')
    )

    const all = await request('mcp.tools', {})
    check(
      `mcp.tools 已包含 ${name} 的工具`,
      toolNames(all).some((toolName) => toolName.startsWith(`mcp__${name}__`))
    )

    if (name === 'everything') {
      const echo = (connected.tools ?? []).find((tool) => tool.name === 'echo')
      check('server-everything 提供 echo 工具', Boolean(echo), toolNames(connected).join(' '))
      if (echo) {
        const schema = echo.input_schema ?? {}
        const key = (schema.required ?? [])[0] ?? Object.keys(schema.properties ?? {})[0] ?? 'message'
        const result = await request('mcp.call', {
          tool: `mcp__everything__${echo.name}`,
          arguments: { [key]: 'e2e-m7 real server' },
        })
        check(
          'mcp.call 真调 server-everything 的 echo 并回显参数',
          result.is_error !== true && String(result.text).includes('e2e-m7 real server'),
          JSON.stringify(result.text).slice(0, 120)
        )
      }
    }

    if (name === 'filesystem') {
      const listing = (connected.tools ?? []).find((tool) => tool.name === 'list_directory')
      check('server-filesystem 提供 list_directory 工具', Boolean(listing), toolNames(connected).join(' '))
      if (listing) {
        const listed = await request('mcp.call', {
          server: 'filesystem',
          tool: 'list_directory',
          arguments: { path: fsRoot },
        })
        const text = String(listed.text)
        check(
          'mcp.call 真列到 sandbox 目录里的文件',
          listed.is_error !== true && text.includes('hello.txt') && text.includes('nested'),
          text.replace(/\s+/g, ' ').slice(0, 160)
        )

        // 读文件：参数路径边界由 Server 自己把关，内核只管转发。
        const readTool = (connected.tools ?? []).find((tool) => tool.name === 'read_text_file')
        if (readTool) {
          const readResult = await request('mcp.call', {
            tool: 'mcp__filesystem__read_text_file',
            arguments: { path: join(fsRoot, 'hello.txt') },
          })
          check(
            'mcp.call 真读到文件正文',
            readResult.is_error !== true && String(readResult.text).includes('m7 real server e2e'),
            JSON.stringify(readResult.text).slice(0, 120)
          )
        }

        const listedEmpty = await request('mcp.call', {
          server: 'filesystem',
          tool: 'list_directory',
          arguments: { path: join(sandbox, 'not-exists') },
        })
        check(
          '工具执行失败按 is_error 回传而非协议错误',
          listedEmpty.is_error === true,
          JSON.stringify(listedEmpty.text).slice(0, 120)
        )
      }
    }
  }

  const after = await request('mcp.servers', {})
  check(
    'mcp.servers 连上后状态与工具数如实',
    selected.every((name) => {
      const server = findServer(after, name)
      return server?.connected === true && server?.state === 'ready' && server?.tool_count > 0
    }),
    JSON.stringify((after.servers ?? []).map((server) => [server.name, server.state, server.tool_count]))
  )

  // 幂等：再连一次不该翻倍
  if (selected.length > 0) {
    const name = selected[0]
    const first = (after.servers ?? []).find((server) => server.name === name)?.tool_count
    const again = await request('mcp.connect', { server: name })
    check(`重复 mcp.connect ${name} 幂等（工具数不翻倍）`, again.tool_count === first, `${first} → ${again.tool_count}`)
  }

  for (const name of selected) {
    await request('mcp.disconnect', { server: name })
  }
  const disconnected = await request('mcp.servers', {})
  check(
    'mcp.disconnect 后全部回到未连接',
    selected.every((name) => findServer(disconnected, name)?.connected === false)
  )
  check('断开后 mcp.tools 清空', toolNames(await request('mcp.tools', {})).length === 0)

  const shutdown = await request('core.shutdown', { graceful: true })
  const exit = await Promise.race([
    exitPromise,
    new Promise((resolveTimeout) => setTimeout(() => resolveTimeout({ code: 'timeout' }), 5000)),
  ])
  check('内核优雅退出（exit code 0）', exit.code === 0, `${JSON.stringify(shutdown)} ${JSON.stringify(exit)}`)
}

try {
  await main()
} catch (error) {
  failures += 1
  console.error(`\n执行中断: ${error.message}`)
} finally {
  if (child.exitCode === null) child.kill()
  if (VERBOSE && stderrText.trim().length > 0) {
    console.log('\n--- 内核 stderr ---\n' + stderrText)
  } else if (failures > 0 && stderrText.trim().length > 0) {
    console.log('\n--- 内核 stderr（尾部）---\n' + stderrText.trim().split('\n').slice(-12).join('\n'))
  }
}

const failed = results.filter((result) => !result.ok)
console.log(`\n合计 ${results.length} 项，通过 ${results.length - failed.length} 项，失败 ${failed.length} 项`)

if (!KEEP) {
  rmSync(sandbox, { recursive: true, force: true })
} else {
  console.log(`现场保留在 ${sandbox}（文件：${readdirSync(sandbox).join(', ')}）`)
}

process.exit(failed.length === 0 && failures === 0 ? 0 : 1)
