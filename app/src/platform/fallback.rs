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

/// Nothing to do: the Dock and Spaces are a macOS idea.
pub fn make_accessory<R: Runtime>(_app: &mut App<R>) {}

/// Spaces are a macOS idea, so the window is always where the user is.
pub fn is_on_active_space<R: Runtime>(_window: &WebviewWindow<R>) -> bool {
    true
}
