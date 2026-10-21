/**
 * M4 真实模型端到端联调（补 `docs/10-M4-验收报告.md` 里挂着的两条硬指标缺口）。
 *
 * 与 `scripts/e2e-m1.mjs` 的分工：
 *   - e2e-m1.mjs 看**真界面**（Electron + 真实点击）；
 *   - 本脚本看**真模型**：拉起真实内核二进制（`target/debug/ai-ide-core.exe`），
 *     把 provider 指向真实厂商网关，让真模型跑 `docs/05` §M4 的四条基线任务，
 *     并把「`edit_file` 非唯一匹配自动重试成功率」跑够 20 个用例量化出来。
 *
 * ## 非唯一匹配成功率是怎么算的
 *
 * 20 个用例共用一份「同一个键在文件里重复 2~4 次、只许改其中一段」的配置，并且**要求模型
 * 第一次 `edit` 只用歧义锚点**（整行 `timeout: 1000`，不带上下文行）。不这么要求的话，
 * 模型会先 `grep` 再带上下文读文件，第一枪就把锚点凑唯一了——指标看着 100%，非唯一通道
 * 一次都没走到，等于没测。于是分两层记：
 *
 *   - **成功率**（硬指标，≥ 90%）：20 个用例里「目标段改对、其余段一字未动」的比例；
 *   - **通道命中**：真撞上非唯一（工具回可回灌的 `3003`「无法唯一匹配」）的用例数，
 *     以及这些用例里补足上下文后重试成功的数量——这才是该指标要量的那条恢复通道。
 *
 * ## 观测手段：本地透明录制代理
 *
 * 内核只回 `agent.approval_required` / `agent.finished` 两类事件，**没有**逐次工具调用
 * 的事件，光看协议面看不出「模型第几轮调了什么工具、工具回了什么错」。所以脚本在
 * 本地起一个 HTTP 代理，把 provider 的 `base_url` 指到 `http://127.0.0.1:<port>/v1`：
 * 请求原样转发给真实网关（不改一个字节），同时把「发给模型的 messages」与
 * 「模型返的 SSE」留档。工具调用时间线直接从最后一条请求的 messages 里还原——
 * 那是内核自己拼的完整对话，比解析 SSE 增量更可靠。
 *
 * 代理只记 URL、请求体里的对话、响应摘要；**不记任何请求头**，因此 api_key 不会被落盘。
 * 若想完全绕开代理，加 `--direct`：base_url 直连真实网关（代价是没有工具调用明细）。
 *
 * ## 凭据安全
 *
 * 凭据只从**本地文件**读（`base_url` + `api_key` + 可选 `name` / `protocol` /
 * `model` / `timeout_ms`），绝不打印 api_key（只回显长度与前 4 位做校验），也绝不写进
 * 被版本控制的文件。默认读仓库根下的 `.e2e-m4-credentials.json`：该路径已写进
 * `.gitignore`，允许存放明文 key 而不入库（字段说明见该文件的 `_comment` / `_fields`）。
 * 要换凭据就用 `--creds <path>` 或环境变量 `AI_IDE_E2E_CREDS` 覆盖；覆盖目标若不在
 * `.gitignore` 里，请自行确认它不会被提交。
 *
 * ## 用法
 *
 *   node scripts/e2e-m4.mjs                    # 默认读仓库根 .e2e-m4-credentials.json
 *   node scripts/e2e-m4.mjs --check-creds      # 只校验凭据文件可读与字段解析，不拉内核不联网
 *   node scripts/e2e-m4.mjs --creds <path> --cases 20 --parallel 3 --model deepseek-v4-pro
 *   node scripts/e2e-m4.mjs --tasks 1,2 --cases 0        # 只跑基线
 *
 * 退出码：四条基线任务全过、且非唯一匹配成功率 ≥ 90% 时为 0，否则 1。
 */

import { execFileSync, spawn } from 'node:child_process'
import { createHash, randomUUID } from 'node:crypto'
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync, writeSync } from 'node:fs'
import http from 'node:http'
import { createInterface } from 'node:readline'
import { dirname, join, resolve } from 'node:path'
import { performance } from 'node:perf_hooks'
import { fileURLToPath } from 'node:url'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const defaultCore = join(
  repoRoot,
  'target',
  'debug',
  process.platform === 'win32' ? 'ai-ide-core.exe' : 'ai-ide-core'
)
const defaultSandbox = join(repoRoot, 'target', 'e2e-m4-workspace')
/** 默认凭据文件：仓库根下的本机凭据（已在 `.gitignore` 里忽略，可放明文 key）。 */
const defaultCreds = join(repoRoot, '.e2e-m4-credentials.json')

// ---------------------------------------------------------------------------
// 0. 参数与凭据
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const opts = {
    core: defaultCore,
    sandbox: defaultSandbox,
    creds: process.env.AI_IDE_E2E_CREDS ?? '',
    model: process.env.AI_IDE_E2E_MODEL ?? '',
    cases: 20,
    parallel: 3,
    tasks: '1,2,3,4',
    runTimeoutMs: 240_000,
    direct: false,
    keep: false,
    checkCreds: false,
    report: ''
  }
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i]
    const next = () => {
      const value = argv[i + 1]
      if (value === undefined) throw new Error(`${flag} 缺参数`)
      i += 1
      return value
    }
    if (flag === '--core') opts.core = resolve(next())
    else if (flag === '--sandbox') opts.sandbox = resolve(next())
    else if (flag === '--creds') opts.creds = next()
    else if (flag === '--model') opts.model = next()
    else if (flag === '--cases') opts.cases = Number(next())
    else if (flag === '--parallel') opts.parallel = Math.max(1, Number(next()))
    else if (flag === '--tasks') opts.tasks = next()
    else if (flag === '--run-timeout') opts.runTimeoutMs = Number(next())
    else if (flag === '--report') opts.report = resolve(next())
    else if (flag === '--direct') opts.direct = true
    else if (flag === '--keep') opts.keep = true
    else if (flag === '--check-creds') opts.checkCreds = true
    else throw new Error(`未知参数：${flag}`)
  }
  opts.report ||= join(opts.sandbox, 'e2e-m4-report.json')
  // 没显式给凭据就落到仓库根的默认凭据文件（`--creds` / `AI_IDE_E2E_CREDS` 优先）。
  opts.credsExplicit = opts.creds.length > 0
  opts.creds = opts.credsExplicit ? resolve(opts.creds) : defaultCreds
  return opts
}

