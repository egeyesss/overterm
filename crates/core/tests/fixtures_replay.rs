//! Replay the recorded fixtures and pin the transition sequences the
//! detector must produce for them. The fixtures are real recordings made
//! with the `record` example, timing included, so these tests exercise
//! the same byte streams and pacing a live session produces.

use std::path::PathBuf;

use overterm_core::detect::heuristic::{HeuristicAdapter, HeuristicConfig};
use overterm_core::detect::replay::{read_fixture, replay, replay_with};
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

/// Replay a fixture and record, on every tick, whether the detector could
/// show the session was working.
fn working_over_time(name: &str) -> Vec<(u64, bool)> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    let events = read_fixture(&path).expect("fixture should parse");
    let mut detector = Detector::new(vec![Box::new(HeuristicAdapter::new(
        HeuristicConfig::default(),
    ))]);
    let mut samples = Vec::new();
    replay_with(&mut detector, &events, 100, 1000, |t, now, detector| {
        samples.push((t, detector.is_working(now)));
    });
    samples
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
    // Recording: real Claude Code TUI. Trust dialog accepted at 4000ms,
    // question submitted at 7000ms, the streamed answer ends around
    // 11300ms, /exit submitted at 40000ms.
    let changes = run("claude-simple-question.ndjson");
    assert_eq!(
        states(&changes),
        vec![
            AgentState::Busy,
            AgentState::Done,
            AgentState::Busy,
            AgentState::Done,
            AgentState::Busy,
        ],
        "full sequence: {changes:#?}"
    );
    // Busy comes from the startup paint burst.
    assert!(matches!(changes[0].1.cause, Signal::OutputBurst));
    // The welcome screen settles with the cursor in the input row, so
    // the session reads ready before the question is even typed.
    assert!(
        changes[1].0 > 4000 && changes[1].0 < 7000,
        "welcome-settled Done fired at {}ms",
        changes[1].0
    );
    assert_eq!(changes[2].0, 7000);
    // Done must fire shortly after the answer stops streaming, well
    // before the next idle repaint at ~14300ms.
    assert!(
        changes[3].0 > 11300 && changes[3].0 < 12500,
        "answer-finished Done fired at {}ms",
        changes[3].0
    );
    // /exit flips back to Busy.
    assert_eq!(changes[4].0, 40000);
    assert!(matches!(changes[4].1.cause, Signal::UserInput));
}

#[test]
fn zsh_silent_command_stays_busy_until_prompt_returns() {
    // Recording: /bin/zsh with an empty enter at 1000ms (which leaves a
    // bare prompt row on screen), `sleep 2` at 2200ms and `echo after`
    // at 5500ms. The stale prompt row above the echoed command must not
    // produce an early Done while sleep runs silently.
    let changes = run("zsh-sleep.ndjson");
    assert_eq!(
        states(&changes),
        vec![
            AgentState::Busy,
            AgentState::Done,
            AgentState::Busy,
            AgentState::Done,
            AgentState::Busy,
            AgentState::Done,
        ],
        "full sequence: {changes:#?}"
    );
    // The sleep submit at 2200ms may only resolve after sleep ends at
    // ~4200ms plus the quiet window.
    assert_eq!(changes[2].0, 2200);
    assert!(
        changes[3].0 > 4400 && changes[3].0 < 5500,
        "sleep Done fired at {}ms",
        changes[3].0
    );
}

#[test]
fn claude_long_streamed_answer_holds_busy_until_the_end() {
    // Recording: claude launched inside zsh -l at 120x38, trust dialog
    // accepted, then a question whose answer streams in batches from
    // ~13s to ~34s. Streaming pauses between batches while the cursor
    // rests in the input box, so without the busy-hint hold this replay
    // flapped through 17 false Busy/Idle cycles.
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("claude-long-answer.ndjson");
    let events = read_fixture(&path).expect("fixture should parse");
    let mut detector = Detector::new(vec![Box::new(HeuristicAdapter::new(HeuristicConfig {
        cols: 120,
        rows: 38,
        ..Default::default()
    }))]);
    let changes = replay(&mut detector, &events, 100, 1000);

    // The question submit may resolve exactly once, after streaming ends.
    let during_stream: Vec<_> = changes
        .iter()
        .filter(|(t, _)| *t > 14000 && *t < 34000)
        .collect();
    assert!(
        during_stream.is_empty(),
        "state flapped during streaming: {during_stream:#?}"
    );
    let after: Vec<_> = changes
        .iter()
        .filter(|(t, c)| *t >= 34000 && *t < 60000 && c.to == AgentState::Done)
        .collect();
    assert_eq!(
        after.len(),
        1,
        "expected one Done after streaming: {changes:#?}"
    );
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

#[test]
fn claude_waiting_on_the_trust_dialog_does_not_read_as_working() {
    // Same recording as above: claude launches, paints the "do you trust
    // this folder" dialog, and sits there until the answer at 4000ms. The
    // state is Busy that whole time, because nothing has told the detector
    // otherwise, so Busy on its own must not be treated as permission to
    // hide the terminal. Collapsing here would hide the question.
    let samples = working_over_time("claude-simple-question.ndjson");
    let waiting: Vec<_> = samples
        .iter()
        .filter(|(t, _)| (2500..3900).contains(t))
        .collect();
    assert!(!waiting.is_empty(), "no samples in the dialog window");
    assert!(
        waiting.iter().all(|(_, working)| !working),
        "detector claimed the trust dialog was working: {waiting:?}"
    );
}

#[test]
fn claude_streaming_an_answer_reads_as_working() {
    // The other side of the same rule: while an answer streams, the
    // session must read as working or the window would never collapse.
    // The question goes in at 7000ms and the answer runs to about
    // 11300ms. The window collapses 1500ms after a submit and confirms
    // 500ms later, so those two moments are what decide whether the bar
    // ever appears for real work.
    let samples = working_over_time("claude-simple-question.ndjson");
    for at in [8500, 9000] {
        let working = samples.iter().find(|(t, _)| *t == at).map(|(_, w)| *w);
        assert_eq!(working, Some(true), "not working at the {at}ms check");
    }

    // Typing the question is the user's own echo, so the first second
    // after they stop deliberately does not count as the agent working.
    let streaming: Vec<_> = samples
        .iter()
        .filter(|(t, _)| (8100..11000).contains(t))
        .collect();
    assert!(
        !streaming.is_empty(),
        "no samples while the answer streamed"
    );
    assert!(
        streaming.iter().all(|(_, working)| *working),
        "detector missed work in progress: {streaming:?}"
    );
}
