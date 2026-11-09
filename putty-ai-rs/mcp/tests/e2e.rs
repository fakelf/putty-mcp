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
async fn adapter() -> Result<NamedPipeClient> {
    let mut pipe = connect().await?;
    let mut hello = Frame::new(ADAPTER_HELLO, Uuid::nil());
    hello.data = b"Pipe integration test".to_vec();
    ensure!(
        exchange(&mut pipe, hello).await?.kind == REPLY,
        "adapter registration failed"
    );
    Ok(pipe)
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
    let mut ack = Uuid::nil();
    let mut ack_status = 0u32;
    let mut command_offset = 0u64;
    let prompt = format!("device-{index}> ");
    let mut data = format!("mock-session-{index}\r\n{prompt}").into_bytes();
    let mut flood = 0;
    loop {
        while let Ok(a) = actions.try_recv() {
            match a {
                Action::Flood(n) => flood += n,
                Action::Close => return Ok(()),
            }
        }
        let mut f = Frame::new(if data.is_empty() { POLL } else { OUTPUT }, id);
        f.flags = READ | CONNECTED;
        f.epoch = 1;
        f.seq = seq;
        if data.is_empty() {
            let status = std::mem::take(&mut ack_status);
            f.data.extend_from_slice(&status.to_le_bytes());
            f.data.extend_from_slice(&0u64.to_le_bytes());
            f.request = std::mem::take(&mut ack);
            if status == COMMAND_SENT {
                f.data.extend_from_slice(&command_offset.to_le_bytes());
                f.data.extend_from_slice(prompt.as_bytes());
            }
        } else {
            f.data = std::mem::take(&mut data);
            seq += f.data.len() as u64;
        }
        let reply = exchange(&mut p, f).await?;
        if reply.kind == COMMAND {
            let cmd = std::str::from_utf8(&reply.data[17 + reply.data[16] as usize..])?;
            ack = reply.request;
            ack_status = COMMAND_SENT;
            command_offset = seq;
            // The real bridge can drain captured output before polling its acknowledgement.
            data = format!("{cmd}\r\nsynthetic command result {index}\r\n").into_bytes();
            if cmd != "synthetic-long-task" {
                data.extend_from_slice(prompt.as_bytes());
            }
        }
        if flood > 0 && data.is_empty() {
            let n = flood.min(16384);
            data = vec![b'A' + index as u8; n];
            // Keep the flood printable, including when a page begins mid-line.
            // Unbounded lines are intentionally redacted by the output layer.
            data[n - 1] = b'\n';
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
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_windows_pipes_twenty_sessions_and_faults() -> Result<()> {
    ensure!(
        ClientOptions::new().open(pipe_name()?).is_err(),
        "a broker already exists; stop it before isolated tests"
    );
    let mut children = Children(vec![
        Command::new(env!("CARGO_BIN_EXE_putty-ai-mcp"))
            .arg("--broker")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    ]);
    let mut a = adapter().await?;
    let mut b = adapter().await?;
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
    let v=rpc(&mut a,"submit_command",json!({"session_id":ids[0],"command":"uname -a","execution_timeout_ms":5000,"request_id":"script-1"})).await?;
    let task = v["task_id"].clone();
    ensure!(
        rpc(&mut b, "get_task_status", json!({"task_id":task}))
            .await
            .is_err(),
        "task visible to another adapter"
    );
    let mut completed = false;
    for _ in 0..100 {
        let v = rpc(&mut a, "get_task_status", json!({"task_id":task})).await?;
        if v["status"] == "completed" {
            ensure!(
                v["completion"] == "prompt_observed" && v["exit_code"].is_null(),
                "terminal prompt completion reported an exit code or wrong reason"
            );
            ensure!(
                v["output"]
                    .as_str()
                    .is_some_and(|output| output.contains("synthetic command result 0")),
                "terminal command output missing"
            );
            completed = true;
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    ensure!(completed, "command not completed");
    let v=rpc(&mut a,"submit_command",json!({"session_id":ids[1],"command":"synthetic-long-task","execution_timeout_ms":1000,"request_id":"timeout"})).await?;
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
        let output = v["output"].as_str().unwrap();
        ensure!(
            output.len() == putty_ai_common::OUTPUT_LIMIT / 8
                && output.bytes().all(|b| b == b'A' + i as u8 || b == b'\n'),
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
    drop(b);
    // A duplicate broker cannot replace the current pipe.
    let mut duplicate = Command::new(env!("CARGO_BIN_EXE_putty-ai-mcp"))
        .arg("--broker")
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
        Command::new(env!("CARGO_BIN_EXE_putty-ai-mcp"))
            .arg("--broker")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let mut new = adapter().await?;
    until_count(&mut new, 0).await?;
    Ok(())
}
