/**
 * Git 面板（M6 第四切片，外壳侧入口）：分支条 + 变更清单 + unified diff + 提交框。
 *
 * 三层分工与 `CheckpointPanel` 一致：状态与动作在 `git-store`，这里只做「用户点了什么」
 * 的转发与展示。分组成「已暂存 / 未暂存」两组，同一个文件可能同时出现在两组里
 * （改了 → 暂存 → 又改），这不是 bug，是 git 的两列状态。
 *
 * diff 的行号与增删配色都由 `parseUnifiedDiff` 算，组件只按 `kind` 上样式类；
 * 未跟踪文件不发 `git.diff`（两侧都没有基准版本），直接给一句说明——否则一次空 diff
 * 会被读成「没改动」。
 */

import { useEffect, useMemo, useState } from 'react'

import type { GitStatusEntry } from '@ai-ide/shared-types'

import { parseUnifiedDiff } from '../git-api'
import {
  clearGitSelection,
  commitStaged,
  dismissGitHint,
  entryStatusLabel,
  groupEntries,
  refreshGit,
  selectGitEntry,
  setCommitMessage,
  stageAll,
  stageEntry,
  stagedCount,
  unstageAll,
  unstageEntry,
  useGit
} from '../git-store'

/** 分支列表一次最多渲染多少行：大仓库的远端分支可能有几百条。 */
const MAX_BRANCH_ROWS = 50

function flagClass(letter: string): string {
  if (letter === 'A' || letter === '?') return 'git-entry__flag git-entry__flag--new'
  if (letter === 'D') return 'git-entry__flag git-entry__flag--gone'
  if (letter === 'U') return 'git-entry__flag git-entry__flag--conflict'
  if (letter === 'R' || letter === 'C') return 'git-entry__flag git-entry__flag--moved'
  return 'git-entry__flag'
}

