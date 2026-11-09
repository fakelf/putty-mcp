use crate::model::{ApprovalMode, Model, Session, arg_uuid};
use anyhow::{Result, ensure};
use putty_ai_common::protocol::*;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use uuid::Uuid;

const MAX_CHILDREN: usize = 4;
const MAX_SESSIONS: usize = 32;
const MAX_LIFECYCLE: usize = 128;
const APPROVAL_IDLE: Duration = Duration::from_secs(300);
const COMPLETION_TIMEOUT: Duration = Duration::from_secs(30);
const HISTORY_TTL: Duration = Duration::from_secs(600);
const PARENT_READY: u32 = READ | CONNECTED | CHILD_MODE;
const CHILD_READY: u32 = READ | CONNECTED | CHILD_SESSION;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Open,
    Close,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Queued,
    Authorized,
    Accepted,
    Revoking,
    Completed,
    Rejected,
    Failed,
    Unknown,
}

impl Stage {
    fn pending(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Authorized | Self::Accepted | Self::Revoking
        )
    }

    fn committed(self) -> bool {
        matches!(self, Self::Authorized | Self::Accepted)
    }

    fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Authorized => "authorized",
            Self::Accepted => "starting",
            Self::Completed => "completed",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
            Self::Unknown | Self::Revoking => "unknown",
        }
    }
}

pub(crate) struct ChildTask {
    operation: Operation,
    client: Uuid,
    source: String,
    request_id: String,
    session: Uuid,
    generation: Uuid,
    parent: Uuid,
    parent_generation: Uuid,
    epoch: u64,
    approval: ApprovalMode,
    stage: Stage,
    updated: Instant,
    error: Option<&'static str>,
}

impl ChildTask {
    fn set(&mut self, stage: Stage, error: Option<&'static str>) {
        self.stage = stage;
        self.error = error;
        self.updated = Instant::now();
    }

    fn result(&self, id: Uuid) -> Value {
        let child = if self.operation == Operation::Open {
            id
        } else {
            self.session
        };
        let status = if self.operation == Operation::Close && self.stage == Stage::Accepted {
            "closing"
        } else {
            self.stage.name()
        };
        json!({"task_id":id,"session_id":self.session,"child_session_id":child,
            "parent_session_id":self.parent,"status":status,"error":self.error,
            "source":self.source,"client_id":self.client,"approval_mode":self.approval.name(),
            "requires_local_command_approval":self.approval.requires_local()})
    }

    fn audit(&self, id: Uuid, event: &'static str) -> Value {
        json!({"event":event,"client":self.client,"source":self.source,"session":self.session,
            "parent_session_id":self.parent,"task":id,"status":self.stage.name(),"approval_mode":self.approval.name()})
    }
}

impl Model {
    pub(crate) fn register_native(
        &mut self,
        id: Uuid,
        mut session: Session,
        parent_id: Option<Uuid>,
    ) -> Result<Option<Value>> {
        ensure!(
            self.sessions.len() < MAX_SESSIONS && !self.sessions.contains_key(&id),
            "session limit or duplicate identity"
        );
        let event = if let Some(parent_id) = parent_id {
            ensure!(
                session.flags & (CHILD_READY | CHILD_MODE) == CHILD_READY,
                "invalid child registration flags"
            );
            let parent = self
                .sessions
                .get(&parent_id)
                .ok_or_else(|| anyhow::anyhow!("unknown child parent"))?;
            ensure!(
                parent.parent.is_none()
                    && parent.flags & PARENT_READY == PARENT_READY
                    && parent.pid == session.pid
                    && parent.start == session.start
                    && parent.approval_mode() == session.approval_mode()
                    && parent.lifecycle == Some(id),
                "child registration does not match its live parent"
            );
            let task = self
                .child_tasks
                .get_mut(&id)
                .ok_or_else(|| anyhow::anyhow!("unknown child opening request"))?;
            ensure!(
                task.operation == Operation::Open
                    && task.stage.committed()
                    && task.session == parent_id
                    && task.generation == parent.generation
                    && task.approval == parent.approval_mode(),
                "child registration lacks committed local approval"
            );
            session.parent = Some((parent_id, parent.generation));
            task.set(Stage::Completed, None);
            let event = task.audit(id, "child_registered");
            self.release_child_operation(parent_id, id);
            Some(event)
        } else {
            ensure!(
                session.flags & CHILD_SESSION == 0,
                "child registration requires parent identity"
            );
            ensure!(
                session.flags & AUTO_RUN == 0 || session.flags & CHILD_MODE != 0,
                "automatic execution requires multi-child mode"
            );
            None
        };
        self.sessions.insert(id, session);
        Ok(event)
    }

