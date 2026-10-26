/**
 * Git 面板（M6 第四切片 + 缺口第二批，外壳侧入口）：分支条 + 远端同步 + 变更清单 +
 * unified diff + 提交框 + stash 面板。
 *
 * 三层分工与 `CheckpointPanel` 一致：状态与动作在 `git-store`，这里只做「用户点了什么」
 * 的转发与展示。分组成「已暂存 / 未暂存」两组，同一个文件可能同时出现在两组里
 * （改了 → 暂存 → 又改），这不是 bug，是 git 的两列状态。
 *
 * diff 的行号与增删配色都由 `parseUnifiedDiff` 算，组件只按 `kind` 上样式类；
 * 未跟踪文件不发 `git.diff`（两侧都没有基准版本），直接给一句说明——否则一次空 diff
 * 会被读成「没改动」。
 *
 * 缺口第二批新增的四处界面，各有必须钉住的行为：
 *
 * - **分支切换**：失败（`7006`）时把 `files` 逐条列出来，不许只显示一句「切换失败」；
 * - **远端按钮**：`7007` / `7008` / `7009` 的提示来自 `git-api` 的人话表，`7009` 还多
 *   一个「以它作上游推送」的按钮（分支名取自错误载荷，不让用户手填）；
 * - **stash 面板**：`drop` 必须先弹确认（说明不可逆）；`pop` 撞 `7010` 时条目仍在，
 *   界面既不能删掉那一行，也不能只报错不解释；
 * - **强制推送**：与 `drop` 同一套确认弹窗，且文案由 store 提供（可被单测钉住）。
 */

import { useEffect, useMemo, useState } from 'react'

import type { GitStatusEntry, GitStashEntry } from '@ai-ide/shared-types'

