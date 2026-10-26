/**
 * `core-log-store` 的单元测试：切行、残行拼接、上限与清空。
 *
 * 状态是模块级单例（`useSyncExternalStore` 需要），所以每个用例前必须
 * `resetCoreLog()`——否则上个用例的残行会污染下一个用例的首行。
 */

import { beforeEach, describe, expect, it } from 'vitest'

import type { CoreLogChunk } from '../../shared/ipc'

import {
  MAX_CORE_LOG_LINES,
  clearCoreLog,
  getCoreLogState,
  receiveCoreLog,
  resetCoreLog
} from './core-log-store'

function chunk(text: string, stream: CoreLogChunk['stream'] = 'stderr'): CoreLogChunk {
  return { stream, text }
}

beforeEach(() => {
  resetCoreLog()
})

describe('receiveCoreLog', () => {
  it('按行切分，最新的在最前', () => {
    receiveCoreLog(chunk('a\nb\nc\n'))
    expect(getCoreLogState().lines.map((line) => line.text)).toEqual(['c', 'b', 'a'])
  })

  it('末尾残行留到下一块拼完再入列表', () => {
    receiveCoreLog(chunk('第一段'))
    expect(getCoreLogState().lines).toEqual([])

    receiveCoreLog(chunk('接上\n第二行\n'))
    expect(getCoreLogState().lines.map((line) => line.text)).toEqual(['第二行', '第一段接上'])
  })

  it('Windows 换行去掉行尾 \\r', () => {
    receiveCoreLog(chunk('a\r\nb\r\n'))
    expect(getCoreLogState().lines.map((line) => line.text)).toEqual(['b', 'a'])
  })

  it('空行被丢弃，但不会打断其它行', () => {
    receiveCoreLog(chunk('a\n\n   \nb\n'))
    expect(getCoreLogState().lines.map((line) => line.text)).toEqual(['b', 'a'])
  })

  it('超过上限时丢最旧的，并累计 dropped', () => {
    const many = Array.from({ length: MAX_CORE_LOG_LINES + 5 }, (_item, index) => `l${index}`)
    receiveCoreLog(chunk(`${many.join('\n')}\n`))

    const state = getCoreLogState()
    expect(state.lines).toHaveLength(MAX_CORE_LOG_LINES)
    expect(state.lines[0]?.text).toBe(`l${MAX_CORE_LOG_LINES + 4}`)
    expect(state.dropped).toBe(5)
  })

  it('保留来源标记：stderr 与 event 各自可辨', () => {
    receiveCoreLog(chunk('来自 stderr\n'))
    receiveCoreLog(chunk('[warn] core: 事件\n', 'event'))

    expect(getCoreLogState().lines.map((line) => line.stream)).toEqual(['event', 'stderr'])
  })

  it('切换来源时丢弃上一段残行（不让两路输出粘成一行）', () => {
    receiveCoreLog(chunk('stderr 半行'))
    receiveCoreLog(chunk('事件全文\n', 'event'))

    expect(getCoreLogState().lines.map((line) => line.text)).toEqual(['事件全文'])
  })

  it('空文本与未知载荷不影响状态', () => {
    receiveCoreLog(chunk(''))
    receiveCoreLog({ stream: 'stderr', text: undefined } as unknown as CoreLogChunk)
    expect(getCoreLogState().lines).toEqual([])
  })

  it('clearCoreLog 清空列表并归零 dropped', () => {
    receiveCoreLog(chunk('a\nb\n'))
    clearCoreLog()
    expect(getCoreLogState()).toEqual({ lines: [], dropped: 0 })
  })
})
