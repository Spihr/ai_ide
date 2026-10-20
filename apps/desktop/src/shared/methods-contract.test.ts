/**
 * 协议常量的字面量契约测试。
 *
 * `Methods` / `PROTOCOL_VERSION` 这些是**运行时值**，ts-rs 导不出来，只能在
 * TypeScript 侧手工维护。而手工维护的东西会漂移——把 `core.ping` 写成
 * `core.ping ` 或 `core.pings`，类型检查一声不吭，运行时才变成
 * "method not found"。
 *
 * 这里把这些字符串逐字钉死，作为与 Rust `ai_ide_protocol::methods` 的最后一道对齐。
 */

import { describe, expect, it } from 'vitest'

import {
  DECLARED_NOT_IMPLEMENTED_METHODS,
  IMPLEMENTED_METHODS,
  JSONRPC_VERSION,
  Methods,
  PROTOCOL_VERSION
} from '@ai-ide/shared-types'

describe('协议常量', () => {
  it('JSON-RPC 版本为 2.0', () => {
    expect(JSONRPC_VERSION).toBe('2.0')
  })

  it('协议版本为 1.0', () => {
    expect(PROTOCOL_VERSION).toBe('1.0')
  })

  it('方法名与 Rust 侧 methods 模块逐字一致', () => {
    expect({
      hello: Methods.core.hello,
      ping: Methods.core.ping,
      stats: Methods.core.stats,
      shutdown: Methods.core.shutdown,
      cancel: Methods.cancel.request,
      log: Methods.event.log,
      wsOpen: Methods.workspace.open,
      wsClose: Methods.workspace.close,
      wsTree: Methods.workspace.tree,
      wsRead: Methods.workspace.readFile,
      wsWrite: Methods.workspace.writeFile,
      wsCreate: Methods.workspace.create,
      wsRename: Methods.workspace.rename,
      wsDelete: Methods.workspace.delete,
      wsReveal: Methods.workspace.reveal,
      searchFiles: Methods.search.files,
      searchContent: Methods.search.content,
      providerUpsert: Methods.provider.upsert,
      providerList: Methods.provider.list,
      providerDelete: Methods.provider.delete,
      providerTest: Methods.provider.test,
      modelUpsert: Methods.model.upsert,
      modelList: Methods.model.list,
      modelDelete: Methods.model.delete,
      llmChatStream: Methods.llm.chatStream,
      llmCancel: Methods.llm.cancel,
      llmListModels: Methods.llm.listModels,
      llmCountTokens: Methods.llm.countTokens,
      wsChanged: Methods.event.workspaceChanged,
      wsIndexed: Methods.event.workspaceIndexed,
      providerChanged: Methods.event.providerChanged,
      llmStarted: Methods.event.llmStarted,
      llmDelta: Methods.event.llmDelta,
      llmReasoning: Methods.event.llmReasoning,
      llmToolCall: Methods.event.llmToolCall,
      llmUsage: Methods.event.llmUsage,
      llmFinished: Methods.event.llmFinished,
      llmError: Methods.event.llmError
    }).toEqual({
      hello: 'core.hello',
      ping: 'core.ping',
      stats: 'core.stats',
      shutdown: 'core.shutdown',
      cancel: '$/cancelRequest',
      log: 'core.log',
      wsOpen: 'workspace.open',
      wsClose: 'workspace.close',
      wsTree: 'workspace.tree',
      wsRead: 'workspace.read_file',
      wsWrite: 'workspace.write_file',
      wsCreate: 'workspace.create',
      wsRename: 'workspace.rename',
      wsDelete: 'workspace.delete',
      wsReveal: 'workspace.reveal',
      searchFiles: 'search.files',
      searchContent: 'search.content',
      providerUpsert: 'provider.upsert',
      providerList: 'provider.list',
      providerDelete: 'provider.delete',
      providerTest: 'provider.test',
      modelUpsert: 'model.upsert',
      modelList: 'model.list',
      modelDelete: 'model.delete',
      llmChatStream: 'llm.chat_stream',
      llmCancel: 'llm.cancel',
      llmListModels: 'llm.list_models',
      llmCountTokens: 'llm.count_tokens',
      agentStart: 'agent.start',
      agentStop: 'agent.stop',
      agentDecide: 'agent.decide',
      wsChanged: 'workspace.changed',
      wsIndexed: 'workspace.indexed',
      providerChanged: 'provider.changed',
      llmStarted: 'llm.started',
      llmDelta: 'llm.delta',
      llmReasoning: 'llm.reasoning',
      llmToolCall: 'llm.tool_call',
      llmUsage: 'llm.usage',
      llmFinished: 'llm.finished',
      llmError: 'llm.error',
      agentApprovalRequired: 'agent.approval_required',
      agentFinished: 'agent.finished'
    })
  })

  it('已实现方法清单与 Rust IMPLEMENTED 一致', () => {
    expect([...IMPLEMENTED_METHODS]).toEqual([
      'core.hello',
      'core.ping',
      'core.stats',
      'core.shutdown',
      '$/cancelRequest',
      'workspace.open',
      'workspace.close',
      'workspace.tree',
      'workspace.read_file',
      'workspace.write_file',
      'workspace.create',
      'workspace.rename',
      'workspace.delete',
      'workspace.reveal',
      'search.files',
      'search.content',
      'provider.upsert',
      'provider.list',
      'provider.delete',
      'provider.test',
      'model.upsert',
      'model.list',
      'model.delete',
      'llm.chat_stream',
      'llm.cancel',
      'llm.list_models',
      'llm.count_tokens',
      'agent.start',
      'agent.stop',
      'agent.decide'
    ])
  })

  it('未接线清单已清空（M2 收口），且与已实现清单互斥', () => {
    // M2 把 llm.* 接进 dispatch 后，「协议已定义」与「内核已接线」的差值应为空。
    expect([...DECLARED_NOT_IMPLEMENTED_METHODS]).toEqual([])
    for (const method of DECLARED_NOT_IMPLEMENTED_METHODS) {
      // 虚报能力会让外壳照单调进一片 -32601，互斥关系必须钉死。
      expect(IMPLEMENTED_METHODS as readonly string[]).not.toContain(method)
    }
  })
})
