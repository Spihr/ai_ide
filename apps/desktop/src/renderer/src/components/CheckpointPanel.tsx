/**
 * 检查点面板（M5 外壳侧入口）：快照列表 + 回滚入口 + 回滚报告。
 *
 * 三层分工与 `SessionPanel` 一致：状态与动作在 `checkpoint-store`，这里只做
 * 「用户点了什么」的转发与确认交互。
 *
 * 回滚是**破坏性动作**（覆盖工作区文件），所以不弹浏览器 `confirm`——那会把整个渲染
 * 线程卡住，也测不了。这里用「点一下变成两个按钮」的行内二次确认：第一下只是选中，
 * 第二下才真发请求。误触的成本因此是一次额外点击，而不是丢掉一整个工作区的改动。
 */

import { useState } from 'react'

import {
  createCheckpoint,
  deleteCheckpoint,
  dismissCheckpointHint,
  refreshCheckpoints,
  restoreCheckpoint,
  useCheckpoints
} from '../checkpoint-store'
import {
  formatBytes,
  formatCheckpointTime,
  originText,
  restoreStatusText
} from '../checkpoint-api'

export function CheckpointPanel(): React.JSX.Element {
  const state = useCheckpoints()
  const [confirming, setConfirming] = useState<string | null>(null)

  const busy = state.busy
  const report = state.lastRestore

  return (
    <div className="ai-section" data-testid="checkpoint-panel">
      <h3 className="ai-section__title">检查点</h3>

      <div className="ai-row">
        <button
          className="ai-button"
          type="button"
          data-testid="checkpoint-create"
          disabled={busy}
          title="按当前磁盘内容拍一次快照（大工作区可能要几百毫秒）"
          onClick={() => void createCheckpoint()}
        >
          拍快照
        </button>
        <button
          className="ai-button"
          type="button"
          data-testid="checkpoint-refresh"
          disabled={state.loading}
          onClick={() => void refreshCheckpoints()}
        >
          刷新
        </button>
      </div>

      {state.error !== null ? (
        <div className="ai-result ai-result--bad" data-testid="checkpoint-error">
          {state.error}
        </div>
      ) : null}
      {state.hint !== null ? (
        <div className="ai-result ai-result--ok" data-testid="checkpoint-hint">
          {state.hint}
          <button
            className="ai-button ai-button--tiny"
            type="button"
            data-testid="checkpoint-hint-dismiss"
            onClick={dismissCheckpointHint}
          >
            知道了
          </button>
        </div>
      ) : null}

      <ul className="ai-list" data-testid="checkpoint-list">
        {state.checkpoints.length === 0 ? (
          <li className="ai-note">
            {state.loading
              ? '正在加载检查点…'
              : '这个工作区还没有检查点：点「拍快照」手动建一个；Agent 改动文件前后也会自动建。'}
          </li>
        ) : (
          state.checkpoints.map((checkpoint) => {
            const armed = confirming === checkpoint.checkpoint_id
            return (
              <li
                key={checkpoint.checkpoint_id}
                className="ai-list__item"
                data-testid="checkpoint-item"
                title={checkpoint.workspace_root}
              >
                <span className="ai-list__meta">
                  {checkpoint.label}
                  {' · '}
                  {originText(checkpoint)}
                  {' · '}
                  {formatCheckpointTime(checkpoint.created_at_ms)}
                  <br />
                  <span className="ai-note">
                    {checkpoint.files.length} 个文件 · {formatBytes(checkpoint.total_bytes)}
                    {checkpoint.skipped > 0 ? ` · 跳过 ${checkpoint.skipped}` : ''}
                  </span>
                </span>

                {armed ? (
                  <>
                    <button
                      className="ai-button ai-button--tiny ai-button--danger"
                      type="button"
                      data-testid="checkpoint-restore-confirm"
                      disabled={busy}
                      title="确认：按这个快照覆盖工作区里对应的文件"
                      onClick={() => {
                        setConfirming(null)
                        void restoreCheckpoint(checkpoint.checkpoint_id)
                      }}
                    >
                      确认回滚
                    </button>
                    <button
                      className="ai-button ai-button--tiny"
                      type="button"
                      data-testid="checkpoint-restore-cancel"
                      onClick={() => setConfirming(null)}
                    >
                      取消
                    </button>
                  </>
                ) : (
                  <button
                    className="ai-button ai-button--tiny"
                    type="button"
                    data-testid="checkpoint-restore"
                    disabled={busy}
                    title="把这个快照里的文件写回工作区（会覆盖同名文件）"
                    onClick={() => setConfirming(checkpoint.checkpoint_id)}
                  >
                    回滚
                  </button>
                )}

                <button
                  className="ai-button ai-button--tiny ai-button--danger"
                  type="button"
                  data-testid="checkpoint-delete"
                  disabled={busy}
                  title="删掉这个检查点（快照本体一并删除，之后无法回滚到它）"
                  onClick={() => void deleteCheckpoint(checkpoint.checkpoint_id)}
                >
                  删除
                </button>
              </li>
            )
          })
        )}
      </ul>

      {state.total > state.checkpoints.length ? (
        <div className="ai-note">
          共 {state.total} 个，当前显示最近 {state.checkpoints.length} 个。
        </div>
      ) : null}

      {report !== null ? (
        <>
          <h4 className="ai-section__subtitle">
            上次回滚{report.verified ? '（校验通过）' : '（校验未通过）'}
          </h4>
          <ul className="ai-list" data-testid="checkpoint-report">
            {report.files.length === 0 ? (
              <li className="ai-note">这次快照里没有文件。</li>
            ) : (
              report.files.map((file) => (
                <li
                  key={file.path}
                  className="ai-list__item"
                  data-testid="checkpoint-report-item"
                  title={file.detail ?? file.path}
                >
                  <span className="ai-list__meta">{file.path}</span>
                  <span
                    className={
                      file.status === 'failed' ? 'ai-result ai-result--bad' : 'ai-note'
                    }
                  >
                    {restoreStatusText(file.status)}
                  </span>
                </li>
              ))
            )}
          </ul>
        </>
      ) : null}
    </div>
  )
}
