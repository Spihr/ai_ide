/**
 * 窗口级快捷键的纯映射表。
 *
 * 抽出来的理由：「Ctrl+T 到底该开什么」只在真按键盘时才验证得到，App 里那串
 * if/else 一旦顺序写错（`Ctrl+P` 抢在 `Ctrl+Shift+P` 前面就是典型），单测根本
 * 碰不到。这里输入一个按键描述、输出一个动作名，组件只负责按动作分发。
 *
 * 判定约定与 VS Code 对齐：`Ctrl`（macOS 上是 `Cmd`）为唯一主修饰键；`Shift`
 * 必须单独判定——`event.key` 已经把 `Shift+P` 归一成大写 `'P'`，靠大小写区分
 * 组合键是错的。
 */

/** 快捷键能触发的动作。名字是稳定的对外契约（单测按它断言）。 */
export type ShortcutAction =
  | 'save'
  | 'command-palette'
  | 'search'
  | 'explorer'
  | 'terminal'
  | 'symbols'
  | 'inline-edit'

/** 只取按键里用得到的几个字段，方便测试直接构造，不必造 `KeyboardEvent`。 */
export interface ShortcutEvent {
  key: string
  ctrlKey?: boolean
  metaKey?: boolean
  shiftKey?: boolean
}

/**
 * 解析一次按键。命中返回动作名，不命中返回 `null`（调用方不要 `preventDefault`）。
 */
export function resolveShortcut(event: ShortcutEvent): ShortcutAction | null {
  const primary = event.ctrlKey === true || event.metaKey === true
  if (!primary) return null
  const key = event.key.toLowerCase()
  const shift = event.shiftKey === true
  if (key === 's') return shift ? null : 'save'
  // `Ctrl+Shift+P` 必须在 `Ctrl+P` 之前返回，否则「命令面板」永远被「切到搜索」吃掉。
  if (key === 'p') return shift ? 'command-palette' : 'search'
  if (key === 'b') return 'explorer'
  /**
   * `Ctrl+T`：转到符号（FR-1.7 的后半个入口）。带 Shift 的组合留给后续的
   * 「工作区符号」之类，这里先不吞键。
   */
  if (key === 't') return shift ? null : 'symbols'
  /**
   * `Ctrl+K`：内联编辑（选中代码 → 说一句要改成什么）。与 VS Code 同键。
   *
   * 这里**不判断有没有选区**：拦截键位是窗口级的，判断「选中了没有」要么去读 store
   * （纯函数就不纯了）、要么在 App 里再写一遍。统一交给 `openInlineEdit`——它在
   * 无选区时只弹提示，不会吞掉用户的按键去干别的。
   */
  if (key === 'k') return shift ? null : 'inline-edit'
  // `Ctrl+`` 开关终端；macOS 的 `Cmd+`` 是系统级窗口切换，不抢。
  if (key === '`') return event.metaKey === true ? null : 'terminal'
  return null
}
