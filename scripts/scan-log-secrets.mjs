/**
 * 日志密钥门禁：扫描 JSONL 日志，确认没有任何明文凭据落盘。
 *
 * 为什么要有这个脚本：docs/05 把「日志不含 API Key」写成 M5 的验收硬指标，
 * 而「脱敏代码写了」和「日志真的干净」是两件事——只有拿真实日志扫一遍才算数。
 * 脱敏规则本身由 `crates/core/src/logging.rs` + `crates/store/src/redact.rs` 实现，
 * 这里只做黑盒校验。
 *
 * 用法：
 *   node scripts/scan-log-secrets.mjs <日志目录> [--require-files]
 *   node scripts/scan-log-secrets.mjs --self-test
 *
 * `--require-files`：目录里没有任何日志行就判失败，避免「扫了个空目录」蒙混过关。
 * `--self-test`：先自证检测器有效（正样本必中、脱敏后的形态必不中），再退出。
 */

import { readdirSync, readFileSync, statSync } from 'node:fs'
import { extname, join } from 'node:path'

/** 会被当作密钥的特征模式。刻意比脱敏侧宽：宁可误报，不可漏报。 */
const PATTERNS = [
  { name: 'OpenAI 风格密钥', regex: /sk-[A-Za-z0-9_-]{16,}/ },
  { name: 'GitHub 令牌', regex: /(?:ghp|gho|ghu|ghs)_[A-Za-z0-9]{16,}|github_pat_[A-Za-z0-9_]{16,}/ },
  { name: 'Slack 令牌', regex: /xox[bpas]-[A-Za-z0-9-]{10,}/ },
  { name: 'AWS Access Key', regex: /AKIA[0-9A-Z]{16}/ },
  { name: 'Google API Key', regex: /AIza[0-9A-Za-z_-]{20,}/ },
  { name: 'Bearer 令牌', regex: /Bearer\s+[A-Za-z0-9._-]{16,}/ },
  // JSON 里键名就叫 api_key / token 的，值里必须出现打码痕迹 `***`，
  // 否则视为明文（`sk-prob***0000` 这类脱敏形态要放行）。
  { name: '明文密钥字段', regex: /"(?:api[_-]?key|access[_-]?token|refresh[_-]?token|secret|password)"\s*:\s*"(?![^"]*\*\*\*)[^"]{12,}"/i },
]

/** 只扫这些扩展名：日志目录里可能还会放别的东西。 */
const EXTENSIONS = new Set(['.jsonl', '.log', '.ndjson', '.txt'])

/** 单个文件超过这个大小就分段读，避免一口气吃掉几百 MB。 */
const CHUNK_BYTES = 4 * 1024 * 1024

function listLogFiles(dir) {
  const found = []
  const walk = (current) => {
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const path = join(current, entry.name)
      if (entry.isDirectory()) {
        walk(path)
        continue
      }
      if (!entry.isFile()) continue
      if (EXTENSIONS.has(extname(entry.name).toLowerCase())) found.push(path)
    }
  }
  walk(dir)
  return found
}

/** 把命中片段打码，日志里只留「像什么」而不留「是什么」。 */
function maskHit(text) {
  if (text.length <= 8) return '***'
  return `${text.slice(0, 4)}***${text.slice(-2)}`
}

function scanText(text, file, hits) {
  const lines = text.split(/\r?\n/)
  lines.forEach((line, index) => {
    for (const pattern of PATTERNS) {
      const match = pattern.regex.exec(line)
      if (match) {
        hits.push({
          file,
          line: index + 1,
          kind: pattern.name,
          sample: maskHit(match[0]),
        })
      }
    }
  })
}

function scanFile(path, hits) {
  const size = statSync(path).size
  if (size <= CHUNK_BYTES) {
    scanText(readFileSync(path, 'utf8'), path, hits)
    return
  }
  // 大文件分块读，块边界按换行对齐，保证不把一条记录劈成两半。
  const handle = readFileSync(path)
  let start = 0
  while (start < handle.length) {
    let end = Math.min(start + CHUNK_BYTES, handle.length)
    if (end < handle.length) {
      const nextBreak = handle.indexOf(0x0a, end)
      end = nextBreak === -1 ? handle.length : nextBreak + 1
    }
    scanText(handle.subarray(start, end).toString('utf8'), path, hits)
    start = end
  }
}

function countLines(dir) {
  let lines = 0
  for (const path of listLogFiles(dir)) {
    const text = readFileSync(path, 'utf8')
    lines += text.split(/\r?\n/).filter((line) => line.trim().length > 0).length
  }
  return lines
}

function selfTest() {
  const failures = []
  const mustHit = [
    '{"note":"key sk-live0123456789abcdefghijklmn"}',
    '{"authorization":"Bearer abcdef0123456789abcdef"}',
    '{"api_key":"abcdef1234567890abcdef"}',
    '{"note":"github ghp_0123456789abcdefghijABCDEF"}',
  ]
  const mustNotHit = [
    '{"note":"key sk-prob***0000"}',
    '{"api_key":"***"}',
    '{"api_key":"sk-prob***0000"}',
    '{"message":"握手完成","level":"INFO"}',
    '{"note":"task-manager 与 sk-1 不算密钥"}',
  ]

  for (const line of mustHit) {
    const hits = []
    scanText(line, 'sample.jsonl', hits)
    if (hits.length === 0) failures.push(`应命中却漏报：${line}`)
  }
  for (const line of mustNotHit) {
    const hits = []
    scanText(line, 'sample.jsonl', hits)
    if (hits.length > 0) failures.push(`不应命中却误报：${line} → ${hits[0].kind}`)
  }

  if (failures.length > 0) {
    for (const item of failures) console.error(`FAIL  ${item}`)
    process.exit(1)
  }
  console.log(
    `PASS  检测器自检：${mustHit.length} 条正样本全部命中，${mustNotHit.length} 条脱敏形态全部放行`,
  )
}

function main() {
  const args = process.argv.slice(2)
  if (args.includes('--self-test')) {
    selfTest()
    return
  }

  const requireFiles = args.includes('--require-files')
  const dir = args.find((arg) => !arg.startsWith('--'))
  if (!dir) {
    console.error('用法：node scripts/scan-log-secrets.mjs <日志目录> [--require-files]')
    process.exit(2)
  }

  const files = listLogFiles(dir)
  if (files.length === 0) {
    if (requireFiles) {
      console.error(`FAIL  ${dir} 下没有任何日志文件——门禁没扫到东西，视为失败`)
      process.exit(1)
    }
    console.log(`SKIP  ${dir} 下没有日志文件`)
    return
  }

  const hits = []
  for (const file of files) scanFile(file, hits)

  if (hits.length > 0) {
    for (const hit of hits.slice(0, 20)) {
      console.error(`FAIL  ${hit.file}:${hit.line} 命中「${hit.kind}」→ ${hit.sample}`)
    }
    if (hits.length > 20) console.error(`FAIL  另有 ${hits.length - 20} 条未显示`)
    process.exit(1)
  }

  const lines = countLines(dir)
  if (requireFiles && lines === 0) {
    console.error('FAIL  日志文件存在但一行都没有——门禁取样失败')
    process.exit(1)
  }
  console.log(`PASS  ${files.length} 个日志文件 / ${lines} 行，未发现明文凭据`)
}

main()
