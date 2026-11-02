//! Runs real Windows named pipes and real executables. Native peers are simulated;
//! this never connects to SSH, a bastion, or a production server.
use anyhow::{Result, ensure};
use putty_ai_common::{protocol::*, security::*};
use serde_json::{Value, json};
use std::{
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tokio::{
    net::windows::named_pipe::{ClientOptions, NamedPipeClient},
    sync::mpsc,
    time::{sleep, timeout},
};
use uuid::Uuid;

struct Children(Vec<Child>);
impl Drop for Children {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
async fn connect() -> Result<NamedPipeClient> {
    for _ in 0..100 {
        if let Ok(p) = ClientOptions::new().open(pipe_name()?) {
            peer_pid(&p, false)?;
            return Ok(p);
        }
        sleep(Duration::from_millis(20)).await;
    }
    anyhow::bail!("test pipe unavailable")
}
async fn exchange(p: &mut NamedPipeClient, f: Frame) -> Result<Frame> {
    f.write(p).await?;
    timeout(Duration::from_secs(5), Frame::read(p)).await?
}
async fn rpc(p: &mut NamedPipeClient, method: &str, args: Value) -> Result<Value> {
    let mut f = Frame::new(RPC, Uuid::nil());
    f.request = Uuid::new_v4();
    f.data = serde_json::to_vec(&json!({"method":method,"args":args}))?;
    let r = exchange(p, f).await?;
    ensure!(r.kind == REPLY, "invalid reply");
    let v: Value = serde_json::from_slice(&r.data)?;
    if let Some(e) = v.get("error") {
        anyhow::bail!("{e}");
    }
    Ok(v["ok"].clone())
}
enum Action {
    Flood(usize),
    Human,
    Close,
}
async fn mock(id: Uuid, index: usize, mut actions: mpsc::Receiver<Action>) -> Result<()> {
    let mut p = connect().await?;
    let mut h = Frame::new(HELLO, id);
    h.flags = READ | CONNECTED;
    h.epoch = 1;
    h.data.extend_from_slice(&std::process::id().to_le_bytes());
    h.data
        .extend_from_slice(&process_start(std::process::id())?.to_le_bytes());
    h.data.extend_from_slice(&0u64.to_le_bytes());
    exchange(&mut p, h).await?;
    let mut seq = 0;
    let mut flags = READ | CONNECTED;
    let mut epoch = 1;
    let mut client = Uuid::nil();
    let mut ack = Uuid::nil();
    let mut ack_status = 0u32;
    let mut data = format!("mock-session-{index}\n").into_bytes();
    let mut flood = 0;
    loop {
        while let Ok(a) = actions.try_recv() {
            match a {
                Action::Flood(n) => flood += n,
                Action::Human => {
                    flags &= !CONTROL;
                    epoch += 1;
                }
                Action::Close => return Ok(()),
            }
        }
        let mut f = Frame::new(if data.is_empty() { POLL } else { OUTPUT }, id);
        f.flags = flags;
        f.epoch = epoch;
        f.seq = seq;
        if data.is_empty() {
            f.data.extend_from_slice(client.as_bytes());
            f.data.extend_from_slice(&ack_status.to_le_bytes());
            f.request = ack;
        } else {
            f.data = std::mem::take(&mut data);
            seq += f.data.len() as u64;
        }
        let reply = exchange(&mut p, f).await?;
        match reply.kind {
            ACQUIRE => {
                client = Uuid::from_slice(&reply.data)?;
                flags |= CONTROL;
                ack_status = 1;
                ack = Uuid::nil();
            }
            COMMAND => {
                let cmd = u32::from_le_bytes(reply.data[16..20].try_into()?);
                ack = reply.request;
                ack_status = 1;
                let token = ack.simple().to_string();
                data = format!("\x1ePAI:{token}:BEGIN\x1f\nsynthetic command result {index}\n")
                    .into_bytes();
                if cmd != 4 {
                    data.extend_from_slice(format!("\x1ePAI:{token}:END:0\x1f\n").as_bytes());
                }
            }
            REVOKE => {
                flags &= !CONTROL;
                epoch += 1;
            }
            _ => {}
        }
        if flood > 0 && data.is_empty() {
            let n = flood.min(16384);
            data = vec![b'A' + index as u8; n];
            flood -= n;
        }
        if flood == 0 && data.is_empty() {
            sleep(Duration::from_millis(10)).await;
        }
    }
}
async fn until_count(p: &mut NamedPipeClient, n: usize) -> Result<Value> {
    for _ in 0..200 {
        let v = rpc(p, "list_sessions", json!({})).await?;
        if v["sessions"].as_array().unwrap().len() == n {
            return Ok(v);
        }
        sleep(Duration::from_millis(10)).await;
    }
    anyhow::bail!("session count did not reach {n}")
}
async fn acquire(p: &mut NamedPipeClient, id: Uuid) -> Result<()> {
    for _ in 0..100 {
        let v = rpc(
            p,
            "acquire_control",
            json!({"session_id":id,"requested_mode":"diagnostic"}),
        )
        .await?;
        if v["status"] == "granted" {
            return Ok(());
        }
        sleep(Duration::from_millis(10)).await;
    }
    anyhow::bail!("mock approval timed out")
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_windows_pipes_twenty_sessions_and_faults() -> Result<()> {
    ensure!(
        ClientOptions::new().open(pipe_name()?).is_err(),
        "a broker already exists; stop it before isolated tests"
    );
    let mut children = Children(vec![
        Command::new(env!("CARGO_BIN_EXE_putty-ai-broker"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    ]);
    let mut a = connect().await?;
    let mut b = connect().await?;
    // Real wire rejection: a native client cannot claim a different PID.
    let mut invalid = connect().await?;
    let mut bad = Frame::new(HELLO, Uuid::new_v4());
    bad.data
        .extend_from_slice(&(std::process::id() + 1).to_le_bytes());
    bad.data
        .extend_from_slice(&process_start(std::process::id())?.to_le_bytes());
    bad.data.extend_from_slice(&0u64.to_le_bytes());
    bad.write(&mut invalid).await?;
    ensure!(
        timeout(Duration::from_secs(2), Frame::read(&mut invalid))
            .await?
            .is_err(),
        "forged peer PID accepted"
    );
    let mut jobs = vec![];
    let mut controls = vec![];
    let mut ids = vec![];
    for i in 0..20 {
        let id = Uuid::new_v4();
        ids.push(id);
        let (tx, rx) = mpsc::channel(8);
        controls.push(tx);
        jobs.push(tokio::spawn(mock(id, i, rx)));
    }
    until_count(&mut a, 20).await?;
    sleep(Duration::from_millis(100)).await;
    for (i, id) in ids.iter().enumerate() {
        let v = rpc(
            &mut a,
            "read_terminal",
            json!({"session_id":id,"cursor":0,"max_bytes":8192}),
        )
        .await?;
        ensure!(
            v["output"]
                .as_str()
                .unwrap()
                .contains(&format!("mock-session-{i}")),
            "cross-session data"
        );
        let next = v["next_cursor"].clone();
        let empty = rpc(
            &mut b,
            "read_terminal",
            json!({"session_id":id,"cursor":next}),
        )
        .await?;
        ensure!(empty["output"] == "", "cursor duplicated output");
    }
    ensure!(
        rpc(
            &mut a,
            "read_terminal",
            json!({"session_id":Uuid::new_v4()})
        )
        .await
        .is_err(),
        "invalid identity accepted"
    );
    acquire(&mut a, ids[0]).await?;
    ensure!(
        rpc(
            &mut b,
            "acquire_control",
            json!({"session_id":ids[0],"requested_mode":"diagnostic"})
        )
        .await
        .is_err(),
        "lease not isolated"
    );
    let v=rpc(&mut a,"submit_command",json!({"session_id":ids[0],"command":"uname -a","execution_timeout_ms":5000,"request_id":"diagnostic-1"})).await?;
    let task = v["task_id"].clone();
    let mut completed = false;
    for _ in 0..100 {
        let v = rpc(&mut a, "get_task_status", json!({"task_id":task})).await?;
        if v["status"] == "completed" {
            ensure!(v["exit_code"] == 0, "wrong exit");
            completed = true;
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    ensure!(completed, "command not completed");
    controls[0].send(Action::Human).await?;
    sleep(Duration::from_millis(100)).await;
    ensure!(rpc(&mut a,"submit_command",json!({"session_id":ids[0],"command":"uptime","execution_timeout_ms":5000,"request_id":"after-human"})).await.is_err(),"human priority failed");
    acquire(&mut a, ids[1]).await?;
    let v=rpc(&mut a,"submit_command",json!({"session_id":ids[1],"command":"iostat -x 1 5","execution_timeout_ms":1000,"request_id":"timeout"})).await?;
    sleep(Duration::from_millis(1400)).await;
    let v = rpc(&mut a, "get_task_status", json!({"task_id":v["task_id"]})).await?;
    ensure!(
        v["status"] == "timeout" && v["exit_code"].is_null(),
        "timeout falsely completed"
    );
    let cpu_before = process_usage(children.0[0].id())?.0;
    let start = Instant::now();
    let mut drained = false;
    for c in &controls {
        c.send(Action::Flood(4 * 1024 * 1024)).await?;
    }
    for _ in 0..600 {
        let v = rpc(&mut a, "list_sessions", json!({})).await?;
        if v["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["received_bytes"].as_u64().unwrap() >= 4 * 1024 * 1024)
        {
            drained = true;
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    let seconds = start.elapsed().as_secs_f64();
    ensure!(
        drained,
        "flood did not fully reach broker before benchmark deadline"
    );
    let (cpu_after, peak_working_set) = process_usage(children.0[0].id())?;
    for (i, id) in ids.iter().enumerate() {
        let v = rpc(&mut a, "read_terminal", json!({"session_id":id,"cursor":0})).await?;
        ensure!(
            v["lost_bytes"].as_u64().unwrap() > 0,
            "overflow not disclosed"
        );
        ensure!(
            v["output"]
                .as_str()
                .unwrap()
                .bytes()
                .all(|b| b == b'A' + i as u8),
            "flood mixed sessions"
        );
    }
    println!(
        "PERF {}",
        json!({"sessions":20,"bytes":80*1024*1024,"seconds":seconds,"MiB_per_second":80.0/seconds,
            "broker_cpu_seconds":(cpu_after-cpu_before) as f64/1e7,"broker_peak_working_set_bytes":peak_working_set,
            "kind":"simulated_native_real_windows_named_pipe"})
    );
    controls[19].send(Action::Close).await?;
    until_count(&mut a, 19).await?;
    ensure!(
        rpc(&mut a, "read_terminal", json!({"session_id":ids[19]}))
            .await
            .is_err(),
        "closed ID usable"
    );
    // A client disconnect releases its lease without affecting other clients.
    acquire(&mut b, ids[2]).await?;
    drop(b);
    sleep(Duration::from_millis(100)).await;
    acquire(&mut a, ids[2]).await?;
    // A duplicate broker cannot replace the current pipe.
    let mut duplicate = Command::new(env!("CARGO_BIN_EXE_putty-ai-broker"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    for _ in 0..100 {
        if duplicate.try_wait()?.is_some() {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    ensure!(!duplicate.wait()?.success(), "singleton bypassed");
    // Real broker failure disconnects only sidecar clients. Mock frontends stay under test ownership.
    children.0[0].kill()?;
    children.0[0].wait()?;
    ensure!(
        rpc(&mut a, "list_sessions", json!({})).await.is_err(),
        "dead broker accepted RPC"
    );
    for job in jobs {
        let _ = timeout(Duration::from_secs(2), job).await;
    }
    children.0.push(
        Command::new(env!("CARGO_BIN_EXE_putty-ai-broker"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let mut new = connect().await?;
    until_count(&mut new, 0).await?;
    Ok(())
}
