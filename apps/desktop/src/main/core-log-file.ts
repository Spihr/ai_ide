/**
 * 内核日志落盘（P5 切片八 B6）：把「内核 stderr」与「内核事件通知」追加到
 * `<userData>/logs/core-shell-<day>.log`。
 *
 * 为什么外壳还要再记一份，而不是只依赖内核自己的 `core-*.jsonl`：
 * 内核的日志从内核启动、装了订阅器那一刻才开始写，`spawn` 失败、握手超时、
 * 协议错帧这些**外壳先看到**的事故，内核根本没机会记；反过来，外壳把内核
 * stderr 原样吞掉的话，用户点「内核异常」时手里只剩一句 `failed`，无从排查。
 * 两份日志的分工：`core-*.jsonl` 是内核内部的结构化事实，`core-shell-*.log`
 * 是外壳视角的原始字节流（含启动期）。
 *
 * 前缀刻意取 `core-shell` 而不是 `core`：同目录下 `core-<day>.jsonl` 已归内核，
 * 文件名撞车会让人以为是同一条链路写了两种格式。
 *
 * 写盘策略：**串行 + 追加 + 按天分文件 + 单文件超限滚动**。串行是必须的——
 * stderr 与事件通知来自不同回调，并发 `appendFile` 会让两行交错；上限是为了
 * 让「开着窗口挂一整天」不至于把磁盘写满。
 */

import { appendFile, mkdir, stat } from 'node:fs/promises'
import { join } from 'node:path'

/** 文件名前缀：与内核的 `core-*.jsonl` 区分开。 */
export const CORE_LOG_PREFIX = 'core-shell'

/** 单文件滚动上限（8 MiB）：超过就换 `.1` / `.2` 序号文件。 */
export const DEFAULT_MAX_BYTES = 8 * 1024 * 1024

/** 同日最多换几份就放弃滚动（防止上限被设成极小值时死循环）。 */
const MAX_ROLLS = 1000

export interface CoreLogSinkOptions {
  /** 日志目录（通常是 `<userData>/logs`）。 */
  dir: string
  /** 单文件上限，默认 {@link DEFAULT_MAX_BYTES}。 */
  maxBytes?: number
  /** 时间源（测试注入用）。 */
  now?: () => Date
}

export interface CoreLogSink {
  /** 追加一段文本（自动补齐换行）；错误内部记账，不抛出。 */
  append: (text: string) => void
  /** 等队列排空。 */
  flush: () => Promise<void>
  /** 最近一次落盘的完整路径（还没写过时是「今天的第一份」的预测路径）。 */
  currentPath: () => string
  /** 目录。 */
  directory: () => string
}

/** 本地日期 → `YYYY-MM-DD`。 */
function dayStamp(at: Date): string {
  const year = at.getFullYear()
  const month = `${at.getMonth() + 1}`.padStart(2, '0')
  const day = `${at.getDate()}`.padStart(2, '0')
  return `${year}-${month}-${day}`
}

async function sizeOf(path: string): Promise<number> {
  try {
    return (await stat(path)).size
  } catch {
    // 文件还不存在（第一次写今天这一份）：大小当 0，不算错误。
    return 0
  }
}

export function createCoreLogSink(options: CoreLogSinkOptions): CoreLogSink {
  const dir = options.dir
  const maxBytes = options.maxBytes ?? DEFAULT_MAX_BYTES
  const now = options.now ?? ((): Date => new Date())

  // 队列：`append` 只入队，真正的读写按入队顺序串行执行。
  let queue: Promise<void> = Promise.resolve()
  let lastPath = ''

  const fileFor = (at: Date, index: number): string => {
    const suffix = index === 0 ? '' : `.${index}`
    return join(dir, `${CORE_LOG_PREFIX}-${dayStamp(at)}${suffix}.log`)
  }

  const writeOnce = async (text: string): Promise<void> => {
    await mkdir(dir, { recursive: true })
    const at = now()
    const bytes = Buffer.byteLength(text, 'utf8')

    let index = 0
    let target = fileFor(at, index)
    for (;;) {
      const size = await sizeOf(target)
      // 空文件一律写：上限被设得比一行还小时，至少保证这一行落得下去。
      if (size === 0 || size + bytes <= maxBytes || index >= MAX_ROLLS) break
      index += 1
      target = fileFor(at, index)
    }

    await appendFile(target, text, 'utf8')
    lastPath = target
  }

  const append = (text: string): void => {
    if (text.length === 0) return
    const normalized = text.endsWith('\n') ? text : `${text}\n`
    queue = queue.then(() => writeOnce(normalized)).catch(() => undefined)
  }

  return {
    append,
    flush: async (): Promise<void> => {
      await queue
    },
    currentPath: (): string => (lastPath.length > 0 ? lastPath : fileFor(now(), 0)),
    directory: (): string => dir
  }
}
