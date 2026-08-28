//! Turning detected agent state into window behaviour.
//!
//! This is a pure function of one state transition plus the user's
//! preferences. Keeping it here rather than in the app crate means the
//! rules are unit-testable without a window, and a port to another
//! platform inherits them unchanged.

use serde::{Deserialize, Serialize};

use crate::detect::{AgentState, StateChange};

/// Something worth reconsidering the window over.
#[derive(Clone, Debug)]
pub enum ChoreoEvent {
    /// The detector concluded the session changed state.
    StateChanged(StateChange),
    /// The user sent a line to the session.
    Submitted,
}

/// How much of the terminal is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WindowMode {
    /// Compact strip: one input line and a status pill.
    Bar,
    /// The full terminal.
    #[default]
    Panel,
}

/// Ways of getting the user's attention when the agent wants them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cues {
    /// Glowing border around the window.
    pub glow: bool,
    /// Short sound.
    pub sound: bool,
}

impl Default for Cues {
    fn default() -> Self {
        // Quiet by default: the window is on screen anyway, so a glow is
        // enough. Sound is opt-in.
        Self {
            glow: true,
            sound: false,
        }
    }
}

/// Something the app should do to the window.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "action", content = "data")]
pub enum WindowAction {
    /// Shrink to the bar, but only if the agent is still working when the
    /// delay is up. Without the delay a one-second `ls` would collapse and
    /// expand the window for no reason.
    Collapse { after_ms: u64 },
    /// Grow to the full terminal without taking keyboard focus.
    Expand,
    /// Fire these attention cues.
    Attention(Cues),
    /// Drop any attention state that is still showing.
    ClearAttention,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChoreoConfig {
    /// Collapse to the bar once the user hands the session something to do.
    pub collapse_on_submit: bool,
    /// How long the agent must stay busy before collapsing.
    pub collapse_delay_ms: u64,
    /// Expand when the agent finishes or asks for something.
    pub expand_when_wanted: bool,
    /// How long a collapsed session may sit still with nothing concluded
    /// before the terminal comes back. A session that stops working
    /// without reaching an end is usually sitting on a question the bar
    /// cannot show, so the full screen has to return for the user to
    /// answer it.
    pub reveal_when_stalled_ms: u64,
    pub cues: Cues,
}

impl Default for ChoreoConfig {
    fn default() -> Self {
        Self {
            collapse_on_submit: true,
            collapse_delay_ms: 1500,
            expand_when_wanted: true,
            reveal_when_stalled_ms: 2000,
            cues: Cues::default(),
        }
    }
}

/// What else is true when an event arrives, beyond the event itself.
///
/// One window can hold several sessions, so what it should do about one
/// of them depends on what the others are doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Context {
    /// The user has handed *this* session something to do since it last
    /// reached a conclusion.
    ///
    /// Things happen in a terminal that nobody asked for, and those are
    /// not worth interrupting anyone over. The first thing Claude Code
    /// does in a new folder is ask whether it is trusted, before the user
    /// has typed anything at all.
    pub user_asked: bool,

    /// Some *other* session is waiting on the user right now.
    ///
    /// The window is shared, so hiding it for one session takes it away
    /// from every other one. A session that has been left sitting on a
    /// question is the case that matters: collapsing on top of it hides
    /// the thing somebody has to answer, and nothing would bring it back
    /// because that session already reached its conclusion.
    pub others_want_user: bool,
}

/// Decide what the window should do about one event.
pub fn plan(event: &ChoreoEvent, cfg: &ChoreoConfig, ctx: Context) -> Vec<WindowAction> {
    match event {
        // Handing the session a job is the one thing that means "I am
        // done with the terminal for now". Busy on its own does not:
        // startup banners, repaints and the echo of the user typing all
        // reach Busy, and hiding the terminal for any of those takes the
        // screen away from someone who is still using it.
        ChoreoEvent::Submitted => {
            let mut actions = Vec::new();
            // Only this session's cue is spent. Another session still
            // waiting is still waiting.
            if !ctx.others_want_user {
                actions.push(WindowAction::ClearAttention);
            }
            if cfg.collapse_on_submit && !ctx.others_want_user {
                actions.push(WindowAction::Collapse {
                    after_ms: cfg.collapse_delay_ms,
                });
            }
            actions
        }
        ChoreoEvent::StateChanged(change) => match change.to {
            // Work is running. The cue from whatever finished last is
            // stale now, but the window stays as the user left it.
            AgentState::Busy => clear_unless_others_waiting(ctx),
            // The agent wants the user. Show the terminal and say so.
            AgentState::Done | AgentState::NeedsInput => {
                let mut actions = Vec::new();
                if cfg.expand_when_wanted {
                    actions.push(WindowAction::Expand);
                }
                // The terminal still comes back either way: an unasked-for
                // question is exactly the thing somebody has to be able to
                // see and answer. It just does it quietly.
                if ctx.user_asked {
                    actions.push(WindowAction::Attention(cfg.cues));
                }
                actions
            }
            // Settling down after output nobody asked for. Nothing
            // happened worth interrupting anyone over.
            AgentState::Idle => clear_unless_others_waiting(ctx),
        },
    }
}

