use anyhow::{Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

pub const MAX_FRAME: usize = 128 * 1024;
pub const HEADER: usize = 64;
pub const HELLO: u16 = 1;
pub const OUTPUT: u16 = 2;
pub const POLL: u16 = 3;
pub const TOP_EVENT: u16 = 4;
pub const TOP_SCREEN: u16 = 5;
pub const CHILD_EVENT: u16 = 6;
pub const TERMINAL_SCREEN: u16 = 7;
pub const COMMAND: u16 = 11;
pub const REVOKE: u16 = 12;
pub const INTERRUPT: u16 = 13;
pub const SEND_INTERRUPT: u16 = 15;
pub const TOP_START: u16 = 16;
pub const TOP_QUIT: u16 = 17;
pub const TOP_INTERRUPT: u16 = 18;
pub const TOP_SEND_KEY: u16 = 19;
pub const VI_START: u16 = 20;
pub const VI_QUIT: u16 = 21;
pub const SPECIAL_KEY: u16 = 22;
pub const VI_INSERT_TEXT: u16 = 23;
pub const VI_FILE_START: u16 = 24;
pub const VI_SAVE: u16 = 25;
pub const CHILD_OPEN: u16 = 26;
pub const CHILD_CLOSE: u16 = 27;
pub const CHILD_COMMIT: u16 = 28;
pub const TERMINAL_ATTACH: u16 = 29;
pub const TERMINAL_INPUT: u16 = 30;
pub const TERMINAL_COMMIT: u16 = 31;
pub const TERMINAL_DETACH: u16 = 32;
pub const VI_TEXT_MAX: usize = 1024;
pub const VI_PATH_MAX: usize = 512;
pub const VI_FILE_PREFIX: &[u8] = b"vi-file\0";
pub const COMMAND_SENT: u32 = 1;
pub const COMMAND_NOT_SENT: u32 = 2;
pub const INTERRUPT_APPROVED: u32 = 3;
pub const INTERRUPT_NOT_SENT: u32 = 4;
pub const INTERRUPT_SENT: u32 = 5;
pub const TOP_STARTED: u32 = 6;
pub const TOP_NOT_STARTED: u32 = 7;
pub const TOP_KEY_APPROVED: u32 = 8;
pub const TOP_KEY_SENT: u32 = 9;
pub const TOP_KEY_NOT_SENT: u32 = 10;
pub const CHILD_APPROVED: u32 = 11;
pub const CHILD_ACCEPTED: u32 = 12;
pub const CHILD_REJECTED: u32 = 13;
pub const TERMINAL_ATTACHED: u32 = 14;
pub const TERMINAL_ATTACH_NOT: u32 = 15;
pub const TERMINAL_INPUT_APPROVED: u32 = 16;
pub const TERMINAL_INPUT_SENT: u32 = 17;
pub const TERMINAL_INPUT_NOT_SENT: u32 = 18;
pub const TERMINAL_ATTACH_APPROVED: u32 = 19;
pub const TERMINAL_DETACHED: u32 = 20;
pub const TERMINAL_DETACH_NOT_SENT: u32 = 21;
pub const RPC: u16 = 100;
pub const REPLY: u16 = 101;
pub const ADAPTER_HELLO: u16 = 102;
pub const CONNECTION_REPORT: u16 = 103;
pub const ADAPTER_PING: u16 = 104;
pub const READ: u32 = 1;
pub const CONNECTED: u32 = 2;
pub const TOP_CAPABLE: u32 = 4;
pub const CHILD_MODE: u32 = 8;
pub const CHILD_SESSION: u32 = 16;
pub const AUTO_RUN: u32 = 32;
pub const TERMINAL_CAPABLE: u32 = 64;
pub const REQUEST_AUTO_RUN: u32 = 1 << 31;
pub const SOURCE_MAX: usize = 128;
pub const PROMPT_MAX: usize = 256;
const WIRE_VERSION: u16 = 2;

pub fn safe_source(source: &str) -> bool {
    !source.trim().is_empty()
        && source.len() <= SOURCE_MAX
        && !source.chars().any(|c| {
            c.is_control()
                || matches!(c, '\u{061c}' | '\u{200b}'..='\u{200f}' |
                '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
        })
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub kind: u16,
    pub flags: u32,
    pub session: Uuid,
    pub request: Uuid,
    pub seq: u64,
    pub epoch: u64,
    pub data: Vec<u8>,
}
impl Frame {
    pub fn new(kind: u16, session: Uuid) -> Self {
        Self {
            kind,
            flags: 0,
            session,
            request: Uuid::nil(),
            seq: 0,
            epoch: 0,
            data: vec![],
        }
    }
    pub fn header(&self) -> Result<[u8; HEADER]> {
        if self.data.len() > MAX_FRAME {
            bail!("frame too large")
        }
        let mut h = [0; HEADER];
        h[0..4].copy_from_slice(b"PAI1");
        h[4..6].copy_from_slice(&WIRE_VERSION.to_le_bytes());
        h[6..8].copy_from_slice(&self.kind.to_le_bytes());
        h[8..12].copy_from_slice(&(self.data.len() as u32).to_le_bytes());
        h[12..16].copy_from_slice(&self.flags.to_le_bytes());
        h[16..32].copy_from_slice(self.session.as_bytes());
        h[32..48].copy_from_slice(self.request.as_bytes());
        h[48..56].copy_from_slice(&self.seq.to_le_bytes());
        h[56..64].copy_from_slice(&self.epoch.to_le_bytes());
        Ok(h)
    }
    pub async fn write(&self, w: &mut (impl AsyncWrite + Unpin)) -> Result<()> {
        w.write_all(&self.header()?).await?;
        w.write_all(&self.data).await?;
        Ok(())
    }
    pub async fn read(r: &mut (impl AsyncRead + Unpin)) -> Result<Self> {
        let mut h = [0; HEADER];
        r.read_exact(&mut h).await?;
        if &h[..4] != b"PAI1" || u16::from_le_bytes(h[4..6].try_into()?) != WIRE_VERSION {
            bail!("unsupported IPC protocol")
        }
        let len = u32::from_le_bytes(h[8..12].try_into()?) as usize;
        if len > MAX_FRAME {
            bail!("frame exceeds limit")
        }
        let mut f = Self {
            kind: u16::from_le_bytes(h[6..8].try_into()?),
            flags: u32::from_le_bytes(h[12..16].try_into()?),
            session: Uuid::from_slice(&h[16..32])?,
            request: Uuid::from_slice(&h[32..48])?,
            seq: u64::from_le_bytes(h[48..56].try_into()?),
            epoch: u64::from_le_bytes(h[56..64].try_into()?),
            data: vec![0; len],
        };
        r.read_exact(&mut f.data).await?;
        Ok(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn roundtrip_and_limits() {
        let (mut w, mut r) = tokio::io::duplex(1024);
        let mut f = Frame::new(OUTPUT, Uuid::new_v4());
        f.data = b"hello".to_vec();
        f.seq = 42;
        f.write(&mut w).await.unwrap();
        let got = Frame::read(&mut r).await.unwrap();
        assert_eq!(got.session, f.session);
        assert_eq!(got.seq, 42);
        assert_eq!(got.data, b"hello");
        f.data.resize(MAX_FRAME + 1, 0);
        assert!(f.header().is_err());
        let mut h = [0u8; HEADER];
        h[..4].copy_from_slice(b"PAI1");
        h[4..6].copy_from_slice(&WIRE_VERSION.to_le_bytes());
        h[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        w.write_all(&h).await.unwrap();
        assert!(Frame::read(&mut r).await.is_err());
    }
}
