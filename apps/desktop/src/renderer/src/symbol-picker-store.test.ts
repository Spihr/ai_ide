/**
 * 「转到符号」浮层状态机的测试。
 *
 * 覆盖三件事，都是「只由人按键盘才会发现」的那类：
 *
 * 1. **打开即列全部**：`Ctrl+T` 一按下去就得有东西可选（空 query 真的发请求）。
 * 2. **无结果不炸**：0 个符号时 `activeSymbol()` 为 `null`，回车什么都不做、不关浮层。
 * 3. **跳转参数**：选中后打开文件、定位到 `range.start_line`，列固定 1。
 *
 * 不渲染 React：状态都在模块级单例里，直接读快照断言。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import {
  Methods,
  type SearchSymbolsResult,
  type Symbol,
  type TreeEntry,
  type WorkspaceReadFileResult,
  type WorkspaceOpenResult
} from '@ai-ide/shared-types'

import type { AiIdeApi } from '../../shared/ipc'
import { getEditorState, openWorkspace } from './store'
import {
  SYMBOL_SEARCH_DEBOUNCE_MS,
  activeSymbol,
  closeSymbolPicker,
  escapeSymbolPicker,
  executeActiveSymbol,
  executeSymbol,
  getSymbolPickerState,
  moveSymbolPickerActive,
  openSymbolPicker,
  resetSymbolPickerState,
  setSymbolPickerQuery,
  symbolPickerRows,
  toggleSymbolPicker
} from './symbol-picker-store'

const ROOT = 'C:\\ws'
const FILE_A = `${ROOT}\\a.ts`

const BOOT: Symbol = {
  name: 'boot',
  kind: 'function',
  path: 'a.ts',
  range: { start_line: 7, end_line: 9 }
}
const LOADER: Symbol = {
  name: 'Loader',
  kind: 'class',
  path: 'src/loader.ts',
  range: { start_line: 42, end_line: 60 }
}

interface Call {
  method: string
  params: Record<string, unknown> | undefined
}

let calls: Call[] = []
let symbols: Symbol[] = [BOOT, LOADER]

function installHarness(): void {
  calls = []
  const entries: TreeEntry[] = [{ name: 'a.ts', path: FILE_A, kind: 'file', size: 0, mtime: 0, ignored: false }]
  const read: WorkspaceReadFileResult = {
    content: 'hello\n',
    encoding: 'utf-8',
    eol: 'lf',
    size: 6,
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
        case Methods.search.symbols:
          return { symbols } satisfies SearchSymbolsResult
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
    onLspDiagnostics: () => () => undefined
  } satisfies AiIdeApi

  vi.stubGlobal('window', { aiide: api, confirm: () => true })
}

function symbolCalls(): Call[] {
  return calls.filter((call) => call.method === Methods.search.symbols)
}

beforeEach(async () => {
  installHarness()
  resetSymbolPickerState()
  symbols = [BOOT, LOADER]
  await openWorkspace(ROOT)
})

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

describe('唤出与关闭', () => {
  it('Ctrl+T 唤出即用空 query 拉全部符号', async () => {
    toggleSymbolPicker()
    await vi.waitFor(() => {
      expect(symbolCalls()).toHaveLength(1)
    })

    expect(getSymbolPickerState().open).toBe(true)
    expect(symbolCalls()[0]?.params).toMatchObject({ workspace_id: 'ws-1', query: '' })
    expect(symbolPickerRows()).toHaveLength(2)
  })

  it('再按一次 Ctrl+T 收起，且清空查询词', async () => {
    openSymbolPicker()
    setSymbolPickerQuery('boot')
    toggleSymbolPicker()

    const state = getSymbolPickerState()
    expect(state.open).toBe(false)
    expect(state.query).toBe('')
  })

  it('Esc：有词先清词并重新列全部，词空了才关', async () => {
    openSymbolPicker()
    await vi.waitFor(() => {
      expect(symbolCalls()).toHaveLength(1)
    })

    setSymbolPickerQuery('boot')
    escapeSymbolPicker()

    let state = getSymbolPickerState()
    expect(state.open).toBe(true)
    expect(state.query).toBe('')

    escapeSymbolPicker()
    state = getSymbolPickerState()
    expect(state.open).toBe(false)
  })
})

describe('键盘导航', () => {
  it('↓ 在扁平顺序里前进，越界回绕', async () => {
    openSymbolPicker()
    await vi.waitFor(() => {
      expect(symbolPickerRows()).toHaveLength(2)
    })

    // 扁平顺序按 kind 排：函数在前，类在后。
    expect(symbolPickerRows().map((item) => item.name)).toEqual(['boot', 'Loader'])
    expect(activeSymbol()?.name).toBe('boot')

    moveSymbolPickerActive(1)
    expect(activeSymbol()?.name).toBe('Loader')

    moveSymbolPickerActive(1)
    expect(activeSymbol()?.name).toBe('boot')

    moveSymbolPickerActive(-1)
    expect(activeSymbol()?.name).toBe('Loader')
  })
})

describe('无结果', () => {
  it('0 个符号时没有高亮项，回车不做事也不关浮层', async () => {
    symbols = []
    openSymbolPicker()
    await vi.waitFor(() => {
      expect(symbolCalls()).toHaveLength(1)
    })

    expect(symbolPickerRows()).toEqual([])
    expect(activeSymbol()).toBeNull()

    await executeActiveSymbol()

    expect(getSymbolPickerState().open).toBe(true)
    expect(calls.filter((call) => call.method === Methods.workspace.readFile)).toHaveLength(0)
  })
})

describe('跳转', () => {
  it('选中符号后打开文件并把光标定位到 range.start_line（列 1）', async () => {
    openSymbolPicker()
    await vi.waitFor(() => {
      expect(symbolPickerRows()).toHaveLength(2)
    })

    await executeSymbol(LOADER)

    const state = getEditorState()
    expect(getSymbolPickerState().open).toBe(false)
    expect(state.activePath).toBe(`${ROOT}\\src\\loader.ts`)
    expect(state.reveal).toMatchObject({ line: 42, column: 1 })
  })

  it('输入查询后检索走防抖，只发最后一次', async () => {
    vi.useFakeTimers()
    openSymbolPicker()
    await vi.advanceTimersByTimeAsync(SYMBOL_SEARCH_DEBOUNCE_MS + 10)
    const baseline = symbolCalls().length

    setSymbolPickerQuery('b')
    setSymbolPickerQuery('bo')
    setSymbolPickerQuery('boot')
    await vi.advanceTimersByTimeAsync(SYMBOL_SEARCH_DEBOUNCE_MS + 10)

    expect(symbolCalls().length).toBe(baseline + 1)
    expect(symbolCalls().at(-1)?.params).toMatchObject({ query: 'boot' })
  })
})

describe('关闭', () => {
  it('close 收起并复位查询与高亮', async () => {
    openSymbolPicker()
    setSymbolPickerQuery('boot')
    closeSymbolPicker()

    expect(getSymbolPickerState()).toMatchObject({ open: false, query: '', activeIndex: 0 })
  })
})
