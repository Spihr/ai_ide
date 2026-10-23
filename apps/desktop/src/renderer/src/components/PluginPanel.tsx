/**
 * 插件面板（P1/P2 外壳侧入口）：看已装清单、按本地目录安装、装载/停止/重载、卸载。
 *
 * 分工与 `McpPanel` / `PermissionPanel` 一致：状态与动作在 `plugin-store`，这里只做
 * 「用户点了什么」的转发与确认交互。
 *
 * 按钮按账本状态给，不给「点了必然失败」的按钮：
 *
 * | 状态 | 能做什么 |
 * | --- | --- |
 * | `disabled`（装好没跑/手动停了） | 加载、卸载 |
 * | `ready` | 停止、重载、卸载 |
 * | `crashed` | 加载（人工重来一次）、重载、卸载；并显示崩溃原因与自动重启次数 |
 * | `needs_grant` | 只有卸载；加载按钮置灰并说明「授权在 P4」——内核会直接回 6103 且不 spawn |
 * | `loading` | 动作全置灰，等内核落账本 |
 *
 * `crashed` 的展示是 P2 的验收点：光有红色标记不够，得说清**为什么崩**（`last_error`
 * 是内核写进账本的原话）与**自动重启过几次**（`restarts`，只在崩溃后自动重启成功时 +1，
 * 每次装载会话最多 3 次）。用户据此判断「再点一次加载」还是「这插件得修」。
 *
 * 面板**不参与**自动重启：崩溃巡检、重试次数、到顶判死全在内核
 * （`crates/plugin/src/host.rs::auto_restart` + `crates/core/src/plugin_api.rs::sweep_crashed`），
 * 外壳只在下一轮 `plugin.list` / `plugin.status` 读到落好的账本。
 *
 * 唯一的行内二次确认是**卸载**：它会删掉账本条目与安装目录，属于不可撤销的动作，
 * 第一下只把按钮变成「确认卸载」。停止（`unload`）不删文件，所以不确认。
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
  loadPlugin,
  refreshPlugins,
  reloadPlugin,
  selectPlugin,
  setInstallPath,
  uninstallPlugin,
  unloadPlugin,
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
        安装 = 校验清单 → 复制包目录 → 记账本，<strong>不启动进程</strong>；点行内「加载」才起子进程并握手，
        成功后工具才进模型可见的工具表。授权（P4）与工具调用（P3）还没开放，所以「待授权」的插件
        暂不能加载。
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
                {plugin.last_error != null && plugin.last_error.length > 0 ? (
                  <div
                    className="ai-result ai-result--bad"
                    data-testid={`plugin-last-error-${plugin.id}`}
                  >
                    <strong>
                      {plugin.state === 'crashed'
                        ? '崩溃原因：'
                        : plugin.state === 'needs_grant'
                          ? '未装载原因：'
                          : '最近一次错误：'}
                    </strong>
                    {plugin.last_error}
                  </div>
                ) : null}
                {/*
                  `restarts` 是「内核自动重启成功」的累计次数（`store.bump_plugin_restarts`
                  只在自动重启成功时 +1）：崩溃后没起来的尝试不算，手动点「加载」也不算。
                  上限 3 次是**每次装载会话**的预算——同一次装载里崩溃三次用完就停手，
                  用户再点「加载」等于重新给一次预算。所以计数可能大于 3，而停在
                  `crashed` 且 `last_error` 写着「已达自动重启上限」才是到顶的那一次。
                */}
                {plugin.state === 'crashed' || plugin.restarts > 0 ? (
                  <div className="ai-list__meta" data-testid={`plugin-restarts-${plugin.id}`}>
                    内核已自动重启 {plugin.restarts} 次（每次装载最多 3 次；到顶停在「已崩溃」，
                    点「加载」可重新开始）
                  </div>
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
                  {/*
                    `needs_grant` 也摆一个「加载」，但**置灰**：内核会回 6103 且不 spawn
                    （docs/14 §4.2 第 4 条），授权入口在 P4。摆而不点，比藏起来少一次困惑。
                  */}
                  {plugin.state === 'needs_grant' ? (
                    <button
                      className="ai-button ai-button--tiny"
                      type="button"
                      data-testid={`plugin-load-${plugin.id}`}
                      disabled
                      title="清单申请的能力还没授齐：内核会拒绝装载（6103），授权面板在 P4"
                    >
                      加载
                    </button>
                  ) : null}
                  {plugin.state === 'disabled' || plugin.state === 'crashed' ? (
                    <button
                      className="ai-button ai-button--tiny"
                      type="button"
                      data-testid={`plugin-load-${plugin.id}`}
                      disabled={busy}
                      onClick={() => void loadPlugin(plugin.id)}
                    >
                      {busy && state.busyAction === 'load' ? '加载中…' : '加载'}
                    </button>
                  ) : null}
                  {plugin.state === 'ready' ? (
                    <button
                      className="ai-button ai-button--tiny"
                      type="button"
                      data-testid={`plugin-unload-${plugin.id}`}
                      disabled={busy}
                      onClick={() => void unloadPlugin(plugin.id)}
                    >
                      {busy && state.busyAction === 'unload' ? '停止中…' : '停止'}
                    </button>
                  ) : null}
                  {plugin.state === 'ready' || plugin.state === 'crashed' ? (
                    <button
                      className="ai-button ai-button--tiny"
                      type="button"
                      data-testid={`plugin-reload-${plugin.id}`}
                      disabled={busy}
                      onClick={() => void reloadPlugin(plugin.id)}
                    >
                      {busy && state.busyAction === 'reload' ? '重载中…' : '重载'}
                    </button>
                  ) : null}
                  <button
                    className="ai-button ai-button--tiny"
                    type="button"
                    data-testid={`plugin-uninstall-${plugin.id}`}
                    disabled={busy}
                    onClick={() => onUninstall(plugin.id)}
                  >
                    {busy && state.busyAction === 'uninstall'
                      ? '卸载中…'
                      : waiting
                        ? '确认卸载'
                        : '卸载'}
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
