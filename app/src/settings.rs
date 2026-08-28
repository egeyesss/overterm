//! OverTerm's own settings file.
//!
//! `~/.config/overterm/config.toml`, or under `$XDG_CONFIG_HOME` when that
//! is set. Small for now: it holds the things the app has to remember
//! between launches. Preferences and per-tool profiles grow into the same
//! file rather than a second one.

use std::path::{Path, PathBuf};

use overterm_core::choreo::{ChoreoConfig, Cues};
use serde::{Deserialize, Serialize};
use tauri::{Runtime, State, WebviewWindow};

use crate::choreograph::Choreographer;
use crate::platform::PlatformWindow;

/// Range the window opacity is held to.
///
/// The floor is not zero on purpose: an always-on-top window that is
/// fully transparent still sits above everything and still takes clicks,
/// so it would be a window nobody can see and nobody can get rid of.
pub const MIN_OPACITY: u8 = 10;
pub const MAX_OPACITY: u8 = 100;

// Not Copy: the terminal font is a String. Everything is cloned or moved
// explicitly instead, which is no hardship for a struct read once a save.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

    /// Whether the user has been told, once, that the app wrote into
    /// Claude Code's settings file.
    ///
    /// Backend state rather than a preference, and set only by dismissing
    /// the notice. Editing somebody's config file silently is not
    /// something a README paragraph covers on its own.
    pub claude_hooks_notice_seen: bool,

    /// How see-through the window is, as a percentage.
    pub opacity: u8,

    // Tables have to come after the plain values above: TOML puts every
    // key before the first section header, so a scalar declared after a
    // table cannot be written back out.
    pub window: WindowSettings,
    pub cues: CueSettings,
    pub terminal: TerminalSettings,
}

/// How the terminal itself is drawn. Read by the frontend, which owns the
/// terminal; nothing on this side acts on them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalSettings {
    /// A CSS font stack, so a missing font falls back rather than
    /// rendering the terminal in something proportional.
    pub font_family: String,
    pub font_size: u16,
    /// Lines kept above the top of the window.
    pub scrollback: u32,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_family: "Menlo, Monaco, \"SF Mono\", monospace".into(),
            font_size: 13,
            scrollback: 10_000,
        }
    }
}

/// What the window does when the agent's state changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowSettings {
    /// Shrink to the bar once the session is handed a job.
    pub collapse_on_submit: bool,
    /// How long it must stay busy first.
    pub collapse_delay_ms: u64,
    /// Come back when the agent finishes or asks for something.
    pub expand_when_wanted: bool,
    /// How long a collapsed session may sit still, having concluded
    /// nothing, before the terminal returns anyway.
    pub reveal_when_stalled_ms: u64,
}

/// How the app asks for attention.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CueSettings {
    pub glow: bool,
    pub sound: bool,
    pub notify: bool,
}

// Both of these take their defaults from the core crate rather than
// repeating the numbers, so the rules and the settings file cannot
// drift apart.
impl Default for WindowSettings {
    fn default() -> Self {
        let cfg = ChoreoConfig::default();
        Self {
            collapse_on_submit: cfg.collapse_on_submit,
            collapse_delay_ms: cfg.collapse_delay_ms,
            expand_when_wanted: cfg.expand_when_wanted,
            reveal_when_stalled_ms: cfg.reveal_when_stalled_ms,
        }
    }
}

