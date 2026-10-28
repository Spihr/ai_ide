/**
 * 外部视口（docs/24 §5.4）在渲染侧的两条纯逻辑：错误码翻译、按钮可见性。
 *
 * 单独成文件而不是塞进 `PluginPanel.tsx`，是因为这里的分支**全部会被用户看见**：
 * 原因码翻译错了，用户会照着错误提示做错事（比如 `program_escape` 提示「重装插件」
 * 就等于把真正的清单问题藏掉了）。纯函数才好逐条钉住。
 */

/**
 * 主进程递过来的原因码 → 面板文案。
 *
 * 每一句都写「下一步该做什么」，而不是重复错误名：这类失败用户没法自己修，
 * 只能靠提示选对动作（重装 / 看日志 / 升级）。
 */
export const EXTERNAL_VIEW_ERROR_HINTS: Readonly<Record<string, string>> = {
  install_missing:
    '内核没给出这个插件的安装目录，无法定位视口程序。重装一次插件即可恢复（旧记录可能是升级前写下的）。',
  program_invalid:
    '清单里的 external.program 必须是插件包内的相对路径且以 .exe 结尾，不能是绝对路径或含空字符。这是插件清单写错了，联系插件作者。',
  program_escape:
    '清单试图指向插件安装目录之外的程序，已拒绝启动。若这不是你自己改的清单，建议卸载该插件。',
  program_ext: '外部视口只允许 .exe：.bat / .cmd / .ps1 会经过 shell，等于绕过参数隔离。',
  program_missing: '插件安装目录里找不到清单声明的那个 .exe，安装包可能不完整，重装一次插件。',
  pipe_timeout:
    '视口进程已启动，但 10 秒内没有连上命名管道。多半是该版本还没实现 --pipe，请看插件日志或升级插件。',
  spawn_failed: '视口进程刚起来就退出了（退出码见上面的说明），先看插件日志。',
  protocol_unsupported: '这个视图的会话协议版本宿主不认识：升级 IDE，或换成支持当前协议的插件版本。',
  plugin_invalid: '插件 id 或视图 id 不合法，宿主拒绝启动视口。',
  view_missing: '这个插件没有声明该视图，插件可能刚被重装或换版。'
}

/**
 * 把主进程的失败翻成人话。
 *
 * 主进程统一用 `…（code=xxx）` 收尾；解析不到 code 时原样返回——宁可露出原始
 * 文本，也不要编一个听起来合理的原因。
 */
export function describeExternalViewError(error: unknown): string {
  const text = (error instanceof Error ? error.message : String(error)).trim()
  if (text === '') return '外部视口启动失败（主进程未给原因）。'

  // `Error` 过 IPC 后 message 会带一层 "Error invoking remote method ...: "，
  // 只取最后一个 `code=` 更稳。
  const match = /code=([a-z_]+)/.exec(text)
  const code = match?.[1]
  const hint = code === undefined ? undefined : EXTERNAL_VIEW_ERROR_HINTS[code]
  if (hint === undefined) return text
  return hint
}

/** 打开成功后的回执：复用与新建要说得不一样，否则用户会以为点了两下起了两个进程。 */
export function describeExternalViewOpened(title: string, reused: boolean): string {
  return reused
    ? `「${title}」的外部视口已在运行，复用已有进程（没有重开一个窗口）。`
    : `已启动「${title}」的外部视口进程。`
}

/**
 * 视口进程退出后的说明（docs/24 §5.3）。
 *
 * 区分「退出码 0」与「非 0」：前者是用户自己关窗，后者是崩了，两种情况该给的
 * 下一步完全不同——一个不用管，一个要去翻插件日志。
 */
export function describeExternalViewExit(title: string, exitCode: number | null): string {
  const name = `「${title}」的视口`
  if (exitCode === 0) return `${name}已关闭。`
  if (exitCode === null) return `${name}已关闭（进程被终止，拿不到退出码）。`
  return `${name}已退出（退出码 ${exitCode}），可能崩溃了，请看插件日志；再点一次可以重开。`
}

/**
 * 这个视图要不要显示「在视口中打开」。
 *
 * 判据只有 `external` 是否存在：`location` 决定挂在哪，`entry` 决定内容怎么取，
 * 两者都不代表「有一个独立窗口可开」。
 */
export function hasExternalViewport(view: { external?: unknown }): boolean {
  return view.external !== undefined && view.external !== null
}
