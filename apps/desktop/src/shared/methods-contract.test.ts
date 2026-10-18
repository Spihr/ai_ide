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
      wsChanged: Methods.event.workspaceChanged,
      wsIndexed: Methods.event.workspaceIndexed
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
      wsChanged: 'workspace.changed',
      wsIndexed: 'workspace.indexed'
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
      'search.content'
    ])
  })
})
