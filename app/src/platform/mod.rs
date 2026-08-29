//! Native window behaviour, kept behind one trait.
//!
//! Everything that needs OS window APIs lives in this module. The rest of
//! the app talks to `PlatformWindow` only, so porting to Windows or Linux
//! means writing one more implementation and changing nothing else.

use tauri::{AppHandle, Runtime, WebviewWindow};

#[cfg_attr(target_os = "macos", path = "macos.rs")]
#[cfg_attr(not(target_os = "macos"), path = "fallback.rs")]
mod imp;

/// Window tricks an always-on-top agent overlay needs.
pub trait PlatformWindow {
    /// Float above ordinary windows.
    fn set_floating(&self) -> Result<(), String>;

    /// Stay on screen while another app is frontmost, which is the normal
    /// state while an agent works.
    fn stay_visible_when_inactive(&self) -> Result<(), String>;

    /// Stay visible on every Space, including other apps' full-screen ones.
    fn join_all_spaces(&self) -> Result<(), String>;

    /// Whether the window is on the Space the user is looking at. A
    /// window can be visible and still be somewhere the user cannot see.
    fn is_on_active_space(&self) -> bool;

    /// Bring the window forward without taking keyboard focus. This is what
    /// an agent finishing its work triggers, so it must never steal a
    /// keystroke from whatever the user is typing in.
    fn show_without_focus(&self) -> Result<(), String>;

    /// Fade the window so what is behind it stays readable. `alpha` runs
    /// from 0.0 to 1.0.
    fn set_opacity(&self, alpha: f64) -> Result<(), String>;
}

impl<R: Runtime> PlatformWindow for WebviewWindow<R> {
    fn set_floating(&self) -> Result<(), String> {
        imp::set_floating(self)
    }

    fn stay_visible_when_inactive(&self) -> Result<(), String> {
        imp::stay_visible_when_inactive(self)
    }

    fn join_all_spaces(&self) -> Result<(), String> {
        imp::join_all_spaces(self)
    }

    fn is_on_active_space(&self) -> bool {
        imp::is_on_active_space(self)
    }

    fn show_without_focus(&self) -> Result<(), String> {
        imp::show_without_focus(self)
    }

    fn set_opacity(&self, alpha: f64) -> Result<(), String> {
        imp::set_opacity(self, alpha)
    }
}

/// Name of the program running as `pid`.
///
/// The second thing in this module that needs writing for a port, and
/// the only one that is not about windows. It is what lets a tab say
/// which agent is in it without the agent having to cooperate.
pub fn process_name(pid: i32) -> Option<String> {
    imp::process_name(pid)
}

/// Full path of the program running as `pid`.
///
/// Needed because a file name is not always a name. Claude Code installs
/// its executable as the bare version number, so the process is called
/// something like `2.1.250` and only the path says what it really is.
pub fn process_path(pid: i32) -> Option<String> {
    imp::process_path(pid)
}

/// Command line of the process running as `pid`.
///
/// The third thing a port has to write. Most agent CLIs are npm
/// packages, so the program on the terminal is `node` and only the script
/// path it was given says which tool it actually is.
pub fn process_args(pid: i32) -> Option<Vec<String>> {
    imp::process_args(pid)
}

/// Working directory of the process running as `pid`.
///
/// The fourth thing a port has to write, and the second that is about
/// processes rather than windows. It is asked of the foreground process
/// rather than recorded at spawn, because a shell changes directory and
/// the value from spawn stops being true the moment it does.
pub fn process_cwd(pid: i32) -> Option<String> {
    imp::process_cwd(pid)
}

/// Take the app out of the Dock and the app switcher.
///
/// The application half of the overlay, and not a preference. On macOS
/// the Dock icon and the ability to sit over another application's
/// full-screen space are the same decision: an app with an icon owns a
/// Space of its own and macOS takes the user to it instead of drawing the
/// window onto the Space in front. A build that offered this as a setting
/// was a build whose overlay could be switched off by accident, so it is
/// not offered. The menu bar item is what the app has instead of an icon.
///
/// A property of the application rather than of any window, so it takes
/// the app handle. Call it before the window is shown, and pair it with
/// `make_overlay`: neither half works on its own.
pub fn make_accessory<R: Runtime>(app: &AppHandle<R>) {
    imp::make_accessory(app);
}

/// Apply the window half of the overlay treatment. Call once during
/// setup, on the main thread.
///
/// The other half is `make_accessory` above, which is an application
/// property and so is set separately. Sitting over another application's
/// full-screen space needs both.
pub fn make_overlay<R: Runtime>(window: &WebviewWindow<R>) -> Result<(), String> {
    window.stay_visible_when_inactive()?;
    window.set_floating()?;
    window.join_all_spaces()?;
    // Set OVERTERM_WINDOW_DEBUG to have the window report what it
    // actually ended up with. Worth keeping: the flags that let a window
    // sit over another app's full-screen Space were found by reading
    // this back rather than by reasoning about what had been set.
    #[cfg(target_os = "macos")]
    if std::env::var_os("OVERTERM_WINDOW_DEBUG").is_some() {
        imp::report_state(window, "after setup")?;
    }
    Ok(())
}