const opts = parseArgs(process.argv.slice(2))
const credsSource = opts.credsExplicit ? '--creds / AI_IDE_E2E_CREDS' : '默认路径'

/**
 * 出错即退。用同步写 fd 2：`console.error` 走的是异步写，紧跟 `process.exit` 时，
 * 管道下（CI / 重定向）多行提示会被截断，只剩第一行。
 */
function fatal(text) {
  writeSync(2, `${text}\n`)
  process.exit(1)
}

// 凭据先读：`--check-creds` 只校验凭据文件可读与字段解析，不拉内核、不联网。
if (!existsSync(opts.creds)) {
  const lines = [
    `找不到凭据文件：${opts.creds}`,
    '',
    '默认凭据文件（仓库根，已在 .gitignore 中忽略，可放明文 key、不会入库）：',
    `  ${defaultCreds}`,
    '',
    '按下面结构创建它（字段名与内核 provider.upsert 入参一致，`_` 开头的说明项可留可删）：',
    '  {',
    '    "_comment": "M4 联调凭据：已在 .gitignore 中忽略，严禁 git add / 提交 / 外发",',
    '    "name": "e2e-m4",',
    '    "protocol": "openai_chat_completions",',
    '    "base_url": "https://<网关域名>/v1",',
    '    "api_key": "<真实 key>",',
    '    "model": "",',
    '    "timeout_ms": 180000',
    '  }',
    '',
    '也可以用 --creds <path> 或环境变量 AI_IDE_E2E_CREDS 指向别处的凭据文件',
    '（若该文件不在 .gitignore 里，请先确认它不会被提交）。',
    opts.credsExplicit ? `本次用的是显式给出的路径：${opts.creds}` : ''
  ]
  fatal(lines.join('\n'))
}

let creds
try {
  creds = JSON.parse(readFileSync(opts.creds, 'utf8'))
} catch (err) {
  fatal(`凭据文件不是合法 JSON：${opts.creds}\n  ${err.message}`)
}

const baseUrl = String(creds.base_url ?? '').replace(/\/+$/, '')
if (!/^https?:\/\//.test(baseUrl)) {
  fatal(`凭据文件里的 base_url 不是 http(s) 地址：${opts.creds}`)
}
const apiKey = String(creds.api_key ?? '')
if (apiKey.length === 0) {
  fatal(`凭据文件里没有 api_key：${opts.creds}`)
}
// 可选字段：与内核 provider.upsert 入参同名，缺省沿用脚本内置值。
const providerName = String(creds.name ?? 'e2e-m4')
const providerProtocol = String(creds.protocol ?? 'openai_chat_completions')
const providerTimeoutMs = Number(creds.timeout_ms ?? 180_000)
// 凭据里点名了模型就优先用（命令行 --model / AI_IDE_E2E_MODEL 仍最优先）。
if (!opts.model && creds.model) opts.model = String(creds.model)

if (opts.checkCreds) {
  console.log('凭据自检（--check-creds：不拉内核、不联网）')
  console.log(`  文件：${opts.creds}（来源：${credsSource}，${readFileSync(opts.creds).length} 字节）`)
  console.log(`  base_url：${baseUrl}`)
  console.log(`  api_key：${apiKey.slice(0, 4)}***（长度 ${apiKey.length}）`)
  console.log(`  provider：${providerName} / ${providerProtocol} / timeout ${providerTimeoutMs} ms`)
  console.log(`  model：${opts.model || '（留空，运行期从 /v1/models 探活选择）'}`)
  process.exit(0)
}

if (!existsSync(opts.core)) {
  fatal(`找不到内核可执行文件：${opts.core}\n请先执行：cargo build -p ai-ide-core`)
}

console.log('M4 真实模型端到端联调')
console.log(`  内核：${opts.core}`)
console.log(
  `  上游：${baseUrl}（凭据来自 ${opts.creds}［${credsSource}］，api_key ${apiKey.slice(0, 4)}***，长度 ${apiKey.length}）`
)
console.log(`  沙箱：${opts.sandbox}\n`)

// ---------------------------------------------------------------------------
// 1. 模型选择：先列模型表，再按偏好探活「能不能工具调用」
// ---------------------------------------------------------------------------

const sleep = (ms) => new Promise((done) => setTimeout(done, ms))

const upstreamHeaders = () => ({
  'content-type': 'application/json',
  authorization: `Bearer ${apiKey}`
})

async function listModels() {
  const res = await fetch(`${baseUrl}/models`, {
    headers: { authorization: `Bearer ${apiKey}` }
  })
  if (!res.ok) return { ok: false, note: `HTTP ${res.status}`, models: [] }
  const body = await res.json()
  const models = (body.data ?? body.models ?? []).map((m) => m.id ?? m.name).filter(Boolean)
  return { ok: true, models }
}

/** 偏好顺序：项目里的示例 / 夹具都用 DeepSeek 家族的 id，故同族优先；其余按通用能力排。 */
const MODEL_PREFERENCE = [
  'deepseek-v4-pro',
  'deepseek-v4-flash',
  'deepseek-flash',
  'gpt-6-sol',
  'claude-sonnet-5',
  'glm-5.3',
  'qwen3.7-max',
  'kimi-k3',
  'MiniMax-M3',
  'grok-4.7',
  'doubao-seed-2.1-turbo'
]

