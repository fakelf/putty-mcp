use anyhow::{Result, bail, ensure};
use putty_ai_common::{COMMANDS, OUTPUT_LIMIT, protocol::*, terminal::OutputRing};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use uuid::Uuid;

pub const LEASE: Duration = Duration::from_secs(60);
pub struct Session {
    pub generation: Uuid,
    pub pid: u32,
    pub start: u64,
    pub hwnd: u64,
    pub flags: u32,
    pub epoch: u64,
    pub output: OutputRing,
    pub tx: mpsc::Sender<Frame>,
    pub lease: Option<(Uuid, Instant)>,
    pub pending: Option<Uuid>,
    pub pending_deadline: Option<Instant>,
    pub rejected_client: Option<Uuid>,
    pub pending_custom: bool,
    pub lease_custom: bool,
    pub active: Option<Uuid>,
    pub received: u64,
    pub last_seen: Instant,
    pub lost_total: u64,
    pub native_dropped: u64,
}
pub struct Task {
    pub client: Uuid,
    pub session: Uuid,
    pub status: &'static str,
    pub exit: Option<i32>,
    pub error: Option<String>,
    pub deadline: Instant,
    pub output: OutputRing,
    pub token: String,
    pub scan: Vec<u8>,
    pub started: bool,
    pub complete: bool,
    pub request: String,
    pub command: String,
    pub created: Instant,
}
#[derive(Default)]
pub struct Model {
    pub sessions: HashMap<Uuid, Session>,
    pub tasks: HashMap<Uuid, Task>,
}
impl Model {
    pub fn disconnect(&mut self, id: Uuid, generation: Uuid) {
        if self
            .sessions
            .get(&id)
            .is_some_and(|s| s.generation == generation)
        {
            self.sessions.remove(&id);
            for t in self.tasks.values_mut().filter(|t| t.session == id) {
                if matches!(t.status, "running" | "queued") {
                    t.status = "unknown";
                    t.created = Instant::now();
                    t.error = Some("terminal disconnected; remote outcome unknown".into());
                }
                t.output.clear();
                t.scan.clear();
            }
        }
    }
    pub fn client_gone(&mut self, client: Uuid) {
        for (id, s) in &mut self.sessions {
            if s.lease.is_some_and(|(c, _)| c == client) || s.pending == Some(client) {
                let _ = s.tx.try_send(Frame::new(REVOKE, *id));
                s.lease = None;
                s.pending = None;
            }
        }
        for t in self.tasks.values_mut().filter(|t| t.client == client) {
            if t.status == "queued" {
                t.status = "unknown";
                t.created = Instant::now();
                t.error =
                    Some("client disconnected during approval; remote outcome unknown".into());
            } else if t.status == "running" {
                t.status = "unknown";
                t.created = Instant::now();
                t.error = Some("MCP client disconnected; remote process may continue".into());
            }
        }
    }
    pub fn expire(&mut self) {
        let now = Instant::now();
        for (id, s) in &mut self.sessions {
            if s.pending.is_some() && s.pending_deadline.is_some_and(|d| now >= d) {
                s.pending = None;
                s.pending_deadline = None;
                let _ = s.tx.try_send(Frame::new(REVOKE, *id));
            }
            if s.lease.is_some_and(|(_, until)| now >= until) {
                s.lease = None;
                s.pending = None;
                let _ = s.tx.try_send(Frame::new(REVOKE, *id));
            }
        }
        for t in self.tasks.values_mut() {
            if now >= t.deadline && matches!(t.status, "queued" | "running") {
                t.error = Some(
                    if t.status == "queued" {
                        "local approval deadline expired; command was not confirmed"
                    } else {
                        "observation timeout; remote process may still be running"
                    }
                    .into(),
                );
                t.status = "timeout";
                t.created = now;
                if let Some(s) = self.sessions.get_mut(&t.session) {
                    s.lease = None;
                    s.pending = None;
                    let _ = s.tx.try_send(Frame::new(REVOKE, t.session));
                }
            }
        }
        self.tasks.retain(|_, t| {
            matches!(t.status, "queued" | "running")
                || now.duration_since(t.created) < Duration::from_secs(600)
        });
    }
    pub fn native_state(&mut self, id: Uuid, f: &Frame) -> Result<()> {
        let s = self
            .sessions
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("session closed"))?;
        let old_epoch = s.epoch;
        let pending_owner = s.pending;
        s.flags = f.flags;
        s.epoch = f.epoch;
        s.last_seen = Instant::now();
        if old_epoch != s.epoch || f.flags & CONTROL == 0 {
            s.lease = None;
            if old_epoch != s.epoch {
                s.pending = None;
                if let Some(tid) = s.active.take()
                    && let Some(t) = self.tasks.get_mut(&tid)
                    && matches!(t.status, "queued" | "running")
                {
                    t.status = "unknown";
                    t.created = Instant::now();
                    t.error = Some(
                        "local user activity or authorization changed; remote outcome unknown"
                            .into(),
                    );
                }
            }
        }
        if f.flags & READ == 0 {
            s.output.clear();
        }
        if f.kind == OUTPUT && f.flags & READ != 0 {
            s.lost_total += f.seq.saturating_sub(s.output.end);
            s.output.push(f.seq, &f.data)?;
            s.received += f.data.len() as u64;
            if let Some(task_id) = s.active
                && let Some(t) = self.tasks.get_mut(&task_id)
                && matches!(t.status, "running" | "queued")
            {
                if f.seq > t.output.end {
                    t.status = "unknown";
                    t.created = Instant::now();
                    t.error =
                        Some("output gap during command; completion cannot be verified".into());
                }
                t.output.push(f.seq, &f.data)?;
                if t.status != "unknown" {
                    // Scan incrementally before trimming, including chunks with both markers.
                    for part in f.data.chunks(256) {
                        t.scan.extend_from_slice(part);
                        t.detect();
                        if t.scan.len() > 256 {
                            t.scan.drain(..t.scan.len() - 256);
                        }
                    }
                }
                if t.complete {
                    s.active = None;
                }
            }
        }
        if f.kind == POLL {
            s.lost_total += f.seq.saturating_sub(s.output.end);
            s.output.advance(f.seq);
            if f.data.len() >= 28 {
                s.native_dropped = u64::from_le_bytes(f.data[20..28].try_into()?);
            }
            // Reply payload: local client id + acknowledgement status (1 sent, 2 rejected).
            if f.data.len() >= 20 {
                let client = Uuid::from_slice(&f.data[..16])?;
                if f.flags & CONTROL != 0 && s.pending == Some(client) {
                    ensure!(
                        (f.flags & CUSTOM_CONTROL != 0) == s.pending_custom,
                        "native granted an unexpected control mode"
                    );
                    s.lease = Some((client, Instant::now() + LEASE));
                    s.lease_custom = s.pending_custom;
                    s.pending = None;
                }
                let ack = u32::from_le_bytes(f.data[16..20].try_into()?);
                if f.request.is_nil() && ack == 2 && pending_owner == Some(client) {
                    s.pending = None;
                    s.pending_deadline = None;
                    s.rejected_client = Some(client);
                }
                if !f.request.is_nil()
                    && let Some(t) = self.tasks.get_mut(&f.request)
                    && t.session == id
                    && t.status == "queued"
                {
                    if ack == 1 {
                        t.status = "running";
                    } else if ack == 2 {
                        t.status = "cancelled";
                        t.created = Instant::now();
                        t.error = Some("PuTTY confirmed command was not sent".into());
                        s.active = None;
                    }
                }
            }
        }
        Ok(())
    }
    pub fn rpc(&mut self, client: Uuid, method: &str, args: &Value) -> Result<Value> {
        self.expire();
        if method == "list_sessions" {
            let list:Vec<_>=self.sessions.iter().filter(|(_,s)|s.flags&READ!=0 && s.flags&CONNECTED!=0).map(|(id,s)|json!({
                "session_id":id,"display_name":format!("PuTTY {} (target must be confirmed locally)",&id.to_string()[..8]),
                "connection_state":"connected","ai_access_state":if s.flags&CONTROL!=0 {"control_authorized"}else{"read_only"},
                "command_state":if s.active.is_some(){"busy"}else{"idle"},
                "available_capabilities":if s.flags&CUSTOM_CAPABLE!=0 {vec!["raw_capture","sanitized_transcript","diagnostic_templates","custom_commands","local_per_command_approval"]} else {vec!["raw_capture","sanitized_transcript","diagnostic_templates","local_per_command_approval"]},
                "control_mode":if s.lease.is_none(){"none"}else if s.lease_custom{"custom"}else{"diagnostic"},
                "pid":s.pid,"process_start":s.start,"hwnd":s.hwnd,"received_bytes":s.received,"output_cursor":s.output.end,
                "last_seen_ms":s.last_seen.elapsed().as_millis()
                ,"lost_capture_bytes":s.lost_total,"native_ring_dropped_bytes":s.native_dropped
            })).collect();
            return Ok(json!({"sessions":list}));
        }
        if method == "get_task_status" || method == "cancel_task" {
            let id = arg_uuid(args, "task_id")?;
            let t = self
                .tasks
                .get_mut(&id)
                .ok_or_else(|| anyhow::anyhow!("unknown task_id"))?;
            ensure!(t.client == client, "task belongs to another MCP client");
            let Some(s) = self.sessions.get(&t.session) else {
                return Ok(
                    json!({"task_id":id,"session_id":t.session,"status":t.status,"exit_code":t.exit,
                    "output":"","next_cursor":t.output.end,"output_truncated":true,"error":t.error}),
                );
            };
            ensure!(s.flags & READ != 0, "terminal access revoked");
            if method == "cancel_task" {
                if t.status == "queued" {
                    // Revocation reaches the GUI before a queued command can be approved.
                    s.tx.try_send(Frame::new(REVOKE, t.session)).map_err(|_| {
                        anyhow::anyhow!("native queue full; cancellation not confirmed")
                    })?;
                    // Delivery can race a local approval. Never claim that an in-flight command was stopped.
                    t.status = "unknown";
                    t.created = Instant::now();
                    t.error =
                        Some("revocation requested; command may already have been sent".into());
                } else if t.status == "running" {
                    t.status = "unknown";
                    t.created = Instant::now();
                    t.error = Some("remote command already sent; no interrupt was sent".into());
                }
            }
            let p = t.output.read(
                args.get("output_cursor").and_then(Value::as_u64),
                limit(args, "max_output_bytes")?,
            )?;
            return Ok(
                json!({"task_id":id,"session_id":t.session,"status":t.status,"exit_code":t.exit,"output":p.output,
                "next_cursor":p.next_cursor,"output_truncated":p.truncated,"lost_bytes":p.lost_bytes,
                "missing_ranges":p.missing_ranges,"error":t.error,"remote_process_stopped":false}),
            );
        }
        let id = arg_uuid(args, "session_id")?;
        let s = self
            .sessions
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("unknown or closed session_id"))?;
        ensure!(
            s.flags & (READ | CONNECTED) == READ | CONNECTED,
            "terminal is not authorized and connected"
        );
        match method {
            "read_terminal" => {
                let p = s.output.read(
                    args.get("cursor").and_then(Value::as_u64),
                    limit(args, "max_bytes")?,
                )?;
                let mut v = serde_json::to_value(p)?;
                v["session_id"] = json!(id);
                v["connection_state"] = json!("connected");
                v["untrusted_terminal_data"] = json!(true);
                Ok(v)
            }
            "acquire_control" => {
                if s.rejected_client == Some(client) {
                    return Ok(
                        json!({"status":"rejected_locally","session_id":id,"client_id":client,
                        "error":"local user rejected or invalidated control; stop polling. Release control before an explicitly requested retry."}),
                    );
                }
                let mode = args
                    .get("requested_mode")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                ensure!(
                    matches!(mode, "diagnostic" | "custom"),
                    "requested_mode must be diagnostic or custom"
                );
                let custom = mode == "custom";
                ensure!(
                    !custom || s.flags & CUSTOM_CAPABLE != 0,
                    "this PuTTY does not support custom mode; open the v2 executable"
                );
                if let Some((owner, until)) = s.lease {
                    ensure!(owner == client, "session controlled by another client");
                    ensure!(
                        s.lease_custom == custom,
                        "release the current mode before requesting a different mode"
                    );
                    return Ok(
                        json!({"status":"granted","requested_mode":mode,"client_id":client,"expires_in_ms":until.saturating_duration_since(Instant::now()).as_millis(),"session_id":id}),
                    );
                }
                ensure!(
                    s.pending.is_none() || s.pending == Some(client),
                    "another client awaits local approval"
                );
                if s.pending.is_none() {
                    let mut f = Frame::new(if custom { ACQUIRE_CUSTOM } else { ACQUIRE }, id);
                    f.data = client.as_bytes().to_vec();
                    f.epoch = s.epoch;
                    s.tx.try_send(f)
                        .map_err(|_| anyhow::anyhow!("native queue full"))?;
                    s.pending = Some(client);
                    s.pending_custom = custom;
                    s.pending_deadline = Some(Instant::now() + LEASE);
                }
                ensure!(
                    s.pending_custom == custom,
                    "another control mode is awaiting local approval"
                );
                Ok(
                    json!({"status":"pending_local_approval","requested_mode":mode,"client_id":client,"session_id":id,"expires_in_ms":s.pending_deadline.unwrap().saturating_duration_since(Instant::now()).as_millis()}),
                )
            }
            "release_control" => {
                ensure!(
                    s.lease.is_none_or(|(c, _)| c == client)
                        && s.pending.is_none_or(|c| c == client),
                    "not lease owner"
                );
                s.tx.try_send(Frame::new(REVOKE, id))
                    .map_err(|_| anyhow::anyhow!("native queue full"))?;
                s.lease = None;
                s.pending = None;
                if s.rejected_client == Some(client) {
                    s.rejected_client = None;
                }
                Ok(json!({"status":"released","session_id":id}))
            }
            "submit_command" => {
                ensure!(
                    s.lease
                        .is_some_and(|(c, until)| c == client && until > Instant::now()),
                    "local control authorization required"
                );
                let cmd = args
                    .get("command")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("command required"))?;
                ensure!(
                    !cmd.trim().is_empty() && cmd.len() <= 2048,
                    "command must be 1..2048 UTF-8 bytes"
                );
                ensure!(
                    !cmd.chars()
                        .any(|c| c.is_control() && c != '\n' && c != '\t'),
                    "terminal control characters are not allowed in shell text"
                );
                ensure!(!cmd.chars().any(|c|matches!(c,'\u{061c}'|'\u{200b}'..='\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}'|'\u{feff}')),"invisible directional characters are not allowed");
                let index = COMMANDS.iter().position(|s| *s == cmd);
                ensure!(
                    cmd.len() + cmd.bytes().filter(|b| *b == b'\'').count() * 3 + 400 < 4000,
                    "quoted command exceeds a 4000-byte terminal line; submit a smaller script"
                );
                ensure!(
                    s.lease_custom || index.is_some(),
                    "diagnostic mode only allows exact templates; explicitly acquire custom mode for other commands"
                );
                ensure!(
                    !s.lease_custom
                        || s.flags & (CUSTOM_CAPABLE | CUSTOM_CONTROL)
                            == CUSTOM_CAPABLE | CUSTOM_CONTROL,
                    "native custom authorization is absent"
                );
                let req = args
                    .get("request_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty() && s.len() <= 128)
                    .ok_or_else(|| anyhow::anyhow!("request_id must be 1..128 bytes"))?;
                for (tid, t) in &self.tasks {
                    if t.client == client && t.session == id && t.request == req {
                        ensure!(t.command == cmd, "request_id reused with different command");
                        return Ok(json!({"task_id":tid,"session_id":id,"status":t.status}));
                    }
                }
                ensure!(
                    s.active.is_none(),
                    "session has an unresolved command; confirm idle shell locally before reacquiring"
                );
                ensure!(self.tasks.len() < 256, "task capacity reached");
                let timeout = args
                    .get("execution_timeout_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(30000);
                ensure!(
                    (1000..=3600000).contains(&timeout),
                    "timeout must be 1000..3600000 ms"
                );
                let tid = Uuid::new_v4();
                let mut f = Frame::new(
                    if s.lease_custom {
                        COMMAND_CUSTOM
                    } else {
                        COMMAND
                    },
                    id,
                );
                f.request = tid;
                f.epoch = s.epoch;
                f.data.extend_from_slice(client.as_bytes());
                if s.lease_custom {
                    f.data.extend_from_slice(&(timeout as u32).to_le_bytes());
                    f.data.extend_from_slice(cmd.as_bytes());
                } else {
                    f.data
                        .extend_from_slice(&(index.unwrap() as u32).to_le_bytes());
                    f.data.extend_from_slice(&(timeout as u32).to_le_bytes());
                }
                s.tx.try_send(f)
                    .map_err(|_| anyhow::anyhow!("native queue full"))?;
                s.active = Some(tid);
                let mut output = OutputRing::new(256 * 1024);
                output.advance(s.output.end);
                self.tasks.insert(
                    tid,
                    Task {
                        client,
                        session: id,
                        status: "queued",
                        exit: None,
                        error: None,
                        deadline: Instant::now() + Duration::from_millis(timeout),
                        output,
                        token: tid.simple().to_string(),
                        scan: vec![],
                        started: false,
                        complete: false,
                        request: req.into(),
                        command: cmd.into(),
                        created: Instant::now(),
                    },
                );
                Ok(
                    json!({"task_id":tid,"session_id":id,"status":"queued","requires_local_command_approval":true}),
                )
            }
            _ => bail!("unknown tool"),
        }
    }
}
impl Task {
    fn detect(&mut self) {
        let begin = format!("\x1ePAI:{}:BEGIN\x1f", self.token);
        if self
            .scan
            .windows(begin.len())
            .any(|w| w == begin.as_bytes())
        {
            self.started = true;
        }
        if !self.started {
            return;
        }
        let end = format!("\x1ePAI:{}:END:", self.token);
        if let Some(pos) = self
            .scan
            .windows(end.len())
            .position(|w| w == end.as_bytes())
        {
            let tail = &self.scan[pos + end.len()..];
            if let Some(stop) = tail.iter().position(|b| *b == 0x1f)
                && let Ok(text) = std::str::from_utf8(&tail[..stop])
                && let Ok(code) = text.parse::<i32>()
                && (0..=255).contains(&code)
            {
                self.exit = Some(code);
                self.complete = true;
                self.created = Instant::now();
                self.status = if code == 0 { "completed" } else { "failed" };
            }
        }
    }
}
fn arg_uuid(v: &Value, k: &str) -> Result<Uuid> {
    Ok(Uuid::parse_str(
        v.get(k)
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("{k} required"))?,
    )?)
}
fn limit(v: &Value, k: &str) -> Result<usize> {
    let n = v
        .get(k)
        .and_then(Value::as_u64)
        .unwrap_or((OUTPUT_LIMIT / 8) as u64) as usize;
    ensure!(
        (4..=OUTPUT_LIMIT / 8).contains(&n),
        "{k} must be 4..8192 bytes (escaped MCP output is bounded to 64 KiB)"
    );
    Ok(n)
}
