//! Bridge between PTY sessions (overterm-core) and the webview.
//!
//! Output flows over a Tauri IPC channel: one reader thread per session pumps
//! PTY bytes into the channel; keystrokes come back in through commands. Each
//! session also runs a detector fed from both directions, plus a ticker
//! thread so time-based conclusions fire during silence. Detected state
//! changes stream to the frontend on the same channel.

use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use overterm_core::detect::heuristic::{HeuristicAdapter, HeuristicConfig};
use overterm_core::{AgentState, Detector, PtySession, SpawnConfig, StateChange};
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
    /// The detector concluded the agent's state changed.
    AgentStateChanged {
        state: AgentState,
        cause: String,
    },
    Exited {
        code: Option<u32>,
    },
}

pub struct SessionHandle {
    session: PtySession,
    detector: Arc<Mutex<Detector>>,
    events: Channel<PtyEvent>,
    alive: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, SessionHandle>>);

fn emit_changes(events: &Channel<PtyEvent>, changes: Vec<StateChange>) {
    for change in changes {
        let _ = events.send(PtyEvent::AgentStateChanged {
            state: change.to,
            cause: format!("{:?}", change.cause),
        });
    }
}

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

    let detector = Arc::new(Mutex::new(Detector::new(vec![Box::new(
        HeuristicAdapter::new(HeuristicConfig::default()),
    )])));
    let alive = Arc::new(AtomicBool::new(true));

    sessions.0.lock().unwrap().insert(
        id.clone(),
        SessionHandle {
            session,
            detector: detector.clone(),
            events: on_event.clone(),
            alive: alive.clone(),
        },
    );

    // Reader: PTY output to the webview and the detector.
    {
        let detector = detector.clone();
        let alive = alive.clone();
        let events = on_event.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match output.reader.read(&mut buf) {
                    Ok(0) | Err(_) => break, // EOF or master closed
                    Ok(n) => {
                        let changes = detector
                            .lock()
                            .unwrap()
                            .feed_output(&buf[..n], Instant::now());
                        if events
                            .send(PtyEvent::Output {
                                bytes: buf[..n].to_vec(),
                            })
                            .is_err()
                        {
                            break; // webview went away
                        }
                        emit_changes(&events, changes);
                    }
                }
            }
            alive.store(false, Ordering::Relaxed);
            let code = output.child.wait().ok().map(|status| status.exit_code());
            let _ = events.send(PtyEvent::Exited { code });
        });
    }

    // Ticker: lets quiescence conclusions fire while the PTY is silent.
    {
        let alive = alive.clone();
        std::thread::spawn(move || {
            while alive.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
                let changes = detector.lock().unwrap().tick(Instant::now());
                emit_changes(&on_event, changes);
            }
        });
    }

    Ok(id)
}

#[tauri::command]
pub fn write_pty(
    session_id: String,
    data: String,
    sessions: State<'_, Sessions>,
) -> Result<(), String> {
    let mut sessions = sessions.0.lock().unwrap();
    let handle = sessions
        .get_mut(&session_id)
        .ok_or_else(|| format!("no session {session_id}"))?;
    let changes = handle
        .detector
        .lock()
        .unwrap()
        .feed_input(data.as_bytes(), Instant::now());
    emit_changes(&handle.events, changes);
    handle
        .session
        .write(data.as_bytes())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn resize_pty(
    session_id: String,
    cols: u16,
    rows: u16,
    sessions: State<'_, Sessions>,
) -> Result<(), String> {
    let sessions = sessions.0.lock().unwrap();
    let handle = sessions
        .get(&session_id)
        .ok_or_else(|| format!("no session {session_id}"))?;
    handle.session.resize(cols, rows).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn kill_session(session_id: String, sessions: State<'_, Sessions>) -> Result<(), String> {
    let mut sessions = sessions.0.lock().unwrap();
    // Removing the session drops the PTY master too, so the reader thread
    // unblocks and the exit event fires.
    match sessions.remove(&session_id) {
        Some(mut handle) => {
            handle.alive.store(false, Ordering::Relaxed);
            handle.session.kill().map_err(|e| e.to_string())
        }
        None => Ok(()),
    }
}
