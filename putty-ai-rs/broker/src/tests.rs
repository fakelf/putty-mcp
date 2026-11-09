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
            flags: READ | CONNECTED,
            epoch: 1,
            output: OutputRing::new(128),
            tx,
            active: None,
            lifecycle: None,
            parent: None,
            received: 0,
            last_seen: Instant::now(),
            lost_total: 0,
            native_dropped: 0,
        },
    );
    (m, id, client, rx)
}
fn submit(m: &mut Model, id: Uuid, c: Uuid) -> Uuid {
    let v=m.rpc(c, "Test host", "submit_command",&json!({"session_id":id,"command":"uname -a","request_id":"one","execution_timeout_ms":1000})).unwrap();
    Uuid::parse_str(v["task_id"].as_str().unwrap()).unwrap()
}
#[test]
fn isolation_and_idempotency() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    assert_eq!(submit(&mut m, id, c), tid);
    assert!(
        m.rpc(
            c,
            "Test host",
            "submit_command",
            &json!({"session_id":id,"command":"uptime","request_id":"two"})
        )
        .is_err()
    );
    assert!(
        m.rpc(
            Uuid::new_v4(),
            "Test host",
            "get_task_status",
            &json!({"task_id":tid})
        )
        .is_err()
    );
    assert!(
        m.rpc(
            c,
            "Test host",
            "read_terminal",
            &json!({"session_id":Uuid::new_v4()})
        )
        .is_err()
    );
}
#[test]
fn approval_has_no_lease_or_execution_deadline() {
    let (mut m, id, c, mut rx) = fixture();
    let tid = submit(&mut m, id, c);
    let f = rx.try_recv().unwrap();
    assert_eq!(f.kind, COMMAND);
    assert_eq!(f.request, tid);
    assert_eq!(&f.data[..16], c.as_bytes());
    let end = 17 + f.data[16] as usize;
    assert_eq!(&f.data[17..end], b"Test host");
    assert_eq!(&f.data[end..], b"uname -a");
    m.tasks.get_mut(&tid).unwrap().created -= Duration::from_secs(120);
    m.expire();
    assert_eq!(m.tasks[&tid].status, "queued");
    assert!(m.tasks[&tid].deadline.is_none());
    let event = m
        .native_state(id, &command_sent(id, tid, 1, 0, ""))
        .unwrap()
        .unwrap();
    assert_eq!(event["source"], "Test host");
    assert_eq!(event["decision"], "sent");
    assert_eq!(m.tasks[&tid].status, "running");
    assert!(m.tasks[&tid].deadline.unwrap() > Instant::now());
}
fn decision(session: Uuid, task: Uuid, epoch: u64, ack: u32) -> Frame {
    let mut f = Frame::new(POLL, session);
    f.request = task;
    f.epoch = epoch;
    f.flags = READ | CONNECTED;
    f.data.extend_from_slice(&ack.to_le_bytes());
    f.data.extend_from_slice(&0u64.to_le_bytes());
    f
}

fn command_sent(session: Uuid, task: Uuid, epoch: u64, offset: u64, prompt: &str) -> Frame {
    let mut f = decision(session, task, epoch, COMMAND_SENT);
    f.data.extend_from_slice(&offset.to_le_bytes());
    f.data.extend_from_slice(prompt.as_bytes());
    f
}
#[test]
fn timeout_never_fakes_exit_or_unlocks_shell() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    m.native_state(id, &command_sent(id, tid, 1, 0, ""))
        .unwrap();
    output(&mut m, id, b"some output\r\nhost$ ");
    poll_prompt(&mut m, id, tid);
    m.tasks.get_mut(&tid).unwrap().deadline = Some(Instant::now());
    m.expire();
    let result = command_status(&mut m, c, tid);
    assert_eq!(result["status"], "timeout");
    assert!(result["completion"].is_null());
    assert!(result["exit_code"].is_null());
    assert_eq!(result["output"], "some output\nhost$ ");
    assert_eq!(m.sessions[&id].active, Some(tid));
}

#[test]
fn timeout_recovers_when_the_terminal_later_returns_the_exact_prompt() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    m.native_state(id, &command_sent(id, tid, 1, 0, "host$ "))
        .unwrap();
    output(&mut m, id, b"waiting\n");
    m.tasks.get_mut(&tid).unwrap().deadline = Some(Instant::now());
    m.expire();
    assert_eq!(m.tasks[&tid].status, "timeout");
    assert_eq!(m.sessions[&id].active, Some(tid));

    output(&mut m, id, b"done\nhost$ ");
    poll_prompt(&mut m, id, tid);
    let result = command_status(&mut m, c, tid);
    assert_eq!(result["status"], "completed");
    assert_eq!(result["completion"], "prompt_observed");
    assert!(result["exit_code"].is_null());
    assert_eq!(result["output"], "waiting\ndone\nhost$ ");
    assert!(result["error"].is_null());
    assert!(m.sessions[&id].active.is_none());
}

#[test]
fn a_gap_prevents_joining_prompt_fragments_and_discloses_lost_bytes() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    m.native_state(id, &command_sent(id, tid, 1, 0, "router#"))
        .unwrap();
    output(&mut m, id, b"start\nrouter");
    let mut after_gap = Frame::new(OUTPUT, id);
    after_gap.flags = READ | CONNECTED;
    after_gap.epoch = 1;
    after_gap.seq = m.sessions[&id].output.end + 7;
    after_gap.data = b"#".to_vec();
    m.native_state(id, &after_gap).unwrap();
    poll_prompt(&mut m, id, tid);
    assert_eq!(m.tasks[&tid].status, "unknown");
    assert_eq!(m.sessions[&id].active, Some(tid));

    output(&mut m, id, b"\r\nrouter# ");
    poll_prompt(&mut m, id, tid);
    let result = command_status(&mut m, c, tid);
    assert_eq!(result["status"], "completed");
    assert!(result["exit_code"].is_null());
    assert_eq!(result["output_truncated"], true);
    assert_eq!(result["lost_bytes"], 7);
    assert!(m.sessions[&id].active.is_none());
}

#[test]
fn manual_takeover_freezes_old_output_and_does_not_attribute_new_prompt() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    m.native_state(id, &command_sent(id, tid, 1, 0, "host$"))
        .unwrap();
    output(&mut m, id, b"Vim active\n");
    m.native_state(id, &decision(id, Uuid::nil(), 2, 0))
        .unwrap();
    assert_eq!(m.tasks[&tid].status, "unknown");
    assert!(m.sessions[&id].active.is_none());

    let next = m
        .rpc(
            c,
            "Test host",
            "submit_command",
            &json!({"session_id":id,"command":"true","request_id":"next"}),
        )
        .unwrap();
    let next_id = Uuid::parse_str(next["task_id"].as_str().unwrap()).unwrap();
    output(&mut m, id, b"user interaction\nhost$ ");
    poll_prompt(&mut m, id, tid);
    for view in ["command", "transcript"] {
        let result = m
            .rpc(
                c,
                "Test host",
                "get_task_status",
                &json!({"task_id":tid,"output_view":view}),
            )
            .unwrap();
        assert_eq!(result["status"], "unknown");
        assert!(result["completion"].is_null());
        assert!(result["exit_code"].is_null());
        assert_eq!(result["output"], "Vim active\n");
        assert_eq!(result["output_truncated"], true);
    }
    assert_eq!(m.sessions[&id].active, Some(next_id));
}

#[test]
fn close_and_adapter_failure() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    m.client_gone(c);
    assert_eq!(m.tasks[&tid].status, "unknown");
    let generation = m.sessions[&id].generation;
    m.disconnect(id, generation);
    assert!(!m.sessions.contains_key(&id));
    assert_eq!(m.tasks[&tid].output.end, 0);
}

fn output(m: &mut Model, session: Uuid, bytes: &[u8]) {
    let mut f = Frame::new(OUTPUT, session);
    f.flags = m.sessions[&session].flags;
    f.epoch = m.sessions[&session].epoch;
    f.seq = m.sessions[&session].output.end;
    f.data = bytes.to_vec();
    m.native_state(session, &f).unwrap();
}

fn command_status(m: &mut Model, client: Uuid, task: Uuid) -> serde_json::Value {
    m.rpc(
        client,
        "Test host",
        "get_task_status",
        &json!({"task_id":task}),
    )
    .unwrap()
}

