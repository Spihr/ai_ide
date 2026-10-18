/**
 * 搜索面板：文件名（模糊）与文件内容（文本/正则）两种检索。
 *
 * 输入做 250ms 防抖——内核每次检索都要扫工作区，逐字符触发太浪费。
 */

import { useEffect, useRef, useState } from 'react'

import { openMatch, runContentSearch, runFileSearch, useEditor } from '../store'

type Mode = 'files' | 'content'

export function SearchPanel() {
  const state = useEditor()
  const [mode, setMode] = useState<Mode>('files')
  const [fileQuery, setFileQuery] = useState(state.fileQuery)
  const [contentQuery, setContentQuery] = useState(state.contentQuery)
  const [isRegex, setIsRegex] = useState(false)
  const [caseSensitive, setCaseSensitive] = useState(false)
  const timerRef = useRef<number | null>(null)

  const schedule = (run: () => void): void => {
    if (timerRef.current !== null) window.clearTimeout(timerRef.current)
    timerRef.current = window.setTimeout(run, 250)
  }

  useEffect(
    () => () => {
      if (timerRef.current !== null) window.clearTimeout(timerRef.current)
    },
    []
  )

  const onFileQueryChange = (value: string): void => {
    setFileQuery(value)
    schedule(() => void runFileSearch(value))
  }

  const onContentQueryChange = (value: string): void => {
    setContentQuery(value)
    schedule(() => void runContentSearch(value, { isRegex, caseSensitive }))
  }

  const toggleRegex = (value: boolean): void => {
    setIsRegex(value)
    if (contentQuery.trim() !== '') {
      schedule(() => void runContentSearch(contentQuery, { isRegex: value, caseSensitive }))
    }
  }

  const toggleCase = (value: boolean): void => {
    setCaseSensitive(value)
    if (contentQuery.trim() !== '') {
      schedule(() => void runContentSearch(contentQuery, { isRegex, caseSensitive: value }))
    }
  }

  return (
    <div className="search">
      <div className="search__modes" role="tablist">
        <button
          type="button"
          role="tab"
          aria-selected={mode === 'files'}
          className={`search__mode${mode === 'files' ? ' is-active' : ''}`}
          onClick={() => setMode('files')}
        >
          文件名
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={mode === 'content'}
          className={`search__mode${mode === 'content' ? ' is-active' : ''}`}
          onClick={() => setMode('content')}
        >
          内容
        </button>
      </div>

      {mode === 'files' ? (
        <input
          className="search__input"
          placeholder="模糊匹配文件名…"
          value={fileQuery}
          spellCheck={false}
          onChange={(event) => onFileQueryChange(event.target.value)}
        />
      ) : (
        <>
          <input
            className="search__input"
            placeholder="在文件内容中查找…"
            value={contentQuery}
            spellCheck={false}
            onChange={(event) => onContentQueryChange(event.target.value)}
          />
          <div className="search__flags">
            <label className="search__flag">
              <input
                type="checkbox"
                checked={isRegex}
                onChange={(event) => toggleRegex(event.target.checked)}
              />
              正则
            </label>
            <label className="search__flag">
              <input
                type="checkbox"
                checked={caseSensitive}
                onChange={(event) => toggleCase(event.target.checked)}
              />
              区分大小写
            </label>
          </div>
        </>
      )}

      {state.searchError && <p className="panel__error">{state.searchError}</p>}
      {state.searching && <p className="panel__hint">检索中…</p>}

      {mode === 'files' && !state.searching && (
        <ul className="results">
          {state.fileMatches.map((match) => (
            <li key={match.path}>
              <button
                type="button"
                className="results__row"
                title={match.path}
                onClick={() => void openRelative(match.path)}
              >
                <span className="results__path">{match.path}</span>
              </button>
            </li>
          ))}
          {state.fileQuery.trim() !== '' && state.fileMatches.length === 0 && (
            <li className="panel__hint">没有匹配的文件</li>
          )}
        </ul>
      )}

      {mode === 'content' && !state.searching && (
        <ul className="results">
          {state.contentMatches.map((match) => (
            <li key={`${match.path}:${match.line}:${match.column}`}>
              <button
                type="button"
                className="results__row"
                title={`${match.path}:${match.line}`}
                onClick={() => void openMatch(match.path, match.line, match.column)}
              >
                <span className="results__path">
                  {match.path}
                  <span className="results__line">:{match.line}</span>
                </span>
                <span className="results__preview">{match.preview.trim()}</span>
              </button>
            </li>
          ))}
          {state.contentTruncated && (
            <li className="panel__hint">命中过多，已截断结果</li>
          )}
          {state.contentQuery.trim() !== '' && state.contentMatches.length === 0 && (
            <li className="panel__hint">没有匹配的内容</li>
          )}
        </ul>
      )}
    </div>
  )
}

/** 文件名搜索结果是相对路径，按「命中即打开」处理。 */
async function openRelative(relPath: string): Promise<void> {
  await openMatch(relPath, 1, 1)
}
