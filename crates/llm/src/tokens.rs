//! token 估算。
//!
//! M2 不引入真实分词器（按模型分发 BPE 词表是独立的工程，且会给安装包增重），
//! 因此 `llm.count_tokens` 返回的是**估算值**，并显式带 `exact: false`。
//! 估算口径参考主流 BPE 的实测经验值：
//!
//! - CJK 字符 ≈ 1 token / 字；
//! - 其它非 ASCII 字符 ≈ 1 token / 字（保守）；
//! - ASCII 可见字符 ≈ 1 token / 4 字符；
//! - 每条消息 ≈ 4 token 结构开销，工具声明按其 JSON 文本再估一次。
//!
//! 误差量级在 ±20% 以内，足够驱动 UI 的上下文占用条；真正精确的计数
//! 由上游返回的 `usage` 校正（见 `llm.usage` 事件）。

use ai_ide_protocol::dto::{ChatMessage, ToolSpec};

/// 每条消息固定开销（role、分隔符等）。
const PER_MESSAGE_OVERHEAD: u64 = 4;

/// 估算一组消息 + 工具声明的 token 数。
pub fn estimate_tokens(messages: &[ChatMessage], tools: &[ToolSpec]) -> u64 {
    let mut total: u64 = 0;
    for message in messages {
        total += PER_MESSAGE_OVERHEAD;
        total += estimate_text(&message.content);
        for call in &message.tool_calls {
            total += estimate_text(&call.name);
            total += estimate_text(&call.arguments);
        }
    }
    for tool in tools {
        total += estimate_text(&tool.name);
        if let Some(description) = &tool.description {
            total += estimate_text(description);
        }
        total += estimate_text(&tool.parameters.to_string());
    }
    total
}

/// 估算一段纯文本的 token 数。
pub fn estimate_text(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    let mut wide: u64 = 0;
    let mut ascii: u64 = 0;
    for ch in text.chars() {
        if ch.is_ascii() {
            ascii += 1;
        } else {
            wide += 1;
        }
    }
    // ASCII 部分按 4 字符 1 token 折算，不足 1 个 token 的零头也计 1。
    let ascii_tokens = ascii.div_ceil(4);
    wide + ascii_tokens
}

/// 面向 UI 的估算口径说明（写进 `llm.count_tokens` 的 `note`）。
pub const ESTIMATE_NOTE: &str =
    "内核未接真实分词器，按「CJK 1 字≈1 token、ASCII 4 字符≈1 token、每消息 +4」估算，误差约 ±20%";

#[cfg(test)]
mod tests {
    use super::*;
    use ai_ide_protocol::dto::{ChatRole, ToolChoice};

    fn msg(role: ChatRole, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: content.to_string(),
            attachments: vec![],
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    #[test]
    fn empty_input_costs_nothing() {
        assert_eq!(estimate_tokens(&[], &[]), 0);
        assert_eq!(estimate_text(""), 0);
    }

    #[test]
    fn ascii_is_about_four_chars_per_token() {
        assert_eq!(estimate_text("abcd"), 1);
        assert_eq!(estimate_text("abcde"), 2);
        assert_eq!(estimate_text("abcdefgh"), 2);
    }

    #[test]
    fn cjk_is_about_one_token_per_char() {
        assert_eq!(estimate_text("你好世界"), 4);
    }

    #[test]
    fn mixed_text_sums_both_buckets() {
        // 4 个 ASCII（1 token）+ 2 个汉字（2 token）
        assert_eq!(estimate_text("hi你好"), 1 + 2);
    }

    #[test]
    fn messages_add_per_message_overhead() {
        let messages = vec![msg(ChatRole::User, "abcd")];
        assert_eq!(estimate_tokens(&messages, &[]), PER_MESSAGE_OVERHEAD + 1);
    }

    #[test]
    fn tool_specs_count_name_description_and_schema() {
        let tool = ToolSpec {
            name: "read_file".to_string(),
            description: Some("读取文件".to_string()),
            parameters: serde_json::json!({ "type": "object" }),
        };
        let with_tool = estimate_tokens(&[], std::slice::from_ref(&tool));
        assert!(with_tool > 0, "工具声明必须计入");
        // tool_choice 不参与估算，这里只是顺带确认枚举可用（避免未使用告警）。
        let _ = ToolChoice::Auto;
    }
}
