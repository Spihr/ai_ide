/**
 * `plugin-view` 的单元测试：V-4（三档限幅）、V-5（未知控件）、
 * V-6（命令未声明）、V-7（危险协议字符串）。
 *
 * 全部跑在 node 环境、不需要 DOM——这正是把判定抽成纯函数的目的：
 * 限幅逻辑不该为了可测而引入渲染测试栈。
 */

import { describe, expect, it } from 'vitest'

import {
  MAX_VIEW_BYTES,
  MAX_VIEW_DEPTH,
  MAX_VIEW_NODES,
  MAX_VIEW_ROWS,
  MAX_VIEW_STRING,
  VIEW_CONTROLS,
  VIEW_SCHEMA,
  actionState,
  isUnsafeText,
  parseView,
  type ViewNode
} from './plugin-view'

/** 造一棵合法的视图描述。 */
function envelope(root: unknown, rev = 1): unknown {
  return { schema: VIEW_SCHEMA, rev, root }
}

/** 深度为 `depth` 的 row 链，用于撞深度闸门。 */
function chain(depth: number): unknown {
  let node: unknown = { type: 'text', value: 'leaf' }
  for (let index = 1; index < depth; index += 1) {
    node = { type: 'column', children: [node] }
  }
  return node
}

function issueCodes(parsed: ReturnType<typeof parseView>): string[] {
  return parsed.issues.map((issue) => issue.code)
}

describe('parseView · 协议与根节点', () => {
  it('接受合法描述并归一 rev', () => {
    const parsed = parseView(envelope({ type: 'text', value: 'hi' }, 7))
    expect(parsed.status).toBe('ok')
    expect(parsed.rev).toBe(7)
    expect(parsed.error).toBeNull()
    expect(parsed.root?.type).toBe('text')
    expect(parsed.root?.value).toBe('hi')
    expect(parsed.nodeCount).toBe(1)
  })

  it('schema 不符直接拒渲染', () => {
    const parsed = parseView({ schema: 'aiide.view/2', rev: 1, root: { type: 'text' } })
    expect(parsed.status).toBe('rejected')
    expect(parsed.root).toBeNull()
    expect(issueCodes(parsed)).toContain('schema_mismatch')
    expect(parsed.error).toContain('aiide.view/2')
  })

  it('非对象与缺 root 都拒渲染而不抛', () => {
    expect(parseView(null).status).toBe('rejected')
    expect(parseView('oops').status).toBe('rejected')
    const parsed = parseView({ schema: VIEW_SCHEMA, rev: 1 })
    expect(parsed.status).toBe('rejected')
    // 根节点缺失也不能让渲染层拿到半棵树。
    expect(parsed.root).toBeNull()
  })

  it('rev 非数字回落 0', () => {
    const parsed = parseView({ schema: VIEW_SCHEMA, rev: 'x', root: { type: 'text' } })
    expect(parsed.rev).toBe(0)
  })
})

describe('parseView · V-4 限幅', () => {
  it('体积超 256 KiB 整棵拒渲染', () => {
    const big = 'x'.repeat(MAX_VIEW_BYTES)
    const parsed = parseView(envelope({ type: 'text', value: big }))
    expect(parsed.status).toBe('rejected')
    expect(issueCodes(parsed)).toContain('too_large')
  })

  it('字节按 UTF-8 计，中文不会被当 ASCII 少算', () => {
    // 每个汉字 3 字节：字符数只有 1/3 上限时，字节数就已经越线。
    const chinese = '汉'.repeat(Math.floor(MAX_VIEW_BYTES / 2))
    const parsed = parseView(envelope({ type: 'text', value: chinese }))
    expect(parsed.status).toBe('rejected')
    expect(issueCodes(parsed)).toContain('too_large')
  })

  it('节点总数超 2000 截断并留诊断', () => {
    const children = Array.from({ length: MAX_VIEW_NODES + 50 }, () => ({ type: 'text', value: 'x' }))
    const parsed = parseView(envelope({ type: 'column', children }))
    expect(parsed.status).toBe('ok')
    expect(parsed.truncated).toBe(true)
    expect(parsed.nodeCount).toBeLessThanOrEqual(MAX_VIEW_NODES)
    expect(issueCodes(parsed)).toContain('too_many_nodes')
    // 根节点仍然可用：截断是「少显示」，不是「不显示」。
    expect(parsed.root?.children?.length).toBeLessThan(MAX_VIEW_NODES + 50)
  })

  it('深度超 16 的子树被丢弃，浅层内容保留', () => {
    const parsed = parseView(envelope(chain(MAX_VIEW_DEPTH + 6)))
    expect(parsed.status).toBe('ok')
    expect(parsed.truncated).toBe(true)
    expect(issueCodes(parsed)).toContain('too_deep')
    expect(parsed.root?.type).toBe('column')
  })

  it('恰好等于深度上限时正常渲染', () => {
    const parsed = parseView(envelope(chain(MAX_VIEW_DEPTH)))
    expect(parsed.status).toBe('ok')
    expect(parsed.truncated).toBe(false)
    expect(issueCodes(parsed)).not.toContain('too_deep')
  })

  it('单字符串超 4 KiB 截断', () => {
    const parsed = parseView(envelope({ type: 'text', value: 'y'.repeat(MAX_VIEW_STRING + 200) }))
    expect(parsed.status).toBe('ok')
    expect(parsed.root?.value).toHaveLength(MAX_VIEW_STRING)
    expect(issueCodes(parsed)).toContain('string_truncated')
  })

  it('table 行数超 500 截断', () => {
    const rows = Array.from({ length: MAX_VIEW_ROWS + 20 }, (_unused, index) => [`#${index}`])
    const parsed = parseView(
      envelope({ type: 'table', columns: ['名称'], rows })
    )
    expect(parsed.root?.rows).toHaveLength(MAX_VIEW_ROWS)
    expect(issueCodes(parsed)).toContain('row_truncated')
    expect(parsed.truncated).toBe(true)
  })

  it('list 条目超 500 截断', () => {
    const items = Array.from({ length: MAX_VIEW_ROWS + 5 }, (_unused, index) => ({
      key: `k${index}`,
      label: `第 ${index} 条`
    }))
    const parsed = parseView(envelope({ type: 'list', items }))
    expect(parsed.root?.items).toHaveLength(MAX_VIEW_ROWS)
    expect(issueCodes(parsed)).toContain('row_truncated')
  })

  it('诊断条数封顶，避免刷满面板', () => {
    const children = Array.from({ length: 300 }, () => ({ type: 'text', value: 'z'.repeat(MAX_VIEW_STRING + 1) }))
    const parsed = parseView(envelope({ type: 'column', children }))
    expect(parsed.issues.length).toBeLessThanOrEqual(32)
  })
})

