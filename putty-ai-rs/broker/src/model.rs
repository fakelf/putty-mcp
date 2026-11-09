use crate::{
    child::ChildTask,
    foreground::{Foreground, Input},
    interactive::{Profile, TopKey, TopTask},
};
use anyhow::{Result, bail, ensure};
use putty_ai_common::{OUTPUT_LIMIT, protocol::*, terminal::OutputRing};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use uuid::Uuid;

const INTERRUPT_OBSERVATION: Duration = Duration::from_secs(5);
const PROMPT_TAIL_MAX: usize = 4096;
const PROMPT_SETTLE: Duration = Duration::from_millis(100);
const FOREGROUND_READY: u32 = READ | CONNECTED | TERMINAL_CAPABLE;

pub struct Session {
    pub generation: Uuid,
    pub pid: u32,
    pub start: u64,
    pub hwnd: u64,
    pub flags: u32,
    pub epoch: u64,
    pub output: OutputRing,
    pub tx: mpsc::Sender<Frame>,
    pub active: Option<Uuid>,
    pub lifecycle: Option<Uuid>,
    pub parent: Option<(Uuid, Uuid)>,
    pub received: u64,
    pub last_seen: Instant,
    pub lost_total: u64,
    pub native_dropped: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalMode {
    PerAction,
    Automatic,
}

impl ApprovalMode {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::PerAction => "per_action",
            Self::Automatic => "automatic",
        }
    }

    pub(crate) fn requires_local(self) -> bool {
        self == Self::PerAction
    }

    fn request_flags(self) -> u32 {
        match self {
            Self::PerAction => 0,
            Self::Automatic => REQUEST_AUTO_RUN,
        }
    }
}

impl Session {
    pub(crate) fn approval_mode(&self) -> ApprovalMode {
        if self.flags & AUTO_RUN != 0 {
            ApprovalMode::Automatic
        } else {
            ApprovalMode::PerAction
        }
    }

    pub(crate) fn stamp_approval(&self, frame: &mut Frame) {
        frame.flags |= self.approval_mode().request_flags();
        frame.seq = self.approval_generation();
    }

    pub(crate) fn approval_generation(&self) -> u64 {
        // Native capabilities occupy the low byte. The remaining bits hold the approval generation.
        u64::from(self.flags >> 8)
    }
}

pub struct Task {
    pub client: Uuid,
    pub session: Uuid,
    pub status: &'static str,
    pub error: Option<String>,
    pub deadline: Option<Instant>,
    pub execution_timeout: Duration,
    pub source: String,
    pub epoch: u64,
    approval: ApprovalMode,
    approval_generation: u64,
    attached: bool,
    attach_committed: bool,
    foreground: Foreground,
    pub interrupt: Option<&'static str>,
    interrupt_approval: Option<ApprovalMode>,
    interrupt_probe: Option<(u64, Instant)>,
    shell_prompt_observed: bool,
    pub output: OutputRing,
    scan: Vec<u8>,
    queued_after: u64,
    result_start: Option<u64>,
    last_gap_end: Option<u64>,
    expected_prompt: Option<String>,
    native_prompt: Option<String>,
    pub(crate) prompt_candidate: Option<Instant>,
    observation_detached: bool,
    pub request: String,
    pub command: String,
    pub created: Instant,
}

#[derive(Default)]
pub struct Model {
    pub sessions: HashMap<Uuid, Session>,
    pub tasks: HashMap<Uuid, Task>,
    pub top_tasks: HashMap<Uuid, TopTask>,
    pub(crate) child_tasks: HashMap<Uuid, ChildTask>,
    pub adapter_count: usize,
}

