/**
 * 插件面板（P1 外壳侧入口）：看已装清单、按本地目录安装、卸载。
 *
 * 分工与 `McpPanel` / `PermissionPanel` 一致：状态与动作在 `plugin-store`，这里只做
 * 「用户点了什么」的转发与确认交互。
 *
 * P1 的边界写进界面本身：清单合法、包已复制、账本已记账，但**没有启动任何进程**。
 * `load` / `grant` 之类的方法内核还回 `6101`，主进程白名单也没放行，所以这里
 * 不摆那些按钮——摆一个必然失败的「加载」，用户只会以为插件坏了。
 *
 * 唯一的行内二次确认是**卸载**：它会删掉账本条目与安装目录，属于不可撤销的动作，
 * 第一下只把按钮变成「确认卸载」。
 */

import { useEffect, useState } from 'react'

import {
  pluginCapabilityLabel,
  pluginIssueKindLabel,
  pluginStateLabel,
  pluginStateTone
} from '../plugin-api'
import {
  dismissPluginError,
  dismissPluginHint,
  initPlugins,
  installPlugin,
  refreshPlugins,
  selectPlugin,
  setInstallPath,
  uninstallPlugin,
  usePlugins
} from '../plugin-store'

export function PluginPanel(): React.JSX.Element {
  const state = usePlugins()
  /** 待确认的卸载：插件 id。 */
  const [confirming, setConfirming] = useState<string | null>(null)

  // 进入面板时拉一次：插件可能在别的窗口/命令行里装过，缓存越久越可能过时。
  useEffect(() => {
    initPlugins()
  }, [])

  const selected =
    state.selectedId === null
      ? null
      : (state.plugins.find((plugin) => plugin.id === state.selectedId) ?? null)

  function onUninstall(pluginId: string): void {
    if (confirming === pluginId) {
      setConfirming(null)
      void uninstallPlugin(pluginId)
      return
    }
    setConfirming(pluginId)
  }

  return (
    <div className="ai-section" data-testid="plugin-panel">
      <h3 className="ai-section__title">插件</h3>
      <div className="ai-note">
        P1 只做「清单与安装」：安装 = 校验清单 → 复制包目录 → 记账本，<strong>全程不启动进程</strong>。
        加载、授权与工具调用要等 P2/P4，因此这里没有「启用」按钮——不是漏了，是还没实现。
      </div>

      <div className="ai-note">
        填含 <code>plugin.json</code> 的那一层包目录绝对路径；同 id 再装一次是覆盖升级，
        旧版本已授予的能力会保留，新增能力一律退回「待授权」。
      </div>
      <input
        className="ai-input"
        type="text"
        spellCheck={false}
        placeholder="E:\my-plugins\hello"
        value={state.installPath}
        data-testid="plugin-install-path"
        onChange={(event) => setInstallPath(event.target.value)}
        onKeyDown={(event) => {
          if (event.key !== 'Enter') return
          void installPlugin()
        }}
      />
      <div className="ai-row">
        <button
          className="ai-button"
          type="button"
          data-testid="plugin-install"
          disabled={state.installing}
          onClick={() => void installPlugin()}
        >
          {state.installing ? '安装中…' : '安装'}
        </button>
        <button
          className="ai-button"
          type="button"
          data-testid="plugin-refresh"
          disabled={state.loading}
          onClick={() => void refreshPlugins()}
        >
          刷新
        </button>
      </div>

      {state.error !== null ? (
        <div className="ai-result ai-result--bad" data-testid="plugin-error">
          <div>{state.error}</div>
          {state.errorDetails.length > 0 ? (
            // 6107 的验收点：内核一次列全，这里就一次列全——不许只显示第一条。
            <ol className="plugin-issues" data-testid="plugin-error-details">
              {state.errorDetails.map((issue, index) => (
                <li className="plugin-issue" key={`${issue.path}-${index}`}>
                  <span className="plugin-issue__code">{pluginIssueKindLabel(issue.code)}</span>
                  <code className="plugin-issue__path">{issue.path.length > 0 ? issue.path : '$'}</code>
                  <span className="plugin-issue__message">{issue.message}</span>
                </li>
              ))}
            </ol>
          ) : null}
          <button
            className="ai-button ai-button--tiny"
            type="button"
            data-testid="plugin-error-dismiss"
            onClick={dismissPluginError}
          >
            知道了
          </button>
        </div>
      ) : null}

      {state.hint !== null ? (
        <div className="ai-result ai-result--ok" data-testid="plugin-hint">
          {state.hint}
          <button className="ai-button ai-button--tiny" type="button" onClick={dismissPluginHint}>
            知道了
          </button>
        </div>
      ) : null}

      {state.plugins.length === 0 ? (
        <p className="panel__hint">
          {state.loading ? '正在读取插件清单…' : '还没有安装任何插件：在上面填包目录，再点「安装」。'}
        </p>
      ) : (
        <div className="ai-list" data-testid="plugin-list">
          {state.plugins.map((plugin) => {
            const busy = state.busyPlugin === plugin.id
            const waiting = confirming === plugin.id
            const isSelected = state.selectedId === plugin.id
            return (
              <div
                className={`ai-list__item plugin-item${isSelected ? ' is-active' : ''}`}
                key={plugin.id}
                data-testid={`plugin-${plugin.id}`}
              >
                <div className="plugin-item__head">
                  <span className="plugin-item__name">{plugin.name}</span>
                  <span className="plugin-item__version">v{plugin.version}</span>
                  <span
                    className={`plugin-state is-${pluginStateTone(plugin.state)}`}
                    data-testid={`plugin-state-${plugin.id}`}
                  >
                    {pluginStateLabel(plugin.state)}
                  </span>
                </div>
                <div className="ai-list__meta">
                  <code>{plugin.id}</code> · {plugin.tool_count} 个工具
                  {plugin.description !== undefined && plugin.description.length > 0
                    ? ` · ${plugin.description}`
                    : ''}
                </div>
                <div className="ai-list__meta" data-testid={`plugin-caps-${plugin.id}`}>
                  {plugin.capabilities.length === 0
                    ? '未申请任何能力'
                    : plugin.capabilities
                        .map((capability) =>
                          pluginCapabilityLabel(capability, plugin.granted_capabilities)
                        )
                        .join('、')}
                  {plugin.capabilities.length > 0 &&
                  plugin.granted_capabilities.length < plugin.capabilities.length
                    ? '（带 ? 的是尚未授予）'
                    : ''}
                </div>
                {plugin.last_error != null ? (
                  <div className="ai-result ai-result--bad">{plugin.last_error}</div>
                ) : null}
                <div className="ai-row">
                  <button
                    className="ai-button ai-button--tiny"
                    type="button"
                    data-testid={`plugin-detail-${plugin.id}`}
                    disabled={state.detailLoading && isSelected}
                    onClick={() => void selectPlugin(isSelected ? null : plugin.id)}
                  >
                    {isSelected ? '收起详情' : '详情'}
                  </button>
                  <button
                    className="ai-button ai-button--tiny"
                    type="button"
                    data-testid={`plugin-uninstall-${plugin.id}`}
                    disabled={busy}
                    onClick={() => onUninstall(plugin.id)}
                  >
                    {busy ? '处理中…' : waiting ? '确认卸载' : '卸载'}
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

      {selected !== null ? (
        <div className="plugin-detail" data-testid="plugin-detail">
          <h3 className="ai-section__title">
            详情：{selected.name} v{selected.version}
          </h3>
          {state.detailLoading ? (
            <p className="panel__hint">正在读取插件详情…</p>
          ) : state.detail === null ? (
            <p className="panel__hint">详情没拉回来，点「详情」重试。</p>
          ) : (
            <>
              <div className="ai-list__meta">
                已授予 {state.detail.plugin.granted_capabilities.length} 项 / 申请{' '}
                {state.detail.plugin.capabilities.length} 项
              </div>
              <h4 className="plugin-detail__title">工具（{state.detail.tools.length}）</h4>
              {state.detail.tools.length === 0 ? (
                <p className="panel__hint">清单没有声明工具，或安装目录里的清单已不可读。</p>
              ) : (
                <div className="ai-list">
                  {state.detail.tools.map((tool) => (
                    <div className="ai-list__item" key={tool.namespaced_name}>
                      <span className="plugin-item__name">{tool.name}</span>
                      <span className="ai-list__meta">
                        <code>{tool.namespaced_name}</code> · {tool.risk} · {tool.description}
                      </span>
                    </div>
                  ))}
                </div>
              )}
              <h4 className="plugin-detail__title">命令（{state.detail.commands.length}）</h4>
              {state.detail.commands.length === 0 ? (
                <p className="panel__hint">没有声明命令。</p>
              ) : (
                <div className="ai-list">
                  {state.detail.commands.map((command) => (
                    <div className="ai-list__item" key={command.name}>
                      <span className="plugin-item__name">{command.title}</span>
                      <span className="ai-list__meta">
                        <code>{command.name}</code> · {command.description ?? '（清单未给说明）'}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </>
          )}
        </div>
      ) : null}
    </div>
  )
}
