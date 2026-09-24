//! Codex desktop threads as oTerm tabs.
//!
//! The Codex desktop app owns every conversation it has open and publishes
//! each one on a local bus that its other surfaces follow. oTerm joins that
//! bus as one more follower: it mirrors the thread a tab shows, sends what
//! the user types as a new turn or a steer, and answers approvals. The
//! desktop app has to stay running, since it is the one doing the work.

#[cfg(unix)]
pub mod bus;
pub mod threads;

use overterm_core::codex::mirror::{Mirror, MirrorError};
use overterm_core::codex::view::{ApprovalKind, ThreadView, Tracker, thread_view};
use overterm_core::codex::wire::Method;
use overterm_core::{AgentState, StateChange};
use serde_json::{Value, json};
use tauri::ipc::Channel;
use tauri::{AppHandle, State};

use crate::choreograph::Choreographer;
use threads::ThreadSummary;

/// What a Codex tab is told, on the channel it opened the thread with.
#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Clone, serde::Serialize)]
#[serde(tag = "event", content = "data", rename_all = "camelCase")]
pub enum CodexEvent {
    View(Box<ThreadView>),
    AgentStateChanged {
        state: AgentState,
        cause: String,
    },
    /// Something is wrong with the connection, or `None` once it is fine
    /// again. The tab offers to reconnect while there is a problem.
    Problem {
        message: Option<String>,
    },
}

// Driven only by the Unix bus below; kept building everywhere so the
// tests run on every platform.
#[cfg_attr(not(unix), allow(dead_code))]
/// One followed thread's mirror, and what was last reported from it.
#[derive(Default)]
struct Follower {
    mirror: Mirror,
    tracker: Tracker,
    view: Option<ThreadView>,
}

#[cfg_attr(not(unix), allow(dead_code))]
/// What one stream change means for the tab and the window.
#[derive(Debug, Default)]
struct Update {
    /// Only set when the view differs from the last one sent. Most patches
    /// are token counts and timestamps that change nothing on screen.
    view: Option<ThreadView>,
    state_change: Option<StateChange>,
}

#[cfg_attr(not(unix), allow(dead_code))]
impl Follower {
    fn on_change(&mut self, change: &Value) -> Result<Update, MirrorError> {
        self.mirror.apply(change)?;
        let Some(state) = self.mirror.state() else {
            return Ok(Update::default());
        };
        let view = thread_view(state);
        let state_change = self.tracker.observe(&view);
        if self.view.as_ref() == Some(&view) {
            return Ok(Update {
                view: None,
                state_change,
            });
        }
        self.view = Some(view.clone());
        Ok(Update {
            view: Some(view),
            state_change,
        })
    }

    /// The thread can no longer be seen, so a turn that was running is
    /// over as far as the window is concerned.
    fn on_lost(&mut self) -> Option<StateChange> {
        // An empty state reads as a thread the desktop app has not loaded.
        self.tracker.observe(&thread_view(&Value::Null))
    }
}

#[cfg_attr(not(unix), allow(dead_code))]
fn text_input(text: &str) -> Value {
    json!([{"type": "text", "text": text, "text_elements": []}])
}

#[cfg_attr(not(unix), allow(dead_code))]
/// A new turn when Codex is idle, or a steer into the one it is running,
/// which is how typing into the desktop app's composer behaves.
fn turn_request(view: Option<&ThreadView>, thread_id: &str, text: &str) -> (Method, Value) {
    if view.and_then(|view| view.active_turn_id.as_ref()).is_some() {
        return (
            Method::SteerTurn,
            json!({
                "conversationId": thread_id,
                "clientUserMessageId": uuid::Uuid::new_v4().to_string(),
                "input": text_input(text),
                "serviceTier": null,
                "attachments": [],
                "additionalContext": null,
                "toolOutput": null,
                "restoreMessage": {"text": text, "cwd": null, "context": {}},
            }),
        );
    }
    (
        Method::StartTurn,
        json!({
            "conversationId": thread_id,
            "turnStart": {
                "request": {"threadId": thread_id, "input": text_input(text)},
                "context": {},
            },
        }),
    )
}

