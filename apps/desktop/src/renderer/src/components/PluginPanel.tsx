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
 *
 * P5 的「全部启用」（`plugin.load_many`）是**批量**入口：候选=已停用 + 已崩溃，
 * 一次请求交给内核并发起进程。按钮上的数字是候选数（按下去之前就得知道会起几个
 * 子进程），进度取自内核本来就会广播的 `plugin.state_changed`，结果条里失败项逐行
 * 给原因并可单独重试——**一个插件失败不牵连其它插件**，这是这条通道与循环 `load`
 * 的关键差别。
 */

import { useEffect, useState } from 'react'

import type { PluginStatusResult, PluginViewSummary } from '@ai-ide/shared-types'

import {
  describeRevokeImpact,
  isKeyCapability,
  pluginApi,
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
  describePluginBatch,
  dismissPluginBatch,
  dismissPluginError,
  dismissPluginHint,
  grantCapabilities,
  initPlugins,
  installPlugin,
  loadablePluginIds,
  loadPlugin,
  loadPluginsMany,
  refreshPlugins,
  reloadPlugin,
  revokeCapability,
  selectPlugin,
  setInstallPath,
  uninstallPlugin,
  unloadPlugin,
  usePlugins
} from '../plugin-store'
import {
  PLUGIN_VIEW_FIXTURE,
  PLUGIN_VIEW_FIXTURE_COMMANDS
} from '../plugin-view.fixture'
import { parseView, type ViewAction } from '../plugin-view'
import {
  refreshPluginView,
  selectPluginView,
  syncPluginViews,
  usePluginView
} from '../plugin-view-store'
import {
  describeExternalViewError,
  describeExternalViewExit,
  describeExternalViewOpened,
  hasExternalViewport
} from '../plugin-external-view'
import { PluginViewHost } from './PluginViewHost'

/** 详情区二级 tab（docs/24 §7.3：工具 / 命令 / 视图）。 */
type DetailTab = 'tools' | 'commands' | 'views'

const DETAIL_TABS: readonly { id: DetailTab; label: string }[] = [
  { id: 'tools', label: '工具' },
  { id: 'commands', label: '命令' },
  { id: 'views', label: '视图' }
]

