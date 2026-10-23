/**
 * MCP 面板（M7 外壳侧入口）：看 Server 清单、连接 / 断开、浏览工具、手动试跑一次。
 *
 * 分工与 `PermissionPanel` / `GitPanel` 一致：状态与动作在 `mcp-store`，这里只做
 * 「用户点了什么」的转发与确认交互。
 *
 * 两处刻意做成行内二次确认（第一下选中、第二下才发请求）：
 *
 * 1. **断开**：断开会把该 Server 的工具从 agent 注册表下架，正在跑的 Agent 下一轮
 *    就少一批工具；用户得先看清自己断的是哪一个。
 * 2. **试跑**：`mcp.call` 是真的让 Server 去干活（写文件、发请求都有可能），参数是
 *    用户手写的——所以按钮先变「确认执行」，让人再扫一眼参数。
 *
 * 反方向不设确认：连接是只读发现（握手 + 列工具），刷新更是纯读。
 */

import { useEffect, useState } from 'react'

import { serverStateLabel } from '../mcp-api'
import {
  callSelectedTool,
  connectServer,
  disconnectServer,
  dismissMcpHint,
  initMcp,
  refreshMcp,
  selectTool,
  setArgsText,
  useMcp
} from '../mcp-store'

export function McpPanel(): React.JSX.Element {
  const state = useMcp()
  /** 待确认的动作：`disconnect:<server>` 或 `call`。 */
  const [confirming, setConfirming] = useState<string | null>(null)

  // 进入面板时拉一次：`mcp.json` 可能在别处被改过，缓存越久越可能过时。
  useEffect(() => {
    initMcp()
  }, [])

  const selected = state.tools.find((tool) => tool.namespaced_name === state.selectedTool) ?? null

  function onToggleServer(name: string, connected: boolean): void {
    if (!connected) {
      setConfirming(null)
      void connectServer(name)
      return
    }
    if (confirming === `disconnect:${name}`) {
      setConfirming(null)
      void disconnectServer(name)
      return
    }
    setConfirming(`disconnect:${name}`)
  }

  function onCall(): void {
    if (confirming === 'call') {
      setConfirming(null)
      void callSelectedTool()
      return
    }
    setConfirming('call')
  }

  return (
    <div className="ai-section" data-testid="mcp-panel">
      <h3 className="ai-section__title">MCP Server</h3>
      <div className="ai-note">
        Server 清单来自工作区外层的 <code>mcp.json</code>（内核自己读，面板只能连、不能加）。
        连接之后它暴露的工具会以 <code>mcp__&lt;server&gt;__&lt;tool&gt;</code> 注入 Agent
        注册表，和内置工具走同一条权限闸门。
      </div>

      <div className="ai-row">
        <button
          className="ai-button"
          type="button"
          data-testid="mcp-refresh"
          disabled={state.loading}
          onClick={() => void refreshMcp()}
        >
          刷新
        </button>
      </div>

      {state.error !== null ? (
        <div className="ai-result ai-result--bad" data-testid="mcp-error">
          {state.error}
        </div>
      ) : null}

      {state.hint !== null ? (
        <div className="ai-result ai-result--ok" data-testid="mcp-hint">
          {state.hint}
          <button className="ai-button ai-button--tiny" type="button" onClick={dismissMcpHint}>
            知道了
          </button>
        </div>
      ) : null}

      {state.servers.length === 0 ? (
        <p className="panel__hint">
          {state.loading ? '正在读取 Server 清单…' : '还没有配置任何 MCP Server：在 mcp.json 里加一条，再点刷新。'}
        </p>
      ) : (
        <div className="ai-list" data-testid="mcp-servers">
          {state.servers.map((server) => {
            const busy = state.busyServer === server.name
            const waiting = confirming === `disconnect:${server.name}`
            return (
              <div
                className="ai-list__item mcp-server"
                key={server.name}
                data-testid={`mcp-server-${server.name}`}
              >
                <div className="mcp-server__head">
                  <span className="mcp-server__name">{server.name}</span>
                  <span
                    className={`mcp-server__state${server.connected ? ' is-ready' : ''}`}
                    data-testid={`mcp-state-${server.name}`}
                  >
                    {serverStateLabel(server.state)}
                  </span>
                </div>
                <div className="ai-list__meta">
                  {server.transport} · {server.tool_count} 个工具
                  {server.configured ? '' : ' · 配置里已移除'}
                </div>
                {server.last_error != null ? (
                  <div className="ai-result ai-result--bad">{server.last_error}</div>
                ) : null}
                <div className="ai-row">
                  <button
                    className="ai-button ai-button--tiny"
                    type="button"
                    data-testid={`mcp-toggle-${server.name}`}
                    disabled={busy}
                    onClick={() => onToggleServer(server.name, server.connected)}
                  >
                    {busy ? '处理中…' : waiting ? '确认断开' : server.connected ? '断开' : '连接'}
                  </button>
                  {waiting ? (
                    <button
                      className="ai-button ai-button--tiny"
                      type="button"
                      onClick={() => setConfirming(null)}
                    >
                      取消
                    </button>
                  ) : null}
                </div>
              </div>
            )
          })}
        </div>
      )}

      {state.tools.length > 0 ? (
        <>
          <h3 className="ai-section__title">工具（{state.tools.length}）</h3>
          <div className="ai-list" data-testid="mcp-tools">
            {state.tools.map((tool) => (
              <button
                className={`ai-list__item mcp-tool${selected?.namespaced_name === tool.namespaced_name ? ' is-active' : ''}`}
                key={tool.namespaced_name}
                type="button"
                data-testid={`mcp-tool-${tool.namespaced_name}`}
                onClick={() =>
                  selectTool(
                    selected?.namespaced_name === tool.namespaced_name ? null : tool.namespaced_name
                  )
                }
              >
                <span className="mcp-tool__name">{tool.name}</span>
                <span className="ai-list__meta">
                  {tool.server} · {tool.description ?? '（Server 未给说明）'}
                </span>
              </button>
            ))}
          </div>
        </>
      ) : null}

      {selected !== null ? (
        <div className="mcp-call" data-testid="mcp-call">
          <h3 className="ai-section__title">试跑：{selected.namespaced_name}</h3>
          <div className="ai-note">
            参数按 Server 的 inputSchema 手写 JSON 对象。这一下会真的让 Server 干活，
            所以按钮按两次才发请求。
          </div>
          <details className="mcp-schema">
            <summary>inputSchema</summary>
            <pre className="mcp-pre">{JSON.stringify(selected.input_schema, null, 2)}</pre>
          </details>
          <textarea
            className="ai-textarea mcp-args"
            spellCheck={false}
            rows={6}
            value={state.argsText}
            data-testid="mcp-args"
            onChange={(event) => setArgsText(event.target.value)}
          />
          <div className="ai-row">
            <button
              className="ai-button"
              type="button"
              data-testid="mcp-call-run"
              disabled={state.calling}
              onClick={onCall}
            >
              {state.calling ? '执行中…' : confirming === 'call' ? '确认执行' : '执行'}
            </button>
            {confirming === 'call' ? (
              <button className="ai-button ai-button--tiny" type="button" onClick={() => setConfirming(null)}>
                取消
              </button>
            ) : null}
          </div>

          {state.callResult !== null ? (
            <div
              className={`ai-result${state.callResult.isError ? ' ai-result--bad' : ' ai-result--ok'}`}
              data-testid="mcp-call-result"
            >
              <pre className="mcp-pre">{state.callResult.text}</pre>
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  )
}
