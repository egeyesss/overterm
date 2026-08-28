//! Agent-state detection.
//!
//! A `Detector` owns one session's state machine and a ranked set of
//! adapters. Adapters watch the output stream and emit `Signal`s; the
//! state machine turns signals into state transitions. Precise adapters
//! (hooks, OSC sequences) outrank the heuristic adapter: once a precise
//! signal has been seen for a session, heuristic signals are ignored so
//! the quiescence timer cannot fight exact events.

pub mod heuristic;
pub mod hook;
pub mod replay;

use std::time::{Duration, Instant};

use regex::Regex;

/// How long after a keystroke the session still counts as the user's,
/// not the agent's. Long enough to cover the gap between letters, short
/// enough that it expires while a submitted job runs.
const TYPING_GRACE: Duration = Duration::from_millis(1000);

/// How long a precise source is trusted after its last signal.
///
/// Suppression is meant to last while a hooked program is running, and
/// whoever installed the hooks says when that ends. This is only a way
/// back from an end that never arrives: the program was killed, or its
/// hooks were removed part way through. Without it, one precise signal
/// would leave the fallback detector switched off for the rest of the
/// shell's life, and a session is a shell, not one run of one program.
const PRECISE_TRUST: Duration = Duration::from_secs(600);

/// How long the screen must look finished, while a precise source still
/// says a turn is running, before that turn is written off.
///
/// Long enough that a program which pauses mid-turn is not cut short,
/// short enough that an interrupted turn does not sit there.
const STALE_TURN: Duration = Duration::from_secs(3);

/// What the agent inside the terminal appears to be doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentState {
    /// Nothing requested yet, or the last activity was not user-initiated.
    Idle,
    /// Working: output is flowing after the user submitted something.
    Busy,
    /// The agent appears to be waiting on the user.
    NeedsInput,
    /// The agent finished work the user asked for.
    Done,
}

/// A single observation about the session.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Signal {
    /// The user handed a hooked program something to do.
    HookSubmit,
    /// A hooked program finished the work it was given.
    HookStop,
    /// A hooked program's work ended on an error rather than an answer.
    /// Still finished: without this the session sits busy forever.
    HookStopFailure,
    /// A hooked program is waiting on the user.
    HookNotification,
    /// OSC 133;D command-finished sequence from shell integration.
    OscCommandEnd { exit: Option<i32> },
    /// OSC 133;A prompt-start sequence from shell integration.
    OscPromptStart,
    /// Bell character in output, outside any escape sequence.
    Bell,
    /// Output went quiet after a burst and the tail looks like a prompt.
    Quiescence { quiet_ms: u64 },
    /// Output started flowing heavily.
    OutputBurst,
    /// The user submitted input (pressed enter).
    UserInput,
    /// A full-screen program gave the terminal back. Says nothing about
    /// the agent; it means whoever was reporting precisely is gone.
    FullScreenExited,
}

/// How much trust a signal carries.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum SignalClass {
    /// Timing and pattern guesses. Ignored once a precise source exists.
    Heuristic,
    /// Exact events from hooks or shell integration.
    Precise,
    /// Things that are simply so: the user's own actions, and what the
    /// terminal itself reports. Always trusted.
    Direct,
}

impl Signal {
    pub fn class(&self) -> SignalClass {
        match self {
            Signal::HookSubmit
            | Signal::HookStop
            | Signal::HookStopFailure
            | Signal::HookNotification
            | Signal::OscCommandEnd { .. }
            | Signal::OscPromptStart => SignalClass::Precise,
            Signal::Bell | Signal::Quiescence { .. } | Signal::OutputBurst => {
                SignalClass::Heuristic
            }
            Signal::UserInput | Signal::FullScreenExited => SignalClass::Direct,
        }
    }
}

/// Whether a chunk written to the session looks like a person typing.
///
/// Not everything the frontend sends is a keystroke. Claude polls the
/// terminal for its cursor position several times a second, and xterm.js
/// answers every one of those, so replies like `ESC[?37;3R` arrive here as
/// input. Counting them would make every idle session look permanently in
/// use, so anything starting an escape sequence is left out.
fn looks_like_typing(bytes: &[u8]) -> bool {
    if bytes.first() == Some(&0x1b) {
        return false;
    }
    bytes.iter().any(|&b| b >= 0x20 && b != 0x7f)
}

