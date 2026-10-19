//! SSE 解析与 OpenAI 流式分片转译。
//!
//! 两层职责分开：
//!
//! 1. [`SseDecoder`] 只做**分帧**——把字节流还原成 `data:` / `event:` 字段，
//!    处理跨 chunk 截断、`\r\n` 与 `\n` 混用、多行 `data:`、`:` 心跳注释；
//! 2. [`translate_openai_chunk`] 只做**语义**——把一条 `data:` 负载变成 [`StreamEvent`]。
//!
//! 分开之后，单测可以分别覆盖「分帧边界」和「厂商字段差异」，不用互相牵连。

use serde::Deserialize;

use crate::error::LlmError;
use crate::types::{StreamEvent, Usage};
use ai_ide_protocol::dto::FinishReason;

/// 一条 SSE 帧。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseFrame {
    /// `event:` 字段（OpenAI 兼容接口一般不发，Anthropic 会发）。
    pub event: Option<String>,
    /// `data:` 字段，多行按 `\n` 连接。
    pub data: String,
}

/// 增量 SSE 分帧器。
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: String,
    current: Option<SseFrame>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一段文本，返回本次凑齐的完整帧。
    pub fn push(&mut self, chunk: &str) -> Vec<SseFrame> {
        self.buffer.push_str(chunk);
        let mut frames = Vec::new();
        while let Some(index) = self.buffer.find('\n') {
            let line = self.buffer[..index].to_string();
            self.buffer.drain(..=index);
            self.consume_line(&line, &mut frames);
        }
        frames
    }

    /// 流结束时收尾：上游没以空行结尾时补出最后一帧。
    ///
    /// 「没以空行结尾」有两种形态，都必须补出：
    /// 1. 最后一行**连换行符都没有**（流恰好在 `}` 之后断掉）——它还在 `buffer` 里、
    ///    从未进过解析器，必须先按整行喂一遍，否则这一帧会被静默丢掉；
    ///    OpenAI 兼容网关在流被代理截断时就是这种形态，丢掉的往往是 `[DONE]`
    ///    或带 `finish_reason` 的那一帧，调用方会误判成「上游提前中断」。
    /// 2. 最后一行有换行符、但没有跟空行——它已在 [`Self::push`] 阶段进了 `current`，
    ///    直接取出即可。
    pub fn finish(&mut self) -> Option<SseFrame> {
        let mut frames = Vec::new();
        if !self.buffer.is_empty() {
            let rest = std::mem::take(&mut self.buffer);
            self.consume_line(&rest, &mut frames);
        }
        frames.pop().or_else(|| self.current.take())
    }

    /// 消费一行原始文本（行尾 `\n` 已剥离，可能残留 `\r`）。
    ///
    /// 只有在遇到空行时才把 `current` 关帧推入 `frames`——这是 SSE 的分帧规则，
    /// [`Self::finish`] 复用同一套语义来补出流末尾没被空行关掉的那一帧。
    fn consume_line(&mut self, raw: &str, frames: &mut Vec<SseFrame>) {
        let line = raw.trim_end_matches('\r');
        if line.is_empty() {
            if let Some(frame) = self.current.take() {
                frames.push(frame);
            }
            return;
        }
        if line.starts_with(':') {
            // 注释行（常见于心跳 `: ping`），忽略。
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        let frame = self.current.get_or_insert_with(SseFrame::default);
        match field {
            "event" => frame.event = Some(value.to_string()),
            "data" => {
                if !frame.data.is_empty() {
                    frame.data.push('\n');
                }
                frame.data.push_str(value);
            }
            // `id:` / `retry:` 与对话内容无关，M2 不需要。
            _ => {}
        }
    }
}

/// 一次 `data:` 负载的转译结果。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TranslatedChunk {
    /// 本条负载产生的事件（可能为空，例如纯 usage 帧之外的元信息帧）。
    pub events: Vec<StreamEvent>,
    /// 上游响应 id。
    pub message_id: Option<String>,
    /// 本条负载给出的结束原因。
    pub finish_reason: Option<FinishReason>,
    /// 是否为 `[DONE]` 结束标记。
    pub done: bool,
}

