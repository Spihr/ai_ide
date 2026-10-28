/**
 * 插件声明视图的宿主（docs/24 §7.3 第 3 刀）。
 *
 * 职责边界：**只画，不判**。所有「能不能渲染 / 截到多少 / 按钮该不该禁」的判断
 * 都在 `plugin-view.ts` 里做完，本文件拿到的永远是已归一、已限幅的 `ViewNode`，
 * 于是渲染层只剩一个 switch。
 *
 * 插件代码不进渲染进程：这里渲染的是数据，没有一个控件接受 HTML 字符串，
 * 也不存在 `dangerouslySetInnerHTML`。命中危险协议的节点（`tainted`）会被降级
 * 成纯文本——连按钮都不生成，自然没有可点元素（V-7）。
 *
 * 单节点错误边界：某一类控件炸了只让那一块显示占位，其余照常渲染。
 */

import { Component, useState } from 'react'
import type { ReactNode } from 'react'

import {
  actionState,
  type ParsedView,
  type ViewAction,
  type ViewItem,
  type ViewNode
} from '../plugin-view'

export interface PluginViewHostProps {
  /** `parseView` 的结果；`null` 表示还没拉到视图。 */
  view: ParsedView | null
  /** 本插件声明的命令名，用于按钮禁用态判定。 */
  commands?: readonly string[]
  /** 正在执行动作时禁用所有按钮，避免连点。 */
  busy?: boolean
  /** 动作回传：只有 `plugin.command` 走审批链，本组件绝不自作主张执行。 */
  onAction?: (action: ViewAction, node: ViewNode) => void
}

interface RenderContext {
  commands: readonly string[]
  busy: boolean
  onAction: ((action: ViewAction, node: ViewNode) => void) | undefined
  /** 节点路径，只用于错误边界文案与调试定位。 */
  path: string
}

interface NodeProps {
  node: ViewNode
  ctx: RenderContext
}

/** 单节点错误边界：降级粒度是「一个控件」，不是整棵树。 */
class NodeBoundary extends Component<{ path: string; children: ReactNode }, { failed: boolean }> {
  constructor(props: { path: string; children: ReactNode }) {
    super(props)
    this.state = { failed: false }
  }

  static getDerivedStateFromError(): { failed: boolean } {
    return { failed: true }
  }

  override render(): ReactNode {
    if (this.state.failed) {
      return (
        <div className="plugin-view__node-error" data-testid="plugin-view-node-error">
          该控件渲染失败（{this.props.path}），已跳过
        </div>
      )
    }
    return this.props.children
  }
}

/** 子节点统一走边界 + 容器。 */
function renderChildren(node: ViewNode, ctx: RenderContext): ReactNode {
  const children = node.children ?? []
  if (children.length === 0) return null
  return children.map((child, index) => (
    <NodeBoundary key={`${ctx.path}.${index}`} path={`${ctx.path}[${index}]`}>
      <NodeView node={child} ctx={{ ...ctx, path: `${ctx.path}[${index}]` }} />
    </NodeBoundary>
  ))
}

function formatNumber(value: number): string {
  return Number.isInteger(value) ? String(value) : value.toFixed(2)
}

function ViewLayout({ node, ctx, className }: NodeProps & { className: string }): ReactNode {
  return (
    <div className={className}>
      {node.title !== undefined ? <div className="plugin-view__title">{node.title}</div> : null}
      {renderChildren(node, ctx)}
    </div>
  )
}

function ViewText({ node }: NodeProps): ReactNode {
  const text = node.value ?? node.label ?? ''
  if (text === '') return null
  return (
    <p className={`plugin-view__text${node.tainted ? ' is-tainted' : ''}`} title={node.tainted ? '含外部链接文本，按纯文本显示' : undefined}>
      {text}
    </p>
  )
}

function ViewStat({ node }: NodeProps): ReactNode {
  return (
    <div className="plugin-view__stat">
      <span className="plugin-view__stat-label">{node.label ?? ''}</span>
      <span className={`plugin-view__stat-value${node.tainted ? ' is-tainted' : ''}`}>{node.value ?? '—'}</span>
    </div>
  )
}

