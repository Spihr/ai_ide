/**
 * M5 渲染侧会话状态行为测试。
 *
 * 与 `ai-store.test.ts` 同一套路：不渲染 React，直接驱动导出的动作函数，用假的
 * `window.aiide` 顶替 IPC，断言「发了什么请求」与「状态怎么变」。
 *
 * 这里钉的是 M5 外壳最容易写错、错了以后**看起来像内核坏了**的几条：
 * 切换会话必须把历史折成气泡灌回对话面板（但 system / tool / 空回复不是气泡）、
 * 删掉当前会话必须同时解除桥的绑定（否则下一条消息会追加进已删除的 id）、
 * 导出必须先拿到系统对话框的落点（取消不是错误）、分叉要真的切到新会话。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  Methods,
  type AuditEventView,
  type MessageView,
  type SessionView
} from '@ai-ide/shared-types'

import type { AiIdeApi } from '../../shared/ipc'
import { getAiState, resetAiState } from './ai-store'
import { getActiveSessionId, releaseActiveSession, setActiveSessionId } from './session-bridge'
import {
  createSession,
  deleteSession,
  dismissSessionHint,
  exportSession,
  forkSession,
  getSessionState,
  initSessions,
  loadMoreAudit,
  loadMoreSessions,
  bubblesOf,
  refreshAudit,
  refreshSessions,
  renameSession,
  resetSessionState,
  selectSession,
  setSessionQuery,
  startNewChat
} from './session-store'

interface Call {
  method: string
  params: Record<string, unknown> | undefined
}

interface Harness {
  calls: Call[]
  /** 最近一次 `session.fork` 的 `from_message_id`。 */
  lastForkFrom: () => string | null
  /** 让系统保存对话框返回这个路径；`null` 表示用户取消。 */
  setSavePath: (path: string | null) => void
  /** 让下一次请求失败；给了 `method` 就只让该方法的请求失败。 */
  failNext: (message: string, method?: string) => void
}

function session(id: string, title: string, updatedAt = 1): SessionView {
  return {
    session_id: id,
    title,
    workspace_path: null,
    model_ref: null,
    created_at_ms: updatedAt,
    updated_at_ms: updatedAt
  }
}

function message(
  id: string,
  sessionId: string,
  role: 'user' | 'assistant' | 'system' | 'tool',
  content: string
): MessageView {
  return {
    message_id: id,
    session_id: sessionId,
    parent_id: null,
    role,
    content,
    attachments: [],
    tool_calls: [],
    tool_call_id: null,
    model_ref: null,
    usage: null,
    created_at_ms: 1
  }
}

function auditEvent(id: number, kind: string): AuditEventView {
  return {
    event_id: id,
    kind,
    session_id: 's-1',
    actor: 'core',
    summary: kind,
    detail: null,
    created_at_ms: id
  }
}

