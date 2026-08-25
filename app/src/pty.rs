//! Bridge between PTY sessions (overterm-core) and the webview.
//!
//! Output flows over a Tauri IPC channel: one reader thread per session pumps
//! PTY bytes into the channel; keystrokes come back in through commands.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Mutex;

use overterm_core::{PtySession, SpawnConfig};
use serde::Serialize;
use tauri::State;
use tauri::ipc::Channel;

/// Events streamed to the frontend for one session.
#[derive(Clone, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "event",
    content = "data"
)]
pub enum PtyEvent {
    /// Raw output bytes. Sent as bytes (not a lossy string) so multi-byte
    /// UTF-8 sequences split across chunks survive; xterm.js reassembles them.
    Output {
        bytes: Vec<u8>,
    },
    Exited {
        code: Option<u32>,
    },
}

#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, PtySession>>);

#[tauri::command]
pub fn spawn_session(
    cols: u16,
    rows: u16,
    on_event: Channel<PtyEvent>,
    sessions: State<'_, Sessions>,
) -> Result<String, String> {
    let config = SpawnConfig {
        // Login shell so GUI-launched instances still get the user's PATH
        // (needed to find `claude` and friends).
        args: vec!["-l".into()],
        cols,
        rows,
        ..Default::default()
    };
    let (session, mut output) = PtySession::spawn(config).map_err(|e| e.to_string())?;
    let id = session.id().to_string();
    sessions.0.lock().unwrap().insert(id.clone(), session);

    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match output.reader.read(&mut buf) {
                Ok(0) | Err(_) => break, // EOF or master closed
                Ok(n) => {
                    if on_event
                        .send(PtyEvent::Output {
                            bytes: buf[..n].to_vec(),
                        })
                        .is_err()
                    {
                        break; // webview went away
                    }
                }
            }
        }
        let code = output.child.wait().ok().map(|status| status.exit_code());
        let _ = on_event.send(PtyEvent::Exited { code });
    });

    Ok(id)
}

#[tauri::command]
pub fn write_pty(
    session_id: String,
    data: String,
    sessions: State<'_, Sessions>,
) -> Result<(), String> {
    let mut sessions = sessions.0.lock().unwrap();
    let session = sessions
        .get_mut(&session_id)
        .ok_or_else(|| format!("no session {session_id}"))?;
    session.write(data.as_bytes()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn resize_pty(
    session_id: String,
    cols: u16,
    rows: u16,
    sessions: State<'_, Sessions>,
) -> Result<(), String> {
    let sessions = sessions.0.lock().unwrap();
    let session = sessions
        .get(&session_id)
        .ok_or_else(|| format!("no session {session_id}"))?;
    session.resize(cols, rows).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn kill_session(session_id: String, sessions: State<'_, Sessions>) -> Result<(), String> {
    let mut sessions = sessions.0.lock().unwrap();
    // Removing the session drops the PTY master too, so the reader thread
    // unblocks and the exit event fires.
    match sessions.remove(&session_id) {
        Some(mut session) => session.kill().map_err(|e| e.to_string()),
        None => Ok(()),
    }
}
