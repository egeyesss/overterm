//! Non-macOS placeholder.
//!
//! Ports fill these in: Windows wants `WS_EX_TOPMOST | WS_EX_NOACTIVATE`,
//! Linux wants layer-shell on Wayland and `_NET_WM_STATE_ABOVE` on X11.
//! Until then the window is a plain always-on-top window, which is what
//! `tauri.conf.json` already asks for.

use tauri::{AppHandle, Runtime, WebviewWindow};

#[cfg(target_os = "linux")]
use std::fs;

#[cfg(target_os = "linux")]
use std::path::PathBuf;

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
#[cfg(target_os = "linux")]
pub fn process_name(pid: i32) -> Option<String> {
    let name = fs::read_to_string(proc_path(pid, "comm")).ok()?;
    let name = name.trim_end_matches('\n');
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(not(target_os = "linux"))]
pub fn process_name(_pid: i32) -> Option<String> {
    None
}

/// Ports fill this in too. Linux has it as the `/proc/<pid>/exe`
/// symlink. It matters because a program's file name is not always its
/// name: Claude Code installs its executable as the version number, so
/// the path is the only thing that says what it is.
#[cfg(target_os = "linux")]
pub fn process_path(pid: i32) -> Option<String> {
    fs::read_link(proc_path(pid, "exe"))
        .ok()?
        .into_os_string()
        .into_string()
        .ok()
        .filter(|path| !path.is_empty())
}

#[cfg(not(target_os = "linux"))]
pub fn process_path(_pid: i32) -> Option<String> {
    None
}

/// Ports fill this in too. Linux has it as `/proc/<pid>/cmdline`, which
/// is already NUL-separated and needs no unpacking. It matters because
/// most of these tools are npm packages, so the program is `node` and
/// only the arguments say which tool it is.
#[cfg(target_os = "linux")]
pub fn process_args(pid: i32) -> Option<Vec<String>> {
    parse_proc_args(&fs::read(proc_path(pid, "cmdline")).ok()?)
}

#[cfg(not(target_os = "linux"))]
pub fn process_args(_pid: i32) -> Option<Vec<String>> {
    None
}

/// Ports fill this in too. Linux has it as the `/proc/<pid>/cwd` symlink,
/// which is simpler than the macOS route. Windows has no equivalent that
/// works on another process without opening a handle to it.
///
/// Returning `None` is safe: the status bar leaves the path out rather
/// than showing a wrong one.
#[cfg(target_os = "linux")]
pub fn process_cwd(pid: i32) -> Option<String> {
    fs::read_link(proc_path(pid, "cwd"))
        .ok()?
        .into_os_string()
        .into_string()
        .ok()
        .filter(|path| !path.is_empty())
}

#[cfg(not(target_os = "linux"))]
pub fn process_cwd(_pid: i32) -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn proc_path(pid: i32, entry: &str) -> PathBuf {
    PathBuf::from(format!("/proc/{pid}/{entry}"))
}

#[cfg(target_os = "linux")]
fn parse_proc_args(mut args: &[u8]) -> Option<Vec<String>> {
    if args.last() == Some(&0) {
        args = &args[..args.len() - 1];
    }
    if args.is_empty() {
        return None;
    }

    args.split(|byte| *byte == 0)
        .map(|arg| String::from_utf8(arg.to_vec()).ok())
        .collect()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn reads_the_current_process_from_proc() {
        let pid = std::process::id() as i32;

        assert!(
            !process_name(pid)
                .expect("current process has a comm entry")
                .is_empty()
        );
        assert!(
            process_path(pid)
                .expect("current process has an exe entry")
                .starts_with('/')
        );
        assert!(
            !process_args(pid)
                .expect("current process has a cmdline entry")
                .is_empty()
        );
        assert_eq!(
            process_cwd(pid).expect("current process has a cwd entry"),
            std::env::current_dir()
                .expect("test has a working directory")
                .to_str()
                .expect("test directory is UTF-8")
        );
    }

    #[test]
    fn a_process_that_is_gone_returns_none() {
        let pid = -1;

        assert_eq!(process_name(pid), None);
        assert_eq!(process_path(pid), None);
        assert_eq!(process_args(pid), None);
        assert_eq!(process_cwd(pid), None);
    }

    #[test]
    fn parses_nul_separated_arguments_without_an_empty_tail() {
        assert_eq!(
            parse_proc_args(b"node\0tool.js\0--model\0"),
            Some(vec![
                "node".to_string(),
                "tool.js".to_string(),
                "--model".to_string()
            ])
        );
    }
}

/// Ports fill this in. Windows hides an app from the taskbar with
/// `WS_EX_TOOLWINDOW` on the window rather than with an app-wide policy,
/// and most Linux desktops read a window type hint. Doing nothing leaves
/// the app in whatever the desktop shows by default, which is the safe
/// failure: visible and ordinary.
pub fn make_accessory<R: Runtime>(_app: &AppHandle<R>) {}

/// Spaces are a macOS idea, so the window is always where the user is.
pub fn is_on_active_space<R: Runtime>(_window: &WebviewWindow<R>) -> bool {
    true
}