/** 一次极小的工具调用探活：能返回 tool_calls 才算可用于 agent 闭环。 */
async function probeToolCall(model) {
  const body = {
    model,
    stream: false,
    messages: [
      { role: 'user', content: '请立刻调用 read 工具，path 填 "README.md"。不要用文字回答。' }
    ],
    tools: [
      {
        type: 'function',
        function: {
          name: 'read',
          description: '读取工作区里的文件',
          parameters: {
            type: 'object',
            properties: { path: { type: 'string' } },
            required: ['path'],
            additionalProperties: false
          }
        }
      }
    ]
  }
  let res
  try {
    res = await fetch(`${baseUrl}/chat/completions`, {
      method: 'POST',
      headers: upstreamHeaders(),
      body: JSON.stringify(body)
    })
  } catch (err) {
    return { ok: false, note: `连不上上游：${err.message}` }
  }
  const text = await res.text()
  if (!res.ok) return { ok: false, note: `HTTP ${res.status}：${text.slice(0, 160)}` }
  let json
  try {
    json = JSON.parse(text)
  } catch {
    return { ok: false, note: `响应不是 JSON：${text.slice(0, 160)}` }
  }
  const calls = json.choices?.[0]?.message?.tool_calls ?? []
  return calls.length > 0
    ? { ok: true, note: `首轮返回工具调用 ${calls[0].function?.name ?? '?'}` }
    : { ok: false, note: '未返回 tool_calls' }
}

async function chooseModel() {
  const listed = await listModels()
  if (!listed.ok) {
    console.log(`模型表不可用（${listed.note}），改用凭据/参数里的模型`)
    return { model: opts.model || MODEL_PREFERENCE[0], models: [], probes: [] }
  }
  console.log(`上游模型表：${listed.models.length} 个可用模型`)

  const available = new Set(listed.models)
  const probes = []
  const candidates = []
  if (opts.model) candidates.push(opts.model)
  for (const id of MODEL_PREFERENCE) if (available.has(id) && !candidates.includes(id)) candidates.push(id)
  // 偏好表都没命中时，退到「非图像、非 embedding」的第一个模型，避免脚本白跑。
  if (candidates.length === 0) {
    const fallback = listed.models.find((id) => !/image|embed|tts|whisper/i.test(id))
    if (fallback) candidates.push(fallback)
  }

  for (const model of candidates) {
    const probe = await probeToolCall(model)
    probes.push({ model, ...probe })
    console.log(`  探活 ${model}：${probe.ok ? 'PASS' : 'FAIL'} ${probe.note}`)
    if (probe.ok) return { model, models: listed.models, probes }
  }
  return { model: '', models: listed.models, probes }
}

// ---------------------------------------------------------------------------
// 2. 透明录制代理
// ---------------------------------------------------------------------------

/** 从 OpenAI 兼容响应里抽 SSE 增量（兼容非流式 JSON）。 */
function parseCompletion(text) {
  const out = { toolCalls: [], content: '', finish: null, usage: null }
  const take = (choice) => {
    const delta = choice.delta ?? choice.message ?? {}
    if (typeof delta.content === 'string') out.content += delta.content
    for (const call of delta.tool_calls ?? []) {
      const index = call.index ?? 0
      out.toolCalls[index] ??= { name: '', args: '' }
      if (call.function?.name) out.toolCalls[index].name = call.function.name
      if (typeof call.function?.arguments === 'string') {
        out.toolCalls[index].args += call.function.arguments
      }
    }
    if (choice.finish_reason) out.finish = choice.finish_reason
  }

  const whole = text.trim()
  if (whole.startsWith('{')) {
    try {
      const json = JSON.parse(whole)
      for (const choice of json.choices ?? []) take(choice)
      out.usage = json.usage ?? null
      return out
    } catch {
      /* 落到 SSE 分支 */
    }
  }
  for (const line of text.split(/\r?\n/)) {
    if (!line.startsWith('data:')) continue
    const payload = line.slice(5).trim()
    if (payload.length === 0 || payload === '[DONE]') continue
    let json
    try {
      json = JSON.parse(payload)
    } catch {
      continue
    }
    for (const choice of json.choices ?? []) take(choice)
    if (json.usage) out.usage = json.usage
  }
  return out
}

function startProxy() {
  const records = []
  const server = http.createServer((req, res) => {
    void handle(req, res)
  })

  async function handle(req, res) {
    const chunks = []
    for await (const chunk of req) chunks.push(chunk)
    const rawBody = Buffer.concat(chunks).toString('utf8')

    const headers = { ...req.headers }
    for (const key of ['host', 'content-length', 'connection', 'accept-encoding']) delete headers[key]
    headers['accept-encoding'] = 'identity'

    const target = `${baseUrl}${req.url.replace(/^\/v1/, '')}`
    const started = performance.now()
    let upstream
    try {
      upstream = await fetch(target, {
        method: req.method,
        headers,
        body: req.method === 'GET' || req.method === 'HEAD' ? undefined : rawBody
      })
    } catch (err) {
      records.push({ url: target, error: err.message })
      res.writeHead(502, { 'content-type': 'application/json' })
      res.end(JSON.stringify({ error: { message: `代理转发失败：${err.message}` } }))
      return
    }

    const outHeaders = {}
    for (const [key, value] of upstream.headers) {
      if (['content-encoding', 'content-length', 'transfer-encoding', 'connection'].includes(key)) continue
      outHeaders[key] = value
    }
    res.writeHead(upstream.status, outHeaders)

    const parts = []
    if (upstream.body) {
      for await (const chunk of upstream.body) {
        parts.push(Buffer.from(chunk))
        res.write(chunk)
      }
    }
    res.end()

    const text = Buffer.concat(parts).toString('utf8')
    const record = {
      url: target,
      status: upstream.status,
      ms: Math.round(performance.now() - started),
      request: null,
      completion: parseCompletion(text)
    }
    try {
      record.request = JSON.parse(rawBody)
    } catch {
      /* 非 JSON 请求体（例如 GET /models） */
    }
    records.push(record)
  }

  return {
    records,
    /** 启动并返回代理的 base_url（`/v1` 前缀）。 */
    async listen() {
      await new Promise((done) => server.listen(0, '127.0.0.1', done))
      const { port } = server.address()
      return `http://127.0.0.1:${port}/v1`
    },
    async close() {
      await new Promise((done) => server.close(done))
    }
  }
}

const proxy = opts.direct ? null : startProxy()
const proxyBase = proxy ? await proxy.listen() : baseUrl
console.log(
  proxy
    ? `透明录制代理就绪：${proxyBase} → ${baseUrl}（请求原样转发，仅留档对话与响应）\n`
    : `直连模式：base_url = ${baseUrl}（无工具调用明细）\n`
)

