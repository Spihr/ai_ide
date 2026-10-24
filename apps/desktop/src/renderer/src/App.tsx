/** 应用外壳：标题栏 + 活动栏 + 侧栏 + 编辑器区 + 状态栏。 */

import { useEffect, useState } from 'react'

import type { CoreStatus } from '../../shared/ipc'
import { disposeAi, initAi } from './ai-store'
import { ActivityBar } from './components/ActivityBar'
import { AiPanel } from './components/AiPanel'
import { CheckpointPanel } from './components/CheckpointPanel'
import { CodeEditor } from './components/CodeEditor'
import { CommandPalette } from './components/CommandPalette'
import { EditorTabs } from './components/EditorTabs'
import { FileTree } from './components/FileTree'
import { GitPanel } from './components/GitPanel'
import { McpPanel } from './components/McpPanel'
import { PermissionPanel } from './components/PermissionPanel'
import { PluginPanel } from './components/PluginPanel'
import { SearchPanel } from './components/SearchPanel'
import { StatusBar } from './components/StatusBar'
import { TerminalPanel } from './components/TerminalPanel'
import { initCheckpoints } from './checkpoint-store'
import { togglePalette } from './command-palette-store'
import { initGit } from './git-store'
import { initSessions } from './session-store'
import { initTerminalEvents, toggleTerminalPanel } from './terminal-store'
import {
  closeWorkspace,
  dismissNotice,
  initWorkspaceEvents,
  pickAndOpenWorkspace,
  refreshTree,
  saveTab,
  setActivity,
  useEditor,
  type ActivityView
} from './store'

/** 侧栏标题：与活动栏一一对应，用表而不是嵌套三元——再加一个视图不该动结构。 */
const SIDEBAR_TITLES: Record<ActivityView, string> = {
  explorer: '资源管理器',
  search: '搜索',
  ai: 'AI',
  git: '源代码管理',
  checkpoints: '检查点',
  permissions: '权限',
  mcp: 'MCP Server',
  plugin: '插件'
}

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
    // M6：终端输出与退出事件常驻订阅——面板收起时也要收，否则后台跑完的输出
    // 会在下次展开时整段缺失。
    const offTerminal = initTerminalEvents()
    // M6：Git 状态是「工作区内容」的视图——`git.*` 没有自己的事件，靠 `workspace.changed`
    // 去抖后重拉。订阅常驻，这样活动栏角标在别的视图里也能实时对上。
    const offGit = initGit()
    return () => {
      alive = false
      offStatus()
      offWorkspace()
      offTerminal()
      offGit()
      disposeAi()
    }
  }, [])

  // 窗口级快捷键
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent): void => {
      if (!event.ctrlKey && !event.metaKey) return
      const key = event.key.toLowerCase()
      if (key === 's' && !event.shiftKey) {
        event.preventDefault()
        void saveTab()
      } else if (key === 'p' && event.shiftKey) {
        // Ctrl/Cmd+Shift+P：全局命令面板（FR-1.7）。必须排在纯 Ctrl+P 之前，
        // 否则 shift+p 会先被「切到搜索」吃掉——`event.key` 已经把 shift 归一成 'P'。
        event.preventDefault()
        togglePalette()
      } else if (key === 'p') {
        event.preventDefault()
        setActivity('search')
      } else if (key === 'b') {
        event.preventDefault()
        setActivity('explorer')
      } else if (key === '`' && !event.metaKey) {
        // Ctrl+` 开关终端面板（与 VS Code 一致）；不拦 Meta 组合，避免抢系统快捷键。
        event.preventDefault()
        void toggleTerminalPanel()
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
          <div className="sidebar__header">{SIDEBAR_TITLES[state.activity]}</div>
          {state.workspaceError &&
            state.activity !== 'ai' &&
            state.activity !== 'git' &&
            state.activity !== 'checkpoints' &&
            state.activity !== 'permissions' &&
            // MCP 的 Server 清单来自配置文件、不依赖工作区，没有工作区时照样能用。
            state.activity !== 'mcp' &&
            // 插件清单同理：装在用户数据目录、按绝对路径安装，与工作区无关。
            state.activity !== 'plugin' && (
              <p className="panel__error">{state.workspaceError}</p>
            )}
          {state.activity === 'explorer' ? (
            hasWorkspace ? <FileTree /> : <p className="panel__hint">尚未打开工作区</p>
          ) : state.activity === 'search' ? (
            <SearchPanel />
          ) : state.activity === 'git' ? (
            <GitPanel />
          ) : state.activity === 'checkpoints' ? (
            <CheckpointPanel />
          ) : state.activity === 'permissions' ? (
            <PermissionPanel />
          ) : state.activity === 'mcp' ? (
            <McpPanel />
          ) : state.activity === 'plugin' ? (
            <PluginPanel />
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

      {/* M6：终端是底部面板，横跨整个外壳（在 body 之外），不挤占侧栏与编辑器。 */}
      <TerminalPanel />

      {/* P5：全局命令面板是浮层，放在 body 之外、notice 之前——浮层要压住所有面板。 */}
      <CommandPalette />

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
