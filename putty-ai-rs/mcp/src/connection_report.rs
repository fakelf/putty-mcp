use anyhow::{Context, Result, ensure};
use putty_ai_common::{protocol::*, security::*};
use serde::Deserialize;
use serde_json::Value;
use std::{fmt::Write, path::PathBuf, time::Duration};
use tokio::{io::AsyncReadExt, net::windows::named_pipe::ClientOptions, time::timeout};
use uuid::Uuid;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MB_ICONINFORMATION, MB_SETFOREGROUND, MessageBoxW,
};

#[derive(Deserialize)]
struct BrokerStatus {
    adapter_count: usize,
    session_count: usize,
    window_registered: bool,
}

async fn probe(session: Uuid) -> Result<Option<BrokerStatus>> {
    let mut pipe = match ClientOptions::new().open(pipe_name()?) {
        Ok(pipe) => pipe,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    peer_pid(&pipe, false)?;
    Frame::new(CONNECTION_REPORT, session)
        .write(&mut pipe)
        .await?;
    let reply = Frame::read(&mut pipe).await?;
    ensure!(
        reply.kind == REPLY && reply.session == session,
        "invalid broker report"
    );
    Ok(Some(serde_json::from_slice(&reply.data)?))
}

async fn qwenpaw_status() -> String {
    let Some(root) = std::env::var_os("LOCALAPPDATA") else {
        return "Windows profile unavailable".into();
    };
    let path = PathBuf::from(root).join("PuTTY-AI-v4.4.0/qwenpaw-connection.json");
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return "No recorded QwenPaw connection. Enable diagnostics in its generated MCP configuration.".into();
        }
        Err(_) => return "Cannot read the QwenPaw connection record".into(),
    };
    let mut bytes = Vec::new();
    if file.take(16_385).read_to_end(&mut bytes).await.is_err() || bytes.len() > 16_384 {
        return "Cannot read the QwenPaw connection record".into();
    }
    let Ok(state) = serde_json::from_slice::<Value>(&bytes) else {
        return "Invalid QwenPaw connection record".into();
    };
    let live = state["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .and_then(|pid| process_start(pid).ok())
        .is_some_and(|start| {
            state["process_start"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                == Some(start)
        });
    let result = match state["failure"]["stage"].as_str() {
        Some("initialize") => "MCP handshake failed".into(),
        Some("broker") => {
            let detail = state["failure"]["message"].as_str().unwrap_or("");
            if detail.starts_with("Broker process launch failed") {
                "Broker process launch failed".into()
            } else if detail.starts_with("Broker IPC handshake failed") {
                "Broker found, IPC handshake failed".into()
            } else {
                "Broker unavailable to QwenPaw".into()
            }
        }
        Some("list_sessions") => "Session query failed".into(),
        _ if state["initialized"].as_bool() != Some(true) => {
            "Adapter started, handshake incomplete".into()
        }
        _ if state["tools_listed"].as_bool() != Some(true) => {
            "Handshake complete, tools not queried".into()
        }
        _ => match state["session_count"].as_u64() {
            None => "Tools discovered, sessions not queried".into(),
            Some(n) => format!("Handshake and tools OK, last session query returned {n}"),
        },
    };
    if live {
        result
    } else {
        format!("Previous adapter exited. Last result: {result}")
    }
}

pub async fn show(session: Option<Uuid>, text_only: bool) -> Result<()> {
    let mut report = String::from("PuTTY MCP v4.4.0 connection check\r\n\r\n");
    let status = timeout(Duration::from_secs(2), probe(session.unwrap_or_default())).await;
    match &status {
        Ok(Ok(Some(status))) => {
            write!(
                report,
                "Broker: online\r\nMCP hosts registered: {}\r\nAuthorized PuTTY sessions: {}\r\n",
                status.adapter_count, status.session_count
            )?;
            if session.is_some() {
                let registration = if status.window_registered {
                    "registered"
                } else {
                    "not registered"
                };
                write!(report, "Current PuTTY window: {registration}\r\n")?;
            }
        }
        Ok(Ok(None)) => report.push_str("Broker: not reachable for this Windows user\r\n"),
        Ok(Err(_)) => report.push_str("Broker: local pipe access or handshake failed\r\n"),
        Err(_) => report.push_str("Broker: connection check timed out\r\n"),
    }
    if session.is_none() {
        report.push_str("Current PuTTY window: AI observation off (or no window supplied)\r\n");
    } else if !matches!(status, Ok(Ok(Some(_)))) {
        report.push_str("Current PuTTY window: observation enabled, registration unverified\r\n");
    }
    write!(report, "\r\nQwenPaw: {}\r\n", qwenpaw_status().await)?;
    if text_only {
        print!("{report}");
    } else {
        report.push_str("\r\nPress Ctrl+C in this dialog to copy the report.");
        let result = unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                wide(&report).as_ptr(),
                wide("PuTTY MCP connection check").as_ptr(),
                MB_ICONINFORMATION | MB_SETFOREGROUND,
            )
        };
        ensure!(result != 0, "cannot show connection report");
    }
    Ok(())
}

pub async fn from_args(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<()> {
    let session = args
        .next()
        .context("connection report requires a session ID or 'off'")?;
    let session = if session == "off" {
        None
    } else {
        Some(Uuid::parse_str(
            session.to_str().context("invalid session ID")?,
        )?)
    };
    let mode = args.next();
    ensure!(
        mode.as_ref().is_none_or(|arg| arg == "--text") && args.next().is_none(),
        "invalid connection report arguments"
    );
    show(session, mode.is_some()).await
}
