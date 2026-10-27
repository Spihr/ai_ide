/**
 * 清单校验。
 *
 * 校验顺序刻意是「形状 → 语义 → 与授权交叉」，因为后一步依赖前一步的结果：
 * 形状不过的清单根本不该进入语义讨论（同 docs/03 §5 的协商顺序）。
 *
 * 一条硬规则：工具声明的 risk 必须被 capabilities 覆盖，
 * 否则会出现「声明了 write 工具却没有 fs.write 能力」的自相矛盾包，
 * 那种包在调用时只能要么绕过闸门、要么运行时炸，两种都不可接受——直接拒载。
 */

import { ErrorCodes, PluginError, isCompatible } from './protocol.mjs'

/** 能力白名单。不在表里的能力名一律拒（拒绝未知优于猜测语义）。 */
export const CAPABILITIES = new Set([
  'fs.read',
  'fs.write',
  'search',
  'progress',
  'net.fetch',
  'process.exec',
  'llm.invoke',
  'events.subscribe'
])

/** 工具风险等级 → 所需能力。read 类不需要授权（工作区只读是基线）。 */
const RISK_REQUIRES = {
  read: [],
  write: ['fs.write'],
  exec: ['process.exec']
}

const ID_RE = /^[a-z0-9][a-z0-9-]{1,63}$/
const TOOL_RE = /^[a-z][a-z0-9_]{1,40}$/
const SEMVER_RE = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/

/**
 * @returns {{ manifest: object, errors: string[], warnings: string[] }}
 */
export function validateManifest(raw) {
  const errors = []
  const warnings = []

  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
    return { manifest: null, errors: ['清单必须是 JSON 对象'], warnings }
  }

  const id = raw.id
  if (typeof id !== 'string' || !ID_RE.test(id)) {
    errors.push('id 必须匹配 ^[a-z0-9][a-z0-9-]{1,63}$')
  }
  if (typeof raw.name !== 'string' || raw.name.trim() === '') errors.push('name 必填')
  if (typeof raw.version !== 'string' || !SEMVER_RE.test(raw.version)) {
    errors.push('version 必须是语义化版本 x.y.z')
  }
  if (typeof raw.pluginApi !== 'string') {
    errors.push('pluginApi 必填，形如 "1.0"')
  } else {
    const [major, minor] = raw.pluginApi.split('.').map((n) => Number.parseInt(n, 10))
    if (!Number.isInteger(major) || !Number.isInteger(minor)) {
      errors.push('pluginApi 必须形如 "1.0"')
    } else if (!isCompatible({ major, minor }, { major: 1, minor: 0 })) {
      errors.push(`pluginApi ${raw.pluginApi} 与宿主 1.0 不兼容`)
    } else if (major === 1 && minor > 0) {
      warnings.push(`pluginApi 1.${minor} 高于宿主 1.0，按 1.0 能力集降级运行`)
    }
  }

  // 入口必须是包内相对路径：允许 `index.mjs`，拒绝 `../other/index.mjs` 与绝对路径。
  if (typeof raw.main !== 'string' || raw.main.trim() === '') {
    errors.push('main 必填')
  } else if (/^[a-zA-Z]:|^\//.test(raw.main) || raw.main.split(/[\\/]/).includes('..')) {
    errors.push('main 必须是包内相对路径，不得越出插件目录')
  }

  const capabilities = Array.isArray(raw.capabilities) ? raw.capabilities : []
  if (raw.capabilities !== undefined && !Array.isArray(raw.capabilities)) {
    errors.push('capabilities 必须是数组')
  }
  for (const cap of capabilities) {
    if (typeof cap !== 'string' || !CAPABILITIES.has(cap)) {
      errors.push(`未知能力：${JSON.stringify(cap)}`)
    }
  }

  const tools = Array.isArray(raw.tools) ? raw.tools : []
  if (raw.tools !== undefined && !Array.isArray(raw.tools)) errors.push('tools 必须是数组')
  const seenTools = new Set()
  const normalizedTools = []
  for (const tool of tools) {
    if (!tool || typeof tool !== 'object') {
      errors.push('tools[] 每项必须是对象')
      continue
    }
    if (typeof tool.name !== 'string' || !TOOL_RE.test(tool.name)) {
      errors.push(`工具名非法：${JSON.stringify(tool.name)}（须匹配 ^[a-z][a-z0-9_]{1,40}$）`)
      continue
    }
    if (seenTools.has(tool.name)) {
      errors.push(`工具名在包内重复：${tool.name}`)
      continue
    }
    seenTools.add(tool.name)

    const risk = tool.risk ?? 'read'
    if (!RISK_REQUIRES[risk]) {
      errors.push(`工具 ${tool.name} 的 risk 非法：${JSON.stringify(tool.risk)}`)
      continue
    }
    const missing = RISK_REQUIRES[risk].filter((cap) => !capabilities.includes(cap))
    if (missing.length > 0) {
      errors.push(
        `工具 ${tool.name} 声明 risk=${risk} 但缺能力 ${missing.join(', ')}——清单自相矛盾`
      )
    }
    if (typeof tool.description !== 'string' || tool.description.trim() === '') {
      warnings.push(`工具 ${tool.name} 缺 description，模型将无法判断何时调用`)
    }
    const schema = tool.parameters
    if (!schema || typeof schema !== 'object' || schema.type !== 'object') {
      errors.push(`工具 ${tool.name} 的 parameters 必须是 {type:"object", properties:{}}`)
    }

    normalizedTools.push({
      name: tool.name,
      description: tool.description ?? '',
      parameters: schema,
      risk,
      /** 审批 scope 用的路径参数名，缺省 null 表示该工具不涉及具体文件 */
      pathField: typeof tool.pathField === 'string' ? tool.pathField : null
    })
  }

  const commands = Array.isArray(raw.commands) ? raw.commands : []
  for (const cmd of commands) {
    // 与内核一致：命令名的规范键是 name，id 只作为老包兼容。
    const commandName = cmd?.name ?? cmd?.id
    if (typeof commandName !== 'string' || !TOOL_RE.test(commandName)) {
      errors.push(`命令 name 非法：${JSON.stringify(commandName)}（须匹配 ${TOOL_RE}）`)
    }
  }

  if (errors.length > 0) {
    return { manifest: null, errors, warnings }
  }

  return {
    manifest: {
      id: raw.id,
      name: raw.name,
      version: raw.version,
      description: raw.description ?? '',
      pluginApi: raw.pluginApi,
      main: raw.main,
      capabilities,
      tools: normalizedTools,
      // 内部仍用 id 做键（命令面板按 id 触发），但来源统一取 name ?? id。
      commands: commands.map((c) => {
        const commandName = c.name ?? c.id
        return { id: commandName, name: commandName, title: c.title ?? commandName }
      }),
      subscriptions: Array.isArray(raw.subscriptions) ? raw.subscriptions : []
    },
    errors,
    warnings
  }
}

export function assertManifest(manifest) {
  if (!manifest) throw new PluginError(ErrorCodes.PLUGIN_MANIFEST_INVALID, '清单缺失')
  return manifest
}
