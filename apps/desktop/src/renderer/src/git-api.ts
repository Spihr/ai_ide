/**
 * Git 协议调用的薄封装 + unified diff 的行级解析。
 *
 * 十四个 `git.*` 方法都走通用的 `window.aiide.request`：内核不给 Git 单独开
 * 事件通道（`workspace.changed` 已经覆盖「工作区内容变了」这个事实来源，
 * 见 `docs/03-内核接口协议规范.md` §2.8），所以 preload 也不需要新增订阅入口。
 *
 * 这里额外做三件内核不该做、UI 也不该散着做的事：
 *
 * 1. **diff 行级解析**：内核只回未着色的 unified diff 原文，行号与增删标记
 *    要由前端算。解析是纯函数，可直接单测。
 * 2. **错误码翻译**：`7001`–`7010` 是 Git 域独有的语义（找不到 git、不是仓库、
 *    git 非零退出、路径越界、暂存区为空、脏工作区、非快进、认证失败、无上游、
 *    stash 冲突），照 `ai-store.ts` 的惯例在这里补一句人话提示，面板只负责显示。
 * 3. **错误载荷还原**：`7006` / `7009` / `7010` 的 `data` 是 UI 的必要输入
 *    （冲突文件清单、要设上游的分支名、条目是否还在），主进程把它编进 message
 *    尾部（`shared/ipc.ts` 的信封），这里剥回来并用 `stripRpcErrorPayload` 保证
 *    用户看到的文案里没有那段 JSON。
 */

import {
  Methods,
  type GitBranchesResult,
  type GitCheckoutResult,
  type GitCommitResult,
  type GitDiffResult,
  type GitFetchResult,
  type GitPullResult,
  type GitPushResult,
  type GitStageResult,
  type GitStashApplyResult,
  type GitStashDropResult,
  type GitStashListResult,
  type GitStashResult,
  type GitStatusResult
} from '@ai-ide/shared-types'

import { decodeRpcErrorPayload, stripRpcErrorPayload } from '../../shared/ipc'

/** diff 的一行；`kind` 决定配色，行号用于左侧栏（`null` 表示不该显示侧栏）。 */
export interface GitDiffLine {
  kind: 'meta' | 'hunk' | 'add' | 'del' | 'context'
  /** 已去掉增删前缀的正文（`meta` / `hunk` 保留原样）。 */
  text: string
  /** 旧文件行号；新增行为 `null`。 */
  oldLine: number | null
  /** 新文件行号；删除行为 `null`。 */
  newLine: number | null
}

/** hunk 头：`@@ -12,7 +12,9 @@ 上下文`。分组 2 / 4 是可选的行数。 */
const HUNK_HEADER = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/

/**
 * 把 unified diff 原文拆成带行号的行。
 *
 * 边界处理（都是 git 真实会吐的形态）：
 *
 * - `\ No newline at end of file` 归 `meta`，不吃行号；
 * - 首个 hunk 之前的 `diff --git` / `index` / `---` / `+++` 也归 `meta`；
 * - 尾随换行不产生空行（否则每个 diff 末尾都会多一条空 context）。
 */
export function parseUnifiedDiff(text: string): GitDiffLine[] {
  if (text.length === 0) return []

  const raw = text.replace(/\r\n/g, '\n').split('\n')
  if (raw.length > 0 && raw[raw.length - 1] === '') raw.pop()

  const lines: GitDiffLine[] = []
  let oldLine = 0
  let newLine = 0
  let inHunk = false

  for (const line of raw) {
    const header = HUNK_HEADER.exec(line)
    if (header) {
      oldLine = Number(header[1])
      newLine = Number(header[2])
      inHunk = true
      lines.push({ kind: 'hunk', text: line, oldLine: null, newLine: null })
      continue
    }

    if (!inHunk || line.startsWith('\\')) {
      lines.push({ kind: 'meta', text: line, oldLine: null, newLine: null })
      continue
    }

    if (line.startsWith('+')) {
      lines.push({ kind: 'add', text: line.slice(1), oldLine: null, newLine })
      newLine += 1
      continue
    }
    if (line.startsWith('-')) {
      lines.push({ kind: 'del', text: line.slice(1), oldLine, newLine: null })
      oldLine += 1
      continue
    }
    if (line.startsWith(' ')) {
      lines.push({ kind: 'context', text: line.slice(1), oldLine, newLine })
      oldLine += 1
      newLine += 1
      continue
    }

    // hunk 内冒出无法归类的行（理论上不会有）：当元信息原样展示，别丢内容。
    lines.push({ kind: 'meta', text: line, oldLine: null, newLine: null })
  }

  return lines
}

