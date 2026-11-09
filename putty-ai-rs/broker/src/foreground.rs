use crate::model::{ApprovalMode, Session};
use anyhow::{Result, bail, ensure};
use putty_ai_common::{protocol::*, terminal::safe_text};
use serde_json::{Value, json};
use std::collections::VecDeque;
use uuid::Uuid;

const ACTION_LIMIT: usize = 512;
const SCREEN_LIMIT: usize = 8;
const TEXT_MAX: usize = 1024;
const SCREEN_MAX: usize = 8192;

#[derive(PartialEq, Eq)]
pub(crate) enum Input {
    Key(String),
    Text(String),
    Paste(String),
    Detach,
}

impl Input {
    pub(crate) fn parse(args: &Value) -> Result<(Self, Option<u64>)> {
        let frame =
            match &args["if_frame"] {
                Value::Null => None,
                value => Some(value.as_u64().filter(|&n| n != 0).ok_or_else(|| {
                    anyhow::anyhow!("if_frame must be a nonzero screen frame ID")
                })?),
            };
        let modifiers = match &args["modifiers"] {
            Value::Null => &[][..],
            Value::Array(values) => values.as_slice(),
            _ => bail!("modifiers must be an array"),
        };
        let input = match args["action"].as_str() {
            Some("press_key") => {
                ensure!(args["text"].is_null(), "press_key accepts a key, not text");
                let key = args["key"]
                    .as_str()
                    .filter(|key| valid_key(key))
                    .ok_or_else(|| anyhow::anyhow!("unknown terminal key"))?;
                let names = ["ctrl", "alt", "shift"];
                let mut mask = 0u8;
                for modifier in modifiers {
                    let index = names
                        .iter()
                        .position(|name| modifier == *name)
                        .ok_or_else(|| anyhow::anyhow!("unknown terminal modifier"))?;
                    ensure!(mask & (1 << index) == 0, "duplicate terminal modifier");
                    mask |= 1 << index;
                }
                let mut chord = String::new();
                for (index, name) in names.into_iter().enumerate() {
                    if mask & (1 << index) != 0 {
                        chord.push_str(name);
                        chord.push('+');
                    }
                }
                chord.push_str(key);
                Self::Key(chord)
            }
            Some("type_text" | "paste_text") => {
                ensure!(
                    args["key"].is_null() && modifiers.is_empty(),
                    "text input does not accept a key or modifiers"
                );
                let paste = args["action"] == "paste_text";
                let text = args["text"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("text is required"))?;
                ensure!((1..=TEXT_MAX).contains(&text.len()) && !text.chars().any(|c| {
                    (c.is_control() && !(paste && matches!(c, '\n' | '\t')))
                        || matches!(c, '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' |
                            '\u{2066}'..='\u{2069}' | '\u{feff}')
                }), "text must be 1..1024 UTF-8 bytes without control characters; bracketed paste also accepts LF and Tab");
                if paste {
                    Self::Paste(text.into())
                } else {
                    Self::Text(text.into())
                }
            }
            Some("detach") => {
                ensure!(
                    frame.is_none()
                        && args["key"].is_null()
                        && args["text"].is_null()
                        && modifiers.is_empty(),
                    "detach does not accept a key, text, modifiers or if_frame"
                );
                Self::Detach
            }
            _ => bail!("unsupported foreground terminal action"),
        };
        Ok((input, frame))
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Key(_) => "press_key",
            Self::Text(_) => "type_text",
            Self::Paste(_) => "paste_text",
            Self::Detach => "detach",
        }
    }

    fn payload(&self) -> (u32, &[u8]) {
        match self {
            Self::Key(key) => (1, key.as_bytes()),
            Self::Text(text) => (2, text.as_bytes()),
            Self::Paste(text) => (3, text.as_bytes()),
            Self::Detach => (0, &[]),
        }
    }
}

