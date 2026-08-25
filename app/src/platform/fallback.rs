//! Non-macOS placeholder.
//!
//! Ports fill these in: Windows wants `WS_EX_TOPMOST | WS_EX_NOACTIVATE`,
//! Linux wants layer-shell on Wayland and `_NET_WM_STATE_ABOVE` on X11.
//! Until then the window is a plain always-on-top window, which is what
//! `tauri.conf.json` already asks for.

use tauri::{Runtime, WebviewWindow};

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