/** 把代理留档按「哪条请求的 messages 里带着这句目标」归到一次运行上。 */
function analyzeRun(goal) {
  const empty = {
    rounds: 0,
    modelToolCalls: [],
    toolCalls: [],
    results: [],
    ambiguousCount: 0,
    errorCount: 0,
    usage: { prompt: 0, completion: 0 }
  }
  if (!proxy) return empty

  const mine = proxy.records.filter((r) =>
    (r.request?.messages ?? []).some(
      (m) => m.role === 'user' && typeof m.content === 'string' && m.content.includes(goal)
    )
  )
  if (mine.length === 0) return empty

  // 工具时间线：把本次运行的每一条请求的 messages 按时间顺序扫一遍，按 tool_call.id 去重。
  // 只看最后一条请求是不够的——内核的 history 窗口可能把早先的调用挤出去，会漏掉真实发生过的 grep/edit。
  const byCallId = new Map()
  const seenResultText = new Set()
  const toolCalls = []
  const results = []
  for (const r of mine) {
    for (const message of r.request?.messages ?? []) {
      if (message.role === 'assistant') {
        for (const call of message.tool_calls ?? []) {
          const entry = { name: call.function?.name ?? '?', args: call.function?.arguments ?? '' }
          if (call.id && byCallId.has(call.id)) continue
          toolCalls.push(entry)
          if (call.id) byCallId.set(call.id, entry)
        }
      } else if (message.role === 'tool') {
        const text = typeof message.content === 'string' ? message.content : JSON.stringify(message.content)
        const call = byCallId.get(message.tool_call_id)
        if (call) {
          if (call.result !== undefined) continue
          call.result = text
        } else if (seenResultText.has(text)) {
          continue
        } else {
          seenResultText.add(text)
        }
        results.push({ name: call?.name ?? '?', content: text })
      }
    }
  }

  // 响应侧兜底：模型已经吐出、但还没进过任何后续请求 messages 的调用（末轮或中途夭折），
  // 按 name + args 与已知调用去重后补进时间线。
  const seenCalls = new Set(toolCalls.map((c) => `${c.name}\u0000${c.args}`))
  for (const call of mine.flatMap((r) => r.completion?.toolCalls ?? [])) {
    const entry = { name: call.name || '?', args: call.args ?? '' }
    const key = `${entry.name}\u0000${entry.args}`
    if (seenCalls.has(key)) continue
    seenCalls.add(key)
    toolCalls.push(entry)
  }

  const usage = mine.reduce(
    (acc, r) => {
      const u = r.completion?.usage
      if (u) {
        acc.prompt += u.prompt_tokens ?? 0
        acc.completion += u.completion_tokens ?? 0
      }
      return acc
    },
    { prompt: 0, completion: 0 }
  )

  // 响应侧的调用：运行中途夭折时，请求侧 messages 里还看不到工具调用，只能看模型自己吐了什么。
  const modelToolCalls = mine.flatMap((r) =>
    (r.completion?.toolCalls ?? []).map((c) => ({ name: c.name || '?', args: c.args ?? '' }))
  )

  return {
    rounds: mine.length,
    modelToolCalls,
    toolCalls,
    results,
    ambiguousCount: results.filter((r) => r.content.includes('无法唯一匹配')).length,
    errorCount: results.filter((r) =>
      /("ok"\s*:\s*false|错误|失败|找不到|ENOENT|Cannot find|no such file|EACCES)/i.test(r.content)
    ).length,
    usage
  }
}

// ---------------------------------------------------------------------------
// 3. 内核会话（JSON-RPC over stdio）
// ---------------------------------------------------------------------------

class CoreSession {
  constructor(label) {
    this.label = label
    this.pending = new Map()
    this.runs = new Map()
    this.early = new Map()
    this.stderrText = ''
    this.approvalLog = []
    this.decision = 'allow_once'
  }

  async start() {
    this.child = spawn(opts.core, [], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true })
    this.rl = createInterface({ input: this.child.stdout, crlfDelay: Infinity })
    this.rl.on('line', (line) => this.onLine(line))
    this.child.stderr.setEncoding('utf8')
    this.child.stderr.on('data', (chunk) => {
      this.stderrText += chunk
    })
    this.child.on('exit', (code) => {
      for (const slot of this.pending.values()) {
        slot.reject(new Error(`内核已退出（code=${code}）`))
      }
      this.pending.clear()
    })
    await this.request('core.hello', {
      client_version: '0.1.0',
      protocol_version: { major: 1, minor: 0 },
      platform: process.platform,
      locale: 'zh-CN',
      capabilities: []
    })
  }

  onLine(line) {
    const text = line.trim()
    if (text.length === 0) return
    let frame
    try {
      frame = JSON.parse(text)
    } catch {
      return
    }
    if (frame.id && ('result' in frame || 'error' in frame)) {
      const slot = this.pending.get(frame.id)
      if (!slot) return
      this.pending.delete(frame.id)
      if (frame.error) slot.reject(new Error(`[${frame.error.code}] ${frame.error.message}`))
      else slot.resolve(frame.result)
      return
    }
    this.dispatch(frame)
  }

  dispatch(frame) {
    const run = this.runs.get(frame.params?.run_id)
    if (!run) {
      // 响应与首条事件可能贴得很近：先兜住，登记后再补投。
      const queue = this.early.get(frame.params?.run_id) ?? []
      queue.push(frame)
      this.early.set(frame.params?.run_id, queue)
      return
    }
    if (frame.method === 'agent.approval_required') {
      const params = frame.params
      const entry = { tool: params.tool, risk: params.risk, reason: params.reason, args: params.args }
      run.approvals.push(entry)
      this.approvalLog.push({ label: this.label, ...entry })
      void this.request('agent.decide', {
        run_id: params.run_id,
        approval_id: params.approval_id,
        decision: this.decision
      }).catch(() => {})
      return
    }
    if (frame.method === 'agent.finished') {
      this.runs.delete(frame.params.run_id)
      run.resolve({
        status: frame.params.status,
        steps: frame.params.steps,
        summary: frame.params.summary ?? null,
        approvals: run.approvals,
        durationMs: Math.round(performance.now() - run.startedAt)
      })
    }
  }

  request(method, params = {}) {
    const id = randomUUID()
    const line = `${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`
    return new Promise((done, fail) => {
      this.pending.set(id, { resolve: done, reject: fail })
      this.child.stdin.write(line)
    })
  }

  /** 注册 provider（api_key 只进内核内存，不落盘），返回 provider_id。 */
  async upsertProvider() {
    const result = await this.request('provider.upsert', {
      name: providerName,
      protocol: providerProtocol,
      base_url: proxyBase,
      api_key: apiKey,
      enabled: true,
      timeout_ms: providerTimeoutMs
    })
    this.providerId = result.provider_id
    return result.provider_id
  }

  async openWorkspace(dir) {
    return this.request('workspace.open', { path: dir, watch: false })
  }

  async runAgent({ model, goal, maxSteps = 16 }) {
    const { run_id: runId } = await this.request('agent.start', {
      model_ref: { provider_id: this.providerId, model_id: model },
      goal,
      max_steps: maxSteps
    })

    const run = { approvals: [], startedAt: performance.now() }
    const promise = new Promise((done) => {
      run.resolve = done
    })
    this.runs.set(runId, run)
    for (const frame of this.early.get(runId) ?? []) this.dispatch(frame)
    this.early.delete(runId)

    const timer = setTimeout(() => {
      if (this.runs.delete(runId)) {
        void this.request('agent.stop', { run_id: runId }).catch(() => {})
        run.resolve({
          status: 'timeout',
          steps: -1,
          summary: `超过 ${opts.runTimeoutMs} ms 未收到 agent.finished`,
          approvals: run.approvals,
          durationMs: Math.round(performance.now() - run.startedAt)
        })
      }
    }, opts.runTimeoutMs)

    const result = await promise
    clearTimeout(timer)
    return { runId, ...result }
  }

  async close() {
    try {
      this.rl?.close()
    } catch {
      /* 忽略 */
    }
    this.child?.kill()
    await sleep(50)
  }
}