function installHarness(options: { seedSessions?: number } = {}): Harness {
  const calls: Call[] = []
  const seed = options.seedSessions ?? 2
  let sessions: SessionView[] =
    seed === 2
      ? [session('s-1', '周报整理'), session('s-2', '接口调试', 2)]
      : // 翻页用例要一个「超过一页」的列表：标题带序号，方便断言第二页从第 51 条起。
        Array.from({ length: seed }, (_, index) =>
          session(`s-${index + 1}`, `会话 ${String(index + 1).padStart(3, '0')}`, index + 1)
        )
  const messages: Record<string, MessageView[]> = {
    's-1': [
      message('m-1', 's-1', 'system', '你是助手'),
      message('m-2', 's-1', 'user', '帮我看下这周周报'),
      message('m-3', 's-1', 'tool', '{"ok":true}'),
      message('m-4', 's-1', 'assistant', '这周你做了三件事'),
      // 空回复与纯空白：回放时会变成一个空气泡，必须跳过。
      message('m-5', 's-1', 'assistant', '   ')
    ],
    's-2': [message('m-9', 's-2', 'user', '接口 500 了')]
  }
  let savePath: string | null = 'C:\\out\\会话.md'
  let failure: { message: string; method: string | null } | null = null
  let created = 0
  /** 最近一次分叉请求带的截断点，供「消息级分叉」用例断言。 */
  let lastForkFrom: string | null = null

  const api: AiIdeApi = {
    getCoreStatus: async () => {
      throw new Error('测试未使用 getCoreStatus')
    },
    restartCore: async () => {
      throw new Error('测试未使用 restartCore')
    },
    ping: async () => {
      throw new Error('测试未使用 ping')
    },
    pickWorkspaceDirectory: async () => null,
    pickSavePath: async () => savePath,
    request: async (method: string, params?: unknown) => {
      calls.push({ method, params: params as Record<string, unknown> | undefined })
      if (failure && (failure.method === null || failure.method === method)) {
        const text = failure.message
        failure = null
        throw new Error(text)
      }
      const p = (params ?? {}) as Record<string, unknown>
      switch (method) {
        case Methods.session.list: {
          const query = (p.query as string | null) ?? null
          const matched = query
            ? sessions.filter((item) => item.title.toLowerCase().includes(query.toLowerCase()))
            : sessions
          // 真按 offset/limit 切：翻页用例要能看出「第二次请求确实带了新偏移」。
          const offset = (p.offset as number | undefined) ?? 0
          const limit = (p.limit as number | undefined) ?? 50
          return { sessions: matched.slice(offset, offset + limit), total: matched.length }
        }
        case Methods.session.get: {
          const id = p.session_id as string
          const found = sessions.find((item) => item.session_id === id)
          if (!found) throw new Error(`会话不存在：${id}`)
          return { session: found, messages: p.include_messages === false ? [] : (messages[id] ?? []) }
        }
        case Methods.session.create: {
          created += 1
          const next = session(`s-new-${created}`, '新会话', 100 + created)
          sessions = [next, ...sessions]
          messages[next.session_id] = []
          return { session_id: next.session_id, session: next }
        }
        case Methods.session.update: {
          const id = p.session_id as string
          const index = sessions.findIndex((item) => item.session_id === id)
          const base = sessions[index]
          if (base === undefined) throw new Error(`会话不存在：${id}`)
          const updated: SessionView = { ...base, title: p.title as string, updated_at_ms: 999 }
          sessions = sessions.map((item) => (item.session_id === id ? updated : item))
          return { session: updated }
        }
        case Methods.session.delete: {
          const ids = p.session_ids as string[]
          sessions = sessions.filter((item) => !ids.includes(item.session_id))
          return { ok: true, deleted: ids.length }
        }
        case Methods.session.export:
          return { ok: true, path: p.out_path as string, bytes: 512, messages: 3 }
        case Methods.session.fork: {
          const parent = sessions.find((item) => item.session_id === (p.session_id as string))
          lastForkFrom = (p.from_message_id as string | null) ?? null
          const next = session('s-fork', `${parent?.title ?? '会话'}（分叉）`, 500)
          sessions = [next, ...sessions]
          messages['s-fork'] = [message('m-f1', 's-fork', 'user', '接口 500 了')]
          return { new_session_id: 's-fork', session: next, messages: 1 }
        }
        case Methods.audit.list:
          return { events: [auditEvent(2, 'session.created'), auditEvent(1, 'message.appended')], total: 2 }
        default:
          return { ok: true }
      }
    },
    onCoreStatus: () => () => undefined,
    onCoreLog: () => () => undefined,
    onWorkspaceChanged: () => () => undefined,
    onWorkspaceIndexed: () => () => undefined,
    getCredentialStatus: async () => ({
      encryptionAvailable: true,
      vaultPath: 'C:\\ud\\credentials.enc',
      managedProviderIds: [],
      lastError: null
    }),
    forgetCredential: async () => ({
      encryptionAvailable: true,
      vaultPath: 'C:\\ud\\credentials.enc',
      managedProviderIds: [],
      lastError: null
    }),
    onAiEvent: () => () => undefined,
    onCredentialStatus: () => () => undefined
  }

  vi.stubGlobal('window', { aiide: api })

  return {
    calls,
    lastForkFrom: () => lastForkFrom,
    setSavePath: (next) => {
      savePath = next
    },
    failNext: (text, method) => {
      failure = { message: text, method: method ?? null }
    }
  }
}

let h: Harness

/** 过滤出某个方法的全部调用。 */
function callsOf(method: string): Call[] {
  return h.calls.filter((call) => call.method === method)
}

