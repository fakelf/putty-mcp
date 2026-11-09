mod connection_report;
mod diagnostics;
mod server;
use anyhow::{Context, Result, ensure};
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
    pipe: Option<NamedPipeClient>,
    source: Option<String>,
}
impl Broker {
    async fn connect(source: &str) -> Result<NamedPipeClient> {
        let name = pipe_name()?;
        for attempt in 0..40 {
            if let Ok(mut pipe) = ClientOptions::new().open(&name) {
                let handshake: Result<()> = async {
                    peer_pid(&pipe, false)?;
                    let mut hello = Frame::new(ADAPTER_HELLO, Uuid::nil());
                    hello.data = source.as_bytes().to_vec();
                    timeout(Duration::from_secs(2), hello.write(&mut pipe)).await??;
                    let ack = timeout(Duration::from_secs(2), Frame::read(&mut pipe)).await??;
                    ensure!(
                        ack.kind == REPLY && ack.request.is_nil(),
                        "adapter registration rejected"
                    );
                    Ok(())
                }
                .await;
                handshake.context("Broker IPC handshake failed")?;
                return Ok(pipe);
            }
            if attempt == 0 {
                let path = std::env::current_exe()?;
                // Independent process; MCP exit never terminates the broker.
                putty_ai_common::process::start_broker(&path)
                    .context("Broker process launch failed")?;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        anyhow::bail!("broker unavailable")
    }
    async fn initialize(&mut self, client_name: &str) -> Result<()> {
        ensure!(self.source.is_none(), "MCP client already initialized");
        let source = std::env::var("PUTTY_MCP_SOURCE")
            .ok()
            .and_then(|name| {
                let name = name.trim();
                let label = format!("{name} (local configuration)");
                (!name.is_empty() && safe_source(&label)).then_some(label)
            })
            .or_else(|| {
                let client_name = client_name.trim();
                let name = format!("{client_name} (reported by MCP client)");
                (!client_name.is_empty() && safe_source(&name)).then_some(name)
            })
            .unwrap_or_else(|| "Unknown MCP client".into());
        self.pipe = Some(Self::connect(&source).await?);
        self.source = Some(source);
        Ok(())
    }
    async fn rpc(&mut self, method: &str, args: Value) -> Result<Value> {
        if self.pipe.is_none() {
            let source = self
                .source
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("MCP client not initialized"))?;
            self.pipe = Some(Self::connect(source).await?);
        }
        let mut f = Frame::new(RPC, Uuid::nil());
        f.request = Uuid::new_v4();
        f.data = serde_json::to_vec(&json!({"method":method,"args":args}))?;
        let reply = self.exchange(&f).await?;
        let v: Value = serde_json::from_slice(&reply.data)?;
        if let Some(e) = v.get("error") {
            anyhow::bail!("{}", e.as_str().unwrap_or("broker error"));
        }
        Ok(v["ok"].clone())
    }
    async fn heartbeat(&mut self) {
        if self.pipe.is_some() {
            let mut ping = Frame::new(ADAPTER_PING, Uuid::nil());
            ping.request = Uuid::new_v4();
            let _ = self.exchange(&ping).await;
        }
    }
    async fn exchange(&mut self, f: &Frame) -> Result<Frame> {
        let pipe = self.pipe.as_mut().unwrap();
        let result: Result<Frame> = async {
            timeout(Duration::from_secs(3), f.write(pipe)).await??;
            timeout(Duration::from_secs(3), Frame::read(pipe)).await?
        }
        .await;
        let reply = match result {
            Ok(r) => r,
            Err(e) => {
                self.pipe = None;
                anyhow::bail!(
                    "broker IPC failed; operation outcome may be unknown; no automatic retry: {e}"
                );
            }
        };
        ensure!(
            reply.kind == REPLY && reply.request == f.request,
            "broker response mismatch"
        );
        Ok(reply)
    }
}
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    if let Some(arg) = args.next() {
        if arg == "--connection-report" {
            return connection_report::from_args(args).await;
        }
        ensure!(
            arg == "--broker" && args.next().is_none(),
            "MCP startup takes no arguments"
        );
        return putty_ai_broker::run().await;
    }
    let broker = Arc::new(Mutex::new(Broker {
        pipe: None,
        source: None,
    }));
    let heartbeat = broker.clone();
    let task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        loop {
            tick.tick().await;
            heartbeat.lock().await.heartbeat().await;
        }
    });
    let transport =
        rmcp::transport::async_rw::AsyncRwTransport::new(tokio::io::stdin(), tokio::io::stdout())
            .with_max_line_length(128 * 1024);
    let diagnostics = diagnostics::Diagnostics::start().await;
    let running = server::Mcp::new(broker, diagnostics)
        .serve(transport)
        .await?;
    let result = running.waiting().await;
    task.abort();
    result?;
    Ok(())
}