fn poll_prompt(m: &mut Model, session: Uuid, task: Uuid) {
    if let Some(since) = &mut m.tasks.get_mut(&task).unwrap().prompt_candidate {
        *since -= Duration::from_millis(100);
    }
    let mut poll = decision(session, Uuid::nil(), m.sessions[&session].epoch, 0);
    poll.flags = m.sessions[&session].flags;
    poll.seq = m.sessions[&session].output.end;
    m.native_state(session, &poll).unwrap();
}

#[test]
fn output_before_ack_is_cropped_at_the_exact_native_send_offset() {
    let (mut m, id, c, _rx) = fixture();
    output(&mut m, id, b"login banner\n");
    let tid = submit(&mut m, id, c);
    let prior = b"unrelated output\r\nrouter# ";
    let offset = m.sessions[&id].output.end + prior.len() as u64;
    let result = format!(
        "display current-configuration\r\n{}\r\n<branch-switch-lab> ",
        "x".repeat(16000)
    );
    output(&mut m, id, &[prior.as_slice(), result.as_bytes()].concat());
    let pending = command_status(&mut m, c, tid);
    assert_eq!(pending["status"], "queued");
    assert_eq!(pending["output"], "");
    m.native_state(
        id,
        &command_sent(id, tid, 1, offset, "<branch-switch-lab>"),
    )
    .unwrap();
    assert_eq!(m.tasks[&tid].status, "running");
    poll_prompt(&mut m, id, tid);
    let first = command_status(&mut m, c, tid);
    assert_eq!(first["command_start_offset"], offset);
    assert_eq!(first["completion"], "prompt_observed");
    let text = first["output"].as_str().unwrap();
    assert!(text.starts_with("display current-configuration\n"));
    assert!(!text.contains("unrelated output"));
    assert!(first["exit_code"].is_null());
}

#[test]
fn ack_before_late_presend_bytes_keeps_output_boundary_and_crlf_paging() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    let before = b"old> ";
    m.native_state(id, &command_sent(id, tid, 1, before.len() as u64, "new>"))
        .unwrap();
    assert_eq!(command_status(&mut m, c, tid)["output"], "");
    output(&mut m, id, &[before.as_slice(), b"abc\r\ndef"].concat());
    let first = m
        .rpc(
            c,
            "Test host",
            "get_task_status",
            &json!({"task_id":tid,"max_output_bytes":4}),
        )
        .unwrap();
    assert_eq!(first["output"], "abc");
    assert_eq!(first["output_truncated"], true);
    let next = m
        .rpc(
            c,
            "Test host",
            "get_task_status",
            &json!({"task_id":tid,"output_cursor":first["next_cursor"]}),
        )
        .unwrap();
    assert_eq!(next["output"], "\ndef");
    assert_eq!(next["status"], "running");
    output(&mut m, id, b"\r\nnew> ");
    poll_prompt(&mut m, id, tid);
    let done = command_status(&mut m, c, tid);
    assert_eq!(done["output"], "abc\ndef\nnew> ");
    assert!(done["exit_code"].is_null());
    let transcript = m
        .rpc(
            c,
            "Test host",
            "get_task_status",
            &json!({"task_id":tid,"output_view":"transcript"}),
        )
        .unwrap();
    assert!(!transcript["output"].as_str().unwrap().contains("old> "));
}

#[test]
fn literal_override_requires_a_stable_whole_open_prompt_line() {
    let (mut m, id, c, _rx) = fixture();
    let args = json!({"session_id":id,"command":"system-view","request_id":"switch","expected_prompt":" [router] "});
    let result = m.rpc(c, "Test host", "submit_command", &args).unwrap();
    let tid = Uuid::parse_str(result["task_id"].as_str().unwrap()).unwrap();
    let mut other = args.clone();
    other["expected_prompt"] = json!("router>");
    assert!(m.rpc(c, "Test host", "submit_command", &other).is_err());
    m.native_state(id, &command_sent(id, tid, 1, 0, "router>"))
        .unwrap();
    output(&mut m, id, b"system-view\r\nrouter> ");
    poll_prompt(&mut m, id, tid);
    assert_eq!(m.tasks[&tid].status, "running");
    output(&mut m, id, b"\r\nprefix [router]");
    poll_prompt(&mut m, id, tid);
    assert_eq!(m.tasks[&tid].status, "running");
    output(&mut m, id, b"\r\n[router]");
    assert_eq!(m.tasks[&tid].status, "running");
    output(&mut m, id, b" show history\r\n");
    poll_prompt(&mut m, id, tid);
    assert_eq!(m.tasks[&tid].status, "running");
    output(&mut m, id, b"\x1b[?2004h\x1b[31m[rou");
    output(&mut m, id, b"ter]\x1b[0m ");
    assert_eq!(m.tasks[&tid].status, "running");
    poll_prompt(&mut m, id, tid);
    let result = command_status(&mut m, c, tid);
    assert_eq!(result["completion"], "prompt_observed");
    assert!(result["exit_code"].is_null());
}

#[test]
fn interrupt_prompt_is_correlated_with_sent_key_and_has_no_exit_code() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    m.native_state(id, &command_sent(id, tid, 1, 0, "user@host:~$"))
        .unwrap();
    output(&mut m, id, b"^C\r\nnot the expected prompt# ");
    m.rpc(c, "Test host", "interrupt_task", &json!({"task_id":tid}))
        .unwrap();
    assert_eq!(m.interrupt_commit(id, tid).kind, SEND_INTERRUPT);
    let sent_at = m.sessions[&id].output.end;
    let mut sent = decision(id, tid, 1, INTERRUPT_SENT);
    sent.data.extend_from_slice(&sent_at.to_le_bytes());
    m.native_state(id, &sent).unwrap();
    assert_eq!(m.tasks[&tid].status, "interrupting");
    assert_eq!(m.sessions[&id].active, Some(tid));
    let prompt = b"^C\r\n\x1b[?2004huser@host:~$ ";
    output(&mut m, id, &prompt[..7]);
    output(&mut m, id, &prompt[7..]);
    poll_prompt(&mut m, id, tid);
    let result = command_status(&mut m, c, tid);
    assert_eq!(result["interrupt_status"], "sent");
    assert_eq!(result["status"], "completed");
    assert_eq!(result["shell_prompt_observed"], true);
    assert!(result["exit_code"].is_null());
    assert_eq!(result["remote_process_stopped"], false);
    assert!(m.sessions[&id].active.is_none());
}

#[test]
fn unconfirmed_interrupt_leaves_running_and_retains_session_lock() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    m.native_state(id, &command_sent(id, tid, 1, 0, "router#"))
        .unwrap();
    m.rpc(c, "Test host", "interrupt_task", &json!({"task_id":tid}))
        .unwrap();
    assert_eq!(m.interrupt_commit(id, tid).kind, SEND_INTERRUPT);
    m.native_state(id, &decision(id, tid, 1, INTERRUPT_SENT))
        .unwrap();
    m.tasks.get_mut(&tid).unwrap().deadline = Some(Instant::now());
    m.expire();
    let result = command_status(&mut m, c, tid);
    assert_eq!(result["status"], "unknown");
    assert!(result["exit_code"].is_null());
    assert_eq!(m.sessions[&id].active, Some(tid));
}