/** 一套「独立内核 + 独立工作区」的组合，避免用例之间互相污染。 */
async function withSession(dir, fn) {
  const session = new CoreSession(dir)
  try {
    await session.start()
    await session.openWorkspace(dir)
    await session.upsertProvider()
    return await fn(session)
  } finally {
    await session.close()
  }
}

// ---------------------------------------------------------------------------
// 4. 沙箱
// ---------------------------------------------------------------------------

rmSync(opts.sandbox, { recursive: true, force: true })
mkdirSync(opts.sandbox, { recursive: true })

function writeFiles(root, files) {
  for (const [rel, content] of Object.entries(files)) {
    const abs = join(root, rel)
    mkdirSync(dirname(abs), { recursive: true })
    writeFileSync(abs, content, 'utf8')
  }
}

const readText = (abs) => readFileSync(abs, 'utf8')
const sha = (text) => createHash('sha256').update(text, 'utf8').digest('hex').slice(0, 12)

function runNode(root, script) {
  try {
    const stdout = execFileSync(process.execPath, [script], {
      cwd: root,
      encoding: 'utf8',
      timeout: 60_000,
      stdio: ['ignore', 'pipe', 'pipe']
    })
    return { code: 0, stdout: stdout.trim() }
  } catch (err) {
    return {
      code: err.status ?? -1,
      stdout: `${err.stdout ?? ''}`.trim(),
      stderr: `${err.stderr ?? ''}`.trim()
    }
  }
}

function git(root, argv) {
  return execFileSync('git', argv, { cwd: root, encoding: 'utf8' }).trim()
}

// ---------------------------------------------------------------------------
// 5. 四条基线任务（定义取自 docs/05 §M4 验收）
// ---------------------------------------------------------------------------

const TASK1_MAIN = `export interface Config {
  baseUrl: string
}

export class Client {
  constructor(private readonly config: Config) {}
  run(): void {
    console.log('client running against', this.config.baseUrl)
  }
}

export function main(): void {
  const config = loadConfig()
  const client = createClient(config)
  client.run()
}

function loadConfig(): Config {
  return { baseUrl: process.env.API_BASE ?? 'http://127.0.0.1:8787' }
}

function createClient(config: Config): Client {
  return new Client(config)
}
`

const TASK2_FETCH = `export async function loadUser(id: string): Promise<unknown> {
  const res = await fetch(\`/api/user/\${id}\`)
  return res.json()
}

export async function loadOrders(id: string): Promise<unknown> {
  const res = await fetch(\`/api/orders/\${id}\`)
  return res.json()
}

export async function sendPing(): Promise<void> {
  await fetch('/api/ping', { method: 'POST' })
}
`

const TASK3_MATH = `export function add(a: number, b: number): number {
  return Math.abs(a) + Math.abs(b)
}
`

const TASK3_TEST = `import { add } from './math.mjs'

const cases = [
  [2, 3, 5],
  [0, 0, 0],
  [-1, 1, 0],
  [10, -4, 6]
]

let failed = 0
for (const [a, b, want] of cases) {
  const got = add(a, b)
  if (got !== want) {
    failed += 1
    console.log(\`FAIL add(\${a}, \${b}) = \${got}，期望 \${want}\`)
  }
}
console.log(failed === 0 ? '全部通过' : \`失败 \${failed} 例\`)
process.exit(failed === 0 ? 0 : 1)
`

const TASK4_TEXT = `export function slugify(input: string): string {
  return input
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
}
`

const TASK4_TEST = `import { slugify } from './utils/text.mjs'

const got = slugify('  Hello M4 World  ')
if (got !== 'hello-m4-world') {
  console.log(\`FAIL slugify = \${got}\`)
  process.exit(1)
}
console.log('全部通过')
`

const TASK4_APP = `import { slugify } from './utils/text.mjs'

console.log(slugify('Page Title'))
`

