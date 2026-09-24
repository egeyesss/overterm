//! What a mirrored conversation shows, and what it means for the window.
//!
//! The desktop app's conversation state is large and internal. This reads
//! the handful of fields a chat tab needs out of it, so the frontend never
//! has to know that shape and a change to it breaks one tested function.

use serde::Serialize;
use serde_json::Value;

use crate::detect::{AgentState, Signal, StateChange};

/// Command output shown in the tab is cut to its last this-many characters.
/// The full output stays in the desktop app.
pub const OUTPUT_TAIL_CHARS: usize = 4000;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadView {
    pub title: String,
    pub cwd: Option<String>,
    pub activity: Activity,
    /// The turn a steer or an interrupt would go to.
    pub active_turn_id: Option<String>,
    pub turns: Vec<TurnView>,
    pub approval: Option<Approval>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Activity {
    Idle,
    Working,
    /// Working, but stopped on a question for the user.
    Waiting,
    /// The desktop app has not loaded the thread, or it failed.
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnView {
    pub id: String,
    pub status: String,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Item {
    User {
        id: String,
        text: String,
    },
    Agent {
        id: String,
        text: String,
        /// Progress notes Codex writes while it works, as opposed to the
        /// answer that ends the turn.
        commentary: bool,
    },
    Command {
        id: String,
        command: String,
        status: String,
        output: Option<String>,
        exit_code: Option<i64>,
    },
    FileChange {
        id: String,
        status: String,
        paths: Vec<String>,
    },
    /// Anything else worth a line, such as a plan or a subagent starting.
    Note {
        id: String,
        text: String,
    },
}

impl Item {
    pub fn id(&self) -> &str {
        match self {
            Item::User { id, .. }
            | Item::Agent { id, .. }
            | Item::Command { id, .. }
            | Item::FileChange { id, .. }
            | Item::Note { id, .. } => id,
        }
    }

    /// The same name the frontend reads from the serialized `kind` tag.
    pub fn kind(&self) -> &'static str {
        match self {
            Item::User { .. } => "user",
            Item::Agent { .. } => "agent",
            Item::Command { .. } => "command",
            Item::FileChange { .. } => "fileChange",
            Item::Note { .. } => "note",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    /// Passed back unchanged when answering: the desktop app uses numbers
    /// but the protocol allows strings.
    pub request_id: Value,
    pub kind: ApprovalKind,
    pub reason: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    /// The exact decisions offered by Codex, including object decisions.
    /// `None` means the request did not provide usable decision metadata.
    pub available_decisions: Option<Vec<Value>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalKind {
    Command,
    FileChange,
    /// Permission grants, questions and the like. oTerm shows that Codex is
    /// waiting but leaves the answer to the desktop app.
    Other,
}

pub fn thread_view(state: &Value) -> ThreadView {
    let approval = state
        .get("requests")
        .and_then(Value::as_array)
        .and_then(|requests| requests.first())
        .map(approval);
    let status = state.get("threadRuntimeStatus");
    let activity = match status.and_then(|s| s.get("type")).and_then(Value::as_str) {
        Some("idle") => Activity::Idle,
        Some("active") => {
            let flagged = status
                .and_then(|s| s.get("activeFlags"))
                .and_then(Value::as_array)
                .is_some_and(|flags| !flags.is_empty());
            if flagged || approval.is_some() {
                Activity::Waiting
            } else {
                Activity::Working
            }
        }
        _ => Activity::Unavailable,
    };
    let turns = turns(state);
    let active_turn_id = match activity {
        Activity::Working | Activity::Waiting => turns
            .iter()
            .rev()
            .find(|turn| turn.status == "inProgress")
            .map(|turn| turn.id.clone()),
        Activity::Idle | Activity::Unavailable => None,
    };
    ThreadView {
        title: text(state, "title")
            .or_else(|| text(state, "generatedTitle"))
            .unwrap_or_else(|| "Untitled thread".into()),
        cwd: text(state, "cwd"),
        activity,
        active_turn_id,
        turns,
        approval,
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Turns in display order. History is paged into islands, each an ordered
/// list of keys into one shared table of turns.
fn turns(state: &Value) -> Vec<TurnView> {
    let history = &state["turnHistory"]["history"];
    let Some(islands) = history.get("islands").and_then(Value::as_array) else {
        return Vec::new();
    };
    islands
        .iter()
        .filter_map(|island| island.get("entries").and_then(Value::as_array))
        .flatten()
        .filter_map(|entry| {
            let key = entry.get("value").and_then(Value::as_str)?;
            let turn = history.get("entitiesByKey")?.get(key)?;
            Some(TurnView {
                // A turn the server has not confirmed yet has no id of its
                // own, and the table key is stable until it does.
                id: text(turn, "turnId").unwrap_or_else(|| key.to_owned()),
                status: text(turn, "status").unwrap_or_else(|| "unknown".into()),
                items: turn
                    .get("items")
                    .and_then(Value::as_array)
                    .map(|items| items.iter().filter_map(item).collect())
                    .unwrap_or_default(),
            })
        })
        .collect()
}

fn item(value: &Value) -> Option<Item> {
    let id = text(value, "id").unwrap_or_default();
    let note = |text: String| {
        Some(Item::Note {
            id: id.clone(),
            text,
        })
    };
    match value.get("type").and_then(Value::as_str)? {
        "userMessage" => Some(Item::User {
            id: id.clone(),
            text: input_text(value.get("content")),
        }),
        "steeringUserMessage" => Some(Item::User {
            id: id.clone(),
            text: input_text(value.get("input")),
        }),
        "agentMessage" => Some(Item::Agent {
            id: id.clone(),
            text: text(value, "text").unwrap_or_default(),
            commentary: value.get("phase").and_then(Value::as_str) == Some("commentary"),
        }),
        "commandExecution" => Some(Item::Command {
            id: id.clone(),
            command: text(value, "command").unwrap_or_default(),
            status: text(value, "status").unwrap_or_else(|| "unknown".into()),
            output: text(value, "aggregatedOutput").map(|output| tail(&output)),
            exit_code: value.get("exitCode").and_then(Value::as_i64),
        }),
        "fileChange" => Some(Item::FileChange {
            id: id.clone(),
            status: text(value, "status").unwrap_or_else(|| "unknown".into()),
            paths: value
                .get("changes")
                .and_then(Value::as_array)
                .map(|changes| changes.iter().filter_map(|c| text(c, "path")).collect())
                .unwrap_or_default(),
        }),
        "plan" => note(format!("Plan: {}", text(value, "text").unwrap_or_default())),
        "subAgentActivity" => note(format!(
            "Subagent {}",
            text(value, "agentPath")
                .or_else(|| text(value, "agentThreadId"))
                .unwrap_or_default()
        )),
        "collabAgentToolCall" => note("Subagent call".into()),
        "mcpToolCall" => note(format!(
            "Tool {}/{}",
            text(value, "server").unwrap_or_default(),
            text(value, "tool").unwrap_or_default()
        )),
        "webSearch" => note(match text(value, "query") {
            Some(query) => format!("Searched the web for {query}"),
            None => "Searched the web".into(),
        }),
        "imageView" => note(format!(
            "Viewed {}",
            text(value, "path").unwrap_or_default()
        )),
        "imageGeneration" => note("Generated an image".into()),
        "contextCompaction" => note("Context compacted".into()),
        "enteredReviewMode" => note("Review started".into()),
        "exitedReviewMode" => note("Review finished".into()),
        // Empty in the stream, or bookkeeping the chat does not need.
        "reasoning" | "steered" | "hookPrompt" | "functionCallOutput" | "sleep"
        | "dynamicToolCall" => None,
        // Shown by name rather than dropped, so a new kind of item is
        // visible as something missing rather than silently absent.
        other => note(format!("[{other}]")),
    }
}

/// The text of a list of user inputs, with anything that is not text
/// named in brackets.
fn input_text(input: Option<&Value>) -> String {
    input
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .map(|part| match part.get("type").and_then(Value::as_str) {
                    Some("text") => text(part, "text").unwrap_or_default(),
                    Some(kind) => format!("[{kind}]"),
                    None => String::new(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn tail(output: &str) -> String {
    let count = output.chars().count();
    if count <= OUTPUT_TAIL_CHARS {
        return output.to_owned();
    }
    output.chars().skip(count - OUTPUT_TAIL_CHARS).collect()
}

fn approval(request: &Value) -> Approval {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").unwrap_or(&Value::Null);
    let kind = match method {
        "item/commandExecution/requestApproval" | "execCommandApproval" => ApprovalKind::Command,
        "item/fileChange/requestApproval" | "applyPatchApproval" => ApprovalKind::FileChange,
        _ => ApprovalKind::Other,
    };
    Approval {
        request_id: request.get("id").cloned().unwrap_or(Value::Null),
        kind,
        reason: text(params, "reason"),
        command: text(params, "command"),
        cwd: text(params, "cwd"),
        available_decisions: params
            .get("availableDecisions")
            .and_then(Value::as_array)
            .filter(|decisions| !decisions.is_empty())
            .cloned(),
    }
}

/// Turns the views of one conversation into the state changes the window
/// choreography understands.
#[derive(Debug)]
pub struct Tracker {
    state: AgentState,
}

impl Default for Tracker {
    fn default() -> Self {
        Self {
            state: AgentState::Idle,
        }
    }
}

impl Tracker {
    pub fn state(&self) -> AgentState {
        self.state
    }

    pub fn observe(&mut self, view: &ThreadView) -> Option<StateChange> {
        let was_working = matches!(self.state, AgentState::Busy | AgentState::NeedsInput);
        let (to, cause) = match view.activity {
            Activity::Working => (AgentState::Busy, Signal::HookSubmit),
            Activity::Waiting => (AgentState::NeedsInput, Signal::HookNotification),
            // Idle only means a turn finished if one was seen running.
            // Attaching to a quiet thread must not raise a done cue.
            Activity::Idle if was_working => {
                let failed = view
                    .turns
                    .last()
                    .is_some_and(|turn| turn.status == "failed");
                let cause = if failed {
                    Signal::HookStopFailure
                } else {
                    Signal::HookStop
                };
                (AgentState::Done, cause)
            }
            Activity::Unavailable if was_working => (AgentState::Idle, Signal::FullScreenExited),
            Activity::Idle | Activity::Unavailable => return None,
        };
        if to == self.state {
            return None;
        }
        let change = StateChange {
            from: self.state,
            to,
            cause,
        };
        self.state = to;
        Some(change)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::mirror::Mirror;
    use serde_json::json;

    /// Every view the recorded approval turn went through, in order.
    fn recorded_views() -> Vec<ThreadView> {
        let recording = include_str!("../../fixtures/codex-approval-declined.jsonl");
        let mut mirror = Mirror::default();
        recording
            .lines()
            .map(|line| {
                let frame: Value = serde_json::from_str(line).unwrap();
                mirror.apply(&frame["params"]["change"]).unwrap();
                thread_view(mirror.state().unwrap())
            })
            .collect()
    }

    fn state_with(turn_items: Value, status: Value, requests: Value) -> Value {
        json!({
            "title": "Fixture",
            "cwd": "/work/project",
            "threadRuntimeStatus": status,
            "requests": requests,
            "turnHistory": {"history": {
                "islands": [{"id": "tail:0", "entries": [{"key": "k1", "value": "k1"}]}],
                "entitiesByKey": {"k1": {"turnId": "turn-1", "status": "inProgress", "items": turn_items}}
            }}
        })
    }

    #[test]
    fn reads_title_cwd_and_turns_in_island_order() {
        let view = recorded_views().pop().unwrap();
        assert_eq!(view.title, "Test PR");
        assert_eq!(
            view.cwd.as_deref(),
            Some("/Users/someone/Desktop/testing_pr")
        );
        assert_eq!(view.activity, Activity::Idle);
        assert_eq!(view.turns.len(), 4);
        assert_eq!(
            view.turns[0].items[0],
            Item::User {
                id: view.turns[0].items[0].id().to_owned(),
                text: "testing the PR, tell me something.\n".into()
            }
        );
    }

    #[test]
    fn the_declined_turn_reads_as_a_conversation() {
        let view = recorded_views().pop().unwrap();
        let last = view.turns.last().unwrap();
        assert_eq!(last.status, "completed");
        let kinds: Vec<&str> = last.items.iter().map(Item::kind).collect();
        // Reasoning is dropped: it is empty in the stream and noise in a chat.
        assert_eq!(kinds, ["user", "agent", "command", "agent"]);
        let Item::Command {
            command, status, ..
        } = &last.items[2]
        else {
            panic!("expected a command");
        };
        assert_eq!(
            command,
            "/bin/zsh -lc 'touch ~/Desktop/overterm-approval-probe.txt'"
        );
        assert_eq!(status, "declined");
        assert!(matches!(
            &last.items[1],
            Item::Agent {
                commentary: true,
                ..
            }
        ));
        assert!(
            matches!(&last.items[3], Item::Agent { text, commentary: false, .. } if text == "declined")
        );
    }

    #[test]
    fn a_pending_command_approval_is_surfaced_with_its_command() {
        let views = recorded_views();
        let waiting = views
            .iter()
            .find(|view| view.approval.is_some())
            .expect("the recording asked for approval");
        assert_eq!(waiting.activity, Activity::Waiting);
        let approval = waiting.approval.as_ref().unwrap();
        assert_eq!(approval.request_id, json!(2));
        assert_eq!(approval.kind, ApprovalKind::Command);
        assert_eq!(
            approval.command.as_deref(),
            Some("/bin/zsh -lc 'touch ~/Desktop/overterm-approval-probe.txt'")
        );
        let decisions = json!([
            "accept",
            {"acceptWithExecpolicyAmendment": {
                "execpolicy_amendment": ["/bin/zsh", "-lc", "touch ~/Desktop/overterm-approval-probe.txt"]
            }},
            "cancel"
        ]);
        assert_eq!(
            approval.available_decisions,
            Some(decisions.as_array().unwrap().clone())
        );
        assert_eq!(
            serde_json::to_value(waiting).unwrap()["approval"]["availableDecisions"],
            decisions
        );
        assert!(
            approval
                .reason
                .as_deref()
                .unwrap()
                .contains("outside the workspace")
        );
        assert!(waiting.active_turn_id.is_some());
    }

    #[test]
    fn steering_messages_read_as_user_messages_and_markers_are_dropped() {
        let state = state_with(
            json!([
                {"type": "steeringUserMessage", "id": "s1", "status": "accepted",
                 "input": [{"type": "text", "text": "also do this", "text_elements": []}]},
                {"type": "steered", "id": "m1"},
                {"type": "reasoning", "id": "r1", "summary": [], "content": []}
            ]),
            json!({"type": "active", "activeFlags": []}),
            json!([]),
        );
        let view = thread_view(&state);
        assert_eq!(view.activity, Activity::Working);
        assert_eq!(view.active_turn_id.as_deref(), Some("turn-1"));
        assert_eq!(
            view.turns[0].items,
            [Item::User {
                id: "s1".into(),
                text: "also do this".into()
            }]
        );
    }

    #[test]
    fn long_command_output_keeps_only_its_tail() {
        let output = format!("{}END", "x".repeat(OUTPUT_TAIL_CHARS * 2));
        let state = state_with(
            json!([{"type": "commandExecution", "id": "c1", "command": "make", "status": "completed",
                     "aggregatedOutput": output, "exitCode": 0}]),
            json!({"type": "idle"}),
            json!([]),
        );
        let Item::Command {
            output, exit_code, ..
        } = &thread_view(&state).turns[0].items[0]
        else {
            panic!("expected a command");
        };
        let output = output.as_deref().unwrap();
        assert_eq!(output.chars().count(), OUTPUT_TAIL_CHARS);
        assert!(output.ends_with("END"));
        assert_eq!(*exit_code, Some(0));
    }

    #[test]
    fn file_changes_and_unanswerable_requests() {
        let state = state_with(
            json!([{"type": "fileChange", "id": "f1", "status": "completed",
                     "changes": [{"path": "src/a.rs", "kind": {"type": "update"}, "diff": ""}]}]),
            json!({"type": "active", "activeFlags": ["waitingOnUserInput"]}),
            json!([{"method": "item/tool/requestUserInput", "id": "q1", "params": {"threadId": "t"}}]),
        );
        let view = thread_view(&state);
        assert_eq!(
            view.turns[0].items[0],
            Item::FileChange {
                id: "f1".into(),
                status: "completed".into(),
                paths: vec!["src/a.rs".into()]
            }
        );
        let approval = view.approval.unwrap();
        assert_eq!(approval.kind, ApprovalKind::Other);
        assert_eq!(approval.request_id, json!("q1"));
        assert_eq!(approval.available_decisions, None);
        assert_eq!(view.activity, Activity::Waiting);
    }

    #[test]
    fn missing_or_malformed_available_decisions_fall_back_to_legacy_choices() {
        let missing = state_with(
            json!([]),
            json!({"type": "active", "activeFlags": ["waitingOnApproval"]}),
            json!([{
                "method": "item/commandExecution/requestApproval",
                "id": 2,
                "params": {"kind": "command"}
            }]),
        );
        assert_eq!(
            thread_view(&missing).approval.unwrap().available_decisions,
            None
        );

        let state = state_with(
            json!([]),
            json!({"type": "active", "activeFlags": ["waitingOnApproval"]}),
            json!([{
                "method": "item/commandExecution/requestApproval",
                "id": 2,
                "params": {
                    "kind": "command",
                    "availableDecisions": {"accept": true}
                }
            }]),
        );
        let view = thread_view(&state);
        assert_eq!(view.approval.unwrap().available_decisions, None);
    }

    #[test]
    fn missing_fields_degrade_instead_of_failing() {
        let view = thread_view(&json!({}));
        assert_eq!(view.title, "Untitled thread");
        assert_eq!(view.activity, Activity::Unavailable);
        assert!(view.turns.is_empty());
        assert!(view.approval.is_none());
    }

    #[test]
    fn tracker_follows_the_recorded_turn_through_its_approval() {
        let mut tracker = Tracker::default();
        let changes: Vec<(AgentState, Signal)> = recorded_views()
            .iter()
            .filter_map(|view| tracker.observe(view))
            .map(|change| (change.to, change.cause))
            .collect();
        assert_eq!(
            changes,
            [
                (AgentState::Busy, Signal::HookSubmit),
                (AgentState::NeedsInput, Signal::HookNotification),
                (AgentState::Busy, Signal::HookSubmit),
                (AgentState::Done, Signal::HookStop),
            ]
        );
    }

    #[test]
    fn attaching_to_an_idle_thread_is_not_a_finished_turn() {
        let mut tracker = Tracker::default();
        let idle = thread_view(&state_with(json!([]), json!({"type": "idle"}), json!([])));
        assert!(tracker.observe(&idle).is_none());
        assert_eq!(tracker.state(), AgentState::Idle);
    }

    #[test]
    fn a_failed_turn_ends_on_stop_failure() {
        let mut tracker = Tracker::default();
        let working = state_with(
            json!([]),
            json!({"type": "active", "activeFlags": []}),
            json!([]),
        );
        tracker.observe(&thread_view(&working));
        let mut failed = working.clone();
        failed["threadRuntimeStatus"] = json!({"type": "idle"});
        failed["turnHistory"]["history"]["entitiesByKey"]["k1"]["status"] = json!("failed");
        let change = tracker.observe(&thread_view(&failed)).unwrap();
        assert_eq!(
            (change.to, change.cause),
            (AgentState::Done, Signal::HookStopFailure)
        );
    }

    #[test]
    fn losing_the_thread_mid_turn_settles_to_idle() {
        let mut tracker = Tracker::default();
        let working = state_with(
            json!([]),
            json!({"type": "active", "activeFlags": []}),
            json!([]),
        );
        tracker.observe(&thread_view(&working));
        let change = tracker.observe(&thread_view(&json!({}))).unwrap();
        assert_eq!(
            (change.to, change.cause),
            (AgentState::Idle, Signal::FullScreenExited)
        );
    }
}
