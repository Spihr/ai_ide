/** 应用外壳：标题栏 + 活动栏 + 侧栏 + 编辑器区 + 状态栏。 */

import { useEffect, useState } from 'react'

import type { CoreStatus } from '../../shared/ipc'
import { disposeAi, initAi } from './ai-store'
import { ActivityBar } from './components/ActivityBar'
import { AiPanel } from './components/AiPanel'
import { CheckpointPanel } from './components/CheckpointPanel'
import { CodeEditor } from './components/CodeEditor'
import { EditorTabs } from './components/EditorTabs'
import { FileTree } from './components/FileTree'
import { PermissionPanel } from './components/PermissionPanel'
import { SearchPanel } from './components/SearchPanel'
import { StatusBar } from './components/StatusBar'
import { initCheckpoints } from './checkpoint-store'
import { initSessions } from './session-store'
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
    // AI 事件订阅常驻进程级：只在「AI」视图打开时才订阅会漏掉流式事件。
    initAi()
    // M5：会话桥的回调与列表初值也在这里绑一次。必须早于任何一次发送——桥负责
    // 在「第一条消息」落盘前自动建会话，晚绑一帧就会让这一轮对话丢归属。
    initSessions()
    // 检查点列表同理：有工作区就先拉一次，切到「检查点」视图时看到的就是最新的。
    initCheckpoints()
    return () => {
      alive = false
      offStatus()
      offWorkspace()
      disposeAi()
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
            {state.activity === 'explorer'
              ? '资源管理器'
              : state.activity === 'search'
                ? '搜索'
                : state.activity === 'checkpoints'
                  ? '检查点'
                  : state.activity === 'permissions'
                    ? '权限'
                    : 'AI'}
          </div>
          {state.workspaceError &&
            state.activity !== 'ai' &&
            state.activity !== 'checkpoints' &&
            state.activity !== 'permissions' && (
              <p className="panel__error">{state.workspaceError}</p>
            )}
          {state.activity === 'explorer' ? (
            hasWorkspace ? <FileTree /> : <p className="panel__hint">尚未打开工作区</p>
          ) : state.activity === 'search' ? (
            <SearchPanel />
          ) : state.activity === 'checkpoints' ? (
            <CheckpointPanel />
          ) : state.activity === 'permissions' ? (
            <PermissionPanel />
          ) : (
            <AiPanel />
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
