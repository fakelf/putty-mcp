use putty_ai_common::security::process_start;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{fs, sync::Mutex};

#[derive(Clone)]
pub struct Diagnostics {
    path: PathBuf,
    snapshot: Arc<Mutex<Value>>,
}

impl Diagnostics {
    pub async fn start() -> Option<Self> {
        if std::env::var("PUTTY_MCP_DIAGNOSTICS").ok().as_deref() != Some("qwenpaw") {
            return None;
        }
        let pid = std::process::id();
        let diagnostics = Self {
            path: PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
                .join("PuTTY-AI-v4.4.0")
                .join("qwenpaw-connection.json"),
            snapshot: Arc::new(Mutex::new(json!({
                "adapter": std::env::current_exe().ok()?,
                "pid": pid,
                "process_start": process_start(pid).ok()?.to_string(),
                "initialized": false,
                "tools_listed": false,
                "session_count": null,
                "failure": null
            }))),
        };
        diagnostics.update(|_| {}).await;
        Some(diagnostics)
    }

    pub async fn update(&self, change: impl FnOnce(&mut Value)) {
        let mut snapshot = self.snapshot.lock().await;
        change(&mut snapshot);
        snapshot["updated_ms"] = json!(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64
        );
        if let Err(error) = self.write(&snapshot).await {
            eprintln!("PuTTY MCP diagnostic status could not be saved: {error}");
        }
    }

    async fn write(&self, snapshot: &Value) -> std::io::Result<()> {
        let dir = self.path.parent().expect("diagnostic file has a parent");
        fs::create_dir_all(dir).await?;
        let temp = dir.join(format!("qwenpaw-connection-{}.tmp", std::process::id()));
        fs::write(&temp, snapshot.to_string()).await?;
        if let Err(error) = fs::rename(&temp, &self.path).await {
            let _ = fs::remove_file(temp).await;
            return Err(error);
        }
        Ok(())
    }
}
