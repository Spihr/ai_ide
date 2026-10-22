/**
 * 权限面板（M5 外壳侧入口）：列出「总是允许」留下的免问规则，并支持撤销。
 *
 * 分工与 `SessionPanel` / `CheckpointPanel` 一致：状态与动作在 `permission-store`，
 * 这里只做「用户点了什么」的转发与确认交互。
 *
 * 撤销用行内二次确认（第一下选中、第二下才发请求），理由与检查点回滚相同：撤销会
 * 让某个工具重新开始弹窗，用户得先看清自己撤的是哪一条。
 */

import { useEffect, useState } from 'react'

import { formatRuleTime, scopeLabel, scopeParts } from '../permission-api'
import {
  dismissPermissionHint,
  refreshPermissions,
  revokePermission,
  revokePermissionTool,
  usePermissions
} from '../permission-store'

export function PermissionPanel(): React.JSX.Element {
  const state = usePermissions()
  const [confirming, setConfirming] = useState<string | null>(null)

  // 进入面板时拉一次：权限表会被 Agent 的 `allow_always` 改动，缓存越久越可能过时。
  useEffect(() => {
    void refreshPermissions()
  }, [])

  return (
    <div className="ai-section" data-testid="permission-panel">
      <h3 className="ai-section__title">权限</h3>
      <div className="ai-note">
        这些是你在裁决里点过「总是允许」之后留下的免问范围。撤销之后，同类操作会重新
        回到「先问一次」；面板里不能反向新增——授权只能在裁决时给。
      </div>

      <div className="ai-row">
        <button
          className="ai-button"
          type="button"
          data-testid="permission-refresh"
          disabled={state.loading}
          onClick={() => void refreshPermissions()}
        >
          刷新
        </button>
      </div>

      {state.error !== null ? (
        <div className="ai-result ai-result--bad" data-testid="permission-error">
          {state.error}
        </div>
      ) : null}
      {state.hint !== null ? (
        <div className="ai-result ai-result--ok" data-testid="permission-hint">
          {state.hint}
          <button
            className="ai-button ai-button--tiny"
            type="button"
            data-testid="permission-hint-dismiss"
            onClick={dismissPermissionHint}
          >
            知道了
          </button>
        </div>
      ) : null}

      <ul className="ai-list" data-testid="permission-list">
        {state.rules.length === 0 ? (
          <li className="ai-note">
            {state.loading
              ? '正在加载权限规则…'
              : '还没有免问规则：Agent 每次要动命令或写文件时都会先问你。'}
          </li>
        ) : (
          state.rules.map((rule) => {
            const parts = scopeParts(rule)
            const armed = confirming === rule.id
            const busy = state.revoking !== null
            return (
              <li
                key={rule.id}
                className="ai-list__item"
                data-testid="permission-item"
                title={rule.id}
              >
                <span className="ai-list__meta">
                  {parts.kind}：{parts.detail}
                  <br />
                  <span className="ai-note">
                    工具 {rule.tool_pattern} · 范围 {scopeLabel(rule.scope)} ·{' '}
                    {formatRuleTime(rule.created_at_ms)}
                  </span>
                </span>

                {armed ? (
                  <>
                    <button
                      className="ai-button ai-button--tiny ai-button--danger"
                      type="button"
                      data-testid="permission-revoke-confirm"
                      disabled={busy}
                      title="确认：撤销后同类操作会重新询问"
                      onClick={() => {
                        setConfirming(null)
                        void revokePermission(rule.id)
                      }}
                    >
                      确认撤销
                    </button>
                    <button
                      className="ai-button ai-button--tiny"
                      type="button"
                      data-testid="permission-revoke-cancel"
                      onClick={() => setConfirming(null)}
                    >
                      取消
                    </button>
                  </>
                ) : (
                  <button
                    className="ai-button ai-button--tiny"
                    type="button"
                    data-testid="permission-revoke"
                    disabled={busy}
                    title="撤销这条免问规则"
                    onClick={() => setConfirming(rule.id)}
                  >
                    撤销
                  </button>
                )}

                <button
                  className="ai-button ai-button--tiny ai-button--danger"
                  type="button"
                  data-testid="permission-revoke-tool"
                  disabled={busy}
                  title={`把「${rule.tool_pattern}」名下所有免问规则一并撤销`}
                  onClick={() => void revokePermissionTool(rule.tool_pattern)}
                >
                  撤销该工具全部
                </button>
              </li>
            )
          })
        )}
      </ul>

      {state.total > state.rules.length ? (
        <div className="ai-note">
          共 {state.total} 条，当前显示 {state.rules.length} 条（内核单次上限 200 条）。
        </div>
      ) : null}
    </div>
  )
}
