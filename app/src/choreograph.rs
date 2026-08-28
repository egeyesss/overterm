//! Driving the window from detected agent state.
//!
//! The rules live in `overterm_core::choreo` as a pure function. This
//! module is only the hands: it resizes the window, raises it without
//! stealing focus, and tells the frontend which view to show.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use overterm_core::choreo::{ChoreoConfig, ChoreoEvent, Cues, WindowAction, WindowMode, plan};
use overterm_core::{AgentState, StateChange};
use serde::Serialize;
use tauri::{AppHandle, Emitter, LogicalSize, Manager, Runtime, State};

use crate::platform::PlatformWindow;

/// Frontend event names.
pub const EVENT_MODE: &str = "overterm://mode";
pub const EVENT_ATTENTION: &str = "overterm://attention";

/// Height of the collapsed bar, in logical pixels.
const BAR_HEIGHT: f64 = 64.0;
/// Height to restore to if the window was never measured while expanded.
const DEFAULT_PANEL_HEIGHT: f64 = 620.0;
/// Gap between the two checks that a collapse is safe. Claude repaints its
/// status area every several seconds while it sits idle, which reads as
/// activity for a moment; a session that is genuinely working is still
/// working half a second later, a stray repaint is not.
const CONFIRM_MS: u64 = 500;

/// Answers "is the session actually doing work right now", asked before
/// hiding the terminal. Supplied by the session layer, which owns the
/// detector.
pub type WorkCheck = Arc<dyn Fn() -> bool + Send + Sync>;

