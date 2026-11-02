mod server;
use anyhow::{Result, ensure};
use putty_ai_common::{protocol::*, security::*};
use rmcp::ServiceExt;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{
    net::windows::named_pipe::{ClientOptions, NamedPipeClient},
    sync::Mutex,
    time::timeout,
};
use uuid::Uuid;

pub struct Broker {
    pipe: NamedPipeClient,
    healthy: bool,
}
impl Broker {
    async fn connect() -> Result<Self> {
        let name = pipe_name()?;
        for attempt in 0..40 {
            if let Ok(pipe) = ClientOptions::new().open(&name) {
                peer_pid(&pipe, false)?;
                return Ok(Self {
                    pipe,
                    healthy: true,
                });
            }
            if attempt == 0 {
                let path = std::env::current_exe()?.with_file_name("putty-ai-broker.exe");
                // Independent process; MCP exit never terminates the broker.
                putty_ai_common::process::start_broker(&path)?;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        anyhow::bail!("broker unavailable")
    }
    async fn rpc(&mut self, method: &str, args: Value) -> Result<Value> {
        if !self.healthy {
            *self = Self::connect().await?;
        }
        let mut f = Frame::new(RPC, Uuid::nil());
        f.request = Uuid::new_v4();
        f.data = serde_json::to_vec(&json!({"method":method,"args":args}))?;
        let result: Result<Frame> = async {
            timeout(Duration::from_secs(3), f.write(&mut self.pipe)).await??;
            timeout(Duration::from_secs(3), Frame::read(&mut self.pipe)).await?
        }
        .await;
        let reply = match result {
            Ok(r) => r,
            Err(e) => {
                self.healthy = false;
                anyhow::bail!(
                    "broker IPC failed; operation outcome may be unknown; no automatic retry: {e}"
                );
            }
        };
        ensure!(
            reply.kind == REPLY && reply.request == f.request,
            "broker response mismatch"
        );
        let v: Value = serde_json::from_slice(&reply.data)?;
        if let Some(e) = v.get("error") {
            anyhow::bail!("{}", e.as_str().unwrap_or("broker error"));
        }
        Ok(v["ok"].clone())
    }
}
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let broker = Arc::new(Mutex::new(Broker::connect().await?));
    let heartbeat = broker.clone();
    let task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        loop {
            tick.tick().await;
            let _ = heartbeat.lock().await.rpc("list_sessions", json!({})).await;
        }
    });
    let transport =
        rmcp::transport::async_rw::AsyncRwTransport::new(tokio::io::stdin(), tokio::io::stdout())
            .with_max_line_length(128 * 1024);
    let running = server::Mcp::new(broker).serve(transport).await?;
    let result = running.waiting().await;
    task.abort();
    result?;
    Ok(())
}