/// What to look for on screen while a particular program is running.
///
/// Reading the screen is a guess, and the guess is only as good as what
/// it is told to look for. Claude Code keeps "esc to interrupt" up for
/// the whole time it works; another agent says something else, or says
/// nothing. A field left as `None` means go back to the built-in
/// default, so leaving a program restores what a plain shell needs.
#[derive(Clone, Debug, Default)]
pub struct Profile {
    /// Marks the screen as still working even though output paused.
    pub busy_pattern: Option<Regex>,
    /// Decides whether a row looks like a prompt.
    pub prompt_pattern: Option<Regex>,
}

/// A source of signals watching one session's byte streams.
pub trait Adapter: Send {
    /// Consume a chunk of terminal output.
    fn feed(&mut self, bytes: &[u8], now: Instant) -> Vec<Signal>;
    /// Called periodically so time-based signals (quiescence) can fire
    /// without waiting for the next output chunk.
    fn tick(&mut self, now: Instant) -> Vec<Signal>;
    /// The terminal was resized. Adapters that model the screen need this.
    fn resize(&mut self, _cols: u16, _rows: u16) {}
    /// A different program owns the terminal now, so what is worth
    /// looking for on screen has changed. Adapters that do not read the
    /// screen have no use for this.
    fn set_profile(&mut self, _profile: &Profile) {}

    /// Whether there is positive evidence the session is doing work right
    /// now, as opposed to sitting on a question.
    ///
    /// Busy alone cannot answer this. A program that printed a dialog and
    /// is waiting for an answer looks exactly like one that is grinding
    /// away silently, and both hold the state at Busy. Hiding the terminal
    /// in the first case hides the thing the user has to answer, so the
    /// window choreography asks for evidence before collapsing rather than
    /// treating Busy as permission.
    fn is_working(&self, _now: Instant) -> bool {
        false
    }
}

/// One applied transition, with the signal that caused it.
#[derive(Clone, Debug)]
pub struct StateChange {
    pub from: AgentState,
    pub to: AgentState,
    pub cause: Signal,
}

pub struct Detector {
    state: AgentState,
    adapters: Vec<Box<dyn Adapter>>,
    /// When a precise signal last arrived. While a precise source is
    /// attached, heuristic conclusions are ignored so the quiescence
    /// timer cannot fight exact events.
    precise_since: Option<Instant>,
    /// When the user last typed something. The window must not collapse
    /// out from under someone in the middle of writing a message.
    last_typing: Option<Instant>,
    /// Whether the user has submitted input since the last Done or Idle.
    /// Quiescence only counts as Done for work the user asked for, so a
    /// fresh shell printing its banner settles into Idle rather than
    /// announcing a finished task at launch.
    awaiting_result: bool,
    /// When a precise source said the work it is doing started, if it is
    /// still doing it. Kept here rather than in the adapter that reports
    /// it because every signal passes through here, so there is one copy
    /// of the fact and no way for two of them to disagree.
    turn_since: Option<Instant>,
    /// When the screen first looked finished while a precise source
    /// still said work was running.
    quiet_while_running: Option<Instant>,
    /// A precise submit has arrived that the window has not been told
    /// about yet.
    submit_pending: bool,
}

impl Detector {
    pub fn new(adapters: Vec<Box<dyn Adapter>>) -> Self {
        Self {
            state: AgentState::Idle,
            adapters,
            precise_since: None,
            last_typing: None,
            awaiting_result: false,
            turn_since: None,
            quiet_while_running: None,
            submit_pending: false,
        }
    }

    pub fn state(&self) -> AgentState {
        self.state
    }

