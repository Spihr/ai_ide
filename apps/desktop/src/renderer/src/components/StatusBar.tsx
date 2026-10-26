/** 底部状态栏：内核状态 + 自检 + 语言服务 + 当前文件信息。 */

import { useState } from 'react'

import type { CoreStatus } from '../../../shared/ipc'
import { useCoreLog } from '../core-log-store'
import { currentFileDiagnostics, statusLabel, useLspState } from '../lsp-store'
import { useEditor } from '../store'

interface StatusBarProps {
  core: CoreStatus | null
}

/** 状态栏里的日志摘要长度上限：超过就截断，完整内容在 title 里。 */
const LOG_PREVIEW_CHARS = 48

function truncateLog(text: string): string {
  const single = text.replace(/\s+/g, ' ').trim()
  return single.length > LOG_PREVIEW_CHARS ? `${single.slice(0, LOG_PREVIEW_CHARS)}…` : single
}

/** 毫秒 → 状态栏可读文案：一秒以内给毫秒，超过给秒（保留一位小数）。 */
function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return '-'
  return ms < 1000 ? `${Math.round(ms)}ms` : `${(ms / 1000).toFixed(1)}s`
}

const CORE_LABEL: Record<CoreStatus['state'], string> = {
  idle: '未启动',
  starting: '启动中',
  ready: '就绪',
  failed: '异常',
  stopped: '已停止'
}

export function StatusBar({ core }: StatusBarProps) {
  const state = useEditor()
  const lsp = useLspState()
  // 内核日志只显示「最近一行」：状态栏是窄条，塞进滚动列表会把别的指标挤掉。
  // 悬停给完整一行，完整历史在磁盘的 core-shell-*.log 里。
  const coreLog = useCoreLog()
  const latestLog = coreLog.lines[0] ?? null
  const [selfCheck, setSelfCheck] = useState<string | null>(null)
  const [checking, setChecking] = useState(false)

  const active = state.tabs.find((tab) => tab.path === state.activePath) ?? null
  const dirty = active ? active.content !== active.savedContent : false

  // 后台索引提示：内核的 workspace.open 不再等整库遍历，索引没就绪时 stats 是空快照，
  // 直接显示「0 文件 / 0 目录」会被当成「这个工作区是空的」。
  const indexSummary = state.indexSummary
  const indexLabel = !state.workspace
    ? null
    : state.indexing
      ? '索引中…'
      : indexSummary
        ? `索引完成 ${formatDuration(indexSummary.durationMs)}`
        : '索引未就绪'
  const indexTitle =
    indexSummary && !state.indexing
      ? `${indexSummary.incremental ? '增量' : '全量'}索引：${indexSummary.files} 文件 / ${indexSummary.dirs} 目录 / 忽略 ${indexSummary.ignored} 项，耗时 ${formatDuration(indexSummary.durationMs)}`
      : '内核在后台遍历工作区，完成后会自动刷新统计'

  // 语言服务只在「有话说」时占位：`idle`（还没开过文件）不显示，免得空工作区
  // 里挂一条「语言服务器不可用」吓人。`unavailable` / `error` 走 tooltip 说明，
  // 不弹框——没装某个语言服务器是常态，不是故障。
  const lspLabel = statusLabel(lsp)
  const lspProblems = currentFileDiagnostics(lsp)
  const lspTone =
    lsp.status === 'ready'
      ? 'ready'
      : lsp.status === 'error'
        ? 'failed'
        : lsp.status === 'starting'
          ? 'starting'
          : 'idle'

  const runSelfCheck = async (): Promise<void> => {
    setChecking(true)
    setSelfCheck(null)
    try {
      const result = await window.aiide.ping(20)
      setSelfCheck(`往返 20 次 · 均值 ${result.meanMs.toFixed(2)}ms · p95 ${result.p95Ms.toFixed(2)}ms`)
    } catch (error) {
      setSelfCheck(error instanceof Error ? error.message : String(error))
    } finally {
      setChecking(false)
    }
  }

  return (
    <footer className="status">
      <span className={`status__item status__item--${core?.state ?? 'idle'}`}>
        内核 {CORE_LABEL[core?.state ?? 'idle']}
        {core?.pid ? ` · pid ${core.pid}` : ''}
      </span>

      <button
        type="button"
        className="status__button"
        disabled={checking || core?.state !== 'ready'}
        onClick={() => void runSelfCheck()}
      >
        {checking ? '自检中…' : '往返自检'}
      </button>

      {selfCheck && <span className="status__item">{selfCheck}</span>}

      {latestLog && (
        <span
          className="status__item"
          title={`${coreLog.lines.length} 行${coreLog.dropped > 0 ? `（更早的 ${coreLog.dropped} 行已省略）` : ''}：${latestLog.text}`}
        >
          日志 {truncateLog(latestLog.text)}
        </span>
      )}
      {core?.state === 'failed' && core.error && (
        <span className="status__item status__item--failed" title={core.error}>
          {core.error}
        </span>
      )}

      <span className="status__spacer" />

      {lspLabel.length > 0 && (
        <span className={`status__item status__item--${lspTone}`} title={lsp.notice ?? lspLabel}>
          {lspLabel}
          {lspProblems !== null && lspProblems.total > 0
            ? ` · ${lspProblems.problems} 问题/${lspProblems.total} 条`
            : ''}
        </span>
      )}

      {active && (
        <>
          <span className="status__item">{dirty ? '未保存' : '已保存'}{active.saving ? ' · 写入中' : ''}</span>
          <span className="status__item">{active.encoding} · {active.eol.toUpperCase()}</span>
        </>
      )}
      {indexLabel && (
        <span
          className={`status__item status__item--${state.indexing ? 'starting' : 'ready'}`}
          title={indexTitle}
        >
          {indexLabel}
        </span>
      )}
      <span className="status__item">
        {state.workspace
          ? `${state.workspace.stats.files} 文件 / ${state.workspace.stats.dirs} 目录`
          : '无工作区'}
      </span>
    </footer>
  )
}
