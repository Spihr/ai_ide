/**
 * AI 侧栏（M2 外壳侧的入口）：Provider 设置 + 对话两段。
 *
 * 订阅**不在这里**做：`initAi()` 由 `App` 在应用启动时调用一次，常驻整个进程。
 * 侧栏只是「AI 视图的一个渲染分支」，切到资源管理器时它会被卸载——订阅若挂在这里，
 * 卸载期间到达的 `llm.delta` 会丢字、`llm.finished` 会被漏掉，重新进来时那条流
 * 就永远停在「生成中…」。视图卸载不该影响数据流。
 */

import { ChatPanel } from './ChatPanel'
import { ProviderSettings } from './ProviderSettings'

export function AiPanel(): React.JSX.Element {
  return (
    <div className="ai" data-testid="ai-panel">
      <ProviderSettings />
      <ChatPanel />
    </div>
  )
}
