/**
 * 路径闸门：插件拿不到裸文件系统，一切路径先过这里。
 *
 * 这是「第三道闸门」的 demo 实现，语义与 `crates/workspace/src/path.rs` 的
 * `PathGuard` 对齐：
 * - 只接受相对路径，绝对路径一律拒绝（插件不该知道工作区在哪）
 * - `..` 归一化后仍在根内才放行
 * - 敏感段（.git / .ssh / .env / credentials 等）拒写，读也拒
 *
 * 真实实现还要处理 Windows 的短名、UNC、符号链接与大小写不敏感比较，
 * demo 用 `path.resolve` + 前缀比较把主干语义跑通。
 */

import path from 'node:path'
import { ErrorCodes, PluginError } from './protocol.mjs'

/** 敏感路径段：命中即拒（沿用 docs/02 §8 与 PathGuard 的规则集）。 */
const DENIED_SEGMENTS = new Set([
  '.git',
  '.svn',
  '.ssh',
  '.aws',
  '.kube',
  '.env',
  'credentials.json',
  'providers.json'
])

export class PathGuard {
  constructor(root) {
    this.root = path.resolve(root)
  }

  /**
   * 把插件给的相对路径解析成工作区内的绝对路径。
   * @throws {PluginError} 1002 越界 / 1003 敏感路径
   */
  resolve(relative, { forWrite = false } = {}) {
    if (typeof relative !== 'string' || relative.trim() === '') {
      throw new PluginError(ErrorCodes.INVALID_PARAMS, 'path 必须是非空字符串')
    }
    const raw = relative.replace(/\\/g, '/')

    // 绝对路径（含盘符）直接拒：插件没有「知道工作区绝对位置」的理由。
    if (raw.startsWith('/') || /^[a-zA-Z]:/.test(raw)) {
      throw new PluginError(ErrorCodes.PATH_OUT_OF_WORKSPACE, `只接受工作区相对路径：${relative}`)
    }
    if (raw.split('/').some((seg) => seg === '..')) {
      throw new PluginError(ErrorCodes.PATH_OUT_OF_WORKSPACE, `路径含 .. 跳转：${relative}`)
    }

    const abs = path.resolve(this.root, raw)
    const prefix = this.root.endsWith(path.sep) ? this.root : this.root + path.sep
    if (abs !== this.root && !abs.startsWith(prefix)) {
      throw new PluginError(ErrorCodes.PATH_OUT_OF_WORKSPACE, `路径越出工作区：${relative}`)
    }

    const relativeSegments = path.relative(this.root, abs).split(path.sep).filter(Boolean)
    for (const seg of relativeSegments) {
      if (DENIED_SEGMENTS.has(seg.toLowerCase()) || seg.toLowerCase().startsWith('.env')) {
        throw new PluginError(
          ErrorCodes.PATH_DENIED,
          `${forWrite ? '拒写' : '拒读'}敏感路径：${relative}`
        )
      }
    }
    return abs
  }

  /** 展示路径：统一 `/` 分隔，避免模型拿到 `src\\a.ts` 后在 JSON 里转义出错。 */
  display(abs) {
    const rel = path.relative(this.root, abs)
    return rel === '' ? '.' : rel.split(path.sep).join('/')
  }
}
