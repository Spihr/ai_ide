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

import { lspProviders, registerLspProviders, type ZeroBasedRange } from '../lsp-providers'
import { languageForPath, monaco } from '../monaco-setup'
import {
  clearReveal,
  getEditorState,
  setEditorSelection,
  updateContent,
  useEditor
} from '../store'
import { runTabCompletion } from '../tab-completion'
import { workspaceApi } from '../workspace-api'

/**
 * 绝对路径 → 相对工作区根的路径（`/` 分隔）。
 *
 * 符号索引里的 `Symbol.path` 是相对路径，而编辑器只知道绝对路径；把当前文件也算成
 * 相对路径，`toTabCandidates` 才能判断「这个符号是不是就在本文件里」。
 * 不在工作区内（理论上不该发生）返回 `null`，此时不做本文件优先，不影响补全。
 */
function relativeToRoot(root: string, path: string): string | null {
  const normalize = (value: string): string => value.replace(/\\/g, '/').replace(/\/+$/, '')
  const rootNorm = normalize(root)
  const fileNorm = path.replace(/\\/g, '/')
  if (rootNorm.length === 0) return null
  if (fileNorm === rootNorm) return ''
  const prefix = `${rootNorm}/`
  if (!fileNorm.startsWith(prefix)) return null
  return fileNorm.slice(prefix.length)
}

/**
 * 建议列表是否已经展开（用户按了 `Ctrl+空格`）。
 *
 * Monaco 不接受公开查询接口，只能看它的 widget 是否带 `visible` 类——这里只读、
 * 不改任何状态。展开时 Tab 属于「接受选中项」，必须让给 Monaco，否则会和
 * `Ctrl+空格` 的补全流程打架。查不到一律算「未展开」。
 */
function suggestWidgetVisible(): boolean {
  return document.querySelector('.suggest-widget.visible') !== null
}

/**
 * 把 Tab 原样交回 Monaco。
 *
 * 接线的 action 抢占了 `Tab`，不显式触发内置 `tab` 命令的话，缩进、反向缩进、
 * 片段占位符跳转都会失效——那是最不能接受的一种「帮倒忙」。
 */
function defaultTab(editor: monaco.editor.ICodeEditor): void {
  editor.trigger('aiide.tabCompletion', 'tab', null)
}

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

    // 四个语言服务 provider（定义 / 引用 / 悬停 / 补全）挂在编辑器初始化这一处：
    // Monaco 的 provider 注册是全局的、与具体 editor 实例无关，所以只注册一次。
    // 幂等由 `registerLspProviders` 自己保证；卸载时撤销，避免 HMR 之后重复注册
    // 导致每次查询跑两遍、定义列表出现重复项。
    const disposeLspProviders = registerLspProviders(monaco)

    /**
     * Tab 键的唯一入口。
     *
     * 只做四件事：读光标的词、确认当前文件属于某个工作区、把两路候选来源接到
     * `runTabCompletion`、把结果落到编辑器。所有「该不该补、补哪段」的判断都在
     * `tab-completion.ts` 里（纯逻辑、可单测），这里不含业务规则。
     */
    const handleTab = async (instance: monaco.editor.ICodeEditor): Promise<void> => {
      const model = instance.getModel()
      const position = instance.getPosition()
      if (!model || !position) {
        defaultTab(instance)
        return
      }
      // 建议列表已经展开：这一次 Tab 的意思是「接受选中项」，让给 Monaco。
      if (suggestWidgetVisible()) {
        defaultTab(instance)
        return
      }
      const word = model.getWordUntilPosition(position)
      const path = currentRef.current
      const workspace = getEditorState().workspace
      if (path === null || workspace === null) {
        defaultTab(instance)
        return
      }
      // LSP 的替换区间同样要 0-based（与 `lsp-providers` 的中间形态一致）。
      const replaceRange: ZeroBasedRange = {
        start: { line: position.lineNumber - 1, character: word.startColumn - 1 },
        end: { line: position.lineNumber - 1, character: word.endColumn - 1 }
      }
      await runTabCompletion({
        prefix: word.word,
        position,
        currentPath: relativeToRoot(workspace.root, path),
        // 来源一：语言服务（未连接时 `lspProviders` 自己回空数组）。
        // 内核收的路径是正斜杠形式（与 Monaco 的 `pathFromUriPath` 一致），这里先归一。
        completions: () =>
          lspProviders.completion(
            {
              path: path.replace(/\\/g, '/'),
              position: { line: position.lineNumber, character: position.column }
            },
            replaceRange
          ),
        // 来源二：内核符号索引（空串以外的子串查询；未就绪时回空表）。
        symbols: async () =>
          (await workspaceApi.searchSymbols(workspace.workspace_id, word.word)).symbols,
        insert: (text, range) => {
          const now = instance.getPosition()
          const nowWord = now === null ? null : instance.getModel()?.getWordUntilPosition(now)
          // 查询是异步的：回来时用户可能挪了光标或又敲了几个字。这时替换区间已经过期，
          // 宁可什么都不做，也不能把别处的字符换掉。
          if (
            now === null ||
            now.lineNumber !== position.lineNumber ||
            now.column !== position.column ||
            !nowWord ||
            nowWord.word !== word.word
          ) {
            return
          }
          instance.executeEdits('aiide.tabCompletion', [
            {
              range: new monaco.Range(
                range.start.line + 1,
                range.start.character + 1,
                range.end.line + 1,
                range.end.character + 1
              ),
              text,
              forceMoveMarkers: true
            }
          ])
          instance.focus()
        },
        pass: () => defaultTab(instance)
      })
    }

    // Tab 补全：注册在编辑器实例上，`pass` 时显式转交内置 `tab` 命令（缩进照旧）。
    const tabAction = editor.addAction({
      id: 'aiide.tabCompletion',
      label: 'Tab 补全（索引符号 + 语言服务）',
      keybindings: [monaco.KeyCode.Tab],
      run: (instance) => {
        void handleTab(instance)
      }
    })

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
      tabAction.dispose()
      disposeLspProviders()
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
