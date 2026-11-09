//! Bounded metadata-only audit writer. Disk I/O never runs on a terminal thread
//! or on the broker reactor. Two rotating 1 MiB files; no commands or output.
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{SyncSender, sync_channel},
    },
    time::{SystemTime, UNIX_EPOCH},
};
#[derive(Clone)]
pub struct Audit {
    tx: SyncSender<Value>,
    healthy: Arc<AtomicBool>,
}
impl Audit {
    pub fn start() -> Result<Self> {
        let dir = PathBuf::from(
            std::env::var_os("LOCALAPPDATA")
                .ok_or_else(|| anyhow::anyhow!("LOCALAPPDATA unavailable"))?,
        )
        .join("PuTTY-AI-v4.4.0");
        std::fs::create_dir_all(&dir)?;
        putty_ai_common::security::restrict_path(&dir)?;
        let path = dir.join("audit.jsonl");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        putty_ai_common::security::restrict_path(&path)?;
        let (tx, rx) = sync_channel::<Value>(128);
        let healthy = Arc::new(AtomicBool::new(true));
        let flag = healthy.clone();
        std::thread::Builder::new()
            .name("putty-ai-audit".into())
            .spawn(move || {
                let result: Result<()> = (|| {
                    for mut event in rx {
                        event["unix_ms"] =
                            json!(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis());
                        if file.metadata()?.len() > 1024 * 1024 {
                            drop(file);
                            let old = dir.join("audit.previous.jsonl");
                            if old.exists() {
                                std::fs::remove_file(&old)?;
                            }
                            std::fs::rename(&path, &old)?;
                            file = std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(&path)?;
                            putty_ai_common::security::restrict_path(&path)?;
                        }
                        serde_json::to_writer(&mut file, &event)?;
                        file.write_all(b"\n")?;
                        file.flush()?;
                    }
                    Ok(())
                })();
                if result.is_err() {
                    flag.store(false, Ordering::Release);
                    eprintln!("audit storage failed; new write requests will be rejected");
                }
            })?;
        Ok(Self { tx, healthy })
    }
    pub fn record(&self, event: Value) -> Result<()> {
        ensure!(
            self.healthy.load(Ordering::Acquire),
            "audit storage unavailable"
        );
        self.tx
            .try_send(event)
            .map_err(|_| anyhow::anyhow!("audit queue full; write request rejected"))
    }
}
