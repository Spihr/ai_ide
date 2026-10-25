/**
 * 快捷键映射的测试。
 *
 * 这层是 `Ctrl+T` 能被按出来的唯一保证：组件里只剩 `switch`，组合键的判定顺序全在
 * `resolveShortcut` 里。所以这里既钉「Ctrl+T → symbols」，也钉住几组**不能互相抢**
 * 的相邻组合（`Ctrl+P` / `Ctrl+Shift+P`、`Ctrl+S` / `Ctrl+Shift+S`）。
 */

import { describe, expect, it } from 'vitest'

import { resolveShortcut } from './shortcuts'

describe('快捷键解析', () => {
  it('Ctrl+T 触发「转到符号」', () => {
    expect(resolveShortcut({ key: 't', ctrlKey: true })).toBe('symbols')
  })

  it('大写 T 同样命中（真实 KeyboardEvent 里 shift 会把 key 归一成大写）', () => {
    expect(resolveShortcut({ key: 'T', ctrlKey: true })).toBe('symbols')
  })

  it('Cmd+T（macOS）等价于 Ctrl+T', () => {
    expect(resolveShortcut({ key: 't', metaKey: true })).toBe('symbols')
  })

  it('Ctrl+Shift+T 不吞键（留给后续的工作区符号 / 重开标签）', () => {
    expect(resolveShortcut({ key: 'T', ctrlKey: true, shiftKey: true })).toBeNull()
  })

  it('没有主修饰键时一律不命中', () => {
    expect(resolveShortcut({ key: 't' })).toBeNull()
    expect(resolveShortcut({ key: 'p' })).toBeNull()
  })

  it('已有快捷键的映射不被 Ctrl+T 影响', () => {
    expect(resolveShortcut({ key: 's', ctrlKey: true })).toBe('save')
    expect(resolveShortcut({ key: 'p', ctrlKey: true })).toBe('search')
    expect(resolveShortcut({ key: 'P', ctrlKey: true, shiftKey: true })).toBe('command-palette')
    expect(resolveShortcut({ key: 'b', ctrlKey: true })).toBe('explorer')
    expect(resolveShortcut({ key: '`', ctrlKey: true })).toBe('terminal')
  })

  it('Ctrl+Shift+S 不被当成保存', () => {
    expect(resolveShortcut({ key: 'S', ctrlKey: true, shiftKey: true })).toBeNull()
  })
})