export function PluginPanel(): React.JSX.Element {
  const state = usePlugins()
  const activity = usePluginActivity()
  /** 待确认的卸载：插件 id。 */
  const [confirming, setConfirming] = useState<string | null>(null)
  /**
   * 详情区二级 tab：工具 / 命令 / 视图（docs/24 §7.3）。
   *
   * 把 `pluginId` 一起存进来，是为了「切插件就回工具页」这件派生事——用
   * `useEffect` 去 `setState` 会多打一轮渲染，也过不了 `set-state-in-effect`；
   * 这里直接按「记录的 id 是不是当前选中」算出要显示的 tab。
   */
  const [tabState, setTabState] = useState<{ pluginId: string | null; tab: DetailTab }>({
    pluginId: null,
    tab: 'tools'
  })
  const detailTab: DetailTab = tabState.pluginId === state.selectedId ? tabState.tab : 'tools'
  const setDetailTab = (tab: DetailTab): void => setTabState({ pluginId: state.selectedId, tab })

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

  /**
   * 「全部启用」的候选（已停用 + 已崩溃）。
   *
   * 按钮上的数字就是它：用户在按下之前就该知道这一次会起几个进程——装了一堆插件
   * 却在点下去之后才知道要起十几个子进程，是面板最不该有的惊喜。
   */
  const batchCandidates = loadablePluginIds(state.plugins)
  const batchRunning = state.batch?.running === true
  const batchBusy = batchRunning || state.busyPlugin !== null || state.installing

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
        {/*
          P5 切片八：一次请求并发装载。逐个点「加载」是串行的——每个插件一轮握手，
          且第一个超时会把后面全堵住；这条走 `plugin.load_many`，内核并发起进程。
          失败项互不牵连，结果条里逐条列原因。
        */}
        <button
          className="ai-button ai-button--primary"
          type="button"
          data-testid="plugin-load-many"
          disabled={batchBusy || batchCandidates.length === 0}
          title={
            batchCandidates.length === 0
              ? '没有可批量启用的插件：已经在运行的不必再启用，待授权的需先到详情里授权'
              : `并发启用这 ${batchCandidates.length} 个插件：${batchCandidates.join('、')}`
          }
          onClick={() => void loadPluginsMany()}
        >
          {batchRunning
            ? `启用中 ${state.batch?.done ?? 0}/${state.batch?.total ?? 0}`
            : `全部启用${batchCandidates.length > 0 ? `（${batchCandidates.length}）` : ''}`}
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

      {/*
        「全部启用」的结果条：进度（装载中）+ 结局（成功几个/失败哪几个）。
        失败项**逐行**列出，而不是折叠成「3 个失败」——用户要的是「哪个插件、为什么」，
        而且每一行都能单独重试（批量失败不代表这个插件永远起不来，常常是缺能力或
        端口占用，修完点一下就够，不必再整批重来）。
      */}
      {state.batch !== null ? (
        <div
          className={`ai-result ${
            state.batch.failures.length > 0 ? 'ai-result--bad' : 'ai-result--ok'
          }`}
          data-testid="plugin-batch"
        >
          <div data-testid="plugin-batch-progress">{describePluginBatch(state.batch)}</div>
          {state.batch.failures.length > 0 ? (
            <ul className="plugin-issues" data-testid="plugin-batch-failures">
              {state.batch.failures.map((failure) => {
                const known = state.plugins.find((plugin) => plugin.id === failure.pluginId)
                return (
                  <li
                    className="plugin-issue"
                    key={failure.pluginId}
                    data-testid={`plugin-batch-failure-${failure.pluginId}`}
                  >
                    <span className="plugin-issue__code">[{failure.code}]</span>
                    <code className="plugin-issue__path">{known?.name ?? failure.pluginId}</code>
                    <span className="plugin-issue__message">
                      {failure.hint.length > 0
                        ? `${failure.hint}（内核原文：${failure.message}）`
                        : failure.message}
                    </span>
                    <button
                      className="ai-button ai-button--tiny"
                      type="button"
                      data-testid={`plugin-batch-retry-${failure.pluginId}`}
                      disabled={batchBusy}
                      onClick={() => void loadPlugin(failure.pluginId)}
                    >
                      单独重试
                    </button>
                  </li>
                )
              })}
            </ul>
          ) : null}
          {state.batch.running ? null : (
            <button
              className="ai-button ai-button--tiny"
              type="button"
              data-testid="plugin-batch-dismiss"
              onClick={dismissPluginBatch}
            >
              知道了
            </button>
          )}
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
              <div
                className="plugin-detail__tabs"
                role="tablist"
                aria-label="详情分区"
                data-testid="plugin-detail-tabs"
              >
                {DETAIL_TABS.map((tab) => (
                  <button
                    key={tab.id}
                    className={`plugin-detail__tab${detailTab === tab.id ? ' is-active' : ''}`}
                    type="button"
                    role="tab"
                    aria-selected={detailTab === tab.id}
                    data-testid={`plugin-detail-tab-${tab.id}`}
                    onClick={() => setDetailTab(tab.id)}
                  >
                    {tab.label}
                    {tab.id === 'views' ? `（${detail.views.length}）` : null}
                  </button>
                ))}
              </div>

              {detailTab === 'tools' ? (
                <>
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
                </>
              ) : null}

              {detailTab === 'commands' ? (
                <>
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
              ) : null}

              {detailTab === 'views' ? <PluginViewSection detail={detail} /> : null}
            </>
          )}
        </div>
      ) : null}
    </div>
  )
}

/**
 * 详情区的「视图」页（docs/24 §7.3 第 4 刀）。
 *
 * 三件事：把 `plugin.status` 里声明的视图同步进 store、按用户点击拉取并渲染、
 * 把按钮动作转成一次 `plugin.command`（**不在这里执行任何插件代码**，审批与
 * 能力闸门都在内核）。
 *
 * 空态给一个「控件样例」入口：第 3 刀还没有真插件声明视图，没有这个入口，
 * 宿主在界面上就是一段永远看不到的代码——V-4~V-7 也就无从目测。
 */
