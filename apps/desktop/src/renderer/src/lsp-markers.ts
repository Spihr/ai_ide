/**
 * 把 `lsp-store` 里的诊断打到 Monaco 模型上。
 *
 * 为什么要单独一个文件：`lsp-api.ts` / `lsp-store.ts` 要能在 vitest 里跑（没有
 * DOM、没有 worker），不能 import `monaco-editor`。这里做的是「store → monaco」的
 * 单向接线：store 只认「路径 + 标记数组」，怎么找到 model、按什么 owner 打点
 * 全在本文件。
 *
 * **落在 model 上而不是 editor 上**：`setModelMarkers` 是 model 级 API，同一个
 * model 被两个 editor 打开（分屏）时标记自然共享；换成 editor 级就得自己同步两份。
 */

import { LSP_MARKER_OWNER, type LspMarkerData } from './lsp-api'
import { setMarkerWriter } from './lsp-store'
import { monaco } from './monaco-setup'

import type * as Monaco from 'monaco-editor'

/** 路径归一：Monaco 的 `model.uri.path` 是正斜杠，Windows 路径要先转过去再比。 */
function toUriPath(path: string): string {
  return path.replace(/\\/g, '/')
}

/**
 * 按文件路径找到 model。
 *
 * 优先按 `uri.path` 精确匹配；找不到再退回「后缀匹配」——`file:///c%3A/...` 这类
 * 编码过的 uri 与原始路径可能只差盘符大小写，用后缀兜一下比直接放弃稳妥。
 */
function findModel(path: string): Monaco.editor.ITextModel | null {
  const target = toUriPath(path).toLowerCase()
  const models = monaco.editor.getModels()

  for (const model of models) {
    if (toUriPath(model.uri.path).toLowerCase() === target) return model
  }
  for (const model of models) {
    if (toUriPath(model.uri.path).toLowerCase().endsWith(target)) return model
  }
  return null
}

/**
 * 写一批标记到指定文件的 model。
 *
 * 文件没在 Monaco 里打开时**静默丢弃**：诊断可能在做索引时批量推来（用户没打开
 * 那些文件），为它们建 model 会把内存吃满。等文件真被打开时，`openFile` 会重新
 * 同步一次，服务器会再推一遍。
 */
export function writeMarkers(path: string, markers: LspMarkerData[]): void {
  const model = findModel(path)
  if (model === null) return
  monaco.editor.setModelMarkers(
    model,
    LSP_MARKER_OWNER,
    markers.map((marker) => ({ ...marker, owner: LSP_MARKER_OWNER }))
  )
}

/**
 * 接线。在编辑器挂载前调一次即可（重复调用是幂等的，只会替换写入器）。
 *
 * 返回一个「摘掉写入器」的函数：编辑器卸载时调它，避免 store 往已 dispose 的
 * model 上打点。
 */
export function installLspMarkers(): () => void {
  setMarkerWriter(writeMarkers)
  return () => {
    setMarkerWriter(() => {
      // 卸载后到达的诊断直接丢弃。
    })
  }
}