#[cfg_attr(not(unix), allow(dead_code))]
fn interrupt_request(
    view: Option<&ThreadView>,
    thread_id: &str,
) -> Result<(Method, Value), String> {
    let turn = view
        .and_then(|view| view.active_turn_id.as_deref())
        .ok_or("Codex is not running a turn.")?;
    Ok((
        Method::InterruptTurn,
        json!({"conversationId": thread_id, "mode": "user-stop", "expectedTurnId": turn}),
    ))
}

#[cfg_attr(not(unix), allow(dead_code))]
fn answer_request(
    view: Option<&ThreadView>,
    thread_id: &str,
    decision: Value,
) -> Result<(Method, Value), String> {
    let approval = view
        .and_then(|view| view.approval.as_ref())
        .ok_or("Codex is not waiting on an approval.")?;
    let method = match approval.kind {
        ApprovalKind::Command => Method::CommandApproval,
        ApprovalKind::FileChange => Method::FileApproval,
        ApprovalKind::Other => return Err("Answer this one in the Codex desktop app.".into()),
    };
    Ok((
        method,
        json!({
            "conversationId": thread_id,
            "requestId": approval.request_id,
            "decision": decision,
        }),
    ))
}

#[cfg_attr(not(unix), allow(dead_code))]
fn session_id(thread_id: &str) -> String {
    format!("codex:{thread_id}")
}

