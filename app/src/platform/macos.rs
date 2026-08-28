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

/// Name of the program running as `pid`, if it is still there.
///
/// `proc_name` is asked directly rather than shelling out to `ps`,
/// because this is asked once a second per session and an app that
/// spawns a process on a timer has no business calling itself
/// lightweight.
///
/// The answer is the executable's name, so it is `claude`, `zsh` or
/// `nvim`, and it needs no cooperation from the program itself. Two
/// things it cannot do: the kernel truncates it to sixteen characters,
/// and a tool that runs through an interpreter reports the interpreter,
/// so a CLI shipped as a script shows up as `node` or `python`. The
/// marker a hooked program writes is what covers that case exactly.
pub fn process_path(pid: i32) -> Option<String> {
    // PROC_PIDPATHINFO_MAXSIZE. Written out rather than pulled from a
    // header so the buffer and the constant cannot disagree.
    let mut buf = [0u8; 4096];

    // Safety: the buffer is ours and the length passed is its real one.
    // A pid that has gone returns zero without writing anything.
    let written = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if written <= 0 {
        return None;
    }
    let path = std::str::from_utf8(&buf[..written as usize]).ok()?;
    (!path.is_empty()).then(|| path.to_string())
}

/// The command line of the process running as `pid`.
///
/// Needed because a lot of these tools are npm packages, so the program
/// on the terminal is `node` and the only thing naming the tool is the
/// script path it was given. Reading the executable alone labels every
/// one of them `node`.
///
/// `KERN_PROCARGS2` hands back a block laid out as the argument count,
/// then the executable path, then padding, then that many NUL-separated
/// arguments, then the environment. Only the arguments are wanted, and
/// the environment is deliberately not walked into: it belongs to the
/// user and holds their secrets.
pub fn process_args(pid: i32) -> Option<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: usize = 0;

    // Safety: asking with a null buffer only reports how big one has to
    // be, which is the documented way to size this call.
    let sized = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            std::ptr::null_mut(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if sized != 0 || size == 0 {
        return None;
    }

    let mut buf = vec![0u8; size];
    // Safety: the buffer is ours and `size` describes it. A process that
    // has gone, or one owned by somebody else, fails rather than writing.
    let ok = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            buf.as_mut_ptr().cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ok != 0 {
        return None;
    }
    buf.truncate(size);

    let argc = u32::from_ne_bytes(buf.get(..4)?.try_into().ok()?) as usize;
    let mut rest = buf.get(4..)?.splitn(2, |b| *b == 0).nth(1)?;
    // Padding between the executable path and the first argument.
    while rest.first() == Some(&0) {
        rest = &rest[1..];
    }

    Some(
        rest.split(|b| *b == 0)
            .take(argc)
            .filter_map(|arg| std::str::from_utf8(arg).ok())
            .map(str::to_string)
            .collect(),
    )
}

pub fn process_name(pid: i32) -> Option<String> {
    // Long enough for the truncated name the kernel returns, with room
    // to spare rather than a number worked out from a header.
    let mut buf = [0u8; 256];

    // Safety: the buffer is ours, and the length passed is its real one.
    // A pid that has gone returns zero without writing anything.
    let written = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if written <= 0 {
        return None;
    }

    let name = std::str::from_utf8(&buf[..written as usize]).ok()?;
    let name = name.trim_end_matches('\0').trim();
    (!name.is_empty()).then(|| name.to_string())
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
