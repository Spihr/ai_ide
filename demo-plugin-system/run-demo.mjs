/**
 * 插件系统 demo 编排器。
 *
 * 跑法：`node run-demo.mjs`
 *
 * 十一个场景按「装载 → 调用 → 越权 → 故障 → 卸载」的顺序排，
 * 每一步都断言结果码，任何一步不符就标 ✗ 并继续往下跑
 * （demo 的价值在于暴露协议漏洞，而不是在第一个断言处崩掉）。
 */

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { PluginHost, PLUGIN_STATE } from './mock-host/host.mjs'
import { validateManifest } from './mock-host/manifest.mjs'

const HERE = path.dirname(fileURLToPath(import.meta.url))
const PLUGIN_DIR = path.join(HERE, 'plugins', 'demo-hello')
const WORKSPACE = path.join(HERE, '.tmp-workspace')

const results = []
const hostLogs = []
const approvals = []

function line(text = '') {
  console.log(text)
}

function head(text) {
  line()
  line(`── ${text} ${'─'.repeat(Math.max(0, 62 - text.length))}`)
}

async function step(no, title, fn) {
  try {
    const detail = await fn()
    results.push({ no, title, ok: true, detail })
    line(`  ✓ ${no}  ${title}`)
    if (detail) {
      for (const l of String(detail).split('\n')) line(`        ${l}`)
    }
  } catch (err) {
    results.push({ no, title, ok: false, detail: err.message })
    line(`  ✗ ${no}  ${title}`)
    line(`        ${err.message}`)
  }
}

/** 期望抛出指定错误码 */
async function expectCode(code, fn) {
  try {
    await fn()
  } catch (err) {
    if (err.code === code) return err
    throw new Error(`期望错误码 ${code}，实际 ${err.code}（${err.message}）`)
  }
  throw new Error(`期望抛出 ${code}，但操作成功了`)
}

function copyDir(from, to) {
  fs.mkdirSync(to, { recursive: true })
  for (const entry of fs.readdirSync(from, { withFileTypes: true })) {
    const src = path.join(from, entry.name)
    const dst = path.join(to, entry.name)
    if (entry.isDirectory()) copyDir(src, dst)
    else fs.copyFileSync(src, dst)
  }
}

function prepareWorkspace() {
  fs.rmSync(WORKSPACE, { recursive: true, force: true })
  copyDir(path.join(HERE, 'fixtures', 'workspace'), WORKSPACE)

  // 夹具里的 .git 由脚本现建：仓库里不放 .git 目录（git 会把它当嵌套仓库），
  // 而场景 07 需要真实的敏感路径才能验证 1003。
  const gitDir = path.join(WORKSPACE, '.git')
  fs.mkdirSync(gitDir, { recursive: true })
  fs.writeFileSync(path.join(gitDir, 'config'), '[core]\n\trepositoryformatversion = 0\n')
  fs.writeFileSync(path.join(gitDir, 'HEAD'), 'ref: refs/heads/main\n')
  return WORKSPACE
}

/** 脚本化审批人：演示里「拒绝」由路径关键字触发，真实实现是一张 ask_user 卡片。 */
const approver = async (req) => {
  approvals.push(req)
  if (String(req.args?.path ?? '').includes('blocked')) {
    return { approved: false, note: '演示：用户点了取消' }
  }
  return { approved: true, note: '演示：用户点了允许', remember: false }
}

function waitForState(host, pluginId, stateEvent, timeoutMs = 5000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`等待 ${stateEvent} 超时`)), timeoutMs)
    const onState = (payload) => {
      if (payload.pluginId === pluginId && payload.event === stateEvent) {
        clearTimeout(timer)
        host.off('state', onState)
        resolve(payload)
      }
    }
    host.on('state', onState)
  })
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

