/**
 * demo-hello 插件入口。
 *
 * 只依赖 node 内置模块，因为「插件市场里 90% 的包不应该被逼着装一堆依赖」。
 *
 * 铁律：stdout 只写协议帧，任何日志走 stderr。
 * 一旦往 stdout 打了一行 println，宿主那侧的帧解析立刻错位——
 * 这类 bug 在真实插件里非常常见，所以 demo 里把 log() 做成唯一出口。
 */

import readline from 'node:readline'

const PLUGIN_API = { major: 1, minor: 0 }
const pending = new Map()
let nextId = 1
let context = null

function log(level, message) {
  // stderr 的每一行会被宿主转成 plugin.log 事件
  process.stderr.write(`[demo-hello/${level}] ${message}\n`)
}

function send(frame) {
  process.stdout.write(`${JSON.stringify(frame)}\n`)
}

/** 向宿主发起能力调用。这是插件唯一的副作用入口。 */
function callHost(method, params) {
  const id = `p-${nextId++}`
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      pending.delete(id)
      const err = new Error(`${method} 超时`)
      err.code = 6005
      reject(err)
    }, 10000)
    if (typeof timer.unref === 'function') timer.unref()
    pending.set(id, { resolve, reject, timer })
    send({ jsonrpc: '2.0', id, method, params })
  })
}

/**
 * 上报长任务进度：`host.progress({ token, message, percent? })`。
 *
 * 通知帧（无 `id`），发出即忘：进度不该占住调用链等回包。`token` 把同一次
 * 长任务的多次上报串起来；`percent` 省略时不带这个键，宿主也不会替我们编一个。
 * 需要清单里申请 `progress` 能力，否则宿主按未授权丢掉这一帧（不连坐插件）。
 */
function progress(token, message, percent) {
  const params = { token, message }
  if (typeof percent === 'number' && Number.isFinite(percent)) params.percent = percent
  send({ jsonrpc: '2.0', method: 'host/progress', params })
}

// ── 工具实现 ──────────────────────────────────────────────────────────────

async function repoStats() {
  const info = await callHost('host/workspace/info')
  const extCount = new Map()
  let files = 0
  let dirs = 0
  let bytes = 0

  const walk = async (dir, depth) => {
    if (depth > 4) return
    const { entries } = await callHost('host/fs.list', { path: dir })
    // 每进一层报一次进度：遍历是这次调用里最慢的一段，用户该看到它在动。
    progress('repo-stats', `扫描 ${dir}`, Math.min(90, depth * 25))
    for (const entry of entries) {
      // 点开头目录自己先跳过：即便不跳，PathGuard 也会拦（escape_probe 证明了这点）
      if (entry.kind === 'dir') {
        if (entry.name.startsWith('.')) continue
        dirs += 1
        await walk(entry.path, depth + 1)
      } else {
        files += 1
        bytes += entry.size
        const ext = entry.name.includes('.') ? entry.name.split('.').pop().toLowerCase() : '(noext)'
        extCount.set(ext, (extCount.get(ext) ?? 0) + 1)
      }
    }
  }
  await walk('.', 0)
  progress('repo-stats', '统计完成', 100)

  const top = [...extCount.entries()].sort((a, b) => b[1] - a[1]).slice(0, 5)
  const lines = [
    `工作区：${info.name}（${info.id}）`,
    `文件 ${files} 个 / 目录 ${dirs} 个 / 合计 ${(bytes / 1024).toFixed(1)} KiB`,
    '扩展名分布：',
    ...top.map(([ext, n]) => `  ${ext.padEnd(10, ' ')} ${n}`)
  ]
  return { content: lines.join('\n'), data: { files, dirs, bytes, extensions: Object.fromEntries(extCount) } }
}

async function writeNote(args) {
  const notePath = String(args.path ?? '')
  if (notePath === '') {
    const err = new Error('path 必填')
    err.code = -32602
    throw err
  }
  const title = String(args.title ?? '未命名笔记')
  const body = String(args.body ?? '')
  const markdown = `# ${title}\n\n> 由插件 demo-hello 写入 · ${new Date().toISOString()}\n\n${body}\n`
  const result = await callHost('host/fs.write', { path: notePath, content: markdown })
  log('info', `写入 ${result.path}（${result.bytes} 字节）`)
  return { content: `已写入 ${result.path}（${result.bytes} 字节）`, data: result }
}

async function escapeProbe() {
  const attempts = [
    { label: '../outside.txt（越界）', path: '../outside.txt' },
    { label: '.git/config（敏感路径）', path: '.git/config' },
    { label: '/etc/passwd（绝对路径）', path: '/etc/passwd' }
  ]
  const lines = []
  for (const attempt of attempts) {
    try {
      const r = await callHost('host/fs.read', { path: attempt.path })
      lines.push(`✗ ${attempt.label} 未被拦截，读到 ${r.size} 字节 —— 闸门失效`)
    } catch (err) {
      lines.push(`✓ ${attempt.label} 被拦：${err.code} ${err.message}`)
    }
  }
  return { content: lines.join('\n') }
}

