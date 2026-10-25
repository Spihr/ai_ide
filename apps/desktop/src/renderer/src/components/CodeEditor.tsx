/**
 * Monaco 编辑器宿主。
 *
 * 一个标签对应一个 `ITextModel`，模型按路径缓存、跨标签切换复用，
 * 这样撤销栈和滚动位置不会因为切换标签而丢失（视图状态另存 `viewsRef`）。
 *
 * `applyingRef` 用来区分「用户敲键」和「程序 setValue」——外部改动重载、
 * 切换标签时我们不希望把程序写入当成用户编辑再回填一次。
 */

import { useEffect, useRef } from 'react'

import { languageForPath, monaco } from '../monaco-setup'
import { clearReveal, setEditorSelection, updateContent, useEditor } from '../store'

/**
 * 把 Monaco 的选区读成 store 里那对真实行号。
 *
 * 只有「真的选中了东西」才上报：光标停在某一行时 `isEmpty()` 为真，此时的
 * `startLineNumber` 只是一个光标位置，报上去会凭空多出一条用户没选过的单行引用。
 * 行号取 1-based（Monaco 与内核 `ContextRef` 的约定正好一致），不在这里做任何换算。
 */
function reportSelection(
  editor: monaco.editor.IStandaloneCodeEditor | null,
  path: string | null
): void {
  if (!editor || !path) {
    setEditorSelection(null)
    return
  }
  const selection = editor.getSelection()
  if (!selection || selection.isEmpty()) {
    setEditorSelection(null)
    return
  }
  setEditorSelection({
    path,
    startLine: Math.min(selection.selectionStartLineNumber, selection.positionLineNumber),
    endLine: Math.max(selection.selectionStartLineNumber, selection.positionLineNumber),
    // 列也一并上报：Inline Edit（Ctrl+K）按列精确替换，只给行范围会把整行连同缩进
    // 一起重写。Monaco 的列是 1-based，内核 `TextSelection` 是 0-based，这里减 1。
    // 用 `selectionStart` / `position` 而不是 `start*` / `end*`：前三者是「已按方向
    // 归一」的那一对，反着拖（从下往上选）时也不会把起止行弄反。
    startColumn: Math.max(selection.selectionStartColumn - 1, 0),
    endColumn: Math.max(selection.positionColumn - 1, 0)
  })
}

export function CodeEditor() {
  const state = useEditor()
  const hostRef = useRef<HTMLDivElement | null>(null)
  const editorRef = useRef<monaco.editor.IStandaloneCodeEditor | null>(null)
  const modelsRef = useRef(new Map<string, monaco.editor.ITextModel>())
  const viewsRef = useRef(new Map<string, monaco.editor.ICodeEditorViewState | null>())
  const currentRef = useRef<string | null>(null)
  const applyingRef = useRef(false)

  const activeTab = state.tabs.find((tab) => tab.path === state.activePath) ?? null

  // 建编辑器（只建一次，宿主节点常驻）
  useEffect(() => {
    const host = hostRef.current
    if (!host) return

    const editor = monaco.editor.create(host, {
      theme: 'ai-ide-dark',
      automaticLayout: true,
      fontSize: 13,
      lineHeight: 20,
      fontFamily: 'Consolas, "Cascadia Mono", "Courier New", monospace',
      minimap: { enabled: true, renderCharacters: false },
      scrollBeyondLastLine: false,
      renderWhitespace: 'selection',
      tabSize: 2,
      smoothScrolling: true,
      fixedOverflowWidgets: true
    })
    editorRef.current = editor
    currentRef.current = null

    const subscription = editor.onDidChangeModelContent(() => {
      if (applyingRef.current) return
      const path = currentRef.current
      if (path) updateContent(path, editor.getValue())
    })

    // 选区变化就是「编辑器侧引用」的数据源：只如实上报，怎么用交给对话面板。
    const selectionSubscription = editor.onDidChangeCursorSelection(() => {
      reportSelection(editorRef.current, currentRef.current)
    })

    // 卸载时 ref 可能已被改写，先取快照再清理
    const models = modelsRef.current
    const views = viewsRef.current

    return () => {
      subscription.dispose()
      selectionSubscription.dispose()
      editor.dispose()
      for (const model of models.values()) model.dispose()
      models.clear()
      views.clear()
      editorRef.current = null
      currentRef.current = null
    }
  }, [])

  // 同步活动标签 → 模型
  useEffect(() => {
    const editor = editorRef.current
    if (!editor) return

    // 已关闭标签的模型/视图状态及时释放
    for (const [path, model] of [...modelsRef.current]) {
      if (!state.tabs.some((tab) => tab.path === path)) {
        model.dispose()
        modelsRef.current.delete(path)
        viewsRef.current.delete(path)
      }
    }

    const active = state.activePath
    const previous = currentRef.current
    if (previous && previous !== active) {
      viewsRef.current.set(previous, editor.saveViewState())
    }

    if (!active) {
      currentRef.current = null
      editor.setModel(null)
      reportSelection(null, null)
      return
    }

    const tab = state.tabs.find((item) => item.path === active)
    if (!tab || tab.loading) return

    let model = modelsRef.current.get(active)
    if (!model) {
      model = monaco.editor.createModel(tab.content, languageForPath(active), monaco.Uri.file(active))
      modelsRef.current.set(active, model)
    } else if (model.getValue() !== tab.content) {
      // 磁盘侧变了（外部改动 / 重新加载）→ 覆盖模型
      applyingRef.current = true
      model.setValue(tab.content)
      applyingRef.current = false
    }

    if (editor.getModel() !== model) {
      editor.setModel(model)
      const view = viewsRef.current.get(active)
      if (view) editor.restoreViewState(view)
      editor.focus()
    }
    currentRef.current = active
    // 换文件 / 换模型之后 Monaco 的光标位置属于新内容，不是用户选过的东西：
    // 以当前真实选区重报一次（多半是空选），免得 chip 拿着上一个文件的行号。
    reportSelection(editor, active)
  }, [state.activePath, state.tabs])

  // 搜索命中跳转
  useEffect(() => {
    const editor = editorRef.current
    const reveal = state.reveal
    if (!editor || !reveal || reveal.path !== state.activePath) return
    const position = { lineNumber: Math.max(1, reveal.line), column: Math.max(1, reveal.column) }
    editor.setPosition(position)
    editor.revealPositionInCenter(position)
    editor.focus()
    clearReveal()
  }, [state.reveal, state.activePath])

  const showHost = activeTab !== null

  return (
    <div className="editor">
      <div
        className="editor__host"
        ref={hostRef}
        style={{ display: showHost ? 'block' : 'none' }}
      />
      {activeTab?.error && <div className="editor__banner">{activeTab.error}</div>}
      {!showHost && (
        <div className="editor__empty">
          <p>从左侧文件树打开文件</p>
          <p className="editor__empty-hint">Ctrl+P 按关键字搜索文件与内容</p>
        </div>
      )}
      {activeTab?.loading && <div className="editor__empty">读取中…</div>}
    </div>
  )
}
