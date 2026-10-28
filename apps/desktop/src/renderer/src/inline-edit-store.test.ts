/**
 * Inline Edit（`Ctrl+K`）外壳侧状态机的测试。
 *
 * 覆盖的是「只有真按键盘才暴露」的那五件事，也正是验收点：
 *
 * 1. **快捷键触发**：`Ctrl+K` 解析成 `inline-edit`，且不抢已有组合键。
 * 2. **无选区只提示**：不打开浮层、不发 `inline_edit.propose`——空浮层等于让用户
 *    自己去猜为什么点了没反应。
 * 3. **接受才写盘**：`accept` 走 `workspace.write_file`，内容是按列拼回原文的结果。
 * 4. **拒绝不写盘**：`reject` 之后一次写请求都没有，文件内容仍是原样。
 * 5. **9001 / 9003 是人话**：错误码不能原样甩给用户，浮层要留着让他改指令再来。
 *
 * 不渲染 React：状态都在模块级单例里，动作也都在 store 上（组件只做展示与转发），
 * 所以这里直接读快照断言——与 `symbol-picker-store.test.ts` 同一套写法。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import {
  Methods,
  type InlineEditProposeResult,
  type TextSelection,
  type TreeEntry,
  type WorkspaceOpenResult,
  type WorkspaceReadFileResult
} from '@ai-ide/shared-types'

import { RPC_ERROR_PAYLOAD_MARKER, type AiIdeApi } from '../../shared/ipc'
import {
  INLINE_EDIT_ERROR_TEXT,
  describeInlineEditError,
  inlineEditRangeLabel,
  spliceSelection,
  toTextSelection
} from './inline-edit'
import {
  NO_SELECTION_HINT,
  acceptInlineEdit,
  getInlineEditState,
  openInlineEdit,
  rejectInlineEdit,
  resetInlineEditState,
  setInlineEditInstruction,
  submitInlineEdit
} from './inline-edit-store'
import { resolveShortcut } from './shortcuts'
import { getEditorState, openFile, openWorkspace, setEditorSelection } from './store'

const ROOT = 'C:\\ws'
const FILE_A = `${ROOT}\\a.ts`
const CONTENT = 'function f() {\n  return 1\n}\n'

const RANGE: TextSelection = {
  start_line: 2,
  start_column: 2,
  end_line: 2,
  end_column: 10
}
const PROPOSAL: InlineEditProposeResult = {
  path: 'a.ts',
  range: RANGE,
  original: 'return 1',
  replacement: 'return 0',
  diff: '--- a/a.ts\n+++ b/a.ts\n@@ -2 +2 @@\n-  return 1\n+  return 0\n',
  usage: null
}

interface Call {
  method: string
  params: Record<string, unknown> | undefined
}

let calls: Call[] = []
let proposeError: Error | null = null
let proposal: InlineEditProposeResult = PROPOSAL

/** 拼一条外壳认得的 RPC 错误：`decodeRpcErrorPayload` 靠人不可见的分隔符取 JSON。 */
function rpcError(code: number, message: string): Error {
  return new Error(`${message}${RPC_ERROR_PAYLOAD_MARKER}${JSON.stringify({ code })}`)
}

function installHarness(): void {
  calls = []
  proposeError = null
  proposal = PROPOSAL
  const entries: TreeEntry[] = [
    { name: 'a.ts', path: FILE_A, kind: 'file', size: CONTENT.length, mtime: 0, ignored: false }
  ]
  const read: WorkspaceReadFileResult = {
    content: CONTENT,
    encoding: 'utf-8',
    eol: 'lf',
    size: CONTENT.length,
    truncated: false,
    hash: 'h1'
  }
  const opened: WorkspaceOpenResult = {
    workspace_id: 'ws-1',
    root: ROOT,
    name: 'ws',
    stats: { files: 1, dirs: 0, ignored: 0 },
    indexing: false
  }

  const api = {
    getCoreStatus: async () => {
      throw new Error('未使用')
    },
    restartCore: async () => {
      throw new Error('未使用')
    },
    ping: async () => {
      throw new Error('未使用')
    },
    pickSavePath: async () => null,
    pickWorkspaceDirectory: async () => ROOT,
    request: async (method: string, params?: unknown) => {
      calls.push({ method, params: params as Record<string, unknown> | undefined })
      switch (method) {
        case Methods.workspace.open:
          return opened
        case Methods.workspace.tree:
          return { entries }
        case Methods.workspace.readFile:
          return read
        case Methods.inlineEdit.propose:
          if (proposeError !== null) throw proposeError
          return proposal
        case Methods.workspace.writeFile:
          return { ok: true, hash: 'h2' }
        default:
          return { ok: true }
      }
    },
    onCoreStatus: () => () => undefined,
    onCoreLog: () => () => undefined,
    onWorkspaceChanged: () => () => undefined,
    onWorkspaceIndexed: () => () => undefined,
    getCredentialStatus: async () => ({
      encryptionAvailable: false,
      vaultPath: 'C:\\c.enc',
      managedProviderIds: [],
      lastError: null
    }),
    forgetCredential: async () => ({
      encryptionAvailable: false,
      vaultPath: 'C:\\c.enc',
      managedProviderIds: [],
      lastError: null
    }),
    onAiEvent: () => () => undefined,
    onCredentialStatus: () => () => undefined,
    onTerminalData: () => () => undefined,
    onTerminalExit: () => () => undefined,
    onPluginLog: () => () => undefined,
    onPluginProgress: () => () => undefined,
    onPluginStateChanged: () => () => undefined,
    onPluginView: () => () => undefined,
    onViewportState: () => () => undefined,
    openExternalViewport: () => Promise.resolve({ reused: false }),

    onLspDiagnostics: () => () => undefined
  } satisfies AiIdeApi

  vi.stubGlobal('window', { aiide: api, confirm: () => true })
}