struct CommandSubmission<'a> {
    text: &'a str,
    timeout: Duration,
    expected_prompt: Option<&'a str>,
}
impl Model {
    pub fn disconnect(&mut self, id: Uuid, generation: Uuid) {
        if self
            .sessions
            .get(&id)
            .is_some_and(|s| s.generation == generation)
        {
            self.disconnect_children(id);
            self.lifecycle_disconnected(id, generation, true);
            self.sessions.remove(&id);
            for t in self.top_tasks.values_mut().filter(|t| t.session == id) {
                t.terminal_gone();
            }
            for t in self.tasks.values_mut().filter(|t| t.session == id) {
                t.observation_detached = true;
                if t.unresolved() {
                    t.status = "unknown";
                    t.created = Instant::now();
                    t.error = Some("terminal disconnected; remote outcome unknown".into());
                }
                if t.interrupt_pending() {
                    t.interrupt = Some("unknown");
                }
                t.output.clear();
                t.foreground.clear();
                t.scan.clear();
                t.result_start = None;
            }
        }
    }
    pub fn client_gone(&mut self, client: Uuid) {
        self.lifecycle_client_gone(client);
        for (id, t) in self
            .top_tasks
            .iter_mut()
            .filter(|(_, t)| t.client == client)
        {
            t.owner_gone(*id, self.sessions.get(&t.session));
        }
        for (tid, t) in self.tasks.iter_mut().filter(|(_, t)| t.client == client) {
            t.observation_detached = true;
            if t.result_start.is_none()
                && let Some(s) = self.sessions.get(&t.session)
                && s.active == Some(*tid)
            {
                let mut revoke = Frame::new(REVOKE, t.session);
                revoke.request = *tid;
                let _ = s.tx.try_send(revoke);
            }
            if let Some(s) = self.sessions.get(&t.session) {
                t.revoke_interrupt(s, *tid);
                t.foreground.revoke(t.session, s);
            }
            if t.unresolved() {
                t.status = "unknown";
                t.created = Instant::now();
                t.error = Some("MCP client disconnected; remote outcome unknown".into());
            }
        }
    }
    pub fn expire(&mut self) {
        let now = Instant::now();
        self.expire_lifecycle(now);
        self.top_tasks.retain(|id, t| {
            if let Some(session) = self.sessions.get(&t.session)
                && session.active == Some(*id)
            {
                t.expire(*id, session, now);
                return true;
            }
            now.duration_since(t.updated) < Duration::from_secs(600)
        });
        for (id, t) in &mut self.tasks {
            if t.status == "interrupting"
                && (t.interrupt_probe.is_some_and(|(_, until)| now >= until)
                    || t.deadline.is_some_and(|deadline| now >= deadline))
            {
                t.status = "unknown";
                t.created = now;
                t.error = Some("Ctrl+C sent; no matching terminal prompt observed".into());
            } else if t.status == "running" && t.deadline.is_some_and(|deadline| now >= deadline) {
                t.error = Some("observation timeout; remote process may still be running".into());
                t.status = "timeout";
                t.created = now;
                if let Some(s) = self.sessions.get(&t.session) {
                    t.revoke_interrupt(s, *id);
                }
            }
        }
        self.tasks.retain(|id, t| {
            t.unresolved()
                || self
                    .sessions
                    .get(&t.session)
                    .is_some_and(|s| s.active == Some(*id))
                || now.duration_since(t.created) < Duration::from_secs(600)
        });
    }
    pub fn native_state(&mut self, id: Uuid, f: &Frame) -> Result<Option<Value>> {
        self.check_native_child_state(id, f)?;
        let s = self
            .sessions
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("session closed"))?;
        let old_epoch = s.epoch;
        let old_approval = s.approval_mode();
        let old_generation = s.approval_generation();
        s.flags = f.flags;
        s.epoch = f.epoch;
        s.last_seen = Instant::now();
        let authorized = f.flags & (READ | CONNECTED) == READ | CONNECTED;
        if (old_epoch != s.epoch || !authorized)
            && let Some(tid) = s.active
        {
            if let Some(t) = self.tasks.get_mut(&tid) {
                t.observation_detached = true;
                if t.unresolved() {
                    t.status = "unknown";
                    t.created = Instant::now();
                    t.error = Some("terminal state changed; remote outcome unknown".into());
                }
                // A pending GUI decision still occupies the native inbox until acknowledged.
                t.revoke_interrupt(s, tid);
                t.foreground.revoke(id, s);
                if t.result_start.is_some() && !t.input_pending() {
                    s.active = None;
                }
            } else if let Some(t) = self.top_tasks.get_mut(&tid) {
                t.revoke_key(tid, s);
            } else {
                s.active = None;
            }
        }
        if (old_approval != s.approval_mode() || old_generation != s.approval_generation())
            && let Some(tid) = s.active
        {
            if let Some(t) = self.tasks.get_mut(&tid) {
                t.revoke_interrupt(s, tid);
                t.foreground.revoke(id, s);
            } else if let Some(t) = self.top_tasks.get_mut(&tid) {
                t.revoke_key(tid, s);
            }
        }
        if f.flags & READ == 0 {
            s.output.clear();
        }
        if f.kind == CHILD_EVENT {
            return self.child_event(id, f);
        }
        if f.kind == TERMINAL_SCREEN {
            ensure!(
                f.request.is_nil() == f.data.is_empty() && f.seq != 0,
                "invalid foreground terminal screen identity"
            );
            if s.flags & FOREGROUND_READY == FOREGROUND_READY
                && let Some(task_id) = s.active
                && (f.data.is_empty() || f.request == task_id)
                && let Some(task) = self.tasks.get_mut(&task_id)
                && task.epoch == f.epoch
                && task.observing_output()
            {
                task.foreground.save_screen(f.seq, &f.data)?;
            }
            return Ok(None);
        }
        if matches!(f.kind, TOP_EVENT | TOP_SCREEN) {
            if f.flags & READ != 0
                && s.active == Some(f.request)
                && let Some(t) = self.top_tasks.get_mut(&f.request)
                && t.session == id
            {
                if f.kind == TOP_EVENT {
                    t.event(&f.data)?;
                    if t.status != "running" || f.epoch != old_epoch {
                        t.revoke_key(f.request, s);
                    }
                    if t.release_slot() {
                        s.active = None;
                    }
                } else {
                    t.save_screen(f.seq, &f.data)?;
                }
            }
            return Ok(None);
        }
        if f.kind == OUTPUT && f.flags & READ != 0 {
            s.lost_total += f.seq.saturating_sub(s.output.end);
            s.output.push(f.seq, &f.data)?;
            s.received += f.data.len() as u64;
            if let Some(task_id) = s.active
                && let Some(t) = self.tasks.get_mut(&task_id)
                && t.observing_output()
            {
                t.capture(f.seq, &f.data)?;
            }
        }
        if f.kind == POLL {
            s.lost_total += f.seq.saturating_sub(s.output.end);
            s.output.advance(f.seq);
            ensure!(f.data.len() >= 12, "invalid native status");
            let ack = u32::from_le_bytes(f.data[..4].try_into()?);
            let (send_offset, native_prompt) = match ack {
                COMMAND_SENT => {
                    ensure!(
                        (20..=20 + PROMPT_MAX).contains(&f.data.len()),
                        "invalid command acknowledgement size"
                    );
                    let offset = u64::from_le_bytes(f.data[12..20].try_into()?);
                    let text = std::str::from_utf8(&f.data[20..])?;
                    let prompt = if text.is_empty() {
                        None
                    } else {
                        Some(validate_prompt(text)?.to_owned())
                    };
                    (Some(offset), prompt)
                }
                TERMINAL_ATTACHED => {
                    ensure!(
                        f.data.len() == 20,
                        "invalid terminal attachment acknowledgement size"
                    );
                    (Some(u64::from_le_bytes(f.data[12..20].try_into()?)), None)
                }
                INTERRUPT_SENT if f.data.len() == 20 => {
                    (Some(u64::from_le_bytes(f.data[12..20].try_into()?)), None)
                }
                _ => {
                    ensure!(f.data.len() == 12, "invalid native status size");
                    (None, None)
                }
            };
            s.native_dropped = u64::from_le_bytes(f.data[4..12].try_into()?);
            if let Some(tid) = s.active
                && let Some(t) = self.tasks.get_mut(&tid)
                && t.observing_output()
            {
                t.advance_capture(f.seq);
                t.confirm_prompt(f.seq);
                if t.shell_prompt_observed {
                    t.revoke_interrupt(s, tid);
                    t.foreground.revoke(id, s);
                    if !t.input_pending() {
                        s.active = None;
                    }
                }
            }
            if matches!(ack, CHILD_APPROVED | CHILD_ACCEPTED | CHILD_REJECTED) {
                return self.child_ack(id, f.request, f.epoch, ack);
            }
            if matches!(
                ack,
                TERMINAL_ATTACHED | TERMINAL_ATTACH_NOT | TERMINAL_ATTACH_APPROVED
            ) {
                return self.foreground_attach_ack(id, f.request, ack, send_offset);
            }
            if matches!(
                ack,
                TERMINAL_INPUT_APPROVED
                    | TERMINAL_INPUT_SENT
                    | TERMINAL_INPUT_NOT_SENT
                    | TERMINAL_DETACHED
                    | TERMINAL_DETACH_NOT_SENT
            ) {
                return self.foreground_action_ack(id, f.request, ack);
            }
            if let Some(t) = self.top_tasks.get_mut(&f.request)
                && t.session == id
            {
                let approval = t.decision_approval(ack);
                let decision = t.ack(ack)?;
                if t.release_slot() && s.active == Some(f.request) {
                    s.active = None;
                }
                return Ok(decision.map(|decision| {
                    json!({"event":"top_decision","client":t.client,
                    "source":t.source,"session":id,"task":f.request,"decision":decision,"approval_mode":approval.name()})
                }));
            }
            if !f.request.is_nil()
                && let Some(t) = self.tasks.get_mut(&f.request)
                && t.session == id
            {
                if matches!(ack, INTERRUPT_NOT_SENT | INTERRUPT_SENT) && t.interrupt_pending() {
                    let decision = if ack == INTERRUPT_SENT {
                        "sent"
                    } else {
                        "not_sent"
                    };
                    t.interrupt = Some(decision);
                    if ack == INTERRUPT_SENT && t.status == "running" {
                        t.status = "interrupting";
                        t.interrupt_probe = Some((
                            send_offset.unwrap_or(t.output.end),
                            Instant::now() + INTERRUPT_OBSERVATION,
                        ));
                        t.observe_prompt();
                    }
                    if (t.shell_prompt_observed || t.epoch != s.epoch)
                        && s.active == Some(f.request)
                    {
                        s.active = None;
                    }
                    return Ok(Some(json!({"event":"interrupt_decision","client":t.client,
                        "source":t.source,"session":id,"task":f.request,"decision":decision,
                        "approval_mode":t.interrupt_approval.map(ApprovalMode::name)})));
                }
                if t.result_start.is_some() || t.status == "cancelled" || t.attached {
                    return Ok(None);
                }
                let decision = match ack {
                    COMMAND_SENT => {
                        t.begin_capture(send_offset.unwrap(), native_prompt)?;
                        if (t.shell_prompt_observed || t.epoch != s.epoch)
                            && s.active == Some(f.request)
                        {
                            s.active = None;
                        }
                        "sent"
                    }
                    COMMAND_NOT_SENT => {
                        t.status = "cancelled";
                        t.created = Instant::now();
                        t.error = Some("PuTTY confirmed command was not sent".into());
                        if s.active == Some(f.request) {
                            s.active = None;
                        }
                        "not_sent"
                    }
                    _ => bail!("invalid command acknowledgement"),
                };
                return Ok(Some(json!({"event":"command_decision","client":t.client,
                    "source":t.source,"session":id,"task":f.request,"decision":decision,"approval_mode":t.approval.name()})));
            }
        }
        Ok(None)
    }

    fn foreground_attach_ack(
        &mut self,
        session_id: Uuid,
        id: Uuid,
        ack: u32,
        offset: Option<u64>,
    ) -> Result<Option<Value>> {
        let session = self.sessions.get_mut(&session_id).unwrap();
        let Some(task) = self.tasks.get_mut(&id) else {
            return Ok(None);
        };
        if !task.attached
            || task.session != session_id
            || session.active != Some(id)
            || task.result_start.is_some()
        {
            return Ok(None);
        }
        let decision = match ack {
            TERMINAL_ATTACH_APPROVED => "ready_to_commit",
            TERMINAL_ATTACHED if task.attach_committed => {
                task.begin_capture(offset.unwrap(), None)?;
                if task.observation_detached || task.epoch != session.epoch {
                    session.active = None;
                }
                "attached"
            }
            TERMINAL_ATTACH_NOT => {
                task.status = "cancelled";
                task.error = Some("PuTTY confirmed terminal attachment did not start".into());
                task.created = Instant::now();
                session.active = None;
                "not_attached"
            }
            _ => return Ok(None),
        };
        Ok(Some(
            json!({"event":"terminal_attachment","client":task.client,"source":task.source,
            "session":session_id,"task":id,"decision":decision,"approval_mode":task.approval.name()}),
        ))
    }

    fn foreground_action_ack(
        &mut self,
        session_id: Uuid,
        id: Uuid,
        ack: u32,
    ) -> Result<Option<Value>> {
        let session = self.sessions.get_mut(&session_id).unwrap();
        let Some(task_id) = session.active else {
            return Ok(None);
        };
        let Some(task) = self.tasks.get_mut(&task_id) else {
            return Ok(None);
        };
        let Some(action) = task.foreground.acknowledge(id, ack) else {
            return Ok(None);
        };
        let event = json!({"event":"terminal_action","client":task.client,"source":task.source,
            "session":session_id,"task":task_id,"action_id":action.id,"decision":action.stage,
            "approval_mode":action.approval.name()});
        if ack == TERMINAL_DETACHED {
            task.observation_detached = true;
            task.prompt_candidate = None;
            if !task.shell_prompt_observed {
                task.status = "detached";
                task.error = None;
                task.created = Instant::now();
            }
        }
        if (task.observation_detached || task.shell_prompt_observed || task.epoch != session.epoch)
            && !task.input_pending()
        {
            session.active = None;
        }
        Ok(Some(event))
    }

    pub(crate) fn foreground_commit(&mut self, session_id: Uuid, id: Uuid) -> Frame {
        self.expire();
        let mut reply = Frame::new(REVOKE, session_id);
        reply.request = id;
        if let Some(session) = self.sessions.get(&session_id)
            && session.flags & FOREGROUND_READY == FOREGROUND_READY
            && let Some(task_id) = session.active
            && let Some(task) = self.tasks.get_mut(&task_id)
            && task.epoch == session.epoch
            && !task.observation_detached
        {
            let authorized = if task.attached && task_id == id && task.status == "queued" {
                if task.attach_committed
                    || task.approval != session.approval_mode()
                    || task.approval_generation != session.approval_generation()
                {
                    false
                } else {
                    task.attach_committed = true;
                    true
                }
            } else {
                let guard = task.foreground.pending_guard(id);
                task.can_send_foreground(guard, false) && task.foreground.commit(id, session)
            };
            if authorized {
                reply.kind = TERMINAL_COMMIT;
                reply.epoch = session.epoch;
            }
        }
        reply
    }

    fn attach_terminal(&mut self, client: Uuid, source: &str, args: &Value) -> Result<Value> {
        ensure!(safe_source(source), "invalid adapter source");
        let session_id = arg_uuid(args, "session_id")?;
        let request = task_request_id(args)?;
        if let Some((id, task)) = self.tasks.iter().find(|(_, task)| {
            task.client == client && task.session == session_id && task.request == request
        }) {
            ensure!(task.attached, "request_id reused for a different task kind");
            return Ok(task.start_result(*id));
        }
        let session = self
            .sessions
            .get_mut(&session_id)
            .ok_or_else(|| anyhow::anyhow!("unknown or closed session_id"))?;
        ensure!(
            session.flags & FOREGROUND_READY == FOREGROUND_READY,
            "terminal is not authorized for foreground attachment"
        );
        ensure!(
            session.active.is_none() && session.lifecycle.is_none(),
            "session has an unresolved task or lifecycle operation"
        );
        ensure!(self.tasks.len() < 256, "task capacity reached");
        let id = Uuid::new_v4();
        let task = Task::new(client, source, session_id, session, request, None);
        session
            .tx
            .try_send(task.request_frame(TERMINAL_ATTACH, id, session))?;
        session.active = Some(id);
        let result = task.start_result(id);
        self.tasks.insert(id, task);
        Ok(result)
    }

    fn foreground_rpc(
        &mut self,
        client: Uuid,
        id: Uuid,
        method: &str,
        args: &Value,
    ) -> Result<Value> {
        let task = self
            .tasks
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("unknown task_id"))?;
        ensure!(task.client == client, "task belongs to another MCP client");
        let session = self.sessions.get(&task.session);
        if method == "read_task_screen" {
            ensure!(
                session.is_none_or(|s| s.flags & READ != 0),
                "terminal access revoked"
            );
            return Ok(task.foreground.read_screen(
                id,
                task.status,
                args["after_frame"].as_u64(),
                task.result_start.is_some(),
            ));
        }
        let request = task_request_id(args)?;
        let (input, if_frame) = Input::parse(args)?;
        if let Some(action) = task.foreground.replay(request, &input, if_frame)? {
            return Ok(action.result(id, task.session, task.status));
        }
        let session = session.ok_or_else(|| anyhow::anyhow!("terminal session closed"))?;
        ensure!(
            session.flags & FOREGROUND_READY == FOREGROUND_READY
                && session.active == Some(id)
                && session.epoch == task.epoch
                && session.lifecycle.is_none(),
            "terminal is not available for this foreground task"
        );
        let detach = input == Input::Detach;
        ensure!(
            task.can_send_foreground(if_frame, detach),
            "foreground task is not accepting input; an unresolved task requires a current if_frame guard"
        );
        ensure!(
            !task.input_pending(),
            "another terminal action is unresolved"
        );
        let kind = if detach {
            TERMINAL_DETACH
        } else {
            TERMINAL_INPUT
        };
        let wire = task.request_frame(kind, Uuid::new_v4(), session);
        let action = task
            .foreground
            .queue(wire, session, id, request, input, if_frame)?;
        Ok(action.result(id, task.session, task.status))
    }

    pub fn top_key_commit(&mut self, session: Uuid, id: Uuid) -> Frame {
        self.expire();
        if let (Some(s), Some(t)) = (self.sessions.get(&session), self.top_tasks.get_mut(&id))
            && t.session == session
        {
            return t.commit(id, s);
        }
        let mut reply = Frame::new(REVOKE, session);
        reply.request = id;
        reply
    }

    fn start_top(&mut self, client: Uuid, source: &str, args: &Value) -> Result<Value> {
        ensure!(safe_source(source), "invalid adapter source");
        let path = match &args["path"] {
            Value::Null => None,
            Value::String(path) => Some(path.as_str()),
            _ => bail!("path must be a string"),
        };
        let profile = Profile::parse(args["program"].as_str().unwrap_or_default(), path)?;
        let session_id = arg_uuid(args, "session_id")?;
        let request = args["request_id"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 128)
            .ok_or_else(|| anyhow::anyhow!("request_id must be 1..128 bytes"))?;
        let s = self
            .sessions
            .get_mut(&session_id)
            .ok_or_else(|| anyhow::anyhow!("unknown or closed session_id"))?;
        ensure!(
            s.flags & (READ | CONNECTED | TOP_CAPABLE) == READ | CONNECTED | TOP_CAPABLE,
            "interactive tasks require an authorized direct SSH-2 window with a known login account"
        );
        if let Some((id, task)) = self
            .top_tasks
            .iter()
            .find(|(_, t)| t.client == client && t.session == session_id && t.request == request)
        {
            ensure!(
                task.profile == profile,
                "request_id reused with a different program or path"
            );
            return Ok(task.result(*id));
        }
        ensure!(
            s.active.is_none() && s.lifecycle.is_none(),
            "session has an unresolved command or interactive task"
        );
        ensure!(
            self.top_tasks.len() < 64,
            "interactive task capacity reached"
        );
        let id = Uuid::new_v4();
        let task = TopTask::new(
            client,
            session_id,
            request,
            source,
            s.epoch,
            profile,
            s.approval_mode(),
        );
        s.tx.try_send(task.start_frame(id, s))?;
        s.active = Some(id);
        let result = task.result(id);
        self.top_tasks.insert(id, task);
        Ok(result)
    }

    fn top_rpc(&mut self, client: Uuid, id: Uuid, method: &str, args: &Value) -> Result<Value> {
        let t = self
            .top_tasks
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("unknown interactive task_id"))?;
        ensure!(t.client == client, "task belongs to another MCP client");
        if method == "get_task_status" {
            return Ok(t.result(id));
        }
        let s = self
            .sessions
            .get(&t.session)
            .ok_or_else(|| anyhow::anyhow!("PuTTY session closed"))?;
        ensure!(s.flags & READ != 0, "terminal access revoked");
        if method == "request_task_action" && args["action"] != "insert_text" {
            ensure!(
                args["text"].is_null(),
                "text is only allowed with insert_text"
            );
        }
        match method {
            "read_task_screen" => Ok(t.read_screen(id, args["after_frame"].as_u64())),
            "request_task_action" if args["action"] == "quit" => t.request_key(id, TopKey::Quit, s),
            "request_task_action" => {
                let request_id = args["request_id"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 128)
                    .ok_or_else(|| {
                        anyhow::anyhow!("request_id must be 1..128 bytes for this action")
                    })?;
                if args["action"] == "insert_text" {
                    let text = args["text"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("insert_text requires text"))?;
                    return t.request_text(id, text, request_id, s);
                }
                if args["action"] == "save" {
                    return t.request_save(id, request_id, s);
                }
                t.request_special_key(
                    id,
                    args["action"].as_str().unwrap_or_default(),
                    request_id,
                    s,
                )
            }
            "interrupt_task" => t.request_key(id, TopKey::Interrupt, s),
            "cancel_task" => {
                t.cancel_start(id, s)?;
                Ok(t.result(id))
            }
            _ => bail!("unknown top tool"),
        }
    }
    pub fn interrupt_commit(&mut self, session: Uuid, task: Uuid) -> Frame {
        let mut reply = Frame::new(REVOKE, session);
        reply.request = task;
        if let (Some(s), Some(t)) = (self.sessions.get(&session), self.tasks.get_mut(&task))
            && t.session == session
            && s.active == Some(task)
            && t.status == "running"
            && t.deadline.is_some_and(|deadline| deadline > Instant::now())
            && t.epoch == s.epoch
            && t.interrupt_approval == Some(s.approval_mode())
            && !t.foreground.pending()
            && s.flags & (READ | CONNECTED) == READ | CONNECTED
            && t.interrupt == Some("pending_approval")
        {
            t.interrupt = Some("authorized");
            reply.kind = SEND_INTERRUPT;
            reply.epoch = s.epoch;
        }
        reply
    }
    pub fn rpc(&mut self, client: Uuid, source: &str, method: &str, args: &Value) -> Result<Value> {
        self.expire();
        if matches!(method, "open_child_session" | "close_child_session") {
            return self.request_child_lifecycle(client, source, method, args);
        }
        if method == "attach_terminal" {
            return self.attach_terminal(client, source, args);
        }
        if method == "start_interactive" {
            return self.start_top(client, source, args);
        }
        if method == "list_sessions" {
            let list:Vec<_>=self.sessions.iter().filter(|(_,s)|s.flags&READ!=0 && s.flags&CONNECTED!=0).map(|(id,s)|json!({
                "session_id":id,"display_name":format!("PuTTY {} (target must be confirmed locally)",&id.to_string()[..8]),
                "connection_state":"connected","ai_access_state":"read_only",
                "command_state":if s.active.is_some() || s.lifecycle.is_some(){"busy"}else{"idle"},
                "parent_session_id":s.parent.map(|(parent, _)| parent),
                "mode":if s.parent.is_some(){"child"}else if s.flags & CHILD_MODE != 0 {"multi_child"}else{"normal"},
                "approval_mode":s.approval_mode().name(),
                "available_capabilities":({
                    let mut caps = vec!["raw_capture","sanitized_transcript","custom_commands"];
                    if s.approval_mode().requires_local() {
                        caps.extend(["local_per_command_approval", "local_interrupt_approval"]);
                    } else {
                        caps.push("automatic_execution");
                    }
                    if s.flags & TOP_CAPABLE != 0 {
                        caps.extend([
                            "interactive_top",
                            "interactive_vi",
                            "interactive_keys",
                            "interactive_vi_file",
                        ]);
                    }
                    if s.flags & TERMINAL_CAPABLE != 0 {
                        caps.extend(["terminal_attach", "terminal_input", "terminal_screen"]);
                    }
                    if s.parent.is_some() {
                        caps.push("child_close");
                    } else if s.flags & CHILD_MODE != 0 {
                        caps.push("child_open");
                    }
                    caps
                }),
                "pid":s.pid,"process_start":s.start,"hwnd":s.hwnd,"received_bytes":s.received,"output_cursor":s.output.end,
                "last_seen_ms":s.last_seen.elapsed().as_millis()
                ,"lost_capture_bytes":s.lost_total,"native_ring_dropped_bytes":s.native_dropped
            })).collect();
            return Ok(json!({"sessions":list}));
        }
        if matches!(
            method,
            "read_task_screen"
                | "request_task_action"
                | "get_task_status"
                | "cancel_task"
                | "interrupt_task"
        ) {
            let id = arg_uuid(args, "task_id")?;
            if let Some(result) = self.child_task_status(client, id, method) {
                return result;
            }
            if self.top_tasks.contains_key(&id) {
                return self.top_rpc(client, id, method, args);
            }
            if matches!(method, "read_task_screen" | "request_task_action") {
                return self.foreground_rpc(client, id, method, args);
            }
        }
        if matches!(method, "get_task_status" | "cancel_task" | "interrupt_task") {
            let id = arg_uuid(args, "task_id")?;
            let t = self
                .tasks
                .get_mut(&id)
                .ok_or_else(|| anyhow::anyhow!("unknown task_id"))?;
            ensure!(t.client == client, "task belongs to another MCP client");
            let Some(s) = self.sessions.get(&t.session) else {
                return Ok(t.with_action_status(
                    json!({"task_id":id,"session_id":t.session,"status":t.status,"exit_code":Value::Null,
                    "task_kind":t.kind(),
                    "output":"","next_cursor":t.output.end,"output_truncated":true,"error":t.error,
                    "approval_mode":t.approval.name(),"requires_local_command_approval":t.approval.requires_local(),
                    "interrupt_approval_mode":t.interrupt_approval.map(ApprovalMode::name),
                    "interrupt_status":t.interrupt,"shell_prompt_observed":t.shell_prompt_observed,
                    "completion":t.completion(),"command_start_offset":t.result_start,"remote_process_stopped":false}),
                ));
            };
            ensure!(s.flags & READ != 0, "terminal access revoked");
            if method == "interrupt_task" {
                ensure!(
                    !t.attached,
                    "use request_task_action press_key with ctrl+c on attached terminals"
                );
                if t.interrupt.is_none() {
                    ensure!(
                        t.status == "running"
                            && s.active == Some(id)
                            && t.epoch == s.epoch
                            && !t.foreground.pending()
                            && s.flags & CONNECTED != 0,
                        "only the current running task can request an interrupt"
                    );
                    let approval = s.approval_mode();
                    s.tx.try_send(t.request_frame(INTERRUPT, id, s))
                        .map_err(|_| anyhow::anyhow!("native queue full"))?;
                    t.interrupt = Some("pending_approval");
                    t.interrupt_approval = Some(approval);
                }
                let approval = t.interrupt_approval.unwrap_or(t.approval);
                return Ok(
                    json!({"task_id":id,"session_id":t.session,"status":t.status,
                    "interrupt_status":t.interrupt,"source":t.source,"client_id":t.client,
                    "approval_mode":approval.name(),"requires_local_command_approval":approval.requires_local(),
                    "remote_process_stopped":false}),
                );
            }
            if method == "cancel_task" {
                if t.status == "queued" && s.active == Some(id) {
                    let mut revoke = Frame::new(REVOKE, t.session);
                    revoke.request = id;
                    s.tx.try_send(revoke).map_err(|_| {
                        anyhow::anyhow!("native queue full; cancellation not confirmed")
                    })?;
                    // Delivery can race a local approval. Never claim that an in-flight command was stopped.
                    t.status = "unknown";
                    t.observation_detached = true;
                    t.created = Instant::now();
                    t.error =
                        Some("revocation requested; command may already have been sent".into());
                } else if matches!(t.status, "running" | "interrupting") {
                    ensure!(
                        !t.attached,
                        "use request_task_action detach to release a terminal attachment"
                    );
                    bail!(
                        "command already sent; use interrupt_task to request local Ctrl+C approval"
                    );
                }
            }
            let cursor = args.get("output_cursor").and_then(Value::as_u64);
            let max = limit(args, "max_output_bytes")?;
            let view = args
                .get("output_view")
                .and_then(Value::as_str)
                .unwrap_or("command");
            let clean = match view {
                "command" => true,
                "transcript" => false,
                _ => bail!("output_view must be command or transcript"),
            };
            ensure!(
                cursor.is_none_or(|pos| pos <= t.output.end),
                "cursor is ahead of task output"
            );
            let start = t.result_start.unwrap_or(t.queued_after);
            let end = t.result_start.map_or(start, |_| t.output.end);
            let mut p =
                t.output
                    .read_range(cursor.unwrap_or(start).clamp(start, end), end, max, clean)?;
            p.truncated |= t.observation_detached && t.result_start.is_some();
            return Ok(t.with_action_status(
                json!({"task_id":id,"session_id":t.session,"status":t.status,"exit_code":Value::Null,"output":p.output,
                "task_kind":t.kind(),"command":(!t.attached).then_some(&t.command),"output_view":view,"next_cursor":p.next_cursor,
                "output_truncated":p.truncated,"lost_bytes":p.lost_bytes,
                "missing_ranges":p.missing_ranges,"error":t.error,"interrupt_status":t.interrupt,
                "approval_mode":t.approval.name(),"requires_local_command_approval":t.approval.requires_local(),
                "interrupt_approval_mode":t.interrupt_approval.map(ApprovalMode::name),
                "shell_prompt_observed":t.shell_prompt_observed,"completion":t.completion(),
                "command_start_offset":t.result_start,"remote_process_stopped":false}),
            ));
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
            "submit_command" => {
                ensure!(safe_source(source), "invalid adapter source");
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
                let expected_prompt = match &args["expected_prompt"] {
                    Value::Null => None,
                    Value::String(prompt) => Some(validate_prompt(prompt)?),
                    _ => bail!("expected_prompt must be a string"),
                };
                let req = task_request_id(args)?;
                let timeout = args
                    .get("execution_timeout_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(30000);
                ensure!(
                    (1000..=3600000).contains(&timeout),
                    "timeout must be 1000..3600000 ms"
                );
                let execution_timeout = Duration::from_millis(timeout);
                for (tid, t) in &self.tasks {
                    if t.client == client && t.session == id && t.request == req {
                        ensure!(
                            !t.attached
                                && t.command == cmd
                                && t.execution_timeout == execution_timeout
                                && t.expected_prompt.as_deref() == expected_prompt,
                            "request_id reused with different command, timeout or expected prompt"
                        );
                        return Ok(t.start_result(*tid));
                    }
                }
                ensure!(
                    s.active.is_none() && s.lifecycle.is_none(),
                    "session has an unresolved command; finish it or confirm idle shell locally"
                );
                ensure!(self.tasks.len() < 256, "task capacity reached");
                let tid = Uuid::new_v4();
                let submission = CommandSubmission {
                    text: cmd,
                    timeout: execution_timeout,
                    expected_prompt,
                };
                let task = Task::new(client, source, id, s, req, Some(submission));
                s.tx.try_send(task.request_frame(COMMAND, tid, s))
                    .map_err(|_| anyhow::anyhow!("native queue full"))?;
                s.active = Some(tid);
                let result = task.start_result(tid);
                self.tasks.insert(tid, task);
                Ok(result)
            }
            _ => bail!("unknown tool"),
        }
    }
}
impl Task {
    fn new(
        client: Uuid,
        source: &str,
        session_id: Uuid,
        session: &Session,
        request: &str,
        submission: Option<CommandSubmission<'_>>,
    ) -> Self {
        let queued_after = session.output.end;
        let mut output = OutputRing::new(256 * 1024);
        output.advance(queued_after);
        Self {
            client,
            session: session_id,
            status: "queued",
            error: None,
            deadline: None,
            execution_timeout: submission.as_ref().map_or(Duration::ZERO, |v| v.timeout),
            source: source.into(),
            epoch: session.epoch,
            approval: session.approval_mode(),
            approval_generation: session.approval_generation(),
            attached: submission.is_none(),
            attach_committed: false,
            foreground: Foreground::default(),
            interrupt: None,
            interrupt_approval: None,
            interrupt_probe: None,
            shell_prompt_observed: false,
            output,
            scan: vec![],
            queued_after,
            result_start: None,
            last_gap_end: None,
            expected_prompt: submission
                .as_ref()
                .and_then(|v| v.expected_prompt)
                .map(str::to_owned),
            native_prompt: None,
            prompt_candidate: None,
            observation_detached: false,
            request: request.into(),
            command: submission.map_or_else(String::new, |v| v.text.into()),
            created: Instant::now(),
        }
    }