/// 把 OpenAI Chat Completions 的一条 `data:` 负载转成事件。
///
/// 兼容要点：`reasoning_content`（DeepSeek 系）与 `reasoning`（部分网关）同义；
/// 工具调用以「index + 参数片段」增量下发；`[DONE]` 是显式终止标记。
pub fn translate_openai_chunk(data: &str) -> Result<TranslatedChunk, LlmError> {
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return Ok(TranslatedChunk::default());
    }
    if trimmed == "[DONE]" {
        return Ok(TranslatedChunk {
            done: true,
            ..TranslatedChunk::default()
        });
    }

    let envelope: ChunkEnvelope = serde_json::from_str(trimmed).map_err(|err| {
        LlmError::BadResponse(format!("分片不是合法 JSON: {err}; 原文: {trimmed}"))
    })?;

    let mut out = TranslatedChunk {
        message_id: envelope.id.clone(),
        ..TranslatedChunk::default()
    };

    if let Some(usage) = envelope.usage {
        out.events.push(StreamEvent::Usage(Usage {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            cached_tokens: usage
                .prompt_tokens_details
                .and_then(|details| details.cached_tokens),
        }));
    }

    for choice in envelope.choices {
        if let Some(text) = choice
            .delta
            .content
            .as_ref()
            .filter(|text| !text.is_empty())
        {
            out.events.push(StreamEvent::Delta { text: text.clone() });
        }
        if let Some(reasoning) = choice
            .delta
            .reasoning_content
            .as_ref()
            .filter(|text| !text.is_empty())
        {
            out.events.push(StreamEvent::Reasoning {
                text: reasoning.clone(),
            });
        }
        for call in choice.delta.tool_calls {
            let (name, arguments_delta) = match call.function {
                Some(function) => (function.name, function.arguments.unwrap_or_default()),
                None => (None, String::new()),
            };
            out.events.push(StreamEvent::ToolCall {
                index: call.index,
                id: call.id,
                name,
                arguments_delta,
            });
        }
        if let Some(raw) = choice.finish_reason {
            out.finish_reason = Some(map_finish_reason(&raw));
        }
    }

    Ok(out)
}

/// 上游结束原因 → 协议枚举。未知值不丢弃，落到 `Unknown`（原文保留在日志层）。
pub fn map_finish_reason(raw: &str) -> FinishReason {
    match raw {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "tool_calls" | "function_call" => FinishReason::ToolCalls,
        "content_filter" => FinishReason::ContentFilter,
        _ => FinishReason::Unknown,
    }
}

#[derive(Debug, Deserialize)]
struct ChunkEnvelope {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<UsagePayload>,
}

