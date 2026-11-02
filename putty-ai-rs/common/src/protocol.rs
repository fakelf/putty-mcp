use anyhow::{Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

pub const MAX_FRAME: usize = 128 * 1024;
pub const HEADER: usize = 64;
pub const HELLO: u16 = 1;
pub const OUTPUT: u16 = 2;
pub const POLL: u16 = 3;
pub const ACQUIRE: u16 = 10;
pub const COMMAND: u16 = 11;
pub const REVOKE: u16 = 12;
pub const ACQUIRE_CUSTOM: u16 = 13;
pub const COMMAND_CUSTOM: u16 = 14;
pub const RPC: u16 = 100;
pub const REPLY: u16 = 101;
pub const READ: u32 = 1;
pub const CONNECTED: u32 = 2;
pub const CONTROL: u32 = 8;
pub const CUSTOM_CAPABLE: u32 = 16;
pub const CUSTOM_CONTROL: u32 = 32;

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
        h[4..6].copy_from_slice(&1u16.to_le_bytes());
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
        if &h[..4] != b"PAI1" || u16::from_le_bytes(h[4..6].try_into()?) != 1 {
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
        h[4] = 1;
        h[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        w.write_all(&h).await.unwrap();
        assert!(Frame::read(&mut r).await.is_err());
    }
}
