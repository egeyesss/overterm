//! Native window behaviour, kept behind one trait.
//!
//! Everything that needs OS window APIs lives in this module. The rest of
//! the app talks to `PlatformWindow` only, so porting to Windows or Linux
//! means writing one more implementation and changing nothing else.

use tauri::{Runtime, WebviewWindow};

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

    /// Bring the window forward without taking keyboard focus. This is what
    /// an agent finishing its work triggers, so it must never steal a
    /// keystroke from whatever the user is typing in.
    fn show_without_focus(&self) -> Result<(), String>;
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

    fn show_without_focus(&self) -> Result<(), String> {
        imp::show_without_focus(self)
    }
}

/// Apply the full overlay treatment. Call once during setup, on the main
/// thread.
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
