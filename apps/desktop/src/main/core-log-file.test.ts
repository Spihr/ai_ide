/**
 * `core-log-file` 的单元测试：命名、追加、滚动与串行。
 *
 * 这里只测**文件系统行为**，不碰 Electron：`createCoreLogSink` 之所以把目录与
 * 时间源做成入参，就是为了能在 Node 里跑这一层。目录用 `mkdtemp`，绝不复用
 * 用户的真实日志目录——测试往里写脏数据比不测更糟。
 */

import { mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { afterEach, describe, expect, it } from 'vitest'

import { CORE_LOG_PREFIX, createCoreLogSink } from './core-log-file'

const dirs: string[] = []

async function makeDir(): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), 'ai-ide-core-log-'))
  dirs.push(dir)
  return dir
}

afterEach(async () => {
  while (dirs.length > 0) {
    const dir = dirs.pop()
    if (dir) await rm(dir, { recursive: true, force: true })
  }
})

describe('createCoreLogSink', () => {
  it('按天分文件，文件名带 core-shell 前缀', async () => {
    const dir = await makeDir()
    const at = new Date(2026, 9, 26, 10, 0, 0)
    const sink = createCoreLogSink({ dir, now: () => at })

    sink.append('第一行')
    await sink.flush()

    expect(sink.currentPath()).toBe(join(dir, `${CORE_LOG_PREFIX}-2026-10-26.log`))
    expect(await readFile(sink.currentPath(), 'utf8')).toBe('第一行\n')
    expect(await readdir(dir)).toEqual(['core-shell-2026-10-26.log'])
  })

  it('多次 append 按调用顺序追加，不会互相覆盖', async () => {
    const dir = await makeDir()
    const sink = createCoreLogSink({ dir, now: () => new Date(2026, 9, 26) })

    // 连发不等 await：队列必须保证顺序，否则 stderr 与事件通知会交错。
    sink.append('a\n')
    sink.append('b')
    sink.append('c\n')
    await sink.flush()

    expect(await readFile(sink.currentPath(), 'utf8')).toBe('a\nb\nc\n')
  })

  it('超过单文件上限后滚到序号文件', async () => {
    const dir = await makeDir()
    const sink = createCoreLogSink({ dir, maxBytes: 8, now: () => new Date(2026, 9, 26) })

    sink.append('1234567') // 8 字节（含换行）：正好占满第一份
    sink.append('x') // 再写就要滚到 .1
    await sink.flush()

    const files = (await readdir(dir)).sort()
    expect(files).toEqual(['core-shell-2026-10-26.1.log', 'core-shell-2026-10-26.log'])
    expect(await readFile(join(dir, 'core-shell-2026-10-26.log'), 'utf8')).toBe('1234567\n')
    expect(await readFile(join(dir, 'core-shell-2026-10-26.1.log'), 'utf8')).toBe('x\n')
  })

  it('已有的空文件不会触发空滚动（首写仍落在第一份）', async () => {
    const dir = await makeDir()
    await writeFile(join(dir, 'core-shell-2026-10-26.log'), '', 'utf8')
    const sink = createCoreLogSink({ dir, maxBytes: 4, now: () => new Date(2026, 9, 26) })

    sink.append('abc')
    await sink.flush()

    expect(await readdir(dir)).toEqual(['core-shell-2026-10-26.log'])
    expect(await readFile(join(dir, 'core-shell-2026-10-26.log'), 'utf8')).toBe('abc\n')
  })

  it('目录不存在时自动创建', async () => {
    const root = await makeDir()
    const dir = join(root, 'nested', 'logs')
    const sink = createCoreLogSink({ dir, now: () => new Date(2026, 9, 26) })

    sink.append('x')
    await sink.flush()

    expect(await readFile(sink.currentPath(), 'utf8')).toBe('x\n')
  })
})