import { parseUnifiedDiff } from '../git-api'
import {
  applyStash,
  cancelGitAction,
  checkoutBranch,
  clearGitSelection,
  commitStaged,
  confirmGitAction,
  createBranch,
  dismissGitError,
  dismissGitHint,
  entryStatusLabel,
  fetchRemote,
  groupEntries,
  popStash,
  pullRemote,
  pushRemote,
  pushWithUpstream,
  refreshGit,
  requestDropStash,
  requestForcePush,
  saveStash,
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

/**
 * 远端分支名 → 建议的本地分支名（`origin/feat/x` → `feat/x`）。
 *
 * 只去掉第一段：`origin` 是远端名，后面整段才是分支名，`feat/x` 里的斜杠要留着。
 */
function localNameOf(remoteBranch: string): string {
  const slash = remoteBranch.indexOf('/')
  return slash === -1 ? remoteBranch : remoteBranch.slice(slash + 1)
}

export function GitPanel(): React.JSX.Element {
  const state = useGit()
  const [showBranches, setShowBranches] = useState(false)
  const [showStashes, setShowStashes] = useState(true)
  const [newBranch, setNewBranch] = useState('')
  const [stashMessage, setStashMessage] = useState('')
  const [includeUntracked, setIncludeUntracked] = useState(false)
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
  const branchBusy = state.loading || busy

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

  const renderStash = (entry: GitStashEntry): React.JSX.Element => (
    <li key={`${entry.index}:${entry.oid}`} className="ai-list__item" data-testid="git-stash-row">
      <span className="ai-list__meta" title={entry.message}>
        stash@{`{${entry.index}}`}
        <span className="ai-note"> {entry.short_oid}</span>
        <br />
        <span className="ai-note">{entry.message}</span>
      </span>
      <span className="ai-row">
        <button
          className="ai-button ai-button--tiny"
          type="button"
          data-testid="git-stash-pop"
          disabled={busy}
          title="取出并从列表删除该条（git stash pop）"
          onClick={() => void popStash(entry.index)}
        >
          取出
        </button>
        <button
          className="ai-button ai-button--tiny"
          type="button"
          data-testid="git-stash-apply"
          disabled={busy}
          title="取出但保留该条（git stash apply）"
          onClick={() => void applyStash(entry.index)}
        >
          应用
        </button>
        <button
          className="ai-button ai-button--tiny"
          type="button"
          data-testid="git-stash-drop"
          disabled={busy}
          title="删除该条（不可逆，会先弹确认）"
          onClick={() => requestDropStash(entry.index)}
        >
          删除
        </button>
      </span>
    </li>
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
          disabled={branchBusy}
          onClick={() => void refreshGit()}
        >
          刷新
        </button>
      </div>

      <div className="ai-row" data-testid="git-remote-actions">
        <button
          className="ai-button ai-button--tiny"
          type="button"
          data-testid="git-fetch"
          disabled={branchBusy}
          title="从远端取回引用，不改动本地分支（git fetch）"
          onClick={() => void fetchRemote()}
        >
          获取
        </button>
        <button
          className="ai-button ai-button--tiny"
          type="button"
          data-testid="git-pull"
          disabled={branchBusy}
          title="只做快进拉取（--ff-only），分叉时会被拒绝而不是生成合并提交"
          onClick={() => void pullRemote()}
        >
          拉取
        </button>
        <button
          className="ai-button ai-button--tiny"
          type="button"
          data-testid="git-push"
          disabled={branchBusy}
          title="推送到上游分支（git push，从不强推）"
          onClick={() => void pushRemote()}
        >
          推送
        </button>
        <button
          className="ai-button ai-button--tiny"
          type="button"
          data-testid="git-force-push"
          disabled={branchBusy}
          title="强制推送（--force-with-lease），会先弹确认"
          onClick={() => requestForcePush()}
        >
          强制推送
        </button>
      </div>

      {showBranches ? (
        <div data-testid="git-branches">
          <div className="ai-row">
            <input
              className="ai-input"
              data-testid="git-new-branch-name"
              placeholder="新分支名（从当前 HEAD 建）"
              value={newBranch}
              onChange={(event) => setNewBranch(event.target.value)}
              onKeyDown={(event) => {
                if (event.key !== 'Enter') return
                event.preventDefault()
                const name = newBranch.trim()
                if (name.length === 0) return
                setNewBranch('')
                void createBranch(name)
              }}
            />
            <button
              className="ai-button ai-button--tiny"
              type="button"
              data-testid="git-create-branch"
              disabled={branchBusy || newBranch.trim().length === 0}
              onClick={() => {
                const name = newBranch.trim()
                if (name.length === 0) return
                setNewBranch('')
                void createBranch(name)
              }}
            >
              新建并切换
            </button>
          </div>

          <ul className="ai-list" data-testid="git-branch-list">
            {state.branches.length === 0 ? (
              <li className="ai-note">这个仓库还没有分支（或仍在加载）。</li>
            ) : (
              state.branches.slice(0, MAX_BRANCH_ROWS).map((branch) => {
                const current = branch.is_current
                const local = localNameOf(branch.name)
                return (
                  <li
                    key={branch.name}
                    className="ai-list__item"
                    title={branch.upstream ?? branch.oid}
                    data-testid="git-branch-row"
                  >
                    <span className="ai-list__meta">
                      {current ? '● ' : ''}
                      {branch.name}
                      {branch.is_remote ? '（远端）' : ''}
                      <span className="ai-note"> {branch.short_oid}</span>
                      {branch.upstream === undefined ? null : (
                        <span className="ai-note"> → {branch.upstream}</span>
                      )}
                    </span>
                    <button
                      className="ai-button ai-button--tiny"
                      type="button"
                      data-testid={branch.is_remote ? 'git-checkout-remote' : 'git-checkout'}
                      disabled={branchBusy || (current && !branch.is_remote)}
                      title={
                        branch.is_remote
                          ? `以 ${branch.name} 为起点建本地分支 ${local} 并切过去`
                          : current
                            ? '当前已在那个分支上'
                            : `切换到 ${branch.name}（工作区有改动时会被拒绝）`
                      }
                      onClick={() =>
                        void (branch.is_remote
                          ? createBranch(local, branch.name)
                          : checkoutBranch(branch.name))
                      }
                    >
                      {branch.is_remote ? '检出' : current ? '当前' : '切换'}
                    </button>
                  </li>
                )
              })
            )}
            {state.branches.length > MAX_BRANCH_ROWS ? (
              <li className="ai-note">
                共 {state.branches.length} 个，只显示前 {MAX_BRANCH_ROWS} 个。
              </li>
            ) : null}
          </ul>
        </div>
      ) : null}

      {state.error !== null ? (
        <div className="ai-result ai-result--bad" data-testid="git-error">
          <div>{state.error}</div>
          {/* git 自己的原始报错。7007 的 non-fast-forward / 7008 的 Authentication failed
              是用户搜索和贴给同事的唯一凭据，不能只留一句人话。 */}
          {state.errorDetail !== null ? (
            <pre className="ai-note" data-testid="git-error-detail">
              {state.errorDetail}
            </pre>
          ) : null}
          {state.errorFiles.length > 0 ? (
            <div data-testid="git-error-files">
              <div className="ai-note">
                {/* 7006 是「有改动不许切」，7010 是「撞冲突」，两种清单必须分开说，
                    否则冲突文件会被读成「只是没提交」。 */}
                {state.errorCode === 7010
                  ? `以下 ${state.errorFiles.length} 个文件在取出时冲突：`
                  : `以下 ${state.errorFiles.length} 个文件有未提交改动：`}
              </div>
              <ul className="ai-list">
                {state.errorFiles.map((file) => (
                  <li key={file} className="ai-note" data-testid="git-error-file">
                    {file}
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
          {state.errorBranch !== null ? (
            <button
              className="ai-button ai-button--tiny"
              type="button"
              data-testid="git-set-upstream"
              disabled={busy}
              title={`git push --set-upstream origin ${state.errorBranch}`}
              onClick={() => void pushWithUpstream(state.errorBranch ?? '')}
            >
              以 {state.errorBranch} 为上游推送
            </button>
          ) : null}
          {state.stashKept ? (
            <div className="ai-note ai-note__warn" data-testid="git-stash-kept">
              stash 条目没有被删除，仍在下面的列表里；解决冲突后可继续 pop / apply / drop。
            </div>
          ) : null}
          <button
            className="ai-button ai-button--tiny"
            type="button"
            data-testid="git-error-dismiss"
            onClick={dismissGitError}
          >
            忽略
          </button>
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
        Stash（{state.stashes.length}）
        <button
          className="ai-button ai-button--tiny"
          type="button"
          data-testid="git-stashes-toggle"
          onClick={() => setShowStashes((open) => !open)}
        >
          {showStashes ? '收起' : '展开'}
        </button>
      </h4>
      {showStashes ? (
        <div data-testid="git-stashes">
          <div className="ai-row">
            <input
              className="ai-input"
              data-testid="git-stash-message"
              placeholder="stash 说明（可留空，git 会写 WIP on …）"
              value={stashMessage}
              onChange={(event) => setStashMessage(event.target.value)}
            />
            <button
              className="ai-button ai-button--tiny"
              type="button"
              data-testid="git-stash-save"
              disabled={busy}
              title="把当前改动存进 stash（git stash push）"
              onClick={() => {
                const message = stashMessage.trim()
                setStashMessage('')
                void saveStash({
                  message: message.length === 0 ? undefined : message,
                  includeUntracked
                })
              }}
            >
              保存到 stash
            </button>
          </div>
          <label className="git-check">
            <input
              type="checkbox"
              data-testid="git-stash-untracked"
              checked={includeUntracked}
              onChange={(event) => setIncludeUntracked(event.target.checked)}
            />
            同时保存未跟踪文件
          </label>

          {state.stashes.length === 0 ? (
            <div className="ai-note" data-testid="git-stash-empty">
              {state.stashLoading ? '正在读取 stash…' : '没有 stash 条目。'}
            </div>
          ) : (
            <ul className="ai-list" data-testid="git-stash-list">
              {state.stashes.map(renderStash)}
            </ul>
          )}
        </div>
      ) : null}

      {state.confirm !== null ? (
        <div className="ai-result ai-result--bad" role="dialog" data-testid="git-confirm">
          <div data-testid="git-confirm-title">{state.confirm.title}</div>
          <div className="ai-note" data-testid="git-confirm-detail">
            {state.confirm.detail}
          </div>
          <div className="ai-note ai-note__warn" data-testid="git-confirm-warning">
            {state.confirm.warning}
          </div>
          <div className="ai-row">
            <button
              className="ai-button ai-button--primary"
              type="button"
              data-testid="git-confirm-ok"
              disabled={busy}
              onClick={() => void confirmGitAction()}
            >
              确认（不可撤销）
            </button>
            <button
              className="ai-button ai-button--tiny"
              type="button"
              data-testid="git-confirm-cancel"
              onClick={cancelGitAction}
            >
              取消
            </button>
          </div>
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