const GIT_ERROR_HINTS: Record<number, string> = {
  7001: '找不到可执行的 git：确认 git 已安装并在 PATH 里。',
  7002: '当前工作区不是 Git 仓库：请先 `git init` 或换一个仓库内的工作区。',
  7003: 'git 命令非零退出：下方/stderr 里有原始输出，常见是冲突未解或钩子失败。',
  7004: '参数非法：路径越界、含 `..`，或提交信息为空。',
  7005: '暂存区没有可提交的内容：先暂存文件，或勾选「允许空提交」。',
  // 7006~7010：缺口第二批补的专用码。每条的用户下一步动作都不同，所以文案必须
  // 指名「接下来做什么」，而不是笼统地说「操作失败」。
  7006: '工作区有未提交改动，内核拒绝切换分支：先提交这些文件，或先 stash 起来再切。',
  7007: '非快进：远端已有你本地没有的提交，先 pull（快进）再 push；强推请用「强制推送」并二次确认。',
  7008: '远端认证失败：检查凭据（HTTPS token / SSH key）或远端地址，重试不会解决。',
  7009: '当前分支没有上游分支：先设置 upstream（如 `git push -u origin <branch>`）再操作。',
  7010: 'stash 取出时撞上冲突：条目**仍在**列表里（没有丢），解完冲突后再 pop / drop。'
}

/** 错误码 → 人话；未知码返回空串，由调用方回落到原始 message。 */
export function gitHintForError(code: number): string {
  return GIT_ERROR_HINTS[code] ?? ''
}

/** 从主进程抛上来的错误文本里抠出 `[7002]` 这类协议码。 */
export function gitErrorCode(message: string): number | null {
  const matched = /\[(-?\d+)\]/.exec(message)
  if (!matched) return null
  const code = Number(matched[1])
  return Number.isFinite(code) ? code : null
}

/** 一次 Git 失败的结构化解释：原文 + 人话 + 内核 `data` 里的关键字段。 */
export interface GitFailure {
  /** 协议码；文本里没有码时为 `null`。 */
  code: number | null
  /** 剥掉信封后的内核原文（用户贴报错的凭据）。 */
  message: string
  /** 该码对应的一句人话；未知码为空串。 */
  hint: string
  /** `7006` / `7010` 的路径清单；其它码为空表。 */
  files: string[]
  /** `7009` 缺少上游的分支名；其它码为 `null`。 */
  branch: string | null
  /** `7007` / `7008` 的细节（git 原文）；其它码为 `null`。 */
  detail: string | null
  /** `7010` 恒真：冲突时 stash 条目没有被删。 */
  stashKept: boolean
}

/** 从 `data` 里按需取字段：内核给什么就取什么，缺的类型不对就当没有。 */
function stringsOf(value: unknown): string[] {
  if (!Array.isArray(value)) return []
  return value.filter((item): item is string => typeof item === 'string')
}

function stringOf(value: unknown): string | null {
  return typeof value === 'string' && value.length > 0 ? value : null
}

/**
 * 把异常整理成结构化失败信息。
 *
 * 三种输入形态都要吃得下：`Error`（主进程抛上来的）、字符串（测试/兜底）、
 * 以及带信封的 message（主进程把 `data` 编在了尾部）。
 */
