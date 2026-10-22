/**
 * 底部终端面板（M6 切片二）。
 *
 * 结构：标签条 + 会话区。每个标签一个 xterm 实例，实例**跟随标签存活**——
 * 切到别的标签只是 `display: none`，不销毁、不重放：xterm 自己有屏幕缓冲，
 * 重建实例就得让内核补发历史（协议里没有回放接口），所以这里靠隐藏保住滚动区。
 *
 * 与内核的边界：
 * - 输出：`subscribeOutput(id)` 直接拿 `Uint8Array` 调 `term.write`，绕过 React。
 * - 输入：`term.onData` → base64 → `terminal.write`（按键原样透传，含 Ctrl-C）。
 * - 尺寸：`FitAddon` 按容器算行列 → `term.onResize` → `terminal.resize`；
 *   面板尺寸变化用 `ResizeObserver` 触发重新 fit。
 * - 退出：`terminal.exit` 只改 store 里的标签状态，这里据此禁写并显示退出条，
 *   用户可「重启」（新会话占原坑位）或「关闭」（摘标签 + kill）。
 */

import { useEffect, useRef } from 'react'

import { FitAddon } from '@xterm/addon-fit'
import { Terminal } from '@xterm/xterm'
import '@xterm/xterm/css/xterm.css'

import {
  closeTerminalPanel,
  closeTerminalTab,
  dismissTerminalError,
  newTerminalTab,
  restartTerminalTab,
  setActiveTerminal,
  subscribeOutput,
  syncTerminalSize,
  useTerminal,
  type TerminalTab
} from '../terminal-store'
import { writeTerminal } from '../terminal-api'

/** 与外壳配色对齐的 xterm 主题（背景取自 `--bg`）。 */
const TERMINAL_THEME = {
  background: '#14161a',
  foreground: '#e6e9ef',
  cursor: '#5aa9ff',
  selectionBackground: '#2c333f',
  black: '#14161a',
  red: '#ff6b6b',
  green: '#4fd18b',
  yellow: '#f0b429',
  blue: '#5aa9ff',
  magenta: '#c792ea',
  cyan: '#7fdbca',
  white: '#e6e9ef'
}

interface PaneProps {
  tab: TerminalTab
  active: boolean
}

function TerminalPane({ tab, active }: PaneProps) {
  const hostRef = useRef<HTMLDivElement | null>(null)
  const termRef = useRef<Terminal | null>(null)
  const fitRef = useRef<FitAddon | null>(null)
  // 输入闸门跟着状态走：用 ref 而不是闭包里的 `tab.status`，
  // 否则挂载时是 running、之后退了，回调里还是旧值。
  const writableRef = useRef(tab.status === 'running')

  useEffect(() => {
    writableRef.current = tab.status === 'running'
  }, [tab.status])

  // 实例只在会话 id 变化时重建（重启会换 id，正好换掉整个实例）。
  useEffect(() => {
    const host = hostRef.current
    if (!host) return

    const term = new Terminal({
      // 内核发的是 `\n`/`\r\n` 混着的原始字节，交给 convertEol 统一成换行，
      // 不然 PowerShell 的输出会变成阶梯状。
      convertEol: true,
      cursorBlink: true,
      fontSize: 12,
      fontFamily: 'Consolas, "Cascadia Mono", "Courier New", monospace',
      scrollback: 5000,
      theme: TERMINAL_THEME
    })
    const fit = new FitAddon()
    term.loadAddon(fit)
    term.open(host)
    termRef.current = term
    fitRef.current = fit

    // 首帧容器可能还没量出尺寸（面板刚展开），fit 抛错不算问题。
    try {
      fit.fit()
    } catch {
      // 忽略：ResizeObserver 的第一轮回调会补上
    }

    // 已经有历史缓冲的会话（后台跑过一段）会在这里同步灌回屏幕。
    const offOutput = subscribeOutput(tab.id, (bytes) => term.write(bytes))

    const dataSub = term.onData((data) => {
      if (!writableRef.current) return
      void writeTerminal(tab.id, data).catch(() => {
        // 写失败多半是会话刚退出；`terminal.exit` 会把标签标成已退出并提示。
      })
    })

    const resizeSub = term.onResize(({ cols, rows }) => {
      void syncTerminalSize(tab.id, cols, rows)
    })

    // 首次 fit 之后的真实尺寸补报一次：内核建会话时用的是 80×24 的缺省值。
    if (writableRef.current) void syncTerminalSize(tab.id, term.cols, term.rows)

    const observer = new ResizeObserver(() => {
      try {
        fit.fit()
      } catch {
        // 容器被隐藏（切到后台标签）时 fit 会抛，跳过即可
      }
    })
    observer.observe(host)

    return () => {
      observer.disconnect()
      offOutput()
      dataSub.dispose()
      resizeSub.dispose()
      term.dispose()
      termRef.current = null
      fitRef.current = null
    }
  }, [tab.id])

  // 从后台切回来时容器尺寸可能变过，补一次 fit。
  useEffect(() => {
    if (!active) return
    const fit = fitRef.current
    if (!fit) return
    try {
      fit.fit()
    } catch {
      // 同上，隐藏状态下拟合没有意义
    }
  }, [active])

  return (
    <div className="terminal__pane" style={{ display: active ? 'block' : 'none' }}>
      <div className="terminal__host" ref={hostRef} />
      {tab.status === 'exited' && (
        <div className="terminal__exit" role="status">
          <span>
            进程已退出
            {tab.exitCode !== null ? `（退出码 ${tab.exitCode}）` : ''}
            {tab.signal ? `（信号 ${tab.signal}）` : ''}
          </span>
          <button type="button" className="btn" onClick={() => void restartTerminalTab(tab.id)}>
            重启
          </button>
          <button type="button" className="btn" onClick={() => void closeTerminalTab(tab.id)}>
            关闭
          </button>
        </div>
      )}
    </div>
  )
}