#[cfg(unix)]
mod live {
    //! The part that needs a running desktop app and a window.

    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, Weak};
    use std::time::{Duration, Instant};

    use super::*;
    use bus::{Bus, BusError, StreamEvent, StreamSink};

    /// How long a thread gets to appear after asking the desktop app to
    /// open it, and how long a reloaded window gets to take threads back.
    const OWNER_WAIT: Duration = Duration::from_secs(8);
    const OWNER_POLL: Duration = Duration::from_millis(400);

    struct Tab {
        follower: Arc<Mutex<Follower>>,
    }

    #[derive(Default)]
    pub struct Sessions {
        bus: Mutex<Option<Arc<Bus>>>,
        tabs: Mutex<HashMap<String, Tab>>,
    }

    impl Sessions {
        fn bus(&self) -> Result<Arc<Bus>, String> {
            let mut current = self.bus.lock().unwrap();
            if let Some(bus) = current.as_ref().filter(|bus| bus.is_alive()) {
                return Ok(bus.clone());
            }
            let path = bus::socket_path(
                std::env::var_os("CODEX_HOME").map(Into::into),
                std::env::var_os("HOME").map(Into::into),
            )
            .ok_or("HOME is not set, so the Codex desktop app cannot be found.")?;
            let bus = Arc::new(Bus::connect(&path).map_err(|e| e.to_string())?);
            *current = Some(bus.clone());
            Ok(bus)
        }

        fn follower(&self, thread_id: &str) -> Result<Arc<Mutex<Follower>>, String> {
            self.tabs
                .lock()
                .unwrap()
                .get(thread_id)
                .map(|tab| tab.follower.clone())
                .ok_or_else(|| "This Codex thread is not open in oTerm.".into())
        }

        fn view(&self, thread_id: &str) -> Result<Option<ThreadView>, String> {
            Ok(self.follower(thread_id)?.lock().unwrap().view.clone())
        }

        fn request(&self, method: Method, params: Value) -> Result<(), String> {
            self.bus()?
                .request(method, params)
                .map(|_| ())
                .map_err(|e| e.to_string())
        }

        pub fn open(
            &self,
            thread_id: &str,
            events: Channel<CodexEvent>,
            app: &AppHandle,
            choreo: &Choreographer,
        ) -> Result<String, String> {
            // Opening a thread that is already open is how a tab reconnects,
            // so whatever was there is dropped first.
            self.close(thread_id, choreo);
            let bus = self.bus()?;
            wait_for_owner(&bus, thread_id)?;

            let follower = Arc::new(Mutex::new(Follower::default()));
            let id = session_id(thread_id);
            choreo.add_session(app, &id, {
                let follower = follower.clone();
                Arc::new(move || follower.lock().unwrap().tracker.state() == AgentState::Busy)
            });
            let sink = sink(
                thread_id,
                follower.clone(),
                events,
                Arc::downgrade(&bus),
                app.clone(),
                choreo.clone(),
            );
            if let Err(error) = bus.follow(thread_id, sink) {
                choreo.remove_session(&id);
                return Err(error.to_string());
            }
            self.tabs
                .lock()
                .unwrap()
                .insert(thread_id.to_owned(), Tab { follower });
            Ok(id)
        }

        pub fn send(
            &self,
            thread_id: &str,
            text: &str,
            app: &AppHandle,
            choreo: &Choreographer,
        ) -> Result<(), String> {
            let (method, params) = turn_request(self.view(thread_id)?.as_ref(), thread_id, text);
            self.request(method, params)?;
            choreo.on_submit(app, &session_id(thread_id));
            Ok(())
        }

        pub fn interrupt(&self, thread_id: &str) -> Result<(), String> {
            let (method, params) = interrupt_request(self.view(thread_id)?.as_ref(), thread_id)?;
            self.request(method, params)
        }

        pub fn answer(&self, thread_id: &str, decision: Value) -> Result<(), String> {
            let (method, params) =
                answer_request(self.view(thread_id)?.as_ref(), thread_id, decision)?;
            self.request(method, params)
        }

        pub fn close(&self, thread_id: &str, choreo: &Choreographer) {
            if self.tabs.lock().unwrap().remove(thread_id).is_none() {
                return;
            }
            if let Some(bus) = self.bus.lock().unwrap().as_ref() {
                bus.unfollow(thread_id);
            }
            choreo.remove_session(&session_id(thread_id));
        }
    }

    /// Make sure the desktop app has the thread loaded, asking it to open
    /// the thread in the background if it does not.
    fn wait_for_owner(bus: &Bus, thread_id: &str) -> Result<(), String> {
        let discover = || {
            bus.request(
                Method::OwnerDiscovery,
                json!({"conversationId": thread_id, "hostId": "local"}),
            )
        };
        match discover() {
            Ok(_) => return Ok(()),
            Err(BusError::NoOwner) => {}
            Err(error) => return Err(error.to_string()),
        }
        open_in_background(thread_id)?;
        let deadline = Instant::now() + OWNER_WAIT;
        while Instant::now() < deadline {
            std::thread::sleep(OWNER_POLL);
            match discover() {
                Ok(_) => return Ok(()),
                Err(BusError::NoOwner) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        Err(BusError::NoOwner.to_string())
    }

    #[cfg(target_os = "macos")]
    fn open_in_background(thread_id: &str) -> Result<(), String> {
        // `-g` keeps the desktop app behind oTerm while it loads the thread.
        std::process::Command::new("open")
            .args(["-g", &format!("codex://threads/{thread_id}")])
            .status()
            .map_err(|e| format!("Could not ask the Codex desktop app to open the thread: {e}"))
            .map(|_| ())
    }

    #[cfg(not(target_os = "macos"))]
    fn open_in_background(_thread_id: &str) -> Result<(), String> {
        Err("Open this thread in the Codex desktop app first.".into())
    }

    /// What happens to one thread's stream events. Runs on the bus's reader
    /// thread, so anything that needs an answer from the bus is sent off to
    /// a thread of its own.
    fn sink(
        thread_id: &str,
        follower: Arc<Mutex<Follower>>,
        events: Channel<CodexEvent>,
        bus: Weak<Bus>,
        app: AppHandle,
        choreo: Choreographer,
    ) -> StreamSink {
        let thread_id = thread_id.to_owned();
        let id = session_id(&thread_id);
        let problem = move |events: &Channel<CodexEvent>, message: Option<&str>| {
            let _ = events.send(CodexEvent::Problem {
                message: message.map(str::to_owned),
            });
        };
        Arc::new(move |event| {
            let report = |change: StateChange| {
                let _ = events.send(CodexEvent::AgentStateChanged {
                    state: change.to,
                    cause: format!("{:?}", change.cause),
                });
                choreo.on_state_change(&app, &id, &change);
            };
            match event {
                StreamEvent::Change(change) => {
                    // The lock is released before anything reaches the
                    // window: the choreography asks the follower whether it
                    // is working, from threads of its own.
                    let update = follower.lock().unwrap().on_change(&change);
                    match update {
                        Ok(update) => {
                            if let Some(view) = update.view {
                                let _ = events.send(CodexEvent::View(Box::new(view)));
                            }
                            if let Some(change) = update.state_change {
                                report(change);
                            }
                        }
                        Err(_) => {
                            // Asking for the history again makes the owner
                            // send a fresh snapshot.
                            let bus = bus.clone();
                            let thread_id = thread_id.clone();
                            std::thread::spawn(move || {
                                if let Some(bus) = bus.upgrade() {
                                    let _ = bus.request(
                                        Method::LoadHistory,
                                        json!({"conversationId": thread_id}),
                                    );
                                }
                            });
                        }
                    }
                }
                StreamEvent::Incompatible => {
                    problem(&events, Some(&BusError::Incompatible.to_string()));
                }
                StreamEvent::OwnerGone => {
                    problem(
                        &events,
                        Some("Codex reloaded. Reconnecting to this thread\u{2026}"),
                    );
                    let bus = bus.clone();
                    let thread_id = thread_id.clone();
                    let events = events.clone();
                    std::thread::spawn(move || {
                        let deadline = Instant::now() + OWNER_WAIT;
                        while Instant::now() < deadline {
                            std::thread::sleep(OWNER_POLL);
                            let Some(bus) = bus.upgrade() else { return };
                            if bus.refollow(&thread_id).is_ok() {
                                problem(&events, None);
                                return;
                            }
                        }
                        problem(
                            &events,
                            Some("Codex stopped sharing this thread. Reconnect to try again."),
                        );
                    });
                }
                StreamEvent::Disconnected => {
                    let lost = follower.lock().unwrap().on_lost();
                    if let Some(change) = lost {
                        report(change);
                    }
                    problem(
                        &events,
                        Some("The Codex desktop app closed. Open it again, then reconnect."),
                    );
                }
            }
        })
    }
}

#[cfg(unix)]
pub use live::Sessions as CodexSessions;

/// Codex's desktop bus is a Unix socket here and a named pipe on Windows.
/// Only the socket is spoken so far.
#[cfg(not(unix))]
#[derive(Default)]
pub struct CodexSessions {}

#[cfg(not(unix))]
impl CodexSessions {
    const UNSUPPORTED: &str = "Codex threads are not supported on this platform yet.";

    pub fn open(
        &self,
        _: &str,
        _: Channel<CodexEvent>,
        _: &AppHandle,
        _: &Choreographer,
    ) -> Result<String, String> {
        Err(Self::UNSUPPORTED.into())
    }
    pub fn send(&self, _: &str, _: &str, _: &AppHandle, _: &Choreographer) -> Result<(), String> {
        Err(Self::UNSUPPORTED.into())
    }
    pub fn interrupt(&self, _: &str) -> Result<(), String> {
        Err(Self::UNSUPPORTED.into())
    }
    pub fn answer(&self, _: &str, _: Value) -> Result<(), String> {
        Err(Self::UNSUPPORTED.into())
    }
    pub fn close(&self, _: &str, _: &Choreographer) {}
}

#[tauri::command(async)]
pub fn codex_list_threads() -> Result<Vec<ThreadSummary>, String> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let binary = threads::codex_binary(home.as_deref(), std::env::var_os("PATH").as_deref())
        .ok_or("Codex is not installed. oTerm looks for the Codex or ChatGPT desktop app.")?;
    threads::list_threads(&binary)
}

