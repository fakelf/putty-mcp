use crate::model::{ApprovalMode, Session};
use anyhow::{Result, ensure};
use putty_ai_common::{protocol::*, terminal::safe_text};
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    collections::VecDeque,
    time::{Duration, Instant},
};
use uuid::Uuid;

const FRAME_CAPACITY: usize = 30;
const FRAME_BYTES: usize = 256 * 1024;
const SCREEN_PREFIX: usize = 3;
const RUN_LIMIT: Duration = Duration::from_secs(3600);
const MAX_SPECIAL_REQUESTS: usize = 256;

// A special key's position plus one is its wire code.
const SPECIAL_KEYS: &[&str] = &[
    "escape",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
    "ctrl_q",
    "ctrl_s",
    "ctrl_d",
    "ctrl_z",
    "backspace",
    "delete",
    "up",
    "down",
    "left",
    "right",
    "home",
    "end",
    "page_up",
    "page_down",
    "insert",
    "tab",
    "shift_tab",
    "enter",
    "undo",
    "redo",
    "insert_mode",
    "discard_and_quit",
];

#[derive(PartialEq, Eq)]
pub enum Profile {
    Top,
    Vi,
    ViFile(String),
}

impl Profile {
    pub fn parse(name: &str, path: Option<&str>) -> Result<Self> {
        match (name, path) {
            ("top", None) => Ok(Self::Top),
            ("top", Some(_)) => anyhow::bail!("path is only available for vi"),
            ("vi", None) => Ok(Self::Vi),
            ("vi", Some(path)) => {
                ensure!(
                    path.len() <= VI_PATH_MAX
                        && path.strip_prefix('/').is_some_and(|rest| {
                            rest.split('/').all(|part| !matches!(part, "" | "." | ".."))
                        })
                        && !path.chars().any(|ch| {
                            ch.is_control()
                                || matches!(ch, '\u{061c}' | '\u{200b}'..='\u{200f}' |
                                    '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
                        }),
                    "path must be an absolute POSIX file path of at most 512 UTF-8 bytes, without empty, dot or dot-dot segments or invisible controls"
                );
                Ok(Self::ViFile(path.into()))
            }
            _ => anyhow::bail!("only the top and vi profiles are supported"),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Vi | Self::ViFile(_) => "vi",
        }
    }

    fn path(&self) -> Option<&str> {
        match self {
            Self::ViFile(path) => Some(path),
            Self::Top | Self::Vi => None,
        }
    }

    fn command(&self) -> Cow<'static, str> {
        match self {
            Self::Top => "exec top -d 2".into(),
            Self::Vi => {
                "exec vi -Nu NONE -i NONE -n -R -Z -c 'set encoding=utf-8 backspace=indent,eol,start'"
                    .into()
            }
            Self::ViFile(path) => {
                let quoted = path.replace('\'', "'\\''");
                format!(
                    "p='{quoted}'; test -f \"$p\" && test ! -L \"$p\" && exec vi -Nu NONE -i NONE -n -Z -m \
                    --cmd 'set encoding=utf-8 backspace=indent,eol,start nomodeline noautowrite noautowriteall' \
                    -c 'let g:putty_mcp_buf = bufnr(\"%\") | let g:putty_mcp_path = expand(\"%:p\") | \
                    let g:putty_mcp_resolved = resolve(g:putty_mcp_path) | \
                    let g:putty_mcp_mtime = getftime(g:putty_mcp_path) | \
                    let g:putty_mcp_size = getfsize(g:putty_mcp_path)' -- \"$p\""
                ).into()
            }
        }
    }

    fn start_kind(&self) -> u16 {
        match self {
            Self::Top => TOP_START,
            Self::Vi => VI_START,
            Self::ViFile(_) => VI_FILE_START,
        }
    }