function callsOf(method: string): Call[] {
  return calls.filter((call) => call.method === method)
}

function tabContent(): string {
  return getEditorState().tabs.find((tab) => tab.path === FILE_A)?.content ?? ''
}

/** 打开工作区 + 文件 + 选中第 2 行 `return 1`（列 2→10，0-based）。 */
async function prepareSelectedFile(): Promise<void> {
  await openWorkspace(ROOT)
  await openFile(FILE_A)
  setEditorSelection({
    path: FILE_A,
    startLine: RANGE.start_line,
    endLine: RANGE.end_line,
    startColumn: RANGE.start_column,
    endColumn: RANGE.end_column
  })
}

beforeEach(() => {
  installHarness()
  resetInlineEditState()
})

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('快捷键与入口', () => {
  it('Ctrl+K 解析为 inline-edit，且不抢 Ctrl+Shift+K', () => {
    expect(resolveShortcut({ key: 'k', ctrlKey: true })).toBe('inline-edit')
    expect(resolveShortcut({ key: 'K', ctrlKey: true })).toBe('inline-edit')
    expect(resolveShortcut({ key: 'k', metaKey: true })).toBe('inline-edit')
    expect(resolveShortcut({ key: 'K', ctrlKey: true, shiftKey: true })).toBeNull()
    expect(resolveShortcut({ key: 'k' })).toBeNull()
  })

  it('有选区时打开浮层，并把选区换算成协议口径', async () => {
    await prepareSelectedFile()
    openInlineEdit()

    const state = getInlineEditState()
    expect(state.open).toBe(true)
    expect(state.phase).toBe('input')
    expect(state.path).toBe(FILE_A)
    expect(state.relativePath).toBe('a.ts')
    expect(state.selection).toEqual(RANGE)
    expect(state.baseContent).toBe(CONTENT)
  })
})

describe('无选区', () => {
  it('不打开浮层、不请求内核，只给一句提示', async () => {
    await prepareSelectedFile()
    setEditorSelection(null)
    openInlineEdit()

    const state = getInlineEditState()
    expect(state.open).toBe(false)
    expect(callsOf(Methods.inlineEdit.propose)).toHaveLength(0)
    expect(getEditorState().notice?.text).toBe(NO_SELECTION_HINT)
  })

  it('没有打开文件时也不发请求', () => {
    openInlineEdit()

    expect(getInlineEditState().open).toBe(false)
    expect(callsOf(Methods.inlineEdit.propose)).toHaveLength(0)
    expect(getEditorState().notice?.kind).toBe('warn')
  })
})

describe('生成建议', () => {
  it('提交后进入 preview，请求带上相对路径与选区', async () => {
    await prepareSelectedFile()
    openInlineEdit()
    setInlineEditInstruction('  改成返回 0  ')
    await submitInlineEdit()

    const propose = callsOf(Methods.inlineEdit.propose)
    expect(propose).toHaveLength(1)
    expect(propose[0]?.params).toMatchObject({
      workspace_id: 'ws-1',
      path: 'a.ts',
      instruction: '改成返回 0',
      selection: RANGE
    })

    const state = getInlineEditState()
    expect(state.phase).toBe('preview')
    expect(state.proposal).toEqual(PROPOSAL)
    expect(inlineEditRangeLabel(PROPOSAL.range)).toBe('第 2 行')
  })

  it('指令为空时不请求，只报一句可读提示', async () => {
    await prepareSelectedFile()
    openInlineEdit()
    setInlineEditInstruction('   ')
    await submitInlineEdit()

    expect(callsOf(Methods.inlineEdit.propose)).toHaveLength(0)
    expect(getInlineEditState().error).toContain('先写一句')
  })
})

