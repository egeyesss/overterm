//! macOS: make Tauri's window behave like a floating overlay.
//!
//! Tauri's own `show()` goes through tao, which calls
//! `makeKeyAndOrderFront:` and therefore takes keyboard focus. An agent
//! finishing its work must never do that to whatever the user is typing
//! in, so raising the window goes through `orderFrontRegardless` here
//! instead.
//!
//! What is deliberately not done: converting the window to an NSPanel.
//! The non-activating style mask (clicking the window without making
//! OverTerm the frontmost app) only exists on NSPanel, and the usual
//! trick is to repoint the existing window at the NSPanel class at
//! runtime. tao's window class carries an extra instance variable and is
//! 8 bytes larger than NSPanel, so that swap is not layout compatible
//! and objc2 rejects it outright. Everything the overlay actually needs
//! works on a plain NSWindow; the only thing given up is that clicking
//! the terminal also switches the active app, which is what any normal
//! window does.
//!
//! AppKit window calls have to happen on the main thread, so each entry
//! point hops there through Tauri's event loop rather than trusting the
//! caller.

use objc2_app_kit::{NSApplication, NSFloatingWindowLevel, NSWindow, NSWindowCollectionBehavior};
use tauri::{Runtime, WebviewWindow};

/// Borrow the window's AppKit object.
///
/// # Safety
/// Must run on the main thread, and the reference must not outlive the
/// window.
unsafe fn ns_window<R: Runtime>(window: &WebviewWindow<R>) -> Result<&NSWindow, String> {
    let ptr = window.ns_window().map_err(|e| e.to_string())? as *const NSWindow;
    if ptr.is_null() {
        return Err("window has no NSWindow".into());
    }
    Ok(unsafe { &*ptr })
}

/// Run `f` against the window on the main thread.
fn with_window<R, F>(window: &WebviewWindow<R>, what: &'static str, f: F) -> Result<(), String>
where
    R: Runtime,
    F: FnOnce(&NSWindow) + Send + 'static,
{
    let target = window.clone();
    window
        .run_on_main_thread(move || match unsafe { ns_window(&target) } {
            Ok(ns) => f(ns),
            Err(e) => eprintln!("[platform] {what}: {e}"),
        })
        .map_err(|e| e.to_string())
}

pub fn set_floating<R: Runtime>(window: &WebviewWindow<R>) -> Result<(), String> {
    with_window(window, "floating level", |ns| {
        ns.setLevel(NSFloatingWindowLevel);
    })
}

pub fn stay_visible_when_inactive<R: Runtime>(window: &WebviewWindow<R>) -> Result<(), String> {
    with_window(window, "stay visible", |ns| {
        // Being inactive is the normal state while an agent works, so the
        // overlay must not vanish when the user clicks back into their
        // editor.
        ns.setHidesOnDeactivate(false);
    })
}

pub fn join_all_spaces<R: Runtime>(window: &WebviewWindow<R>) -> Result<(), String> {
    with_window(window, "join all spaces", |ns| {
        ns.setCollectionBehavior(
            // On every Space, without dragging the user's Space along when
            // they switch, and allowed to sit over full-screen apps.
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
    })
}

pub fn show_without_focus<R: Runtime>(window: &WebviewWindow<R>) -> Result<(), String> {
    with_window(window, "show without focus", |ns| {
        // Raises the window even though this app is not active, and unlike
        // makeKeyAndOrderFront it leaves keyboard focus where it is.
        ns.orderFrontRegardless();
    })
}

/// Read back what the window and the app actually ended up with.
///
/// Behind `OVERTERM_WINDOW_DEBUG`. Every one of these values was set
/// deliberately somewhere above, and reading them back is still the only
/// way to find out which of them macOS honoured.
pub fn report_state<R: Runtime>(
    window: &WebviewWindow<R>,
    when: &'static str,
) -> Result<(), String> {
    with_window(window, "report state", move |ns| {
        let mtm = objc2_foundation::MainThreadMarker::new()
            .expect("with_window already hopped to the main thread");
        let app = NSApplication::sharedApplication(mtm);
        eprintln!(
            "[platform] {when}: level={} behavior={:?} policy={:?} visible={} onActiveSpace={} keyWindow={}",
            ns.level(),
            ns.collectionBehavior(),
            app.activationPolicy(),
            ns.isVisible(),
            ns.isOnActiveSpace(),
            ns.isKeyWindow(),
        );
    })
}
