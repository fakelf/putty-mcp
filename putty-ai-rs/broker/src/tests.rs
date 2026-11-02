use super::model::*;
use putty_ai_common::{protocol::*, terminal::OutputRing};
use serde_json::json;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use uuid::Uuid;

fn fixture() -> (Model, Uuid, Uuid, mpsc::Receiver<Frame>) {
    let mut m = Model::default();
    let id = Uuid::new_v4();
    let client = Uuid::new_v4();
    let (tx, rx) = mpsc::channel(8);
    m.sessions.insert(
        id,
        Session {
            generation: Uuid::new_v4(),
            pid: 1,
            start: 2,
            hwnd: 3,
            flags: READ | CONNECTED | CONTROL,
            epoch: 1,
            output: OutputRing::new(128),
            tx,
            lease: Some((client, Instant::now() + Duration::from_secs(60))),
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
    (m, id, client, rx)
}
fn submit(m: &mut Model, id: Uuid, c: Uuid) -> Uuid {
    let v=m.rpc(c,"submit_command",&json!({"session_id":id,"command":"uname -a","request_id":"one","execution_timeout_ms":1000})).unwrap();
    Uuid::parse_str(v["task_id"].as_str().unwrap()).unwrap()
}
#[test]
fn isolation_allowlist_and_idempotency() {
    let (mut m, id, c, _rx) = fixture();
    assert!(
        m.rpc(
            Uuid::new_v4(),
            "submit_command",
            &json!({"session_id":id,"command":"uname -a","request_id":"x"})
        )
        .is_err()
    );
    for cmd in [
        "uname -a; id",
        "uptime | sh",
        "df -h > /tmp/x",
        "sudo uname -a",
        "$(id)",
        "uname -a\n",
    ] {
        assert!(
            m.rpc(
                c,
                "submit_command",
                &json!({"session_id":id,"command":cmd,"request_id":"x"})
            )
            .is_err()
        );
    }
    let tid = submit(&mut m, id, c);
    assert_eq!(submit(&mut m, id, c), tid);
    assert!(
        m.rpc(
            c,
            "submit_command",
            &json!({"session_id":id,"command":"uptime","request_id":"two"})
        )
        .is_err()
    );
    assert!(
        m.rpc(Uuid::new_v4(), "get_task_status", &json!({"task_id":tid}))
            .is_err()
    );
    assert!(
        m.rpc(c, "read_terminal", &json!({"session_id":Uuid::new_v4()}))
            .is_err()
    );
}
#[test]
fn lease_expiry_and_local_revoke() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    m.sessions.get_mut(&id).unwrap().lease = Some((c, Instant::now() - Duration::from_secs(1)));
    m.expire();
    assert!(m.sessions[&id].lease.is_none());
    let mut f = Frame::new(POLL, id);
    f.flags = READ | CONNECTED;
    f.epoch = 2;
    m.native_state(id, &f).unwrap();
    assert_eq!(m.tasks[&tid].status, "unknown");
    assert!(m.sessions[&id].active.is_none());
}
#[test]
fn timeout_never_fakes_exit_or_unlocks_shell() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    let t = m.tasks.get_mut(&tid).unwrap();
    t.status = "running";
    t.deadline = Instant::now() - Duration::from_secs(1);
    m.expire();
    assert_eq!(m.tasks[&tid].status, "timeout");
    assert_eq!(m.tasks[&tid].exit, None);
    assert_eq!(m.sessions[&id].active, Some(tid));
}
#[test]
fn close_and_adapter_failure() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    m.client_gone(c);
    assert!(m.sessions[&id].lease.is_none());
    let generation = m.sessions[&id].generation;
    m.disconnect(id, generation);
    assert!(!m.sessions.contains_key(&id));
    assert_eq!(m.tasks[&tid].output.end, 0);
}
#[test]
fn incremental_marker_large_output_and_echo() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    let token = tid.simple().to_string();
    let mut f = Frame::new(OUTPUT, id);
    f.flags = READ | CONNECTED | CONTROL;
    f.epoch = 1;
    // Echo text alone is never an actual delimiter.
    f.data = format!("printf '\\036PAI:{token}:END:0\\037'\n").into_bytes();
    m.native_state(id, &f).unwrap();
    assert!(!m.tasks[&tid].complete);
    f.seq += f.data.len() as u64;
    f.data = format!(
        "\x1ePAI:{token}:BEGIN\x1f\n{}\x1ePAI:{token}:END:0\x1f\n",
        "x".repeat(16000)
    )
    .into_bytes();
    m.native_state(id, &f).unwrap();
    assert_eq!(m.tasks[&tid].status, "completed");
    assert_eq!(m.tasks[&tid].exit, Some(0));
}
#[test]
fn output_loss_is_unknown() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    let mut f = Frame::new(OUTPUT, id);
    f.flags = READ | CONNECTED | CONTROL;
    f.epoch = 1;
    f.seq = 20;
    f.data = b"gap".to_vec();
    m.native_state(id, &f).unwrap();
    assert_eq!(m.tasks[&tid].status, "unknown");
    assert_eq!(m.tasks[&tid].exit, None);
}
#[test]
fn cancellation_does_not_claim_remote_termination() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    let v = m.rpc(c, "cancel_task", &json!({"task_id":tid})).unwrap();
    assert_eq!(v["status"], "unknown");
    assert_eq!(v["remote_process_stopped"], false);
}
#[test]
fn output_permission_and_cursor_bounds() {
    let (mut m, id, c, _rx) = fixture();
    m.sessions.get_mut(&id).unwrap().flags = 0;
    assert!(
        m.rpc(c, "read_terminal", &json!({"session_id":id}))
            .is_err()
    );
    assert_eq!(
        m.rpc(c, "list_sessions", &json!({})).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn custom_mode_requires_separate_native_authorization() {
    let (mut m, id, c, mut rx) = fixture();
    assert!(
        m.rpc(
            c,
            "acquire_control",
            &json!({"session_id":id,"requested_mode":"custom"})
        )
        .is_err()
    );
    let s = m.sessions.get_mut(&id).unwrap();
    s.flags |= CUSTOM_CAPABLE;
    s.lease = None;
    let v = m
        .rpc(
            c,
            "acquire_control",
            &json!({"session_id":id,"requested_mode":"custom"}),
        )
        .unwrap();
    assert_eq!(v["status"], "pending_local_approval");
    assert_eq!(rx.try_recv().unwrap().kind, ACQUIRE_CUSTOM);
    assert!(
        m.rpc(
            c,
            "submit_command",
            &json!({"session_id":id,"command":"ip","request_id":"custom"})
        )
        .is_err()
    );
    let mut f = Frame::new(POLL, id);
    f.epoch = 1;
    f.flags = READ | CONNECTED | CUSTOM_CAPABLE | CONTROL | CUSTOM_CONTROL;
    f.data.extend_from_slice(c.as_bytes());
    f.data.extend_from_slice(&1u32.to_le_bytes());
    m.native_state(id, &f).unwrap();
    assert!(m.sessions[&id].lease_custom);
    assert!(
        m.rpc(
            c,
            "acquire_control",
            &json!({"session_id":id,"requested_mode":"diagnostic"})
        )
        .is_err()
    );
    let v=m.rpc(c,"submit_command",&json!({"session_id":id,"command":"ip -br addr; printf '%s' 'ok' | cat","request_id":"custom","execution_timeout_ms":600000})).unwrap();
    assert_eq!(v["status"], "queued");
    let frame = rx.try_recv().unwrap();
    assert_eq!(frame.kind, COMMAND_CUSTOM);
    assert_eq!(&frame.data[20..], b"ip -br addr; printf '%s' 'ok' | cat");
    assert_eq!(
        u32::from_le_bytes(frame.data[16..20].try_into().unwrap()),
        600000
    );
}

#[test]
fn custom_text_remains_bounded_and_rejects_terminal_controls() {
    let (mut m, id, c, _rx) = fixture();
    let s = m.sessions.get_mut(&id).unwrap();
    s.flags |= CUSTOM_CAPABLE | CUSTOM_CONTROL;
    s.lease_custom = true;
    for command in [
        String::new(),
        " ".into(),
        "x".repeat(2049),
        "\u{1b}[1mip".into(),
        "ip\rreboot".into(),
        "a\0b".into(),
        "ip\u{202e}".into(),
        "'".repeat(1500),
    ] {
        assert!(m.rpc(c,"submit_command",&json!({"session_id":id,"command":command,"request_id":"invalid","execution_timeout_ms":1000})).is_err());
    }
}

#[test]
fn lease_expiry_does_not_discard_running_task_observation() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    m.tasks.get_mut(&tid).unwrap().status = "running";
    m.sessions.get_mut(&id).unwrap().lease = Some((c, Instant::now() - Duration::from_secs(1)));
    m.expire();
    let mut f = Frame::new(POLL, id);
    f.epoch = 1;
    f.flags = READ | CONNECTED;
    m.native_state(id, &f).unwrap();
    assert_eq!(m.tasks[&tid].status, "running");
    assert!(m.sessions[&id].lease.is_none());
}

#[test]
fn rejected_control_ack_clears_pending_client_and_deadline() {
    let (mut m, id, c, mut rx) = fixture();
    m.sessions.get_mut(&id).unwrap().lease = None;
    m.rpc(
        c,
        "acquire_control",
        &json!({"session_id":id,"requested_mode":"diagnostic"}),
    )
    .unwrap();
    assert_eq!(rx.try_recv().unwrap().kind, ACQUIRE);
    let mut rejected = Frame::new(POLL, id);
    rejected.epoch = 1;
    rejected.flags = READ | CONNECTED;
    rejected.data.extend_from_slice(c.as_bytes());
    rejected.data.extend_from_slice(&2u32.to_le_bytes());
    m.native_state(id, &rejected).unwrap();
    assert!(m.sessions[&id].pending.is_none());
    assert!(m.sessions[&id].pending_deadline.is_none());
    let denied = m
        .rpc(
            c,
            "acquire_control",
            &json!({"session_id":id,"requested_mode":"diagnostic"}),
        )
        .unwrap();
    assert_eq!(denied["status"], "rejected_locally");
    assert!(rx.try_recv().is_err());
    // An explicitly initiated new request must not be stuck behind stale pending state.
    let next = Uuid::new_v4();
    m.rpc(
        next,
        "acquire_control",
        &json!({"session_id":id,"requested_mode":"diagnostic"}),
    )
    .unwrap();
    assert_eq!(m.sessions[&id].pending, Some(next));
}
