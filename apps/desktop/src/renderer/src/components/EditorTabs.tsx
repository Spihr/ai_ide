/** 编辑器标签栏：脏标记、关闭、中键关闭。 */

import { activateTab, closeTab, useEditor } from '../store'

export function EditorTabs() {
  const { tabs, activePath } = useEditor()

  if (tabs.length === 0) return null

  return (
    <div className="tabs" role="tablist">
      {tabs.map((tab) => {
        const dirty = tab.content !== tab.savedContent
        const active = tab.path === activePath
        return (
          <div
            key={tab.path}
            role="tab"
            aria-selected={active}
            tabIndex={0}
            className={`tab${active ? ' is-active' : ''}`}
            title={tab.path}
            onClick={() => activateTab(tab.path)}
            onKeyDown={(event) => {
              if (event.key === 'Enter' || event.key === ' ') {
                event.preventDefault()
                activateTab(tab.path)
              }
            }}
            onAuxClick={(event) => {
              if (event.button === 1) {
                event.preventDefault()
                closeTab(tab.path)
              }
            }}
          >
            {dirty && <span className="tab__dot" title="未保存" />}
            <span className="tab__name">{tab.name}</span>
            <button
              type="button"
              className="tab__close"
              aria-label={`关闭 ${tab.name}`}
              onClick={(event) => {
                event.stopPropagation()
                closeTab(tab.path)
              }}
            >
              ✕
            </button>
          </div>
        )
      })}
    </div>
  )
}