const baselineTasks = [
  {
    id: 1,
    title: '解释 src/main.ts 的入口流程（read 一次后正确回答）',
    async setup(root) {
      writeFiles(root, { 'src/main.ts': TASK1_MAIN })
    },
    goal: '解释 src/main.ts 的入口流程：main() 依次做了什么、它调用了哪些函数。只读，不要改动任何文件。',
    maxSteps: 8,
    async verify(root, run, analysis) {
      const after = readText(join(root, 'src', 'main.ts'))
      const calls = analysis.toolCalls.map((c) => c.name)
      const wrote = calls.filter((n) => ['write', 'edit', 'create', 'apply_diff'].includes(n))
      const summary = run.summary ?? ''
      return [
        ['运行正常收工', run.status === 'completed', `status=${run.status} steps=${run.steps}`],
        ['调用了 read 工具', calls.includes('read'), `工具序列=[${calls.join(', ')}]`],
        ['没有写类工具调用', wrote.length === 0, wrote.join(', ') || '无'],
        ['答案点到了被调函数', /loadConfig/.test(summary) && /createClient/.test(summary), summary.slice(0, 120)],
        ['文件未被改动', sha(after) === sha(TASK1_MAIN), `sha=${sha(after)}`]
      ]
    }
  },
  {
    id: 2,
    title: '给所有 fetch 加 5s 超时（edit + git diff 符合预期）',
    async setup(root) {
      writeFiles(root, { 'src/fetch.ts': TASK2_FETCH, 'notes.md': '# notes\n\n别动我\n' })
      git(root, ['init', '-q'])
      git(root, ['add', '-A'])
      git(root, ['-c', 'user.email=e2e@local', '-c', 'user.name=e2e', 'commit', '-qm', 'init'])
    },
    goal: '给 src/fetch.ts 里的每一处 fetch 调用都加上 5 秒超时（signal: AbortSignal.timeout(5000)），不要改动其它逻辑，也不要动别的文件。',
    maxSteps: 14,
    async verify(root, run, analysis) {
      const text = readText(join(root, 'src', 'fetch.ts'))
      const fetches = (text.match(/fetch\(/g) ?? []).length
      const signals = (text.match(/AbortSignal\.timeout\(5000\)/g) ?? []).length
      const diff = git(root, ['diff', '--stat'])
      const touched = git(root, ['diff', '--name-only']).split('\n').filter(Boolean)
      const notes = readText(join(root, 'notes.md'))
      return [
        ['运行正常收工', run.status === 'completed', `status=${run.status} steps=${run.steps}`],
        ['用了 edit 工具', analysis.toolCalls.some((c) => c.name === 'edit'), `工具序列=[${analysis.toolCalls.map((c) => c.name).join(', ')}]`],
        ['每处 fetch 都有 5s 超时', fetches > 0 && fetches === signals, `fetch=${fetches} signal=${signals}`],
        ['git diff 只动了 src/fetch.ts', touched.length === 1 && touched[0] === 'src/fetch.ts', touched.join(', ')]
      ].map((row) => (row[0] === '运行正常收工' ? row : row))
        .concat([['notes.md 未被改', notes === '# notes\n\n别动我\n', `diffstat=${diff.split('\n').pop()}`]])
    }
  },
  {
    id: 3,
    title: '跑 node test.mjs，红了就修到绿（命令执行 + 迭代修复）',
    async setup(root) {
      writeFiles(root, { 'math.mjs': TASK3_MATH, 'test.mjs': TASK3_TEST })
    },
    goal: '先运行 `node test.mjs`（不要改 test.mjs 里的期望值），如果测试失败就把 math.mjs 的源码改到测试全部通过，改完再跑一次确认。',
    maxSteps: 16,
    async verify(root, run, analysis) {
      const result = runNode(root, 'test.mjs')
      const math = readText(join(root, 'math.mjs'))
      const commands = analysis.toolCalls.filter((c) => c.name === 'run_command').length
      return [
        ['运行正常收工', run.status === 'completed', `status=${run.status} steps=${run.steps}`],
        ['命令执行走过了审批', run.approvals.length > 0, `审批=${run.approvals.length} 次（${run.approvals.map((a) => a.tool).join(',')}）`],
        ['run_command 至少跑了两次', commands >= 2, `run_command=${commands} 次`],
        ['改了源码', math.trim() !== TASK3_MATH.trim(), `sha=${sha(math)}`],
        ['测试全绿（脚本外复跑）', result.code === 0, `exit=${result.code} out=${result.stdout.slice(0, 80)}`],
        ['test.mjs 未被改', readText(join(root, 'test.mjs')) === TASK3_TEST, '']
      ]
    }
  },
  {
    id: 4,
    title: '把 utils 目录改名成 helpers 并修复所有 import（grep + edit 多处 + 命令验证）',
    async setup(root) {
      writeFiles(root, {
        'test.mjs': TASK4_TEST,
        'app.mjs': TASK4_APP,
        'utils/text.mjs': TASK4_TEXT
      })
      git(root, ['init', '-q'])
      git(root, ['add', '-A'])
      git(root, ['-c', 'user.email=e2e@local', '-c', 'user.name=e2e', 'commit', '-qm', 'init'])
    },
    // docs/05 把这条基线任务定成「grep + edit 多处 + 命令验证」，所以目标里点名要先用
    // grep 找引用——否则模型会改用 list/read 逐个翻文件，那就不是在跑这条基线了。
    goal:
      '把 utils 目录重命名成 helpers，并修复所有引用它的 import 路径（test.mjs 与 app.mjs），最后运行 `node test.mjs` 验证没坏。' +
      '要求：先用 grep 搜出所有引用 `./utils/` 的地方，再逐个改成新路径。',
    // 这条任务要「改名 + 改 3 处 import + 跑命令验证」，模型实测用掉 14~20 步，
    // 20 步上限会把已经改完的活儿卡在收工前（首轮实测就撞过一次）。本探针量的是
    // 「能不能端到端做完」，不是在量步数上限，所以留足余量。
    maxSteps: 32,
    async verify(root, run, analysis) {
      const moved = existsSync(join(root, 'helpers', 'text.mjs')) && !existsSync(join(root, 'utils'))
      const test = readText(join(root, 'test.mjs'))
      const app = readText(join(root, 'app.mjs'))
      const importsFixed = !/\.\/utils\//.test(test) && !/\.\/utils\//.test(app)
      const result = runNode(root, 'test.mjs')
      return [
        ['运行正常收工', run.status === 'completed', `status=${run.status} steps=${run.steps}`],
        ['目录真的改名了', moved, `helpers/text.mjs=${existsSync(join(root, 'helpers', 'text.mjs'))} utils=${existsSync(join(root, 'utils'))}`],
        ['import 都改成新路径', importsFixed, `test=${/\.\/utils\//.test(test)} app=${/\.\/utils\//.test(app)}`],
        ['用了 grep 找引用', analysis.toolCalls.some((c) => c.name === 'grep'), `工具序列=[${analysis.toolCalls.map((c) => c.name).join(', ')}]`],
        ['测试全绿（脚本外复跑）', result.code === 0, `exit=${result.code} out=${result.stdout.slice(0, 80)}`]
      ]
    }
  }
]

// ---------------------------------------------------------------------------
// 6. `edit_file` 非唯一匹配 20 用例
// ---------------------------------------------------------------------------

const SECTION_NAMES = ['alpha', 'beta', 'gamma', 'delta', 'epsilon']

function buildEditCase(index) {
  const repeats = 2 + (index % 3) // 2 ~ 4 处重复片段
  const targetIndex = 1 + (index % repeats) // 目标落在第几处（1 起）
  const sections = []
  const expected = []
  for (let i = 0; i < repeats; i += 1) {
    const name = SECTION_NAMES[i]
    const timeout = 1000
    sections.push(`[server-${name}]\ntimeout: ${timeout}\nretries: ${i + 1}\n`)
    expected.push({ name, timeout: i + 1 === targetIndex ? 5000 : 1000, retries: i + 1 })
  }
  const target = expected[targetIndex - 1]
  return {
    index,
    repeats,
    targetIndex,
    targetSection: target.name,
    file: 'config.ini',
    content: sections.join('\n'),
    expected,
    goal:
      `（用例 #${index + 1}）把 config.ini 里 [server-${target.name}] 这一段的 timeout 从 1000 改成 5000，其它段落一律保持原样。\n` +
      `执行要求：第一次 edit 必须只用整行 \`timeout: 1000\` 当 old_text（不要带上下文行）；` +
      `如果工具回报这个锚点不唯一，就补足上下文重试，直到改对为止。`,
    verify(root) {
      const text = readText(join(root, 'config.ini'))
      const seen = new Map()
      let current = null
      for (const line of text.split(/\r?\n/)) {
        const head = line.match(/^\[server-([a-z]+)\]$/)
        if (head) {
          current = head[1]
          seen.set(current, { timeout: null, retries: null })
          continue
        }
        const field = line.match(/^(timeout|retries):\s*(\d+)$/)
        if (field && current) seen.get(current)[field[1]] = Number(field[2])
      }
      const problems = []
      for (const want of expected) {
        const got = seen.get(want.name)
        if (!got) {
          problems.push(`段落 [server-${want.name}] 丢了`)
          continue
        }
        if (got.timeout !== want.timeout) {
          problems.push(`[server-${want.name}] timeout=${got.timeout}（期望 ${want.timeout}）`)
        }
        if (got.retries !== want.retries) {
          problems.push(`[server-${want.name}] retries=${got.retries}（期望 ${want.retries}）`)
        }
      }
      return { ok: problems.length === 0, problems }
    }
  }
}

// ---------------------------------------------------------------------------
// 7. 跑起来
// ---------------------------------------------------------------------------

const chosen = await chooseModel()
if (!chosen.model) {
  console.error('\n没有探活成功的模型，无法联调。探活明细：')
  for (const probe of chosen.probes) console.error(`  ${probe.model}: ${probe.note}`)
  await proxy?.close()
  process.exit(1)
}
console.log(`\n选定模型：${chosen.model}\n`)

const report = {
  generated_at: new Date().toISOString().replace(/\.\d+Z$/, 'Z'),
  upstream: baseUrl,
  proxy: proxy ? proxyBase : null,
  model: chosen.model,
  models_available: chosen.models.length,
  probes: chosen.probes,
  tasks: [],
  edit_cases: { total: 0, passed: 0, with_ambiguous_first_attempt: 0, retried_and_passed: 0, rate: null, cases: [] }
}

const wantTasks = new Set(opts.tasks.split(',').map((t) => Number(t.trim())))

console.log('=== 四条基线任务 ===')
for (const task of baselineTasks) {
  if (!wantTasks.has(task.id)) continue
  const root = join(opts.sandbox, `task${task.id}`)
  mkdirSync(root, { recursive: true })
  await task.setup(root)

  let run = { status: 'not-started', steps: -1, summary: null, approvals: [], durationMs: 0 }
  let analysis = { rounds: 0, toolCalls: [], results: [], ambiguousCount: 0, errorCount: 0, usage: { prompt: 0, completion: 0 } }
  let error = null
  try {
    run = await withSession(root, (session) => session.runAgent({ model: chosen.model, goal: task.goal, maxSteps: task.maxSteps }))
    analysis = analyzeRun(task.goal)
  } catch (err) {
    error = err.message
  }

  const checks = error ? [['联调自身报错', false, error]] : await task.verify(root, run, analysis)
  const passedAll = checks.every(([, ok]) => ok)
  report.tasks.push({
    id: task.id,
    title: task.title,
    goal: task.goal,
    status: run.status,
    steps: run.steps,
    duration_ms: run.durationMs,
    approvals: run.approvals.map((a) => ({ tool: a.tool, risk: a.risk })),
    rounds: analysis.rounds,
    tool_calls: analysis.toolCalls.map((c) => c.name),
    model_tool_calls: (analysis.modelToolCalls ?? []).map((c) => ({ name: c.name, args: String(c.args).slice(0, 200) })),
    tool_call_detail: analysis.toolCalls.map((c) => ({ name: c.name, args: c.args })),
    ambiguous_matches: analysis.ambiguousCount,
    tool_errors: analysis.errorCount,
    usage: analysis.usage,
    summary: run.summary,
    error,
    passed: passedAll,
    checks: checks.map(([name, ok, detail]) => ({ name, ok, detail }))
  })

  console.log(`\n任务 ${task.id}：${task.title}`)
  console.log(`  status=${run.status} steps=${run.steps} 轮次=${analysis.rounds} 耗时=${run.durationMs} ms 审批=${run.approvals.length}`)
  console.log(`  工具序列=[${analysis.toolCalls.map((c) => c.name).join(', ')}]`)
  if (analysis.toolCalls.length === 0 && analysis.modelToolCalls.length > 0) {
    console.log(
      `  模型已发起的调用（尚未回流）=[${analysis.modelToolCalls
        .map((c) => `${c.name}(${String(c.args).slice(0, 80)})`)
        .join(', ')}]`
    )
  }
  for (const [name, ok, detail] of checks) {
    console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  —— ${detail}` : ''}`)
  }
  if (run.summary) console.log(`  收尾：${run.summary.slice(0, 200)}`)
}