/** 等异步动作（含 `void refreshAudit(...)` 这类不等的分支）跑完。 */
async function flush(): Promise<void> {
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
}

beforeEach(() => {
  resetSessionState()
  resetAiState()
  // 桥的活动会话 id 是模块级状态，不清会把上一个用例的会话带进下一个用例。
  setActiveSessionId(null)
  h = installHarness()
})

describe('会话列表', () => {
  it('拉列表不按工作区过滤，只带标题过滤词与分页', async () => {
    const ok = await refreshSessions('')

    expect(ok).toBe(true)
    const params = callsOf(Methods.session.list)[0]?.params
    expect(params?.workspace_id).toBeNull()
    expect(params?.query).toBeNull()
    expect(params?.limit).toBe(50)

    expect(getSessionState().sessions.map((item) => item.session_id)).toEqual(['s-1', 's-2'])
    expect(getSessionState().total).toBe(2)
    expect(getSessionState().loading).toBe(false)
  })

  it('输入过滤词后只显示命中项，但执行中的选中态不被清掉', async () => {
    await selectSession('s-1')
    // 先打开一条，再用不命中的词去搜：选中态保留（用户只是搜了一下）。
    await refreshSessions('接口')

    const state = getSessionState()
    expect(state.query).toBe('接口')
    expect(state.sessions).toHaveLength(1)
    expect(state.currentId).toBe('s-1')
    expect(callsOf(Methods.session.list).at(-1)?.params?.query).toBe('接口')
  })

  it('标题过滤走 setSessionQuery，空白词归零为 null', async () => {
    setSessionQuery('   ')
    await flush()
    expect(callsOf(Methods.session.list)[0]?.params?.query).toBeNull()
  })

  it('拉列表失败时留下可读错误，loading 复位', async () => {
    h.failNext('数据库锁住了', Methods.session.list)
    const ok = await refreshSessions()

    expect(ok).toBe(false)
    expect(getSessionState().error).toContain('数据库锁住了')
    expect(getSessionState().loading).toBe(false)
  })
})

describe('打开会话', () => {
  it('把历史折成气泡灌回对话面板：system / tool / 空回复都不占气泡', async () => {
    const ok = await selectSession('s-1')

    expect(ok).toBe(true)
    const state = getSessionState()
    expect(state.currentId).toBe('s-1')
    expect(state.current?.title).toBe('周报整理')
    expect(state.messagesTotal).toBe(5)

    const bubbles = getAiState().bubbles
    expect(bubbles.map((bubble) => bubble.role)).toEqual(['user', 'assistant'])
    expect(bubbles[0]?.text).toBe('帮我看下这周周报')
    expect(bubbles[1]?.text).toBe('这周你做了三件事')
    // 回放出来的气泡一律是「已完成」：它们不是正在生成的流。
    expect(bubbles.every((bubble) => bubble.status === 'done')).toBe(true)
  })

  it('打开后活动会话同步到桥，并顺手拉该会话的审计', async () => {
    await selectSession('s-2')
    await flush()

    expect(getActiveSessionId()).toBe('s-2')
    expect(callsOf(Methods.audit.list)[0]?.params?.session_id).toBe('s-2')
    expect(getSessionState().audit.map((item) => item.kind)).toEqual([
      'session.created',
      'message.appended'
    ])
  })

  it('打开失败时不留半截状态，错误落到 error', async () => {
    h.failNext('会话不存在：s-9', Methods.session.get)
    const ok = await selectSession('s-9')

    expect(ok).toBe(false)
    expect(getSessionState().error).toContain('会话不存在')
    expect(getSessionState().currentId).toBeNull()
    expect(getActiveSessionId()).toBeNull()
    expect(getSessionState().busy).toBe(false)
  })
})

