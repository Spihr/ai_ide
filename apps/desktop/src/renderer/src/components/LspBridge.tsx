/**
 * 编辑器 ↔ 语言服务的接线层。
 *
 * 放在组件里而不是 store 里，是因为它要「跟着编辑器的生命周期走」：只有编辑器真
 * 挂上了、模型真存在了，才谈得上把诊断画成标记。store 仍然是唯一状态源，本文件
 * 只做四件事：
 *
 * 1. **开文件 → `lsp.start`**：按**当前文件的真实语言**起服务（`languageForPath`
 *    与 Monaco 建模型时用的是同一个函数，两边不会各判一套后缀）。命令缺失时
 *    store 自己降级成「不可用」，这里不弹框、不打日志。
 * 2. **保存 → `lsp.sync`**：保存后把磁盘上的新正文再同步一遍。内核那边 `didSave`
 *    只管「保存了」这个事实，正文是随 `didChange` 走的；少了这一步，服务器对
 *    未改动的文件仍持有旧文本。
 * 3. **切文件 → `lsp.sync`**：切回一个已经开过的文件时，编辑器里可能还有没保存
 *    的改动，只让服务器读磁盘会拿到旧版本，所以按当前缓冲同步。
 * 4. **关工作区 → `stopAll`**：内核进程不关会一直占内存，且下次开新工作区时
 *    旧的 `server_id` 全是废的。
 *
 * 一律 `void` 掉异步调用并对 `false` 视而不见：语言服务是增强，任何一步失败都
 * 不该影响编辑。
 */

import { useEffect, useRef } from 'react'

import { installLspMarkers } from '../lsp-markers'
import { openFile, stopAll, syncFile } from '../lsp-store'
import { languageForPath } from '../monaco-setup'
import { getEditorState, useEditor } from '../store'

export function LspBridge() {
  const state = useEditor()

  // 标记写入器只装一次；卸载时摘掉，避免往已 dispose 的 model 上打点。
  useEffect(() => installLspMarkers(), [])

  const workspaceRoot = state.workspace?.root ?? null
  const previousRoot = useRef<string | null>(null)

  // 换 / 关工作区：旧连接全停掉。
  //
  // 不能只在「关成 null」时停：直接从一个工作区切到另一个时根路径是换值而不是变
  // 空，旧语言服务器会一直挂在旧根上（诊断报的还是旧文件），必须一并停掉。新根
  // 下的文件由下面的 effect 重新起服务，所以这里不做任何 restart。
  useEffect(() => {
    if (previousRoot.current === workspaceRoot) return
    const hadWorkspace = previousRoot.current !== null
    previousRoot.current = workspaceRoot
    if (hadWorkspace) void stopAll()
  }, [workspaceRoot])

  const activeTab = state.tabs.find((tab) => tab.path === state.activePath) ?? null
  const activePath = activeTab?.path ?? null
  const loading = activeTab?.loading ?? false

  // 打开 / 切换文件：起服务（或复用语言连接）并同步当前缓冲。
  // 依赖里只放「文件身份 + 是否读完 + 工作区根」——正文变化不进依赖，否则每敲一个
  // 字都会走一次 openFile（保存时的同步由下面的 effect 单独负责）。
  useEffect(() => {
    if (activePath === null || loading) return
    // 正文从 store 现取而不是走依赖：进依赖会让每敲一个字都重跑一次「开文件」。
    // 这里要的只是「切到某个文件那一刻的完整缓冲」，之后的变化归保存/切换路径管。
    const content = getEditorState().tabs.find((tab) => tab.path === activePath)?.content ?? null
    if (content === null) return

    void openFile({
      path: activePath,
      languageId: languageForPath(activePath),
      content,
      ...(workspaceRoot === null ? {} : { root: workspaceRoot })
    })
  }, [activePath, loading, workspaceRoot])

  // 保存后同步：`savedContent` 追上 `content` 的那一刻就是「刚存过盘」。
  // 只在活动文件上做——后台标签被外部改写时内容会整篇换掉，那属于重载而不是保存。
  const savedRef = useRef<{ path: string | null; saved: string | null }>({
    path: null,
    saved: null
  })

  useEffect(() => {
    if (activeTab === null || activeTab.loading) return
    const { path, savedContent } = activeTab
    const previous = savedRef.current
    savedRef.current = { path, saved: savedContent }
    if (previous.path !== path) return
    if (previous.saved === null || previous.saved === savedContent) return
    void syncFile(path, activeTab.content)
  }, [activeTab])

  return null
}
