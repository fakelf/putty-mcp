use serde::Serialize;
use std::collections::VecDeque;

const REDACTION_CONTEXT: usize = 16 * 1024;
const REDACTED: &str = "[sensitive line redacted]\n";

/// Raw byte offsets never reset within one session. Missing ranges stay visible.
pub struct OutputRing {
    chunks: VecDeque<(u64, Vec<u8>)>,
    bytes: usize,
    capacity: usize,
    pub end: u64,
}
#[derive(Debug, Serialize)]
pub struct Page {
    pub output: String,
    pub next_cursor: u64,
    pub truncated: bool,
    pub lost_bytes: u64,
    pub missing_ranges: Vec<(u64, u64)>,
    pub encoding: &'static str,
}
impl OutputRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            chunks: VecDeque::new(),
            bytes: 0,
            capacity,
            end: 0,
        }
    }
    pub fn push(&mut self, start: u64, data: &[u8]) -> anyhow::Result<()> {
        anyhow::ensure!(start >= self.end, "non-monotonic output");
        self.end = start
            .checked_add(data.len() as u64)
            .ok_or_else(|| anyhow::anyhow!("sequence overflow"))?;
        let trim = data.len().saturating_sub(self.capacity);
        let data = &data[trim..];
        while self.bytes + data.len() > self.capacity || self.chunks.len() >= 1024 {
            if let Some((_, old)) = self.chunks.pop_front() {
                self.bytes -= old.len();
            } else {
                break;
            }
        }
        if !data.is_empty() {
            self.chunks.push_back((start + trim as u64, data.to_vec()));
            self.bytes += data.len();
        }
        Ok(())
    }
    pub fn advance(&mut self, end: u64) {
        self.end = self.end.max(end);
    }
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.bytes = 0;
    }
    pub fn read(&self, cursor: Option<u64>, max: usize) -> anyhow::Result<Page> {
        self.read_range(
            cursor.unwrap_or_else(|| self.chunks.front().map_or(self.end, |c| c.0)),
            self.end,
            max,
            false,
        )
    }
    fn context_before(&self, mut pos: u64) -> Vec<u8> {
        let mut reversed = Vec::new();
        for (start, chunk) in self.chunks.iter().rev() {
            if *start >= pos {
                continue;
            }
            if pos > *start + chunk.len() as u64 {
                break;
            }
            let part = &chunk[..(pos - *start) as usize];
            reversed.extend(
                part.iter()
                    .rev()
                    .copied()
                    .take_while(|b| *b != b'\n')
                    .take(REDACTION_CONTEXT - reversed.len()),
            );
            if part.contains(&b'\n') || reversed.len() == REDACTION_CONTEXT {
                break;
            }
            pos = *start;
        }
        reversed.reverse();
        reversed
    }

    fn context_after(&self, mut pos: u64) -> Vec<u8> {
        let mut context = Vec::new();
        for (start, chunk) in &self.chunks {
            let end = *start + chunk.len() as u64;
            if end <= pos {
                continue;
            }
            if *start > pos {
                break;
            }
            let part = &chunk[(pos - *start) as usize..];
            context.extend(
                part.iter()
                    .copied()
                    .take_while(|b| *b != b'\n')
                    .take(REDACTION_CONTEXT - context.len()),
            );
            if part.contains(&b'\n') || context.len() == REDACTION_CONTEXT {
                break;
            }
            pos = end;
        }
        context
    }

    pub fn read_range(
        &self,
        mut pos: u64,
        end: u64,
        max: usize,
        clean: bool,
    ) -> anyhow::Result<Page> {
        anyhow::ensure!(
            pos <= end && end <= self.end,
            "cursor is ahead of available output"
        );
        let mut raw = Vec::new();
        let mut gaps = vec![];
        let mut first_byte = None;
        let mut raw_end = pos;
        for (start, data) in &self.chunks {
            let chunk_end = *start + data.len() as u64;
            if pos >= chunk_end {
                continue;
            }
            if raw.len() == max || *start >= end || pos == end {
                break;
            }
            if pos < *start {
                gaps.push((pos, *start));
                pos = *start;
            }
            let from = (pos - start) as usize;
            let n = (chunk_end.min(end) - pos) as usize;
            let n = n.min(max - raw.len());
            first_byte.get_or_insert(pos);
            raw.extend_from_slice(&data[from..from + n]);
            pos += n as u64;
            raw_end = pos;
        }
        if raw.len() < max && pos < end {
            gaps.push((pos, end));
            pos = end;
        }
        // An incremental page is explicitly a transcript, never a screen snapshot.
        // Remove an incomplete UTF-8 tail so the next page can decode it in full.
        if !gaps.iter().any(|&(_, b)| b == pos) {
            for from in raw.len().saturating_sub(3)..raw.len() {
                if let Err(e) = std::str::from_utf8(&raw[from..])
                    && e.valid_up_to() == 0
                    && e.error_len().is_none()
                {
                    let tail = raw.len() - from;
                    raw.truncate(from);
                    pos -= tail as u64;
                    raw_end -= tail as u64;
                    break;
                }
            }
        }
        // Keep a split CRLF together in the command view.
        if clean && pos < end && raw.last() == Some(&b'\r') {
            raw.pop();
            pos -= 1;
            raw_end -= 1;
        }
        let output = if let Some(first_byte) = first_byte.filter(|_| !raw.is_empty()) {
            let mut context = self.context_before(first_byte);
            let after = self.context_after(raw_end);
            let start = context.len();
            let stop = start + raw.len();
            let left_bounded = start == REDACTION_CONTEXT;
            let right_bounded = after.len() == REDACTION_CONTEXT;
            context.extend_from_slice(&raw);
            context.extend_from_slice(&after);
            let mut line_start = 0;
            let mut output = String::new();
            for line in context.split_inclusive(|b| *b == b'\n') {
                let line_end = line_start + line.len();
                let first = start.max(line_start);
                let last = stop.min(line_end);
                if first < last {
                    // Classify the retained line before exposing any page fragment.
                    // Redact a line whose context exceeds the bounded inspection window.
                    if (left_bounded && line_start == 0)
                        || (right_bounded && line_end == context.len())
                        || sensitive_line(line)
                    {
                        output.push_str(REDACTED);
                    } else {
                        escape_text(&context[first..last], clean, &mut output);
                    }
                }
                line_start = line_end;
            }
            output
        } else {
            String::new()
        };
        Ok(Page {
            output,
            next_cursor: pos,
            truncated: pos < end || !gaps.is_empty(),
            lost_bytes: gaps.iter().map(|(a, b)| b - a).sum(),
            missing_ranges: gaps,
            encoding: if clean {
                "sanitized_utf8_command_output"
            } else {
                "sanitized_utf8_transcript_not_screen"
            },
        })
    }
}

