//! Incremental Server-Sent Events parser (WHATWG "event stream" format).
//!
//! Handles `\n`, `\r\n` and bare `\r` line endings (even when a `\r\n` pair is split across
//! network chunks), multi-line `data:`, `:comment` lines, `event:` and `id:` fields, a UTF-8
//! BOM, and multi-byte characters split across chunks (bytes are buffered until a line ends).

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SseEvent {
    /// `event:` field, if any (`None` = the default "message" type).
    pub event: Option<String>,
    /// `data:` lines joined with `\n`.
    pub data: String,
    pub id: Option<String>,
}

#[derive(Default)]
pub(crate) struct SseParser {
    line: Vec<u8>,
    /// Previous byte was `\r`, so a following `\n` belongs to the same line break.
    after_cr: bool,
    first_line: bool,
    event: Option<String>,
    id: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self {
            first_line: true,
            ..Self::default()
        }
    }

    /// Feeds raw bytes; returns every event completed by them.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        let mut out = Vec::new();
        for &b in chunk {
            match b {
                b'\n' if self.after_cr => {
                    self.after_cr = false;
                }
                b'\n' | b'\r' => {
                    self.after_cr = b == b'\r';
                    let line = std::mem::take(&mut self.line);
                    self.process_line(&line, &mut out);
                }
                _ => {
                    self.after_cr = false;
                    self.line.push(b);
                }
            }
        }
        out
    }

    /// Flushes at end of stream. A trailing event without its blank line is dispatched
    /// (lenient: some servers close right after the last `data:` line).
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut out = Vec::new();
        if !self.line.is_empty() {
            let line = std::mem::take(&mut self.line);
            self.process_line(&line, &mut out);
        }
        self.dispatch(&mut out);
        out
    }

    fn process_line(&mut self, raw: &[u8], out: &mut Vec<SseEvent>) {
        let text = String::from_utf8_lossy(raw);
        let mut line: &str = &text;
        if self.first_line {
            self.first_line = false;
            line = line.strip_prefix('\u{feff}').unwrap_or(line);
        }
        if line.is_empty() {
            self.dispatch(out);
            return;
        }
        if line.starts_with(':') {
            return; // comment / keep-alive
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            "id" if !value.contains('\0') => self.id = Some(value.to_string()),
            _ => {} // retry and unknown fields are ignored
        }
    }

    fn dispatch(&mut self, out: &mut Vec<SseEvent>) {
        let event = self.event.take();
        let id = self.id.take();
        if self.data.is_empty() {
            return;
        }
        let data = std::mem::take(&mut self.data).join("\n");
        out.push(SseEvent { event, data, id });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut p = SseParser::new();
        let mut v = Vec::new();
        for c in chunks {
            v.extend(p.feed(c));
        }
        v.extend(p.finish());
        v
    }

    fn ev(event: Option<&str>, data: &str) -> SseEvent {
        SseEvent {
            event: event.map(String::from),
            data: data.into(),
            id: None,
        }
    }

    #[test]
    fn lf_crlf_and_cr() {
        let expect = vec![ev(Some("a"), "1"), ev(None, "2")];
        assert_eq!(all(&[b"event: a\ndata: 1\n\ndata: 2\n\n"]), expect);
        assert_eq!(
            all(&[b"event: a\r\ndata: 1\r\n\r\ndata: 2\r\n\r\n"]),
            expect
        );
        assert_eq!(all(&[b"event: a\rdata: 1\r\rdata: 2\r\r"]), expect);
        // Mixed endings.
        assert_eq!(all(&[b"event: a\r\ndata: 1\n\r\ndata: 2\n\n"]), expect);
    }

    #[test]
    fn crlf_split_across_chunks() {
        let e = all(&[b"data: x\r", b"\n\r", b"\n", b"data: y\r\n\r\n"]);
        assert_eq!(e, vec![ev(None, "x"), ev(None, "y")]);
    }

    #[test]
    fn multiline_data_joined_with_newline() {
        let e = all(&[b"data: line1\ndata: line2\ndata:\ndata: line4\n\n"]);
        assert_eq!(e, vec![ev(None, "line1\nline2\n\nline4")]);
    }

    #[test]
    fn comments_and_unknown_fields_ignored() {
        let e = all(&[b": keep-alive\n\nretry: 3000\nfoo: bar\ndata: ok\n: another\n\n"]);
        assert_eq!(e, vec![ev(None, "ok")]);
    }

    #[test]
    fn space_stripping_only_one() {
        let e = all(&[b"data:no-space\n\ndata:  two-spaces\n\ndata\n\n"]);
        assert_eq!(
            e,
            vec![ev(None, "no-space"), ev(None, " two-spaces"), ev(None, "")]
        );
    }

    #[test]
    fn byte_by_byte_and_multibyte_split() {
        let payload = "event: delta\ndata: {\"t\":\"héllo — 日本\"}\n\n".as_bytes();
        let chunks: Vec<&[u8]> = payload.chunks(1).collect();
        let e = all(&chunks);
        assert_eq!(e, vec![ev(Some("delta"), "{\"t\":\"héllo — 日本\"}")]);
    }

    #[test]
    fn event_type_resets_between_events() {
        let e = all(&[b"event: a\ndata: 1\n\ndata: 2\n\n"]);
        assert_eq!(e, vec![ev(Some("a"), "1"), ev(None, "2")]);
    }

    #[test]
    fn empty_event_not_dispatched_and_type_cleared() {
        let e = all(&[b"event: orphan\n\ndata: x\n\n"]);
        assert_eq!(e, vec![ev(None, "x")]);
    }

    #[test]
    fn trailing_event_without_blank_line_is_flushed() {
        let e = all(&[b"data: last"]);
        assert_eq!(e, vec![ev(None, "last")]);
        let e = all(&[b"data: last\n"]);
        assert_eq!(e, vec![ev(None, "last")]);
    }

    #[test]
    fn bom_and_id() {
        let e = all(&[b"\xEF\xBB\xBFid: 7\ndata: x\n\n"]);
        assert_eq!(
            e,
            vec![SseEvent {
                event: None,
                data: "x".into(),
                id: Some("7".into())
            }]
        );
    }

    #[test]
    fn colon_inside_value() {
        let e = all(&[b"data: {\"a\":\"b:c\"}\n\n"]);
        assert_eq!(e, vec![ev(None, "{\"a\":\"b:c\"}")]);
    }
}
