//! 消息拼装：把循环里发生的三件事翻译成协议 [`ChatMessage`]。
//!
//! 只有一条不变量：**助手消息里的每个 `tool_calls[i].id` 都必须有且只有一条
//! `role = tool` 的消息回应它**。OpenAI 兼容的上游对不上的调用会直接报错，所以
//! 「撞上步数上限提前收工」这种路径也必须给没来得及执行的调用补一条结果消息
//! （见 `runner::AgentLoop::run`）。

use ai_ide_protocol::dto::{ChatAttachment, ChatMessage, ChatRole, ChatToolCall};

/// 系统消息。
pub fn system_message(text: impl Into<String>) -> ChatMessage {
    plain(ChatRole::System, text.into())
}

/// 用户消息。
pub fn user_message(text: impl Into<String>) -> ChatMessage {
    plain(ChatRole::User, text.into())
}

/// 助手消息：文本 +（可选的）工具调用。
pub fn assistant_message(text: impl Into<String>, tool_calls: &[ChatToolCall]) -> ChatMessage {
    ChatMessage {
        role: ChatRole::Assistant,
        content: text.into(),
        attachments: Vec::new(),
        tool_calls: tool_calls.to_vec(),
        tool_call_id: None,
    }
}

/// 工具结果消息（成功）。
pub fn tool_message(call_id: &str, content: impl Into<String>) -> ChatMessage {
    ChatMessage {
        role: ChatRole::Tool,
        content: content.into(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: Some(call_id.to_string()),
    }
}

/// 工具结果消息（失败）。
///
/// 正文包一层 `<tool_error>`：模型必须能一眼分清「工具返回了内容」与「工具没干成」，
/// 后者才是它该改参数重试的信号。
pub fn tool_error_message(call_id: &str, tool: &str, reason: &str) -> ChatMessage {
    tool_message(
        call_id,
        format!(
            "<tool_error tool=\"{}\">{}</tool_error>",
            escape_xml(tool),
            escape_xml(reason)
        ),
    )
}

/// XML 转义：工具报错与检索词里可能出现 `<`、`&`、`"`，不转义会把标签撑坏。
pub(crate) fn escape_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn plain(role: ChatRole, content: String) -> ChatMessage {
    ChatMessage {
        role,
        content,
        attachments: Vec::<ChatAttachment>::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, name: &str) -> ChatToolCall {
        ChatToolCall {
            id: id.into(),
            name: name.into(),
            arguments: "{}".into(),
        }
    }

    #[test]
    fn assistant_message_carries_tool_calls() {
        let message = assistant_message("先看看", &[call("c1", "read")]);
        assert_eq!(message.role, ChatRole::Assistant);
        assert_eq!(message.content, "先看看");
        assert_eq!(message.tool_calls.len(), 1);
        assert_eq!(message.tool_calls[0].id, "c1");
        assert!(message.tool_call_id.is_none());
    }

    #[test]
    fn tool_message_links_the_call_id() {
        let message = tool_message("c1", "<file path=\"a\" />");
        assert_eq!(message.role, ChatRole::Tool);
        assert_eq!(message.tool_call_id.as_deref(), Some("c1"));
        assert!(message.tool_calls.is_empty());
    }

    #[test]
    fn tool_error_is_wrapped_and_escaped() {
        let message = tool_error_message("c1", "edit", "old_text <a> 出现 2 次 & 无法唯一匹配");
        assert!(message.content.starts_with("<tool_error tool=\"edit\">"));
        assert!(message.content.ends_with("</tool_error>"));
        assert!(message.content.contains("&lt;a&gt;"));
        assert!(message.content.contains("&amp;"));
    }
}
