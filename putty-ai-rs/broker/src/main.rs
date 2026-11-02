mod audit;
mod model;
#[cfg(test)]
mod tests;
use anyhow::{Result, ensure};
use model::{Model, Session};
use putty_ai_common::{RING_SIZE, protocol::*, security::*, terminal::OutputRing};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
    sync::{Mutex, Semaphore, mpsc},
    time::timeout,
};
use uuid::Uuid;

type State = Arc<Mutex<Model>>;
fn pipe(first: bool) -> Result<NamedPipeServer> {
    let acl = PipeAcl::new()?;
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .max_instances(64)
        .in_buffer_size(128 * 1024)
        .out_buffer_size(128 * 1024);
    Ok(unsafe {
        options.create_with_security_attributes_raw(
            pipe_name()?,
            &acl.attributes as *const _ as *mut _,
        )
    }?)
}
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    // FILE_FLAG_FIRST_PIPE_INSTANCE is the atomic per-user singleton guard.
    let mut listener = pipe(true)?;
    let state: State = Arc::new(Mutex::new(Model::default()));
    let audit = audit::Audit::start()?;
    let slots = Arc::new(Semaphore::new(60));
    let cleanup = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            tick.tick().await;
            cleanup.lock().await.expire();
        }
    });
    eprintln!("PuTTY AI Broker ready (local current-user named pipe, protocol v1)");
    loop {
        listener.connect().await?;
        let connected = listener;
        listener = pipe(false)?;
        if let Ok(permit) = slots.clone().try_acquire_owned() {
            let state = state.clone();
            let audit = audit.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Err(e) = connection(connected, state, audit).await {
                    eprintln!("IPC connection ended: {e}");
                }
            });
        }
    }
}
async fn connection(mut pipe: NamedPipeServer, state: State, audit: audit::Audit) -> Result<()> {
    let pid = peer_pid(&pipe, true)?;
    let first = timeout(Duration::from_secs(3), Frame::read(&mut pipe)).await??;
    if first.kind == HELLO {
        native(pipe, state, pid, first).await
    } else {
        adapter(pipe, state, first, audit).await
    }
}
async fn native(mut pipe: NamedPipeServer, state: State, pid: u32, hello: Frame) -> Result<()> {
    ensure!(
        hello.data.len() == 20 && !hello.session.is_nil(),
        "invalid native registration"
    );
    ensure!(
        u32::from_le_bytes(hello.data[..4].try_into()?) == pid,
        "registration PID mismatch"
    );
    let start = u64::from_le_bytes(hello.data[4..12].try_into()?);
    ensure!(
        start == process_start(pid)?,
        "process start identity mismatch"
    );
    let hwnd = u64::from_le_bytes(hello.data[12..20].try_into()?);
    let id = hello.session;
    let generation = Uuid::new_v4();
    let (tx, mut rx) = mpsc::channel(8);
    {
        let mut m = state.lock().await;
        ensure!(
            m.sessions.len() < 32 && !m.sessions.contains_key(&id),
            "session limit or duplicate identity"
        );
        let mut output = OutputRing::new(RING_SIZE);
        output.advance(hello.seq);
        m.sessions.insert(
            id,
            Session {
                generation,
                pid,
                start,
                hwnd,
                flags: hello.flags,
                epoch: hello.epoch,
                output,
                tx,
                lease: None,
                pending: None,
                pending_deadline: None,
                rejected_client: None,
                pending_custom: false,
                lease_custom: false,
                active: None,
                received: 0,
                last_seen: Instant::now(),
                lost_total: 0,
                native_dropped: 0,
            },
        );
    }
    let result: Result<()> = async {
        let mut ack = Frame::new(0, id);
        ack.epoch = hello.epoch;
        timeout(Duration::from_secs(2), ack.write(&mut pipe)).await??;
        loop {
            let f = timeout(Duration::from_secs(5), Frame::read(&mut pipe)).await??;
            ensure!(
                f.session == id && matches!(f.kind, OUTPUT | POLL),
                "wrong session or native message"
            );
            let mut m = state.lock().await;
            m.native_state(id, &f)?;
            let reply = rx.try_recv().unwrap_or_else(|_| Frame::new(0, id));
            drop(m);
            timeout(Duration::from_secs(2), reply.write(&mut pipe)).await??;
        }
    }
    .await;
    state.lock().await.disconnect(id, generation);
    result
}
async fn adapter(
    mut pipe: NamedPipeServer,
    state: State,
    mut f: Frame,
    audit: audit::Audit,
) -> Result<()> {
    let client = Uuid::new_v4();
    let result:Result<()>=async {
        loop {
            ensure!(f.kind==RPC && f.session.is_nil() && !f.request.is_nil(),"invalid adapter frame");
            let req:Value=serde_json::from_slice(&f.data)?;
            let method=req.get("method").and_then(Value::as_str).unwrap_or("");
            let args=req.get("args").unwrap_or(&Value::Null);
            let session=args.get("session_id").and_then(Value::as_str).and_then(|s|Uuid::parse_str(s).ok());
            let task=args.get("task_id").and_then(Value::as_str).and_then(|s|Uuid::parse_str(s).ok());
            let writing=matches!(method,"acquire_control"|"submit_command"|"cancel_task"|"release_control");
            let audited=if writing { audit.record(json!({"event":"request","client":client,"session":session,"task":task,"tool":method})) } else {Ok(())};
            let result=match audited { Ok(())=>state.lock().await.rpc(client,method,args),Err(e)=>Err(e) };
            let data=match result { Ok(v)=>json!({"ok":v}),Err(e)=>json!({"error":e.to_string()}) };
            if writing {
                // Metadata only; commands and terminal content are never logged.
                eprintln!("audit client={client} session={session:?} tool={method} rpc_success={}",data.get("ok").is_some());
                let _=audit.record(json!({"event":"result","client":client,"session":session,"tool":method,"rpc_success":data.get("ok").is_some(),"status":data.pointer("/ok/status")}));
            }
            let mut reply=Frame::new(REPLY,Uuid::nil()); reply.request=f.request;
            reply.data=serde_json::to_vec(&data)?;
            timeout(Duration::from_secs(2),reply.write(&mut pipe)).await??;
            // Adapter heartbeats use list_sessions, keeping idle leases tied to a live connection.
            f=timeout(Duration::from_secs(10),Frame::read(&mut pipe)).await??;
        }
    }.await;
    state.lock().await.client_gone(client);
    result
}