/// How often a collapsed session is checked for having gone still.
const STALL_POLL_MS: u64 = 400;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModePayload {
    mode: WindowMode,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AttentionPayload {
    /// False means clear whatever cue is showing.
    active: bool,
    cues: Cues,
}

struct Inner {
    /// Behind a lock because the settings can change while the app runs.
    /// Always copied out rather than held: the window calls below can
    /// block, and nothing else should wait on them to read a preference.
    cfg: Mutex<ChoreoConfig>,
    state: Mutex<Windowing>,
    /// Bumped on every state change. A delayed collapse only fires if the
    /// generation it was scheduled with is still current, so work that
    /// finishes quickly never collapses the window.
    generation: AtomicU64,
    work_check: Mutex<Option<WorkCheck>>,
    /// Bumped when a new session registers, so the watcher belonging to
    /// the old one stops.
    work_generation: AtomicU64,
}

struct Windowing {
    mode: WindowMode,
    state: AgentState,
    /// Whether the user has handed this session something to do since it
    /// last reached a conclusion. Attention cues are for answering a
    /// question somebody asked, and a terminal does things nobody asked
    /// for: the folder trust prompt Claude Code shows before the user has
    /// said anything is the case this exists for.
    user_asked: bool,
    /// Height to restore when expanding, remembered from the last time
    /// the window was expanded so the user's own resizing survives.
    panel_height: f64,
}

#[derive(Clone)]
pub struct Choreographer(Arc<Inner>);

impl Choreographer {
    pub fn new(cfg: ChoreoConfig) -> Self {
        Self(Arc::new(Inner {
            cfg: Mutex::new(cfg),
            state: Mutex::new(Windowing {
                mode: WindowMode::default(),
                state: AgentState::Idle,
                user_asked: false,
                panel_height: DEFAULT_PANEL_HEIGHT,
            }),
            generation: AtomicU64::new(0),
            work_check: Mutex::new(None),
            work_generation: AtomicU64::new(0),
        }))
    }

    fn cfg(&self) -> ChoreoConfig {
        *self.0.cfg.lock().unwrap()
    }

    /// Take new preferences without a restart.
    ///
    /// Only affects what happens next. A collapse already scheduled runs
    /// on the delay it was scheduled with, which is a moment old at
    /// worst and not worth the bookkeeping to chase.
    pub fn set_config(&self, cfg: ChoreoConfig) {
        *self.0.cfg.lock().unwrap() = cfg;
    }

    /// Register how to tell whether the session is working, and start
    /// watching for it going still while collapsed. Called when a session
    /// spawns.
    pub fn set_work_check<R: Runtime>(&self, app: &AppHandle<R>, check: WorkCheck) {
        *self.0.work_check.lock().unwrap() = Some(check);
        let generation = self.0.work_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.watch_for_stall(app, generation);
    }

    /// Bring the terminal back when a collapsed session stops working
    /// without reaching a conclusion.
    ///
    /// The bar can only show one line, so a program that printed a
    /// question and is waiting on it leaves the user stuck: the state
    /// stays Busy, nothing finishes, and none of the event-driven rules
    /// fire. Going still is the signal that whatever is on screen is
    /// meant to be read.
    fn watch_for_stall<R: Runtime>(&self, app: &AppHandle<R>, generation: u64) {
        let this = self.clone();
        let app = app.clone();
        std::thread::spawn(move || {
            let mut still_for_ms = 0u64;
            loop {
                std::thread::sleep(Duration::from_millis(STALL_POLL_MS));
                if this.0.work_generation.load(Ordering::SeqCst) != generation {
                    return; // a newer session owns the window now
                }
                let (mode, state) = {
                    let windowing = this.0.state.lock().unwrap();
                    (windowing.mode, windowing.state)
                };
                if mode != WindowMode::Bar || state != AgentState::Busy || this.working() {
                    still_for_ms = 0;
                    continue;
                }
                still_for_ms += STALL_POLL_MS;
                if still_for_ms < this.cfg().reveal_when_stalled_ms {
                    continue;
                }
                still_for_ms = 0;
                // Do not drag a window the user deliberately hid back onto
                // the screen; just have it ready when they summon it.
                let visible = app
                    .get_webview_window(crate::MAIN_WINDOW)
                    .and_then(|w| w.is_visible().ok())
                    .unwrap_or(false);
                this.expand(&app, visible);
            }
        });
    }

    /// With no session to protect there is nothing to hide, so an
    /// unanswerable question is not a reason to refuse.
    fn working(&self) -> bool {
        let check = self.0.work_check.lock().unwrap().clone();
        check.is_none_or(|check| check())
    }

    pub fn mode(&self) -> WindowMode {
        self.0.state.lock().unwrap().mode
    }

    /// Run the plan for one detected transition.
    pub fn on_state_change<R: Runtime>(&self, app: &AppHandle<R>, change: &StateChange) {
        self.dispatch(app, ChoreoEvent::StateChanged(change.clone()));
    }

    /// The user sent a line to the session. Handing over a job is the only
    /// thing that means the terminal can get out of the way.
    pub fn on_submit<R: Runtime>(&self, app: &AppHandle<R>) {
        self.dispatch(app, ChoreoEvent::Submitted);
    }

    fn dispatch<R: Runtime>(&self, app: &AppHandle<R>, event: ChoreoEvent) {
        let user_asked = {
            let mut windowing = self.0.state.lock().unwrap();
            match &event {
                ChoreoEvent::Submitted => windowing.user_asked = true,
                ChoreoEvent::StateChanged(change) => windowing.state = change.to,
            }
            let asked = windowing.user_asked;
            // Spent on the conclusion it belongs to, so the next thing
            // the session does on its own is quiet again.
            if let ChoreoEvent::StateChanged(change) = &event
                && matches!(change.to, AgentState::Done | AgentState::NeedsInput)
            {
                windowing.user_asked = false;
            }
            asked
        };
        let generation = self.0.generation.fetch_add(1, Ordering::SeqCst) + 1;
        for action in plan(&event, &self.cfg(), user_asked) {
            self.apply(app, action, generation);
        }
    }

    /// Switch modes because the user asked, not the agent. Cancels any
    /// collapse still waiting to fire so it cannot undo the choice.
    pub fn set_mode<R: Runtime>(&self, app: &AppHandle<R>, mode: WindowMode) {
        self.0.generation.fetch_add(1, Ordering::SeqCst);
        match mode {
            WindowMode::Bar => self.collapse(app),
            WindowMode::Panel => self.expand(app, false),
        }
    }

    fn apply<R: Runtime>(&self, app: &AppHandle<R>, action: WindowAction, generation: u64) {
        match action {
            WindowAction::Collapse { after_ms } => {
                let this = self.clone();
                let app = app.clone();
                std::thread::spawn(move || {
                    let still_current = || this.0.generation.load(Ordering::SeqCst) == generation;
                    std::thread::sleep(Duration::from_millis(after_ms));
                    // Busy is not permission to hide the terminal. A CLI
                    // that printed a question and is waiting on an answer
                    // holds the state at Busy too, and collapsing then
                    // hides the very thing the user has to respond to.
                    if !still_current() || !this.working() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(CONFIRM_MS));
                    if !still_current() || !this.working() {
                        return;
                    }
                    this.collapse(&app);
                });
            }
            WindowAction::Expand => self.expand(app, true),
            WindowAction::Attention(cues) => {
                let _ = app.emit(EVENT_ATTENTION, AttentionPayload { active: true, cues });
            }
            WindowAction::ClearAttention => {
                let _ = app.emit(
                    EVENT_ATTENTION,
                    AttentionPayload {
                        active: false,
                        cues: Cues::default(),
                    },
                );
            }
        }
    }

    fn collapse<R: Runtime>(&self, app: &AppHandle<R>) {
        let Some(window) = app.get_webview_window(crate::MAIN_WINDOW) else {
            return;
        };
        {
            let mut state = self.0.state.lock().unwrap();
            if state.mode == WindowMode::Bar {
                return;
            }
            if let Some(height) = logical_height(&window) {
                state.panel_height = height;
            }
            state.mode = WindowMode::Bar;
        }
        if let Some(width) = logical_width(&window) {
            let _ = window.set_size(LogicalSize::new(width, BAR_HEIGHT));
        }
        let _ = app.emit(
            EVENT_MODE,
            ModePayload {
                mode: WindowMode::Bar,
            },
        );
    }

    fn expand<R: Runtime>(&self, app: &AppHandle<R>, raise: bool) {
        let Some(window) = app.get_webview_window(crate::MAIN_WINDOW) else {
            return;
        };
        let height = {
            let mut state = self.0.state.lock().unwrap();
            state.mode = WindowMode::Panel;
            state.panel_height
        };
        if let Some(width) = logical_width(&window) {
            let _ = window.set_size(LogicalSize::new(width, height));
        }
        if raise {
            // An agent finishing its work must never pull keyboard focus
            // out of whatever the user is typing in.
            if let Err(e) = window.show_without_focus() {
                eprintln!("[choreograph] raise failed: {e}");
            }
        }
        let _ = app.emit(
            EVENT_MODE,
            ModePayload {
                mode: WindowMode::Panel,
            },
        );
    }
}

