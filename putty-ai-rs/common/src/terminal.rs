use serde::Serialize;
use std::collections::VecDeque;

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
        let mut pos = cursor.unwrap_or_else(|| self.chunks.front().map_or(self.end, |c| c.0));
        anyhow::ensure!(pos <= self.end, "cursor is ahead of session output");
        let mut raw = Vec::new();
        let mut gaps = vec![];
        for (start, data) in &self.chunks {
            let end = *start + data.len() as u64;
            if pos >= end {
                continue;
            }
            if raw.len() == max {
                break;
            }
            if pos < *start {
                gaps.push((pos, *start));
                pos = *start;
            }
            let from = (pos - start) as usize;
            let n = (data.len() - from).min(max - raw.len());
            raw.extend_from_slice(&data[from..from + n]);
            pos += n as u64;
        }
        if raw.len() < max && pos < self.end {
            gaps.push((pos, self.end));
            pos = self.end;
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
                    break;
                }
            }
        }
        Ok(Page {
            output: safe_text(&raw),
            next_cursor: pos,
            truncated: pos < self.end || !gaps.is_empty(),
            lost_bytes: gaps.iter().map(|(a, b)| b - a).sum(),
            missing_ranges: gaps,
            encoding: "sanitized_utf8_transcript_not_screen",
        })
    }
}

/// Escape sequences and C0/C1 controls become visible escapes. This is stateless
/// across pages: even a split OSC/CSI cannot become an active terminal instruction.
/// Credential-like lines are conservatively redacted; no filter can infer all secrets.
pub fn safe_text(data: &[u8]) -> String {
    let text = String::from_utf8_lossy(data);
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        let lower = line.to_ascii_lowercase();
        if [
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
        .any(|s| lower.contains(s))
        {
            out.push_str("[sensitive line redacted]\n");
            continue;
        }
        for c in line.chars() {
            if c == '\n' || c == '\t' || !c.is_control() {
                out.push(c);
            } else {
                use std::fmt::Write;
                let _ = write!(out, "\\u{{{:02x}}}", c as u32);
            }
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
}