function ViewGroup({ node, ctx }: NodeProps): ReactNode {
  return (
    <fieldset className="plugin-view__group">
      <legend className="plugin-view__group-title">{node.title ?? node.label ?? '分组'}</legend>
      {renderChildren(node, ctx)}
    </fieldset>
  )
}

function ViewTable({ node }: NodeProps): ReactNode {
  const columns = node.columns ?? []
  const rows = node.rows ?? []
  if (columns.length === 0 && rows.length === 0) {
    return <p className="plugin-view__text">（空表）</p>
  }
  return (
    <div className="plugin-view__table-wrap">
      <table className="plugin-view__table">
        {columns.length > 0 ? (
          <thead>
            <tr>
              {columns.map((column, index) => (
                <th key={`${column}-${index}`}>{column}</th>
              ))}
            </tr>
          </thead>
        ) : null}
        <tbody>
          {rows.map((row, rowIndex) => (
            <tr key={rowIndex} className={row.tainted ? 'is-tainted' : undefined}>
              {row.cells.map((cell, cellIndex) => (
                <td key={cellIndex}>{cell}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

function ViewItems({ items, className, renderLabel }: { items: ViewItem[]; className: string; renderLabel?: (item: ViewItem) => ReactNode }): ReactNode {
  if (items.length === 0) return <p className="plugin-view__text">（无条目）</p>
  return (
    <ul className={className}>
      {items.map((item) => (
        <li className="plugin-view__list-item" key={item.key}>
          {renderLabel ? renderLabel(item) : item.label}
          {item.children && item.children.length > 0 ? (
            <ViewItems items={item.children} className="plugin-view__tree-children" />
          ) : null}
        </li>
      ))}
    </ul>
  )
}

function ViewTree({ node }: NodeProps): ReactNode {
  return <ViewItems items={node.items ?? []} className="plugin-view__tree" />
}

function ViewProgress({ node }: NodeProps): ReactNode {
  const percent = node.progress ?? 0
  return (
    <div className="plugin-view__progress">
      <div className="plugin-view__progress-bar" role="progressbar" aria-valuenow={percent} aria-valuemin={0} aria-valuemax={100}>
        <div className="plugin-view__progress-fill" style={{ width: `${percent}%` }} />
      </div>
      <span className="plugin-view__progress-value">
        {node.label !== undefined ? `${node.label} ` : ''}
        {formatNumber(percent)}%
      </span>
    </div>
  )
}

function ViewTabs({ node, ctx }: NodeProps): ReactNode {
  const items = node.items ?? []
  const [active, setActive] = useState(0)
  if (items.length === 0) return <p className="plugin-view__text">（无页签）</p>
  // 插件可能删页：索引越界时回落到第一页，而不是崩掉。
  const index = active < items.length ? active : 0
  const current = items[index]
  return (
    <div className="plugin-view__tabs">
      <div className="plugin-view__tab-strip" role="tablist">
        {items.map((item, itemIndex) => (
          <button
            key={item.key}
            type="button"
            role="tab"
            aria-selected={itemIndex === index}
            className={`plugin-view__tab${itemIndex === index ? ' is-active' : ''}`}
            onClick={() => setActive(itemIndex)}
          >
            {item.label}
          </button>
        ))}
      </div>
      <div className="plugin-view__tab-body" role="tabpanel">
        {current?.content && current.content.length > 0 ? (
          current.content.map((child, childIndex) => (
            <NodeBoundary key={`${current.key}.${childIndex}`} path={`tabs[${index}][${childIndex}]`}>
              <NodeView node={child} ctx={{ ...ctx, path: `tabs[${index}][${childIndex}]` }} />
            </NodeBoundary>
          ))
        ) : current !== undefined && current.children !== undefined ? (
          <ViewItems items={current.children} className="plugin-view__list" />
        ) : (
          <p className="plugin-view__text">（本页没有内容）</p>
        )}
      </div>
    </div>
  )
}

/** 输入类控件统一外壳：一个标签 + 一个控件槽。 */
function Field({ node, children }: { node: ViewNode; children: ReactNode }): ReactNode {
  return (
    <label className="plugin-view__field">
      <span className="plugin-view__field-label">{node.label ?? node.title ?? ''}</span>
      {children}
    </label>
  )
}

function ViewToggle({ node, ctx }: NodeProps): ReactNode {
  const [checked, setChecked] = useState(node.checked === true)
  if (node.tainted) return <TextualField node={node} value={checked ? '开' : '关'} />
  const state = actionState(node, ctx.commands)
  return (
    <Field node={node}>
      <input
        type="checkbox"
        className="plugin-view__checkbox"
        checked={checked}
        disabled={!state.enabled || ctx.busy}
        onChange={(event) => {
          const next = event.target.checked
          setChecked(next)
          if (node.action !== undefined) ctx.onAction?.(node.action, node)
        }}
      />
    </Field>
  )
}

function ViewSlider({ node, ctx }: NodeProps): ReactNode {
  const min = node.min ?? 0
  const max = node.max ?? 100
  const step = node.step ?? 1
  const [value, setValue] = useState(node.current ?? String(min))
  if (node.tainted) return <TextualField node={node} value={value} />
  const state = actionState(node, ctx.commands)
  return (
    <Field node={node}>
      <span className="plugin-view__slider">
        <input
          type="range"
          min={min}
          max={max}
          step={step}
          value={value}
          disabled={!state.enabled || ctx.busy}
          onChange={(event) => setValue(event.target.value)}
          onPointerUp={() => {
            if (node.action !== undefined) ctx.onAction?.(node.action, node)
          }}
        />
        <span className="plugin-view__slider-value">{value}</span>
      </span>
    </Field>
  )
}

function ViewSelect({ node, ctx }: NodeProps): ReactNode {
  const options = node.options ?? []
  const [value, setValue] = useState(node.current ?? options[0]?.key ?? '')
  if (node.tainted) return <TextualField node={node} value={value} />
  const state = actionState(node, ctx.commands)
  return (
    <Field node={node}>
      <select
        className="plugin-view__select"
        value={value}
        disabled={!state.enabled || ctx.busy}
        onChange={(event) => {
          setValue(event.target.value)
          if (node.action !== undefined) ctx.onAction?.(node.action, node)
        }}
      >
        {options.map((option) => (
          <option key={option.key} value={option.key}>
            {option.label}
          </option>
        ))}
      </select>
    </Field>
  )
}

function ViewColor({ node, ctx }: NodeProps): ReactNode {
  const value = node.current ?? '#000000'
  if (node.tainted) return <TextualField node={node} value={value} />
  const state = actionState(node, ctx.commands)
  return (
    <Field node={node}>
      <span className="plugin-view__color">
        <input
          type="color"
          value={value}
          disabled={!state.enabled || ctx.busy}
          onChange={() => {
            if (node.action !== undefined) ctx.onAction?.(node.action, node)
          }}
        />
        <code>{value}</code>
      </span>
    </Field>
  )
}

function ViewButton({ node, ctx }: NodeProps): ReactNode {
  const label = node.label ?? node.title ?? node.value ?? '执行'
  // 含外部协议文本时不生成任何可点击元素（V-7）。
  if (node.tainted) return <TextualField node={node} value={label} />
  const state = actionState(node, ctx.commands)
  return (
    <button
      type="button"
      className="ai-button plugin-view__button"
      disabled={!state.enabled || ctx.busy || node.action === undefined}
      title={state.reason ?? undefined}
      onClick={() => {
        if (node.action !== undefined) ctx.onAction?.(node.action, node)
      }}
    >
      {label}
    </button>
  )
}

/** tainted 或结构不完整的输入控件统一降级为纯文本，不生成任何可交互元素。 */
function TextualField({ node, value }: { node: ViewNode; value: string }): ReactNode {
  return (
    <div className="plugin-view__field is-tainted">
      <span className="plugin-view__field-label">{node.label ?? node.title ?? ''}</span>
      <span className="plugin-view__field-value">{value}</span>
      <span className="plugin-view__field-note">含外部链接文本，已降级为只读</span>
    </div>
  )
}

function ViewUnknown({ node }: NodeProps): ReactNode {
  return (
    <div className="plugin-view__unknown" data-testid="plugin-view-unknown">
      未知控件 <code>{node.type}</code>
      {node.label !== undefined ? ` · ${node.label}` : ''}
      {node.value !== undefined ? ` · ${node.value}` : ''}
    </div>
  )
}

function NodeView({ node, ctx }: NodeProps): ReactNode {
  switch (node.type) {
    case 'row':
      return <ViewLayout node={node} ctx={ctx} className="plugin-view__row" />
    case 'column':
      return <ViewLayout node={node} ctx={ctx} className="plugin-view__column" />
    case 'group':
      return <ViewGroup node={node} ctx={ctx} />
    case 'tabs':
      return <ViewTabs node={node} ctx={ctx} />
    case 'text':
      return <ViewText node={node} ctx={ctx} />
    case 'stat':
      return <ViewStat node={node} ctx={ctx} />
    case 'table':
      return <ViewTable node={node} ctx={ctx} />
    case 'list':
      return <ViewItems items={node.items ?? []} className="plugin-view__list" />
    case 'tree':
      return <ViewTree node={node} ctx={ctx} />
    case 'progress':
      return <ViewProgress node={node} ctx={ctx} />
    case 'toggle':
      return <ViewToggle node={node} ctx={ctx} />
    case 'slider':
      return <ViewSlider node={node} ctx={ctx} />
    case 'select':
      return <ViewSelect node={node} ctx={ctx} />
    case 'color':
      return <ViewColor node={node} ctx={ctx} />
    case 'button':
      return <ViewButton node={node} ctx={ctx} />
    default:
      return <ViewUnknown node={node} ctx={ctx} />
  }
}

/**
 * 视图渲染入口。
 *
 * 四种终态：未加载（`view === null`）、被拒（`rejected`）、正常、空树。
 * 诊断与截断提示统一收在底部一行，不打断主内容。
 */
export function PluginViewHost({ view, commands, busy, onAction }: PluginViewHostProps): ReactNode {
  const ctx: RenderContext = {
    commands: commands ?? [],
    busy: busy === true,
    onAction,
    path: 'root'
  }

  if (view === null) {
    return <p className="panel__hint">还没有拉到视图内容。</p>
  }

  if (view.status === 'rejected') {
    return (
      <div className="plugin-view__error" data-testid="plugin-view-rejected">
        <strong>视图无法渲染</strong>
        <p>{view.error ?? '描述格式不符合 aiide.view/1'}</p>
      </div>
    )
  }

  if (view.root === null) {
    return <p className="panel__hint">插件返回了空视图。</p>
  }

  const issues = view.issues
  return (
    <div className="plugin-view" data-testid="plugin-view">
      <NodeBoundary path="root">
        <NodeView node={view.root} ctx={ctx} />
      </NodeBoundary>
      {view.truncated ? (
        <p className="plugin-view__truncated" data-testid="plugin-view-truncated">
          内容超过宿主上限，已截断显示（节点 {view.nodeCount}）
        </p>
      ) : null}
      {issues.length > 0 ? (
        <details className="plugin-view__diag">
          <summary>
            渲染诊断（{issues.length}
            {issues.length >= 32 ? '+' : ''}）
          </summary>
          <ul>
            {issues.map((issue, index) => (
              <li key={`${issue.code}-${index}`}>
                <code>{issue.code}</code> {issue.path} · {issue.message}
              </li>
            ))}
          </ul>
        </details>
      ) : null}
    </div>
  )
}
