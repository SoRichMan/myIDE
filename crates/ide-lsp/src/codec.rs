//! LSP JSON-RPC 帧编解码：`Content-Length` 头 + UTF-8 JSON 体。
//!
//! 帧格式（LSP 基础协议）：
//! ```text
//! Content-Length: <字节数>\r\n
//! \r\n
//! <JSON body，恰好 <字节数> 个 UTF-8 字节>
//! ```
//!
//! 注意：`Content-Length` 是 **字节** 数，不是字符数；多字节 UTF-8（如中文注释）
//! 必须按 `str::len()`（字节）计算。
//!
//! 解码器是增量式的：喂入任意长度的字节块，返回其中完整的所有帧；
//! 不完整的头部/体保留在内部缓冲，等下一次 `push`。天然支持分包/粘包。

/// 帧头与体之间的分隔符。
const HEADER_TERM_CRLF: &[u8] = b"\r\n\r\n";
/// 宽容模式：部分服务端只用 LF 结尾。
const HEADER_TERM_LF: &[u8] = b"\n\n";

/// 编解码错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    /// 头部已终止但缺少 `Content-Length` 行。
    MissingLength,
    /// `Content-Length` 值不是合法非负整数。
    InvalidLength(String),
    /// 体不是合法 UTF-8。
    InvalidUtf8,
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecError::MissingLength => write!(f, "header terminated without Content-Length"),
            CodecError::InvalidLength(v) => write!(f, "invalid Content-Length value: {v:?}"),
            CodecError::InvalidUtf8 => write!(f, "frame body is not valid UTF-8"),
        }
    }
}

impl std::error::Error for CodecError {}

/// 把一条 JSON body 编码成完整帧。
///
/// `body` 必须是合法的 JSON 文本；长度按字节（`body.len()`）计入头部。
pub fn encode(body: &str) -> Vec<u8> {
    let len = body.len();
    let mut out = Vec::with_capacity(len + 64);
    out.extend_from_slice(b"Content-Length: ");
    out.extend_from_slice(len.to_string().as_bytes());
    out.extend_from_slice(HEADER_TERM_CRLF);
    out.extend_from_slice(body.as_bytes());
    out
}

/// 增量帧解码器。
///
/// 用法：
/// ```no_run
/// use ide_lsp::codec::FrameDecoder;
///
/// fn feed(dec: &mut FrameDecoder, data: &[u8]) -> Result<Vec<String>, ide_lsp::codec::CodecError> {
///     dec.push(data) // 可能为空（数据不完整），凑齐后返回完整帧
/// }
/// # let mut dec = FrameDecoder::new();
/// # let _ = feed(&mut dec, b"Content-Length: 2\r\n\r\nab");
/// ```
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前缓冲的字节数（尚未消费的部分）。
    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }

    /// 喂入字节流，返回解析出的完整帧（JSON body 字符串，按到达顺序）。
    ///
    /// 输入可以是任意切分：一帧拆成多次 push、多帧拼在一次 push 均可。
    pub fn push(&mut self, data: &[u8]) -> Result<Vec<String>, CodecError> {
        self.buf.extend_from_slice(data);
        let mut frames = Vec::new();
        while let Some((header_end, term_len)) = find_header_term(&self.buf) {
            let header = &self.buf[..header_end];
            let Some(len) = parse_content_length(header)? else {
                // 有终止符但没有 Content-Length：协议错误。
                // 重同步：丢弃到下一个 Content-Length 行之前（容忍垃圾字节）；
                // 找不到则丢弃该头部并报错。
                match find_next_content_length(&self.buf) {
                    Some(pos) => {
                        self.buf.drain(..pos);
                        continue;
                    }
                    None => {
                        self.buf.drain(..header_end + term_len);
                        return Err(CodecError::MissingLength);
                    }
                }
            };
            let body_start = header_end + term_len;
            if self.buf.len() < body_start + len {
                break; // 体还没到齐
            }
            let body = &self.buf[body_start..body_start + len];
            let text = std::str::from_utf8(body).map_err(|_| CodecError::InvalidUtf8)?;
            frames.push(text.to_string());
            self.buf.drain(..body_start + len);
        }
        Ok(frames)
    }
}