    fn quit_kind(&self) -> u16 {
        match self {
            Self::Top => TOP_QUIT,
            Self::Vi | Self::ViFile(_) => VI_QUIT,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TopKey {
    Quit,
    Interrupt,
    Special(usize),
}

struct SpecialRequest {
    id: String,
    input: InteractiveInput,
    state: KeyState,
}

#[derive(Default)]
struct KeyState {
    status: Option<&'static str>,
    approval: Option<ApprovalMode>,
}

#[derive(PartialEq, Eq)]
enum InteractiveInput {
    Named(usize),
    Text(String),
    Save,
}

impl InteractiveInput {
    fn name(&self) -> &'static str {
        match self {
            Self::Named(key) => SPECIAL_KEYS[*key],
            Self::Text(_) => "insert_text",
            Self::Save => "save",
        }
    }
}

struct Screen {
    number: u64,
    text: Vec<u8>,
    truncated: bool,
    cursor: Option<(u8, u8)>,
    paste_available: bool,
}

pub struct TopTask {
    pub client: Uuid,
    pub session: Uuid,
    pub request: String,
    pub profile: Profile,
    pub source: String,
    pub status: &'static str,
    pub updated: Instant,
    epoch: u64,
    approval: ApprovalMode,
    acked_start: bool,
    channel_closed: bool,
    ended: bool,
    owner_gone: bool,
    taken_over: bool,
    exit_code: Option<i32>,
    exit_signal_received: bool,
    error: Option<&'static str>,
    deadline: Option<Instant>,
    pending: Option<TopKey>,
    quit: KeyState,
    interrupt: KeyState,
    special_requests: Vec<SpecialRequest>,
    frames: VecDeque<Screen>,
    bytes: usize,
    lost_frames: u64,
    latest_frame: u64,
}

impl TopTask {
    pub fn new(
        client: Uuid,
        session: Uuid,
        request: &str,
        source: &str,
        epoch: u64,
        profile: Profile,
        approval: ApprovalMode,
    ) -> Self {
        Self {
            client,
            session,
            request: request.into(),
            profile,
            source: source.into(),
            epoch,
            approval,
            status: "queued",
            updated: Instant::now(),
            acked_start: false,
            channel_closed: false,
            ended: false,
            owner_gone: false,
            taken_over: false,
            exit_code: None,
            exit_signal_received: false,
            error: None,
            deadline: None,
            pending: None,
            quit: KeyState::default(),
            interrupt: KeyState::default(),
            special_requests: Vec::new(),
            frames: VecDeque::new(),
            bytes: 0,
            lost_frames: 0,
            latest_frame: 0,
        }
    }

    pub fn start_frame(&self, id: Uuid, session: &Session) -> Frame {
        let mut frame = self.frame(self.profile.start_kind(), id);
        session.stamp_approval(&mut frame);
        frame
    }

    fn frame(&self, kind: u16, id: Uuid) -> Frame {
        let mut f = Frame::new(kind, self.session);
        f.request = id;
        f.epoch = self.epoch;
        if matches!(
            kind,
            TOP_START
                | VI_START
                | VI_FILE_START
                | TOP_QUIT
                | VI_QUIT
                | TOP_INTERRUPT
                | SPECIAL_KEY
                | VI_INSERT_TEXT
                | VI_SAVE
        ) {
            f.data.extend_from_slice(self.client.as_bytes());
            f.data.push(self.source.len() as u8);
            f.data.extend_from_slice(self.source.as_bytes());
            if let Some(path) = self.profile.path() {
                f.data.extend_from_slice(VI_FILE_PREFIX);
                f.data.extend_from_slice(&(path.len() as u16).to_le_bytes());
                f.data.extend_from_slice(path.as_bytes());
            } else {
                f.data.extend_from_slice(self.profile.command().as_bytes());
            }
        }
        f
    }

    pub fn result(&self, id: Uuid) -> Value {
        self.result_with_key(id, self.special_requests.last(), self.approval)
    }

    fn result_with_key(
        &self,
        id: Uuid,
        special: Option<&SpecialRequest>,
        approval: ApprovalMode,
    ) -> Value {
        json!({"task_id":id,"session_id":self.session,"client_id":self.client,
            "source":self.source,"program":self.profile.name(),"path":self.profile.path(),"command":self.profile.command(),"status":self.status,
            "approval_mode":approval.name(),"requires_local_command_approval":approval.requires_local(),
            "exit_code":self.exit_code,"exit_signal_received":self.exit_signal_received,
            "channel_closed":self.channel_closed,"remote_process_stopped":false,
            "taken_over":self.taken_over,"quit_status":self.quit.status,"interrupt_status":self.interrupt.status,
            "quit_approval_mode":self.quit.approval.map(ApprovalMode::name),
            "interrupt_approval_mode":self.interrupt.approval.map(ApprovalMode::name),
            "key_request_id":special.map(|request| &request.id),
            "key":special.map(|request| request.input.name()),
            "key_status":special.and_then(|request| request.state.status),
            "key_approval_mode":special.and_then(|request| request.state.approval).map(ApprovalMode::name),
            "error":self.error})
    }

    fn key_state(&mut self, key: TopKey) -> &mut KeyState {
        match key {
            TopKey::Quit => &mut self.quit,
            TopKey::Interrupt => &mut self.interrupt,
            TopKey::Special(index) => &mut self.special_requests[index].state,
        }
    }

    pub fn request_key(&mut self, id: Uuid, key: TopKey, session: &Session) -> Result<Value> {
        if self.key_state(key).status.is_none() {
            self.check_request(id, session)?;
            let mut frame = match key {
                TopKey::Quit => self.frame(self.profile.quit_kind(), id),
                TopKey::Interrupt => self.frame(TOP_INTERRUPT, id),
                TopKey::Special(index) => match &self.special_requests[index].input {
                    InteractiveInput::Named(key) => {
                        let mut frame = self.frame(SPECIAL_KEY, id);
                        frame.flags = *key as u32 + 1;
                        frame
                    }
                    InteractiveInput::Text(text) => {
                        let mut frame = self.frame(VI_INSERT_TEXT, id);
                        frame.data.extend_from_slice(text.as_bytes());
                        frame
                    }
                    InteractiveInput::Save => self.frame(VI_SAVE, id),
                },
            };
            let approval = session.approval_mode();
            session.stamp_approval(&mut frame);
            session.tx.try_send(frame)?;
            self.pending = Some(key);
            *self.key_state(key) = KeyState {
                status: Some("pending_approval"),
                approval: Some(approval),
            };
        }
        let approval = self.key_state(key).approval.unwrap_or(self.approval);
        Ok(self.result_with_key(id, self.special_requests.last(), approval))
    }

    pub fn request_special_key(
        &mut self,
        id: Uuid,
        action: &str,
        request_id: &str,
        session: &Session,
    ) -> Result<Value> {
        let key = SPECIAL_KEYS
            .iter()
            .position(|candidate| *candidate == action)
            .ok_or_else(|| anyhow::anyhow!("unsupported interactive key"))?;
        ensure!(
            !matches!(action, "undo" | "redo" | "insert_mode" | "discard_and_quit")
                || self.profile != Profile::Top,
            "this action is only available for vi"
        );
        self.request_input(id, InteractiveInput::Named(key), request_id, session)
    }

    pub fn request_text(
        &mut self,
        id: Uuid,
        text: &str,
        request_id: &str,
        session: &Session,
    ) -> Result<Value> {
        ensure!(
            self.profile != Profile::Top,
            "insert_text is only available for vi"
        );
        ensure!(
            (1..=VI_TEXT_MAX).contains(&text.len())
                && !text.chars().any(|ch| {
                    (ch.is_control() && !matches!(ch, '\n' | '\t'))
                        || matches!(ch, '\u{061c}' | '\u{200b}'..='\u{200f}' |
                        '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
                }),
            "text must be 1..1024 UTF-8 bytes with only LF and Tab as controls and no invisible directional characters"
        );
        self.request_input(id, InteractiveInput::Text(text.into()), request_id, session)
    }

    pub fn request_save(&mut self, id: Uuid, request_id: &str, session: &Session) -> Result<Value> {
        ensure!(
            self.profile.path().is_some(),
            "save requires vi opened with a file path"
        );
        self.request_input(id, InteractiveInput::Save, request_id, session)
    }

    fn request_input(
        &mut self,
        id: Uuid,
        input: InteractiveInput,
        request_id: &str,
        session: &Session,
    ) -> Result<Value> {
        if let Some(request) = self.special_requests.iter().find(|r| r.id == request_id) {
            ensure!(
                request.input == input,
                "request_id reused with a different key or text"
            );
            return Ok(self.result_with_key(
                id,
                Some(request),
                request.state.approval.unwrap_or(self.approval),
            ));
        }
        ensure!(
            self.special_requests.len() < MAX_SPECIAL_REQUESTS,
            "interactive key request limit reached"
        );
        let index = self.special_requests.len();
        self.special_requests.push(SpecialRequest {
            id: request_id.into(),
            input,
            state: KeyState::default(),
        });
        let result = self.request_key(id, TopKey::Special(index), session);
        if result.is_err() {
            self.special_requests.pop();
        }
        result
    }

    fn check_request(&self, id: Uuid, session: &Session) -> Result<()> {
        ensure!(
            self.can_send(id, session),
            "interactive task is not available for AI input"
        );
        ensure!(
            self.pending.is_none(),
            "another interactive action is awaiting a decision"
        );
        Ok(())
    }

    fn can_send(&self, id: Uuid, session: &Session) -> bool {
        self.acked_start
            && self.status == "running"
            && !self.ended
            && !self.owner_gone
            && !self.taken_over
            && self.epoch == session.epoch
            && session.active == Some(id)
            && session.flags & (READ | CONNECTED | TOP_CAPABLE) == READ | CONNECTED | TOP_CAPABLE
    }

    pub fn commit(&mut self, id: Uuid, session: &Session) -> Frame {
        if self.can_send(id, session)
            && let Some(key) = self.pending
            && self.key_state(key).status == Some("pending_approval")
            && self.key_state(key).approval == Some(session.approval_mode())
        {
            self.key_state(key).status = Some("authorized");
            return self.frame(TOP_SEND_KEY, id);
        }
        self.frame(REVOKE, id)
    }

    pub fn cancel_start(&mut self, id: Uuid, session: &Session) -> Result<()> {
        if self.status == "queued" && !self.acked_start && session.active == Some(id) {
            session.tx.try_send(self.frame(REVOKE, id))?;
            self.status = "unknown";
            self.error =
                Some("interactive startup revocation requested; awaiting PuTTY confirmation");
        } else if !self.ended {
            anyhow::bail!("interactive task already dispatched; use quit or interrupt");
        }
        Ok(())
    }

    pub fn release_slot(&self) -> bool {
        self.acked_start && self.ended && self.pending.is_none()
    }

    pub(crate) fn decision_approval(&mut self, code: u32) -> ApprovalMode {
        if matches!(code, TOP_KEY_SENT | TOP_KEY_NOT_SENT)
            && let Some(key) = self.pending
            && let Some(approval) = self.key_state(key).approval
        {
            approval
        } else {
            self.approval
        }
    }

    pub fn ack(&mut self, code: u32) -> Result<Option<&'static str>> {
        match code {
            TOP_STARTED => {
                self.acked_start = true;
                self.deadline = Some(Instant::now() + RUN_LIMIT);
                if self.status == "queued" {
                    self.status = "starting";
                }
                Ok(Some("started"))
            }
            TOP_NOT_STARTED => {
                self.acked_start = true;
                self.ended = true;
                self.status = "cancelled";
                self.error = Some("PuTTY did not open the interactive task channel");
                self.updated = Instant::now();
                Ok(Some("not_started"))
            }
            TOP_KEY_SENT | TOP_KEY_NOT_SENT => {
                let status = if code == TOP_KEY_SENT {
                    "sent"
                } else {
                    "not_sent"
                };
                if let Some(key) = self.pending.take() {
                    self.key_state(key).status = Some(status);
                }
                Ok(Some(status))
            }
            TOP_KEY_APPROVED => Ok(None),
            _ => anyhow::bail!("invalid interactive acknowledgement"),
        }
    }

    pub fn event(&mut self, data: &[u8]) -> Result<()> {
        ensure!(data.len() == 12, "invalid interactive channel state");
        let state = u32::from_le_bytes(data[..4].try_into()?);
        let detail = i32::from_le_bytes(data[4..8].try_into()?);
        if !self.taken_over && u32::from_le_bytes(data[8..].try_into()?) != 0 {
            self.taken_over = true;
            self.clear_frames();
        }
        if !self.ended {
            self.updated = Instant::now();
            match state {
                1 | 2 => {
                    if !self.owner_gone && self.error.is_none() {
                        self.status = if state == 1 { "starting" } else { "running" };
                    }
                }
                3 => {
                    self.channel_closed = true;
                    self.ended = true;
                    self.exit_code = Some(detail);
                    self.status = if detail == 0 { "completed" } else { "failed" };
                    self.error = None;
                }
                4 => {
                    self.channel_closed = true;
                    self.ended = true;
                    self.exit_signal_received = true;
                    self.status = "failed";
                    self.error = Some("SSH server reported an exit signal");
                }
                5 | 7 => {
                    self.channel_closed = state == 5;
                    self.ended = true;
                    self.status = "unknown";
                    self.error = Some(if state == 5 {
                        "SSH channel closed without an exit status"
                    } else {
                        "interactive channel disposed locally or disconnected; remote outcome unknown"
                    });
                }
                6 => {
                    self.ended = true;
                    self.status = "failed";
                    self.error = Some(match detail {
                        1 => "SSH server refused a separate session channel",
                        2 => "SSH server refused the task PTY",
                        _ => "SSH server refused to start the interactive program",
                    });
                }
                8 | 9 => {
                    if state == 8 {
                        self.exit_code = Some(detail);
                    } else {
                        self.exit_signal_received = true;
                    }
                    if !self.owner_gone && self.error.is_none() {
                        self.status = "closing";
                    }
                }
                _ => anyhow::bail!("unknown interactive channel state"),
            }
        }
        Ok(())
    }

    pub fn revoke_key(&mut self, id: Uuid, session: &Session) {
        if let Some(key) = self.pending
            && matches!(
                self.key_state(key).status,
                Some("pending_approval" | "authorized")
            )
            && session.tx.try_send(self.frame(REVOKE, id)).is_ok()
        {
            self.key_state(key).status = Some("cancelling");
        }
    }

    pub fn expire(&mut self, id: Uuid, session: &Session, now: Instant) {
        if !self.ended && self.deadline.is_some_and(|deadline| now >= deadline) {
            if self.status != "timeout" && !self.owner_gone {
                self.status = "timeout";
                self.error =
                    Some("interactive observation expired; remote process may still be running");
                self.updated = now;
            }
            self.revoke_key(id, session);
        }
    }

    pub fn owner_gone(&mut self, id: Uuid, session: Option<&Session>) {
        self.owner_gone = true;
        self.clear_frames();
        if let Some(session) = session {
            if !self.acked_start {
                let _ = session.tx.try_send(self.frame(REVOKE, id));
            }
            self.revoke_key(id, session);
        }
        if !self.ended {
            self.status = "unknown";
            self.error = Some("MCP client disconnected; remote outcome unknown");
            self.updated = Instant::now();
        }
    }

    pub fn terminal_gone(&mut self) {
        if !self.ended {
            self.ended = true;
            self.status = "unknown";
            self.error = Some("PuTTY disconnected; remote outcome unknown");
        }
        if let Some(key) = self.pending.take() {
            self.key_state(key).status = Some("unknown");
        }
        self.clear_frames();
        self.updated = Instant::now();
    }

    fn clear_frames(&mut self) {
        self.frames.clear();
        self.bytes = 0;
    }

    pub fn save_screen(&mut self, number: u64, data: &[u8]) -> Result<()> {
        ensure!(
            (SCREEN_PREFIX..=SCREEN_PREFIX + 8192).contains(&data.len())
                && data[0] <= 7
                && data[1] < 240
                && data[2] < 100,
            "invalid interactive screen frame"
        );
        if self.owner_gone
            || self.taken_over
            || self.status == "timeout"
            || number <= self.latest_frame
        {
            return Ok(());
        }
        self.lost_frames += number - self.latest_frame - 1;
        self.latest_frame = number;
        let text = &data[SCREEN_PREFIX..];
        while self.frames.len() >= FRAME_CAPACITY || self.bytes + text.len() > FRAME_BYTES {
            let old = self
                .frames
                .pop_front()
                .expect("screen exceeds nonempty buffer");
            self.bytes -= old.text.len();
            self.lost_frames += 1;
        }
        self.bytes += text.len();
        self.frames.push_back(Screen {
            number,
            text: text.to_vec(),
            truncated: data[0] & 1 != 0,
            cursor: (data[0] & 2 != 0).then_some((data[1], data[2])),
            paste_available: data[0] & 4 != 0,
        });
        Ok(())
    }

    pub fn read_screen(&self, id: Uuid, after: Option<u64>) -> Value {
        let next = match after {
            Some(after) => self.frames.iter().find(|f| f.number > after),
            None => self.frames.back(),
        };
        let frame = next.map(|f| {
            let text = String::from_utf8_lossy(&f.text).replace("\r\n", "\n");
            json!({"frame_id":f.number,"text":safe_text(text.as_bytes()),"truncated":f.truncated,
                "cursor":f.cursor.map(|(column, row)| json!({"column":column,"row":row})),
                "paste_available":f.paste_available})
        });
        let gap =
            after.and_then(|after| next.map(|f| f.number.saturating_sub(after).saturating_sub(1)));
        json!({"task_id":id,"frame":frame,"latest_frame_id":self.latest_frame,
            "lost_frames":self.lost_frames,"skipped_since_cursor":gap,"status":self.status,
            "taken_over":self.taken_over,"untrusted_terminal_data":true})
    }
}