#[tauri::command(async)]
pub fn codex_open_thread(
    thread_id: String,
    on_event: Channel<CodexEvent>,
    app: AppHandle,
    sessions: State<'_, CodexSessions>,
    choreo: State<'_, Choreographer>,
) -> Result<String, String> {
    sessions.open(&thread_id, on_event, &app, &choreo)
}

#[tauri::command(async)]
pub fn codex_send(
    thread_id: String,
    text: String,
    app: AppHandle,
    sessions: State<'_, CodexSessions>,
    choreo: State<'_, Choreographer>,
) -> Result<(), String> {
    sessions.send(&thread_id, &text, &app, &choreo)
}

#[tauri::command(async)]
pub fn codex_interrupt(
    thread_id: String,
    sessions: State<'_, CodexSessions>,
) -> Result<(), String> {
    sessions.interrupt(&thread_id)
}

#[tauri::command(async)]
pub fn codex_answer(
    thread_id: String,
    decision: Value,
    sessions: State<'_, CodexSessions>,
) -> Result<(), String> {
    sessions.answer(&thread_id, decision)
}

#[tauri::command(async)]
pub fn codex_close(
    thread_id: String,
    sessions: State<'_, CodexSessions>,
    choreo: State<'_, Choreographer>,
) {
    sessions.close(&thread_id, &choreo);
}

