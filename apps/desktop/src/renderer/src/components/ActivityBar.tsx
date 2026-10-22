/** 左侧活动栏：资源管理器 / 搜索 / AI / 检查点 / 权限五个视图入口。 */

import { setActivity, useEditor, type ActivityView } from '../store'

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
    </nav>
  )
}