/// 找到头部终止符，返回 `(header 内容长度, 终止符长度)`，支持 `\r\n\r\n` 与 `\n\n`。
fn find_header_term(buf: &[u8]) -> Option<(usize, usize)> {
    if buf.len() >= HEADER_TERM_CRLF.len() {
        if let Some(pos) = find_sub(buf, HEADER_TERM_CRLF) {
            return Some((pos, HEADER_TERM_CRLF.len()));
        }
    }
    if buf.len() >= HEADER_TERM_LF.len() {
        if let Some(pos) = find_sub(buf, HEADER_TERM_LF) {
            return Some((pos, HEADER_TERM_LF.len()));
        }
    }
    None
}

fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

/// 从头部字节中解析 `Content-Length`。
///
/// 头部按行拆分，行键大小写不敏感（`Content-Type` 等其它行忽略）。
/// 返回 `Ok(None)` 表示头部完整但没有 Content-Length 行。
fn parse_content_length(header: &[u8]) -> Result<Option<usize>, CodecError> {
    let text = std::str::from_utf8(header).map_err(|_| CodecError::InvalidLength(hex_preview(header)))?;
    for raw_line in text.split('\n') {
        let line = raw_line.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue; // 非 key: value 行，忽略
        };
        if key.trim().eq_ignore_ascii_case("content-length") {
            let v = value.trim();
            if v.is_empty() {
                return Err(CodecError::InvalidLength(v.to_string()));
            }
            match v.parse::<usize>() {
                Ok(n) => return Ok(Some(n)),
                Err(_) => return Err(CodecError::InvalidLength(v.to_string())),
            }
        }
    }
    Ok(None)
}

