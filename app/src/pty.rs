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

use crate::settings::Agent;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};

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
    /// A different program owns this session's terminal now, so the tab
    /// should say something else.
    AgentChanged {
        agent: Agent,
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

/// How often the detector is given a chance to conclude something during
/// silence.
const TICK_MS: u64 = 100;

/// How many of those ticks pass between asking what is running in a
/// session. Once a second: it is a question about a person starting and
/// leaving programs, so asking ten times a second buys nothing.
const IDENTIFY_EVERY: u32 = 10;

/// What to call whatever currently owns this session's terminal.
///
/// A session is a shell, and the shell runs other programs in it, so this
/// changes as the user starts and leaves things. It is the shell's own
/// name at a prompt and the agent's while one is running, which is what
/// makes it a way to tell what a session is doing without the program
/// having to cooperate.
fn identify(app: &AppHandle, session_id: &str) -> Option<Agent> {
    let pid = {
        let sessions = app.state::<Sessions>();
        let sessions = sessions.0.lock().unwrap();
        sessions.get(session_id)?.session.foreground_pid()?
    };
    // The lock is dropped before this: reading the settings file and
    // asking the kernel about a process both take longer than any other
    // session should have to wait to report a state change.
    //
    // The path rather than the process name. A file name is not always a
    // name: Claude Code installs its executable as the bare version
    // number, so the process is called something like 2.1.250 and only
    // the path it sits in says what it is.
    let path = crate::platform::process_path(pid).or_else(|| crate::platform::process_name(pid))?;
    // Most of these tools are npm packages, so the program on the
    // terminal is node and only the script path it was handed says which
    // tool it is. Without this every one of them is called "node".
    //
    // The first argument matters too, for a different reason. A tool that
    // sets its own process title has no script path left to read: on
    // macOS that write clears the whole block and leaves the title in the
    // first entry. Pi does it, and that entry is the only place the word
    // "pi" survives.
    let args = crate::platform::process_args(pid).unwrap_or_default();
    let settings = crate::settings::load();

    // The same lookup answers both questions. Knowing which program owns
    // the terminal is what lets the fallback detector look for the right
    // thing on screen, and looking for the wrong thing is worse than
    // looking for nothing: between batches of output an agent's cursor
    // rests in its input box, and without a pattern that holds the state
    // the window decides the turn ended and comes back mid-answer.
    let profile = settings.profile_for(&path, &args);
    {
        let sessions = app.state::<Sessions>();
        let sessions = sessions.0.lock().unwrap();
        if let Some(handle) = sessions.get(session_id) {
            handle.detector.lock().unwrap().set_profile(&profile);
        }
    }

    // Writing our markers used to be taken as proof a session was Claude
    // Code, since its hooks were the only ones the app installed. Pi
    // writes the same markers now, so the marker names no tool and that
    // guess renamed every unidentified Pi session Claude.
    //
    // Nothing replaces it. The case it was insurance for, Claude Code's
    // executable being installed under its version number, is already
    // covered by the directory walk in `match_program`, which has a test
    // of its own. A tab reading `node` is a worse answer than `Claude`
    // and a much better one than the wrong agent's name and mark.
    let mut agent = settings.label_for(&path, &args);
    // Asked of the foreground process rather than remembered from the
    // spawn, because a shell changes directory and the value from spawn
    // stops being true the first time somebody types `cd`.
    agent.cwd = crate::platform::process_cwd(pid).map(|path| shorten_home(&path));
    Some(agent)
}

/// Write a path the way a shell prompt does, with the home directory as
/// `~`. Done here rather than in the interface because this side is the
/// one that knows where home is.
fn shorten_home(path: &str) -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return path.to_string();
    };
    let home = home.to_string_lossy();
    if home.is_empty() {
        return path.to_string();
    }
    if path == home {
        return "~".to_string();
    }
    match path.strip_prefix(&format!("{home}/")) {
        Some(rest) => format!("~/{rest}"),
        None => path.to_string(),
    }
}

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

    // Ticker: lets quiescence conclusions fire while the PTY is silent,
    // and keeps an eye on what is running in the session.
    {
        let ticker_id = id.clone();
        let alive = alive.clone();
        let choreo = choreo.inner().clone();
        std::thread::spawn(move || {
            let mut ticks = 0u32;
            let mut agent: Option<Agent> = None;
            while alive.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(TICK_MS));
                let changes = detector.lock().unwrap().tick(Instant::now());
                emit_changes(&app, &choreo, &ticker_id, &on_event, changes);

                ticks += 1;
                if ticks.is_multiple_of(IDENTIFY_EVERY)
                    && let Some(next) = identify(&app, &ticker_id)
                    && Some(&next) != agent.as_ref()
                {
                    agent = Some(next.clone());
                    let _ = on_event.send(PtyEvent::AgentChanged { agent: next });
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_under_home_is_written_with_a_tilde() {
        // SAFETY: single-threaded test, and the value is restored below.
        unsafe { std::env::set_var("HOME", "/Users/someone") };
        assert_eq!(
            shorten_home("/Users/someone/dev/overterm"),
            "~/dev/overterm"
        );
        assert_eq!(shorten_home("/Users/someone"), "~");
    }

    #[test]
    fn a_path_outside_home_is_left_alone() {
        unsafe { std::env::set_var("HOME", "/Users/someone") };
        // The prefix matches as text but is a different directory, so it
        // must not be rewritten.
        assert_eq!(
            shorten_home("/Users/someone-else/dev"),
            "/Users/someone-else/dev"
        );
        assert_eq!(shorten_home("/opt/homebrew"), "/opt/homebrew");
    }
}
