//! Driving the window from detected agent state.
//!
//! The rules live in `overterm_core::choreo` as a pure function. This
//! module is only the hands: it resizes the window, raises it without
//! stealing focus, and tells the frontend which view to show.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use overterm_core::choreo::{
    ChoreoConfig, ChoreoEvent, Context, Cues, WindowAction, WindowMode, plan,
};
use overterm_core::{AgentState, StateChange};
use serde::Serialize;
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, Runtime, State, WebviewWindow,
};

use crate::platform::PlatformWindow;

/// Frontend event names.
pub const EVENT_MODE: &str = "overterm://mode";
pub const EVENT_ATTENTION: &str = "overterm://attention";

/// Height of the collapsed bar, in logical pixels.
///
/// Two rows: the status strip and somewhere to type. This has a twin in
/// `tauri.conf.json` as the window's minimum height, and the two have to
/// move together or the window ends up a different size from the layout
/// inside it.
const BAR_HEIGHT: f64 = 68.0;
/// How much of the expanded window's width the collapsed bar keeps.
///
/// It follows the panel so that picking a smaller terminal gives a smaller
/// bar, and it is a fraction rather than the same width because out of the
/// way it should take less room than it does in use.
const BAR_WIDTH_FRACTION: f64 = 0.72;
/// Narrowest the bar goes. Below this the path and the timer start
/// fighting each other for the row.
const MIN_BAR_WIDTH: f64 = 380.0;
/// Widest the bar goes, however large the terminal is set. A bar as wide
/// as a half-screen window is not out of the way any more.
const MAX_BAR_WIDTH: f64 = 620.0;

/// Width of the collapsed bar for a given expanded width.
fn bar_width(panel_width: f64) -> f64 {
    (panel_width * BAR_WIDTH_FRACTION).clamp(MIN_BAR_WIDTH, MAX_BAR_WIDTH)
}

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

/// What the window knows about one session inside it.
struct SessionState {
    state: AgentState,
    /// The user has handed this session something since it last reached a
    /// conclusion. Attention cues are for answering a question somebody
    /// asked, and a terminal does things nobody asked for.
    user_asked: bool,
    /// Whether this session is really doing work, asked before the
    /// terminal is hidden. Supplied by the session layer, which owns the
    /// detector.
    work_check: WorkCheck,
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
    /// Every live session in this window, by id.
    sessions: Mutex<HashMap<String, SessionState>>,
    /// Whether the watcher that reveals a stalled session is running. One
    /// per window rather than one per session, because the question it
    /// asks is about the window and several copies would race to answer.
    watching: AtomicBool,
}

struct Windowing {
    mode: WindowMode,
    /// Height to restore when expanding, remembered from the last time
    /// the window was expanded so the user's own resizing survives.
    panel_height: f64,
    panel_width: f64,
}

#[derive(Clone)]
pub struct Choreographer(Arc<Inner>);