export function GitPanel(): React.JSX.Element {
  const state = useGit()
  const [showBranches, setShowBranches] = useState(false)
  const [amend, setAmend] = useState(false)
  const [allowEmpty, setAllowEmpty] = useState(false)

  // 每次切到本视图都重拉一次：文件可能在别的视图里被改过、被 Agent 改过。
  useEffect(() => {
    void refreshGit()
  }, [])

  const groups = useMemo(
    () => groupEntries(state.status?.entries ?? []),
    [state.status]
  )
  const lines = useMemo(
    () => (state.diff === null ? [] : parseUnifiedDiff(state.diff.diff)),
    [state.diff]
  )
  const stagedTotal = stagedCount(state.status)
  const busy = state.busy

  const renderEntries = (entries: GitStatusEntry[], staged: boolean): React.JSX.Element => (
    <ul className="ai-list" data-testid={staged ? 'git-staged-list' : 'git-changes-list'}>
      {entries.length === 0 ? (
        <li className="ai-note">{staged ? '暂存区是空的。' : '没有未暂存的改动。'}</li>
      ) : (
        entries.map((entry) => {
          const letter = staged ? entry.index : entry.worktree
          const active =
            state.selected !== null &&
            state.selected.path === entry.path &&
            state.selected.staged === staged
          return (
            <li key={`${staged ? 'i' : 'w'}:${entry.path}`} className="git-entry-item">
              <button
                type="button"
                className={`git-entry${active ? ' is-active' : ''}`}
                data-testid="git-entry"
                title={`${entry.path}｜${entryStatusLabel(letter)}（${letter}）`}
                onClick={() => void selectGitEntry(entry, staged)}
              >
                <span className={flagClass(letter)} aria-hidden="true">
                  {letter}
                </span>
                <span className="git-entry__path">
                  {entry.path}
                  {entry.original_path !== undefined ? (
                    <span className="git-entry__orig"> ← {entry.original_path}</span>
                  ) : null}
                </span>
              </button>
              <button
                className="ai-button ai-button--tiny"
                type="button"
                data-testid={staged ? 'git-unstage' : 'git-stage'}
                disabled={busy}
                title={staged ? '把这个文件移出暂存区（git reset）' : '把这个文件加入暂存区（git add）'}
                onClick={() =>
                  void (staged ? unstageEntry(entry.path) : stageEntry(entry.path))
                }
              >
                {staged ? '取消' : '暂存'}
              </button>
            </li>
          )
        })
      )}
    </ul>
  )

  return (
    <div className="ai-section" data-testid="git-panel">
      <div className="git-branch">
        <span
          className="git-branch__name"
          title={state.status?.upstream ?? '未设置上游分支'}
          data-testid="git-branch"
        >
          {state.status === null
            ? '—'
            : state.status.detached
              ? '分离头指针'
              : (state.status.branch ?? '（无分支）')}
        </span>
        {state.status !== null && (state.status.ahead > 0 || state.status.behind > 0) ? (
          <span className="ai-note" data-testid="git-ahead-behind">
            {state.status.ahead > 0 ? `↑${state.status.ahead}` : ''}
            {state.status.behind > 0 ? ` ↓${state.status.behind}` : ''}
          </span>
        ) : null}
        <button
          className="ai-button ai-button--tiny"
          type="button"
          data-testid="git-branches-toggle"
          title="展开/收起分支列表"
          onClick={() => setShowBranches((open) => !open)}
        >
          分支 {state.branches.length}
        </button>
        <button
          className="ai-button ai-button--tiny"
          type="button"
          data-testid="git-refresh"
          disabled={state.loading || busy}
          onClick={() => void refreshGit()}
        >
          刷新
        </button>
      </div>

      {showBranches ? (
        <ul className="ai-list" data-testid="git-branches">
          {state.branches.length === 0 ? (
            <li className="ai-note">这个仓库还没有分支（或仍在加载）。</li>
          ) : (
            state.branches.slice(0, MAX_BRANCH_ROWS).map((branch) => (
              <li key={branch.name} className="ai-list__item" title={branch.upstream ?? branch.oid}>
                <span className="ai-list__meta">
                  {branch.is_current ? '● ' : ''}
                  {branch.name}
                  {branch.is_remote ? '（远端）' : ''}
                  <span className="ai-note"> {branch.short_oid}</span>
                </span>
              </li>
            ))
          )}
          {state.branches.length > MAX_BRANCH_ROWS ? (
            <li className="ai-note">
              共 {state.branches.length} 个，只显示前 {MAX_BRANCH_ROWS} 个。
            </li>
          ) : null}
        </ul>
      ) : null}

      {state.error !== null ? (
        <div className="ai-result ai-result--bad" data-testid="git-error">
          {state.error}
        </div>
      ) : null}
      {state.hint !== null ? (
        <div className="ai-result ai-result--ok" data-testid="git-hint">
          {state.hint}
          <button
            className="ai-button ai-button--tiny"
            type="button"
            data-testid="git-hint-dismiss"
            onClick={dismissGitHint}
          >
            知道了
          </button>
        </div>
      ) : null}

      <h4 className="ai-section__subtitle">
        已暂存（{groups.staged.length}）
        {groups.staged.length > 0 ? (
          <button
            className="ai-button ai-button--tiny"
            type="button"
            data-testid="git-unstage-all"
            disabled={busy}
            title="把暂存区整个清空（git reset）"
            onClick={() => void unstageAll()}
          >
            全部取消
          </button>
        ) : null}
      </h4>
      {renderEntries(groups.staged, true)}

      <h4 className="ai-section__subtitle">
        更改（{groups.unstaged.length}）
        {groups.unstaged.length > 0 ? (
          <button
            className="ai-button ai-button--tiny"
            type="button"
            data-testid="git-stage-all"
            disabled={busy}
            title="暂存全部变更，含删除（git add -A）"
            onClick={() => void stageAll()}
          >
            全部暂存
          </button>
        ) : null}
      </h4>
      {renderEntries(groups.unstaged, false)}

      {state.status !== null && state.status.entries.length === 0 && state.selected === null ? (
        <div className="ai-note" data-testid="git-clean">
          工作区干净，没有待提交的改动。
        </div>
      ) : null}

      {state.selected !== null ? (
        <div className="git-diff" data-testid="git-diff">
          <div className="git-diff__head">
            <span className="git-diff__path" title={state.selected.path}>
              {state.selected.path}
            </span>
            <span className="ai-note">
              {state.selected.staged ? '暂存区相对 HEAD' : '工作区相对暂存区'}
            </span>
            <button
              className="ai-button ai-button--tiny"
              type="button"
              data-testid="git-diff-close"
              onClick={clearGitSelection}
            >
              关闭
            </button>
          </div>

          {state.diffLoading ? (
            <div className="ai-note">正在加载 diff…</div>
          ) : state.selected.kind === 'untracked' ? (
            <div className="ai-note" data-testid="git-diff-untracked">
              未跟踪文件没有基准版本，暂存后才能看到 diff。
            </div>
          ) : state.diff === null ? null : lines.length === 0 ? (
            <div className="ai-note" data-testid="git-diff-empty">
              没有可显示的差异（可能是二进制文件或仅权限位变化）。
            </div>
          ) : (
            <div className="git-diff__body" data-testid="git-diff-body">
              {lines.map((line, index) => (
                <div
                  key={`${index}:${line.kind}`}
                  className={`git-diff__line git-diff__line--${line.kind}`}
                >
                  <span className="git-diff__no">{line.oldLine ?? ''}</span>
                  <span className="git-diff__no">{line.newLine ?? ''}</span>
                  <span className="git-diff__text">{line.text}</span>
                </div>
              ))}
            </div>
          )}

          {state.diff?.truncated === true ? (
            <div className="ai-note ai-note__warn" data-testid="git-diff-truncated">
              diff 超过 1 MiB，已截断尾部（头部完整）。
            </div>
          ) : null}
        </div>
      ) : null}

      <h4 className="ai-section__subtitle">提交（暂存区 {stagedTotal} 个文件）</h4>
      <textarea
        className="ai-input ai-textarea"
        data-testid="git-commit-message"
        rows={3}
        placeholder="提交信息，首行作为摘要（Ctrl+Enter 提交）"
        value={state.message}
        onChange={(event) => setCommitMessage(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) {
            event.preventDefault()
            void commitStaged({ amend, allowEmpty })
          }
        }}
      />
      <div className="ai-row">
        <label className="git-check">
          <input
            type="checkbox"
            data-testid="git-commit-amend"
            checked={amend}
            onChange={(event) => setAmend(event.target.checked)}
          />
          追加到上次提交
        </label>
        <label className="git-check">
          <input
            type="checkbox"
            data-testid="git-commit-allow-empty"
            checked={allowEmpty}
            onChange={(event) => setAllowEmpty(event.target.checked)}
          />
          允许空提交
        </label>
      </div>
      <div className="ai-row">
        <button
          className="ai-button ai-button--primary"
          type="button"
          data-testid="git-commit"
          disabled={busy || state.message.trim().length === 0}
          onClick={() => void commitStaged({ amend, allowEmpty })}
        >
          {busy ? '处理中…' : '提交'}
        </button>
        {amend ? <span className="ai-note">amend 会改写 HEAD 的提交信息</span> : null}
      </div>

      {state.lastCommit !== null ? (
        <div className="ai-note" data-testid="git-last-commit" title={state.lastCommit.oid}>
          上次提交 {state.lastCommit.short_oid} · {state.lastCommit.files} 个文件 ·{' '}
          {state.lastCommit.author}
          <br />
          {state.lastCommit.summary}
        </div>
      ) : null}
    </div>
  )
}