// ---- 非唯一匹配 20 用例 ----
const caseTotal = Number.isFinite(opts.cases) ? opts.cases : 0
if (caseTotal > 0) {
  console.log(`\n=== edit_file 非唯一匹配用例（${caseTotal} 个，并发 ${opts.parallel}）===`)
  const cases = Array.from({ length: caseTotal }, (_, i) => buildEditCase(i))

  const runCase = async (item) => {
    const root = join(opts.sandbox, 'edit-unique', `case-${String(item.index + 1).padStart(2, '0')}`)
    mkdirSync(root, { recursive: true })
    writeFileSync(join(root, item.file), item.content, 'utf8')

    let run = { status: 'not-started', steps: -1, summary: null, approvals: [], durationMs: 0 }
    let analysis = { rounds: 0, toolCalls: [], results: [], ambiguousCount: 0, errorCount: 0, usage: { prompt: 0, completion: 0 } }
    let error = null
    try {
      run = await withSession(root, (session) =>
        session.runAgent({ model: chosen.model, goal: item.goal, maxSteps: 8 })
      )
      analysis = analyzeRun(item.goal)
    } catch (err) {
      error = err.message
    }
    const verdict = error ? { ok: false, problems: [error] } : item.verify(root)
    const record = {
      index: item.index + 1,
      repeats: item.repeats,
      target_index: item.targetIndex,
      target_section: item.targetSection,
      status: run.status,
      steps: run.steps,
      duration_ms: run.durationMs,
      rounds: analysis.rounds,
      tool_calls: analysis.toolCalls.map((c) => c.name),
      ambiguous_first_attempt: analysis.ambiguousCount > 0,
      ambiguous_count: analysis.ambiguousCount,
      tool_errors: analysis.errorCount,
      first_attempt: analysis.toolCalls[0]
        ? `${analysis.toolCalls[0].name}(${String(analysis.toolCalls[0].args).slice(0, 60)})`
        : '（无工具调用）',
      passed: verdict.ok,
      problems: verdict.problems ?? [],
      summary: (run.summary ?? '').slice(0, 160)
    }
    console.log(
      `  用例 ${String(record.index).padStart(2, '0')}：${verdict.ok ? 'PASS' : 'FAIL'}` +
        `  重复=${item.repeats} 目标=第${item.targetIndex}处` +
        `  轮次=${record.rounds} 步数=${record.steps} 耗时=${record.duration_ms}ms` +
        `  撞非唯一=${record.ambiguous_first_attempt ? '是' : '否'}` +
        (verdict.ok ? '' : `  ← ${record.problems.join('；')}`)
    )
    return record
  }

  const records = new Array(cases.length)
  let cursor = 0
  const workers = Array.from({ length: Math.min(opts.parallel, cases.length) }, async () => {
    for (;;) {
      const index = cursor
      cursor += 1
      if (index >= cases.length) return
      records[index] = await runCase(cases[index])
    }
  })
  await Promise.all(workers)

  const passed = records.filter((r) => r.passed).length
  const withAmbiguous = records.filter((r) => r.ambiguous_first_attempt).length
  const retriedAndPassed = records.filter((r) => r.ambiguous_first_attempt && r.passed).length
  report.edit_cases = {
    total: records.length,
    passed,
    with_ambiguous_first_attempt: withAmbiguous,
    retried_and_passed: retriedAndPassed,
    rate: records.length === 0 ? null : passed / records.length,
    first_unique_rate: records.length === 0 ? null : (records.length - withAmbiguous) / records.length,
    cases: records
  }
  console.log(
    `\n非唯一匹配用例：${passed}/${records.length} 通过` +
      `（成功率 ${((passed / records.length) * 100).toFixed(1)}%，硬指标 ≥ 90%）`
  )
  console.log(
    `  非唯一通道命中 ${withAmbiguous}/${records.length} 例（工具回可回灌的 3003「无法唯一匹配」），` +
      `其中补足上下文后重试成功 ${retriedAndPassed} 例；` +
      `第一枪锚点就没撞歧义的 ${records.length - withAmbiguous} 例`
  )
}

