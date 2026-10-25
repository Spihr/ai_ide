/**
 * 渲染进程访问工作区能力的唯一入口。
 *
 * 调用链：renderer → `window.aiide.request` → 主进程策略闸门 → Rust 内核。
 * 渲染进程不持有内核连接，也不知道方法是否真的存在于内核——那是主进程与
 * 内核之间的事；这里只负责把参数形状（协议里的 snake_case 字段）拼对。
 */

import { Methods } from '@ai-ide/shared-types'
import type {
  CreateKind,
  Eol,
  OkResult,
  SearchContentResult,
  SearchFilesResult,
  SearchSymbolsResult,
  SymbolKind,
  WorkspaceDeleteResult,
  WorkspaceOpenResult,
  WorkspaceReadFileResult,
  WorkspaceTreeResult,
  WorkspaceWriteFileResult
} from '@ai-ide/shared-types'

function req<T>(method: string, params?: unknown): Promise<T> {
  return window.aiide.request(method, params) as Promise<T>
}

/** `search.symbols` 的可选参数。 */
export interface SymbolSearchOptions {
  /** 只保留这些 kind；缺省由内核返回全部 kind。 */
  kinds?: SymbolKind[]
}

/** `search.content` 的可选参数。 */
export interface ContentSearchOptions {
  isRegex?: boolean
  caseSensitive?: boolean
  includes?: string[]
  excludes?: string[]
  maxResults?: number
}

export const workspaceApi = {
  /** 打开工作区；`watch` 打开后会收到 `workspace.changed` 事件。 */
  open(path: string, watch = true): Promise<WorkspaceOpenResult> {
    return req<WorkspaceOpenResult>(Methods.workspace.open, { path, watch })
  },

  close(workspaceId: string): Promise<OkResult> {
    return req<OkResult>(Methods.workspace.close, { workspace_id: workspaceId })
  },

  /** 列目录；`path` 为相对根目录的路径，缺省为根。 */
  tree(workspaceId: string, path?: string, depth = 1): Promise<WorkspaceTreeResult> {
    return req<WorkspaceTreeResult>(Methods.workspace.tree, {
      workspace_id: workspaceId,
      path: path ?? null,
      depth,
      include_ignored: false
    })
  },

  readFile(path: string): Promise<WorkspaceReadFileResult> {
    return req<WorkspaceReadFileResult>(Methods.workspace.readFile, { path })
  },

  writeFile(
    path: string,
    content: string,
    expectedHash?: string | null,
    eol?: Eol
  ): Promise<WorkspaceWriteFileResult> {
    return req<WorkspaceWriteFileResult>(Methods.workspace.writeFile, {
      path,
      content,
      expected_hash: expectedHash ?? null,
      eol: eol ?? null
    })
  },

  create(path: string, kind: CreateKind): Promise<OkResult> {
    return req<OkResult>(Methods.workspace.create, { path, kind })
  },

  rename(from: string, to: string, overwrite = false): Promise<OkResult> {
    return req<OkResult>(Methods.workspace.rename, { from, to, overwrite })
  },

  /** 删除（移入回收站）；内核侧 `to_trash` 恒为 true，无永久删除能力。 */
  remove(paths: string[]): Promise<WorkspaceDeleteResult> {
    return req<WorkspaceDeleteResult>(Methods.workspace.delete, { paths, to_trash: true })
  },

  reveal(path: string): Promise<OkResult> {
    return req<OkResult>(Methods.workspace.reveal, { path })
  },

  searchFiles(workspaceId: string, query: string, glob?: string): Promise<SearchFilesResult> {
    return req<SearchFilesResult>(Methods.search.files, {
      workspace_id: workspaceId,
      query,
      glob: glob ?? null,
      limit: null
    })
  },

  /**
   * 符号检索（`search.symbols`）。`query` 为**子串**匹配，空串表示列出全部符号
   * （内核上限 200 条，按相关度排序）——快速选择器一打开就要有东西可选，靠的就是它。
   */
  searchSymbols(
    workspaceId: string,
    query: string,
    options: SymbolSearchOptions = {}
  ): Promise<SearchSymbolsResult> {
    return req<SearchSymbolsResult>(Methods.search.symbols, {
      workspace_id: workspaceId,
      query,
      kinds: options.kinds ?? null
    })
  },

  searchContent(
    workspaceId: string,
    query: string,
    options: ContentSearchOptions = {}
  ): Promise<SearchContentResult> {
    return req<SearchContentResult>(Methods.search.content, {
      workspace_id: workspaceId,
      query,
      is_regex: options.isRegex ?? false,
      case_sensitive: options.caseSensitive ?? false,
      includes: options.includes ?? [],
      excludes: options.excludes ?? [],
      max_results: options.maxResults ?? null
    })
  }
}