/** 标签上的状态点：跑着是绿的，退了是灰的。 */
function statusDot(tab: TerminalTab): string {
  if (tab.status === 'running') return '●'
  if (tab.status === 'starting') return '◌'
  return '○'
}

export function TerminalPanel() {
  const state = useTerminal()

  if (!state.open) return null

  return (
    <section className="terminal" aria-label="终端">
      <div className="terminal__tabs">
        {state.tabs.map((tab) => (
          <div
            key={tab.id}
            className={`terminal__tab${tab.id === state.activeId ? ' is-active' : ''}${
              tab.status === 'exited' ? ' is-exited' : ''
            }`}
          >
            <button
              type="button"
              className="terminal__tab-label"
              title={`${tab.shell}${tab.cwd ? ` — ${tab.cwd}` : ''}`}
              onClick={() => setActiveTerminal(tab.id)}
            >
              <span className="terminal__dot" aria-hidden="true">
                {statusDot(tab)}
              </span>
              {tab.title}
              {tab.status === 'exited' && tab.exitCode !== null ? ` (${tab.exitCode})` : ''}
            </button>
            <button
              type="button"
              className="terminal__tab-close"
              aria-label={`关闭 ${tab.title}`}
              title="关闭（终止该会话）"
              onClick={() => void closeTerminalTab(tab.id)}
            >
              ×
            </button>
          </div>
        ))}
        <button
          type="button"
          className="terminal__action"
          title="新建终端"
          aria-label="新建终端"
          disabled={state.busy}
          onClick={() => void newTerminalTab()}
        >
          {state.busy ? '…' : '+'}
        </button>
        <span className="terminal__spacer" />
        <button
          type="button"
          className="terminal__action"
          title="收起面板（会话保留）"
          aria-label="收起终端面板"
          onClick={() => closeTerminalPanel()}
        >
          ▾
        </button>
      </div>

      {state.error && (
        <div className="terminal__error" role="alert">
          <span>{state.error}</span>
          <button type="button" className="btn" onClick={() => dismissTerminalError()}>
            知道了
          </button>
        </div>
      )}

      <div className="terminal__body">
        {state.tabs.length === 0 ? (
          <p className="panel__hint">还没有终端会话，点「+」新建一个。</p>
        ) : (
          state.tabs.map((tab) => (
            <TerminalPane key={tab.id} tab={tab} active={tab.id === state.activeId} />
          ))
        )}
      </div>
    </section>
  )
}