#[test]
fn output_loss_in_a_poll_is_reported_even_without_further_output() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    m.native_state(id, &command_sent(id, tid, 1, 0, "router#"))
        .unwrap();
    let mut dropped = decision(id, Uuid::nil(), 1, 0);
    dropped.seq = 20;
    m.native_state(id, &dropped).unwrap();
    let result = command_status(&mut m, c, tid);
    assert_eq!(result["status"], "unknown");
    assert_eq!(result["lost_bytes"], 20);
    assert_eq!(result["missing_ranges"], json!([[0, 20]]));
    assert!(result["exit_code"].is_null());
}
#[test]
fn cancellation_does_not_claim_remote_termination() {
    let (mut m, id, c, _rx) = fixture();
    let tid = submit(&mut m, id, c);
    let v = m
        .rpc(c, "Test host", "cancel_task", &json!({"task_id":tid}))
        .unwrap();
    assert_eq!(v["status"], "unknown");
    assert_eq!(v["remote_process_stopped"], false);
}
#[test]
fn output_permission_and_cursor_bounds() {
    let (mut m, id, c, _rx) = fixture();
    m.sessions.get_mut(&id).unwrap().flags = 0;
    assert!(
        m.rpc(c, "Test host", "read_terminal", &json!({"session_id":id}))
            .is_err()
    );
    assert_eq!(
        m.rpc(c, "Test host", "list_sessions", &json!({})).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn custom_text_remains_bounded_and_rejects_terminal_controls() {
    let (mut m, id, c, mut rx) = fixture();
    for command in [
        String::new(),
        " ".into(),
        "x".repeat(2049),
        "\u{1b}[1mip".into(),
        "ip\rreboot".into(),
        "a\0b".into(),
        "ip\u{202e}".into(),
    ] {
        assert!(m.rpc(c, "Test host", "submit_command",&json!({"session_id":id,"command":command,"request_id":"invalid","execution_timeout_ms":1000})).is_err());
    }
    for prompt in ["", " ", "a\rb", "a\tb", "a\n", "\u{202e}", &"测".repeat(86)] {
        assert!(m.rpc(c, "Test host", "submit_command", &json!({"session_id":id,"command":"display version","request_id":"invalid","expected_prompt":prompt})).is_err());
    }
    let command = format!("description {}\n\tdisplay version", "'".repeat(1500));
    m.rpc(
        c,
        "Test host",
        "submit_command",
        &json!({"session_id":id,"command":command,"request_id":"direct"}),
    )
    .unwrap();
    let frame = rx.try_recv().unwrap();
    assert_eq!(
        &frame.data[17 + frame.data[16] as usize..],
        command.as_bytes()
    );
}

#[test]
fn rejection_and_epoch_change_leave_no_reusable_approval() {
    let (mut m, id, c, mut rx) = fixture();
    let tid = submit(&mut m, id, c);
    rx.try_recv().unwrap();
    m.native_state(id, &decision(id, Uuid::nil(), 2, 0))
        .unwrap();
    assert_eq!(m.tasks[&tid].status, "unknown");
    assert_eq!(m.sessions[&id].active, Some(tid));
    m.native_state(id, &decision(id, tid, 2, 2)).unwrap();
    assert_eq!(m.tasks[&tid].status, "cancelled");
    assert!(m.sessions[&id].active.is_none());
    assert_eq!(submit(&mut m, id, c), tid);
    assert!(
        rx.try_recv().is_err(),
        "same request must never reopen the dialog"
    );
    let other = Uuid::new_v4();
    let next = submit(&mut m, id, other);
    assert_ne!(next, tid);
    assert_eq!(rx.try_recv().unwrap().kind, COMMAND);
}

#[test]
fn interactive_top_binds_frames_and_actions_to_one_session() {
    let (mut m, first, client, mut first_rx) = fixture();
    let (second_model, second, _, mut second_rx) = fixture();
    m.sessions.extend(second_model.sessions);
    for id in [first, second] {
        m.sessions.get_mut(&id).unwrap().flags |= TOP_CAPABLE;
    }
    let start = |m: &mut Model, session| {
        let task = m
            .rpc(
                client,
                "Test host",
                "start_interactive",
                &json!({"session_id":session,"program":"top","request_id":"top"}),
            )
            .unwrap();
        Uuid::parse_str(task["task_id"].as_str().unwrap()).unwrap()
    };
    let a = start(&mut m, first);
    let b = start(&mut m, second);
    assert_ne!(a, b);
    assert_eq!(first_rx.try_recv().unwrap().kind, TOP_START);
    assert_eq!(second_rx.try_recv().unwrap().kind, TOP_START);
    for (session, id) in [(first, a), (second, b)] {
        let mut ack = decision(session, id, 1, TOP_STARTED);
        ack.flags |= TOP_CAPABLE;
        m.native_state(session, &ack).unwrap();
        let mut event = Frame::new(TOP_EVENT, session);
        event.request = id;
        event.flags = READ | CONNECTED | TOP_CAPABLE;
        event.epoch = 1;
        event.data = vec![2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        m.native_state(session, &event).unwrap();
    }
    let mut screen = Frame::new(TOP_SCREEN, second);
    screen.request = a;
    screen.flags = READ | CONNECTED | TOP_CAPABLE;
    screen.epoch = 1;
    screen.seq = 1;
    screen.data = b"\0\0\0wrong window".to_vec();
    m.native_state(second, &screen).unwrap();
    let read = m
        .rpc(
            client,
            "Test host",
            "read_task_screen",
            &json!({"task_id":a}),
        )
        .unwrap();
    assert!(read["frame"].is_null());

    for action in ["undo", "redo", "insert_mode", "discard_and_quit"] {
        let error = m
            .rpc(
                client,
                "Test host",
                "request_task_action",
                &json!({"task_id":a,"action":action,"request_id":action}),
            )
            .unwrap_err();
        assert!(error.to_string().contains("only available for vi"));
    }
    assert!(first_rx.try_recv().is_err());

    m.rpc(
        client,
        "Test host",
        "request_task_action",
        &json!({"task_id":a,"action":"quit"}),
    )
    .unwrap();
    assert_eq!(first_rx.try_recv().unwrap().kind, TOP_QUIT);
    assert!(second_rx.try_recv().is_err());
    assert_eq!(m.top_key_commit(second, a).kind, REVOKE);
    assert_eq!(m.top_key_commit(first, a).kind, TOP_SEND_KEY);
    assert_eq!(m.top_key_commit(first, a).kind, REVOKE);
}

fn running_interactive(
    program: &str,
    start_kind: u16,
) -> (Model, Uuid, Uuid, Uuid, mpsc::Receiver<Frame>) {
    let (mut m, session, client, mut frames) = fixture();
    m.sessions.get_mut(&session).unwrap().flags |= TOP_CAPABLE;
    let task = m
        .rpc(
            client,
            "Test host",
            "start_interactive",
            &json!({"session_id":session,"program":program,"request_id":"start"}),
        )
        .unwrap();
    let id = Uuid::parse_str(task["task_id"].as_str().unwrap()).unwrap();
    assert_eq!(frames.try_recv().unwrap().kind, start_kind);
    assert!(task["path"].is_null());
    mark_interactive_running(&mut m, session, id);
    (m, session, client, id, frames)
}

fn mark_interactive_running(m: &mut Model, session: Uuid, id: Uuid) {
    let mut ack = decision(session, id, 1, TOP_STARTED);
    ack.flags |= TOP_CAPABLE;
    m.native_state(session, &ack).unwrap();
    let mut running = Frame::new(TOP_EVENT, session);
    running.request = id;
    running.flags = READ | CONNECTED | TOP_CAPABLE;
    running.epoch = 1;
    running.data = vec![2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    m.native_state(session, &running).unwrap();
}

#[test]
fn vi_editing_keys_use_stable_wire_codes_and_one_approval_each() {
    let (mut m, session, client, id, mut frames) = running_interactive("vi", VI_START);
    for (name, code) in [
        ("backspace", 18),
        ("delete", 19),
        ("up", 20),
        ("down", 21),
        ("left", 22),
        ("right", 23),
        ("home", 24),
        ("end", 25),
        ("page_up", 26),
        ("page_down", 27),
        ("insert", 28),
        ("tab", 29),
        ("shift_tab", 30),
        ("enter", 31),
        ("undo", 32),
        ("redo", 33),
        ("insert_mode", 34),
        ("discard_and_quit", 35),
    ] {
        let args = json!({"task_id":id,"action":name,"request_id":name});
        let pending = m
            .rpc(client, "Test host", "request_task_action", &args)
            .unwrap();
        assert_eq!(pending["key"], name);
        assert_eq!(pending["key_status"], "pending_approval");
        let key = frames.try_recv().unwrap();
        assert_eq!((key.kind, key.flags, key.request), (SPECIAL_KEY, code, id));
        assert_eq!(m.top_key_commit(session, id).kind, TOP_SEND_KEY);

        let mut sent = decision(session, id, 1, TOP_KEY_SENT);
        sent.flags |= TOP_CAPABLE;
        m.native_state(session, &sent).unwrap();
        let repeat = m
            .rpc(client, "Test host", "request_task_action", &args)
            .unwrap();
        assert_eq!(repeat["key_status"], "sent");
        assert!(frames.try_recv().is_err());
    }
}

#[test]
fn vi_insert_text_is_literal_approved_once_and_omitted_from_status() {
    let (mut m, session, client, id, mut frames) = running_interactive("vi", VI_START);
    let content = ":qa! 测试\n\tשלום 🙂";
    let args = json!({"task_id":id,"action":"insert_text","request_id":"paste","text":content});
    let pending = m
        .rpc(client, "Test host", "request_task_action", &args)
        .unwrap();
    assert_eq!(pending["key_request_id"], "paste");
    assert_eq!(pending["key"], "insert_text");
    assert_eq!(pending["key_status"], "pending_approval");

    let frame = frames.try_recv().unwrap();
    assert_eq!(frame.kind, VI_INSERT_TEXT);
    assert_eq!(frame.flags, 0);
    assert_eq!(frame.session, session);
    assert_eq!(frame.request, id);
    assert_eq!(frame.epoch, 1);
    let mut expected = client.as_bytes().to_vec();
    expected.extend_from_slice(b"\x09Test hostexec vi -Nu NONE -i NONE -n -R -Z -c 'set encoding=utf-8 backspace=indent,eol,start'");
    expected.extend_from_slice(content.as_bytes());
    assert_eq!(frame.data, expected);

    let repeat = m
        .rpc(client, "Test host", "request_task_action", &args)
        .unwrap();
    assert_eq!(repeat, pending);
    let mut different = args.clone();
    different["text"] = json!("different text");
    let error = m
        .rpc(client, "Test host", "request_task_action", &different)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("reused with a different key or text")
    );
    let different_key = json!({"task_id":id,"action":"escape","request_id":"paste"});
    assert!(
        m.rpc(client, "Test host", "request_task_action", &different_key)
            .is_err()
    );
    assert!(frames.try_recv().is_err());

    assert_eq!(m.top_key_commit(session, id).kind, TOP_SEND_KEY);
    assert_eq!(m.top_key_commit(session, id).kind, REVOKE);
    let mut sent = decision(session, id, 1, TOP_KEY_SENT);
    sent.flags |= TOP_CAPABLE;
    m.native_state(session, &sent).unwrap();
    let status = m
        .rpc(
            client,
            "Test host",
            "get_task_status",
            &json!({"task_id":id}),
        )
        .unwrap();
    assert_eq!(status["key_status"], "sent");
    assert!(status.get("text").is_none());
    assert!(!status.to_string().contains(":qa!"));
    let repeat = m
        .rpc(client, "Test host", "request_task_action", &args)
        .unwrap();
    assert_eq!(repeat, status);
    assert!(frames.try_recv().is_err());

    let max_length = format!("{}a", "测".repeat(341));
    let boundary =
        json!({"task_id":id,"action":"insert_text","request_id":"boundary","text":max_length});
    m.rpc(client, "Test host", "request_task_action", &boundary)
        .unwrap();
    assert!(
        frames
            .try_recv()
            .unwrap()
            .data
            .ends_with(max_length.as_bytes())
    );
    let repeat = m
        .rpc(client, "Test host", "request_task_action", &args)
        .unwrap();
    assert_eq!(repeat, status);
    assert!(frames.try_recv().is_err());
}

#[test]
fn vi_insert_text_rejects_unsafe_text_and_other_tasks() {
    let (mut m, _, client, id, mut frames) = running_interactive("vi", VI_START);
    let oversized = "测".repeat(342);
    for content in ["", "\x1b", "hi\x1b[201~", "a\rb", "\u{009b}", &oversized] {
        let error = m
            .rpc(
                client,
                "Test host",
                "request_task_action",
                &json!({"task_id":id,"action":"insert_text","request_id":"rejected","text":content}),
            )
            .unwrap_err();
        assert!(error.to_string().contains("1..1024 UTF-8 bytes"));
    }
    let error = m
        .rpc(
            Uuid::new_v4(),
            "Test host",
            "request_task_action",
            &json!({"task_id":id,"action":"insert_text","request_id":"foreign","text":"hello"}),
        )
        .unwrap_err();
    assert!(error.to_string().contains("another MCP client"));
    assert!(frames.try_recv().is_err());

    let (mut m, _, client, id, mut frames) = running_interactive("top", TOP_START);
    let error = m
        .rpc(
            client,
            "Test host",
            "request_task_action",
            &json!({"task_id":id,"action":"insert_text","request_id":"top-paste","text":"hello"}),
        )
        .unwrap_err();
    assert!(error.to_string().contains("only available for vi"));
    assert!(frames.try_recv().is_err());
}

#[test]
fn vi_file_binds_start_and_approved_actions_to_exact_path() {
    let (mut m, session, client, mut frames) = fixture();
    m.sessions.get_mut(&session).unwrap().flags |= TOP_CAPABLE;
    let listed = m
        .rpc(client, "Test host", "list_sessions", &json!({}))
        .unwrap();
    assert!(
        listed["sessions"][0]["available_capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("interactive_vi_file"))
    );
    let path = "/srv/配置/O'Reilly $(echo risk).txt";
    let start = json!({"session_id":session,"program":"vi","request_id":"file","path":path});
    let task = m
        .rpc(client, "Test host", "start_interactive", &start)
        .unwrap();
    let id = Uuid::parse_str(task["task_id"].as_str().unwrap()).unwrap();
    assert_eq!(task["program"], "vi");
    assert_eq!(task["path"], path);
    assert_eq!(task["requires_local_command_approval"], true);
    let command = task["command"].as_str().unwrap();
    assert!(command.starts_with("p='/srv/配置/O'\\''Reilly $(echo risk).txt'; test -f \"$p\" && test ! -L \"$p\" && exec vi "));
    assert!(command.ends_with("getfsize(g:putty_mcp_path)' -- \"$p\""));

    let mut expected = client.as_bytes().to_vec();
    expected.extend_from_slice(b"\x09Test hostvi-file\0");
    expected.extend_from_slice(&(path.len() as u16).to_le_bytes());
    expected.extend_from_slice(path.as_bytes());
    let frame = frames.try_recv().unwrap();
    assert_eq!(
        (
            frame.kind,
            frame.flags,
            frame.session,
            frame.request,
            frame.epoch
        ),
        (VI_FILE_START, 0, session, id, 1)
    );
    assert_eq!(frame.data, expected);

    let repeat = m
        .rpc(client, "Test host", "start_interactive", &start)
        .unwrap();
    assert_eq!(repeat["task_id"], task["task_id"]);
    let mut other_path = start.clone();
    other_path["path"] = json!("/srv/other.txt");
    let error = m
        .rpc(client, "Test host", "start_interactive", &other_path)
        .unwrap_err();
    assert!(error.to_string().contains("different program or path"));
    assert!(frames.try_recv().is_err());

    mark_interactive_running(&mut m, session, id);
    let save = json!({"task_id":id,"action":"save","request_id":"save-file"});
    let pending = m
        .rpc(client, "Test host", "request_task_action", &save)
        .unwrap();
    assert_eq!(pending["key"], "save");
    assert_eq!(pending["key_request_id"], "save-file");
    assert_eq!(pending["key_status"], "pending_approval");
    let frame = frames.try_recv().unwrap();
    assert_eq!(
        (
            frame.kind,
            frame.flags,
            frame.session,
            frame.request,
            frame.epoch
        ),
        (VI_SAVE, 0, session, id, 1)
    );
    assert_eq!(frame.data, expected);
    assert_eq!(
        m.rpc(client, "Test host", "request_task_action", &save)
            .unwrap(),
        pending
    );
    assert!(frames.try_recv().is_err());
    assert_eq!(m.top_key_commit(session, id).kind, TOP_SEND_KEY);
    assert_eq!(m.top_key_commit(session, id).kind, REVOKE);
    let mut sent = decision(session, id, 1, TOP_KEY_SENT);
    sent.flags |= TOP_CAPABLE;
    m.native_state(session, &sent).unwrap();
    let saved = m
        .rpc(client, "Test host", "request_task_action", &save)
        .unwrap();
    assert_eq!(saved["key_status"], "sent");
    assert_eq!(saved["status"], "running");
    assert!(frames.try_recv().is_err());

    for (method, args, kind, flags, suffix) in [
        (
            "request_task_action",
            json!({"task_id":id,"action":"insert_text","request_id":"paste","text":"文本\n"}),
            VI_INSERT_TEXT,
            0,
            "文本\n",
        ),
        (
            "request_task_action",
            json!({"task_id":id,"action":"undo","request_id":"undo"}),
            SPECIAL_KEY,
            32,
            "",
        ),
        (
            "interrupt_task",
            json!({"task_id":id}),
            TOP_INTERRUPT,
            0,
            "",
        ),
        (
            "request_task_action",
            json!({"task_id":id,"action":"quit"}),
            VI_QUIT,
            0,
            "",
        ),
    ] {
        m.rpc(client, "Test host", method, &args).unwrap();
        let frame = frames.try_recv().unwrap();
        assert_eq!((frame.kind, frame.flags, frame.request), (kind, flags, id));
        let mut action_data = expected.clone();
        action_data.extend_from_slice(suffix.as_bytes());
        assert_eq!(frame.data, action_data);
        assert_eq!(m.top_key_commit(session, id).kind, TOP_SEND_KEY);
        m.native_state(session, &sent).unwrap();
    }
}

#[test]
fn vi_file_rejects_invalid_paths_and_save_requires_file_profile() {
    let (mut m, session, client, mut frames) = fixture();
    m.sessions.get_mut(&session).unwrap().flags |= TOP_CAPABLE;
    let oversized = format!("/{}", "测".repeat(171));
    for path in [
        "",
        "relative",
        "/",
        "//etc/passwd",
        "/etc//passwd",
        "/etc/passwd/",
        "/etc/./passwd",
        "/etc/../passwd",
        "/etc/a\nb",
        "/etc/a\0b",
        "/etc/a\x7f",
        "/etc/a\u{009b}",
        "/etc/a\u{2028}",
        "/etc/a\u{202e}",
        &oversized,
    ] {
        let args = json!({"session_id":session,"program":"vi","request_id":"invalid","path":path});
        let error = m
            .rpc(client, "Test host", "start_interactive", &args)
            .unwrap_err();
        assert!(
            error.to_string().contains("absolute POSIX file path"),
            "{path:?}: {error}"
        );
    }
    let top_with_path =
        json!({"session_id":session,"program":"top","request_id":"wrong-program","path":"/file"});
    let error = m
        .rpc(client, "Test host", "start_interactive", &top_with_path)
        .unwrap_err();
    assert!(error.to_string().contains("path is only available for vi"));
    assert!(frames.try_recv().is_err());

    for (program, kind) in [("top", TOP_START), ("vi", VI_START)] {
        let (mut m, _, client, id, mut frames) = running_interactive(program, kind);
        let save = json!({"task_id":id,"action":"save","request_id":"save"});
        let error = m
            .rpc(client, "Test host", "request_task_action", &save)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("save requires vi opened with a file path")
        );
        assert!(frames.try_recv().is_err());
    }
}

fn child_endpoint() -> (Session, mpsc::Receiver<Frame>) {
    let (mut m, id, _, frames) = fixture();
    let mut endpoint = m.sessions.remove(&id).unwrap();
    endpoint.flags |= CHILD_SESSION | TOP_CAPABLE;
    (endpoint, frames)
}

fn child_task(m: &mut Model, client: Uuid, id: Uuid) -> serde_json::Value {
    m.rpc(
        client,
        "Test host",
        "get_task_status",
        &json!({"task_id":id}),
    )
    .unwrap()
}

#[test]
fn child_lifecycle_authenticates_registration_and_serializes_approval() {
    let (mut m, parent, client, mut parent_frames) = fixture();
    let args = json!({"session_id":parent,"request_id":"open"});
    assert!(
        m.rpc(client, "Test host", "open_child_session", &args)
            .is_err()
    );
    m.sessions.get_mut(&parent).unwrap().flags |= CHILD_MODE;
    let open = m
        .rpc(client, "Test host", "open_child_session", &args)
        .unwrap();
    assert_eq!(open["status"], "queued");
    let child = Uuid::parse_str(open["child_session_id"].as_str().unwrap()).unwrap();
    assert_eq!(open["task_id"], child.to_string());
    let frame = parent_frames.try_recv().unwrap();
    assert_eq!(
        (frame.kind, frame.session, frame.request, frame.epoch),
        (CHILD_OPEN, parent, child, 1)
    );
    assert_eq!(
        frame.data,
        [client.as_bytes().as_slice(), b"\x09Test host"].concat()
    );
    assert_eq!(
        m.rpc(client, "Test host", "open_child_session", &args)
            .unwrap(),
        open
    );
    assert!(
        m.rpc(
            client,
            "Test host",
            "open_child_session",
            &json!({"session_id":parent,"request_id":"next"})
        )
        .is_err()
    );
    assert!(parent_frames.try_recv().is_err());

    let (endpoint, _) = child_endpoint();
    assert!(m.register_native(child, endpoint, Some(parent)).is_err());
    assert_eq!(m.child_commit(Uuid::new_v4(), child).kind, REVOKE);
    let mut local_approval = decision(parent, child, 1, CHILD_APPROVED);
    local_approval.flags |= CHILD_MODE;
    m.native_state(parent, &local_approval).unwrap();
    assert_eq!(m.child_commit(parent, child).kind, CHILD_COMMIT);
    assert_eq!(m.child_commit(parent, child).kind, REVOKE);
    assert_eq!(child_task(&mut m, client, child)["status"], "authorized");
    let (mut impostor, _) = child_endpoint();
    impostor.pid += 1;
    assert!(m.register_native(child, impostor, Some(parent)).is_err());
    let (endpoint, _) = child_endpoint();
    assert!(
        m.register_native(Uuid::new_v4(), endpoint, Some(parent))
            .is_err()
    );
    let (endpoint, mut child_frames) = child_endpoint();
    let generation = endpoint.generation;
    m.register_native(child, endpoint, Some(parent)).unwrap();
    assert_eq!(child_task(&mut m, client, child)["status"], "completed");

    let sessions = m
        .rpc(client, "Test host", "list_sessions", &json!({}))
        .unwrap();
    let rows = sessions["sessions"].as_array().unwrap();
    let parent_row = rows
        .iter()
        .find(|row| row["session_id"] == parent.to_string())
        .unwrap();
    let child_row = rows
        .iter()
        .find(|row| row["session_id"] == child.to_string())
        .unwrap();
    assert_eq!(parent_row["mode"], "multi_child");
    assert!(parent_row["parent_session_id"].is_null());
    assert!(
        parent_row["available_capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("child_open"))
    );
    assert_eq!(child_row["mode"], "child");
    assert_eq!(child_row["parent_session_id"], parent.to_string());
    assert!(
        child_row["available_capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("child_close"))
    );

    let command = json!({"session_id":child,"request_id":"command","command":"pwd"});
    let running = m
        .rpc(client, "Test host", "submit_command", &command)
        .unwrap();
    let running = Uuid::parse_str(running["task_id"].as_str().unwrap()).unwrap();
    assert_eq!(child_frames.try_recv().unwrap().kind, COMMAND);
    let close = json!({"session_id":child,"request_id":"close"});
    assert!(
        m.rpc(client, "Test host", "close_child_session", &close)
            .is_err()
    );
    let mut declined = decision(child, running, 1, COMMAND_NOT_SENT);
    declined.flags |= CHILD_SESSION | TOP_CAPABLE;
    m.native_state(child, &declined).unwrap();
    let queued = m
        .rpc(client, "Test host", "close_child_session", &close)
        .unwrap();
    let id = Uuid::parse_str(queued["task_id"].as_str().unwrap()).unwrap();
    assert_eq!(queued["child_session_id"], child.to_string());
    assert_eq!(queued["parent_session_id"], parent.to_string());
    let frame = child_frames.try_recv().unwrap();
    assert_eq!(
        (frame.kind, frame.session, frame.request, frame.epoch),
        (CHILD_CLOSE, child, id, 1)
    );
    assert_eq!(
        frame.data,
        [client.as_bytes().as_slice(), b"\x09Test host"].concat()
    );
    assert!(
        m.rpc(
            client,
            "Test host",
            "submit_command",
            &json!({"session_id":child,"request_id":"later","command":"pwd"})
        )
        .is_err()
    );
    assert!(
        m.rpc(
            Uuid::new_v4(),
            "Other",
            "get_task_status",
            &json!({"task_id":id})
        )
        .is_err()
    );
    assert_eq!(m.child_commit(child, id).kind, CHILD_COMMIT);
    let mut accepted = decision(child, id, 1, CHILD_ACCEPTED);
    accepted.flags |= CHILD_SESSION | TOP_CAPABLE;
    m.native_state(child, &accepted).unwrap();
    assert_eq!(child_task(&mut m, client, id)["status"], "closing");
    m.disconnect(child, Uuid::new_v4());
    assert_eq!(child_task(&mut m, client, id)["status"], "closing");
    m.disconnect(child, generation);
    assert_eq!(child_task(&mut m, client, id)["status"], "closing");
    let mut closed = Frame::new(CHILD_EVENT, parent);
    closed.request = id;
    closed.flags = READ | CONNECTED | CHILD_MODE;
    closed.epoch = 1;
    closed.data.extend_from_slice(&5u32.to_le_bytes());
    m.native_state(parent, &closed).unwrap();
    assert_eq!(child_task(&mut m, client, id)["status"], "completed");
    assert!(m.sessions.contains_key(&parent));
    assert!(!m.sessions.contains_key(&child));
}

#[test]
fn child_open_failure_timeout_and_parent_revocation_never_register_late_child() {
    let (mut m, parent, client, mut frames) = fixture();
    m.sessions.get_mut(&parent).unwrap().flags |= CHILD_MODE;
    for (index, reason) in ["pty_refused", "timeout", "parent_revoked"]
        .into_iter()
        .enumerate()
    {
        let queued = m
            .rpc(
                client,
                "Test host",
                "open_child_session",
                &json!({"session_id":parent,"request_id":reason}),
            )
            .unwrap();
        let id = Uuid::parse_str(queued["task_id"].as_str().unwrap()).unwrap();
        assert_eq!(frames.try_recv().unwrap().kind, CHILD_OPEN);
        let mut ack = decision(parent, id, 1, CHILD_APPROVED);
        ack.flags |= CHILD_MODE;
        m.native_state(parent, &ack).unwrap();
        assert_eq!(m.child_commit(parent, id).kind, CHILD_COMMIT);
        match index {
            0 => {
                let mut refused = Frame::new(CHILD_EVENT, parent);
                refused.request = id;
                refused.flags = READ | CONNECTED | CHILD_MODE;
                refused.epoch = 1;
                refused.data.extend_from_slice(&2u32.to_le_bytes());
                m.native_state(parent, &refused).unwrap();
                let status = child_task(&mut m, client, id);
                assert_eq!(status["status"], "failed");
                assert_eq!(status["error"], "SSH server refused a child PTY");
            }
            1 => {
                m.expire_lifecycle(Instant::now() + Duration::from_secs(31));
                assert_eq!(child_task(&mut m, client, id)["status"], "unknown");
            }
            _ => {
                let mut state = decision(parent, Uuid::nil(), 1, 0);
                m.native_state(parent, &state).unwrap();
                assert_eq!(child_task(&mut m, client, id)["status"], "unknown");
                state.flags |= CHILD_MODE;
                m.native_state(parent, &state).unwrap();
            }
        }
        let (endpoint, _) = child_endpoint();
        assert!(m.register_native(id, endpoint, Some(parent)).is_err());
        assert!(m.sessions[&parent].lifecycle.is_none());
    }
}

#[test]
fn four_children_are_isolated_and_unconfirmed_close_stays_unknown() {
    let (mut m, parent, client, mut frames) = fixture();
    m.sessions.get_mut(&parent).unwrap().flags |= CHILD_MODE;
    let mut children = Vec::new();
    for index in 0..4 {
        let open = m
            .rpc(
                client,
                "Test host",
                "open_child_session",
                &json!({"session_id":parent,"request_id":format!("open-{index}")}),
            )
            .unwrap();
        let id = Uuid::parse_str(open["task_id"].as_str().unwrap()).unwrap();
        assert_eq!(frames.try_recv().unwrap().kind, CHILD_OPEN);
        assert_eq!(m.child_commit(parent, id).kind, CHILD_COMMIT);
        let (endpoint, child_frames) = child_endpoint();
        let generation = endpoint.generation;
        m.register_native(id, endpoint, Some(parent)).unwrap();
        let mut output = Frame::new(OUTPUT, id);
        output.flags = READ | CONNECTED | CHILD_SESSION | TOP_CAPABLE;
        output.epoch = 1;
        output.data = format!("child {index}\n").into_bytes();
        m.native_state(id, &output).unwrap();
        children.push((id, generation, child_frames));
    }
    let error = m
        .rpc(
            client,
            "Test host",
            "open_child_session",
            &json!({"session_id":parent,"request_id":"limit"}),
        )
        .unwrap_err();
    assert!(error.to_string().contains("capacity"));
    assert!(frames.try_recv().is_err());

    for (index, (id, _, _)) in children.iter().enumerate() {
        let output = m
            .rpc(
                Uuid::new_v4(),
                "Reader",
                "read_terminal",
                &json!({"session_id":id,"cursor":0}),
            )
            .unwrap();
        assert_eq!(output["output"], format!("child {index}\n"));
    }
    let (child, generation, mut child_frames) = children.remove(0);
    let close = m
        .rpc(
            client,
            "Test host",
            "close_child_session",
            &json!({"session_id":child,"request_id":"close"}),
        )
        .unwrap();
    let id = Uuid::parse_str(close["task_id"].as_str().unwrap()).unwrap();
    assert_eq!(child_frames.try_recv().unwrap().kind, CHILD_CLOSE);
    assert_eq!(m.child_commit(child, id).kind, CHILD_COMMIT);
    m.disconnect(child, generation);
    assert_eq!(child_task(&mut m, client, id)["status"], "authorized");
    m.expire_lifecycle(Instant::now() + Duration::from_secs(31));
    assert_eq!(child_task(&mut m, client, id)["status"], "unknown");

    let parent_generation = m.sessions[&parent].generation;
    m.disconnect(parent, parent_generation);
    assert!(m.sessions.is_empty());
}

#[test]
fn automatic_child_mode_requires_trusted_parent_and_marks_lifecycle_frames() {
    let parent_generation = 0x8a_1234;
    let child_generation = 0x01_2345;
    let (mut m, parent, client, mut frames) = fixture();
    let (mut impostor, _) = child_endpoint();
    impostor.flags = READ | CONNECTED | AUTO_RUN;
    assert!(m.register_native(Uuid::new_v4(), impostor, None).is_err());
    let mut root_state = decision(parent, Uuid::nil(), 1, 0);
    root_state.flags |= AUTO_RUN;
    assert!(m.native_state(parent, &root_state).is_err());
    root_state.flags |= CHILD_MODE | (parent_generation << 8);
    m.native_state(parent, &root_state).unwrap();
    let listing = m
        .rpc(client, "Test host", "list_sessions", &json!({}))
        .unwrap();
    let row = &listing["sessions"][0];
    assert_eq!(row["approval_mode"], "automatic");
    let caps = row["available_capabilities"].as_array().unwrap();
    assert!(caps.contains(&json!("automatic_execution")));
    assert!(!caps.contains(&json!("local_per_command_approval")));

    let open = m
        .rpc(
            client,
            "Test host",
            "open_child_session",
            &json!({"session_id":parent,"request_id":"open"}),
        )
        .unwrap();
    assert_eq!(open["approval_mode"], "automatic");
    assert_eq!(open["requires_local_command_approval"], false);
    let child = Uuid::parse_str(open["child_session_id"].as_str().unwrap()).unwrap();
    let frame = frames.try_recv().unwrap();
    assert_eq!((frame.kind, frame.flags), (CHILD_OPEN, REQUEST_AUTO_RUN));
    assert_eq!(frame.seq, u64::from(parent_generation));
    let mut approved = decision(parent, child, 1, CHILD_APPROVED);
    approved.flags = root_state.flags;
    let event = m.native_state(parent, &approved).unwrap().unwrap();
    assert_eq!(event["event"], "child_auto_authorized");
    let commit = m.child_commit(parent, child);
    assert_eq!((commit.kind, commit.flags), (CHILD_COMMIT, 0));
    assert_eq!(commit.seq, 0);
    let (endpoint, _) = child_endpoint();
    assert!(m.register_native(child, endpoint, Some(parent)).is_err());
    let (mut endpoint, mut child_frames) = child_endpoint();
    endpoint.flags |= AUTO_RUN | (child_generation << 8);
    m.register_native(child, endpoint, Some(parent)).unwrap();
    let mut child_state = decision(child, Uuid::nil(), 1, 0);
    child_state.flags |= CHILD_SESSION | TOP_CAPABLE;
    assert!(m.native_state(child, &child_state).is_err());

    let close = m
        .rpc(
            client,
            "Test host",
            "close_child_session",
            &json!({"session_id":child,"request_id":"close"}),
        )
        .unwrap();
    assert_eq!(close["requires_local_command_approval"], false);
    let frame = child_frames.try_recv().unwrap();
    assert_eq!((frame.kind, frame.flags), (CHILD_CLOSE, REQUEST_AUTO_RUN));
    assert_eq!(frame.seq, u64::from(child_generation));
    root_state.flags &= !(CHILD_MODE | AUTO_RUN);
    m.native_state(parent, &root_state).unwrap();
    assert!(!m.sessions.contains_key(&child));
}

#[test]
fn active_interactive_task_follows_current_mode_for_each_new_action() {
    let (mut m, session, client, id, mut frames) = running_interactive("vi", VI_START);
    let pending_args = json!({"task_id":id,"action":"escape","request_id":"ordinary-escape"});
    let pending = m
        .rpc(client, "Test host", "request_task_action", &pending_args)
        .unwrap();
    assert_eq!(pending["requires_local_command_approval"], true);
    assert_eq!(frames.try_recv().unwrap().flags, 1);
    let mut state = decision(session, Uuid::nil(), 1, 0);
    state.flags |= TOP_CAPABLE | CHILD_MODE | AUTO_RUN | (3 << 8);
    m.native_state(session, &state).unwrap();
    assert_eq!(frames.try_recv().unwrap().kind, REVOKE);
    assert_eq!(m.top_key_commit(session, id).kind, REVOKE);
    let mut declined = decision(session, id, 1, TOP_KEY_NOT_SENT);
    declined.flags = state.flags;
    let event = m.native_state(session, &declined).unwrap().unwrap();
    assert_eq!(event["approval_mode"], "per_action");

    let automatic_args = json!({"task_id":id,"action":"f2","request_id":"automatic-f2"});
    let automatic = m
        .rpc(client, "Test host", "request_task_action", &automatic_args)
        .unwrap();
    assert_eq!(automatic["requires_local_command_approval"], false);
    let key = frames.try_recv().unwrap();
    assert_eq!((key.kind, key.flags), (SPECIAL_KEY, REQUEST_AUTO_RUN | 3));
    assert_eq!(key.seq, 3);
    let commit = m.top_key_commit(session, id);
    assert_eq!((commit.kind, commit.flags), (TOP_SEND_KEY, 0));
    assert_eq!(commit.seq, 0);
    let mut sent = decision(session, id, 1, TOP_KEY_SENT);
    sent.flags = state.flags;
    let event = m.native_state(session, &sent).unwrap().unwrap();
    assert_eq!(event["approval_mode"], "automatic");
    state.flags = READ | CONNECTED | TOP_CAPABLE | (4 << 8);
    m.native_state(session, &state).unwrap();
    let replay = m
        .rpc(client, "Test host", "request_task_action", &automatic_args)
        .unwrap();
    assert_eq!(replay["approval_mode"], "automatic");
    let ordinary = m
        .rpc(
            client,
            "Test host",
            "request_task_action",
            &json!({"task_id":id,"action":"quit"}),
        )
        .unwrap();
    assert_eq!(ordinary["approval_mode"], "per_action");
    let quit = frames.try_recv().unwrap();
    assert_eq!((quit.kind, quit.flags), (VI_QUIT, 0));
    assert_eq!(quit.seq, 4);
}

#[test]
fn command_and_interrupt_keep_independent_approval_modes() {
    let (mut m, session, client, mut frames) = fixture();
    let mut state = decision(session, Uuid::nil(), 1, 0);
    state.flags |= CHILD_MODE | AUTO_RUN | (9 << 8);
    m.native_state(session, &state).unwrap();
    let request = json!({"session_id":session,"command":"sleep 30","request_id":"original"});
    let task = m
        .rpc(client, "Test host", "submit_command", &request)
        .unwrap();
    assert_eq!(task["requires_local_command_approval"], false);
    let id = Uuid::parse_str(task["task_id"].as_str().unwrap()).unwrap();
    let command = frames.try_recv().unwrap();
    assert_eq!((command.kind, command.flags), (COMMAND, REQUEST_AUTO_RUN));
    assert_eq!(command.seq, 9);
    let mut ack = command_sent(session, id, 1, 0, "");
    ack.flags = state.flags;
    m.native_state(session, &ack).unwrap();
    state.flags = READ | CONNECTED | (10 << 8);
    m.native_state(session, &state).unwrap();
    let interrupt = m
        .rpc(
            client,
            "Test host",
            "interrupt_task",
            &json!({"task_id":id}),
        )
        .unwrap();
    assert_eq!(interrupt["requires_local_command_approval"], true);
    let frame = frames.try_recv().unwrap();
    assert_eq!((frame.kind, frame.flags), (INTERRUPT, 0));
    assert_eq!(frame.seq, 10);
    assert_eq!(m.interrupt_commit(session, id).kind, SEND_INTERRUPT);
    let replay = m
        .rpc(client, "Test host", "submit_command", &request)
        .unwrap();
    assert_eq!(replay["approval_mode"], "automatic");
    assert!(frames.try_recv().is_err());
}

fn foreground_screen(m: &mut Model, session: Uuid, task: Uuid, frame: u64, text: &str) {
    let mut f = Frame::new(TERMINAL_SCREEN, session);
    f.request = task;
    f.flags = m.sessions[&session].flags;
    f.epoch = m.sessions[&session].epoch;
    f.seq = frame;
    f.data = vec![14, 80, 0, 24, 0, 7, 0, 3, 0];
    f.data.extend_from_slice(text.as_bytes());
    m.native_state(session, &f).unwrap();
}

fn terminal_action_ack(m: &mut Model, session: Uuid, action: Uuid, ack: u32) {
    let s = &m.sessions[&session];
    let mut f = decision(session, action, s.epoch, ack);
    f.flags = s.flags;
    f.seq = s.output.end;
    m.native_state(session, &f).unwrap();
}

fn id_field(value: &serde_json::Value, name: &str) -> Uuid {
    Uuid::parse_str(value[name].as_str().unwrap()).unwrap()
}

#[test]
fn raw_terminal_action_checks_owner_live_frame_and_serializes_interrupt() {
    let (mut m, session, client, mut frames) = fixture();
    m.sessions.get_mut(&session).unwrap().flags |= TERMINAL_CAPABLE;
    let task = submit(&mut m, session, client);
    assert_eq!(frames.try_recv().unwrap().kind, COMMAND);
    let mut ack = command_sent(session, task, 1, 0, "<H3C>");
    ack.flags |= TERMINAL_CAPABLE;
    m.native_state(session, &ack).unwrap();
    let screen_args = json!({"task_id":task});
    foreground_screen(&mut m, session, Uuid::new_v4(), 1, "old view");
    assert!(
        m.rpc(client, "Test host", "read_task_screen", &screen_args)
            .unwrap()["frame"]
            .is_null()
    );
    foreground_screen(&mut m, session, task, 2, "---- More ----");
    let screen = m
        .rpc(client, "Test host", "read_task_screen", &screen_args)
        .unwrap();
    assert_eq!(screen["frame"]["frame_id"], 2);
    assert_eq!(screen["frame"]["cursor"], json!({"column":7,"row":3}));
    assert!(
        m.rpc(Uuid::new_v4(), "Other", "read_task_screen", &screen_args)
            .is_err()
    );

    let args = json!({"task_id":task,"request_id":"next-page","action":"press_key","key":"space","if_frame":2});
    assert!(
        m.rpc(Uuid::new_v4(), "Other", "request_task_action", &args)
            .is_err()
    );
    let receipt = m
        .rpc(client, "Test host", "request_task_action", &args)
        .unwrap();
    assert_eq!(receipt["action_status"], "pending_approval");
    assert_eq!(receipt["requires_local_command_approval"], true);
    let action = id_field(&receipt, "action_id");
    let wire = frames.try_recv().unwrap();
    assert_eq!(
        (wire.kind, wire.request, wire.flags),
        (TERMINAL_INPUT, action, 1)
    );
    let source_end = 17 + wire.data[16] as usize;
    assert_eq!(&wire.data[source_end..source_end + 16], task.as_bytes());
    assert_eq!(
        &wire.data[source_end + 16..source_end + 24],
        &2u64.to_le_bytes()
    );
    assert_eq!(&wire.data[source_end + 24..], b"space");
    assert!(
        m.rpc(client, "Test host", "interrupt_task", &screen_args)
            .is_err()
    );
    foreground_screen(&mut m, session, task, 3, "---- More ----");
    assert_eq!(m.foreground_commit(session, action).kind, REVOKE);
    terminal_action_ack(&mut m, session, action, TERMINAL_INPUT_NOT_SENT);
    let replay = m
        .rpc(client, "Test host", "request_task_action", &args)
        .unwrap();
    assert_eq!(replay["action_status"], "not_sent");
    assert!(frames.try_recv().is_err());

    m.tasks.get_mut(&task).unwrap().deadline = Some(Instant::now());
    let next = json!({"task_id":task,"request_id":"next-page-2","action":"press_key","key":"space","if_frame":3});
    let unguarded =
        json!({"task_id":task,"request_id":"unguarded","action":"press_key","key":"space"});
    assert!(
        m.rpc(client, "Test host", "request_task_action", &unguarded)
            .is_err()
    );
    let receipt = m
        .rpc(client, "Test host", "request_task_action", &next)
        .unwrap();
    assert_eq!(receipt["status"], "timeout");
    let action = id_field(&receipt, "action_id");
    assert_eq!(frames.try_recv().unwrap().request, action);
    assert_eq!(m.foreground_commit(session, action).kind, TERMINAL_COMMIT);
    terminal_action_ack(&mut m, session, action, TERMINAL_INPUT_SENT);
    assert_eq!(
        command_status(&mut m, client, task)["action_status"],
        "sent"
    );
    output(&mut m, session, b"page ended\r\n<H3C>");
    poll_prompt(&mut m, session, task);
    assert!(m.sessions[&session].active.is_none());
    let replay = m
        .rpc(client, "Test host", "request_task_action", &next)
        .unwrap();
    assert_eq!(replay["action_status"], "sent");
    assert!(frames.try_recv().is_err());
}

fn start_attachment(m: &mut Model, session: Uuid, client: Uuid, request: &str) -> Uuid {
    let args = json!({"session_id":session,"request_id":request});
    let receipt = m
        .rpc(client, "Test host", "attach_terminal", &args)
        .unwrap();
    assert_eq!(receipt["task_kind"], "terminal_attachment");
    assert_eq!(receipt["status"], "queued");
    let task = id_field(&receipt, "task_id");
    assert_eq!(m.foreground_commit(session, task).kind, TERMINAL_COMMIT);
    let s = &m.sessions[&session];
    let mut ack = decision(session, task, s.epoch, TERMINAL_ATTACHED);
    ack.flags = s.flags;
    ack.seq = s.output.end;
    ack.data.extend_from_slice(&ack.seq.to_le_bytes());
    m.native_state(session, &ack).unwrap();
    assert_eq!(command_status(m, client, task)["status"], "running");
    task
}

#[test]
fn attachment_is_owned_and_human_input_revokes_pending_action() {
    let (mut m, session, client, mut frames) = fixture();
    let flags = READ | CONNECTED | TERMINAL_CAPABLE | CHILD_MODE | AUTO_RUN | (27 << 8);
    m.sessions.get_mut(&session).unwrap().flags = flags;
    let task = start_attachment(&mut m, session, client, "attach-1");
    let attach = frames.try_recv().unwrap();
    assert_eq!(
        (attach.kind, attach.request, attach.seq, attach.flags),
        (TERMINAL_ATTACH, task, 27, REQUEST_AUTO_RUN)
    );
    assert_eq!(
        attach.data,
        [client.as_bytes().as_slice(), b"\x09Test host"].concat()
    );
    assert!(
        m.rpc(
            client,
            "Test host",
            "interrupt_task",
            &json!({"task_id":task})
        )
        .is_err()
    );
    let args = json!({"task_id":task,"request_id":"combo","action":"press_key","key":"left","modifiers":["shift","ctrl","alt"]});
    let receipt = m
        .rpc(client, "Test host", "request_task_action", &args)
        .unwrap();
    assert_eq!(receipt["requires_local_command_approval"], false);
    let action = id_field(&receipt, "action_id");
    let wire = frames.try_recv().unwrap();
    assert_eq!((wire.seq, wire.flags), (27, REQUEST_AUTO_RUN | 1));
    assert!(wire.data.ends_with(b"ctrl+alt+shift+left"));

    let mut human = decision(session, Uuid::nil(), 2, 0);
    human.flags = flags;
    m.native_state(session, &human).unwrap();
    let revoked = frames.try_recv().unwrap();
    assert_eq!((revoked.kind, revoked.request), (REVOKE, action));
    assert_eq!(m.foreground_commit(session, action).kind, REVOKE);
    assert_eq!(m.sessions[&session].active, Some(task));
    terminal_action_ack(&mut m, session, action, TERMINAL_INPUT_NOT_SENT);
    assert!(m.sessions[&session].active.is_none());
    let old = command_status(&mut m, client, task);
    assert_eq!(old["status"], "unknown");
    assert_eq!(old["action_status"], "not_sent");

    let new = start_attachment(&mut m, session, client, "attach-2");
    assert_eq!(frames.try_recv().unwrap().kind, TERMINAL_ATTACH);
    let args = json!({"task_id":new,"request_id":"detach","action":"detach"});
    let receipt = m
        .rpc(client, "Test host", "request_task_action", &args)
        .unwrap();
    let action = id_field(&receipt, "action_id");
    assert_eq!(receipt["requires_local_command_approval"], false);
    let wire = frames.try_recv().unwrap();
    assert_eq!(
        (wire.kind, wire.request, wire.seq, wire.flags),
        (TERMINAL_DETACH, action, 27, REQUEST_AUTO_RUN)
    );
    assert!(wire.data.ends_with(new.as_bytes()));
    terminal_action_ack(&mut m, session, action, TERMINAL_DETACHED);
    assert!(m.sessions[&session].active.is_none());
    assert_eq!(command_status(&mut m, client, new)["status"], "detached");
    let replay = m
        .rpc(client, "Test host", "request_task_action", &args)
        .unwrap();
    assert_eq!(replay["action_status"], "detached");
    assert!(frames.try_recv().is_err());
}

#[test]
fn text_and_paste_preserve_exact_bytes_but_reject_control_injection() {
    let (mut m, session, client, mut frames) = fixture();
    m.sessions.get_mut(&session).unwrap().flags |= TERMINAL_CAPABLE;
    let task = start_attachment(&mut m, session, client, "attach");
    frames.try_recv().unwrap();
    for (action, text) in [
        ("type_text", "a\n"),
        ("type_text", "a\t"),
        ("paste_text", "\x1b[201~"),
        ("paste_text", "a\rb"),
        ("paste_text", "\u{202e}"),
    ] {
        let invalid = json!({"task_id":task,"request_id":"bad","action":action,"text":text});
        assert!(
            m.rpc(client, "Test host", "request_task_action", &invalid)
                .is_err()
        );
    }
    assert!(frames.try_recv().is_err());
    for (action, text, flags) in [("type_text", "写入 🙂", 2), ("paste_text", "a\n\t测试", 3)]
    {
        let args = json!({"task_id":task,"request_id":action,"action":action,"text":text});
        let receipt = m
            .rpc(client, "Test host", "request_task_action", &args)
            .unwrap();
        let action_id = id_field(&receipt, "action_id");
        let wire = frames.try_recv().unwrap();
        assert_eq!(
            (wire.kind, wire.request, wire.flags),
            (TERMINAL_INPUT, action_id, flags)
        );
        assert!(wire.data.ends_with(text.as_bytes()));
        assert_eq!(
            m.foreground_commit(session, action_id).kind,
            TERMINAL_COMMIT
        );
        terminal_action_ack(&mut m, session, action_id, TERMINAL_INPUT_SENT);
        let status = command_status(&mut m, client, task);
        assert_eq!(status["action_status"], "sent");
        assert!(!status.to_string().contains("测试"));
    }
}