    fn kind(&self) -> &'static str {
        if self.attached {
            "terminal_attachment"
        } else {
            "command"
        }
    }

    fn start_result(&self, id: Uuid) -> Value {
        json!({"task_id":id,"session_id":self.session,"status":self.status,"source":self.source,
            "client_id":self.client,"task_kind":self.kind(),"approval_mode":self.approval.name(),
            "requires_local_command_approval":self.approval.requires_local()})
    }

    fn with_action_status(&self, mut result: Value) -> Value {
        if let Some(action) = self.foreground.latest_action() {
            result
                .as_object_mut()
                .unwrap()
                .extend(action.metadata().as_object().unwrap().clone());
        }
        result
    }

    fn input_pending(&self) -> bool {
        self.interrupt_pending() || self.foreground.pending()
    }

    fn can_send_foreground(&self, guard: Option<u64>, detach: bool) -> bool {
        self.result_start.is_some()
            && !self.observation_detached
            && !self.shell_prompt_observed
            && (detach
                || (self.prompt_candidate.is_none()
                    && (matches!(self.status, "running" | "interrupting")
                        || (matches!(self.status, "timeout" | "unknown") && guard.is_some()))))
    }

    fn unresolved(&self) -> bool {
        matches!(self.status, "queued" | "running" | "interrupting")
    }
    fn observing_output(&self) -> bool {
        !self.observation_detached
            && !self.shell_prompt_observed
            && (self.unresolved() || self.result_start.is_some())
    }
    fn request_frame(&self, kind: u16, task: Uuid, session: &Session) -> Frame {
        let mut frame = Frame::new(kind, self.session);
        frame.request = task;
        frame.epoch = session.epoch;
        session.stamp_approval(&mut frame);
        frame.data.extend_from_slice(self.client.as_bytes());
        frame.data.push(self.source.len() as u8);
        frame.data.extend_from_slice(self.source.as_bytes());
        if matches!(kind, COMMAND | INTERRUPT) {
            frame.data.extend_from_slice(self.command.as_bytes());
        }
        frame
    }
    fn interrupt_pending(&self) -> bool {
        matches!(
            self.interrupt,
            Some("pending_approval" | "authorized" | "cancelling")
        )
    }
    fn revoke_interrupt(&mut self, session: &Session, task: Uuid) {
        if matches!(self.interrupt, Some("pending_approval" | "authorized")) {
            let mut revoke = Frame::new(REVOKE, self.session);
            revoke.request = task;
            if session.tx.try_send(revoke).is_ok() {
                self.interrupt = Some("cancelling");
            }
        }
    }
    fn completion(&self) -> Option<&'static str> {
        self.shell_prompt_observed.then_some("prompt_observed")
    }

    fn begin_capture(&mut self, offset: u64, prompt: Option<String>) -> Result<()> {
        ensure!(
            offset >= self.queued_after,
            "foreground capture offset predates queueing"
        );
        self.result_start = Some(offset);
        self.native_prompt = prompt;
        if !self.attached {
            self.deadline = Some(Instant::now() + self.execution_timeout);
        }
        if self.status == "queued" {
            self.status = "running";
        }
        if offset > self.output.end {
            // Unsent pre-command chunks can still arrive after this acknowledgement.
            self.output.advance(offset);
            self.scan.clear();
        }
        if self.last_gap_end.is_some_and(|end| end > offset) {
            self.note_gap_status();
        }
        self.observe_prompt();
        Ok(())
    }

    fn note_gap_status(&mut self) {
        if matches!(self.status, "running" | "interrupting") {
            self.status = "unknown";
            self.created = Instant::now();
            self.error =
                Some("output gap during command; matching terminal prompt not yet observed".into());
        }
    }

    fn advance_capture(&mut self, end: u64) {
        if end <= self.output.end {
            return;
        }
        self.output.advance(end);
        self.scan.clear();
        self.prompt_candidate = None;
        self.last_gap_end = Some(end);
        if self.result_start.is_some() {
            self.note_gap_status();
        }
    }

    fn capture(&mut self, seq: u64, data: &[u8]) -> Result<()> {
        if seq + data.len() as u64 <= self.output.end {
            return Ok(());
        }
        let from = self.output.end.saturating_sub(seq) as usize;
        let seq = seq + from as u64;
        let data = &data[from..];
        self.advance_capture(seq);
        self.output.push(seq, data)?;
        let part = &data[data.len().saturating_sub(PROMPT_TAIL_MAX)..];
        let drop = (self.scan.len() + part.len()).saturating_sub(PROMPT_TAIL_MAX);
        self.scan.drain(..drop);
        self.scan.extend_from_slice(part);
        self.observe_prompt();
        Ok(())
    }

    fn observe_prompt(&mut self) {
        self.prompt_candidate = self.tail_matches_prompt().then(Instant::now);
    }

    fn tail_matches_prompt(&self) -> bool {
        let Some(offset) = self.result_start.filter(|_| self.observing_output()) else {
            return false;
        };
        let Some(prompt) = self
            .expected_prompt
            .as_ref()
            .or(self.native_prompt.as_ref())
        else {
            return false;
        };
        let floor = self
            .interrupt_probe
            .map_or(offset, |(sent_at, _)| offset.max(sent_at));
        if floor >= self.output.end {
            return false;
        }
        let tail_offset = self.output.end - self.scan.len() as u64;
        let first = floor.saturating_sub(tail_offset) as usize;
        let tail = &self.scan[first..];
        // A returned prompt is the open, whole last line following a post-send newline.
        // Never accept a substring, an old line, or a newline-terminated echo.
        let Some(newline) = tail.iter().rposition(|&byte| byte == b'\n') else {
            return false;
        };
        normalize_prompt_line(&tail[newline + 1..]).as_deref() == Some(prompt.as_str())
    }

    fn confirm_prompt(&mut self, seq: u64) {
        if seq == self.output.end
            && self
                .prompt_candidate
                .is_some_and(|since| since.elapsed() >= PROMPT_SETTLE)
            && self.observing_output()
        {
            self.prompt_candidate = None;
            self.shell_prompt_observed = true;
            self.status = "completed";
            self.created = Instant::now();
            self.error = None;
        }
    }
}

