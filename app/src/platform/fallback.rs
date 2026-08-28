//! Non-macOS placeholder.
//!
//! Ports fill these in: Windows wants `WS_EX_TOPMOST | WS_EX_NOACTIVATE`,
//! Linux wants layer-shell on Wayland and `_NET_WM_STATE_ABOVE` on X11.
//! Until then the window is a plain always-on-top window, which is what
//! `tauri.conf.json` already asks for.

use tauri::{App, Runtime, WebviewWindow};

pub fn set_floating<R: Runtime>(window: &WebviewWindow<R>) -> Result<(), String> {
    window.set_always_on_top(true).map_err(|e| e.to_string())
}

pub fn stay_visible_when_inactive<R: Runtime>(_window: &WebviewWindow<R>) -> Result<(), String> {
    Ok(())
}

pub fn join_all_spaces<R: Runtime>(_window: &WebviewWindow<R>) -> Result<(), String> {
    Ok(())
}

pub fn show_without_focus<R: Runtime>(window: &WebviewWindow<R>) -> Result<(), String> {
    window.show().map_err(|e| e.to_string())
}

/// Ports fill this in: Windows wants `WS_EX_LAYERED` plus
/// `SetLayeredWindowAttributes`, and a Linux compositor wants
/// `_NET_WM_WINDOW_OPACITY`. Leaving the window opaque is the safe
/// failure, since a window nobody can see through is still usable.
pub fn set_opacity<R: Runtime>(_window: &WebviewWindow<R>, _alpha: f64) -> Result<(), String> {
    Ok(())
}

/// Ports fill this in. Linux reads `/proc/<pid>/comm`, which is a plain
/// file and simpler than the macOS route. Windows has no process group on
/// a console handle at all, so it needs a different question entirely.
///
/// Returning `None` is safe: a session with no name falls back to a
/// plain label and everything else works.
pub fn process_name(_pid: i32) -> Option<String> {
    None
}

/// Ports fill this in too. Linux has it as the `/proc/<pid>/exe`
/// symlink. It matters because a program's file name is not always its
/// name: Claude Code installs its executable as the version number, so
/// the path is the only thing that says what it is.
pub fn process_path(_pid: i32) -> Option<String> {
    None
}

/// Ports fill this in too. Linux has it as `/proc/<pid>/cmdline`, which
/// is already NUL-separated and needs no unpacking. It matters because
/// most of these tools are npm packages, so the program is `node` and
/// only the arguments say which tool it is.
pub fn process_args(_pid: i32) -> Option<Vec<String>> {
    None
}

/// Nothing to do: the Dock and Spaces are a macOS idea.
pub fn make_accessory<R: Runtime>(_app: &mut App<R>) {}

/// Spaces are a macOS idea, so the window is always where the user is.
pub fn is_on_active_space<R: Runtime>(_window: &WebviewWindow<R>) -> bool {
    true
}
