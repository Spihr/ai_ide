import { describe, expect, it } from 'vitest'

import {
  describeExternalViewError,
  describeExternalViewExit,
  describeExternalViewOpened,
  hasExternalViewport
} from './plugin-external-view'

describe('describeExternalViewError', () => {
  it('按原因码给出下一步动作，而不是复述错误名', () => {
    const text = describeExternalViewError(
      new Error('外部视口程序指向安装目录之外：..\\evil.exe（code=program_escape）')
    )
    expect(text).toContain('已拒绝启动')
    expect(text).not.toContain('code=')
  })

  it('能穿透 IPC 包裹层（只认最后一个 code）', () => {
    const text = describeExternalViewError(
      new Error(
        "Error invoking remote method 'plugin:spawn-external-view': Error: 管道连不上（code=pipe_timeout）"
      )
    )
    expect(text).toContain('--pipe')
  })

  it('未知原因码原样返回，不编造解释', () => {
    expect(describeExternalViewError(new Error('boom（code=brand_new）'))).toBe('boom（code=brand_new）')
  })

  it('完全没有 code 时也原样返回', () => {
    expect(describeExternalViewError(new Error('pluginId 必须是非空字符串'))).toBe(
      'pluginId 必须是非空字符串'
    )
  })

  it('空错误给兜底文案', () => {
    expect(describeExternalViewError(new Error('   '))).toContain('未给原因')
  })

  it('非 Error 也能处理', () => {
    expect(describeExternalViewError('plain（code=program_missing）')).toContain('安装包可能不完整')
  })

  it('每个已知原因码都有文案（防新增码漏配）', () => {
    for (const code of [
      'install_missing',
      'program_invalid',
      'program_escape',
      'program_ext',
      'program_missing',
      'pipe_timeout',
      'spawn_failed',
      'protocol_unsupported',
      'plugin_invalid',
      'view_missing'
    ]) {
      expect(describeExternalViewError(new Error(`x（code=${code}）`))).not.toContain('code=')
    }
  })
})

describe('describeExternalViewOpened', () => {
  it('复用与新建文案不同', () => {
    expect(describeExternalViewOpened('模型', true)).toContain('复用')
    expect(describeExternalViewOpened('模型', false)).toContain('已启动')
  })
})

describe('hasExternalViewport', () => {
  it('只有声明 external 才算', () => {
    expect(hasExternalViewport({ external: { program: 'a.exe', protocol: '1' } })).toBe(true)
    expect(hasExternalViewport({})).toBe(false)
    expect(hasExternalViewport({ external: null })).toBe(false)
  })
})

describe('describeExternalViewExit', () => {
  it('退出码 0 = 用户自己关窗，不必指挥他排查', () => {
    const text = describeExternalViewExit('模型', 0)
    expect(text).toContain('已关闭')
    expect(text).not.toContain('日志')
  })

  it('非 0 = 崩溃，明确指向插件日志并说明可以重开', () => {
    const text = describeExternalViewExit('模型', 3221225477)
    expect(text).toContain('3221225477')
    expect(text).toContain('日志')
    expect(text).toContain('重开')
  })

  it('拿不到退出码（被信号终止）时如实说，不硬凑一个 0', () => {
    const text = describeExternalViewExit('模型', null)
    expect(text).toContain('终止')
    expect(text).not.toContain('退出码 0')
  })

  it('带上视图标题：同时开着两个插件视口时，用户要知道是哪一个关了', () => {
    expect(describeExternalViewExit('模型', 0)).toContain('「模型」')
  })
})