export function describeGitFailure(error: unknown): GitFailure {
  const raw = error instanceof Error ? error.message : String(error)
  const payload = decodeRpcErrorPayload(raw)
  // `decodeRpcErrorPayload` 已经把信封切掉了，直接用它的 message 就够。
  const message = payload === null ? raw : payload.message.trim()
  const code = payload?.code ?? gitErrorCode(message)
  const data =
    payload?.data !== null && typeof payload?.data === 'object'
      ? (payload.data as Record<string, unknown>)
      : {}

  return {
    code,
    message: message.length > 0 ? message : raw,
    hint: code === null ? '' : gitHintForError(code),
    files: stringsOf(data['files']),
    branch: stringOf(data['branch']),
    detail: stringOf(data['detail']),
    stashKept: data['stash_kept'] === true
  }
}

/** 把异常整理成「原文｜人话」；原文始终保留，否则用户贴不出可复现的报错。 */
export function describeGitError(error: unknown): string {
  const failure = describeGitFailure(error)
  const message = stripRpcErrorPayload(failure.message)
  return failure.hint.length > 0 && !message.includes(failure.hint)
    ? `${message}｜${failure.hint}`
    : message
}

/** 参数里 `workspace_id` 可缺省（缺省即活动工作区），这里统一显式传，避免歧义。 */
function withWorkspace(workspaceId: string | null): { workspace_id?: string } {
  return workspaceId === null ? {} : { workspace_id: workspaceId }
}