    /// Feed a chunk of terminal output through every adapter.
    pub fn feed_output(&mut self, bytes: &[u8], now: Instant) -> Vec<StateChange> {
        let mut changes = Vec::new();
        let signals: Vec<Signal> = self
            .adapters
            .iter_mut()
            .flat_map(|a| a.feed(bytes, now))
            .collect();
        for signal in signals {
            changes.extend(self.apply(signal, now));
        }
        changes
    }

    /// Feed user keystrokes. Enter means the user submitted something.
    pub fn feed_input(&mut self, bytes: &[u8], now: Instant) -> Vec<StateChange> {
        if looks_like_typing(bytes) {
            self.last_typing = Some(now);
        }
        if bytes.contains(&b'\r') || bytes.contains(&b'\n') {
            self.apply(Signal::UserInput, now).into_iter().collect()
        } else {
            Vec::new()
        }
    }

    /// Forward a terminal resize to the adapters.
    /// Tell every adapter which program owns the terminal now.
    pub fn set_profile(&mut self, profile: &Profile) {
        for adapter in &mut self.adapters {
            adapter.set_profile(profile);
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        for adapter in &mut self.adapters {
            adapter.resize(cols, rows);
        }
    }

    /// Whether a program that reports its own events owns this terminal.
    ///
    /// Unlike `precise_source_active` this has no timeout: the question is
    /// "what is running", not "should the guesses be suppressed". Only a
    /// program we installed hooks into writes those markers, so seeing
    /// one identifies it exactly, which reading a process name cannot
    /// always do.
    pub fn has_precise_source(&self) -> bool {
        self.precise_since.is_some()
    }

    /// Whether a precise source is currently answering for this session.
    pub fn precise_source_active(&self, now: Instant) -> bool {
        self.precise_since
            .is_some_and(|t| now.duration_since(t) < PRECISE_TRUST)
    }

    /// Let go of the precise source, so the fallback detector takes over
    /// again. Called when the program that was reporting exits: the
    /// session outlives it, and whatever is run next still needs
    /// detecting.
    pub fn detach_precise_source(&mut self) {
        self.precise_since = None;
        self.end_turn();
    }

    /// Whether a precise source says work is running right now.
    pub fn turn_in_flight(&self) -> bool {
        self.turn_since.is_some()
    }

    /// Whether a precise submit has arrived since this was last asked.
    ///
    /// The window collapses when the user hands the session a job, and a
    /// submit landing while the session already reads as busy produces no
    /// transition for that to hang off, so it is reported separately.
    pub fn take_submit(&mut self) -> bool {
        std::mem::take(&mut self.submit_pending)
    }

    fn end_turn(&mut self) {
        self.turn_since = None;
        self.quiet_while_running = None;
    }

    /// Whether anything can show the session is working. Used to decide
    /// whether collapsing the window is safe.
    pub fn is_working(&self, now: Instant) -> bool {
        // Someone typing into the session repaints the screen with their
        // own echo, which looks exactly like the agent producing output.
        // While that is happening the terminal belongs to the user.
        if self
            .last_typing
            .is_some_and(|t| now.duration_since(t) < TYPING_GRACE)
        {
            return false;
        }
        // A precise source saying its work is still running outranks the
        // screen, which shows nothing at all during a silent tool call.
        if self.turn_in_flight() {
            return true;
        }
        self.adapters.iter().any(|a| a.is_working(now))
    }

    /// Run periodic time-based checks. Call every ~100ms.
    pub fn tick(&mut self, now: Instant) -> Vec<StateChange> {
        let mut changes = Vec::new();
        let signals: Vec<Signal> = self.adapters.iter_mut().flat_map(|a| a.tick(now)).collect();
        for signal in signals {
            changes.extend(self.apply(signal, now));
        }
        changes.extend(self.write_off_stale_turn(now));
        changes
    }

    /// Push one signal through the suppression rule and the state machine.
    pub fn apply(&mut self, signal: Signal, now: Instant) -> Option<StateChange> {
        // Not a reading of the agent at all: whoever was reporting
        // precisely has gone, and the session outlives them.
        if signal == Signal::FullScreenExited {
            self.detach_precise_source();
            return None;
        }

        match signal.class() {
            SignalClass::Precise => self.precise_since = Some(now),
            SignalClass::Heuristic if self.precise_source_active(now) => {
                self.watch_screen_during_turn(&signal, now);
                return None;
            }
            _ => {}
        }

        match signal {
            Signal::HookSubmit => {
                self.turn_since = Some(now);
                self.quiet_while_running = None;
                self.submit_pending = true;
            }
            Signal::HookStop | Signal::HookStopFailure => self.end_turn(),
            _ => {}
        }

        self.conclude(signal)
    }

    /// Run one signal through the state machine, with no suppression and
    /// no bookkeeping.
    fn conclude(&mut self, signal: Signal) -> Option<StateChange> {
        let next = self.transition(&signal)?;
        let change = StateChange {
            from: self.state,
            to: next,
            cause: signal,
        };
        self.state = next;
        Some(change)
    }

    /// Keep track of what the screen says while a precise source has a
    /// turn running. Its conclusions are still ignored; they are only
    /// worth timing.
    fn watch_screen_during_turn(&mut self, signal: &Signal, now: Instant) {
        if !self.turn_in_flight() {
            return;
        }
        match signal {
            Signal::Quiescence { .. } => {
                self.quiet_while_running.get_or_insert(now);
            }
            Signal::OutputBurst => self.quiet_while_running = None,
            _ => {}
        }
    }

    /// Finish a turn that ended without anything saying so.
    ///
    /// A turn can end in silence: the user interrupts it, or the program
    /// running it is killed. Neither fires a hook, so a turn believed to
    /// be running would hold the session busy for as long as the precise
    /// source is trusted, and hide the terminal for all of it.
    ///
    /// The screen settles it. The fallback detector only calls a session
    /// quiet once the working hint is off screen and output has stopped,
    /// which is what the end of a turn looks like, so a quiet reading
    /// that persists through a turn means the turn is over.
    fn write_off_stale_turn(&mut self, now: Instant) -> Vec<StateChange> {
        let Some(since) = self.quiet_while_running else {
            return Vec::new();
        };
        if now.duration_since(since) < STALE_TURN {
            return Vec::new();
        }
        self.end_turn();
        let quiet_ms = now.duration_since(since).as_millis() as u64;
        self.conclude(Signal::Quiescence { quiet_ms })
            .into_iter()
            .collect()
    }

    fn transition(&mut self, signal: &Signal) -> Option<AgentState> {
        use AgentState::*;
        use Signal::*;
        match (self.state, signal) {
            (Busy, UserInput | HookSubmit) => {
                self.awaiting_result = true;
                None
            }
            (_, UserInput | HookSubmit) => {
                self.awaiting_result = true;
                Some(Busy)
            }
            (Idle | Done | NeedsInput, OutputBurst) => Some(Busy),
            (Busy | NeedsInput, Quiescence { .. }) => {
                if self.awaiting_result {
                    self.awaiting_result = false;
                    Some(Done)
                } else {
                    Some(Idle)
                }
            }
            (Busy | NeedsInput, HookStop | HookStopFailure | OscCommandEnd { .. }) => {
                self.awaiting_result = false;
                Some(Done)
            }
            (Busy, Bell | HookNotification) => Some(NeedsInput),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detector() -> Detector {
        Detector::new(Vec::new())
    }

    /// Most of these tests do not care when a signal arrives.
    trait ApplyNow {
        fn apply_now(&mut self, signal: Signal) -> Option<StateChange>;
    }

    impl ApplyNow for Detector {
        fn apply_now(&mut self, signal: Signal) -> Option<StateChange> {
            self.apply(signal, Instant::now())
        }
    }

    fn quiet() -> Signal {
        Signal::Quiescence { quiet_ms: 400 }
    }

    #[test]
    fn submit_then_quiet_is_done() {
        let mut d = detector();
        assert_eq!(d.state(), AgentState::Idle);
        let c = d.apply_now(Signal::UserInput).unwrap();
        assert_eq!(c.to, AgentState::Busy);
        let c = d.apply_now(quiet()).unwrap();
        assert_eq!(c.to, AgentState::Done);
    }

    #[test]
    fn startup_burst_settles_to_idle_without_user_input() {
        let mut d = detector();
        assert_eq!(
            d.apply_now(Signal::OutputBurst).unwrap().to,
            AgentState::Busy
        );
        assert_eq!(d.apply_now(quiet()).unwrap().to, AgentState::Idle);
    }

    #[test]
    fn bell_while_busy_means_needs_input() {
        let mut d = detector();
        d.apply_now(Signal::UserInput);
        assert_eq!(
            d.apply_now(Signal::Bell).unwrap().to,
            AgentState::NeedsInput
        );
        assert_eq!(d.apply_now(Signal::UserInput).unwrap().to, AgentState::Busy);
    }

    #[test]
    fn quiescence_resolves_needs_input() {
        // A bell mid-run flags attention; if the prompt then returns with
        // no user action, the work still finished.
        let mut d = detector();
        d.apply_now(Signal::UserInput);
        d.apply_now(Signal::Bell);
        assert_eq!(d.state(), AgentState::NeedsInput);
        assert_eq!(d.apply_now(quiet()).unwrap().to, AgentState::Done);
    }

    #[test]
    fn bell_while_idle_is_ignored() {
        let mut d = detector();
        assert!(d.apply_now(Signal::Bell).is_none());
        assert_eq!(d.state(), AgentState::Idle);
    }

    #[test]
    fn typing_without_enter_is_not_a_submit() {
        let mut d = detector();
        let now = Instant::now();
        assert!(d.feed_input(b"claud", now).is_empty());
        assert_eq!(d.state(), AgentState::Idle);
        assert_eq!(d.feed_input(b"e\r", now).len(), 1);
        assert_eq!(d.state(), AgentState::Busy);
    }

    #[test]
    fn typing_outranks_the_agent_looking_busy() {
        // Someone composing a message repaints the screen with their own
        // echo, which is indistinguishable from the agent producing
        // output. While they are typing the terminal is theirs, even if
        // the agent also looks busy.
        use crate::detect::heuristic::{HeuristicAdapter, HeuristicConfig};
        let base = Instant::now();
        let mut d = Detector::new(vec![Box::new(HeuristicAdapter::new(
            HeuristicConfig::default(),
        ))]);

        let mut screen = String::from("working");
        screen.push_str(&"\r\n".repeat(28));
        screen.push_str("(esc to interrupt)");
        d.feed_output(screen.as_bytes(), base);
        assert!(d.is_working(base + Duration::from_millis(100)));

        d.feed_input(b"talk to me about", base + Duration::from_millis(200));
        assert!(!d.is_working(base + Duration::from_millis(700)));

        // Once they stop, the agent's own evidence counts again, so a
        // submitted job can still collapse the window while it runs.
        assert!(d.is_working(base + Duration::from_millis(1500)));
    }

    #[test]
    fn cursor_position_replies_are_not_typing() {
        // xterm.js answers the cursor-position queries claude sends
        // several times a second, and those replies come back through the
        // same path as keystrokes. Treating them as typing would keep the
        // window from ever collapsing.
        let base = Instant::now();
        let mut d = detector();
        d.feed_input(b"\x1b[?37;3R", base);
        assert!(d.last_typing.is_none());
    }

    #[test]
    fn precise_signal_suppresses_later_heuristics() {
        let mut d = detector();
        d.apply_now(Signal::UserInput);
        assert_eq!(d.apply_now(Signal::HookStop).unwrap().to, AgentState::Done);
        // A quiescence guess arriving later must do nothing.
        d.apply_now(Signal::OutputBurst);
        assert_eq!(d.state(), AgentState::Done);
        assert!(d.apply_now(quiet()).is_none());
    }

    #[test]
    fn a_precise_source_can_be_let_go_of() {
        // A session is a shell, not one run of one program. Run a program
        // that reports precisely, quit it, and run something ordinary
        // afterwards: the fallback detector has to come back or nothing
        // is ever detected again for the life of that shell.
        let base = Instant::now();
        let mut d = detector();
        d.apply(Signal::UserInput, base);
        assert_eq!(
            d.apply(Signal::HookStop, base).unwrap().to,
            AgentState::Done
        );
        assert!(d.apply(quiet(), base).is_none());

        d.detach_precise_source();
        assert!(!d.precise_source_active(base));
        d.apply(Signal::UserInput, base);
        assert_eq!(d.apply(quiet(), base).unwrap().to, AgentState::Done);
    }

    #[test]
    fn a_precise_source_that_never_says_goodbye_times_out() {
        // The program reporting can be killed, or have its hooks removed
        // part way through, and then nothing ever says it is gone. Trust
        // has to lapse on its own or the session is stuck.
        let base = Instant::now();
        let mut d = detector();
        d.apply(Signal::HookStop, base);
        assert!(d.precise_source_active(base + Duration::from_secs(60)));
        assert!(!d.precise_source_active(base + PRECISE_TRUST));

        d.apply(Signal::UserInput, base + PRECISE_TRUST);
        assert_eq!(
            d.apply(quiet(), base + PRECISE_TRUST).unwrap().to,
            AgentState::Done
        );
    }

    #[test]
    fn a_precise_source_can_report_work_the_screen_cannot_show() {
        // Settles how the two tiers meet: a precise source does not skip
        // the "is it safe to hide the terminal" question, it answers it.
        // A turn known to be in flight counts as working even when the
        // screen has gone completely still, which is what the heuristic
        // alone would read as waiting on the user.
        struct TurnInFlight;
        impl Adapter for TurnInFlight {
            fn feed(&mut self, _: &[u8], _: Instant) -> Vec<Signal> {
                Vec::new()
            }
            fn tick(&mut self, _: Instant) -> Vec<Signal> {
                Vec::new()
            }
            fn is_working(&self, _: Instant) -> bool {
                true
            }
        }

        let base = Instant::now();
        let mut d = Detector::new(vec![Box::new(TurnInFlight)]);
        assert!(d.is_working(base));

        // The user still outranks it. Someone typing owns the terminal
        // whatever the agent is doing.
        d.feed_input(b"hold on", base);
        assert!(!d.is_working(base + Duration::from_millis(500)));
        assert!(d.is_working(base + Duration::from_millis(1500)));
    }

    #[test]
    fn hook_stop_resolves_needs_input() {
        let mut d = detector();
        d.apply_now(Signal::UserInput);
        d.apply_now(Signal::HookNotification);
        assert_eq!(d.state(), AgentState::NeedsInput);
        assert_eq!(d.apply_now(Signal::HookStop).unwrap().to, AgentState::Done);
    }

    #[test]
    fn a_turn_that_errors_out_still_finishes() {
        // A turn dying on a rate limit reports differently from one that
        // answered, but the session is just as finished either way. Left
        // busy, the window would hide the terminal until trust lapsed.
        let mut d = detector();
        d.apply_now(Signal::HookSubmit);
        assert_eq!(d.state(), AgentState::Busy);
        assert_eq!(
            d.apply_now(Signal::HookStopFailure).unwrap().to,
            AgentState::Done
        );
        assert!(!d.turn_in_flight());
    }

    #[test]
    fn a_precise_submit_is_reported_even_with_no_transition_to_show() {
        // The user's keystrokes reach Busy before the hook does, so the
        // submit itself changes nothing. The window still has to hear
        // about it, or handing over a job never collapses the terminal.
        let base = Instant::now();
        let mut d = detector();
        d.feed_input(b"do the thing\r", base);
        assert_eq!(d.state(), AgentState::Busy);
        assert!(!d.take_submit());

        assert!(d.apply(Signal::HookSubmit, base).is_none());
        assert!(d.take_submit(), "the submit must be reported");
        assert!(!d.take_submit(), "and only once");
    }

    #[test]
    fn work_a_precise_source_reports_counts_as_working() {
        // A tool call producing no output looks identical to a program
        // sitting on a question, and the screen cannot tell them apart.
        // The source running the turn can.
        let base = Instant::now();
        let mut d = detector();
        assert!(!d.is_working(base));
        d.apply(Signal::HookSubmit, base);
        assert!(d.is_working(base));
        d.apply(Signal::HookStop, base);
        assert!(!d.is_working(base));
    }

    #[test]
    fn a_turn_that_ends_in_silence_is_written_off() {
        // Interrupting a turn fires no hook at all, so nothing says the
        // work stopped. The screen going quiet and staying quiet has to
        // be enough, or the session is stuck busy until trust lapses.
        let base = Instant::now();
        let mut d = detector();
        d.apply(Signal::HookSubmit, base);
        assert_eq!(d.state(), AgentState::Busy);

        // The fallback detector reaches a conclusion, and is ignored.
        assert!(d.apply(quiet(), base).is_none());
        assert_eq!(d.state(), AgentState::Busy);
        assert!(d.tick(base + Duration::from_secs(1)).is_empty());
        assert!(d.turn_in_flight());

        let changes = d.tick(base + STALE_TURN);
        assert_eq!(changes.len(), 1, "expected one conclusion: {changes:#?}");
        assert_eq!(changes[0].to, AgentState::Done);
        assert!(!d.turn_in_flight());
    }

    #[test]
    fn a_turn_that_goes_quiet_and_starts_up_again_is_left_alone() {
        // Programs pause mid-turn. A quiet moment followed by more work
        // is not the end of anything, and cutting it short would report
        // done while the agent is still going.
        let base = Instant::now();
        let mut d = detector();
        d.apply(Signal::HookSubmit, base);
        d.apply(quiet(), base);
        d.apply(Signal::OutputBurst, base + Duration::from_secs(1));

        assert!(d.tick(base + STALE_TURN).is_empty());
        assert_eq!(d.state(), AgentState::Busy);
        assert!(d.turn_in_flight());
    }

    #[test]
    fn the_screen_only_writes_off_a_turn_that_is_running() {
        // With no turn in flight there is nothing to write off, and a
        // suppressed guess must stay suppressed.
        let base = Instant::now();
        let mut d = detector();
        d.apply(Signal::UserInput, base);
        d.apply(Signal::HookStop, base);
        assert_eq!(d.state(), AgentState::Done);

        d.apply(Signal::OutputBurst, base);
        assert!(d.apply(quiet(), base).is_none());
        assert!(d.tick(base + STALE_TURN).is_empty());
        assert_eq!(d.state(), AgentState::Done);
    }

    #[test]
    fn a_full_screen_program_leaving_hands_detection_back() {
        // A shell outlives the programs run inside it. Claude takes the
        // alternate screen on the way in and gives it back on the way
        // out, and that is the moment the fallback detector has to work
        // again: nothing else is going to say so.
        let base = Instant::now();
        let mut d = detector();
        d.apply(Signal::HookSubmit, base);
        d.apply(Signal::HookStop, base);
        assert!(d.precise_source_active(base));
        assert!(d.apply(quiet(), base).is_none());

        assert!(d.apply(Signal::FullScreenExited, base).is_none());
        assert!(!d.precise_source_active(base));
        assert!(!d.turn_in_flight());

        d.apply(Signal::UserInput, base);
        assert_eq!(d.apply(quiet(), base).unwrap().to, AgentState::Done);
    }

    #[test]
    fn a_full_screen_program_killed_mid_turn_does_not_stay_working() {
        let base = Instant::now();
        let mut d = detector();
        d.apply(Signal::HookSubmit, base);
        assert!(d.is_working(base));
        d.apply(Signal::FullScreenExited, base);
        assert!(!d.is_working(base));
    }

    #[test]
    fn repeat_work_cycles_busy_done() {
        let mut d = detector();
        d.apply_now(Signal::UserInput);
        d.apply_now(quiet());
        assert_eq!(d.state(), AgentState::Done);
        d.apply_now(Signal::UserInput);
        assert_eq!(d.state(), AgentState::Busy);
        d.apply_now(quiet());
        assert_eq!(d.state(), AgentState::Done);
    }
}