describe('parseView · V-5 未知控件', () => {
  it('未知 type 不抛异常，保留占位并记一次诊断', () => {
    const parsed = parseView(
      envelope({
        type: 'column',
        children: [
          { type: 'webview', value: 'https://evil.example' },
          { type: 'webview', value: 'https://evil.example/2' },
          { type: 'text', value: 'ok' }
        ]
      })
    )
    expect(parsed.status).toBe('ok')
    const unknown = parsed.root?.children?.[0]
    expect(unknown?.known).toBe(false)
    expect(unknown?.type).toBe('webview')
    // 同一种未知类型只报一次，剩下的只做占位。
    expect(issueCodes(parsed).filter((code) => code === 'unknown_type')).toHaveLength(1)
  })

  it('缺 type 的节点被跳过而不是让整棵树失败', () => {
    const parsed = parseView(
      envelope({ type: 'column', children: [{ value: 'no type' }, { type: 'text', value: 'ok' }] })
    )
    expect(parsed.status).toBe('ok')
    expect(parsed.root?.children).toHaveLength(1)
    expect(parsed.root?.children?.[0]?.value).toBe('ok')
  })

  it('白名单控件全部被识别', () => {
    const types = ['row', 'column', 'group', 'tabs', 'text', 'stat', 'table', 'list', 'tree', 'progress', 'toggle', 'slider', 'select', 'color', 'button']
    const children = types.map((type) => ({ type, label: type }))
    const parsed = parseView(envelope({ type: 'column', children }))
    expect(parsed.root?.children?.every((child) => child.known)).toBe(true)
    expect(issueCodes(parsed)).not.toContain('unknown_type')
  })
})

describe('parseView · V-7 危险协议字符串', () => {
  it('识别 javascript/data/file/http 五种协议', () => {
    expect(isUnsafeText('javascript:alert(1)')).toBe(true)
    expect(isUnsafeText('JAVASCRIPT:alert(1)')).toBe(true)
    expect(isUnsafeText('data:text/html,<script>')).toBe(true)
    expect(isUnsafeText('file:///C:/Windows/win.ini')).toBe(true)
    expect(isUnsafeText('https://example.com/a.png')).toBe(true)
    expect(isUnsafeText('看这条 javascript: 说明')).toBe(true)
  })

  it('普通文本不误判', () => {
    expect(isUnsafeText('hello world')).toBe(false)
    expect(isUnsafeText('src/js/main.ts')).toBe(false)
    expect(isUnsafeText('C:\\Users\\neroy\\a.txt')).toBe(false)
    expect(isUnsafeText('100%')).toBe(false)
  })

  it('命中协议的节点被标记 tainted，内容原样保留（不做转义）', () => {
    const parsed = parseView(envelope({ type: 'text', value: 'javascript:alert(1)' }))
    expect(parsed.root?.tainted).toBe(true)
    expect(parsed.root?.value).toBe('javascript:alert(1)')
    expect(issueCodes(parsed)).toContain('unsafe_url')
  })

  it('table 单元格含协议时整行标记 tainted', () => {
    const parsed = parseView(
      envelope({
        type: 'table',
        columns: ['路径'],
        rows: [['ok.txt'], ['javascript:alert(1)']]
      })
    )
    expect(parsed.root?.rows?.[0]?.tainted).toBe(false)
    expect(parsed.root?.rows?.[1]?.tainted).toBe(true)
    expect(parsed.root?.tainted).toBe(true)
  })
})