export const gitApi = {
  status: (workspaceId: string | null): Promise<GitStatusResult> =>
    window.aiide.request(Methods.git.status, withWorkspace(workspaceId)) as Promise<GitStatusResult>,

  diff: (input: {
    workspaceId: string | null
    path: string
    staged: boolean
    contextLines?: number
  }): Promise<GitDiffResult> =>
    window.aiide.request(Methods.git.diff, {
      ...withWorkspace(input.workspaceId),
      path: input.path,
      staged: input.staged,
      ...(input.contextLines === undefined ? {} : { context_lines: input.contextLines })
    }) as Promise<GitDiffResult>,

  stage: (input: {
    workspaceId: string | null
    paths?: string[]
    all?: boolean
    unstage?: boolean
  }): Promise<GitStageResult> =>
    window.aiide.request(Methods.git.stage, {
      ...withWorkspace(input.workspaceId),
      paths: input.paths ?? [],
      all: input.all ?? false,
      unstage: input.unstage ?? false
    }) as Promise<GitStageResult>,

  commit: (input: {
    workspaceId: string | null
    message: string
    paths?: string[]
    amend?: boolean
    allowEmpty?: boolean
  }): Promise<GitCommitResult> =>
    window.aiide.request(Methods.git.commit, {
      ...withWorkspace(input.workspaceId),
      message: input.message,
      ...(input.paths === undefined ? {} : { paths: input.paths }),
      amend: input.amend ?? false,
      allow_empty: input.allowEmpty ?? false
    }) as Promise<GitCommitResult>,

  branches: (workspaceId: string | null): Promise<GitBranchesResult> =>
    window.aiide.request(Methods.git.branches, withWorkspace(workspaceId)) as Promise<GitBranchesResult>,

  // -------------------------------------------------------------------------
  // 缺口第二批：分支切换 + 远端同步（M6 切片四）
  // -------------------------------------------------------------------------

  /**
   * 切换分支；`create` 为真时走 `git checkout -b <branch> <start_point>`。
   *
   * 脏工作区会被内核以 `7006` 拒绝（不是偷偷把改动搬过去），`data.files` 由
   * `describeGitFailure` 还原成清单交给面板渲染。
   */
  checkout: (input: {
    workspaceId: string | null
    branch: string
    create?: boolean
    startPoint?: string
  }): Promise<GitCheckoutResult> =>
    window.aiide.request(Methods.git.checkout, {
      ...withWorkspace(input.workspaceId),
      branch: input.branch,
      create: input.create ?? false,
      ...(input.startPoint === undefined ? {} : { start_point: input.startPoint })
    }) as Promise<GitCheckoutResult>,

  fetch: (input: {
    workspaceId: string | null
    remote?: string
    prune?: boolean
  }): Promise<GitFetchResult> =>
    window.aiide.request(Methods.git.fetch, {
      ...withWorkspace(input.workspaceId),
      ...(input.remote === undefined ? {} : { remote: input.remote }),
      prune: input.prune ?? false
    }) as Promise<GitFetchResult>,

  /** 只做快进；非快进由内核报 `7007`，外壳提示「先拉取再推」。 */
  pull: (input: {
    workspaceId: string | null
    remote?: string
    branch?: string
  }): Promise<GitPullResult> =>
    window.aiide.request(Methods.git.pull, {
      ...withWorkspace(input.workspaceId),
      ...(input.remote === undefined ? {} : { remote: input.remote }),
      ...(input.branch === undefined ? {} : { branch: input.branch })
    }) as Promise<GitPullResult>,

  /**
   * 推送。**没有** `force` 参数：强推只能是 `force_with_lease`，且必须由调用方
   * 先走二次确认（`git-store.requestForcePush`），这里只负责把标志传下去。
   */
  push: (input: {
    workspaceId: string | null
    remote?: string
    branch?: string
    setUpstream?: boolean
    forceWithLease?: boolean
  }): Promise<GitPushResult> =>
    window.aiide.request(Methods.git.push, {
      ...withWorkspace(input.workspaceId),
      ...(input.remote === undefined ? {} : { remote: input.remote }),
      ...(input.branch === undefined ? {} : { branch: input.branch }),
      set_upstream: input.setUpstream ?? false,
      force_with_lease: input.forceWithLease ?? false
    }) as Promise<GitPushResult>,

  // -------------------------------------------------------------------------
  // 缺口第二批：stash 五项
  // -------------------------------------------------------------------------

  stash: (input: {
    workspaceId: string | null
    message?: string
    includeUntracked?: boolean
  }): Promise<GitStashResult> =>
    window.aiide.request(Methods.git.stash, {
      ...withWorkspace(input.workspaceId),
      ...(input.message === undefined || input.message.length === 0
        ? {}
        : { message: input.message }),
      include_untracked: input.includeUntracked ?? false
    }) as Promise<GitStashResult>,

  stashList: (workspaceId: string | null): Promise<GitStashListResult> =>
    window.aiide.request(
      Methods.git.stashList,
      withWorkspace(workspaceId)
    ) as Promise<GitStashListResult>,

  /** 取出并从列表删掉该条；冲突时内核报 `7010`，条目**不会**被删。 */
  stashPop: (input: { workspaceId: string | null; index?: number }): Promise<GitStashApplyResult> =>
    window.aiide.request(Methods.git.stashPop, {
      ...withWorkspace(input.workspaceId),
      ...(input.index === undefined ? {} : { index: input.index })
    }) as Promise<GitStashApplyResult>,

  /** 取出但保留该条；成功路径不删条目，冲突同样报 `7010`。 */
  stashApply: (input: { workspaceId: string | null; index?: number }): Promise<GitStashApplyResult> =>
    window.aiide.request(Methods.git.stashApply, {
      ...withWorkspace(input.workspaceId),
      ...(input.index === undefined ? {} : { index: input.index })
    }) as Promise<GitStashApplyResult>,

  /**
   * 删除一条 stash（不可逆）。内核会回带被删条目的 `oid` / `message` 作为事后证据；
   * 外壳必须先二次确认（`git-store.requestDropStash`）。
   */
  stashDrop: (input: { workspaceId: string | null; index?: number }): Promise<GitStashDropResult> =>
    window.aiide.request(Methods.git.stashDrop, {
      ...withWorkspace(input.workspaceId),
      ...(input.index === undefined ? {} : { index: input.index })
    }) as Promise<GitStashDropResult>
}