fn clear_unless_others_waiting(ctx: Context) -> Vec<WindowAction> {
    if ctx.others_want_user {
        Vec::new()
    } else {
        vec![WindowAction::ClearAttention]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Signal;

    /// One session, the user asked for this, nothing else waiting.
    fn asked() -> Context {
        Context {
            user_asked: true,
            others_want_user: false,
        }
    }

    fn changed(from: AgentState, to: AgentState) -> ChoreoEvent {
        ChoreoEvent::StateChanged(StateChange {
            from,
            to,
            cause: Signal::UserInput,
        })
    }

    #[test]
    fn submitting_collapses_and_clears_the_last_cue() {
        let actions = plan(&ChoreoEvent::Submitted, &ChoreoConfig::default(), asked());
        assert_eq!(
            actions,
            vec![
                WindowAction::ClearAttention,
                WindowAction::Collapse { after_ms: 1500 },
            ]
        );
    }

    #[test]
    fn going_busy_on_its_own_never_collapses() {
        // Output bursts reach Busy for reasons the user did not ask for:
        // a shell banner at launch, a repaint, or the terminal echoing
        // what someone is in the middle of typing. Collapsing on any of
        // those hides a terminal that is still in use.
        let actions = plan(
            &changed(AgentState::Idle, AgentState::Busy),
            &ChoreoConfig::default(),
            asked(),
        );
        assert_eq!(actions, vec![WindowAction::ClearAttention]);
    }

    #[test]
    fn finishing_expands_and_fires_cues() {
        let cfg = ChoreoConfig::default();
        let actions = plan(&changed(AgentState::Busy, AgentState::Done), &cfg, asked());
        assert_eq!(
            actions,
            vec![WindowAction::Expand, WindowAction::Attention(cfg.cues)]
        );
    }

    #[test]
    fn needing_input_behaves_like_finishing() {
        let cfg = ChoreoConfig::default();
        assert_eq!(
            plan(
                &changed(AgentState::Busy, AgentState::NeedsInput),
                &cfg,
                asked()
            ),
            plan(&changed(AgentState::Busy, AgentState::Done), &cfg, asked()),
        );
    }

    #[test]
    fn idle_never_interrupts() {
        let actions = plan(
            &changed(AgentState::Busy, AgentState::Idle),
            &ChoreoConfig::default(),
            asked(),
        );
        assert_eq!(actions, vec![WindowAction::ClearAttention]);
        assert!(!actions.contains(&WindowAction::Expand));
    }

    #[test]
    fn collapsing_can_be_turned_off() {
        let cfg = ChoreoConfig {
            collapse_on_submit: false,
            ..ChoreoConfig::default()
        };
        assert_eq!(
            plan(&ChoreoEvent::Submitted, &cfg, asked()),
            vec![WindowAction::ClearAttention]
        );
    }

    #[test]
    fn expanding_can_be_turned_off_while_cues_still_fire() {
        let cfg = ChoreoConfig {
            expand_when_wanted: false,
            ..ChoreoConfig::default()
        };
        assert_eq!(
            plan(&changed(AgentState::Busy, AgentState::Done), &cfg, asked()),
            vec![WindowAction::Attention(cfg.cues)]
        );
    }

    /// Another session in the same window is sitting on a question.
    fn others_waiting() -> Context {
        Context {
            user_asked: true,
            others_want_user: true,
        }
    }

    #[test]
    fn a_session_left_on_a_question_keeps_the_terminal_open() {
        // The window is shared. Handing a job to one session must not
        // hide the terminal out from under another one that is waiting,
        // because nothing would bring it back: that session already
        // reached its conclusion and will not conclude again.
        let cfg = ChoreoConfig::default();
        assert!(cfg.collapse_on_submit, "the default this is guarding");
        let actions = plan(&ChoreoEvent::Submitted, &cfg, others_waiting());
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, WindowAction::Collapse { .. })),
            "collapsed on top of a session waiting for an answer: {actions:?}"
        );
    }

    #[test]
    fn one_session_going_busy_does_not_clear_anothers_cue() {
        let cfg = ChoreoConfig::default();
        let actions = plan(
            &changed(AgentState::Idle, AgentState::Busy),
            &cfg,
            others_waiting(),
        );
        assert!(
            !actions.contains(&WindowAction::ClearAttention),
            "a session still waiting is still waiting: {actions:?}"
        );
    }

    #[test]
    fn with_nothing_else_waiting_the_cue_is_cleared_as_before() {
        let cfg = ChoreoConfig::default();
        let actions = plan(&changed(AgentState::Idle, AgentState::Busy), &cfg, asked());
        assert_eq!(actions, vec![WindowAction::ClearAttention]);
    }

    #[test]
    fn work_nobody_asked_for_finishes_quietly() {
        // What this is really about: Claude Code asks whether a new
        // folder is trusted before the user has said anything, and the
        // Done that lands once it is answered used to glow, chime and
        // notify for work nobody requested.
        let cfg = ChoreoConfig::default();
        let actions = plan(
            &changed(AgentState::Busy, AgentState::Done),
            &cfg,
            Context::default(),
        );
        assert_eq!(
            actions,
            vec![WindowAction::Expand],
            "the terminal still comes back, it just does not shout"
        );
    }

    #[test]
    fn an_unasked_question_still_brings_the_terminal_back() {
        // The case that must not regress: something is on screen waiting
        // for an answer, and the user cannot give one if the window
        // stays collapsed.
        let cfg = ChoreoConfig::default();
        let actions = plan(
            &changed(AgentState::Busy, AgentState::NeedsInput),
            &cfg,
            Context::default(),
        );
        assert!(actions.contains(&WindowAction::Expand));
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, WindowAction::Attention(_)))
        );
    }
}
