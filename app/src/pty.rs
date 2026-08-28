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
use overterm_core::detect::hook::HookAdapter;
use overterm_core::detect::replay::{Dir, Event, append_event, resize_payload};
use overterm_core::{AgentState, Detector, PtySession, SpawnConfig, StateChange};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, State};

use crate::choreograph::Choreographer;

/// Dev-only session recorder, enabled with OVERTERM_CAPTURE=<path-prefix>.
/// Every PTY byte, keystroke and resize is appended as a fixture event, so
/// a misbehaving live session can be replayed through the detector as is.
pub struct Capture {
    file: std::io::BufWriter<std::fs::File>,
    start: Instant,
}

impl Capture {
    fn open(session_id: &str) -> Option<Arc<Mutex<Capture>>> {
        let prefix = std::env::var("OVERTERM_CAPTURE").ok()?;
        let path = format!("{prefix}-{session_id}.ndjson");
        match std::fs::File::create(&path) {
            Ok(file) => {
                eprintln!("[capture] recording session to {path}");
                Some(Arc::new(Mutex::new(Capture {
                    file: std::io::BufWriter::new(file),
                    start: Instant::now(),
                })))
            }
            Err(e) => {
                eprintln!("[capture] cannot create {path}: {e}");
                None
            }
        }
    }

    fn log(&mut self, dir: Dir, bytes: &[u8]) {
        let ev = Event {
            t_ms: self.start.elapsed().as_millis() as u64,
            dir,
            bytes: bytes.to_vec(),
        };
        let _ = append_event(&mut self.file, &ev);
        let _ = std::io::Write::flush(&mut self.file);
    }
}

fn capture_log(capture: &Option<Arc<Mutex<Capture>>>, dir: Dir, bytes: &[u8]) {
    if let Some(capture) = capture {
        capture.lock().unwrap().log(dir, bytes);
    }
}

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
    capture: Option<Arc<Mutex<Capture>>>,
}

#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, SessionHandle>>);

/// Report a batch of transitions: the overlay reacts to them, and the
/// frontend shows what the detector concluded.
fn emit_changes(
    app: &AppHandle,
    choreo: &Choreographer,
    session_id: &str,
    events: &Channel<PtyEvent>,
    changes: Vec<StateChange>,
) {
    for change in changes {
        let _ = events.send(PtyEvent::AgentStateChanged {
            state: change.to,
            cause: format!("{:?}", change.cause),
        });
        choreo.on_state_change(app, session_id, &change);
    }
}

