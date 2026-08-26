//! Replay the recorded fixtures and pin the transition sequences the
//! detector must produce for them. The fixtures are real recordings made
//! with the `record` example, timing included, so these tests exercise
//! the same byte streams and pacing a live session produces.

use std::path::PathBuf;

use overterm_core::detect::heuristic::{HeuristicAdapter, HeuristicConfig};
use overterm_core::detect::hook::HookAdapter;
use overterm_core::detect::replay::{Event, read_fixture, replay, replay_with};
use overterm_core::detect::{AgentState, Detector, Signal, StateChange};

fn fixture(name: &str) -> Vec<Event> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    read_fixture(&path).expect("fixture should parse")
}

fn run(name: &str) -> Vec<(u64, StateChange)> {
    let events = fixture(name);
    let mut detector = Detector::new(vec![Box::new(HeuristicAdapter::new(
        HeuristicConfig::default(),
    ))]);
    replay(&mut detector, &events, 100, 1000)
}

/// The adapters a live session runs, in the order it runs them.
fn hooked_detector() -> Detector {
    Detector::new(vec![
        Box::new(HookAdapter::new()),
        Box::new(HeuristicAdapter::new(HeuristicConfig::default())),
    ])
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

/// Recording: `/bin/sh`, with the hooks installed. `claude` launched at
/// 800ms, one question asked at 10s, `/exit` at 46s, then `sleep 2` in
/// the shell claude left behind. The whole session in one file, because
/// what claude leaves behind is as much of the story as what it does.
const HOOKED: &str = "claude-hooked.ndjson";

#[test]
fn a_hooked_session_takes_its_answer_from_the_hook() {
    let mut detector = hooked_detector();
    let changes = replay(&mut detector, &fixture(HOOKED), 100, 1000);

    let (at, done) = changes
        .iter()
        .find(|(_, c)| matches!(c.cause, Signal::HookStop))
        .expect("the stop hook should have concluded the turn");
    assert_eq!(done.to, AgentState::Done);
    // Recorded at 12186ms. That timing belongs to the hook: the fallback
    // detector needs 400ms of quiet and could not have reached any
    // conclusion while the answer was still painting.
    assert!(
        (12_000..12_500).contains(at),
        "expected the hook's own timing, got {at}ms"
    );
}

#[test]
fn guesswork_stays_off_while_the_hooked_program_runs() {
    // Claude sits at its prompt for half a minute between finishing the
    // answer and being asked to exit, repainting its status area the
    // whole time. Nothing in there is a state change, and no timer gets
    // to invent one while a source that knows is attached.
    let mut detector = hooked_detector();
    let changes = replay(&mut detector, &fixture(HOOKED), 100, 1000);

    let stop = changes
        .iter()
        .position(|(_, c)| matches!(c.cause, Signal::HookStop))
        .expect("the stop hook fired");
    let (stopped_at, _) = changes[stop];
    let (next_at, next) = &changes[stop + 1];
    assert!(
        next_at - stopped_at > 30_000,
        "something concluded {}ms after the hook did: {next:?}",
        next_at - stopped_at
    );
    assert_eq!(next.cause, Signal::UserInput, "and it was the user typing");
}

#[test]
fn a_reported_turn_is_tracked_from_submit_to_stop() {
    // The submit lands after the user's own keystrokes have already
    // pushed the session to Busy, so it shows up as no transition at
    // all. The turn behind it is what makes hiding the terminal safe.
    let mut detector = hooked_detector();
    let mut running: Vec<u64> = Vec::new();
    replay_with(&mut detector, &fixture(HOOKED), 100, 1000, |t, _, d| {
        if d.turn_in_flight() {
            running.push(t);
        }
    });

    let (first, last) = (
        *running.first().expect("a turn should have been tracked"),
        *running.last().expect("a turn should have been tracked"),
    );
    assert!(first > 10_400, "turn started too early, at {first}ms");
    assert!(
        last < 12_300,
        "turn outlived the stop hook, ending {last}ms"
    );
}

#[test]
fn the_shell_claude_leaves_behind_is_still_watched() {
    // A shell outlives the programs run inside it. Claude hands the
    // alternate screen back on the way out, which is what puts the
    // fallback detector in charge again; without that, `sleep 2` here
    // would go unnoticed for as long as the hooks stayed trusted.
    let mut detector = hooked_detector();
    let changes = replay(&mut detector, &fixture(HOOKED), 100, 1000);

    let after_exit: Vec<_> = changes.iter().filter(|(t, _)| *t > 50_000).collect();
    let states: Vec<AgentState> = after_exit.iter().map(|(_, c)| c.to).collect();
    assert_eq!(
        states,
        vec![AgentState::Busy, AgentState::Done],
        "the command run after claude exited was not detected: {after_exit:#?}"
    );
    assert!(matches!(after_exit[1].1.cause, Signal::Quiescence { .. }));
}

#[test]
fn hooks_do_not_change_what_an_unhooked_session_looks_like() {
    // The fallback tier is the whole tool-agnostic pitch, and adding the
    // marker adapter must not disturb a recording that has no markers.
    for name in ["shell-ls-echo.ndjson", "claude-simple-question.ndjson"] {
        let mut hooked = hooked_detector();
        let with = replay(&mut hooked, &fixture(name), 100, 1000);
        assert_eq!(states(&with), states(&run(name)), "{name}");
    }
}