async function netProbe() {
  try {
    await callHost('host/net.fetch', { url: 'https://example.com' })
    return { content: '✗ net.fetch 未被拦截 —— 能力闸门失效' }
  } catch (err) {
    return { content: `✓ net.fetch 被拦：${err.code} ${err.message}` }
  }
}

async function spamProbe() {
  // 宿主只给单层 fs.list，遍历策略归插件——所以「往返次数」必须有人管。
  // 这个探针就是撞配额上限的那面墙。
  let ok = 0
  for (let i = 0; i < 300; i += 1) {
    try {
      await callHost('host/fs.list', { path: '.' })
      ok += 1
    } catch (err) {
      return { content: `第 ${ok + 1} 次宿主调用被拒：${err.code} ${err.message}` }
    }
  }
  return { content: '300 次全部通过 —— 配额未生效' }
}

function boom() {
  // 真实插件里这可能是 OOM 或未捕获异常。宿主只需要看到「进程没了」。
  log('error', '准备主动退出，模拟崩溃')
  setTimeout(() => process.exit(3), 20)
  return { content: 'boom 已触发，进程即将退出' }
}

const TOOLS = {
  repo_stats: repoStats,
  write_note: writeNote,
  escape_probe: escapeProbe,
  net_probe: netProbe,
  spam_probe: spamProbe,
  boom
}

const COMMANDS = {
  greet: async () => {
    const info = await callHost('host/workspace/info')
    return { content: `你好，来自 demo-hello @ ${info.name}` }
  }
}

// ── RPC 主循环 ────────────────────────────────────────────────────────────

async function handleRequest(method, params) {
  switch (method) {
    case 'plugin/initialize': {
      context = params
      log('info', `initialize，宿主 ${context.host?.product} ，工作区 ${context.workspace?.name}`)
      // 刻意不回传 rootPath：插件不需要知道绝对路径。
      return {
        pluginApi: PLUGIN_API,
        name: 'demo-hello',
        version: '1.0.0',
        tools: Object.keys(TOOLS).map((name) => ({ name })),
        commands: [{ id: 'greet', title: 'Demo Hello: 打个招呼' }],
        subscriptions: ['workspace.changed']
      }
    }

    case 'tool/invoke': {
      const fn = TOOLS[params?.tool]
      if (!fn) {
        const err = new Error(`未知工具：${params?.tool}`)
        err.code = -32601
        throw err
      }
      return await fn(params.arguments ?? {})
    }

    case 'command/invoke': {
      const fn = COMMANDS[params?.command]
      if (!fn) {
        const err = new Error(`未知命令：${params?.command}`)
        err.code = -32601
        throw err
      }
      return await fn(params.arguments ?? {})
    }

    case 'plugin/ping':
      return { pong: true, at: Date.now() }

    case 'plugin/shutdown':
      log('info', 'shutdown，准备退出')
      setTimeout(() => process.exit(0), 10)
      return { ok: true }

    default: {
      const err = new Error(`未实现的方法：${method}`)
      err.code = -32601
      throw err
    }
  }
}

const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })

rl.on('line', (line) => {
  if (line.trim() === '') return
  let frame
  try {
    frame = JSON.parse(line)
  } catch (err) {
    log('error', `收到非法帧：${err.message}`)
    return
  }

  // 响应帧
  if (frame.method === undefined && frame.id !== undefined) {
    const entry = pending.get(frame.id)
    if (!entry) return
    pending.delete(frame.id)
    clearTimeout(entry.timer)
    if (frame.error) {
      const err = new Error(frame.error.message)
      err.code = frame.error.code
      err.data = frame.error.data
      entry.reject(err)
    } else {
      entry.resolve(frame.result)
    }
    return
  }

  // 通知帧（无 id）
  if (frame.id === undefined) {
    if (frame.method === 'event/notify') {
      log('info', `收到事件 ${frame.params?.event}：${JSON.stringify(frame.params?.payload ?? {})}`)
    }
    return
  }

  // 请求帧
  Promise.resolve()
    .then(() => handleRequest(frame.method, frame.params))
    .then((result) => send({ jsonrpc: '2.0', id: frame.id, result: result ?? null }))
    .catch((err) => {
      send({
        jsonrpc: '2.0',
        id: frame.id,
        error: { code: err.code ?? -32603, message: err.message, data: err.data }
      })
    })
})

process.on('unhandledRejection', (err) => {
  log('error', `未处理的 Promise 拒绝：${err?.message ?? err}`)
})

log('info', '插件进程已启动，等待 initialize')
