//! OverTerm's own settings file.
//!
//! `~/.config/overterm/config.toml`, or under `$XDG_CONFIG_HOME` when that
//! is set. Small for now: it holds the things the app has to remember
//! between launches. Preferences and per-tool profiles grow into the same
//! file rather than a second one.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
