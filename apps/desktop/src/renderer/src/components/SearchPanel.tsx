/**
 * 搜索面板：文件名（模糊）、文件内容（文本/正则）与符号（`search.symbols`）三种检索。
 *
 * 输入做 250ms 防抖——内核每次检索都要扫工作区，逐字符触发太浪费。
 *
 * 「符号」模式与 `Ctrl+T` 的快速选择器**共用**同一份检索结果和同一套分组
 * （`store.runSymbolSearch` + `symbol-search.groupSymbols`）：面板适合「边搜边看
 * 一屏」，浮层适合「打完就跳」，两者看到的东西必须是同一个，否则用户会怀疑自己搜错了。
 */

import { useEffect, useMemo, useRef, useState } from 'react'

import {
  openMatch,
  openSymbol,
  runContentSearch,
  runFileSearch,
  runSymbolSearch,
  useEditor
} from '../store'
import { groupSymbols, symbolKey, symbolLocationLabel } from '../symbol-search'

type Mode = 'files' | 'content' | 'symbols'

export function SearchPanel() {
  const state = useEditor()
  const [mode, setMode] = useState<Mode>('files')
  const [fileQuery, setFileQuery] = useState(state.fileQuery)
  const [contentQuery, setContentQuery] = useState(state.contentQuery)
  const [symbolQuery, setSymbolQuery] = useState(state.symbolQuery)
  const [isRegex, setIsRegex] = useState(false)
  const [caseSensitive, setCaseSensitive] = useState(false)
  const timerRef = useRef<number | null>(null)

  const symbolGroups = useMemo(() => groupSymbols(state.symbolMatches), [state.symbolMatches])

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

  const onSymbolQueryChange = (value: string): void => {
    setSymbolQuery(value)
    // 面板里空输入就清空结果（与文件名 / 内容一致），不发全库列举请求。
    schedule(() => void runSymbolSearch(value))
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
        <button
          type="button"
          role="tab"
          aria-selected={mode === 'symbols'}
          className={`search__mode${mode === 'symbols' ? ' is-active' : ''}`}
          onClick={() => setMode('symbols')}
        >
          符号
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
      ) : mode === 'symbols' ? (
        <input
          className="search__input"
          placeholder="按名字查找函数 / 类 / 常量…"
          value={symbolQuery}
          spellCheck={false}
          onChange={(event) => onSymbolQueryChange(event.target.value)}
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

      {mode !== 'symbols' && state.searchError && (
        <p className="panel__error">{state.searchError}</p>
      )}
      {mode !== 'symbols' && state.searching && <p className="panel__hint">检索中…</p>}
      {mode === 'symbols' && state.symbolError && (
        <p className="panel__error">{state.symbolError}</p>
      )}
      {mode === 'symbols' && state.symbolSearching && (
        <p className="panel__hint">检索中…</p>
      )}

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

      {mode === 'symbols' && !state.symbolSearching && (
        <div className="results" data-testid="search-symbol-results">
          {symbolGroups.map((group) => (
            <div className="symbol-group" key={group.kind}>
              <div className="symbol-group__title">
                <span className="symbol-picker__glyph" aria-hidden="true">
                  {group.glyph}
                </span>
                {group.label}
                <span className="palette__group-count">{group.symbols.length}</span>
              </div>
              {group.symbols.map((symbol) => (
                <button
                  type="button"
                  key={symbolKey(symbol)}
                  className="results__row"
                  title={symbolLocationLabel(symbol)}
                  data-testid={`search-symbol-${symbolKey(symbol)}`}
                  onClick={() => void openSymbol(symbol)}
                >
                  <span className="results__path">
                    {symbol.name}
                    <span className="results__line">{`:${symbol.range.start_line}`}</span>
                  </span>
                  <span className="results__preview">{symbol.path}</span>
                </button>
              ))}
            </div>
          ))}
          {symbolQuery.trim() !== '' && state.symbolMatches.length === 0 && (
            <p className="panel__hint">没有匹配的符号</p>
          )}
          {symbolQuery.trim() === '' && state.symbolMatches.length > 0 && (
            <p className="panel__hint">共 {state.symbolMatches.length} 个符号，输入名字缩小范围</p>
          )}
        </div>
      )}
    </div>
  )
}

/** 文件名搜索结果是相对路径，按「命中即打开」处理。 */
async function openRelative(relPath: string): Promise<void> {
  await openMatch(relPath, 1, 1)
}