fn valid_key(key: &str) -> bool {
    if matches!(key.as_bytes(), [b'!'..=b'~']) {
        return true;
    }
    if let Some(suffix) = key.strip_prefix('f') {
        return matches!(
            suffix.as_bytes(),
            [b'1'..=b'9'] | [b'1', b'0'..=b'9'] | b"20"
        );
    }
    if let Some(suffix) = key.strip_prefix("keypad_") {
        return matches!(suffix.as_bytes(), [b'0'..=b'9'])
            || matches!(
                suffix,
                "decimal" | "add" | "subtract" | "multiply" | "divide" | "enter"
            );
    }
    matches!(
        key,
        "escape"
            | "enter"
            | "tab"
            | "backspace"
            | "space"
            | "up"
            | "down"
            | "left"
            | "right"
            | "home"
            | "end"
            | "page_up"
            | "page_down"
            | "insert"
            | "delete"
    )
}

pub(crate) struct Action {
    pub(crate) id: Uuid,
    request: String,
    input: Input,
    if_frame: Option<u64>,
    pub(crate) stage: &'static str,
    pub(crate) approval: ApprovalMode,
    approval_generation: u64,
}

impl Action {
    pub(crate) fn metadata(&self) -> Value {
        json!({"action_id":self.id,"action_request_id":self.request,"action":self.input.name(),
            "action_status":self.stage,"action_approval_mode":self.approval.name()})
    }

    pub(crate) fn result(&self, task: Uuid, session: Uuid, task_status: &str) -> Value {
        let mut result = json!({"task_id":task,"session_id":session,"status":task_status,
            "approval_mode":self.approval.name(),
            "requires_local_command_approval":self.input != Input::Detach && self.approval.requires_local()});
        result
            .as_object_mut()
            .unwrap()
            .extend(self.metadata().as_object().unwrap().clone());
        result
    }
}

struct Screen {
    id: u64,
    flags: u8,
    columns: u16,
    rows: u16,
    x: u16,
    y: u16,
    text: Vec<u8>,
}

#[derive(Default)]
pub(crate) struct Foreground {
    actions: Vec<Action>,
    pending: Option<usize>,
    screens: VecDeque<Screen>,
    latest_frame: u64,
    lost_frames: u64,
}

impl Foreground {
    pub(crate) fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn replay(
        &self,
        request: &str,
        input: &Input,
        frame: Option<u64>,
    ) -> Result<Option<&Action>> {
        let action = self.actions.iter().find(|action| action.request == request);
        if let Some(action) = action {
            ensure!(
                &action.input == input && action.if_frame == frame,
                "request_id reused with a different terminal action or screen guard"
            );
        }
        Ok(action)
    }

    pub(crate) fn latest_action(&self) -> Option<&Action> {
        self.actions.last()
    }

    pub(crate) fn pending_guard(&self, id: Uuid) -> Option<u64> {
        self.pending
            .map(|index| &self.actions[index])
            .filter(|action| action.id == id)
            .and_then(|action| action.if_frame)
    }

    fn matches_frame(&self, frame: Option<u64>) -> bool {
        frame.is_none_or(|id| {
            self.screens
                .back()
                .is_some_and(|screen| screen.id == id && id == self.latest_frame)
        })
    }

    pub(crate) fn queue(
        &mut self,
        mut wire: Frame,
        session: &Session,
        task: Uuid,
        request: &str,
        input: Input,
        if_frame: Option<u64>,
    ) -> Result<&Action> {
        ensure!(
            self.pending.is_none(),
            "another terminal action is unresolved"
        );
        ensure!(
            self.actions.len() < ACTION_LIMIT,
            "terminal action history limit reached"
        );
        ensure!(
            self.matches_frame(if_frame),
            "terminal screen frame is stale or unavailable"
        );
        wire.data.extend_from_slice(task.as_bytes());
        let (kind, data) = input.payload();
        if input != Input::Detach {
            wire.data
                .extend_from_slice(&if_frame.unwrap_or(0).to_le_bytes());
            wire.data.extend_from_slice(data);
            wire.flags |= kind;
        }
        let record = Action {
            id: wire.request,
            request: request.into(),
            if_frame,
            stage: if input == Input::Detach {
                "queued"
            } else {
                "pending_approval"
            },
            input,
            approval: session.approval_mode(),
            approval_generation: session.approval_generation(),
        };
        session.tx.try_send(wire)?;
        self.pending = Some(self.actions.len());
        self.actions.push(record);
        Ok(self.actions.last().unwrap())
    }