function PluginViewSection({ detail }: { detail: PluginStatusResult }): React.JSX.Element {
  const viewState = usePluginView()
  const [showFixture, setShowFixture] = useState(false)
  const [busy, setBusy] = useState(false)
  /** 命令回执：成功给正文前 200 字，失败给人话。 */
  const [receipt, setReceipt] = useState<string | null>(null)
  /** 外部视口开关的进行态：一个窗口起得慢，按下就得先禁用，免得连点起两个进程。 */
  const [externalBusy, setExternalBusy] = useState(false)
  /** 外部视口回执（复用 / 新建 / 失败原因），与命令回执分开放，避免互相覆盖。 */
  const [externalNotice, setExternalNotice] = useState<string | null>(null)
  /**
   * 本面板开过、且尚未退出视口的那**一个**视图 id。
   *
   * 存 id 而不是布尔：换个视图这个值自然失效，不需要在 effect 里 setState 复位
   * （那会多渲染一轮）。也只是按钮文案的状态，不是真相来源——接管别人起的实例时
   * 外壳收不到退出事件，这里会一直显示「运行中」，所以文案只说「运行中」。
   */
  const [runningViewId, setRunningViewId] = useState<string | null>(null)

  const pluginId = detail.plugin.id
  const views = detail.views
  const commandNames = detail.commands.map((command) => command.name)
  // 只有**当前选中**的视图才谈得上「打开视口」：tab 上不挂这个入口，
  // 否则用户会以为点一下 tab 就顺手起了进程。
  const activeView = views.find((item) => item.id === viewState.activeViewId) ?? null
  /** 按钮文案用：存 id 的好处就在这一行，换个视图自动失效。 */
  const externalRunning = activeView !== null && runningViewId === activeView.id

  // `views` 是 `plugin.status` 的原样搬运：详情重拉一次就同步一次，
  // 插件被重装/改清单后这里能跟着变。
  useEffect(() => {
    syncPluginViews(pluginId, views)
  }, [pluginId, views])

  // 点进「视图」就是想看内容，第一个视图自动选中并拉取。
  useEffect(() => {
    const first = views[0]
    if (first === undefined) return
    if (viewState.pluginId !== pluginId) return
    if (viewState.activeViewId !== null) return
    void selectPluginView(pluginId, first)
  }, [pluginId, views, viewState.pluginId, viewState.activeViewId])

  // 视口进程退出：只认「当前这个插件 + 当前这个视图」，别的视图开着又被关掉
  // 不该在本文里冒一条提示（用户切回来会莫名其妙）。
  useEffect(() => {
    // `?.`：preload 与渲染进程版本不齐（开发期热重载）时宁可少一条提示，
    // 也不要因为一个可选订阅把整个插件面板炸掉。
    const off = window.aiide.onViewportState?.((event) => {
      if (event.pluginId !== pluginId) return
      if (activeView === null || event.viewId !== activeView.id) return
      setExternalNotice(describeExternalViewExit(activeView.title, event.exitCode))
      setExternalBusy(false)
      setRunningViewId(null)
    })
    return () => off?.()
  }, [pluginId, activeView])

  /**
   * 打开当前视图的外部视口（docs/24 §5.4）。
   *
   * 递出去的只有 `pluginId + viewId`：路径解析、围栏校验、spawn 全在主进程。
   * 渲染侧拿不到管道名，也就没法绕开宿主直接往视口里塞帧。
   */
  async function openExternalViewport(view: PluginViewSummary): Promise<void> {
    setExternalBusy(true)
    setExternalNotice(null)
    try {
      const result = await window.aiide.openExternalViewport(pluginId, view.id)
      setExternalNotice(describeExternalViewOpened(view.title, result.reused))
      setRunningViewId(view.id)
    } catch (error) {
      setExternalNotice(describeExternalViewError(error))
      setRunningViewId(null)
    } finally {
      setExternalBusy(false)
    }
  }

  async function runAction(action: ViewAction): Promise<void> {
    setBusy(true)
    setReceipt(null)
    try {
      const result = await pluginApi.command(pluginId, action.command, action.args)
      const body = result.content.trim()
      setReceipt(
        result.truncated
          ? `${body.slice(0, 200)}…（回执已按 64 KB 截断）`
          : body === ''
            ? `命令 ${action.command} 已执行，无输出。`
            : body.slice(0, 200)
      )
      // 命令往往就是改数据再让视图刷新：rev 变没变由插件说了算，这里只重拉一次。
      void refreshPluginView()
    } catch (error) {
      setReceipt(describeActionError(error))
    } finally {
      setBusy(false)
    }
  }

  if (views.length === 0 || showFixture) {
    const fixture = parseView(PLUGIN_VIEW_FIXTURE)
    return (
      <div data-testid="plugin-view-empty">
        <h4 className="plugin-detail__title">视图（0）</h4>
        <p className="panel__hint">
          {views.length === 0
            ? '清单没有声明视图：这个插件只提供工具与命令，界面由宿主现有面板承载。'
            : '下面是宿主内置的控件样例（不是这个插件的内容），用来确认控件渲染与限界生效。'}
        </p>
        <div className="ai-row">
          <button
            className="ai-button ai-button--tiny"
            type="button"
            data-testid="plugin-view-toggle-fixture"
            onClick={() => setShowFixture((prev) => !prev)}
          >
            {showFixture ? '收起控件样例' : '查看控件样例'}
          </button>
        </div>
        {showFixture ? (
          <>
            <p className="panel__hint">
              样例故意带了一条超长文本、一条 <code>javascript:</code> 单元格与一个未声明的按钮命令：
              截断、降级、禁用三种处理都该在下面看得见。
            </p>
            <PluginViewHost
              view={fixture}
              commands={PLUGIN_VIEW_FIXTURE_COMMANDS}
              onAction={() => setReceipt('控件样例不接动作：它没有对应的插件命令。')}
            />
          </>
        ) : null}
        {receipt === null ? null : (
          <div className="ai-result" data-testid="plugin-view-receipt">
            {receipt}
          </div>
        )}
      </div>
    )
  }

  return (
    <div data-testid="plugin-view-section">
      <div className="ai-row plugin-view__head">
        {views.map((item) => (
          <button
            key={item.id}
            className={`ai-button ai-button--tiny${
              viewState.activeViewId === item.id ? ' ai-button--primary' : ''
            }`}
            type="button"
            data-testid={`plugin-view-tab-${item.id}`}
            title={item.entry === '' ? undefined : `数据来源：${item.entry}`}
            onClick={() => void selectPluginView(pluginId, item)}
          >
            {item.title}
          </button>
        ))}
        <button
          className="ai-button ai-button--tiny"
          type="button"
          data-testid="plugin-view-refresh"
          disabled={viewState.status === 'loading'}
          onClick={() => void refreshPluginView()}
        >
          {viewState.status === 'loading' ? '刷新中…' : '刷新'}
        </button>
      </div>

      {activeView !== null && hasExternalViewport(activeView) ? (
        <div className="ai-row" data-testid="plugin-view-external">
          <button
            className="ai-button ai-button--tiny"
            type="button"
            data-testid="plugin-view-open-external"
            disabled={externalBusy}
            title={`要在独立窗口里查看：${activeView.external?.program ?? ''}`}
            onClick={() => void openExternalViewport(activeView)}
          >
            {externalBusy
              ? '正在打开视口…'
              : externalRunning
                ? `视口运行中（${activeView.title}）`
                : `在视口中打开「${activeView.title}」`}
          </button>
          <span className="panel__hint">外部进程由宿主启动，参数只走管道，不经 shell。</span>
        </div>
      ) : null}

      {externalNotice === null ? null : (
        <div className="ai-result" data-testid="plugin-view-external-notice">
          {externalNotice}
        </div>
      )}

      {viewState.status === 'loading' ? (
        <p className="panel__hint">正在向插件要视图…</p>
      ) : null}

      {viewState.error === null ? null : (
        <div className="ai-result ai-result--bad" data-testid="plugin-view-error">
          {viewState.error}
        </div>
      )}

      {viewState.view === null ? null : (
        <PluginViewHost
          view={viewState.view}
          commands={commandNames}
          busy={busy}
          onAction={(action) => void runAction(action)}
        />
      )}

      {receipt === null ? null : (
        <div className="ai-result" data-testid="plugin-view-receipt">
          {receipt}
        </div>
      )}
    </div>
  )
}

/**
 * 按钮动作失败的人话。
 *
 * `3001` 是**正常路径**：`risk=write` 的插件命令执行前要过审批，视图里的按钮
 * 只是发起方，不是审批人。这一条不翻译成「插件坏了」。
 */
function describeActionError(error: unknown): string {
  const text = error instanceof Error ? error.message : String(error)
  if (text.includes('3001') || text.includes('未被放行')) {
    return '这条命令需要审批才能执行：视图里的按钮只负责发起，审批入口在命令面板里（下一步接入）。'
  }
  return text.trim() === '' ? '命令执行失败（内核未给原因）。' : text
}

