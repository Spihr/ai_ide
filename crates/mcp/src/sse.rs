//! SSE 分帧。
//!
//! HTTP 传输的响应体可能是 `text/event-stream`：MCP Server 把 JSON-RPC 响应包在
//! `data:` 行里下发。这里只做**分帧**，不解释语义——拿到 `data` 负载后交给
//! [`crate::jsonrpc::decode_value`]。
//!
//! ## 为什么按字节分帧而不是按 `String`
//!
//! `reqwest` 的 chunk 边界与 UTF-8 字符边界无关。先 `String::from_utf8_lossy(chunk)`
//! 再拼字符串，会把跨 chunk 的中文字符劈成两个替换符（`U+FFFD`），而工具返回里
//! 中文/emoji 很常见。这里缓存原始字节，只在**整行凑齐后**才解码，跨 chunk 的
//! 多字节字符自然完整。
//!
//! ## 兼容的现实
//!
//! - `\r\n` 与 `\n` 混用（代理改写常见）；
//! - 多行 `data:` 要按 `\n` 拼接；
//! - `:` 开头是注释，常用于心跳（`: ping`），必须忽略而不是当空帧；
//! - 流结束时最后一条帧可能没有以空行收尾，需要显式 flush。

/// 一条 SSE 帧。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseFrame {
    /// `event:` 字段（MCP 一般发 `message`）。
    pub event: Option<String>,
    /// `data:` 字段，多行按 `\n` 连接。
    pub data: String,
}

/// 增量 SSE 分帧器。
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
    current: Option<SseFrame>,
}

impl SseDecoder {
    /// 新建空分帧器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一段字节，返回本次凑齐的完整帧。
    pub fn push_bytes(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        self.buffer.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
            // 取整行（不含 `\n`）；跨 chunk 的多字节字符到这里一定已经完整。
            let line: Vec<u8> = self.buffer.drain(..=index).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            self.consume_line(&line, &mut frames);
        }
        frames
    }

    /// 流结束时收尾：没有以空行关闭的最后一帧也要交出来。
    pub fn finish(&mut self) -> Option<SseFrame> {
        let mut frames = Vec::new();
        if !self.buffer.is_empty() {
            let rest = std::mem::take(&mut self.buffer);
            self.consume_line(&String::from_utf8_lossy(&rest), &mut frames);
        }
        frames.pop().or_else(|| self.current.take())
    }

    fn consume_line(&mut self, raw: &str, frames: &mut Vec<SseFrame>) {
        let line = raw.trim_end_matches('\r');
        if line.is_empty() {
            if let Some(frame) = self.current.take() {
                frames.push(frame);
            }
            return;
        }
        if line.starts_with(':') {
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
            // `id:` / `retry:` 与 JSON-RPC 关联无关。
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_frame() {
        let mut decoder = SseDecoder::new();
        let frames = decoder.push_bytes(b"event: message\ndata: {\"id\":1}\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event.as_deref(), Some("message"));
        assert_eq!(frames[0].data, "{\"id\":1}");
    }

    #[test]
    fn handles_chunk_boundaries_and_crlf() {
        let mut decoder = SseDecoder::new();
        assert!(decoder.push_bytes(b"event: mess").is_empty());
        assert!(decoder.push_bytes(b"age\r\ndata: {\"a\":").is_empty());
        let frames = decoder.push_bytes(b"1}\r\n\r\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "{\"a\":1}");
    }

    #[test]
    fn joins_multiline_data() {
        let mut decoder = SseDecoder::new();
        let frames = decoder.push_bytes(b"data: line1\ndata: line2\n\n");
        assert_eq!(frames[0].data, "line1\nline2");
    }

    #[test]
    fn ignores_comments() {
        let mut decoder = SseDecoder::new();
        let frames = decoder.push_bytes(b": ping\ndata: {\"ok\":true}\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "{\"ok\":true}");
    }

    #[test]
    fn keeps_multibyte_characters_across_chunks() {
        let text = "data: 你好世界\n\n".as_bytes();
        // 故意在「你」的中间（3 字节字符的第 2 个字节处）截断。
        let split = 8;
        let mut decoder = SseDecoder::new();
        assert!(decoder.push_bytes(&text[..split]).is_empty());
        let frames = decoder.push_bytes(&text[split..]);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "你好世界");
    }

    #[test]
    fn finish_returns_trailing_frame_without_blank_line() {
        let mut decoder = SseDecoder::new();
        assert!(decoder.push_bytes(b"data: {\"id\":1}").is_empty());
        let frame = decoder.finish().expect("frame");
        assert_eq!(frame.data, "{\"id\":1}");
    }

    #[test]
    fn finish_after_terminated_line_is_none() {
        let mut decoder = SseDecoder::new();
        let frames = decoder.push_bytes(b"data: x\n\n");
        assert_eq!(frames.len(), 1);
        assert!(decoder.finish().is_none());
    }
}
