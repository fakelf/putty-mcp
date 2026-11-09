mod audit;
mod child;
mod foreground;
mod interactive;
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
pub async fn run() -> Result<()> {
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
    eprintln!("PuTTY AI Broker ready (local current-user named pipe)");
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
        native(pipe, state, pid, first, audit).await
    } else if first.kind == CONNECTION_REPORT {
        ensure!(first.data.is_empty(), "invalid connection report request");
        let mut reply = Frame::new(REPLY, first.session);
        let report = {
            let model = state.lock().await;
            let registered = |s: &Session| s.flags & (READ | CONNECTED) == READ | CONNECTED;
            json!({
                "adapter_count": model.adapter_count,
                "session_count": model.sessions.values().filter(|s| registered(s)).count(),
                "window_registered": model.sessions.get(&first.session).is_some_and(registered),
            })
        };
        reply.data = serde_json::to_vec(&report)?;
        timeout(Duration::from_secs(2), reply.write(&mut pipe)).await??;
        Ok(())
    } else {
        adapter(pipe, state, first, audit).await
    }
}
async fn native(
    mut pipe: NamedPipeServer,
    state: State,
    pid: u32,
    hello: Frame,
    audit: audit::Audit,
) -> Result<()> {
    ensure!(
        matches!(hello.data.len(), 20 | 36) && !hello.session.is_nil(),
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
    let parent = hello
        .data
        .get(20..)
        .filter(|data| !data.is_empty())
        .map(Uuid::from_slice)
        .transpose()?;
    let id = hello.session;
    let generation = Uuid::new_v4();
    let (tx, mut rx) = mpsc::channel(8);
    {
        let mut m = state.lock().await;
        let mut output = OutputRing::new(RING_SIZE);
        output.advance(hello.seq);
        if let Some(event) = m.register_native(
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
                active: None,
                lifecycle: None,
                parent: None,
                received: 0,
                last_seen: Instant::now(),
                lost_total: 0,
                native_dropped: 0,
            },
            parent,
        )? {
            let _ = audit.record(event);
        }
    }
    let result: Result<()> = async {
        let mut ack = Frame::new(0, id);
        ack.epoch = hello.epoch;
        timeout(Duration::from_secs(2), ack.write(&mut pipe)).await??;
        loop {
            let f = timeout(Duration::from_secs(5), Frame::read(&mut pipe)).await??;
            ensure!(
                f.session == id
                    && matches!(
                        f.kind,
                        OUTPUT | POLL | TOP_EVENT | TOP_SCREEN | CHILD_EVENT | TERMINAL_SCREEN
                    ),
                "wrong session or native message"
            );
            let mut m = state.lock().await;
            if let Some(event) = m.native_state(id, &f)? {
                let _ = audit.record(event);
            }
            let reply = if f.kind == POLL
                && !f.request.is_nil()
                && (f.data.get(..4) == Some(&TERMINAL_INPUT_APPROVED.to_le_bytes())
                    || f.data.get(..4) == Some(&TERMINAL_ATTACH_APPROVED.to_le_bytes()))
            {
                m.foreground_commit(id, f.request)
            } else if f.kind == POLL
                && !f.request.is_nil()
                && f.data.get(..4) == Some(&CHILD_APPROVED.to_le_bytes())
            {
                m.child_commit(id, f.request)
            } else if f.kind == POLL
                && !f.request.is_nil()
                && f.data.get(..4) == Some(&TOP_KEY_APPROVED.to_le_bytes())
            {
                m.top_key_commit(id, f.request)
            } else if f.kind == POLL
                && !f.request.is_nil()
                && f.data.get(..4) == Some(&INTERRUPT_APPROVED.to_le_bytes())
            {
                m.interrupt_commit(id, f.request)
            } else {
                (f.kind == POLL)
                    .then(|| rx.try_recv().ok())
                    .flatten()
                    .unwrap_or_else(|| Frame::new(0, id))
            };
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
    first: Frame,
    audit: audit::Audit,
) -> Result<()> {
    let client = Uuid::new_v4();
    ensure!(
        first.kind == ADAPTER_HELLO && first.session.is_nil() && first.request.is_nil(),
        "invalid adapter registration"
    );
    let source = std::str::from_utf8(&first.data)?;
    ensure!(safe_source(source), "invalid adapter source");
    timeout(
        Duration::from_secs(2),
        Frame::new(REPLY, Uuid::nil()).write(&mut pipe),
    )
    .await??;
    state.lock().await.adapter_count += 1;
    let result:Result<()>=async {
        loop {
            let f=timeout(Duration::from_secs(10),Frame::read(&mut pipe)).await??;
            ensure!(f.session.is_nil() && !f.request.is_nil(),"invalid adapter frame");
            if f.kind == ADAPTER_PING && f.data.is_empty() {
                let mut reply = Frame::new(REPLY, Uuid::nil());
                reply.request = f.request;
                timeout(Duration::from_secs(2), reply.write(&mut pipe)).await??;
                continue;
            }
            ensure!(f.kind==RPC,"invalid adapter frame");
            let req:Value=serde_json::from_slice(&f.data)?;
            let method=req.get("method").and_then(Value::as_str).unwrap_or("");
            let args=req.get("args").unwrap_or(&Value::Null);
            let session=args.get("session_id").and_then(Value::as_str).and_then(|s|Uuid::parse_str(s).ok());
            let task=args.get("task_id").and_then(Value::as_str).and_then(|s|Uuid::parse_str(s).ok());
            let writing=matches!(method,"submit_command"|"cancel_task"|"interrupt_task"|"start_interactive"|"request_task_action"|"open_child_session"|"close_child_session"|"attach_terminal");
            let audited=if writing { audit.record(json!({"event":"request","client":client,"source":source,"session":session,"task":task,"tool":method})) } else {Ok(())};
            let result=match audited { Ok(())=>state.lock().await.rpc(client,source,method,args),Err(e)=>Err(e) };
            let data=match result { Ok(v)=>json!({"ok":v}),Err(e)=>json!({"error":e.to_string()}) };
            if writing {
                // Metadata only; commands and terminal content are never logged.
                eprintln!("audit client={client} session={session:?} tool={method} rpc_success={}",data.get("ok").is_some());
                let _=audit.record(json!({"event":"result","client":client,"source":source,"session":session,"task":data.pointer("/ok/task_id"),"tool":method,"rpc_success":data.get("ok").is_some(),"status":data.pointer("/ok/status")}));
            }
            let mut reply=Frame::new(REPLY,Uuid::nil()); reply.request=f.request;
            reply.data=serde_json::to_vec(&data)?;
            timeout(Duration::from_secs(2),reply.write(&mut pipe)).await??;
        }
    }.await;
    let mut model = state.lock().await;
    model.adapter_count -= 1;
    model.client_gone(client);
    result
}
