//! `Content-Length` 分帧：LSP 的字节边界在这一层。
//!
//! LSP 的 stdio 传输不是「一行一帧」，而是：
//!
//! ```text
//! Content-Length: 73\r\n
//! Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n   （可选）
//! \r\n
//! {"jsonrpc":"2.0","id":1,"method":"initialize",...}
//! ```
//!
//! 两个字节层面的坑必须自己兜住：
//!
//! 1. **长度是字节数，不是字符数**。消息体是 UTF-8，一行中文的 `chars().count()`
//!    与 `len()` 差三倍；写头时用错就会把下一帧的字节吞进本帧。
//! 2. **头必须设上限**。服务器写坏一个头（比如少写空行）时，若无限读下去，
//!    客户端会一直吃到内存耗尽；消息体同理（LSP 规范默认建议 8 MiB 上限）。
//!
//! 所以 [`FrameReader::read_frame`] 的契约是：要么返回一段完整消息体，
//! 要么返回 [`LspError::Protocol`]——**绝不在帧边界不明的情况下继续读**。

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};

use crate::error::LspError;

/// 帧头默认上限：正常头只有几十字节，给 8 KiB 已经非常宽松。
pub const DEFAULT_MAX_HEADER_BYTES: usize = 8 * 1024;

/// 消息体默认上限：16 MiB。超过这个量级的单帧基本可判定为服务器异常。
pub const DEFAULT_MAX_CONTENT_BYTES: usize = 16 * 1024 * 1024;

/// 分帧的字节上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportLimits {
    /// 单帧头（含终止空行）的字节上限。
    pub max_header_bytes: usize,
    /// 单帧消息体的字节上限。
    pub max_content_bytes: usize,
}

impl Default for TransportLimits {
    fn default() -> Self {
        Self {
            max_header_bytes: DEFAULT_MAX_HEADER_BYTES,
            max_content_bytes: DEFAULT_MAX_CONTENT_BYTES,
        }
    }
}

impl TransportLimits {
    /// 自定义上限（测试与「超大文件整篇同步」这类特殊场景用）。
    pub fn new(max_header_bytes: usize, max_content_bytes: usize) -> Self {
        Self {
            max_header_bytes,
            max_content_bytes,
        }
    }
}

/// 把一段消息体编成完整帧（头 + 空行 + 原样字节）。
pub fn encode_frame(body: &str) -> Vec<u8> {
    // 用 byte 长度，不是字符长度。
    let mut frame = Vec::with_capacity(body.len() + 32);
    frame.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    frame.extend_from_slice(body.as_bytes());
    frame
}

/// 从帧头块里取出 `Content-Length`。
///
/// 容错点（都来自真实服务器的观测）：头名大小写混用、行尾只有 `\n`、
/// 头部字段顺序不定、多余的 `Content-Type`。
pub fn parse_content_length(header_block: &str) -> Result<usize, LspError> {
    for line in header_block.split('\n') {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(LspError::protocol(format!("帧头行缺少冒号：{line:?}")));
        };
        if !name.trim().eq_ignore_ascii_case("content-length") {
            continue;
        }
        let value = value.trim();
        return value
            .parse::<usize>()
            .map_err(|_| LspError::protocol(format!("Content-Length 不是合法数字：{value:?}")));
    }
    Err(LspError::protocol(
        "帧头缺少 Content-Length（无法确定帧边界）",
    ))
}

/// 按 `Content-Length` 一条条读消息体。
pub struct FrameReader<R> {
    inner: BufReader<R>,
    limits: TransportLimits,
    server: String,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    /// 包装一个字节流。
    pub fn new(reader: R, limits: TransportLimits, server: impl Into<String>) -> Self {
        Self {
            inner: BufReader::new(reader),
            limits,
            server: server.into(),
        }
    }

    /// 读下一帧的消息体。
    ///
    /// - `Ok(Some(body))`：拿到完整一帧；
    /// - `Ok(None)`：流在**帧边界**上正常结束（子进程关掉了 stdout）；
    /// - `Err(Protocol)`：头缺失/非法/超限、消息体被截断——帧边界已经不可信；
    /// - `Err(Transport)`：底层读失败（管道断了）。
    pub async fn read_frame(&mut self) -> Result<Option<Vec<u8>>, LspError> {
        let mut header = Vec::new();
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = self
                .inner
                .read_until(b'\n', &mut line)
                .await
                .map_err(|err| {
                    LspError::transport(&self.server, format!("读取 stdout 失败：{err}"))
                })?;
            if read == 0 {
                if header.is_empty() {
                    // 干净 EOF：上一帧正好读完，子进程收工。
                    return Ok(None);
                }
                return Err(LspError::protocol(format!(
                    "语言服务器 `{}` 的帧头未结束就遇到 EOF",
                    self.server
                )));
            }
            header.extend_from_slice(&line);
            if header.len() > self.limits.max_header_bytes {
                return Err(LspError::protocol(format!(
                    "帧头超过 {} 字节上限，无法确定帧边界",
                    self.limits.max_header_bytes
                )));
            }
            // 空行 = 头结束（兼容 CRLF 与 LF）。
            if line == b"\r\n" || line == b"\n" {
                break;
            }
        }

        let block = std::str::from_utf8(&header)
            .map_err(|err| LspError::protocol(format!("帧头不是合法 UTF-8：{err}")))?;
        let length = parse_content_length(block)?;
        if length > self.limits.max_content_bytes {
            return Err(LspError::protocol(format!(
                "消息体长度 {length} 超过 {} 字节上限",
                self.limits.max_content_bytes
            )));
        }