fn logical_size<R: Runtime>(window: &tauri::WebviewWindow<R>) -> Option<(f64, f64)> {
    let scale = window.scale_factor().ok()?;
    let size = window.inner_size().ok()?.to_logical::<f64>(scale);
    Some((size.width, size.height))
}

fn logical_width<R: Runtime>(window: &tauri::WebviewWindow<R>) -> Option<f64> {
    logical_size(window).map(|(width, _)| width)
}

fn logical_height<R: Runtime>(window: &tauri::WebviewWindow<R>) -> Option<f64> {
    logical_size(window).map(|(_, height)| height)
}

/// The user clicked the bar or the collapse control.
#[tauri::command]
pub fn set_window_mode(mode: WindowMode, app: AppHandle, choreo: State<'_, Choreographer>) {
    choreo.set_mode(&app, mode);
}

/// Current mode, so a reloading frontend comes back in the right view.
#[tauri::command]
pub fn window_mode(choreo: State<'_, Choreographer>) -> WindowMode {
    choreo.mode()
}

/// Push the window off screen. The global hotkey brings it back.
#[tauri::command]
pub fn hide_window(app: AppHandle) {
    if let Some(window) = app.get_webview_window(crate::MAIN_WINDOW)
        && let Err(e) = window.hide()
    {
        eprintln!("[choreograph] hide failed: {e}");
    }
}