    pub(crate) fn request_child_lifecycle(
        &mut self,
        client: Uuid,
        source: &str,
        method: &str,
        args: &Value,
    ) -> Result<Value> {
        ensure!(safe_source(source), "invalid adapter source");
        let operation = if method == "open_child_session" {
            Operation::Open
        } else {
            Operation::Close
        };
        let session_id = arg_uuid(args, "session_id")?;
        let request_id = args["request_id"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 128)
            .ok_or_else(|| anyhow::anyhow!("request_id must be 1..128 bytes"))?;
        if let Some((id, task)) = self.child_tasks.iter().find(|(_, task)| {
            task.client == client && task.session == session_id && task.request_id == request_id
        }) {
            ensure!(
                task.operation == operation,
                "request_id reused for another child operation"
            );
            return Ok(task.result(*id));
        }
        ensure!(
            self.child_tasks.len() < MAX_LIFECYCLE,
            "child lifecycle task capacity reached"
        );
        let session = self
            .sessions
            .get(&session_id)
            .ok_or_else(|| anyhow::anyhow!("unknown or closed session_id"))?;
        let (parent_id, parent_generation) = match operation {
            Operation::Open => {
                ensure!(
                    session.parent.is_none() && session.flags & PARENT_READY == PARENT_READY,
                    "opening a child requires an authorized root window in multi-child mode"
                );
                ensure!(
                    self.sessions
                        .values()
                        .filter(|child| child.parent == Some((session_id, session.generation)))
                        .count()
                        < MAX_CHILDREN
                        && self.sessions.len() < MAX_SESSIONS,
                    "child session capacity reached"
                );
                (session_id, session.generation)
            }
            Operation::Close => {
                let parent = session.parent.ok_or_else(|| {
                    anyhow::anyhow!("only a child session can be closed through MCP")
                })?;
                ensure!(
                    session.flags & CHILD_READY == CHILD_READY,
                    "child window is not authorized and connected"
                );
                parent
            }
        };
        ensure!(
            session.active.is_none() && session.lifecycle.is_none(),
            "target session has an unresolved task or child operation"
        );

        let id = Uuid::new_v4();
        let task = ChildTask {
            operation,
            client,
            source: source.into(),
            request_id: request_id.into(),
            session: session_id,
            generation: session.generation,
            parent: parent_id,
            parent_generation,
            epoch: session.epoch,
            approval: session.approval_mode(),
            stage: Stage::Queued,
            updated: Instant::now(),
            error: None,
        };
        let mut frame = Frame::new(
            if operation == Operation::Open {
                CHILD_OPEN
            } else {
                CHILD_CLOSE
            },
            session_id,
        );
        frame.request = id;
        frame.epoch = task.epoch;
        session.stamp_approval(&mut frame);
        frame.data.extend_from_slice(client.as_bytes());
        frame.data.push(source.len() as u8);
        frame.data.extend_from_slice(source.as_bytes());
        session.tx.try_send(frame)?;
        self.sessions.get_mut(&session_id).unwrap().lifecycle = Some(id);
        let result = task.result(id);
        self.child_tasks.insert(id, task);
        Ok(result)
    }

    pub(crate) fn child_task_status(
        &self,
        client: Uuid,
        id: Uuid,
        method: &str,
    ) -> Option<Result<Value>> {
        let task = self.child_tasks.get(&id)?;
        Some((|| {
            ensure!(task.client == client, "task belongs to another MCP client");
            ensure!(
                method == "get_task_status",
                "child lifecycle tasks only support get_task_status"
            );
            Ok(task.result(id))
        })())
    }

    pub(crate) fn child_commit(&mut self, session_id: Uuid, id: Uuid) -> Frame {
        let mut reply = Frame::new(REVOKE, session_id);
        reply.request = id;
        let (Some(session), Some(task)) = (
            self.sessions.get(&session_id),
            self.child_tasks.get_mut(&id),
        ) else {
            return reply;
        };
        reply.epoch = session.epoch;
        let required = if task.operation == Operation::Open {
            PARENT_READY
        } else {
            CHILD_READY
        };
        if task.stage == Stage::Queued
            && task.session == session_id
            && task.generation == session.generation
            && task.epoch == session.epoch
            && task.approval == session.approval_mode()
            && session.flags & required == required
            && session.lifecycle == Some(id)
            && session.active.is_none()
        {
            task.set(Stage::Authorized, None);
            reply.kind = CHILD_COMMIT;
        }
        reply
    }

