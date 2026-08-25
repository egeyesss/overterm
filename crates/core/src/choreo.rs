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
    /// Native notification, for when the window is hidden entirely.
    pub notify: bool,
}

impl Default for Cues {
    fn default() -> Self {
        // Quiet by default: the window is on screen anyway, so a glow is
        // enough. Sound and notifications are opt-in.
        Self {
            glow: true,
            sound: false,
            notify: false,
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

#[derive(Clone, Copy, Debug)]
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

/// Decide what the window should do about one event.
pub fn plan(event: &ChoreoEvent, cfg: &ChoreoConfig) -> Vec<WindowAction> {
    match event {
        // Handing the session a job is the one thing that means "I am
        // done with the terminal for now". Busy on its own does not:
        // startup banners, repaints and the echo of the user typing all
        // reach Busy, and hiding the terminal for any of those takes the
        // screen away from someone who is still using it.
        ChoreoEvent::Submitted => {
            let mut actions = vec![WindowAction::ClearAttention];
            if cfg.collapse_on_submit {
                actions.push(WindowAction::Collapse {
                    after_ms: cfg.collapse_delay_ms,
                });
            }
            actions
        }
        ChoreoEvent::StateChanged(change) => match change.to {
            // Work is running. The cue from whatever finished last is
            // stale now, but the window stays as the user left it.
            AgentState::Busy => vec![WindowAction::ClearAttention],
            // The agent wants the user. Show the terminal and say so.
            AgentState::Done | AgentState::NeedsInput => {
                let mut actions = Vec::new();
                if cfg.expand_when_wanted {
                    actions.push(WindowAction::Expand);
                }
                actions.push(WindowAction::Attention(cfg.cues));
                actions
            }
            // Settling down after output nobody asked for. Nothing
            // happened worth interrupting anyone over.
            AgentState::Idle => vec![WindowAction::ClearAttention],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Signal;

    fn changed(from: AgentState, to: AgentState) -> ChoreoEvent {
        ChoreoEvent::StateChanged(StateChange {
            from,
            to,
            cause: Signal::UserInput,
        })
    }

    #[test]
    fn submitting_collapses_and_clears_the_last_cue() {
        let actions = plan(&ChoreoEvent::Submitted, &ChoreoConfig::default());
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
        );
        assert_eq!(actions, vec![WindowAction::ClearAttention]);
    }

    #[test]
    fn finishing_expands_and_fires_cues() {
        let cfg = ChoreoConfig::default();
        let actions = plan(&changed(AgentState::Busy, AgentState::Done), &cfg);
        assert_eq!(
            actions,
            vec![WindowAction::Expand, WindowAction::Attention(cfg.cues)]
        );
    }

    #[test]
    fn needing_input_behaves_like_finishing() {
        let cfg = ChoreoConfig::default();
        assert_eq!(
            plan(&changed(AgentState::Busy, AgentState::NeedsInput), &cfg),
            plan(&changed(AgentState::Busy, AgentState::Done), &cfg),
        );
    }

    #[test]
    fn idle_never_interrupts() {
        let actions = plan(
            &changed(AgentState::Busy, AgentState::Idle),
            &ChoreoConfig::default(),
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
            plan(&ChoreoEvent::Submitted, &cfg),
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
            plan(&changed(AgentState::Busy, AgentState::Done), &cfg),
            vec![WindowAction::Attention(cfg.cues)]
        );
    }
}
