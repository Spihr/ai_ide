/** 应用外壳：标题栏 + 活动栏 + 侧栏 + 编辑器区 + 状态栏。 */

import { useEffect, useState } from 'react'

import type { CoreStatus } from '../../shared/ipc'
import { ActivityBar } from './components/ActivityBar'
import { CodeEditor } from './components/CodeEditor'
import { EditorTabs } from './components/EditorTabs'
import { FileTree } from './components/FileTree'
import { SearchPanel } from './components/SearchPanel'
import { StatusBar } from './components/StatusBar'
import {
  closeWorkspace,
  dismissNotice,
  initWorkspaceEvents,
  pickAndOpenWorkspace,
  refreshTree,
  saveTab,
  setActivity,
  useEditor
} from './store'

export function App() {
  const state = useEditor()
  const [core, setCore] = useState<CoreStatus | null>(null)

  // 内核状态 + 工作区事件订阅
  useEffect(() => {
    let alive = true
    void window.aiide.getCoreStatus().then((status) => {
      if (alive) setCore(status)
    })
    const offStatus = window.aiide.onCoreStatus((status) => setCore(status))
    const offWorkspace = initWorkspaceEvents()
    return () => {
      alive = false
      offStatus()
      offWorkspace()
    }
  }, [])

  // 窗口级快捷键
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent): void => {
      if (!event.ctrlKey && !event.metaKey) return
      const key = event.key.toLowerCase()
      if (key === 's') {
        event.preventDefault()
        void saveTab()
      } else if (key === 'p') {
        event.preventDefault()
        setActivity('search')
      } else if (key === 'b') {
        event.preventDefault()
        setActivity('explorer')
      }
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [])

  const hasWorkspace = state.workspace !== null

  return (
    <div className="app">
      <header className="titlebar">
        <span className="titlebar__title">{state.workspace?.name ?? 'AI IDE'}</span>
        <span className="titlebar__root" title={state.workspace?.root ?? ''}>
          {state.workspace?.root ?? '未打开工作区'}
        </span>
        <div className="titlebar__actions">
          <button
            type="button"
            className="btn"
            disabled={state.opening}
            onClick={() => void pickAndOpenWorkspace()}
          >
            打开目录
          </button>
          <button
            type="button"
            className="btn"
            disabled={!hasWorkspace}
            onClick={() => void refreshTree()}
          >
            刷新
          </button>
          <button
            type="button"
            className="btn"
            disabled={!state.activePath}
            onClick={() => void saveTab()}
          >
            保存
          </button>
          <button
            type="button"
            className="btn"
            disabled={!hasWorkspace}
            onClick={() => void closeWorkspace()}
          >
            关闭
          </button>
        </div>
      </header>

      <div className="body">
        <ActivityBar />

        <aside className="sidebar">
          <div className="sidebar__header">
            {state.activity === 'explorer' ? '资源管理器' : '搜索'}
          </div>
          {state.workspaceError && <p className="panel__error">{state.workspaceError}</p>}
          {state.activity === 'explorer' ? (
            hasWorkspace ? <FileTree /> : <p className="panel__hint">尚未打开工作区</p>
          ) : (
            <SearchPanel />
          )}
        </aside>

        <main className="editor-area">
          {hasWorkspace ? (
            <>
              <EditorTabs />
              <CodeEditor />
            </>
          ) : (
            <div className="welcome">
              <h1>AI IDE</h1>
              <p>打开一个目录作为工作区，开始编辑。</p>
              <button
                type="button"
                className="btn btn--primary"
                disabled={state.opening}
                onClick={() => void pickAndOpenWorkspace()}
              >
                {state.opening ? '打开中…' : '打开目录…'}
              </button>
              {state.workspaceError && <p className="panel__error">{state.workspaceError}</p>}
            </div>
          )}
        </main>
      </div>

      {state.notice && (
        <div className={`notice notice--${state.notice.kind}`} onClick={dismissNotice} role="alert">
          {state.notice.text}
          <span className="notice__hint">点击关闭</span>
        </div>
      )}

      <StatusBar core={core} />
    </div>
  )
}