    pub(crate) fn child_ack(
        &mut self,
        session_id: Uuid,
        id: Uuid,
        epoch: u64,
        ack: u32,
    ) -> Result<Option<Value>> {
        let Some(task) = self.child_tasks.get_mut(&id) else {
            return Ok(None);
        };
        if task.session != session_id || !task.stage.pending() {
            return Ok(None);
        }
        let session = &self.sessions[&session_id];
        if task.generation != session.generation || session.lifecycle != Some(id) {
            return Ok(None);
        }
        match ack {
            CHILD_APPROVED if task.stage == Stage::Queued && task.epoch == epoch => {
                let event = if task.approval.requires_local() {
                    "child_local_approved"
                } else {
                    "child_auto_authorized"
                };
                Ok(Some(task.audit(id, event)))
            }
            CHILD_ACCEPTED if task.stage == Stage::Authorized && task.epoch == epoch => {
                task.set(Stage::Accepted, None);
                Ok(Some(task.audit(id, "child_accepted")))
            }
            CHILD_REJECTED => {
                task.set(
                    Stage::Rejected,
                    Some("PuTTY rejected or revoked the child operation"),
                );
                let event = task.audit(id, "child_rejected");
                self.release_child_operation(session_id, id);
                Ok(Some(event))
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn child_event(&mut self, parent: Uuid, frame: &Frame) -> Result<Option<Value>> {
        let code = u32::from_le_bytes(frame.data.as_slice().try_into()?);
        let id = frame.request;
        let session = &self.sessions[&parent];
        if code == 5 {
            if let Some(task) = self.child_tasks.get_mut(&id)
                && task.operation == Operation::Close
                && task.parent == parent
                && task.parent_generation == session.generation
                && session.parent.is_none()
                && session.flags & PARENT_READY == PARENT_READY
                && task.stage.committed()
            {
                task.set(Stage::Completed, None);
                let event = task.audit(id, "child_closed");
                let (child, generation) = (task.session, task.generation);
                self.disconnect(child, generation);
                return Ok(Some(event));
            }
            return Ok(None);
        }
        let error = match code {
            1 => "SSH server refused the child session channel",
            2 => "SSH server refused a child PTY",
            3 => "SSH server refused a child shell",
            4 => "PuTTY could not start the child window locally",
            _ => anyhow::bail!("invalid child startup event"),
        };
        if let Some(task) = self.child_tasks.get_mut(&id)
            && task.operation == Operation::Open
            && task.session == parent
            && task.generation == session.generation
            && task.stage.committed()
        {
            task.set(Stage::Failed, Some(error));
            let event = task.audit(id, "child_startup_failed");
            self.release_child_operation(parent, id);
            return Ok(Some(event));
        }
        Ok(None)
    }

    fn release_child_operation(&mut self, session: Uuid, id: Uuid) {
        if let Some(s) = self.sessions.get_mut(&session)
            && s.lifecycle == Some(id)
        {
            s.lifecycle = None;
        }
    }

    pub(crate) fn disconnect_children(&mut self, parent: Uuid) {
        let children: Vec<_> = self
            .sessions
            .iter()
            .filter_map(|(&id, session)| {
                session
                    .parent
                    .is_some_and(|(p, _)| p == parent)
                    .then_some((id, session.generation))
            })
            .collect();
        for (id, generation) in children {
            self.lifecycle_disconnected(id, generation, false);
            self.disconnect(id, generation);
        }
    }

    pub(crate) fn lifecycle_disconnected(
        &mut self,
        session: Uuid,
        generation: Uuid,
        observed: bool,
    ) {
        for task in self
            .child_tasks
            .values_mut()
            .filter(|task| task.stage.pending())
        {
            let target_lost = task.session == session && task.generation == generation;
            let parent_lost = task.parent == session && task.parent_generation == generation;
            let await_parent =
                observed && task.operation == Operation::Close && task.stage.committed();
            if parent_lost || (target_lost && !await_parent) {
                task.set(
                    Stage::Unknown,
                    Some("PuTTY disconnected before the child operation was confirmed"),
                );
            }
        }
    }

    pub(crate) fn check_native_child_state(&mut self, id: Uuid, f: &Frame) -> Result<()> {
        let session = self
            .sessions
            .get(&id)
            .ok_or_else(|| anyhow::anyhow!("session closed"))?;
        let child_flag = if session.parent.is_some() {
            CHILD_SESSION
        } else {
            0
        };
        ensure!(
            f.flags & CHILD_SESSION == child_flag && (child_flag == 0 || f.flags & CHILD_MODE == 0),
            "invalid child session flags"
        );
        let mode_changed = (f.flags ^ session.flags) & AUTO_RUN != 0;
        if child_flag != 0 {
            ensure!(
                !mode_changed,
                "a live child cannot change its approval mode"
            );
        } else {
            ensure!(
                f.flags & AUTO_RUN == 0 || f.flags & CHILD_MODE != 0,
                "automatic execution requires multi-child mode"
            );
        }
        let pending = session.lifecycle;
        let required = if session.parent.is_some() {
            CHILD_READY
        } else {
            PARENT_READY
        };
        let parent_revoked = session.parent.is_none()
            && session.flags & PARENT_READY == PARENT_READY
            && (f.flags & PARENT_READY != PARENT_READY || mode_changed);
        if let Some(task_id) = pending
            && let Some(task) = self.child_tasks.get_mut(&task_id)
        {
            if task.stage == Stage::Queued
                && (f.flags & required != required || f.epoch != task.epoch || mode_changed)
            {
                Self::revoke_queued(
                    session,
                    task_id,
                    task,
                    "target PuTTY session changed before approval",
                );
            } else if parent_revoked && task.stage.committed() {
                task.set(Stage::Unknown, Some("parent authorization changed before child registration; SSH outcome unknown"));
                self.release_child_operation(id, task_id);
            }
        }
        if parent_revoked {
            self.disconnect_children(id);
            let generation = self.sessions[&id].generation;
            for task in self.child_tasks.values_mut().filter(|task| {
                task.operation == Operation::Close
                    && task.stage.pending()
                    && task.parent == id
                    && task.parent_generation == generation
            }) {
                task.set(
                    Stage::Unknown,
                    Some("parent authorization changed before child closure was confirmed"),
                );
            }
        }
        Ok(())
    }

    pub(crate) fn lifecycle_client_gone(&mut self, client: Uuid) {
        for (&id, task) in self
            .child_tasks
            .iter_mut()
            .filter(|(_, task)| task.client == client && task.stage == Stage::Queued)
        {
            if let Some(session) = self.sessions.get(&task.session) {
                Self::revoke_queued(
                    session,
                    id,
                    task,
                    "MCP client disconnected before local approval",
                );
            }
        }
    }

    fn revoke_queued(session: &Session, id: Uuid, task: &mut ChildTask, error: &'static str) {
        let mut frame = Frame::new(REVOKE, task.session);
        frame.request = id;
        frame.epoch = session.epoch;
        let _ = session.tx.try_send(frame);
        task.set(Stage::Revoking, Some(error));
    }

    pub(crate) fn expire_lifecycle(&mut self, now: Instant) {
        let mut released = Vec::new();
        self.child_tasks.retain(|&id, task| {
            let age = now.duration_since(task.updated);
            if task.stage == Stage::Queued
                && age >= APPROVAL_IDLE
                && let Some(session) = self.sessions.get(&task.session)
            {
                Self::revoke_queued(
                    session,
                    id,
                    task,
                    "child approval expired; awaiting PuTTY revocation confirmation",
                );
            } else if task.stage.committed() && age >= COMPLETION_TIMEOUT {
                let error = if task.operation == Operation::Open {
                    released.push((task.session, id));
                    "child window registration timed out; SSH outcome unknown"
                } else {
                    "child closure was not confirmed by its parent window; SSH outcome unknown"
                };
                task.set(Stage::Unknown, Some(error));
            }
            task.stage.pending()
                || self
                    .sessions
                    .get(&task.session)
                    .is_some_and(|s| s.lifecycle == Some(id))
                || now.duration_since(task.updated) < HISTORY_TTL
        });
        for (session, id) in released {
            self.release_child_operation(session, id);
        }
    }
}
