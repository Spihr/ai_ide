/**
 * 插件活动视图（P5 切片七）：日志 / 进度 / 状态迁移的倒序流水。
 *
 * 这一层只做展示，归并、上限、筛选全在 `plugin-activity-store` 里（纯函数，可单测）。
 * 三条必须守住的展示口径：
 *
 * - **倒序**：最新的一条在最上面。插件遥测是「刚发生了什么」的问题，把最新的压在
 *   几百行下面等于让人翻日志文件，那不如直接去 `audit.list`。
 * - **进度是「行」不是「行流」**：同插件同 token 在 store 里已合并，这里直接按
 *   `percent` 画一条进度条；没有百分比（内核只给了 message）就只显示文案，不瞎猜。
 * - **自动滚动只在用户贴着顶部时生效**：正在看旧记录的人不该被新日志顶走。
 *
 * 级别筛选只过滤日志——进度与状态被挡掉会让「在跑什么 / 崩没崩」一起消失。
 */

import { useEffect, useRef } from 'react'

import {
  activeProgressCount,
  clearPluginActivity,
  setPluginActivityLevel,
  setPluginActivityOpen,
  usePluginActivity,
  visiblePluginActivity,
  type PluginActivityEntry,
  type PluginActivityLevel
} from '../plugin-activity-store'
import { pluginStateLabel, pluginStateTone } from '../plugin-api'
import { usePlugins } from '../plugin-store'

const LEVEL_OPTIONS: { value: PluginActivityLevel; label: string }[] = [
  { value: 'all', label: '全部' },
  { value: 'debug', label: '调试' },
  { value: 'info', label: '信息' },
  { value: 'warn', label: '警告' },
  { value: 'error', label: '错误' }
]

/** 行内级别标记的文案与配色 token。 */
const LEVEL_TAG: Record<string, { text: string; tone: string }> = {
  debug: { text: 'DEBUG', tone: 'muted' },
  info: { text: 'INFO', tone: 'info' },
  warn: { text: 'WARN', tone: 'warn' },
  error: { text: 'ERROR', tone: 'bad' }
}

/** `HH:MM:SS`；日期不进时间列（活动面板只管最近这段时间）。 */
function formatTime(at: number): string {
  const date = new Date(at)
  const pad = (value: number): string => String(value).padStart(2, '0')
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
}

/** 插件 id → 显示名；清单还没拉到（或插件已被卸载）时退回 id 本身。 */
function usePluginNames(): Map<string, string> {
  const plugins = usePlugins().plugins
  const names = new Map<string, string>()
  for (const plugin of plugins) names.set(plugin.id, plugin.name)
  return names
}

function rowKindLabel(entry: PluginActivityEntry): string {
  if (entry.kind === 'progress') return '进度'
  if (entry.kind === 'state') return '状态'
  return LEVEL_TAG[entry.level ?? 'info']?.text ?? 'INFO'
}

export function PluginActivityPanel(): React.JSX.Element | null {
  const activity = usePluginActivity()
  const names = usePluginNames()
  const listRef = useRef<HTMLDivElement | null>(null)
  const visible = visiblePluginActivity(activity)
  const head = visible[0]
  const headSeq = head?.seq ?? 0
  const headKey = head?.key ?? ''
  const running = activeProgressCount(activity)

  /*
   * 自动滚动：新条目（含进度的第 N 次上报）进来时贴回顶部，但只在用户本来就在
   * 顶部附近时才动——`scrollTop > 24` 说明人在往上翻旧记录，别把人拽走。
   */
  useEffect(() => {
    const element = listRef.current
    if (!element) return
    if (element.scrollTop <= 24) element.scrollTop = 0
  }, [headKey, headSeq, activity.open])

  if (!activity.open) return null

  return (
    <section className="plugin-activity" data-testid="plugin-activity">
      <div className="plugin-activity__head">
        <strong className="plugin-activity__title">插件活动</strong>
        <span className="plugin-activity__count" data-testid="plugin-activity-count">
          {activity.entries.length} 条{running > 0 ? ` · ${running} 项进行中` : ''}
        </span>
        <span className="plugin-activity__spacer" />
        {LEVEL_OPTIONS.map((option) => (
          <button
            className={`ai-button ai-button--tiny${
              activity.level === option.value ? ' is-active' : ''
            }`}
            key={option.value}
            type="button"
            data-testid={`plugin-activity-level-${option.value}`}
            onClick={() => setPluginActivityLevel(option.value)}
          >
            {option.label}
          </button>
        ))}
        <button
          className="ai-button ai-button--tiny"
          type="button"
          data-testid="plugin-activity-clear"
          onClick={clearPluginActivity}
        >
          清空
        </button>
        <button
          className="ai-button ai-button--tiny"
          type="button"
          data-testid="plugin-activity-close"
          onClick={() => setPluginActivityOpen(false)}
        >
          收起
        </button>
      </div>

      {/*
        上限裁掉的条数要说出来：默默少一截会让人以为「插件就这么多日志」。
        上限本身在 store 里（500），这里只负责显示差额。
      */}
      {activity.dropped > 0 ? (
        <div className="plugin-activity__dropped" data-testid="plugin-activity-dropped">
          更早的 {activity.dropped} 条已省略（只保留最近 500 条）
        </div>
      ) : null}

      {visible.length === 0 ? (
        <p className="panel__hint" data-testid="plugin-activity-empty">
          {activity.entries.length === 0
            ? '还没有插件活动：插件装载、调用宿主能力、上报进度时会出现在这里。'
            : '当前级别筛选下没有日志（进度与状态不受筛选影响）。'}
        </p>
      ) : (
        <div className="plugin-activity__list" ref={listRef} data-testid="plugin-activity-list">
          {visible.map((entry) => {
            const name = names.get(entry.pluginId) ?? entry.pluginId
            const isLog = entry.kind === 'log'
            const levelTag = isLog ? LEVEL_TAG[entry.level ?? 'info'] : undefined
            const tone = isLog
              ? (levelTag?.tone ?? 'info')
              : entry.kind === 'state'
                ? pluginStateTone(entry.state ?? 'disabled')
                : 'info'
            return (
              <div
                className={`plugin-activity__row is-${tone}`}
                key={entry.key}
                data-testid={`plugin-activity-row-${entry.kind}`}
                data-key={entry.key}
              >
                <span className="plugin-activity__time">{formatTime(entry.at)}</span>
                <span className={`plugin-activity__badge is-${tone}`}>{rowKindLabel(entry)}</span>
                <span className="plugin-activity__plugin" title={entry.pluginId}>
                  {name}
                </span>
                {entry.kind === 'progress' ? (
                  <>
                    <span className="plugin-activity__percent">
                      {entry.percent === null ? '—' : `${entry.percent}%`}
                    </span>
                    {/*
                      进度条只在有百分比时画：内核允许只给 message 的「心跳式」上报
                      （见 `progress_payload`），没数字就别摆一条空的槽。
                    */}
                    {entry.percent === null ? null : (
                      <span className="plugin-activity__bar">
                        <span
                          className="plugin-activity__bar-fill"
                          style={{ width: `${entry.percent}%` }}
                        />
                      </span>
                    )}
                  </>
                ) : null}
                {entry.kind === 'state' ? (
                  <span className={`plugin-state is-${pluginStateTone(entry.state ?? 'disabled')}`}>
                    {pluginStateLabel(entry.state ?? 'disabled')}
                  </span>
                ) : null}
                <span className="plugin-activity__message">{entry.message}</span>
              </div>
            )
          })}
        </div>
      )}
    </section>
  )
}