impl Choreographer {
    pub fn new(cfg: ChoreoConfig, panel_width: f64, panel_height: f64) -> Self {
        Self(Arc::new(Inner {
            cfg: Mutex::new(cfg),
            state: Mutex::new(Windowing {
                mode: WindowMode::default(),
                panel_height,
                panel_width,
            }),
            generation: AtomicU64::new(0),
            sessions: Mutex::new(HashMap::new()),
            watching: AtomicBool::new(false),
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

    /// Take a session into the window, and start watching for a stalled
    /// one if nothing is watching yet.
    pub fn add_session<R: Runtime>(&self, app: &AppHandle<R>, id: &str, check: WorkCheck) {
        self.0.sessions.lock().unwrap().insert(
            id.to_string(),
            SessionState {
                state: AgentState::Idle,
                user_asked: false,
                work_check: check,
            },
        );
        // The watcher asks a question about the window, not about any one
        // session, so it starts once and keeps running. One per session
        // would leave several of them racing to reveal the same window.
        if !self.0.watching.swap(true, Ordering::SeqCst) {
            self.watch_for_stall(app);
        }
    }

    /// A session ended. Its state goes with it, or the window would keep
    /// refusing to collapse on behalf of something that is gone.
    pub fn remove_session(&self, id: &str) {
        self.0.sessions.lock().unwrap().remove(id);
    }

    /// Whether some session other than `except` has something to show.
    fn others_want_user(&self, except: &str) -> bool {
        self.0.sessions.lock().unwrap().iter().any(|(id, session)| {
            id != except && matches!(session.state, AgentState::Done | AgentState::NeedsInput)
        })
    }

    /// Whether some session other than `except` is blocked on an answer.
    fn others_need_answer(&self, except: &str) -> bool {
        self.0
            .sessions
            .lock()
            .unwrap()
            .iter()
            .any(|(id, session)| id != except && session.state == AgentState::NeedsInput)
    }

    /// Bring the terminal back when a collapsed session stops working
    /// without reaching a conclusion.
    ///
    /// The bar can only show one line, so a program that printed a
    /// question and is waiting on it leaves the user stuck: the state
    /// stays Busy, nothing finishes, and none of the event-driven rules
    /// fire. Going still is the signal that whatever is on screen is
    /// meant to be read.
    fn watch_for_stall<R: Runtime>(&self, app: &AppHandle<R>) {
        let this = self.clone();
        let app = app.clone();
        std::thread::spawn(move || {
            let mut still_for_ms = 0u64;
            loop {
                std::thread::sleep(Duration::from_millis(STALL_POLL_MS));
                let mode = this.0.state.lock().unwrap().mode;
                // Only a collapsed window has anything to reveal, and only
                // a session that is busy on paper can be stalled: one that
                // already concluded has had its say.
                if mode != WindowMode::Bar || !this.anything_stuck() {
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

    /// Whether a session is sitting on something nobody can see.
    ///
    /// Busy while doing no work is what a program that printed a question
    /// and is waiting on it looks like. A session that is idle, or that
    /// has finished, is not stuck: it is simply not doing anything, and a
    /// shell sitting at its prompt is the normal resting state of every
    /// tab somebody is not currently using.
    fn stuck(state: AgentState, working: bool) -> bool {
        state == AgentState::Busy && !working
    }

    /// Whether any session in the window is stuck.
    ///
    /// This is the question both the collapse and the stall watcher want.
    /// Asking instead whether *every* session is working means one idle
    /// tab stops the window from ever collapsing, since an idle tab is
    /// doing no work by definition.
    fn anything_stuck(&self) -> bool {
        let sessions: Vec<(AgentState, WorkCheck)> = {
            let sessions = self.0.sessions.lock().unwrap();
            sessions
                .values()
                .map(|s| (s.state, s.work_check.clone()))
                .collect()
        };
        // Released before the checks run: each one reaches into a
        // detector, and holding this meanwhile would make every state
        // change in every other session queue up behind it.
        sessions
            .iter()
            .any(|(state, check)| Self::stuck(*state, check()))
    }

    pub fn mode(&self) -> WindowMode {
        self.0.state.lock().unwrap().mode
    }

    /// Run the plan for one detected transition.
    pub fn on_state_change<R: Runtime>(&self, app: &AppHandle<R>, id: &str, change: &StateChange) {
        self.dispatch(app, id, ChoreoEvent::StateChanged(change.clone()));
    }

    /// The user sent a line to a session. Handing over a job is the only
    /// thing that means the terminal can get out of the way.
    pub fn on_submit<R: Runtime>(&self, app: &AppHandle<R>, id: &str) {
        self.dispatch(app, id, ChoreoEvent::Submitted);
    }

    fn dispatch<R: Runtime>(&self, app: &AppHandle<R>, id: &str, event: ChoreoEvent) {
        // Read before this session's own state is updated, so a session
        // that has just concluded does not count itself as one of the
        // others waiting.
        let others_want_user = self.others_want_user(id);
        let others_need_answer = self.others_need_answer(id);

        let user_asked = {
            let mut sessions = self.0.sessions.lock().unwrap();
            // A session that has already gone is not worth planning for.
            let Some(session) = sessions.get_mut(id) else {
                return;
            };
            match &event {
                ChoreoEvent::Submitted => session.user_asked = true,
                ChoreoEvent::StateChanged(change) => session.state = change.to,
            }
            let asked = session.user_asked;
            // Spent on the conclusion it belongs to, so whatever the
            // session does next on its own is quiet again.
            if let ChoreoEvent::StateChanged(change) = &event
                && matches!(change.to, AgentState::Done | AgentState::NeedsInput)
            {
                session.user_asked = false;
            }
            asked
        };

        let ctx = Context {
            user_asked,
            others_want_user,
            others_need_answer,
        };
        let generation = self.0.generation.fetch_add(1, Ordering::SeqCst) + 1;
        for action in plan(&event, &self.cfg(), ctx) {
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
                    if !still_current() || this.anything_stuck() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(CONFIRM_MS));
                    if !still_current() || this.anything_stuck() {
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

    /// Record the size the window is at, if it is the expanded one.
    ///
    /// Called while somebody drags the window's edge. The bar has a size
    /// of its own, so a resize in that mode is not a preference and is
    /// ignored.
    pub fn remember_panel_size(&self, width: f64, height: f64) -> bool {
        let mut state = self.0.state.lock().unwrap();
        if state.mode != WindowMode::Panel {
            return false;
        }
        if state.panel_width == width && state.panel_height == height {
            return false;
        }
        state.panel_width = width;
        state.panel_height = height;
        true
    }

    /// Set the expanded size directly, from the settings sheet.
    pub fn set_panel_size<R: Runtime>(&self, app: &AppHandle<R>, width: f64, height: f64) {
        let mode = {
            let mut state = self.0.state.lock().unwrap();
            state.panel_width = width;
            state.panel_height = height;
            state.mode
        };
        if let Some(window) = app.get_webview_window(crate::MAIN_WINDOW) {
            // Collapsed, the bar is what needs re-fitting: the panel size
            // is stored and takes effect the next time it expands.
            match mode {
                WindowMode::Panel => resize_keeping_top(&window, width, height),
                WindowMode::Bar => resize_keeping_top(&window, bar_width(width), BAR_HEIGHT),
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
            if let Some(width) = logical_width(&window) {
                state.panel_width = width;
            }
            state.mode = WindowMode::Bar;
        }
        let width = {
            let state = self.0.state.lock().unwrap();
            bar_width(state.panel_width)
        };
        let _ = window.set_resizable(false);
        resize_keeping_top(&window, width, BAR_HEIGHT);
        let _ = app.emit(
            EVENT_MODE,
            ModePayload {
                mode: WindowMode::Bar,
            },
        );
    }

    /// Record that the window is in `mode` now, and say whether that was
    /// a change.
    ///
    /// The answer is what decides whether the window gets touched at all.
    /// Reshaping a window into the shape it is already in is not free: on
    /// macOS `setStyleMask` makes a window resign first responder, so the
    /// terminal stops taking keystrokes and the user has to click back
    /// into it before they can type.
    fn entering(&self, mode: WindowMode) -> bool {
        let mut state = self.0.state.lock().unwrap();
        let changed = state.mode != mode;
        state.mode = mode;
        changed
    }

    fn expand<R: Runtime>(&self, app: &AppHandle<R>, raise: bool) {
        let Some(window) = app.get_webview_window(crate::MAIN_WINDOW) else {
            return;
        };
        let changed = self.entering(WindowMode::Panel);
        if changed {
            let (width, height) = {
                let state = self.0.state.lock().unwrap();
                (state.panel_width, state.panel_height)
            };
            let _ = window.set_resizable(true);
            resize_keeping_top(&window, width, height);
        }
        if raise {
            // An agent finishing its work must never pull keyboard focus
            // out of whatever the user is typing in.
            if let Err(e) = window.show_without_focus() {
                eprintln!("[choreograph] raise failed: {e}");
            }
        }
        // Only on a real change. The frontend refocuses the terminal when
        // this arrives, which is right after a collapse and wrong while
        // somebody is typing in the find bar or the settings sheet.
        if changed {
            let _ = app.emit(
                EVENT_MODE,
                ModePayload {
                    mode: WindowMode::Panel,
                },
            );
        }
    }
}

fn logical_size<R: Runtime>(window: &tauri::WebviewWindow<R>) -> Option<(f64, f64)> {
    let scale = window.scale_factor().ok()?;
    let size = window.inner_size().ok()?.to_logical::<f64>(scale);
    Some((size.width, size.height))
}

/// Resize without moving the top edge, and keep the result on screen.
///
/// Tauri positions a window by its top-left corner, but a macOS window
/// frame is anchored at its bottom-left, so growing the height alone
/// pushes the top of the window upward. Expanding a bar that sat near the
/// top of the display put the title bar above the top of the screen,
/// taking the only part you can drag with it, and there was then no way
/// to get the window back.
fn resize_keeping_top<R: Runtime>(window: &WebviewWindow<R>, width: f64, height: f64) {
    let before = window.outer_position().ok();
    if let Err(e) = window.set_size(LogicalSize::new(width, height)) {
        eprintln!("[choreograph] resize failed: {e}");
        return;
    }
    let Some(top_left) = before else {
        return;
    };
    let _ = window.set_position(top_left);
    keep_on_screen(window, height);

    // Set OVERTERM_WINDOW_DEBUG to watch where the window actually ends
    // up. "I asked for this position" and "the window is at this
    // position" are different claims, and only the second one is worth
    // anything: this whole function exists because a resize was moving
    // the window somewhere nobody asked it to go.
    if std::env::var_os("OVERTERM_WINDOW_DEBUG").is_some()
        && let Ok(after) = window.outer_position()
    {
        eprintln!(
            "[choreograph] resize to {width}x{height}: top-left {},{} -> {},{}",
            top_left.x, top_left.y, after.x, after.y
        );
    }
}

/// Pull the window back onto the display if it now hangs off the bottom.
///
/// Only ever moves it up, and never past the top of the screen, because
/// the top edge is where the part you can grab lives. A window taller
/// than the display keeps its top on screen rather than its bottom.
fn keep_on_screen<R: Runtime>(window: &WebviewWindow<R>, height: f64) {
    let (Ok(Some(monitor)), Ok(position)) = (window.current_monitor(), window.outer_position())
    else {
        return;
    };
    let screen_top = monitor.position().y;
    let screen_bottom = screen_top + monitor.size().height as i32;
    let tall = (height * monitor.scale_factor()).round() as i32;

    let mut y = position.y;
    if y + tall > screen_bottom {
        y = screen_bottom - tall;
    }
    if y < screen_top {
        y = screen_top;
    }
    if y != position.y {
        let _ = window.set_position(PhysicalPosition::new(position.x, y));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_idle_session_is_not_stuck() {
        // The bug this is here to keep fixed: a second tab sitting at its
        // shell prompt, having finished whatever it ran, used to stop the
        // window from ever collapsing for the tab that was working.
        assert!(!Choreographer::stuck(AgentState::Idle, false));
        assert!(!Choreographer::stuck(AgentState::Done, false));
        assert!(!Choreographer::stuck(AgentState::NeedsInput, false));
    }

    #[test]
    fn busy_while_doing_no_work_is_stuck() {
        // What a program that printed a question and is waiting on it
        // looks like: it holds the state at Busy and does nothing.
        assert!(Choreographer::stuck(AgentState::Busy, false));
    }

    #[test]
    fn busy_and_working_is_just_working() {
        assert!(!Choreographer::stuck(AgentState::Busy, true));
    }

    #[test]
    fn expanding_a_window_that_is_already_expanded_touches_nothing() {
        // The bug this is here to keep fixed. A turn short enough that
        // the window never collapsed still ended with an expand, and
        // reshaping an already-expanded window costs the terminal its
        // keyboard focus: setStyleMask makes a macOS window resign first
        // responder. Asking an agent something that came back with a
        // question in under a second meant clicking back into the window
        // before you could answer it.
        let choreo = Choreographer::new(ChoreoConfig::default(), 660.0, 620.0);
        assert_eq!(choreo.mode(), WindowMode::Panel, "starts expanded");

        assert!(
            !choreo.entering(WindowMode::Panel),
            "already expanded, so nothing may touch the window"
        );

        // And a real transition still counts as one, in both directions.
        assert!(choreo.entering(WindowMode::Bar));
        assert!(!choreo.entering(WindowMode::Bar));
        assert!(choreo.entering(WindowMode::Panel));
    }

    #[test]
    fn the_bar_follows_the_terminal_it_belongs_to() {
        // Picking a smaller terminal should give a smaller bar, so the two
        // read as the same window rather than as unrelated sizes.
        let small = bar_width(520.0);
        let medium = bar_width(660.0);
        assert!(
            small < medium,
            "a smaller terminal has to give a smaller bar: {small} vs {medium}"
        );
        assert!(
            medium < 660.0,
            "out of the way it should take less room than in use"
        );
    }

    #[test]
    fn the_bar_stays_between_its_two_ends() {
        // A tiny terminal must not give a bar too narrow for the path and
        // the timer, and a half-screen one must not give a bar so wide it
        // stops being out of the way.
        assert_eq!(bar_width(100.0), MIN_BAR_WIDTH);
        assert_eq!(bar_width(4000.0), MAX_BAR_WIDTH);
    }

    #[test]
    fn the_collapsed_height_matches_the_window_minimum() {
        // The bar's height is decided here and the window's minimum is
        // decided in tauri.conf.json. If they drift, the window is a
        // different size from the layout inside it and the bar either
        // clips or floats in a gap.
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("the config is JSON");
        let minimum = config["app"]["windows"][0]["minHeight"]
            .as_f64()
            .expect("the main window sets a minimum height");

        assert_eq!(
            minimum, BAR_HEIGHT,
            "tauri.conf.json says {minimum} and the bar is {BAR_HEIGHT}"
        );
    }
}