fn hex_preview(bytes: &[u8]) -> String {
    let n = bytes.len().min(16);
    bytes[..n]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 在缓冲中查找下一个 `Content-Length:`（大小写不敏感），返回其字节位置。
/// 用于头部损坏后的重同步。
fn find_next_content_length(buf: &[u8]) -> Option<usize> {
    let needle = b"content-length:";
    let lower: Vec<u8> = buf.iter().map(|b| b.to_ascii_lowercase()).collect();
    lower
        .windows(needle.len())
        .position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_ascii_body_header_len_is_byte_len() {
        let frame = encode(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        let text = String::from_utf8(frame).unwrap();
        let (header, body) = text.split_once("\r\n\r\n").unwrap();
        assert_eq!(header, "Content-Length: 40");
        assert_eq!(body, r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
    }

    #[test]
    fn encode_utf8_body_len_counts_bytes_not_chars() {
        // 中文注释：字符数 11，字节数 11*3 + ascii。
        let json = r#"{"text":"// 中文注释 hello"}"#;
        let frame = encode(json);
        let text = String::from_utf8(frame).unwrap();
        let (header, body) = text.split_once("\r\n\r\n").unwrap();
        let declared: usize = header
            .strip_prefix("Content-Length: ")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(declared, json.len());
        assert_eq!(declared, body.len());
        assert!(declared > json.chars().count(), "byte len must exceed char count");
    }

    #[test]
    fn decode_single_frame() {
        let mut dec = FrameDecoder::new();
        let frames = dec.push(&encode(r#"{"jsonrpc":"2.0","method":"initialized"}"#)).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], r#"{"jsonrpc":"2.0","method":"initialized"}"#);
        assert_eq!(dec.buffered_len(), 0);
    }

    #[test]
    fn decode_fragmented_byte_by_byte() {
        let frame = encode(r#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#);
        let mut dec = FrameDecoder::new();
        let mut collected = Vec::new();
        for b in frame {
            let frames = dec.push(&[b]).unwrap();
            collected.extend(frames);
        }
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0], r#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#);
    }

    #[test]
    fn decode_fragmented_at_header_and_body_boundaries() {
        let mut dec = FrameDecoder::new();
        let frame = encode(r#"{"a":1}"#);
        // 头尾各留 1 字节，中间任意切
        let mid = frame.len() / 2;
        let frames1 = dec.push(&frame[..mid]).unwrap();
        assert!(frames1.is_empty());
        let frames2 = dec.push(&frame[mid..]).unwrap();
        assert_eq!(frames2, vec![r#"{"a":1}"#.to_string()]);
    }

    #[test]
    fn decode_coalesced_multiple_frames() {
        let f1 = encode(r#"{"jsonrpc":"2.0","id":1}"#);
        let f2 = encode(r#"{"jsonrpc":"2.0","id":2}"#);
        let mut both = f1.clone();
        both.extend_from_slice(&f2);
        let mut dec = FrameDecoder::new();
        let frames = dec.push(&both).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], r#"{"jsonrpc":"2.0","id":1}"#);
        assert_eq!(frames[1], r#"{"jsonrpc":"2.0","id":2}"#);
    }

    #[test]
    fn decode_coalesced_then_partial() {
        let f1 = encode(r#"{"id":1}"#);
        let mut dec = FrameDecoder::new();
        // 两帧：第一帧完整，第二帧只给一半
        let mut data = f1.clone();
        let f2 = encode(r#"{"id":2}"#);
        data.extend_from_slice(&f2[..f2.len() - 5]);
        let frames = dec.push(&data).unwrap();
        assert_eq!(frames, vec![r#"{"id":1}"#.to_string()]);
        // 补全第二帧
        let frames = dec.push(&f2[f2.len() - 5..]).unwrap();
        assert_eq!(frames, vec![r#"{"id":2}"#.to_string()]);
        assert_eq!(dec.buffered_len(), 0);
    }

    #[test]
    fn decode_zero_length_body() {
        let mut dec = FrameDecoder::new();
        let frames = dec.push(b"Content-Length: 0\r\n\r\n").unwrap();
        assert_eq!(frames, vec![String::new()]);
    }

    #[test]
    fn decode_header_with_content_type_and_extra_lines() {
        let raw = b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: 5\r\n\r\nhello";
        let mut dec = FrameDecoder::new();
        let frames = dec.push(raw).unwrap();
        assert_eq!(frames, vec!["hello".to_string()]);
    }

    #[test]
    fn decode_lf_only_header() {
        let raw = b"Content-Length: 4\n\nabcd";
        let mut dec = FrameDecoder::new();
        let frames = dec.push(raw).unwrap();
        assert_eq!(frames, vec!["abcd".to_string()]);
    }

    #[test]
    fn decode_case_insensitive_header_key() {
        let raw = b"content-length: 3\r\n\r\nxyz";
        let mut dec = FrameDecoder::new();
        let frames = dec.push(raw).unwrap();
        assert_eq!(frames, vec!["xyz".to_string()]);
    }

    #[test]
    fn decode_invalid_utf8_body_is_error() {
        let mut dec = FrameDecoder::new();
        let mut raw = Vec::new();
        raw.extend_from_slice(b"Content-Length: 2\r\n\r\n");
        raw.push(0xff);
        raw.push(0xfe);
        let err = dec.push(&raw).unwrap_err();
        assert_eq!(err, CodecError::InvalidUtf8);
    }

    #[test]
    fn decode_missing_length_is_error_then_recovers() {
        let mut dec = FrameDecoder::new();
        let junk = b"X-Foo: 1\r\n\r\n";
        let err = dec.push(junk).unwrap_err();
        assert_eq!(err, CodecError::MissingLength);
        // 错误后应丢弃垃圾头，能从后续正常帧恢复
        let frames = dec.push(&encode(r#"{"ok":true}"#)).unwrap();
        assert_eq!(frames, vec![r#"{"ok":true}"#.to_string()]);
    }

    #[test]
    fn decode_invalid_length_value() {
        let mut dec = FrameDecoder::new();
        let raw = b"Content-Length: abc\r\n\r\n{}";
        let err = dec.push(raw).unwrap_err();
        assert!(matches!(err, CodecError::InvalidLength(_)));
    }

    #[test]
    fn decode_ignores_trailing_garbage_after_frame_until_next_header() {
        let mut dec = FrameDecoder::new();
        let f1 = encode(r#"{"id":9}"#);
        let mut data = f1.clone();
        data.extend_from_slice(b"GARBAGE-NOT-A-FRAME");
        data.extend_from_slice(&encode(r#"{"id":10}"#));
        let frames = dec.push(&data).unwrap();
        assert_eq!(frames, vec![r#"{"id":9}"#.to_string(), r#"{"id":10}"#.to_string()]);
    }

    #[test]
    fn encode_decode_roundtrip_with_utf8_and_emoji() {
        let json = r#"{"note":"阶段2 ✓ 🚀"}"#;
        let frame = encode(json);
        let mut dec = FrameDecoder::new();
        let frames = dec.push(&frame).unwrap();
        assert_eq!(frames, vec![json.to_string()]);
    }
}