await proxy?.close()

// ---------------------------------------------------------------------------
// 8. 汇总
// ---------------------------------------------------------------------------

const baselineOk = report.tasks.length > 0 && report.tasks.every((t) => t.passed)
const editOk =
  report.edit_cases.total === 0 || (report.edit_cases.rate ?? 0) >= 0.9
const totalTokens = report.tasks.reduce(
  (acc, t) => acc + t.usage.prompt + t.usage.completion,
  0
)

report.baseline_passed = baselineOk
report.edit_target_met = editOk
report.ok = baselineOk && editOk

mkdirSync(dirname(opts.report), { recursive: true })
writeFileSync(opts.report, `${JSON.stringify(report, null, 2)}\n`, 'utf8')

console.log('\n=== 汇总 ===')
console.log(`模型：${report.model}（上游 ${report.upstream}${report.proxy ? `，录制代理 ${report.proxy}` : '，直连'}）`)
console.log(`基线任务：${report.tasks.filter((t) => t.passed).length}/${report.tasks.length} 通过`)
console.log(
  `非唯一匹配：${report.edit_cases.passed}/${report.edit_cases.total}` +
    `（${report.edit_cases.rate === null ? '未跑' : `${(report.edit_cases.rate * 100).toFixed(1)}%`}）`
)
console.log(`基线任务 token 合计：${totalTokens}`)
console.log(`留档：${opts.report}`)
console.log(`结论：${report.ok ? 'PASS' : 'FAIL'}`)

process.exit(report.ok ? 0 : 1)