#[cfg(test)]
mod tests {
    use super::*;
    use overterm_core::Signal;
    use overterm_core::codex::view::{Activity, Approval};

    fn view(
        activity: Activity,
        active_turn: Option<&str>,
        approval: Option<Approval>,
    ) -> ThreadView {
        ThreadView {
            title: "Fixture".into(),
            cwd: None,
            activity,
            active_turn_id: active_turn.map(str::to_owned),
            turns: Vec::new(),
            approval,
        }
    }

    fn approval(kind: ApprovalKind) -> Option<Approval> {
        Some(Approval {
            request_id: json!(2),
            kind,
            reason: None,
            command: Some("touch x".into()),
            cwd: None,
            available_decisions: None,
        })
    }

    fn approval_with_decisions(
        kind: ApprovalKind,
        available_decisions: Vec<Value>,
    ) -> Option<Approval> {
        Some(Approval {
            available_decisions: Some(available_decisions),
            ..approval(kind).unwrap()
        })
    }

    fn recorded_changes() -> Vec<Value> {
        include_str!("../../../crates/core/fixtures/codex-approval-declined.jsonl")
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["params"]["change"].clone())
            .collect()
    }

    #[test]
    fn an_idle_thread_gets_a_new_turn() {
        let idle = view(Activity::Idle, None, None);
        let (method, params) = turn_request(Some(&idle), "t1", "hello");
        assert_eq!(method, Method::StartTurn);
        assert_eq!(
            params,
            json!({"conversationId": "t1", "turnStart": {
                "request": {"threadId": "t1", "input": [{"type": "text", "text": "hello", "text_elements": []}]},
                "context": {}
            }})
        );
        // Before the first snapshot there is nothing running to steer.
        assert_eq!(turn_request(None, "t1", "hello").0, Method::StartTurn);
    }

    #[test]
    fn a_running_turn_is_steered() {
        let working = view(Activity::Working, Some("turn-1"), None);
        let (method, params) = turn_request(Some(&working), "t1", "also this");
        assert_eq!(method, Method::SteerTurn);
        assert_eq!(params["conversationId"], "t1");
        assert_eq!(
            params["input"],
            json!([{"type": "text", "text": "also this", "text_elements": []}])
        );
        assert!(
            params["clientUserMessageId"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        );
        // The owner reads `restoreMessage.context` while steering, so it
        // has to be an object even though oTerm has nothing to put in it.
        assert_eq!(params["restoreMessage"]["context"], json!({}));
    }

    #[test]
    fn interrupt_names_the_running_turn() {
        let working = view(Activity::Working, Some("turn-1"), None);
        assert_eq!(
            interrupt_request(Some(&working), "t1"),
            Ok((
                Method::InterruptTurn,
                json!({"conversationId": "t1", "mode": "user-stop", "expectedTurnId": "turn-1"})
            ))
        );
        assert!(interrupt_request(Some(&view(Activity::Idle, None, None)), "t1").is_err());
    }

    #[test]
    fn approvals_go_to_the_method_for_their_kind() {
        let command = view(
            Activity::Waiting,
            Some("turn-1"),
            approval_with_decisions(
                ApprovalKind::Command,
                vec![
                    json!("accept"),
                    json!({"acceptWithExecpolicyAmendment": {
                        "execpolicy_amendment": ["/bin/zsh", "-lc", "touch x"]
                    }}),
                    json!("cancel"),
                ],
            ),
        );
        assert_eq!(
            answer_request(Some(&command), "t1", json!("accept")),
            Ok((
                Method::CommandApproval,
                json!({"conversationId": "t1", "requestId": 2, "decision": "accept"})
            ))
        );
        assert_eq!(
            answer_request(Some(&command), "t1", json!("cancel"))
                .unwrap()
                .1["decision"],
            "cancel"
        );
        let amendment = json!({"acceptWithExecpolicyAmendment": {
            "execpolicy_amendment": ["/bin/zsh", "-lc", "touch x"]
        }});
        assert_eq!(
            answer_request(Some(&command), "t1", amendment.clone())
                .unwrap()
                .1["decision"],
            amendment
        );
        let file = view(
            Activity::Waiting,
            Some("turn-1"),
            approval(ApprovalKind::FileChange),
        );
        assert_eq!(
            answer_request(Some(&file), "t1", json!("accept"))
                .unwrap()
                .0,
            Method::FileApproval
        );
        let other = view(
            Activity::Waiting,
            Some("turn-1"),
            approval(ApprovalKind::Other),
        );
        assert!(answer_request(Some(&other), "t1", json!("accept")).is_err());
        assert!(
            answer_request(
                Some(&view(Activity::Idle, None, None)),
                "t1",
                json!("accept")
            )
            .is_err()
        );
    }

    #[test]
    fn follower_reports_only_views_that_changed() {
        let mut follower = Follower::default();
        let changes = recorded_changes();
        let updates: Vec<Update> = changes
            .iter()
            .map(|c| follower.on_change(c).unwrap())
            .collect();
        let views = updates.iter().filter(|u| u.view.is_some()).count();
        assert!(
            views > 1 && views < changes.len(),
            "{views} of {}",
            changes.len()
        );
        let states: Vec<AgentState> = updates
            .iter()
            .filter_map(|u| u.state_change.as_ref().map(|c| c.to))
            .collect();
        assert_eq!(
            states,
            [
                AgentState::Busy,
                AgentState::NeedsInput,
                AgentState::Busy,
                AgentState::Done
            ]
        );
        assert_eq!(follower.view.as_ref().unwrap().turns.len(), 4);
    }

    #[test]
    fn a_broken_stream_is_an_error_until_the_next_snapshot() {
        let mut follower = Follower::default();
        let changes = recorded_changes();
        follower.on_change(&changes[0]).unwrap();
        // Skipping a patch leaves a revision gap.
        assert!(follower.on_change(&changes[3]).is_err());
        let snapshot = changes
            .iter()
            .rev()
            .find(|c| c["type"] == "snapshot")
            .unwrap();
        assert!(follower.on_change(snapshot).is_ok());
    }

    #[test]
    fn losing_a_busy_thread_settles_the_window() {
        let mut follower = Follower::default();
        let changes = recorded_changes();
        // Up to the frame where the turn is running.
        for change in &changes[..5] {
            follower.on_change(change).unwrap();
        }
        assert_eq!(follower.tracker.state(), AgentState::Busy);
        let change = follower.on_lost().unwrap();
        assert_eq!(
            (change.to, change.cause),
            (AgentState::Idle, Signal::FullScreenExited)
        );
        assert!(follower.on_lost().is_none());
    }
}