describe('parseView · 形状容错', () => {
  it('table 行不是数组时跳过该行', () => {
    const parsed = parseView(
      envelope({ type: 'table', columns: ['a'], rows: ['oops', ['ok']] })
    )
    expect(parsed.root?.rows).toHaveLength(1)
    expect(issueCodes(parsed)).toContain('bad_shape')
  })

  it('progress 百分比被夹到 0–100', () => {
    const high = parseView(envelope({ type: 'progress', value: 143 }))
    expect(high.root?.progress).toBe(100)
    const low = parseView(envelope({ type: 'progress', props: { value: -7 } }))
    expect(low.root?.progress).toBe(0)
  })

  it('slider 缺 props 时给安全默认值', () => {
    const parsed = parseView(envelope({ type: 'slider' }))
    expect(parsed.root?.min).toBe(0)
    expect(parsed.root?.max).toBe(100)
    expect(parsed.root?.step).toBe(1)
    expect(parsed.root?.current).toBe('0')
  })

  it('color 只接受 hex，其他值丢弃', () => {
    const ok = parseView(envelope({ type: 'color', props: { value: '#1a2b3c' } }))
    expect(ok.root?.current).toBe('#1a2b3c')
    const bad = parseView(envelope({ type: 'color', props: { value: 'red; background:url(x)' } }))
    expect(bad.root?.current).toBeUndefined()
  })

  it('tabs 的 children 当内容节点，tree 的 children 当子项', () => {
    const tabs = parseView(
      envelope({
        type: 'tabs',
        items: [
          { key: 'a', label: '页一', children: [{ type: 'text', value: 'in tab' }] },
          { key: 'b', label: '页二' }
        ]
      })
    )
    expect(tabs.root?.items?.[0]?.content?.[0]?.value).toBe('in tab')
    expect(tabs.root?.items?.[0]?.children).toBeUndefined()

    const tree = parseView(
      envelope({
        type: 'tree',
        items: [{ key: 'root', label: '根', children: [{ key: 'leaf', label: '叶' }] }]
      })
    )
    expect(tree.root?.items?.[0]?.children?.[0]?.key).toBe('leaf')
  })

  it('action 缺 command 时被忽略', () => {
    const parsed = parseView(envelope({ type: 'button', label: '点我', action: { args: {} } }))
    expect(parsed.root?.action).toBeUndefined()
    expect(issueCodes(parsed)).toContain('bad_shape')
  })
})

describe('fixture 回归', () => {
  it('覆盖全部控件的样例快照能完整解析', async () => {
    const { PLUGIN_VIEW_FIXTURE, PLUGIN_VIEW_FIXTURE_COMMANDS } = await import('./plugin-view.fixture')
    const parsed = parseView(PLUGIN_VIEW_FIXTURE)
    expect(parsed.status).toBe('ok')
    expect(parsed.error).toBeNull()
    // 没有未知控件，也没有任何截断：样例本身就是干净的。
    expect(issueCodes(parsed)).not.toContain('unknown_type')
    expect(issueCodes(parsed)).not.toContain('too_many_nodes')
    expect(parsed.truncated).toBe(false)
    // 样例故意留了一条 javascript: 单元格，应当被标记而不是被放行。
    expect(issueCodes(parsed)).toContain('unsafe_url')

    // 所有控件类型都应出现过：夹具与白名单保持一致。
    const seen = new Set<string>()
    const walk = (node: ViewNode | undefined): void => {
      if (node === undefined) return
      seen.add(node.type)
      node.children?.forEach(walk)
      node.items?.forEach((item) => item.content?.forEach(walk))
    }
    walk(parsed.root ?? undefined)
    expect([...seen].sort()).toEqual([...VIEW_CONTROLS].sort())

    // 样例里有一条未声明的命令，宿主必须能判出来。
    const exportButton = parsed.root?.children?.[2]?.children?.[1]?.children?.[1]
    expect(exportButton?.action?.command).toBe('asset3d.export')
    expect(actionState(exportButton as ViewNode, PLUGIN_VIEW_FIXTURE_COMMANDS).enabled).toBe(false)
  })
})

describe('actionState · V-6 命令未声明', () => {
  function buttonView(command: string): ViewNode {
    const parsed = parseView(envelope({ type: 'button', label: '执行', action: { command } }))
    expect(parsed.root).not.toBeNull()
    return parsed.root as ViewNode
  }

  it('命令已声明 → 可点', () => {
    expect(actionState(buttonView('refresh'), ['refresh', 'export'])).toEqual({ enabled: true, reason: null })
  })

  it('命令未声明 → 禁用并给出原因', () => {
    const state = actionState(buttonView('shell.exec'), ['refresh'])
    expect(state.enabled).toBe(false)
    expect(state.reason).toContain('shell.exec')
  })

  it('清单未读到（commands 为空）时不误禁，交给网关报错', () => {
    expect(actionState(buttonView('refresh'), []).enabled).toBe(true)
  })

  it('无动作的节点永远可点', () => {
    const parsed = parseView(envelope({ type: 'text', value: 'hi' }))
    expect(actionState(parsed.root as ViewNode, []).enabled).toBe(true)
  })
})
