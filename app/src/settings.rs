//! OverTerm's own settings file.
//!
//! `~/.config/overterm/config.toml`, or under `$XDG_CONFIG_HOME` when that
//! is set. Small for now: it holds the things the app has to remember
//! between launches. Preferences and per-tool profiles grow into the same
//! file rather than a second one.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{Runtime, WebviewWindow};

use crate::platform::PlatformWindow;

/// Range the window opacity is held to.
///
/// The floor is not zero on purpose: an always-on-top window that is
/// fully transparent still sits above everything and still takes clicks,
/// so it would be a window nobody can see and nobody can get rid of.
pub const MIN_OPACITY: u8 = 10;
pub const MAX_OPACITY: u8 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
// Missing fields take their default, so a file written by an older build
// still loads, and one written by a newer build loses only what this
// build does not know about.
#[serde(default)]
pub struct Settings {
    /// Whether the Claude Code hooks have been put in place already.
    ///
    /// They go in once, on the first launch that manages it, and after
    /// that the app leaves that file alone. Taking them out has to be a
    /// decision that sticks: an app that quietly puts back what someone
    /// deleted from their own config is worse than one that never
    /// offered to set it up.
    pub claude_hooks_installed: bool,

    /// How see-through the window is, as a percentage.
    pub opacity: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            claude_hooks_installed: false,
            // Opaque. Anyone who wants to see through the overlay asks
            // for it; nobody should have to work out why their terminal
            // arrived faded.
            opacity: MAX_OPACITY,
        }
    }
}

impl Settings {
    /// Opacity as the 0.0 to 1.0 the window layer wants, with a value
    /// from outside the supported range brought back into it. A settings
    /// file is a text file someone can edit by hand, so this cannot
    /// assume the number in it is sensible.
    pub fn alpha(&self) -> f64 {
        f64::from(self.opacity.clamp(MIN_OPACITY, MAX_OPACITY)) / 100.0
    }
}

/// Where the settings file lives.
pub fn path() -> Option<PathBuf> {
    let dir = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(dir.join("overterm").join("config.toml"))
}

/// Read the settings, falling back to the defaults for anything missing.
///
/// A file that cannot be read or parsed is reported and then ignored. A
/// broken config is not a reason to refuse to start, and the defaults are
/// the same ones a new install gets.
pub fn load_from(path: &Path) -> Settings {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Settings::default(),
        Err(e) => {
            eprintln!("[settings] cannot read {}: {e}", path.display());
            return Settings::default();
        }
    };
    match toml::from_str(&text) {
        Ok(settings) => settings,
        Err(e) => {
            eprintln!("[settings] ignoring {}: {e}", path.display());
            Settings::default()
        }
    }
}

/// Read the settings from wherever they live on this machine.
pub fn load() -> Settings {
    path().map(|p| load_from(&p)).unwrap_or_default()
}

/// Put the stored opacity on the window. Called once the window exists.
pub fn apply_to_window<R: Runtime>(window: &WebviewWindow<R>) {
    let settings = load();
    if settings.opacity == MAX_OPACITY {
        return; // already how a window arrives
    }
    if let Err(e) = window.set_opacity(settings.alpha()) {
        eprintln!("[settings] could not set opacity: {e}");
    }
}

/// Change how see-through the window is, and remember it.
///
/// Returns what the opacity ended up as, which is not always what was
/// asked for: the range is clamped so the window cannot be faded to the
/// point of being unfindable.
#[tauri::command]
pub fn set_window_opacity<R: Runtime>(window: WebviewWindow<R>, percent: u8) -> Result<u8, String> {
    let mut settings = load();
    settings.opacity = percent.clamp(MIN_OPACITY, MAX_OPACITY);
    window.set_opacity(settings.alpha())?;
    // The window has already changed, so a settings file that cannot be
    // written costs the user the next launch, not this one.
    let path = path().ok_or("no home directory to store settings in")?;
    save_to(&path, &settings)?;
    Ok(settings.opacity)
}

#[tauri::command]
pub fn window_opacity() -> u8 {
    load().opacity
}

pub fn save_to(path: &Path, settings: &Settings) -> Result<(), String> {
    let dir = path.parent().ok_or("settings path has no directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let text = toml::to_string_pretty(settings).map_err(|e| format!("cannot serialise: {e}"))?;
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("overterm-settings-test");
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        let path = dir.join(format!("{name}.toml"));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn a_missing_file_reads_as_the_defaults() {
        let settings = load_from(&scratch("missing"));
        assert_eq!(settings, Settings::default());
        assert!(
            !settings.claude_hooks_installed,
            "a fresh install has to believe it has not set the hooks up yet"
        );
    }

    #[test]
    fn settings_round_trip() {
        let path = scratch("round-trip");
        let settings = Settings {
            claude_hooks_installed: true,
            opacity: 60,
        };
        save_to(&path, &settings).expect("save");
        assert_eq!(load_from(&path), settings);
    }

    #[test]
    fn a_broken_file_falls_back_instead_of_refusing_to_start() {
        let path = scratch("broken");
        std::fs::write(&path, "this is not = = toml").expect("write");
        assert_eq!(load_from(&path), Settings::default());
    }

    #[test]
    fn opacity_defaults_to_opaque() {
        // A file written before this field existed must not read as a
        // window faded to nothing.
        let path = scratch("no-opacity");
        std::fs::write(&path, "claude_hooks_installed = true\n").expect("write");
        let settings = load_from(&path);
        assert_eq!(settings.opacity, MAX_OPACITY);
        assert_eq!(settings.alpha(), 1.0);
    }

    #[test]
    fn a_hand_edited_opacity_is_brought_back_into_range() {
        for (written, expected) in [(0, 0.1), (5, 0.1), (10, 0.1), (55, 0.55), (200, 1.0)] {
            let settings = Settings {
                opacity: written,
                ..Settings::default()
            };
            assert!(
                (settings.alpha() - expected).abs() < f64::EPSILON,
                "opacity {written} gave {} rather than {expected}",
                settings.alpha()
            );
        }
    }

    #[test]
    fn a_file_from_another_version_keeps_what_this_one_understands() {
        let path = scratch("other-version");
        std::fs::write(
            &path,
            "claude_hooks_installed = true\nsomething_from_later = 3\n",
        )
        .expect("write");
        assert!(load_from(&path).claude_hooks_installed);
    }
}
