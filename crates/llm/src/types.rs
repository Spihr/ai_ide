//! 模型接入层的运行期类型。
//!
//! 这里刻意与协议 DTO 分层：`ai-ide-protocol` 描述的是「跨进程 JSON 长什么样」，
//! 本模块描述的是「内核内部怎么流转」。两者由 `ai-ide-core` 做映射，
//! 好处是上游适配器不需要认识 JSON-RPC，也不必关心事件怎么发给 Main。
//!
//! 复用协议里已经定稿的语义类型（消息、工具、结束原因、模型条目），避免同名结构漂移。

use ai_ide_protocol::dto::{ChatMessage, ChatParams, ModelRef, ToolChoice, ToolSpec};
use tokio::sync::mpsc::UnboundedSender;

pub use ai_ide_protocol::dto::{FinishReason, ModelInfo};

/// 上游返回的用量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// 命中提示缓存的 token 数；上游未报为 `None`。
    pub cached_tokens: Option<u64>,
}

/// Provider 流式输出的单帧事件，由 `ai-ide-core` 翻译成 `llm.*` 协议事件。
///
/// 约定：**流的开始与结束不由 provider 负责**——`llm.started` 由 core 在派发时立即发出，
/// `llm.finished` / `llm.error` 由 core 依据 provider 的返回值统一发出。
/// 这样「一次流恰好一条 started、且 finished 与 error 互斥」这条不变量只有一个实现点。
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// 正文增量。
    Delta { text: String },
    /// 思维链增量。
    Reasoning { text: String },
    /// 工具调用增量（首帧带 id/name，后续帧只带参数片段）。
    ToolCall {
        index: u32,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: String,
    },
    /// 用量统计。
    Usage(Usage),
    /// 上游给出的结束原因。
    Finished {
        reason: FinishReason,
        message_id: Option<String>,
    },
}

/// 事件出口。core 侧有一个转发任务在另一端把事件写进 JSON-RPC 通知通道。
pub type EventSink = UnboundedSender<StreamEvent>;

/// 把事件推给 sink；返回 `false` 表示接收端已关闭（调用方应立刻停止读取上游流）。
pub fn emit(sink: &EventSink, event: StreamEvent) -> bool {
    sink.send(event).is_ok()
}

/// 一次流式对话请求。
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    pub model: ModelRef,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub tool_choice: Option<ToolChoice>,
    pub params: ChatParams,
}

/// 连通性探测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthReport {
    /// 是否连通。
    pub ok: bool,
    /// 往返耗时（毫秒）。
    pub latency_ms: u64,
    /// 拉到的模型条数。
    pub models_found: usize,
    /// 失败原因或附加说明。
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn emit_reports_closed_sink() {
        let (tx, rx) = unbounded_channel();
        assert!(emit(&tx, StreamEvent::Delta { text: "a".into() }));
        drop(rx);
        assert!(
            !emit(&tx, StreamEvent::Delta { text: "b".into() }),
            "接收端关闭后必须能感知，否则会一直空转读上游"
        );
    }
}
