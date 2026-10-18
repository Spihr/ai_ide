/** 工作区文件树：按目录懒加载，点目录展开、点文件打开标签。 */

import type { TreeEntry } from '@ai-ide/shared-types'

import { openFile, toggleDir, useEditor } from '../store'

function toRel(root: string, abs: string): string {
  const base = root.replace(/\\/g, '/').replace(/\/+$/, '')
  const target = abs.replace(/\\/g, '/')
  if (target === base) return ''
  return target.startsWith(`${base}/`) ? target.slice(base.length + 1) : target
}

interface NodeProps {
  entry: TreeEntry
  depth: number
  root: string
}

function TreeNode({ entry, depth, root }: NodeProps) {
  const state = useEditor()
  const isDir = entry.kind === 'dir'
  const rel = toRel(root, entry.path)
  const expanded = isDir && state.expanded.includes(rel)
  const children = isDir ? state.tree[rel] : undefined
  const isActive = !isDir && state.activePath === entry.path

  const handleClick = (): void => {
    if (isDir) {
      void toggleDir(rel)
      return
    }
    void openFile(entry.path)
  }

  return (
    <li className="tree__node">
      <div
        className={[
          'tree__row',
          isActive ? 'is-active' : '',
          entry.ignored ? 'is-ignored' : '',
          entry.kind === 'symlink' ? 'is-symlink' : ''
        ]
          .filter(Boolean)
          .join(' ')}
        style={{ paddingLeft: `${8 + depth * 12}px` }}
        role="treeitem"
        aria-expanded={isDir ? expanded : undefined}
        title={entry.path}
        onClick={handleClick}
      >
        <span className="tree__glyph" aria-hidden="true">
          {isDir ? (expanded ? '▾' : '▸') : '·'}
        </span>
        <span className="tree__name">{entry.name}</span>
      </div>

      {expanded && children && children.length > 0 && (
        <ul className="tree__children" role="group">
          {children.map((child) => (
            <TreeNode key={child.path} entry={child} depth={depth + 1} root={root} />
          ))}
        </ul>
      )}
      {expanded && !children && (
        <div className="tree__hint" style={{ paddingLeft: `${8 + (depth + 1) * 12}px` }}>
          加载中…
        </div>
      )}
    </li>
  )
}

export function FileTree() {
  const state = useEditor()
  const root = state.workspace?.root
  if (!root) return null

  const entries = state.tree[''] ?? []

  if (entries.length === 0) {
    return <p className="panel__hint">目录为空，或仍在加载…</p>
  }

  return (
    <ul className="tree" role="tree">
      {entries.map((entry) => (
        <TreeNode key={entry.path} entry={entry} depth={0} root={root} />
      ))}
    </ul>
  )
}