describe('新建与切换', () => {
  it('新建会话插到列表头并选中，气泡清空', async () => {
    // 真机顺序：initSessions 先拉列表，之后才有「插到列表头」这回事。
    await refreshSessions()
    await selectSession('s-1')

    const ok = await createSession()

    expect(ok).toBe(true)
    const state = getSessionState()
    expect(state.sessions[0]?.session_id).toBe('s-new-1')
    expect(state.sessions).toHaveLength(3)
    expect(state.currentId).toBe('s-new-1')
    expect(state.sessions[0]?.title).toBe('新会话')
    expect(getAiState().bubbles).toHaveLength(0)
    expect(getActiveSessionId()).toBe('s-new-1')
  })

  it('开始新对话只解除绑定，不删库（旧会话仍在列表里）', async () => {
    await refreshSessions()
    await selectSession('s-1')
    startNewChat()

    const state = getSessionState()
    expect(state.currentId).toBeNull()
    expect(state.sessions).toHaveLength(2)
    expect(getActiveSessionId()).toBeNull()
    expect(getAiState().bubbles).toHaveLength(0)
    expect(callsOf(Methods.session.delete)).toHaveLength(0)
  })
})

describe('改名与删除', () => {
  it('空标题直接拒绝，不发请求', async () => {
    const ok = await renameSession('s-1', '   ')

    expect(ok).toBe(false)
    expect(callsOf(Methods.session.update)).toHaveLength(0)
    expect(getSessionState().hint).toContain('不能为空')
  })

  it('改名后列表与当前会话同时更新', async () => {
    await refreshSessions()
    await selectSession('s-1')
    const ok = await renameSession('s-1', '  周报（十月中）  ')

    expect(ok).toBe(true)
    expect(callsOf(Methods.session.update)[0]?.params?.title).toBe('周报（十月中）')
    expect(getSessionState().sessions[0]?.title).toBe('周报（十月中）')
    expect(getSessionState().current?.title).toBe('周报（十月中）')
  })

  it('删掉当前会话时退回「没有选中」，并解除桥的绑定', async () => {
    await refreshSessions()
    await selectSession('s-1')
    const ok = await deleteSession('s-1')

    expect(ok).toBe(true)
    const state = getSessionState()
    expect(state.sessions.map((item) => item.session_id)).toEqual(['s-2'])
    expect(state.currentId).toBeNull()
    expect(state.current).toBeNull()
    expect(getActiveSessionId()).toBeNull()
    expect(getAiState().bubbles).toHaveLength(0)
    expect(state.hint).toContain('已删除 1 个会话')
  })

  it('删掉别的会话不动当前选中', async () => {
    await refreshSessions()
    await selectSession('s-1')
    await deleteSession('s-2')

    expect(getSessionState().currentId).toBe('s-1')
    expect(getActiveSessionId()).toBe('s-1')
    expect(getAiState().bubbles).toHaveLength(2)
  })
})

describe('导出', () => {
  it('用户取消保存对话框时静默返回 false，不调内核', async () => {
    await selectSession('s-1')
    h.setSavePath(null)

    const ok = await exportSession('s-1', 'markdown')

    expect(ok).toBe(false)
    expect(callsOf(Methods.session.export)).toHaveLength(0)
    // 取消不是错误：不该在界面上留红字。
    expect(getSessionState().error).toBeNull()
    expect(getSessionState().busy).toBe(false)
  })

  it('默认文件名取会话标题，并把落点与格式交给内核', async () => {
    await selectSession('s-1')
    const pickSpy = vi.spyOn(window.aiide, 'pickSavePath')

    const ok = await exportSession('s-1', 'json')

    expect(ok).toBe(true)
    expect(pickSpy).toHaveBeenCalledWith('周报整理.json', ['json'])
    const params = callsOf(Methods.session.export)[0]?.params
    expect(params?.format).toBe('json')
    expect(params?.out_path).toBe('C:\\out\\会话.md')
    expect(getSessionState().hint).toContain('已导出 3 条消息')
  })

  it('标题里的文件系统非法字符被清掉：否则保存直接失败', async () => {
    await selectSession('s-1')
    await renameSession('s-1', 'a/b:c')
    const pickSpy = vi.spyOn(window.aiide, 'pickSavePath')

    await exportSession('s-1', 'markdown')

    expect(pickSpy).toHaveBeenCalledWith('a b c.md', ['md'])
  })
})