#[tauri::command]
pub fn spawn_session(
    cols: u16,
    rows: u16,
    on_event: Channel<PtyEvent>,
    app: AppHandle,
    sessions: State<'_, Sessions>,
    choreo: State<'_, Choreographer>,
) -> Result<String, String> {
    let config = SpawnConfig {
        // Login shell so GUI-launched instances still get the user's PATH
        // (needed to find `claude` and friends).
        args: vec!["-l".into()],
        // A dev build launched from inside a Claude Code session inherits
        // these markers, and claude run in our shell then believes it is a
        // nested child session. A normal app launch never has them.
        env_remove: [
            "CLAUDECODE",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_SSE_PORT",
        ]
        .map(String::from)
        .to_vec(),
        cols,
        rows,
        ..Default::default()
    };
    let (session, mut output) = PtySession::spawn(config).map_err(|e| e.to_string())?;
    let id = session.id().to_string();

    let heuristic = HeuristicAdapter::new(HeuristicConfig {
        cols,
        rows,
        ..Default::default()
    });
    // Hooks first: a chunk can carry both an exact event and enough
    // output for the fallback detector to guess at, and the exact one
    // has to land before the guess is weighed.
    let detector = Arc::new(Mutex::new(Detector::new(vec![
        Box::new(HookAdapter::new()),
        Box::new(heuristic),
    ])));
    let alive = Arc::new(AtomicBool::new(true));
    let capture = Capture::open(&id);

    // Let the choreography ask this session whether it is really working
    // before it hides the terminal.
    {
        let detector = detector.clone();
        choreo.add_session(
            &app,
            &id,
            Arc::new(move || detector.lock().unwrap().is_working(Instant::now())),
        );
    }

    sessions.0.lock().unwrap().insert(
        id.clone(),
        SessionHandle {
            session,
            detector: detector.clone(),
            events: on_event.clone(),
            alive: alive.clone(),
            capture: capture.clone(),
        },
    );

    // Reader: PTY output to the webview and the detector.
    {
        let session_id = id.clone();
        let detector = detector.clone();
        let alive = alive.clone();
        let events = on_event.clone();
        let capture = capture.clone();
        let app = app.clone();
        let choreo = choreo.inner().clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match output.reader.read(&mut buf) {
                    Ok(0) | Err(_) => break, // EOF or master closed
                    Ok(n) => {
                        capture_log(&capture, Dir::Output, &buf[..n]);
                        let (changes, submitted) = {
                            let mut detector = detector.lock().unwrap();
                            let changes = detector.feed_output(&buf[..n], Instant::now());
                            (changes, detector.take_submit())
                        };
                        if events
                            .send(PtyEvent::Output {
                                bytes: buf[..n].to_vec(),
                            })
                            .is_err()
                        {
                            break; // webview went away
                        }
                        emit_changes(&app, &choreo, &session_id, &events, changes);
                        // After the transitions, for the same reason the
                        // keystroke path does it last.
                        if submitted {
                            choreo.on_submit(&app, &session_id);
                        }
                    }
                }
            }
            alive.store(false, Ordering::Relaxed);
            // The window stops holding itself open on this session's
            // behalf. Without this a session that exited mid-turn would
            // keep the terminal from ever collapsing again.
            choreo.remove_session(&session_id);
            let code = output.child.wait().ok().map(|status| status.exit_code());
            let _ = events.send(PtyEvent::Exited { code });
        });
    }

    // Ticker: lets quiescence conclusions fire while the PTY is silent.
    {
        let ticker_id = id.clone();
        let alive = alive.clone();
        let choreo = choreo.inner().clone();
        std::thread::spawn(move || {
            while alive.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
                let changes = detector.lock().unwrap().tick(Instant::now());
                emit_changes(&app, &choreo, &ticker_id, &on_event, changes);
            }
        });
    }

    Ok(id)
}

#[tauri::command]
pub fn write_pty(
    session_id: String,
    data: String,
    app: AppHandle,
    sessions: State<'_, Sessions>,
    choreo: State<'_, Choreographer>,
) -> Result<(), String> {
    let mut sessions = sessions.0.lock().unwrap();
    let handle = sessions
        .get_mut(&session_id)
        .ok_or_else(|| format!("no session {session_id}"))?;
    capture_log(&handle.capture, Dir::Input, data.as_bytes());
    let (changes, precise) = {
        let mut detector = handle.detector.lock().unwrap();
        let now = Instant::now();
        let changes = detector.feed_input(data.as_bytes(), now);
        (changes, detector.precise_source_active(now))
    };
    emit_changes(&app, &choreo, &session_id, &handle.events, changes);
    let written = handle
        .session
        .write(data.as_bytes())
        .map_err(|e| e.to_string());
    // Enter only means "here is a job" when nothing better is reporting.
    // Inside a program that reports its own submits, enter is just as
    // likely to be answering a dialog or picking a menu item, and hiding
    // the terminal for one of those takes away what the user is reading.
    // Scheduled after the transitions above so it is the collapse that
    // survives, not one they cancelled.
    if !precise && (data.contains('\r') || data.contains('\n')) {
        choreo.on_submit(&app, &session_id);
    }
    written
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
    capture_log(&handle.capture, Dir::Resize, &resize_payload(cols, rows));
    handle.detector.lock().unwrap().resize(cols, rows);
    handle.session.resize(cols, rows).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn kill_session(
    session_id: String,
    sessions: State<'_, Sessions>,
    choreo: State<'_, Choreographer>,
) -> Result<(), String> {
    let mut sessions = sessions.0.lock().unwrap();
    // Removing the session drops the PTY master too, so the reader thread
    // unblocks and the exit event fires.
    match sessions.remove(&session_id) {
        Some(mut handle) => {
            handle.alive.store(false, Ordering::Relaxed);
            choreo.remove_session(&session_id);
            handle.session.kill().map_err(|e| e.to_string())
        }
        None => Ok(()),
    }
}
