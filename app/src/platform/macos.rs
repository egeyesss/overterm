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

use objc2_app_kit::{NSApplication, NSStatusWindowLevel, NSWindow, NSWindowCollectionBehavior};
use tauri::{ActivationPolicy, App, Runtime, WebviewWindow};

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

/// Fade the whole window, chrome and terminal together.
///
/// `alpha` is 0.0 to 1.0. AppKit composites the window at this alpha, so
/// unlike a CSS opacity it makes what is behind the overlay genuinely
/// visible rather than blending against the window's own background.
pub fn set_opacity<R: Runtime>(window: &WebviewWindow<R>, alpha: f64) -> Result<(), String> {
    with_window(window, "opacity", move |ns| {
        ns.setAlphaValue(alpha);
    })
}

pub fn set_floating<R: Runtime>(window: &WebviewWindow<R>) -> Result<(), String> {
    with_window(window, "floating level", |ns| {
        // Status level, which is 25. The floating level this started on
        // is 3, high enough to sit above ordinary windows and no higher.
        // A full-screen app composites above the menu bar at level 24, so
        // everything under that is behind a full-screen video and the
        // overlay may as well not be there. Watching something while an
        // agent works is exactly when a terminal that stays out of the
        // way earns its keep.
        //
        // One step above the menu bar and no further. The levels past
        // this one cover system alerts and password prompts, which is not
        // ours to do.
        ns.setLevel(NSStatusWindowLevel);
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
            // On every Space, without dragging the user's Space along
            // when they switch.
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                // These two are the ones that matter for sitting over
                // somebody else's full-screen app, and they are easy to
                // miss because every older answer names the three above.
                // CanJoinAllSpaces puts the window on every ordinary
                // Space and stops at a full-screen one, which measured as
                // isOnActiveSpace being false the whole time a video was
                // playing. Auxiliary plus CanJoinAllApplications is what
                // lets a window join a Space another application owns.
                // Both arrived in macOS 13; on anything older they are
                // inert bits and the window behaves as it did before.
                | NSWindowCollectionBehavior::Auxiliary
                | NSWindowCollectionBehavior::CanJoinAllApplications,
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

/// Become an accessory app: no Dock icon, no menu bar of its own.
///
/// Which is what a hotkey-summoned window that lives on top of other
/// things wants to be, and how every comparable overlay ships. It also
/// settles the leftover where clicking the terminal made this the
/// frontmost app.
///
/// It is not what makes the window appear over another application's
/// full-screen space; the collection behaviour above does that. This was
/// added while looking for the cause and kept on its own merits.
pub fn make_accessory<R: Runtime>(app: &mut App<R>) {
    app.set_activation_policy(ActivationPolicy::Accessory);
}

/// Whether the window is on the Space the user is looking at.
pub fn is_on_active_space<R: Runtime>(window: &WebviewWindow<R>) -> bool {
    // Reading this has to happen on the main thread, and the callers are
    // already there: the hotkey handler runs on the event loop.
    match unsafe { ns_window(window) } {
        Ok(ns) => ns.isOnActiveSpace(),
        Err(_) => true,
    }
}