fn validate_prompt(text: &str) -> Result<&str> {
    let trimmed = text.trim();
    ensure!(
        !trimmed.is_empty()
            && text.len() <= PROMPT_MAX
            && !text.chars().any(|c| c.is_control()
                || matches!(c,
                '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' |
                '\u{2066}'..='\u{2069}' | '\u{feff}')),
        "expected_prompt must be a nonempty single line of at most 256 UTF-8 bytes without controls"
    );
    Ok(trimmed)
}

fn normalize_prompt_line(mut raw: &[u8]) -> Option<String> {
    let mut text = Vec::with_capacity(raw.len());
    while let Some((&first, tail)) = raw.split_first() {
        match first {
            b'\r' => text.clear(),
            0x1b => {
                let (&kind, body) = tail.split_first()?;
                raw = match kind {
                    b'[' => {
                        let end = body.iter().position(|b| (0x40..=0x7e).contains(b))?;
                        let args = &body[..end];
                        match body[end] {
                            b'm' => {}
                            b'h' | b'l' if args.starts_with(b"?") => {}
                            b'K' if matches!(args, b"" | b"0") => {}
                            b'K' if args == b"2" => text.clear(),
                            _ => return None,
                        }
                        &body[end + 1..]
                    }
                    b']' => {
                        let end = body.iter().enumerate().find_map(|(i, &b)| {
                            if b == 7 {
                                Some(i + 1)
                            } else if body[i..].starts_with(b"\x1b\\") {
                                Some(i + 2)
                            } else {
                                None
                            }
                        })?;
                        &body[end..]
                    }
                    b'(' | b')' if body.starts_with(b"B") => &body[1..],
                    _ => return None,
                };
                continue;
            }
            _ => text.push(first),
        }
        raw = tail;
    }
    let text = std::str::from_utf8(&text).ok()?;
    validate_prompt(text).ok().map(str::to_owned)
}
pub(crate) fn arg_uuid(v: &Value, k: &str) -> Result<Uuid> {
    Ok(Uuid::parse_str(
        v.get(k)
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("{k} required"))?,
    )?)
}
fn task_request_id(args: &Value) -> Result<&str> {
    args["request_id"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .ok_or_else(|| anyhow::anyhow!("request_id must be 1..128 bytes"))
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