impl Default for CueSettings {
    fn default() -> Self {
        let cues = Cues::default();
        Self {
            glow: cues.glow,
            sound: cues.sound,
            notify: cues.notify,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            claude_hooks_installed: false,
            claude_hooks_notice_seen: false,
            // Opaque. Anyone who wants to see through the overlay asks
            // for it; nobody should have to work out why their terminal
            // arrived faded.
            opacity: MAX_OPACITY,
            window: WindowSettings::default(),
            cues: CueSettings::default(),
            terminal: TerminalSettings::default(),
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

    /// The stored preferences as the rules engine wants them.
    pub fn choreo(&self) -> ChoreoConfig {
        ChoreoConfig {
            collapse_on_submit: self.window.collapse_on_submit,
            collapse_delay_ms: self.window.collapse_delay_ms,
            expand_when_wanted: self.window.expand_when_wanted,
            reveal_when_stalled_ms: self.window.reveal_when_stalled_ms,
            cues: Cues {
                glow: self.cues.glow,
                sound: self.cues.sound,
                notify: self.cues.notify,
            },
        }
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

#[tauri::command]
pub fn settings() -> Settings {
    load()
}

/// Record that the user has seen what the app did to their Claude Code
/// settings, so it is not said twice.
#[tauri::command]
pub fn dismiss_hooks_notice() -> Result<(), String> {
    let mut settings = load();
    if settings.claude_hooks_notice_seen {
        return Ok(());
    }
    settings.claude_hooks_notice_seen = true;
    let path = path().ok_or("no home directory to store settings in")?;
    save_to(&path, &settings)
}

/// Store new preferences and put them into effect straight away.
///
/// Returns what was actually stored, which is not always what was asked
/// for. The opacity is clamped, and whether the Claude Code hooks are in
/// place is read back off disk rather than taken from the caller: that
/// flag is a record of something already done to somebody's config file,
/// so a stale copy arriving from the interface must not rewrite it.
#[tauri::command]
pub fn save_settings<R: Runtime>(
    window: WebviewWindow<R>,
    choreo: State<'_, Choreographer>,
    settings: Settings,
) -> Result<Settings, String> {
    let stored = load();
    let opacity = settings.opacity.clamp(MIN_OPACITY, MAX_OPACITY);
    let next = Settings {
        claude_hooks_installed: stored.claude_hooks_installed,
        claude_hooks_notice_seen: stored.claude_hooks_notice_seen,
        opacity,
        ..settings
    };

    // Put into effect before it is written, so a settings file that
    // cannot be saved costs the user their next launch and not this one.
    choreo.set_config(next.choreo());
    if next.opacity != stored.opacity {
        window.set_opacity(next.alpha())?;
    }

    let path = path().ok_or("no home directory to store settings in")?;
    save_to(&path, &next)?;
    Ok(next)
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
            claude_hooks_notice_seen: true,
            opacity: 60,
            window: WindowSettings {
                collapse_on_submit: false,
                collapse_delay_ms: 900,
                ..WindowSettings::default()
            },
            cues: CueSettings {
                sound: true,
                ..CueSettings::default()
            },
            terminal: TerminalSettings {
                font_size: 16,
                ..TerminalSettings::default()
            },
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
    fn a_fresh_install_has_not_been_told_anything_yet() {
        // Both start false, so the very first launch installs the hooks
        // and then has something to say about it.
        let settings = Settings::default();
        assert!(!settings.claude_hooks_installed);
        assert!(!settings.claude_hooks_notice_seen);
    }

    #[test]
    fn the_terminal_defaults_name_a_font_stack_not_one_font() {
        // A single font name that is missing leaves the terminal in a
        // proportional face, which makes every TUI unreadable.
        let terminal = TerminalSettings::default();
        assert!(
            terminal.font_family.contains("monospace"),
            "the stack has to end somewhere safe: {}",
            terminal.font_family
        );
    }

    #[test]
    fn the_written_file_is_one_a_person_could_edit() {
        // The file is documented as hand-editable, so its shape is part
        // of the interface. It also catches the ordering trap: TOML puts
        // every bare key before the first section header, so a scalar
        // field declared after a table cannot be serialised at all.
        let text = toml::to_string_pretty(&Settings::default()).expect("serialise");
        assert!(text.contains("opacity = 100"), "got:\n{text}");
        assert!(text.contains("[window]"), "got:\n{text}");
        assert!(text.contains("[cues]"), "got:\n{text}");
        assert!(
            text.find("opacity").unwrap() < text.find("[window]").unwrap(),
            "plain values have to come before the first table:\n{text}"
        );
    }

    #[test]
    fn a_file_written_before_the_preferences_existed_still_loads() {
        // The shape v0.1.0 wrote. Everything added since has to arrive as
        // its default rather than as zero, or an upgrade would silently
        // turn the choreography off.
        let path = scratch("v0-1-0-shape");
        std::fs::write(&path, "claude_hooks_installed = true\nopacity = 100\n").expect("write");
        let settings = load_from(&path);
        assert!(settings.claude_hooks_installed);
        assert_eq!(settings.window, WindowSettings::default());
        assert_eq!(settings.cues, CueSettings::default());
        assert_eq!(settings.choreo(), ChoreoConfig::default());
    }

    #[test]
    fn a_half_written_table_keeps_the_defaults_for_the_rest() {
        let path = scratch("partial-table");
        std::fs::write(&path, "[window]\ncollapse_on_submit = false\n").expect("write");
        let settings = load_from(&path);
        assert!(!settings.window.collapse_on_submit);
        assert_eq!(
            settings.window.collapse_delay_ms,
            WindowSettings::default().collapse_delay_ms,
            "a key the user did not write must not read as zero"
        );
    }

    #[test]
    fn stored_preferences_reach_the_rules_engine() {
        let settings = Settings {
            window: WindowSettings {
                expand_when_wanted: false,
                collapse_delay_ms: 250,
                ..WindowSettings::default()
            },
            cues: CueSettings {
                glow: false,
                sound: true,
                notify: true,
            },
            ..Settings::default()
        };
        let cfg = settings.choreo();
        assert!(!cfg.expand_when_wanted);
        assert_eq!(cfg.collapse_delay_ms, 250);
        assert!(!cfg.cues.glow);
        assert!(cfg.cues.sound);
        assert!(cfg.cues.notify);
    }

    #[test]
    fn the_defaults_are_the_ones_the_rules_engine_already_had() {
        // Two copies of the same numbers would drift. This is the check
        // that they have not.
        assert_eq!(Settings::default().choreo(), ChoreoConfig::default());
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
