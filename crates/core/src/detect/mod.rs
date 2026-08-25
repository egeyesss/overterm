//! Agent-state detection.
//!
//! A `Detector` owns one session's state machine and a ranked set of
//! adapters. Adapters watch the output stream and emit `Signal`s; the
//! state machine turns signals into state transitions. Precise adapters
//! (hooks, OSC sequences) outrank the heuristic adapter: once a precise
//! signal has been seen for a session, heuristic signals are ignored so
//! the quiescence timer cannot fight exact events.

pub mod heuristic;
pub mod replay;

use std::time::{Duration, Instant};

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
    /// Claude Code Stop hook fired. No adapter emits this yet.
    HookStop,
    /// Claude Code Notification hook fired. No adapter emits this yet.
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
}

/// How much trust a signal carries.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum SignalClass {
    /// Timing and pattern guesses. Ignored once a precise source exists.
    Heuristic,
    /// Exact events from hooks or shell integration.
    Precise,
    /// The user's own actions. Always trusted.
    Direct,
}

impl Signal {
    pub fn class(&self) -> SignalClass {
        match self {
            Signal::HookStop
            | Signal::HookNotification
            | Signal::OscCommandEnd { .. }
            | Signal::OscPromptStart => SignalClass::Precise,
            Signal::Bell | Signal::Quiescence { .. } | Signal::OutputBurst => {
                SignalClass::Heuristic
            }
            Signal::UserInput => SignalClass::Direct,
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

/// A source of signals watching one session's byte streams.
pub trait Adapter: Send {
    /// Consume a chunk of terminal output.
    fn feed(&mut self, bytes: &[u8], now: Instant) -> Vec<Signal>;
    /// Called periodically so time-based signals (quiescence) can fire
    /// without waiting for the next output chunk.
    fn tick(&mut self, now: Instant) -> Vec<Signal>;
    /// The terminal was resized. Adapters that model the screen need this.
    fn resize(&mut self, _cols: u16, _rows: u16) {}

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
}

impl Detector {
    pub fn new(adapters: Vec<Box<dyn Adapter>>) -> Self {
        Self {
            state: AgentState::Idle,
            adapters,
            precise_since: None,
            last_typing: None,
            awaiting_result: false,
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
    pub fn resize(&mut self, cols: u16, rows: u16) {
        for adapter in &mut self.adapters {
            adapter.resize(cols, rows);
        }
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
    }

    /// Whether any adapter can show the session is working. Used to decide
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
        self.adapters.iter().any(|a| a.is_working(now))
    }

    /// Run periodic time-based checks. Call every ~100ms.
    pub fn tick(&mut self, now: Instant) -> Vec<StateChange> {
        let mut changes = Vec::new();
        let signals: Vec<Signal> = self.adapters.iter_mut().flat_map(|a| a.tick(now)).collect();
        for signal in signals {
            changes.extend(self.apply(signal, now));
        }
        changes
    }

    /// Push one signal through the suppression rule and the state machine.
    pub fn apply(&mut self, signal: Signal, now: Instant) -> Option<StateChange> {
        match signal.class() {
            SignalClass::Precise => self.precise_since = Some(now),
            SignalClass::Heuristic if self.precise_source_active(now) => return None,
            _ => {}
        }

        let next = self.transition(&signal)?;
        let change = StateChange {
            from: self.state,
            to: next,
            cause: signal,
        };
        self.state = next;
        Some(change)
    }

    fn transition(&mut self, signal: &Signal) -> Option<AgentState> {
        use AgentState::*;
        use Signal::*;
        match (self.state, signal) {
            (Busy, UserInput) => {
                self.awaiting_result = true;
                None
            }
            (_, UserInput) => {
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
            (Busy | NeedsInput, HookStop | OscCommandEnd { .. }) => {
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