#[derive(Debug, Deserialize)]
struct ChunkChoice {
    #[serde(default)]
    delta: DeltaPayload,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct DeltaPayload {
    #[serde(default)]
    content: Option<String>,
    #[serde(default, alias = "reasoning")]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallDelta>,
}

#[derive(Debug, Deserialize)]
struct ToolCallDelta {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionDelta>,
}

#[derive(Debug, Default, Deserialize)]
struct FunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsagePayload {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_split_on_blank_line() {
        let mut decoder = SseDecoder::new();
        let frames = decoder.push("data: {\"a\":1}\n\ndata: [DONE]\n\n");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].data, "{\"a\":1}");
        assert_eq!(frames[1].data, "[DONE]");
    }

    #[test]
    fn frames_survive_split_across_chunks() {
        let mut decoder = SseDecoder::new();
        assert!(
            decoder.push("data: {\"a\"").is_empty(),
            "没凑齐空行前不该出帧"
        );
        let frames = decoder.push(":1}\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "{\"a\":1}");
    }

    #[test]
    fn frames_handle_crlf_and_comments_and_multiline_data() {
        let mut decoder = SseDecoder::new();
        let frames =
            decoder.push(": ping\r\n\r\nevent: message\r\ndata: line1\r\ndata: line2\r\n\r\n");
        assert_eq!(frames.len(), 1, "心跳注释不该产出帧");
        assert_eq!(frames[0].event.as_deref(), Some("message"));
        assert_eq!(frames[0].data, "line1\nline2");
    }

    #[test]
    fn finish_returns_trailing_frame_without_blank_line() {
        let mut decoder = SseDecoder::new();
        assert!(decoder.push("data: [DONE]").is_empty());
        let trailing = decoder.finish().expect("残留帧应被补出");
        assert_eq!(trailing.data, "[DONE]");
        assert!(decoder.finish().is_none(), "收尾后不应重复出帧");
    }

    #[test]
    fn finish_flushes_frame_that_ended_with_newline_but_no_blank_line() {
        // 第二种形态：最后一行带换行，但整段流没有以空行收尾。
        // 它已在 push 里进了 current，finish 必须原样取出。
        let mut decoder = SseDecoder::new();
        assert!(decoder.push("data: {\"a\":1}\n").is_empty());
        let trailing = decoder.finish().expect("未闭合的帧应被补出");
        assert_eq!(trailing.data, "{\"a\":1}");
        assert!(decoder.finish().is_none());
    }

    #[test]
    fn finish_keeps_event_and_multiline_fields_of_trailing_frame() {
        let mut decoder = SseDecoder::new();
        decoder.push("event: message\ndata: line1\ndata: line2");
        let trailing = decoder.finish().expect("多行 data 的残留帧应被补出");
        assert_eq!(trailing.event.as_deref(), Some("message"));
        assert_eq!(trailing.data, "line1\nline2");
    }

    #[test]
    fn finish_does_not_invent_frames_from_comments_or_blank_tail() {
        // 只有心跳注释残留时不该凭空造帧；流已正常收尾时 finish 也应为 None。
        let mut quiet = SseDecoder::new();
        quiet.push(": ping");
        assert!(quiet.finish().is_none(), "注释行不该产出帧");

        let mut done = SseDecoder::new();
        assert_eq!(done.push("data: [DONE]\n\n").len(), 1);
        assert!(done.finish().is_none(), "已闭合的帧不该被重复吐出");
    }

    #[test]
    fn translates_content_delta() {
        let translated = translate_openai_chunk(
            r#"{"id":"chatcmpl-1","choices":[{"delta":{"content":"你好"}}]}"#,
        )
        .unwrap();
        assert_eq!(translated.message_id.as_deref(), Some("chatcmpl-1"));
        assert_eq!(
            translated.events,
            vec![StreamEvent::Delta {
                text: "你好".into()
            }]
        );
    }

    #[test]
    fn translates_reasoning_delta_from_both_field_names() {
        for payload in [
            r#"{"choices":[{"delta":{"reasoning_content":"思考中"}}]}"#,
            r#"{"choices":[{"delta":{"reasoning":"思考中"}}]}"#,
        ] {
            let translated = translate_openai_chunk(payload).unwrap();
            assert_eq!(
                translated.events,
                vec![StreamEvent::Reasoning {
                    text: "思考中".into()
                }],
                "负载: {payload}"
            );
        }
    }

    #[test]
    fn translates_tool_call_fragments() {
        let first = translate_openai_chunk(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}"#,
        )
        .unwrap();
        assert_eq!(
            first.events,
            vec![StreamEvent::ToolCall {
                index: 0,
                id: Some("call_1".into()),
                name: Some("read_file".into()),
                arguments_delta: "{\"pa".into(),
            }]
        );

        let second = translate_openai_chunk(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a.ts\"}"}}]}}]}"#,
        )
        .unwrap();
        assert_eq!(
            second.events,
            vec![StreamEvent::ToolCall {
                index: 0,
                id: None,
                name: None,
                arguments_delta: "th\":\"a.ts\"}".into(),
            }]
        );
    }

    #[test]
    fn translates_usage_with_cache_details() {
        let translated = translate_openai_chunk(
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":7}}}"#,
        )
        .unwrap();
        assert_eq!(
            translated.events,
            vec![StreamEvent::Usage(Usage {
                prompt_tokens: 10,
                completion_tokens: 3,
                cached_tokens: Some(7),
            })]
        );
    }

    #[test]
    fn translates_finish_reason_and_done() {
        let translated =
            translate_openai_chunk(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#)
                .unwrap();
        assert_eq!(translated.finish_reason, Some(FinishReason::ToolCalls));

        let done = translate_openai_chunk(" [DONE] ").unwrap();
        assert!(done.done);
        assert!(done.events.is_empty());
    }

    #[test]
    fn unknown_finish_reason_is_not_dropped() {
        assert_eq!(map_finish_reason("stop"), FinishReason::Stop);
        assert_eq!(map_finish_reason("length"), FinishReason::Length);
        assert_eq!(map_finish_reason("function_call"), FinishReason::ToolCalls);
        assert_eq!(
            map_finish_reason("content_filter"),
            FinishReason::ContentFilter
        );
        assert_eq!(map_finish_reason("something_new"), FinishReason::Unknown);
    }

    #[test]
    fn invalid_json_is_a_bad_response_error() {
        let err = translate_openai_chunk("{not json").unwrap_err();
        assert!(
            matches!(err, LlmError::BadResponse(_)),
            "非法分片应归类为响应解析错误: {err}"
        );
    }
}
