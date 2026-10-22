/**
 * Git 协议封装的纯函数测试：unified diff 解析 + 错误码翻译 + 参数形状。
 *
 * 解析这块单独测的理由：它是唯一一处「内核给的是一段文本、UI 要从中算出结构」的地方，
 * 算错了不会报错，只会**安静地显示错的增删**——用户照着看代码、然后提交错误的东西。
 * 所以这里把 git 真实会吐的几种形态（多 hunk、无末尾换行标记、CRLF、空 diff）都钉住。
 */

import { describe, expect, it } from 'vitest'

import { describeGitError, gitErrorCode, gitHintForError, parseUnifiedDiff } from './git-api'

const SAMPLE = [
  'diff --git a/src/a.ts b/src/a.ts',
  'index 1111111..2222222 100644',
  '--- a/src/a.ts',
  '+++ b/src/a.ts',
  '@@ -1,3 +1,4 @@ export function a() {',
  ' const x = 1',
  '-const y = 2',
  '+const y = 3',
  '+const z = 4',
  ' return x',
  '@@ -10,2 +11,2 @@',
  '-old',
  '+new',
  ' tail',
  ''
].join('\n')

describe('parseUnifiedDiff', () => {
  it('把 diff 头部归为 meta，且不吃行号', () => {
    const lines = parseUnifiedDiff(SAMPLE)
    const head = lines.slice(0, 4)

    expect(head.map((line) => line.kind)).toEqual(['meta', 'meta', 'meta', 'meta'])
    for (const line of head) {
      expect(line.oldLine).toBeNull()
      expect(line.newLine).toBeNull()
    }
  })

  it('hunk 头透传原文，并重置两侧行号', () => {
    const lines = parseUnifiedDiff(SAMPLE)
    const hunks = lines.filter((line) => line.kind === 'hunk')

    expect(hunks.map((line) => line.text)).toEqual([
      '@@ -1,3 +1,4 @@ export function a() {',
      '@@ -10,2 +11,2 @@'
    ])
    expect(hunks[0]?.oldLine).toBeNull()
  })

  it('增删行的行号各归一侧，正文已去掉前缀', () => {
    const lines = parseUnifiedDiff(SAMPLE)

    const deleted = lines.filter((line) => line.kind === 'del')
    const added = lines.filter((line) => line.kind === 'add')

    expect(deleted.map((line) => [line.text, line.oldLine, line.newLine])).toEqual([
      ['const y = 2', 2, null],
      ['old', 10, null]
    ])
    expect(added.map((line) => [line.text, line.oldLine, line.newLine])).toEqual([
      ['const y = 3', null, 2],
      ['const z = 4', null, 3],
      ['new', null, 11]
    ])
  })

  it('上下文行两侧行号同时前进', () => {
    const lines = parseUnifiedDiff(SAMPLE)
    const contexts = lines.filter((line) => line.kind === 'context')

    expect(contexts.map((line) => [line.text, line.oldLine, line.newLine])).toEqual([
      ['const x = 1', 1, 1],
      ['return x', 3, 4],
      ['tail', 11, 12]
    ])
  })

  it('第二个 hunk 的行号按 hunk 头重算，不接着第一个 hunk 走', () => {
    const lines = parseUnifiedDiff(SAMPLE)
    const secondHunkAt = lines.findIndex(
      (line) => line.kind === 'hunk' && line.text.startsWith('@@ -10')
    )
    const next = lines[secondHunkAt + 1]

    expect(next?.kind).toBe('del')
    expect(next?.oldLine).toBe(10)
  })

  it('`\\ No newline at end of file` 当元信息，不占行号', () => {
    const text = ['@@ -1,1 +1,1 @@', '-a', '\\ No newline at end of file', '+b'].join('\n')
    const lines = parseUnifiedDiff(text)

    expect(lines.map((line) => line.kind)).toEqual(['hunk', 'del', 'meta', 'add'])
    expect(lines[3]?.newLine).toBe(1)
  })

  it('CRLF 归一：不会在正文里残留 \\r', () => {
    const lines = parseUnifiedDiff('@@ -1,1 +1,1 @@\r\n-a\r\n+b\r\n')

    expect(lines.map((line) => line.text)).toEqual(['@@ -1,1 +1,1 @@', 'a', 'b'])
    expect(lines.some((line) => line.text.includes('\r'))).toBe(false)
  })

  it('末尾换行不产出多余空行，空输入给空数组', () => {
    const withTrailing = parseUnifiedDiff('@@ -1,1 +1,1 @@\n-a\n+b\n')

    expect(withTrailing).toHaveLength(3)
    expect(parseUnifiedDiff('')).toEqual([])
  })

  it('hunk 内无法归类的行不丢弃，落成 meta', () => {
    const lines = parseUnifiedDiff('@@ -1,1 +1,1 @@\n-weird\n?query\n')

    expect(lines.map((line) => line.kind)).toEqual(['hunk', 'del', 'meta'])
    expect(lines[2]?.text).toBe('?query')
  })
})

describe('gitErrorCode / gitHintForError / describeGitError', () => {
  it('从 `[7002]` 这类文本里取出协议码', () => {
    expect(gitErrorCode('Error: 不是 Git 仓库 [-7002]')).toBe(-7002)
    expect(gitErrorCode('git.status 失败 [7003]: exit 128')).toBe(7003)
    expect(gitErrorCode('没有错误码')).toBeNull()
  })

  it('每个 Git 错误码都有一句人话', () => {
    for (const code of [7001, 7002, 7003, 7004, 7005]) {
      expect(gitHintForError(code).length).toBeGreaterThan(0)
    }
    expect(gitHintForError(9999)).toBe('')
  })

  it('describeGitError 保留原文并附上提示（原文是用户贴报错的凭据）', () => {
    const message = describeGitError(new Error('git.status 失败 [7002]: not a repository'))

    expect(message).toContain('not a repository')
    expect(message).toContain('不是 Git 仓库')
  })

  it('未知错误码时原样返回，不瞎编提示', () => {
    expect(describeGitError(new Error('未知 [8888]'))).toBe('未知 [8888]')
    expect(describeGitError('boom')).toBe('boom')
  })
})
