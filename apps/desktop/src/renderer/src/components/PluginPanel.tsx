/**
 * 插件面板（P1/P2/P4 外壳侧入口）：看已装清单、按本地目录安装、装载/停止/重载、
 * 卸载，以及**能力授权/撤销**。
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
 * | `needs_grant` | 卸载 + **详情里逐项授权**；授齐后重新加载才会亮 |
 * | `loading` | 动作全置灰，等内核落账本 |
 *
 * P4 的验收点在详情里的能力表：清单申请了什么、哪几项已授予、缺的那一项点一下就补。
 * 撤销是**两段式**（第一下上膛、第二下才发请求），关键能力（`fs.write` /
 * `net.fetch` / `llm.chat`）的上膛态把「撤了会连带卸载」直接写在按钮旁边——因为内核
 * 撤关键能力会当场 `unload`，事后再解释就晚了。
 *
 * `crashed` 的展示是 P2 的验收点：光有红色标记不够，得说清**为什么崩**（`last_error`
 * 是内核写进账本的原话）与**自动重启过几次**（`restarts`，只在崩溃后自动重启成功时 +1，
 * 每次装载会话最多 3 次）。用户据此判断「再点一次加载」还是「这插件得修」。
 *
 * 面板**不参与**自动重启：崩溃巡检、重试次数、到顶判死全在内核
 * （`crates/plugin/src/host.rs::auto_restart` + `crates/core/src/plugin_api.rs::sweep_crashed`），
 * 外壳只在下一轮 `plugin.list` / `plugin.status` 读到落好的账本。
 *
 * 行内二次确认有两处：**卸载**（删账本 + 删安装目录）与**撤销关键能力**（连带卸载）。
 * 停止（`unload`）不删文件、不撤能力，所以不确认。
 */

import { useEffect, useState } from 'react'

import {
  describeRevokeImpact,
  isKeyCapability,
  pluginCapabilityLabel,
  pluginIssueKindLabel,
  pluginStateLabel,
  pluginStateTone
} from '../plugin-api'
import {
  togglePluginActivity,
  usePluginActivity
} from '../plugin-activity-store'
import {
  cancelRevoke,
  dismissPluginError,
  dismissPluginHint,
  grantCapabilities,
  initPlugins,
  installPlugin,
  loadPlugin,
  refreshPlugins,
  reloadPlugin,
  revokeCapability,
  selectPlugin,
  setInstallPath,
  uninstallPlugin,
  unloadPlugin,
  usePlugins
} from '../plugin-store'

