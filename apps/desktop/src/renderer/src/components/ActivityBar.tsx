/** 左侧活动栏：资源管理器 / 搜索 / AI / 检查点 / 权限五个视图入口 + 底部终端开关。 */

import { setActivity, useEditor, type ActivityView } from '../store'
import { toggleTerminalPanel, useTerminal } from '../terminal-store'

interface ActivityItem {
  id: ActivityView
  label: string
  glyph: string
}

const ITEMS: ActivityItem[] = [
  { id: 'explorer', label: '资源管理器', glyph: '▤' },
  { id: 'search', label: '搜索', glyph: '⌕' },
  { id: 'ai', label: 'AI', glyph: '✦' },
  { id: 'checkpoints', label: '检查点', glyph: '⟲' },
  { id: 'permissions', label: '权限', glyph: '⚿' }
]

export function ActivityBar() {
  const { activity } = useEditor()
  const terminal = useTerminal()

  return (
    <nav className="activity" aria-label="活动栏">
      {ITEMS.map((item) => (
        <button
          key={item.id}
          type="button"
          className={`activity__item${activity === item.id ? ' is-active' : ''}`}
          title={item.label}
          aria-label={item.label}
          aria-pressed={activity === item.id}
          onClick={() => setActivity(item.id)}
        >
          <span aria-hidden="true">{item.glyph}</span>
        </button>
      ))}
      {/*
        终端不是侧栏视图，是底部面板：点它是「开/关面板」，所以不参与 `activity`
        的互斥高亮。会话数用角标显示，收起面板后也能看出后台还有几个在跑。
      */}
      <button
        type="button"
        className={`activity__item activity__item--terminal${terminal.open ? ' is-active' : ''}`}
        title={terminal.open ? '收起终端面板' : '打开终端面板'}
        aria-label="终端"
        aria-pressed={terminal.open}
        onClick={() => void toggleTerminalPanel()}
      >
        <span aria-hidden="true">{'>_'}</span>
        {terminal.tabs.length > 0 && (
          <span className="activity__badge" aria-hidden="true">
            {terminal.tabs.length}
          </span>
        )}
      </button>
    </nav>
  )
}