describe('分叉', () => {
  it('分叉出新会话后直接切过去，并把复制来的消息灌进面板', async () => {
    await refreshSessions()
    await selectSession('s-2')
    const ok = await forkSession('s-2')

    expect(ok).toBe(true)
    expect(getSessionState().sessions[0]?.session_id).toBe('s-fork')
    expect(getSessionState().currentId).toBe('s-fork')
    expect(getActiveSessionId()).toBe('s-fork')
    expect(getAiState().bubbles.map((bubble) => bubble.text)).toEqual(['接口 500 了'])
    expect(getSessionState().hint).toContain('已分叉出')
  })
})

describe('初始化', () => {
  it('initSessions 拉一次列表', async () => {
    initSessions()
    await flush()

    expect(callsOf(Methods.session.list)).toHaveLength(1)
    expect(getSessionState().sessions).toHaveLength(2)
  })

  it('桥收到「释放会话」时同步清空面板：外壳不自己偷偷留着已解除的会话', async () => {
    initSessions()
    await flush()
    await selectSession('s-1')
    expect(getAiState().bubbles).toHaveLength(2)

    releaseActiveSession()

    expect(getSessionState().currentId).toBeNull()
    expect(getAiState().bubbles).toHaveLength(0)
  })

  it('提示可以被关掉，且重复关不产生额外通知', async () => {
    await renameSession('s-1', '')
    expect(getSessionState().hint).not.toBeNull()

    dismissSessionHint()
    expect(getSessionState().hint).toBeNull()
    expect(getSessionState().error).toBeNull()
    dismissSessionHint()
  })
})

describe('列表与审计翻页', () => {
  it('加载更多按已加载条数当偏移，追加且按 id 去重', async () => {
    resetSessionState()
    h = installHarness({ seedSessions: 120 })

    await refreshSessions('')
    expect(getSessionState().sessions).toHaveLength(50)
    expect(getSessionState().total).toBe(120)

    const ok = await loadMoreSessions()

    expect(ok).toBe(true)
    const params = callsOf(Methods.session.list)[1]?.params
    // offset 必须是「已有 50 条」而不是 0：否则第二页会原样重复第一页。
    expect(params?.offset).toBe(50)
    expect(params?.limit).toBe(50)
    expect(getSessionState().sessions).toHaveLength(100)
    expect(new Set(getSessionState().sessions.map((item) => item.session_id)).size).toBe(100)
  })

  it('已加载条数追平 total 时不再发请求', async () => {
    resetSessionState()
    h = installHarness({ seedSessions: 60 })

    await refreshSessions('')
    await loadMoreSessions()
    expect(getSessionState().sessions).toHaveLength(60)

    const before = callsOf(Methods.session.list).length
    expect(await loadMoreSessions()).toBe(false)
    expect(callsOf(Methods.session.list)).toHaveLength(before)
  })

  it('审计翻页追加并按 event_id 去重', async () => {
    resetSessionState()
    await refreshAudit('s-1')
    setSessionQuery('')

    const before = callsOf(Methods.audit.list).length
    const ok = await loadMoreAudit()
    expect(typeof ok).toBe('boolean')
    // mock 的审计恒为 2 条、total=2：已到到底，不该再发请求。
    expect(callsOf(Methods.audit.list)).toHaveLength(before)
  })
})

describe('消息级分叉', () => {
  it('历史气泡带上 message_id，分叉时作为截断点发给内核', async () => {
    await selectSession('s-1')
    const bubbles = getAiState().bubbles
    const assistant = bubbles.find((bubble) => bubble.role === 'assistant')
    expect(assistant?.messageId).toBe('m-4')

    await forkSession('s-1', assistant?.messageId ?? null)

    // 关键：不是「从末尾分叉」，而是把那条消息 id 一起传下去。
    expect(h.lastForkFrom()).toBe('m-4')
    expect(getSessionState().currentId).toBe('s-fork')
  })

  it('气泡与历史一一对应，system / tool / 空回复不进对话面板', async () => {
    const bubbles = bubblesOf([
      message('m-1', 's-1', 'system', '你是助手'),
      message('m-2', 's-1', 'user', '帮我看看'),
      message('m-3', 's-1', 'tool', '{"ok":true}'),
      message('m-4', 's-1', 'assistant', '   '),
      message('m-5', 's-1', 'assistant', '好')
    ])

    expect(bubbles.map((bubble) => bubble.messageId)).toEqual(['m-2', 'm-5'])
  })
})
