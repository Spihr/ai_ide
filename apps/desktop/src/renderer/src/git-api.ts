/**
 * Git 协议调用的薄封装 + unified diff 的行级解析。
 *
 * 五个 `git.*` 方法都走通用的 `window.aiide.request`：内核不给 Git 单独开
 * 事件通道（`workspace.changed` 已经覆盖「工作区内容变了」这个事实来源，
 * 见 `docs/03-内核接口协议规范.md` §2.8），所以 preload 也不需要新增订阅入口。
 *
 * 这里额外做两件内核不该做、UI 也不该散着做的事：
 *
 * 1. **diff 行级解析**：内核只回未着色的 unified diff 原文，行号与增删标记
 *    要由前端算。解析是纯函数，可直接单测。
 * 2. **错误码翻译**：`7001`–`7005` 是 Git 域独有的语义（找不到 git、不是仓库、
 *    git 非零退出、路径越界、暂存区为空），照 `ai-store.ts` 的惯例在这里补一句
 *    人话提示，面板只负责显示。
 */

import {
  Methods,
  type GitBranchesResult,
  type GitCommitResult,
  type GitDiffResult,
  type GitStageResult,
  type GitStatusResult
} from '@ai-ide/shared-types'

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
  7005: '暂存区没有可提交的内容：先暂存文件，或勾选「允许空提交」。'
}

/** 从主进程抛上来的错误文本里抠出 `[7002]` 这类协议码。 */
export function gitErrorCode(message: string): number | null {
  const matched = /\[(-?\d+)\]/.exec(message)
  if (!matched) return null
  const code = Number(matched[1])
  return Number.isFinite(code) ? code : null
}

/** 错误码 → 人话；未知码返回空串，由调用方回落到原始 message。 */
export function gitHintForError(code: number): string {
  return GIT_ERROR_HINTS[code] ?? ''
}

/** 把异常整理成「原文｜人话」；原文始终保留，否则用户贴不出可复现的报错。 */
export function describeGitError(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error)
  const code = gitErrorCode(message)
  const hint = code === null ? '' : gitHintForError(code)
  return hint.length > 0 && !message.includes(hint) ? `${message}｜${hint}` : message
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
    window.aiide.request(Methods.git.branches, withWorkspace(workspaceId)) as Promise<GitBranchesResult>
}
