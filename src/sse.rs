//! SSE 解码: 上游字节流 -> (event, data)。
//!
//! 按字节切分事件块, 块内才转 UTF-8 -> 多字节字符跨 chunk 也不会被截断。

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Event {
    pub name: String,
    pub data: String,
}

#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// 取出下一个完整事件; 不足一个事件时返回 None。
    pub fn next_event(&mut self) -> Option<Event> {
        loop {
            let (end, sep) = boundary(&self.buf)?;
            let block: Vec<u8> = self.buf.drain(..end + sep).collect();
            let text = String::from_utf8_lossy(&block[..end]);
            if let Some(ev) = parse_block(&text) {
                return Some(ev);
            }
        }
    }
}

/// 返回 (事件体长度, 分隔符长度)。
fn boundary(buf: &[u8]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for (pat, len) in [(&b"\r\n\r\n"[..], 4), (&b"\n\n"[..], 2)] {
        if let Some(i) = buf.windows(pat.len()).position(|w| w == pat) {
            if best.is_none_or(|(b, _)| i < b) {
                best = Some((i, len));
            }
        }
    }
    best
}

fn parse_block(text: &str) -> Option<Event> {
    let mut ev = Event::default();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => ev.name = value.to_string(),
            "data" => {
                if !ev.data.is_empty() {
                    ev.data.push('\n');
                }
                ev.data.push_str(value);
            }
            _ => {}
        }
    }
    (!ev.name.is_empty() || !ev.data.is_empty()).then_some(ev)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_events_and_keeps_partial_tail() {
        let mut d = Decoder::default();
        d.push(b"event: a\ndata: {\"x\":1}\n\nevent: b\ndata: hal");
        assert_eq!(
            d.next_event().unwrap(),
            Event {
                name: "a".into(),
                data: "{\"x\":1}".into()
            }
        );
        assert!(d.next_event().is_none());
        d.push(b"f\n\n");
        assert_eq!(d.next_event().unwrap().data, "half");
    }

    #[test]
    fn handles_crlf_comments_and_multiline_data() {
        let mut d = Decoder::default();
        d.push(b": keep-alive\r\n\r\nevent: x\r\ndata: one\r\ndata: two\r\n\r\n");
        let ev = d.next_event().unwrap();
        assert_eq!(ev.name, "x");
        assert_eq!(ev.data, "one\ntwo");
    }

    #[test]
    fn utf8_split_across_chunks_is_intact() {
        let mut d = Decoder::default();
        let payload = "data: 中文\n\n".as_bytes();
        d.push(&payload[..8]);
        assert!(d.next_event().is_none());
        d.push(&payload[8..]);
        assert_eq!(d.next_event().unwrap().data, "中文");
    }
}