        let mut body = vec![0u8; length];
        self.inner.read_exact(&mut body).await.map_err(|err| {
            if err.kind() == std::io::ErrorKind::UnexpectedEof {
                LspError::protocol(format!(
                    "消息体在 {length} 字节中的第 {} 字节前被截断",
                    body.len()
                ))
            } else {
                LspError::transport(&self.server, format!("读取消息体失败：{err}"))
            }
        })?;
        Ok(Some(body))
    }
}

impl<R> std::fmt::Debug for FrameReader<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameReader")
            .field("server", &self.server)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(bytes: &[u8]) -> Vec<Result<Option<Vec<u8>>, LspError>> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        runtime.block_on(async move {
            let mut reader = FrameReader::new(bytes, TransportLimits::default(), "test");
            let mut out = Vec::new();
            loop {
                let item = reader.read_frame().await;
                let stop = !matches!(item, Ok(Some(_)));
                out.push(item);
                if stop {
                    return out;
                }
            }
        })
    }

    #[test]
    fn encode_uses_byte_length_for_multibyte_bodies() {
        let body = r#"{"text":"你好"}"#;
        let frame = encode_frame(body);
        let header = String::from_utf8(frame.clone()).expect("utf8");
        let length = parse_content_length(&header).expect("content-length");
        assert_eq!(length, body.len());
        assert!(length > body.chars().count(), "字节数应大于字符数");
        assert!(frame.ends_with(body.as_bytes()));
    }

    #[test]
    fn roundtrip_reads_consecutive_frames() {
        let mut bytes = encode_frame(r#"{"jsonrpc":"2.0","id":1,"result":null}"#);
        bytes.extend_from_slice(&encode_frame(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#));

        let frames = read_all(&bytes);
        assert_eq!(frames.len(), 3, "{frames:?}");
        assert_eq!(
            String::from_utf8(frames[0].as_ref().expect("ok").clone().expect("frame")).unwrap(),
            r#"{"jsonrpc":"2.0","id":1,"result":null}"#
        );
        let second = r#"{"jsonrpc":"2.0","id":2,"result":{}}"#;
        assert_eq!(
            frames[1].as_ref().expect("ok").as_ref().map(Vec::len),
            Some(second.len())
        );
        // 终点必须是「干净 EOF」，让读循环能区分「收工」和「头读到一半断了」。
        assert!(matches!(frames[2], Ok(None)), "{:?}", frames[2]);
    }

    #[test]
    fn extra_headers_and_lf_only_line_endings_are_tolerated() {
        let body = r#"{"jsonrpc":"2.0","id":3,"result":true}"#;
        let raw = format!(
            "content-TYPE: application/vscode-jsonrpc; charset=utf-8\ncontent-length: {}\n\n{}",
            body.len(),
            body
        );
        let frames = read_all(raw.as_bytes());
        let first = frames[0].as_ref().expect("ok").clone().expect("frame");
        assert_eq!(String::from_utf8(first).unwrap(), body);
    }

    #[test]
    fn missing_content_length_is_a_protocol_error() {
        let frames = read_all(b"Content-Type: application/json\r\n\r\n{}");
        match &frames[0] {
            Err(err @ LspError::Protocol(_)) => {
                assert!(err.to_string().contains("Content-Length"), "{err}");
            }
            other => panic!("期望协议错误，得到 {other:?}"),
        }
    }

    #[test]
    fn non_numeric_content_length_is_a_protocol_error() {
        let frames = read_all(b"Content-Length: not-a-number\r\n\r\n{}");
        assert!(
            matches!(frames[0], Err(LspError::Protocol(_))),
            "{:?}",
            frames[0]
        );
    }

    #[test]
    fn oversized_body_is_rejected_before_allocating() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        let outcome = runtime.block_on(async {
            let mut reader = FrameReader::new(
                &b"Content-Length: 4096\r\n\r\nxx"[..],
                TransportLimits::new(1024, 16),
                "test",
            );
            reader.read_frame().await
        });
        match outcome {
            Err(err @ LspError::Protocol(_)) => {
                assert!(err.to_string().contains("上限"), "{err}");
            }
            other => panic!("期望协议错误，得到 {other:?}"),
        }
    }

    #[test]
    fn oversized_header_is_rejected() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        let outcome = runtime.block_on(async {
            let mut reader = FrameReader::new(
                &b"X-Pad: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\nContent-Length: 2\r\n\r\n{}"[..],
                TransportLimits::new(16, 1024),
                "test",
            );
            reader.read_frame().await
        });
        match outcome {
            Err(err @ LspError::Protocol(_)) => {
                assert!(err.to_string().contains("帧头"), "{err}");
            }
            other => panic!("期望协议错误，得到 {other:?}"),
        }
    }

    #[test]
    fn truncated_body_is_a_protocol_error() {
        let frames = read_all(b"Content-Length: 100\r\n\r\n{\"jsonrpc\":");
        assert!(
            matches!(frames[0], Err(LspError::Protocol(_))),
            "{:?}",
            frames[0]
        );
    }

    #[test]
    fn unterminated_header_is_a_protocol_error() {
        let frames = read_all(b"Content-Length: 2\r\n");
        assert!(
            matches!(frames[0], Err(LspError::Protocol(_))),
            "{:?}",
            frames[0]
        );
    }

    #[test]
    fn zero_length_body_is_a_valid_frame() {
        let frames = read_all(b"Content-Length: 0\r\n\r\n");
        assert_eq!(frames[0].as_ref().expect("ok"), &Some(Vec::new()));
    }
}
