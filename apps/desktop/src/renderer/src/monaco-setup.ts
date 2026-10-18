/**
 * Monaco 的装配。
 *
 * 两件事：
 * 1. 告诉 Monaco 去哪个包拿各类语言的 worker（Vite 的 `?worker` 导入）；
 * 2. 注册一套与外壳 CSS 变量同色的暗色主题。
 *
 * 只应该被 `CodeEditor` 引入一次——Monaco 的 `MonacoEnvironment` 是全局单例。
 */

import * as monaco from 'monaco-editor'
// 注意路径：monaco-editor 的 exports 把 `monaco-editor/X` 映射到 `esm/vs/X.js`，
// 所以这里不能再写 `esm/vs/...` 前缀（会解析成 esm/vs/esm/vs/... 而构建失败）。
import editorWorker from 'monaco-editor/editor/editor.worker?worker'
import jsonWorker from 'monaco-editor/language/json/json.worker?worker'
import cssWorker from 'monaco-editor/language/css/css.worker?worker'
import htmlWorker from 'monaco-editor/language/html/html.worker?worker'
import tsWorker from 'monaco-editor/language/typescript/ts.worker?worker'

self.MonacoEnvironment = {
  getWorker(_workerId: string, label: string): Worker {
    switch (label) {
      case 'json':
        return new jsonWorker()
      case 'css':
      case 'scss':
      case 'less':
        return new cssWorker()
      case 'html':
      case 'handlebars':
      case 'razor':
        return new htmlWorker()
      case 'typescript':
      case 'javascript':
        return new tsWorker()
      default:
        return new editorWorker()
    }
  }
}

monaco.editor.defineTheme('ai-ide-dark', {
  base: 'vs-dark',
  inherit: true,
  rules: [
    { token: 'comment', foreground: '6b7688', fontStyle: 'italic' },
    { token: 'keyword', foreground: '7aa2f7' },
    { token: 'string', foreground: '9ece6a' },
    { token: 'number', foreground: 'ff9e64' },
    { token: 'type', foreground: '2ac3de' }
  ],
  colors: {
    'editor.background': '#14161a',
    'editor.foreground': '#e6e9ef',
    'editorLineNumber.foreground': '#4a5262',
    'editorLineNumber.activeForeground': '#8b94a3',
    'editor.lineHighlightBackground': '#1b1f26',
    'editor.selectionBackground': '#2f466b',
    'editorCursor.foreground': '#5aa9ff',
    'editorIndentGuide.background1': '#242a34',
    'editorIndentGuide.activeBackground1': '#3a4353',
    'editorWidget.background': '#1b1f26',
    'editorWidget.border': '#2c333f',
    'editorSuggestWidget.background': '#1b1f26',
    'editorSuggestWidget.selectedBackground': '#222832',
    'scrollbarSlider.background': '#2c333f80',
    'scrollbarSlider.hoverBackground': '#3a435380',
    'minimap.background': '#14161a'
  }
})

export { monaco }

/** 由扩展名/文件名推断 Monaco 语言 id；未知时回落到纯文本。 */
export function languageForPath(path: string): string {
  const name = path.replace(/\\/g, '/').split('/').pop() ?? ''
  const lower = name.toLowerCase()

  const byName: Record<string, string> = {
    dockerfile: 'dockerfile',
    makefile: 'makefile',
    '.gitignore': 'plaintext',
    '.env': 'plaintext'
  }
  if (lower in byName) return byName[lower] as string

  const dot = lower.lastIndexOf('.')
  if (dot < 0) return 'plaintext'
  const ext = lower.slice(dot + 1)

  const byExt: Record<string, string> = {
    ts: 'typescript',
    tsx: 'typescript',
    mts: 'typescript',
    cts: 'typescript',
    js: 'javascript',
    jsx: 'javascript',
    mjs: 'javascript',
    cjs: 'javascript',
    json: 'json',
    jsonc: 'json',
    md: 'markdown',
    markdown: 'markdown',
    css: 'css',
    scss: 'scss',
    less: 'less',
    html: 'html',
    htm: 'html',
    xml: 'xml',
    yml: 'yaml',
    yaml: 'yaml',
    toml: 'ini',
    ini: 'ini',
    rs: 'rust',
    py: 'python',
    go: 'go',
    java: 'java',
    c: 'c',
    h: 'c',
    cc: 'cpp',
    cpp: 'cpp',
    hpp: 'cpp',
    cs: 'csharp',
    sh: 'shell',
    bash: 'shell',
    ps1: 'powershell',
    sql: 'sql',
    vue: 'html',
    svelte: 'html'
  }
  return byExt[ext] ?? 'plaintext'
}
