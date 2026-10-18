#!/usr/bin/env node
/**
 * 协议类型的生成与同步。
 *
 * 为什么要有这个脚本，而不是直接把 ts-rs 的输出目录指向 TypeScript 侧：
 *
 * ts-rs 在自己的 `bindings/` 目录里生成一整棵自洽的目录树——类型之间用
 * **相对路径**互相 import，`serde_json::Value` 这类内置映射还会落到
 * `bindings/serde_json/` 子目录。如果只把其中一部分类型指到别处，
 * 相对 import 就会指错地方。
 *
 * 所以这里的做法是：让 ts-rs 按默认布局生成，然后**整棵树搬家**到
 * TypeScript 侧。整体搬运不改变相对结构，import 天然保持正确。
 *
 * 用法：pnpm types:gen
 */

import { spawnSync } from 'node:child_process'
import { cpSync, existsSync, mkdirSync, rmSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const bindingsDir = join(repoRoot, 'crates', 'protocol', 'bindings')
const targetDir = join(repoRoot, 'packages', 'shared-types', 'src', 'generated')

const cargo = spawnSync(
  'cargo',
  ['test', '-p', 'ai-ide-protocol', 'export_bindings'],
  { cwd: repoRoot, stdio: 'inherit' }
)

if (cargo.error) {
  console.error(`无法执行 cargo：${cargo.error.message}`)
  process.exit(1)
}
if (cargo.status !== 0) {
  console.error('ts-rs 导出失败，已保留上一份生成结果')
  process.exit(cargo.status ?? 1)
}
if (!existsSync(bindingsDir)) {
  console.error(`ts-rs 没有产出目录：${bindingsDir}`)
  process.exit(1)
}

rmSync(targetDir, { recursive: true, force: true })
mkdirSync(dirname(targetDir), { recursive: true })
cpSync(bindingsDir, targetDir, { recursive: true })
rmSync(bindingsDir, { recursive: true, force: true })

console.log(`协议类型已同步：${bindingsDir}  ->  ${targetDir}`)