describe('接受与拒绝', () => {
  it('接受：按列把 replacement 拼回原文并写盘，标签内容同步', async () => {
    await prepareSelectedFile()
    openInlineEdit()
    setInlineEditInstruction('改成返回 0')
    await submitInlineEdit()
    await acceptInlineEdit()

    const writes = callsOf(Methods.workspace.writeFile)
    expect(writes).toHaveLength(1)
    const expected = 'function f() {\n  return 0\n}\n'
    expect(writes[0]?.params).toMatchObject({
      path: FILE_A,
      content: expected,
      // 带预期 hash：预览期间文件被别人改过就该撞 CAS 失败，而不是静默覆盖。
      expected_hash: 'h1'
    })
    expect(tabContent()).toBe(expected)
    expect(getInlineEditState().open).toBe(false)
    expect(getEditorState().notice?.text).toContain('已写回')
  })

  it('拒绝：一次写请求都没有，文件保持原样', async () => {
    await prepareSelectedFile()
    openInlineEdit()
    setInlineEditInstruction('改成返回 0')
    await submitInlineEdit()
    rejectInlineEdit()

    expect(callsOf(Methods.workspace.writeFile)).toHaveLength(0)
    expect(tabContent()).toBe(CONTENT)
    expect(getInlineEditState().open).toBe(false)
    expect(getInlineEditState().proposal).toBeNull()
  })

  it('预览期间文件被改动过就拒绝写回，并提示建议失效', async () => {
    await prepareSelectedFile()
    openInlineEdit()
    setInlineEditInstruction('改成返回 0')
    await submitInlineEdit()

    const { updateContent } = await import('./store')
    updateContent(FILE_A, `${CONTENT}// 别人改的\n`)
    await acceptInlineEdit()

    expect(callsOf(Methods.workspace.writeFile)).toHaveLength(0)
    expect(getInlineEditState().error).toContain('失效')
    expect(getInlineEditState().open).toBe(true)
  })
})

describe('错误码映射', () => {
  it('9001 留在浮层里，提示选区越界且不关窗', async () => {
    await prepareSelectedFile()
    openInlineEdit()
    setInlineEditInstruction('改成返回 0')
    proposeError = rpcError(9001, 'selection out of range')
    await submitInlineEdit()

    const state = getInlineEditState()
    expect(state.open).toBe(true)
    expect(state.phase).toBe('input')
    expect(state.error).toBe(INLINE_EDIT_ERROR_TEXT[9001])
    expect(callsOf(Methods.workspace.writeFile)).toHaveLength(0)
  })

  it('9003 提示「没给代码」，且不产生空替换', async () => {
    await prepareSelectedFile()
    openInlineEdit()
    setInlineEditInstruction('解释一下这段')
    proposeError = rpcError(9003, 'no code block in response')
    await submitInlineEdit()

    const state = getInlineEditState()
    expect(state.error).toBe(INLINE_EDIT_ERROR_TEXT[9003])
    expect(state.proposal).toBeNull()
    // 没建议就没有「接受」可点，写盘路径自然也不存在。
    await acceptInlineEdit()
    expect(callsOf(Methods.workspace.writeFile)).toHaveLength(0)
    expect(tabContent()).toBe(CONTENT)
  })

  it('改指令会把上一次的错误清掉', async () => {
    await prepareSelectedFile()
    openInlineEdit()
    proposeError = rpcError(9001, 'boom')
    setInlineEditInstruction('改成返回 0')
    await submitInlineEdit()
    expect(getInlineEditState().error).toBe(INLINE_EDIT_ERROR_TEXT[9001])

    setInlineEditInstruction('改成返回 0，并加注释')
    expect(getInlineEditState().error).toBeNull()
  })

  it('未收录的错误码带上内核原话，不把用户丢在「未知错误」里', () => {
    const view = describeInlineEditError(new Error('core exploded'), () => ({
      code: 9100,
      message: 'core exploded'
    }))
    expect(view.code).toBe(9100)
    expect(view.text).toContain('9100')
    expect(view.text).toContain('core exploded')
    // 非 RPC 错误：原话透传，code 为 null。
    const plain = describeInlineEditError(new Error('网络断了'), () => null)
    expect(plain).toEqual({ code: null, text: '网络断了' })
  })
})

describe('拼接与换算', () => {
  it('单行按列替换，只动选中的那一段', () => {
    expect(spliceSelection(CONTENT, RANGE, 'return 0')).toBe(
      'function f() {\n  return 0\n}\n'
    )
  })

  it('跨行选区保留首行前缀与尾行剩余部分', () => {
    // 「a = 1」整行 + 下一行的首列 b：中间那段被替成两行，尾行剩下「 = 2」。
    const src = 'a = 1\nb = 2\nc = 3\n'
    const range: TextSelection = { start_line: 1, start_column: 0, end_line: 2, end_column: 1 }
    expect(spliceSelection(src, range, 'x\ny')).toBe('x\ny = 2\nc = 3\n')
  })

  it('行号越界返回 null（调用方据此拒绝写盘）', () => {
    expect(
      spliceSelection(CONTENT, { start_line: 99, start_column: 0, end_line: 99, end_column: 0 }, 'x')
    ).toBeNull()
  })

  it('只有行没有列时按整行算', () => {
    expect(toTextSelection({ path: FILE_A, startLine: 2, endLine: 2 }, CONTENT)).toEqual({
      start_line: 2,
      start_column: 0,
      end_line: 2,
      end_column: 10
    })
  })

  it('同一条线内反着拖也摆正', () => {
    expect(
      toTextSelection(
        { path: FILE_A, startLine: 2, endLine: 2, startColumn: 9, endColumn: 2 },
        CONTENT
      )
    ).toEqual({ start_line: 2, start_column: 9, end_line: 2, end_column: 9 })
  })
})
