//! Replay the recorded fixtures and pin the transition sequences the
//! detector must produce for them. The fixtures are real recordings made
//! with the `record` example, timing included, so these tests exercise
//! the same byte streams and pacing a live session produces.

use std::path::PathBuf;

use overterm_core::detect::heuristic::{HeuristicAdapter, HeuristicConfig};
use overterm_core::detect::replay::{read_fixture, replay};
use overterm_core::detect::{AgentState, Detector, Signal, StateChange};

fn run(name: &str) -> Vec<(u64, StateChange)> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    let events = read_fixture(&path).expect("fixture should parse");
    let mut detector = Detector::new(vec![Box::new(HeuristicAdapter::new(
        HeuristicConfig::default(),
    ))]);
    replay(&mut detector, &events, 100, 1000)
}

fn states(changes: &[(u64, StateChange)]) -> Vec<AgentState> {
    changes.iter().map(|(_, c)| c.to).collect()
}

#[test]
fn shell_session_flips_busy_done_per_command() {
    // Recording: /bin/sh, `ls` submitted at 1000ms, `echo hello` at 2500ms.
    let changes = run("shell-ls-echo.ndjson");
    assert_eq!(
        states(&changes),
        vec![
            AgentState::Busy,
            AgentState::Done,
            AgentState::Busy,
            AgentState::Done,
        ],
        "full sequence: {changes:#?}"
    );
    // Each command's Done must land after its submit and before the next.
    assert_eq!(changes[0].0, 1000);
    assert!(changes[1].0 > 1000 && changes[1].0 < 2500);
    assert_eq!(changes[2].0, 2500);
    assert!(changes[3].0 > 2500);
    assert!(matches!(changes[1].1.cause, Signal::Quiescence { .. }));
}

#[test]
fn claude_session_reaches_done_when_answer_completes() {
    // Recording: real Claude Code TUI. Question submitted at 7000ms, the
    // streamed answer ends around 11300ms, /exit submitted at 40000ms.
    let changes = run("claude-simple-question.ndjson");
    assert_eq!(
        states(&changes),
        vec![
            AgentState::Busy,
            AgentState::Done,
            AgentState::Busy,
            AgentState::Done,
        ],
        "full sequence: {changes:#?}"
    );
    // Busy comes from the startup paint burst.
    assert!(matches!(changes[0].1.cause, Signal::OutputBurst));
    // Done must fire shortly after the answer stops streaming, well
    // before the next idle repaint at ~14300ms.
    assert!(
        changes[1].0 > 11300 && changes[1].0 < 12500,
        "answer-finished Done fired at {}ms",
        changes[1].0
    );
    // /exit flips back to Busy.
    assert_eq!(changes[2].0, 40000);
    assert!(matches!(changes[2].1.cause, Signal::UserInput));
}

#[test]
fn claude_idle_repaints_do_not_disturb_done() {
    // The TUI repaints a status row occasionally while idle (at roughly
    // 14300, 29300 and 37300ms in this recording). None of those small
    // chunks may bounce the state out of Done.
    let changes = run("claude-simple-question.ndjson");
    let between: Vec<_> = changes
        .iter()
        .filter(|(t, _)| *t > 12500 && *t < 40000)
        .collect();
    assert!(
        between.is_empty(),
        "unexpected transitions while idle: {between:#?}"
    );
}