    pub(crate) fn commit(&mut self, id: Uuid, session: &Session) -> bool {
        let Some(index) = self.pending else {
            return false;
        };
        let action = &self.actions[index];
        if action.id == id
            && action.stage == "pending_approval"
            && action.approval == session.approval_mode()
            && action.approval_generation == session.approval_generation()
            && self.matches_frame(action.if_frame)
        {
            self.actions[index].stage = "authorized";
            true
        } else {
            false
        }
    }

    pub(crate) fn acknowledge(&mut self, id: Uuid, ack: u32) -> Option<&Action> {
        let index = self.pending?;
        let action = &mut self.actions[index];
        if action.id != id {
            return None;
        }
        match ack {
            TERMINAL_INPUT_SENT
                if action.input != Input::Detach
                    && matches!(action.stage, "authorized" | "cancelling") =>
            {
                action.stage = "sent"
            }
            TERMINAL_INPUT_NOT_SENT if action.input != Input::Detach => action.stage = "not_sent",
            TERMINAL_DETACHED if action.input == Input::Detach => action.stage = "detached",
            TERMINAL_DETACH_NOT_SENT if action.input == Input::Detach => action.stage = "not_sent",
            _ => return None,
        }
        self.pending = None;
        Some(action)
    }

    pub(crate) fn revoke(&mut self, session_id: Uuid, session: &Session) {
        if let Some(index) = self.pending {
            let action = &mut self.actions[index];
            if action.stage != "cancelling" {
                let mut wire = Frame::new(REVOKE, session_id);
                wire.request = action.id;
                wire.epoch = session.epoch;
                let _ = session.tx.try_send(wire);
                action.stage = "cancelling";
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        if let Some(index) = self.pending.take() {
            self.actions[index].stage = "unknown";
        }
        self.screens.clear();
    }

    pub(crate) fn save_screen(&mut self, id: u64, data: &[u8]) -> Result<()> {
        if id <= self.latest_frame {
            return Ok(());
        }
        if data.is_empty() {
            self.latest_frame = id;
            return Ok(());
        }
        ensure!(
            (9..=9 + SCREEN_MAX).contains(&data.len()) && data[0] <= 15,
            "invalid terminal screen frame"
        );
        let number = |offset: usize| u16::from_le_bytes([data[offset], data[offset + 1]]);
        let screen = Screen {
            id,
            flags: data[0],
            columns: number(1),
            rows: number(3),
            x: number(5),
            y: number(7),
            text: data[9..].to_vec(),
        };
        ensure!(
            screen.columns != 0
                && screen.rows != 0
                && (screen.flags & 8 == 0 || (screen.x < screen.columns && screen.y < screen.rows)),
            "invalid terminal screen dimensions or cursor"
        );
        std::str::from_utf8(&screen.text)?;
        self.latest_frame = id;
        if self.screens.len() == SCREEN_LIMIT {
            self.screens.pop_front();
            self.lost_frames += 1;
        }
        self.screens.push_back(screen);
        Ok(())
    }

    pub(crate) fn read_screen(
        &self,
        task: Uuid,
        status: &str,
        after: Option<u64>,
        visible: bool,
    ) -> Value {
        let screen = if visible {
            match after {
                Some(id) => self.screens.iter().find(|screen| screen.id > id),
                None => self
                    .screens
                    .back()
                    .filter(|screen| screen.id == self.latest_frame),
            }
        } else {
            None
        };
        let frame = screen.map(|screen| {
            json!({"frame_id":screen.id,"text":safe_text(&screen.text),
            "columns":screen.columns,"rows":screen.rows,"truncated":screen.flags & 1 != 0,
            "paste_available":screen.flags & 2 != 0,"alternate_screen":screen.flags & 4 != 0,
            "cursor":(screen.flags & 8 != 0).then(|| json!({"column":screen.x,"row":screen.y}))})
        });
        let gap = after.and_then(|cursor| {
            screen.map(|screen| screen.id.saturating_sub(cursor).saturating_sub(1))
        });
        json!({"task_id":task,"status":status,"frame":frame,"latest_frame_id":if visible { self.latest_frame } else { 0 },
            "lost_frames":self.lost_frames,"skipped_since_cursor":gap,"untrusted_terminal_data":true})
    }
}