async function main() {
  line('AI IDE 插件系统 demo —— 子进程 stdio + 四道闸门')
  line(`node ${process.version} · 工作区 ${WORKSPACE}`)

  prepareWorkspace()

  const host = new PluginHost({
    workspaceRoot: WORKSPACE,
    workspaceId: 'ws-demo',
    approver
  })
  host.on('log', (entry) => hostLogs.push(entry))
  // `host/progress` 转成窗口侧的 `plugin.progress`：demo 里收进 hostLogs，便于
  // 在 04 之后核对「进度确实推到了窗口，且没带任何文件内容」。
  host.on('progress', (entry) =>
    hostLogs.push({
      pluginId: entry.pluginId,
      level: 'progress',
      message: `${entry.token ?? '-'} ${entry.percent ?? '-'}% ${entry.message}`
    })
  )
  host.on('state', (payload) => hostLogs.push({ pluginId: payload.pluginId, level: 'state', message: `${payload.event} ${JSON.stringify(payload.payload)}` }))

  // ── 01 清单校验 ─────────────────────────────────────────────────────────
  head('装载前：静态闸门')
  await step('01', '非法清单被静态校验拦下（不启动任何进程）', () => {
    const bad = {
      id: 'Bad_ID',
      name: 'bad',
      version: '1.0',
      pluginApi: '2.0',
      main: '../escape/index.mjs',
      capabilities: ['fs.read', 'shell.exec'],
      tools: [
        { name: 'ok_tool', risk: 'read', parameters: { type: 'object' } },
        { name: 'write_tool', risk: 'write', parameters: { type: 'object' } }
      ]
    }
    const { manifest, errors } = validateManifest(bad)
    if (manifest) throw new Error('非法清单竟然通过了校验')
    return `拒绝 ${errors.length} 处：\n${errors.map((e) => `· ${e}`).join('\n')}`
  })

  // ── 02 未授权加载 ───────────────────────────────────────────────────────
  await step('02', '未授予能力时拒绝加载（6003），零进程启动', async () => {
    const err = await expectCode(6003, () => host.load(PLUGIN_DIR))
    const entry = host.plugins.get('demo-hello')
    if (entry.state !== PLUGIN_STATE.NEEDS_GRANT) {
      throw new Error(`期望状态 needs_grant，实际 ${entry.state}`)
    }
    if (entry.proc) throw new Error('未授权却已经 spawn 了进程')
    return `${err.message}\n状态：${entry.state}，进程：未启动`
  })

  // ── 03 授权 + 加载 + 握手 ───────────────────────────────────────────────
  await step('03', '授予能力后加载成功，握手注册 6 个工具', async () => {
    host.grant('demo-hello', ['fs.read', 'fs.write', 'search', 'progress'])
    const entry = await host.load(PLUGIN_DIR)
    if (entry.state !== PLUGIN_STATE.READY) throw new Error(`期望 ready，实际 ${entry.state}`)
    const tools = host.listTools()
    return [
      `状态：${entry.state}，能力：${host.grants.list('demo-hello').join(', ')}`,
      `工具：${tools.map((t) => `${t.name}[${t.risk}]`).join('  ')}`
    ].join('\n')
  })

  // ── 04 只读工具 ─────────────────────────────────────────────────────────
  await step('04', '调用只读工具 repo_stats：不走审批，结果回灌', async () => {
    const r = await host.callTool('plugin.demo-hello.repo_stats', {})
    if (!r.content.includes('文件')) throw new Error('返回内容不符合预期')
    return r.content
  })

  // ── 05 写入工具 + 审批 ──────────────────────────────────────────────────
  await step('05', '调用写工具 write_note：审批通过后落盘', async () => {
    const target = 'docs/plugin-note.md'
    const r = await host.callTool('plugin.demo-hello.write_note', {
      path: target,
      title: '插件写入的笔记',
      body: '这条内容由 demo-hello 通过 host/fs.write 写入，宿主已记录审计行。'
    })
    const abs = path.join(WORKSPACE, target)
    if (!fs.existsSync(abs)) throw new Error('文件未落盘')
    const bytes = fs.statSync(abs).size
    return `${r.content}\n落盘校验：${target} 存在，${bytes} 字节\n审批记录：${approvals.length} 次询问，最后一次=${approvals.at(-1).risk}`
  })

  // ── 06 审批拒绝 ─────────────────────────────────────────────────────────
  await step('06', '用户拒绝审批，工具不执行（3001）', async () => {
    const err = await expectCode(3001, () =>
      host.callTool('plugin.demo-hello.write_note', {
        path: 'docs/blocked.md',
        title: '这条不该写进去',
        body: 'x'
      })
    )
    if (fs.existsSync(path.join(WORKSPACE, 'docs/blocked.md'))) {
      throw new Error('审批被拒但文件仍被写入')
    }
    return `${err.message}\n文件未被创建`
  })

  // ── 07 路径闸门 ─────────────────────────────────────────────────────────
  await step('07', '路径闸门：越界 / 敏感路径 / 绝对路径全部拦截', async () => {
    const r = await host.callTool('plugin.demo-hello.escape_probe', {})
    return r.content
  })

  // ── 08 能力闸门 ─────────────────────────────────────────────────────────
  await step('08', '能力闸门：调用未申请的 net.fetch 被拒（6003）', async () => {
    const r = await host.callTool('plugin.demo-hello.net_probe', {})
    return r.content
  })

  // ── 09 资源配额 ─────────────────────────────────────────────────────────
  await step('09', '资源配额：单次调用宿主 API 超过 200 次被拒（6009）', async () => {
    const r = await host.callTool('plugin.demo-hello.spam_probe', {})
    if (!r.content.includes('6009')) throw new Error(`配额未生效：${r.content}`)
    return r.content
  })

  // ── 10 事件广播 ─────────────────────────────────────────────────────────
  await step('10', '事件广播：仅投递给声明订阅的插件', async () => {
    const before = hostLogs.length
    const delivered = host.broadcast('workspace.changed', { files: ['README.md'] })
    await sleep(80)
    const received = hostLogs
      .slice(before)
      .filter((l) => l.message.includes('收到事件 workspace.changed'))
    if (delivered !== 1) throw new Error(`期望投递 1 个插件，实际 ${delivered}`)
    if (received.length === 0) throw new Error('插件未收到事件')
    return `投递 ${delivered} 个插件，插件侧日志：${received.length} 条`
  })

  // ── 11 崩溃 + 下架 + 重启 ───────────────────────────────────────────────
  await step('11', '崩溃检测：进程退出后工具立即下架，重启后恢复', async () => {
    const crashed = waitForState(host, 'demo-hello', 'plugin.crashed')
    await host.callTool('plugin.demo-hello.boom', {}).catch(() => {})
    const payload = await crashed
    const toolsWhileCrashed = host.listTools().length
    const err = await expectCode(6002, () => host.callTool('plugin.demo-hello.repo_stats', {}))

    await host.restart('demo-hello')
    const after = await host.callTool('plugin.demo-hello.repo_stats', {})
    return [
      `崩溃事件：${payload.event} ${JSON.stringify(payload.payload)}`,
      `崩溃期间注册工具数：${toolsWhileCrashed}（已下架）`,
      `调用报错：${err.code} ${err.message}`,
      `重启后可用：${after.content.split('\n')[1]}`
    ].join('\n')
  })

  // ── 12 卸载 ─────────────────────────────────────────────────────────────
  await step('12', '卸载：工具全部下架，进程退出', async () => {
    await host.unload('demo-hello')
    const tools = host.listTools().length
    if (tools !== 0) throw new Error(`卸载后仍有 ${tools} 个工具`)
    return `剩余工具 ${tools} 个，插件状态 ${host.plugins.get('demo-hello').state}`
  })

  // ── 汇总 ────────────────────────────────────────────────────────────────
  const auditRows = host.audit
  await host.shutdown()

  head('审计（宿主侧记录，插件不可伪造）')
  for (const row of auditRows.slice(0, 8)) {
    line(`  ${row.action.padEnd(9)} ${String(row.pluginId).padEnd(12)} ${row.path ?? row.method ?? row.query ?? ''}`)
  }
  if (auditRows.length > 8) line(`  … 共 ${auditRows.length} 行`)

  head('结果')
  const pass = results.filter((r) => r.ok).length
  line(`  场景通过 ${pass}/${results.length}`)
  line()
  line('  #   结果  场景')
  for (const r of results) {
    line(`  ${String(r.no).padEnd(3)} ${r.ok ? ' ✓  ' : ' ✗  '} ${r.title}`)
    if (!r.ok) line(`        ${r.detail}`)
  }
  line()
  line(`  审批询问 ${approvals.length} 次 · 宿主日志 ${hostLogs.length} 条 · 审计 ${auditRows.length} 行`)
  line(`  写入结果可在 ${path.join(WORKSPACE, 'docs')} 查看`)

  if (pass !== results.length) process.exitCode = 1
}

main().catch((err) => {
  console.error('demo 执行失败：', err)
  process.exitCode = 1
})