export function PluginPanel(): React.JSX.Element {
  const state = usePlugins()
  const activity = usePluginActivity()
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

  /**
   * 详情里的授权/撤销按钮：任何一个动作在飞就全禁用（同一时刻只允许一个动作）。
   * 单独取一份 `detail`：它是可变属性，箭头函数里读 `state.detail` 收窄不到。
   */
  const detailBusy = state.busyPlugin !== null
  const detail = state.detail

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
        成功后工具才进模型可见的工具表。授权在「详情」里：逐项授权或「全部授权」，
        <strong>撤销关键能力（fs.write / net.fetch / llm.chat）会让插件连带卸载</strong>，所以要确认第二下。
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
        {/* P5：日志/进度流水在右下角浮层里，这里只负责开关开关。 */}
        <button
          className="ai-button"
          type="button"
          data-testid="plugin-activity-toggle"
          onClick={() => togglePluginActivity()}
        >
          活动{activity.entries.length > 0 ? `（${activity.entries.length}）` : ''}
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
                    （docs/14 §4.2 第 4 条）。点它之前得先到详情里把能力授齐。
                  */}
                  {plugin.state === 'needs_grant' ? (
                    <button
                      className="ai-button ai-button--tiny"
                      type="button"
                      data-testid={`plugin-load-${plugin.id}`}
                      disabled
                      title="清单申请的能力还没授齐：内核会拒绝装载（6103）。先在「详情」里授权"
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
          ) : detail === null ? (
            <p className="panel__hint">详情没拉回来，点「详情」重试。</p>
          ) : (
            <>
              {/*
                P4 验收点：能力清单要**能操作**。每行一项，已授予的给「撤销」，
                未授予的给「授权」；撤销是两段式，上膛后这一行换成「确认撤销」+ 影响说明。
                关键能力用 is-warn 的「关键」标记：它决定「撤了会不会连带卸载」，用户
                必须在动手前看见，而不是在回执里才知道。
              */}
              <h4 className="plugin-detail__title">
                能力（已授予 {detail.plugin.granted_capabilities.length} / 申请{' '}
                {detail.plugin.capabilities.length}）
              </h4>
              {detail.plugin.capabilities.length === 0 ? (
                <p className="panel__hint">清单没有申请任何能力：它只能做纯计算，碰不了文件与网络。</p>
              ) : (
                <>
                  <div className="ai-row">
                    <button
                      className="ai-button ai-button--tiny ai-button--primary"
                      type="button"
                      data-testid="plugin-grant-all"
                      disabled={
                        detailBusy ||
                        detail.plugin.capabilities.every((capability) =>
                          detail.plugin.granted_capabilities.includes(capability)
                        )
                      }
                      title="把清单申请的全部能力一次性授予（内核按清单逐项校验）"
                      onClick={() => void grantCapabilities(detail.plugin.id)}
                    >
                      {detailBusy && state.busyAction === 'grant' ? '授权中…' : '全部授权'}
                    </button>
                  </div>
                  <div className="ai-list" data-testid="plugin-capability-list">
                    {detail.plugin.capabilities.map((capability) => {
                      const granted = detail.plugin.granted_capabilities.includes(capability)
                      const armed =
                        state.pendingRevoke !== null &&
                        state.pendingRevoke.pluginId === detail.plugin.id &&
                        state.pendingRevoke.capability === capability
                      const key = isKeyCapability(capability)
                      return (
                        <div
                          className="ai-list__item"
                          key={capability}
                          data-testid={`plugin-cap-${capability}`}
                        >
                          <span className="plugin-item__name">
                            <code>{capability}</code>{' '}
                            <span className={`plugin-state ${granted ? 'is-ok' : 'is-warn'}`}>
                              {granted ? '已授予' : '未授予'}
                            </span>
                            {key ? (
                              <span
                                className="plugin-state is-warn"
                                title="关键能力：撤销会让插件连带卸载（进程停、工具下架）"
                              >
                                关键
                              </span>
                            ) : null}
                          </span>

                          {armed ? (
                            <>
                              <div
                                className="ai-result ai-result--bad"
                                data-testid={`plugin-revoke-impact-${capability}`}
                              >
                                {describeRevokeImpact(capability)}
                              </div>
                              <div className="ai-row">
                                <button
                                  className="ai-button ai-button--tiny ai-button--danger"
                                  type="button"
                                  data-testid={`plugin-revoke-confirm-${capability}`}
                                  disabled={detailBusy}
                                  onClick={() =>
                                    void revokeCapability(detail.plugin.id, capability)
                                  }
                                >
                                  {detailBusy && state.busyAction === 'revoke'
                                    ? '撤销中…'
                                    : key
                                      ? '确认撤销并卸载'
                                      : '确认撤销'}
                                </button>
                                <button
                                  className="ai-button ai-button--tiny"
                                  type="button"
                                  data-testid={`plugin-revoke-cancel-${capability}`}
                                  onClick={cancelRevoke}
                                >
                                  取消
                                </button>
                              </div>
                            </>
                          ) : (
                            <div className="ai-row">
                              {granted ? (
                                <button
                                  className="ai-button ai-button--tiny"
                                  type="button"
                                  data-testid={`plugin-revoke-${capability}`}
                                  disabled={detailBusy}
                                  title={
                                    key
                                      ? '撤销关键能力会让插件连带卸载，第一下只上膛'
                                      : '撤销后下一次调用这项能力会被拒绝，第一下只上膛'
                                  }
                                  onClick={() =>
                                    void revokeCapability(detail.plugin.id, capability)
                                  }
                                >
                                  撤销
                                </button>
                              ) : (
                                <button
                                  className="ai-button ai-button--tiny"
                                  type="button"
                                  data-testid={`plugin-grant-${capability}`}
                                  disabled={detailBusy}
                                  onClick={() => void grantCapabilities(detail.plugin.id, [capability])}
                                >
                                  {detailBusy && state.busyAction === 'grant' ? '授权中…' : '授权'}
                                </button>
                              )}
                            </div>
                          )}
                        </div>
                      )
                    })}
                  </div>
                </>
              )}
              <h4 className="plugin-detail__title">工具（{detail.tools.length}）</h4>
              {detail.tools.length === 0 ? (
                <p className="panel__hint">清单没有声明工具，或安装目录里的清单已不可读。</p>
              ) : (
                <div className="ai-list">
                  {detail.tools.map((tool) => (
                    <div className="ai-list__item" key={tool.namespaced_name}>
                      <span className="plugin-item__name">{tool.name}</span>
                      <span className="ai-list__meta">
                        <code>{tool.namespaced_name}</code> · {tool.risk} · {tool.description}
                      </span>
                    </div>
                  ))}
                </div>
              )}
              <h4 className="plugin-detail__title">命令（{detail.commands.length}）</h4>
              {detail.commands.length === 0 ? (
                <p className="panel__hint">没有声明命令。</p>
              ) : (
                <div className="ai-list">
                  {detail.commands.map((command) => (
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
