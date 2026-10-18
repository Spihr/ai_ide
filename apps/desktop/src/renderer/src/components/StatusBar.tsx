/** 底部状态栏：内核状态 + 自检 + 当前文件信息。 */

import { useState } from 'react'

import type { CoreStatus } from '../../../shared/ipc'
import { useEditor } from '../store'

interface StatusBarProps {
  core: CoreStatus | null
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
  const [selfCheck, setSelfCheck] = useState<string | null>(null)
  const [checking, setChecking] = useState(false)

  const active = state.tabs.find((tab) => tab.path === state.activePath) ?? null
  const dirty = active ? active.content !== active.savedContent : false

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
      {core?.state === 'failed' && core.error && (
        <span className="status__item status__item--failed" title={core.error}>
          {core.error}
        </span>
      )}

      <span className="status__spacer" />

      {active && (
        <>
          <span className="status__item">{dirty ? '未保存' : '已保存'}{active.saving ? ' · 写入中' : ''}</span>
          <span className="status__item">{active.encoding} · {active.eol.toUpperCase()}</span>
        </>
      )}
      <span className="status__item">
        {state.workspace
          ? `${state.workspace.stats.files} 文件 / ${state.workspace.stats.dirs} 目录`
          : '无工作区'}
      </span>
    </footer>
  )
}