fn sensitive_line(line: &[u8]) -> bool {
    [
        "password",
        "passphrase",
        "authorization:",
        "private key",
        "access_token",
        "mfa",
        "verification code",
        "token=",
    ]
    .iter()
    .any(|s| {
        line.windows(s.len())
            .any(|window| window.eq_ignore_ascii_case(s.as_bytes()))
    })
}

fn escape_text(data: &[u8], clean: bool, out: &mut String) {
    let text = String::from_utf8_lossy(data);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if clean && c == '\r' && chars.peek() == Some(&'\n') {
            continue;
        }
        if c == '\n' || c == '\t' || !c.is_control() {
            out.push(c);
        } else {
            use std::fmt::Write;
            let _ = write!(out, "\\u{{{:02x}}}", c as u32);
        }
    }
}

/// Convert a full screen or other unpaged data to escaped text with best-effort redaction.
pub fn safe_text(data: &[u8]) -> String {
    let mut out = String::new();
    for line in data.split_inclusive(|b| *b == b'\n') {
        if sensitive_line(line) {
            out.push_str(REDACTED);
        } else {
            escape_text(line, false, &mut out);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gaps_and_eviction() {
        let mut r = OutputRing::new(8);
        r.push(0, b"abcd").unwrap();
        r.push(8, b"efgh").unwrap();
        let p = r.read(Some(0), 8).unwrap();
        assert_eq!(p.output, "abcdefgh");
        assert_eq!(p.lost_bytes, 4);
        r.push(12, b"ijkl").unwrap();
        let p = r.read(Some(0), 8).unwrap();
        assert_eq!(p.lost_bytes, 8);
        assert_eq!(p.next_cursor, 16);
        assert!(r.read(Some(17), 8).is_err());
    }
    #[test]
    fn utf8_and_controls() {
        let mut r = OutputRing::new(64);
        r.push(0, "中ab".as_bytes()).unwrap();
        assert_eq!(r.read(Some(0), 4).unwrap().output, "中a");
        assert_eq!(
            safe_text(b"\x1b[31mhello\rX\x08\x07"),
            "\\u{1b}[31mhello\\u{0d}X\\u{08}\\u{07}"
        );
        assert!(safe_text(b"password=secret").contains("redacted"));
    }
    #[test]
    fn utf8_split_between_callbacks() {
        let mut r = OutputRing::new(64);
        let bytes = "中".as_bytes();
        r.push(0, &bytes[..1]).unwrap();
        let p = r.read(Some(0), 8).unwrap();
        assert_eq!(p.output, "");
        assert_eq!(p.next_cursor, 0);
        r.push(1, &bytes[1..]).unwrap();
        assert_eq!(r.read(Some(0), 8).unwrap().output, "中");
    }

    #[test]
    fn credential_lines_are_redacted_even_when_read_in_four_byte_pages() {
        let mut r = OutputRing::new(128);
        r.push(0, b"ok\npass").unwrap();
        r.push(r.end, b"word=MCP_FAKE_VALUE_FOR_TEST\r\nafter\r\n")
            .unwrap();
        assert!(r.read(Some(0), 128).unwrap().output.contains("redacted"));

        for clean in [false, true] {
            let mut cursor = 0;
            let mut output = String::new();
            while cursor < r.end {
                let page = r.read_range(cursor, r.end, 4, clean).unwrap();
                assert!(
                    page.next_cursor > cursor,
                    "paging must always make progress"
                );
                output.push_str(&page.output);
                cursor = page.next_cursor;
            }
            assert!(
                !output.contains("MCP_FAKE_VALUE_FOR_TEST"),
                "secret leaked: {output}"
            );
            assert!(output.contains("redacted"));
            assert!(output.contains("ok\n") && output.contains("after"));
        }
    }

    #[test]
    fn credential_redaction_uses_context_outside_the_requested_range() {
        let mut r = OutputRing::new(128);
        r.push(0, b"password=MCP_FAKE_VALUE_FOR_TEST").unwrap();
        for clean in [false, true] {
            let page = r.read_range(9, 13, 4, clean).unwrap();
            assert!(page.output.contains("redacted"));
            assert_eq!(page.next_cursor, 13);
        }
    }

    #[test]
    fn overlong_line_is_redacted_when_page_context_cannot_reach_its_start() {
        let mut r = OutputRing::new(3 * REDACTION_CONTEXT);
        let line = format!(
            "password={}MCP_FAKE_VALUE_FOR_TEST\n",
            "x".repeat(REDACTION_CONTEXT + 1)
        );
        r.push(0, line.as_bytes()).unwrap();
        let cursor = (line.len() - "MCP_FAKE_VALUE_FOR_TEST\n".len()) as u64;
        let page = r.read(Some(cursor), 64).unwrap();
        assert_eq!(page.output, REDACTED);
        assert_eq!(page.next_cursor, r.end);
    }
}
