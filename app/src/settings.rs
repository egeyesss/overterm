//! OverTerm's own settings file.
//!
//! `~/.config/overterm/config.toml`, or under `$XDG_CONFIG_HOME` when that
//! is set. Small for now: it holds the things the app has to remember
//! between launches. Preferences and per-tool profiles grow into the same
//! file rather than a second one.

use std::path::{Path, PathBuf};

use overterm_core::choreo::{ChoreoConfig, Cues};
use overterm_core::detect::Profile;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime, State, WebviewWindow};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

use crate::choreograph::Choreographer;
use crate::platform::PlatformWindow;

/// Range the window opacity is held to.
///
/// The floor is not zero on purpose: an always-on-top window that is
/// fully transparent still sits above everything and still takes clicks,
/// so it would be a window nobody can see and nobody can get rid of.
pub const MIN_OPACITY: u8 = 10;
pub const MAX_OPACITY: u8 = 100;

/// The summon chord a fresh install gets.
pub const DEFAULT_HOTKEY: &str = "CmdOrCtrl+Shift+O";

/// How many one-time fixes have been written for the settings file.
///
/// Bumped whenever a stored value has to be corrected for people who
/// already have a file, which a changed default cannot do on its own:
/// saving writes every field, so the old default is already sitting in
/// everybody's config as though they had chosen it.
pub const SETTINGS_VERSION: u32 = 1;

/// What a file written before the version field existed reads as.
///
/// The struct carries `serde(default)`, so without this a missing key
/// would take the value from `Settings::default()`, which is the current
/// version. Every old file would claim to be migrated already.
fn unversioned() -> u32 {
    0
}

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

    /// Chord that summons or hides the window from any application.
    pub hotkey: String,

    /// Which theme to draw, or to follow the machine's own setting.
    pub theme: Theme,

    /// Which one-time fixes have already been applied to this file.
    ///
    /// Backend state rather than a preference: the UI never sends it and
    /// `save_settings` stamps the current value on the way out.
    #[serde(default = "unversioned")]
    pub settings_version: u32,

    // Tables have to come after the plain values above: TOML puts every
    // key before the first section header, so a scalar declared after a
    // table cannot be written back out.
    pub window: WindowSettings,
    pub cues: CueSettings,
    pub terminal: TerminalSettings,

    /// Extra agents, keyed on the name of the program running in a
    /// session. Empty by default and merged over the built-in list below,
    /// so adding one of your own does not silently drop the ones that
    /// ship with the app.
    pub agents: Vec<AgentProfile>,
}

/// Size the expanded window starts at, in logical pixels.
pub const DEFAULT_PANEL_WIDTH: u32 = 660;
pub const DEFAULT_PANEL_HEIGHT: u32 = 620;

/// Smallest the expanded window may be set to.
///
/// A terminal narrower than this wraps every agent's interface into
/// nonsense, and one shorter shows too little to be worth expanding into.
pub const MIN_PANEL_WIDTH: u32 = 400;
pub const MIN_PANEL_HEIGHT: u32 = 200;

/// Clamp a requested window size to something usable.
pub fn sane_panel_size(width: u32, height: u32) -> (u32, u32) {
    (width.max(MIN_PANEL_WIDTH), height.max(MIN_PANEL_HEIGHT))
}

/// Light, dark, or whatever the machine is set to.
///
/// Following the system is the default, because an overlay that sits on
/// top of everything else looks wrong when it is the only bright thing on
/// a dark desktop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
    #[default]
    System,
}

/// What OverTerm knows about one command it might find in a session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfile {
    /// Process name to match, as the operating system reports it.
    #[serde(rename = "match")]
    pub program: String,
    /// What the tab says when it has no icon. Kept short: the rail is
    /// narrow, and anything long is cut off.
    pub label: String,

    /// A single character to show instead of the label. The rail has room
    /// for one glyph and not for a word.
    #[serde(default)]
    pub icon: Option<String>,

    /// Colour for that glyph, as CSS. Left out means the ordinary text
    /// colour.
    #[serde(default)]
    pub color: Option<String>,

    /// What this program puts on screen for as long as it is working.
    ///
    /// The one that matters most. Between batches of output an agent's
    /// cursor rests in its input box and the screen looks exactly like a
    /// finished prompt, so without something to hold the state the window
    /// concludes the turn ended and comes back mid-answer. Claude Code
    /// shows "esc to interrupt"; Gemini CLI shows "esc to cancel".
    ///
    /// Left out means the built-in default, which is what a plain shell
    /// wants.
    #[serde(default)]
    pub busy_pattern: Option<String>,

    /// What a row looks like when this program is waiting for input.
    #[serde(default)]
    pub prompt_pattern: Option<String>,
}

/// One of the agents that ships with the app.
///
/// Named fields rather than a tuple because there are two optional
/// patterns at the end now, and two `Option<&str>` in a row is somewhere
/// a colour and a regex can be swapped without the compiler noticing.
struct Builtin {
    /// Name to look for in the program's path, arguments and process name.
    program: &'static str,
    label: &'static str,
    /// The colour the tool draws itself in, so a tab reads as the same
    /// thing as the terminal underneath it.
    color: &'static str,
    /// What the program puts on screen for as long as it is working.
    busy_pattern: Option<&'static str>,
    /// What a row looks like when it is waiting for input. Only needed
    /// for a tool whose input row the built-in default does not describe.
    prompt_pattern: Option<&'static str>,
}

/// The agents that ship with the app.
///
/// Deliberately short. Anything else is two lines in a config file, and
/// guessing at the process names of tools nobody here has run is how you
/// end up shipping a mapping that is quietly wrong.
///
/// A pattern is only filled in where the wording is actually known, and
/// every one of them here came off a recording. Guessing one is worse
/// than leaving it out: a pattern that never matches reads as support
/// while the window quietly decides every turn ended early. Anything left
/// as `None` falls back to the default and can be filled in from a config
/// file by whoever runs that tool.
const BUILTIN_AGENTS: &[Builtin] = &[
    Builtin {
        program: "claude",
        label: "Claude",
        color: "#d97757",
        busy_pattern: Some("esc to interrupt"),
        prompt_pattern: None,
    },
    Builtin {
        program: "gemini",
        label: "Gemini",
        color: "#4285f4",
        // Wording taken from a real transcript, which reads
        // "Awaiting Further Direction (esc to cancel, 40s)".
        busy_pattern: Some("esc to cancel"),
        prompt_pattern: None,
    },
    Builtin {
        program: "codex",
        label: "Codex",
        color: "#10a37f",
        busy_pattern: None,
        prompt_pattern: None,
    },
    Builtin {
        program: "aider",
        label: "Aider",
        color: "#14b8a6",
        busy_pattern: None,
        prompt_pattern: None,
    },
    Builtin {
        program: "opencode",
        label: "opencode",
        color: "#f59e0b",
        busy_pattern: None,
        prompt_pattern: None,
    },
    Builtin {
        program: "kimi",
        label: "Kimi",
        color: "#1f1f1f",
        busy_pattern: None,
        prompt_pattern: None,
    },
    Builtin {
        program: "ollama",
        label: "Ollama",
        color: "#c8c8c8",
        busy_pattern: None,
        prompt_pattern: None,
    },
    Builtin {
        program: "antigravity",
        label: "Antigravity",
        color: "#4285f4",
        busy_pattern: None,
        prompt_pattern: None,
    },
    Builtin {
        program: "pi",
        label: "Pi",
        // Pi's own mark is near black on light and white on dark. The
        // mark inherits this colour, and the dark theme's background is
        // #101012, so a neutral that reads on both surfaces is worth more
        // here than the exact brand value.
        color: "#a1a1aa",
        // Its streaming indicator, a braille spinner and this word, which
        // it keeps up for as long as a turn runs.
        busy_pattern: Some(r"Working\.\.\."),
        // Pi has no prompt character. Its input box is an empty row
        // between two full-width rules, so the rule above the cursor is
        // what says the box is there and ready. Without this the default
        // pattern finds nothing to match, quiescence never concludes, and
        // a Pi session sits at Busy for the rest of its life.
        prompt_pattern: Some(r"^\u{2500}+\s*$"),
    },
];

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

/// The font the design asks for, shipped with the app rather than assumed
/// to be installed.
pub const DEFAULT_FONT: &str = "'IBM Plex Mono', Menlo, Monaco, monospace";

/// What the default used to be, before the design.
///
/// A settings file written by an older build has this recorded, because
/// saving writes every field whether or not anybody chose it. Somebody who
/// picked their own font keeps it; somebody who only ever accepted the
/// default should get the new one rather than being pinned to a choice they
/// never made.
const SUPERSEDED_FONT: &str = "Menlo, Monaco, \"SF Mono\", monospace";

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_family: DEFAULT_FONT.into(),
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

    /// Size of the expanded window, in logical pixels.
    ///
    /// Written back when the window is resized by dragging it, so the size
    /// somebody settles on is the one they get next launch. Collapsing
    /// does not touch it: the bar has a size of its own.
    pub panel_width: u32,
    pub panel_height: u32,
}

/// How the app asks for attention.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CueSettings {
    pub glow: bool,
    pub sound: bool,
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
            panel_width: DEFAULT_PANEL_WIDTH,
            panel_height: DEFAULT_PANEL_HEIGHT,
        }
    }
}

impl Default for CueSettings {
    fn default() -> Self {
        let cues = Cues::default();
        Self {
            glow: cues.glow,
            sound: cues.sound,
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
            hotkey: DEFAULT_HOTKEY.into(),
            theme: Theme::default(),
            settings_version: SETTINGS_VERSION,
            window: WindowSettings::default(),
            cues: CueSettings::default(),
            terminal: TerminalSettings::default(),
            agents: Vec::new(),
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

    /// What to call a session running `program`.
    ///
    /// The user's own entries are checked first so one of them can
    /// correct a built-in that has gone stale, and an unknown program is
    /// its own label: a session sitting at a shell should say `zsh`
    /// rather than nothing at all.
    /// Most of these tools are npm packages, so the program on the
    /// terminal is `node` and the only thing naming the tool is the
    /// script path it was handed. `args` is that command line; an empty
    /// one just means the executable has to speak for itself.
    ///
    /// `name` is what the process calls itself, for the tools that leave
    /// nothing else to go on. See `match_command`.
    pub fn label_for(&self, path: &str, args: &[String], name: Option<&str>) -> Agent {
        match self.match_command(path, args, name) {
            Some(agent) => agent,
            // Not an agent anybody has described, so it speaks for
            // itself. A tab saying zsh is more use than a blank one.
            None => Agent {
                id: String::new(),
                label: basename(path).to_string(),
                icon: None,
                color: None,
                cwd: None,
            },
        }
    }

    /// Try the executable, then the arguments it was given, then what the
    /// process calls itself.
    ///
    /// In that order on purpose: a real binary naming itself is better
    /// evidence than a path that happens to appear on a command line,
    /// which in turn is better than a name a process chose for itself.
    /// Only arguments that look like paths are considered, so a prompt
    /// typed on the command line cannot rename the tab.
    ///
    /// The name is the last resort and the only thing that works for a
    /// tool that sets its own process title. Node does that through
    /// `process.title`, and on macOS setting it overwrites the whole
    /// argument block, so a tool like Pi that sets it arrives here as the
    /// path to `node` and a command line with nothing left in it. The
    /// name is then the one place the tool is still named.
    fn match_command(&self, path: &str, args: &[String], name: Option<&str>) -> Option<Agent> {
        self.match_program(path)
            .or_else(|| {
                args.iter()
                    .skip(1)
                    .filter(|arg| arg.contains('/'))
                    .find_map(|arg| self.match_program(arg))
            })
            .or_else(|| name.and_then(|name| self.match_program(name)))
    }

    /// Find the profile for an executable path, if there is one.
    ///
    /// The file name is tried first, then the directories above it,
    /// nearest first. That second pass is not tidiness: Claude Code
    /// installs its executable as the bare version number, so the file is
    /// called `2.1.250` and the only thing that says what it is is the
    /// `claude` directory holding it.
    fn match_program(&self, path: &str) -> Option<Agent> {
        // Walked from the file name outwards and stopped at the first
        // hit, so a match close to the executable beats one further up.
        // Bounded so this cannot reach out into a home directory that
        // happens to share a name with an agent.
        let candidates = path
            .rsplit('/')
            .filter(|part| !part.is_empty())
            .take(MATCH_DEPTH);

        for part in candidates {
            let part = trim_suffixes(part);
            if let Some(user) = self.agents.iter().find(|a| a.program == part) {
                return Some(Agent {
                    id: user.program.clone(),
                    label: user.label.clone(),
                    icon: user.icon.clone(),
                    color: user.color.clone(),
                    cwd: None,
                });
            }
            if let Some(builtin) = BUILTIN_AGENTS.iter().find(|b| b.program == part) {
                return Some(Agent {
                    id: builtin.program.to_string(),
                    label: builtin.label.to_string(),
                    icon: None,
                    color: Some(builtin.color.to_string()),
                    cwd: None,
                });
            }
        }
        None
    }

    /// The screen patterns to look for while `program` is running.
    ///
    /// A program nobody has a profile for gets an empty profile, which
    /// means the built-in defaults. That is the right answer for a shell
    /// and a guess for anything else, which is what a profile is for.
    pub fn profile_for(&self, path: &str, args: &[String], name: Option<&str>) -> Profile {
        let described = |p: &Profile| p.busy_pattern.is_some() || p.prompt_pattern.is_some();
        let direct = self.profile_from_path(path);
        if described(&direct) {
            return direct;
        }
        // Same order as `match_command`, so the profile a session gets is
        // always the one belonging to the agent its tab is named after.
        args.iter()
            .skip(1)
            .filter(|arg| arg.contains('/'))
            .map(|arg| self.profile_from_path(arg))
            .find(described)
            .or_else(|| {
                name.map(|name| self.profile_from_path(name))
                    .filter(described)
            })
            .unwrap_or_default()
    }

    fn profile_from_path(&self, path: &str) -> Profile {
        let parts: Vec<&str> = path
            .rsplit('/')
            .filter(|part| !part.is_empty())
            .take(MATCH_DEPTH)
            .collect();

        for part in &parts {
            let part = trim_suffixes(part);
            if let Some(user) = self.agents.iter().find(|a| a.program == part) {
                return Profile {
                    busy_pattern: compile(user.busy_pattern.as_deref(), "busy_pattern", part),
                    prompt_pattern: compile(user.prompt_pattern.as_deref(), "prompt_pattern", part),
                };
            }
            if let Some(builtin) = BUILTIN_AGENTS.iter().find(|b| b.program == part) {
                return Profile {
                    busy_pattern: compile(builtin.busy_pattern, "busy_pattern", part),
                    prompt_pattern: compile(builtin.prompt_pattern, "prompt_pattern", part),
                };
            }
        }
        Profile::default()
    }

    /// The stored preferences as the rules engine wants them.
    /// Move a setting nobody chose onto its new default.
    ///
    /// Saving writes every field, so a file written by an older build
    /// records the old default as though it were a decision. Only an exact
    /// match is replaced, which leaves anybody who picked their own font
    /// alone.
    fn retire_superseded_defaults(&mut self) {
        if self.terminal.font_family == SUPERSEDED_FONT {
            self.terminal.font_family = DEFAULT_FONT.into();
        }
    }

    /// Correct stored values that an older build got wrong.
    ///
    /// Changing a default only reaches new installs. Saving writes every
    /// field, so anybody who ran the older build has the old default in
    /// their file already and would never see the fix.
    fn apply_migrations(&mut self) {
        // Version 1 turned a Dock icon back off for the people 1.0.0 had
        // given one. There is no setting to fix any more: the field is
        // gone and an old file's `show_in_dock` key is ignored on the way
        // in and dropped on the way out. The stamp stays because the next
        // migration needs a number to compare against.
        self.settings_version = SETTINGS_VERSION;
    }

    pub fn choreo(&self) -> ChoreoConfig {
        ChoreoConfig {
            collapse_on_submit: self.window.collapse_on_submit,
            collapse_delay_ms: self.window.collapse_delay_ms,
            expand_when_wanted: self.window.expand_when_wanted,
            reveal_when_stalled_ms: self.window.reveal_when_stalled_ms,
            cues: Cues {
                glow: self.cues.glow,
                sound: self.cues.sound,
            },
        }
    }
}

/// Read a chord, refusing the ones that would make the machine unusable.
///
/// A chord with no modifier registers globally and then swallows that key
/// in every other application, so a settings file saying `hotkey = "o"`
/// would cost somebody the letter o until they worked out why.
pub fn parse_hotkey(chord: &str) -> Result<Shortcut, String> {
    let shortcut: Shortcut = chord
        .parse()
        .map_err(|_| format!("{chord} is not a chord this build understands"))?;
    if shortcut.mods.is_empty() {
        return Err(format!(
            "{chord} has no modifier, so it would swallow that key everywhere"
        ));
    }
    Ok(shortcut)
}

/// The stored chord, or the default if what is stored cannot be used.
///
/// Never fails: a settings file with nonsense in it should cost the user
/// their chosen shortcut, not the ability to start the app.
pub fn hotkey_or_default(chord: &str) -> Shortcut {
    parse_hotkey(chord).unwrap_or_else(|e| {
        eprintln!("[settings] {e}; using {DEFAULT_HOTKEY}");
        parse_hotkey(DEFAULT_HOTKEY).expect("the built-in default parses")
    })
}

/// How far up an executable's path to look for a name we know.
///
/// Four is enough for the versioned layout Claude Code uses and short
/// enough that it cannot reach a home directory: matching `claude` in
/// `/Users/claude/bin/thing` would name the wrong program.
const MATCH_DEPTH: usize = 4;

/// What a tab shows for a session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Agent {
    /// The matched program name, or empty when nothing matched. The
    /// frontend keys its drawn marks off this, so renaming an agent's
    /// label in the settings file does not cost it its icon.
    pub id: String,
    pub label: String,
    /// One glyph, used when there is no drawn mark for this id.
    pub icon: Option<String>,
    /// Colour for the icon, as CSS.
    pub color: Option<String>,

    /// Where this session currently is, filled in by whoever knows the
    /// process rather than by the lookup that names it. `None` means the
    /// question could not be answered, and the status bar then shows no
    /// path rather than a stale one.
    #[serde(default)]
    pub cwd: Option<String>,
}

/// Reduce a path component to the name of the tool inside it.
///
/// npm packages are published as `gemini-cli` and installed into a
/// directory of that name, and the same convention gives `something-cli`
/// and `something-code`. Matching only the exact name would miss every
/// one of them.
fn trim_suffixes(part: &str) -> &str {
    for suffix in ["-cli", "-code", "-cli.js", ".js"] {
        if let Some(stem) = part.strip_suffix(suffix)
            && !stem.is_empty()
        {
            return stem;
        }
    }
    part
}

fn basename(path: &str) -> &str {
    path.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

/// Compile a pattern from the settings file, reporting a bad one once.
///
/// A pattern that does not compile falls back to the default rather than
/// stopping anything. These come from a file people edit by hand, and a
/// typo in one should cost that agent its profile, not the session.
fn compile(pattern: Option<&str>, field: &str, program: &str) -> Option<Regex> {
    let pattern = pattern?;
    match Regex::new(pattern) {
        Ok(regex) => Some(regex),
        Err(e) => {
            eprintln!(
                "[settings] {program} {field} is not a valid pattern ({e}); using the default"
            );
            None
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
    match toml::from_str::<Settings>(&text) {
        Ok(mut settings) => {
            settings.retire_superseded_defaults();
            settings.apply_migrations();
            settings
        }
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

    // The size somebody settled on last time, whether they typed it or
    // dragged the window to it.
    let (width, height) =
        sane_panel_size(settings.window.panel_width, settings.window.panel_height);
    if let Err(e) = window.set_size(tauri::LogicalSize::new(width as f64, height as f64)) {
        eprintln!("[settings] could not restore the window size: {e}");
    }

    if settings.opacity == MAX_OPACITY {
        return; // already how a window arrives
    }
    if let Err(e) = window.set_opacity(settings.alpha()) {
        eprintln!("[settings] could not set opacity: {e}");
    }
}

/// What a size command hands back, so the sheet can show what it got
/// rather than what it asked for.
#[derive(Clone, Copy, Serialize)]
pub struct PanelSize {
    pub width: u32,
    pub height: u32,
}

/// Set the expanded window's size from the settings sheet.
#[tauri::command]
pub fn set_panel_size<R: Runtime>(
    app: AppHandle<R>,
    choreo: State<'_, Choreographer>,
    width: u32,
    height: u32,
) -> PanelSize {
    let (width, height) = sane_panel_size(width, height);
    choreo.set_panel_size(&app, width as f64, height as f64);
    save_panel_size(width, height);
    PanelSize { width, height }
}

/// Sizes worth having a button for.
///
/// Large is measured against the screen rather than written down, because
/// "most of the height" means nothing without knowing the screen. It takes
/// half the width on purpose: this is a window you keep beside your work,
/// so covering all of it would defeat the point.
#[tauri::command]
pub fn size_preset<R: Runtime>(
    app: AppHandle<R>,
    choreo: State<'_, Choreographer>,
    name: String,
) -> Result<PanelSize, String> {
    let window = app
        .get_webview_window(crate::MAIN_WINDOW)
        .ok_or("there is no main window")?;
    let (screen_w, screen_h) = screen_size(&window);

    let (width, height) = match name.as_str() {
        "small" => (520.0, 380.0),
        "medium" => (DEFAULT_PANEL_WIDTH as f64, DEFAULT_PANEL_HEIGHT as f64),
        "large" => (screen_w / 2.0, screen_h * 0.92),
        other => return Err(format!("no size called {other}")),
    };

    let (width, height) = sane_panel_size(width.round() as u32, height.round() as u32);
    choreo.set_panel_size(&app, width as f64, height as f64);
    save_panel_size(width, height);
    Ok(PanelSize { width, height })
}

/// The screen this window is on, in logical pixels. Falls back to a
/// common laptop size, since a preset is better than an error here.
fn screen_size<R: Runtime>(window: &WebviewWindow<R>) -> (f64, f64) {
    match window.current_monitor() {
        Ok(Some(monitor)) => {
            let scale = monitor.scale_factor();
            let size = monitor.size();
            (size.width as f64 / scale, size.height as f64 / scale)
        }
        _ => (1440.0, 900.0),
    }
}

/// Store the expanded window's size.
///
/// Read from disk and written straight back rather than taking whatever
/// the interface last held, because a size arrives from dragging a window
/// edge and a sheet somebody has open must not be able to undo the rest of
/// their settings with it.
pub fn save_panel_size(width: u32, height: u32) {
    let Some(path) = path() else { return };
    let mut settings = load_from(&path);
    let (width, height) = sane_panel_size(width, height);
    if settings.window.panel_width == width && settings.window.panel_height == height {
        return;
    }
    settings.window.panel_width = width;
    settings.window.panel_height = height;
    if let Err(e) = save_to(&path, &settings) {
        eprintln!("[settings] could not store the window size: {e}");
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

/// Take a new summon chord, or explain why it cannot be used.
///
/// The old one is released first. Registering the new one can still fail
/// because another application already holds it, and in that case the old
/// chord is put back rather than leaving the window with no way to be
/// summoned at all.
#[tauri::command]
pub fn set_hotkey<R: Runtime>(app: AppHandle<R>, hotkey: String) -> Result<String, String> {
    let wanted = parse_hotkey(&hotkey)?;
    let mut settings = load();
    let previous = hotkey_or_default(&settings.hotkey);

    let shortcuts = app.global_shortcut();
    let _ = shortcuts.unregister(previous);
    if let Err(e) = shortcuts.register(wanted) {
        let _ = shortcuts.register(previous);
        return Err(format!(
            "{hotkey} could not be registered ({e}). Another application is probably holding it."
        ));
    }

    settings.hotkey = hotkey;
    let path = path().ok_or("no home directory to store settings in")?;
    save_to(&path, &settings)?;
    Ok(settings.hotkey)
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
        // Changing this has to register with the OS and can fail, so it
        // goes through set_hotkey and never through a bulk save.
        hotkey: stored.hotkey,
        // Stamped here rather than taken from the frontend. A save that
        // arrived without it would deserialise as unversioned and every
        // migration would run again on the next launch.
        settings_version: SETTINGS_VERSION,
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
            hotkey: "CmdOrCtrl+Shift+K".into(),
            opacity: 60,
            theme: Theme::Light,
            settings_version: SETTINGS_VERSION,
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
            agents: vec![AgentProfile {
                program: "kimi".into(),
                label: "K3".into(),
                icon: None,

                color: None,
                busy_pattern: Some("esc to stop".into()),
                prompt_pattern: None,
            }],
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
    fn a_known_agent_gets_its_name_and_anything_else_gets_its_own() {
        let settings = Settings::default();
        assert_eq!(
            settings.label_for("/usr/local/bin/claude", &[], None).label,
            "Claude"
        );
        // A shell is not an agent, and a tab saying zsh is more use than
        // a tab saying nothing.
        assert_eq!(settings.label_for("/bin/zsh", &[], None).label, "zsh");
    }

    #[test]
    fn adding_an_agent_does_not_drop_the_built_in_ones() {
        // The trap this is guarding: a list in a config file replaces the
        // default list wholesale, so somebody adding one entry would have
        // quietly lost Claude.
        let settings = Settings {
            agents: vec![AgentProfile {
                program: "kimi".into(),
                label: "K3".into(),
                icon: None,

                color: None,
                busy_pattern: None,
                prompt_pattern: None,
            }],
            ..Settings::default()
        };
        assert_eq!(settings.label_for("/opt/kimi", &[], None).label, "K3");
        assert_eq!(
            settings.label_for("/usr/bin/claude", &[], None).label,
            "Claude"
        );
    }

    #[test]
    fn a_users_entry_can_correct_a_built_in_one() {
        let settings = Settings {
            agents: vec![AgentProfile {
                program: "claude".into(),
                label: "CC".into(),
                icon: None,

                color: None,
                busy_pattern: None,
                prompt_pattern: None,
            }],
            ..Settings::default()
        };
        assert_eq!(settings.label_for("/usr/bin/claude", &[], None).label, "CC");
    }

    #[test]
    fn the_built_in_claude_profile_knows_what_it_looks_like_working() {
        let profile = Settings::default().profile_for("/usr/bin/claude", &[], None);
        let busy = profile.busy_pattern.expect("claude ships with one");
        assert!(busy.is_match("  esc to interrupt  "));
    }

    #[test]
    fn a_shell_gets_no_patterns_of_its_own() {
        // Nothing to hold: a shell has no status line saying it is busy,
        // and claiming otherwise would be worse than saying nothing.
        let profile = Settings::default().profile_for("/bin/zsh", &[], None);
        assert!(profile.busy_pattern.is_none());
        assert!(profile.prompt_pattern.is_none());
    }

    #[test]
    fn an_agent_of_your_own_gets_its_pattern_used() {
        let settings = Settings {
            agents: vec![AgentProfile {
                program: "gemini".into(),
                label: "Gemini".into(),
                icon: None,

                color: None,
                busy_pattern: Some("esc to cancel".into()),
                prompt_pattern: None,
            }],
            ..Settings::default()
        };
        let profile = settings.profile_for("/usr/local/bin/gemini", &[], None);
        let busy = profile.busy_pattern.expect("configured");
        assert!(busy.is_match("Awaiting Further Direction (esc to cancel, 40s)"));
        assert!(
            !busy.is_match("esc to interrupt"),
            "the other agent's wording must not match"
        );
    }

    #[test]
    fn a_pattern_that_does_not_compile_costs_the_profile_and_nothing_else() {
        let settings = Settings {
            agents: vec![AgentProfile {
                program: "broken".into(),
                label: "Broken".into(),
                icon: None,

                color: None,
                busy_pattern: Some("(unclosed".into()),
                prompt_pattern: None,
            }],
            ..Settings::default()
        };
        // Falls back rather than panicking: these come from a file people
        // edit by hand, and a typo should cost that agent its profile
        // rather than the session it is running in.
        assert!(
            settings
                .profile_for("/bin/broken", &[], None)
                .busy_pattern
                .is_none()
        );
        assert_eq!(settings.label_for("/bin/broken", &[], None).label, "Broken");
    }

    #[test]
    fn an_executable_named_after_its_version_is_still_recognised() {
        // The real layout on a machine with Claude Code installed. The
        // executable is the bare version number, so the process is called
        // 2.1.250 and the tab said so until this looked at the path.
        let settings = Settings::default();
        let path = "/Users/someone/.local/share/claude/versions/2.1.250";
        let agent = settings.label_for(path, &[], None);
        assert_eq!(agent.label, "Claude");
        assert_eq!(agent.id, "claude", "the id is what the drawn mark keys off");
        assert!(agent.color.is_some(), "a known agent gets its own colour");
        assert!(
            settings.profile_for(path, &[], None).busy_pattern.is_some(),
            "and its detection patterns, which the version name would have missed"
        );
    }

    #[test]
    fn the_pi_profile_gets_a_recorded_turn_back_to_the_user() {
        // A fixture replay, which normally lives in the core crate. It
        // cannot: the profile being tested is written here, and core does
        // not depend on the app. Testing the patterns against a recording
        // anywhere else would mean writing a second copy of them.
        //
        // Recording: pi 0.84.4, a prompt submitted at 3s, the turn ending
        // on a provider error a second later. An error is still a turn
        // ending, and it is the case where getting the terminal back
        // matters most, since the answer on screen is one somebody has to
        // read and act on.
        use overterm_core::detect::heuristic::{HeuristicAdapter, HeuristicConfig};
        use overterm_core::detect::replay::{read_fixture, replay};
        use overterm_core::{AgentState, Detector};

        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../crates/core/fixtures/pi-turn.ndjson");
        let events = read_fixture(&path).expect("the fixture parses");

        let with_profile = |profile: Option<Profile>| {
            let mut detector = Detector::new(vec![Box::new(HeuristicAdapter::new(
                HeuristicConfig::default(),
            ))]);
            if let Some(profile) = profile {
                detector.set_profile(&profile);
            }
            replay(&mut detector, &events, 100, 1000)
                .into_iter()
                .map(|(_, change)| change.to)
                .collect::<Vec<_>>()
        };

        // Without the profile the default prompt pattern finds nothing to
        // match on Pi's input box, so the session goes Busy on the
        // startup banner and stays there. This is the failure the profile
        // is here to fix, and pinning it is what stops the assertion
        // below passing for the wrong reason.
        let bare = with_profile(None);
        assert_eq!(
            bare,
            vec![AgentState::Busy],
            "a session with no profile should get stuck busy: {bare:?}"
        );

        let states = with_profile(Some(Settings::default().profile_for(
            "/usr/local/bin/pi",
            &[],
            None,
        )));
        assert!(
            states.contains(&AgentState::Busy),
            "the turn has to read as work happening: {states:?}"
        );
        assert_eq!(
            states.last(),
            Some(&AgentState::Done),
            "the turn ended and the terminal has to come back: {states:?}"
        );
    }

    #[test]
    fn the_agents_that_ship_are_all_usable() {
        let settings = Settings::default();
        for builtin in BUILTIN_AGENTS {
            let program = builtin.program;
            let agent = settings.label_for(&format!("/usr/local/bin/{program}"), &[], None);
            assert_eq!(agent.label, builtin.label);
            assert_eq!(agent.id, program, "the id keys the drawn mark");
            assert!(agent.color.is_some(), "{program} has no colour");
            // An icon is the single glyph a config file can set instead
            // of a label, and nothing that ships uses one. Callers asking
            // "did this lookup find anything" have to read the id: the
            // icon says no for every agent in this table.
            assert!(
                agent.icon.is_none(),
                "{program} has an icon, so the id is no longer the only \
                 way to tell a match from a miss"
            );
        }
    }

    #[test]
    fn every_pattern_that_ships_compiles() {
        // These are written by hand in the table above, so a typo would
        // otherwise only turn up as an agent silently losing its profile.
        let settings = Settings::default();
        for builtin in BUILTIN_AGENTS {
            let program = builtin.program;
            let profile = settings.profile_for(&format!("/usr/local/bin/{program}"), &[], None);
            assert_eq!(
                profile.busy_pattern.is_some(),
                builtin.busy_pattern.is_some(),
                "{program} busy pattern did not survive being compiled"
            );
            assert_eq!(
                profile.prompt_pattern.is_some(),
                builtin.prompt_pattern.is_some(),
                "{program} prompt pattern did not survive being compiled"
            );
        }
    }

    #[test]
    fn a_tool_running_through_node_is_named_after_the_tool() {
        // The case that made every npm-installed agent show up as "node":
        // the program on the terminal is the interpreter, and only the
        // script path it was handed says which tool it is.
        let settings = Settings::default();
        let args = [
            "node".to_string(),
            "/Users/someone/.nvm/versions/node/v22.3.0/lib/node_modules/@google/gemini-cli/dist/index.js"
                .to_string(),
        ];
        let agent = settings.label_for("/usr/local/bin/node", &args, None);
        assert_eq!(agent.label, "Gemini");
        assert_eq!(agent.id, "gemini");
        assert!(
            settings
                .profile_for("/usr/local/bin/node", &args, None)
                .busy_pattern
                .is_some(),
            "it needs its detection patterns as well as its name"
        );
    }

    #[test]
    fn a_tool_that_renamed_its_own_process_is_still_found() {
        // A Node tool that sets `process.title` leaves nothing else to go
        // on. On macOS setting it overwrites the whole argument block, so
        // the path is the interpreter's, the command line is down to the
        // one rewritten entry, and the name is the only thing left that
        // says which tool this is. Without the name it reads as "node".
        let settings = Settings {
            agents: vec![AgentProfile {
                program: "pi".into(),
                label: "Pi".into(),
                icon: None,
                color: Some("#a1a1aa".into()),
                busy_pattern: Some("Working".into()),
                prompt_pattern: None,
            }],
            ..Settings::default()
        };
        let path = "/Users/someone/.nvm/versions/node/v24.20.0/bin/node";
        let args = ["pi".to_string()];

        assert_eq!(settings.label_for(path, &args, None).label, "node");
        let agent = settings.label_for(path, &args, Some("pi"));
        assert_eq!(agent.label, "Pi");
        assert_eq!(agent.id, "pi", "the id is what the drawn mark keys off");
        assert!(
            settings
                .profile_for(path, &args, Some("pi"))
                .busy_pattern
                .is_some(),
            "the profile has to follow the name, or the tab is right and \
             the detection is still guessing"
        );
    }

    #[test]
    fn a_name_never_outranks_a_path_or_an_argument() {
        // A process picks its own title, so it is the weakest evidence
        // here and must not be able to rename a session that something
        // more solid already identified.
        let settings = Settings::default();
        assert_eq!(
            settings
                .label_for("/usr/bin/claude", &[], Some("gemini"))
                .label,
            "Claude"
        );
        let args = ["node".to_string(), "/opt/gemini-cli/index.js".to_string()];
        assert_eq!(
            settings
                .label_for("/usr/local/bin/node", &args, Some("codex"))
                .label,
            "Gemini"
        );
    }

    #[test]
    fn a_real_binary_outranks_anything_on_the_command_line() {
        let settings = Settings::default();
        let args = ["claude".to_string(), "/tmp/gemini/notes".to_string()];
        assert_eq!(
            settings.label_for("/usr/bin/claude", &args, None).label,
            "Claude"
        );
    }

    #[test]
    fn an_argument_that_is_not_a_path_cannot_rename_a_tab() {
        // Somebody typing an agent's name into a prompt should not make
        // the tab claim to be running it.
        let settings = Settings::default();
        let args = ["node".to_string(), "tell me about gemini".to_string()];
        assert_eq!(
            settings.label_for("/usr/local/bin/node", &args, None).label,
            "node"
        );
    }

    #[test]
    fn the_npm_naming_convention_still_matches() {
        let settings = Settings::default();
        for path in [
            "/opt/node_modules/gemini-cli/index.js",
            "/opt/@google/gemini-cli/dist/cli.js",
        ] {
            assert_eq!(
                settings.label_for(path, &[], None).label,
                "Gemini",
                "for {path}"
            );
        }
    }

    #[test]
    fn the_name_nearest_the_executable_wins() {
        let settings = Settings {
            agents: vec![AgentProfile {
                program: "inner".into(),
                label: "Inner".into(),
                icon: None,

                color: None,
                busy_pattern: None,
                prompt_pattern: None,
            }],
            ..Settings::default()
        };
        assert_eq!(
            settings.label_for("/opt/claude/inner/run", &[], None).label,
            "Inner"
        );
    }

    #[test]
    fn a_home_directory_sharing_a_name_does_not_claim_the_program() {
        // Reaching far enough up any path eventually finds something, so
        // the search is bounded. Somebody whose account is named after an
        // agent must not have every program they run labelled as it.
        let settings = Settings::default();
        let path = "/Users/claude/projects/deep/nested/build/output/thing";
        assert_eq!(settings.label_for(path, &[], None).label, "thing");
    }

    #[test]
    fn the_default_chord_is_one_this_build_can_register() {
        assert!(parse_hotkey(DEFAULT_HOTKEY).is_ok());
    }

    #[test]
    fn a_chord_with_no_modifier_is_refused() {
        // It would register globally and swallow that key in every other
        // application, which is not a mistake somebody would connect back
        // to a terminal they configured a week ago.
        assert!(parse_hotkey("o").is_err());
        assert!(parse_hotkey("F5").is_err());
    }

    #[test]
    fn nonsense_costs_the_shortcut_and_not_the_app() {
        // Falls back rather than failing: a settings file is editable by
        // hand, and a typo in it must not stop the app starting.
        let fallback = hotkey_or_default("not a chord at all");
        assert_eq!(fallback, parse_hotkey(DEFAULT_HOTKEY).unwrap());
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
    fn a_font_nobody_chose_moves_to_the_new_default() {
        // Saving writes every field, so the old default sits in every
        // settings file written before the design landed as though somebody
        // had picked it. They did not.
        let path = scratch("superseded-font");
        std::fs::write(
            &path,
            format!("[terminal]\nfont_family = {SUPERSEDED_FONT:?}\n"),
        )
        .unwrap();

        let settings = load_from(&path);

        assert_eq!(settings.terminal.font_family, DEFAULT_FONT);
    }

    #[test]
    fn a_font_somebody_did_choose_is_left_alone() {
        let path = scratch("chosen-font");
        std::fs::write(&path, "[terminal]\nfont_family = \"Fira Code\"\n").unwrap();

        let settings = load_from(&path);

        assert_eq!(settings.terminal.font_family, "Fira Code");
    }

    #[test]
    fn a_file_from_before_the_version_field_reads_as_unversioned() {
        // The whole migration hangs off this. If a missing key took the
        // current version from the struct default instead, every old file
        // would claim to be migrated and nobody would get the fix.
        let path = scratch("unversioned-file");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "opacity = 85\n").unwrap();

        let raw: Settings = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        assert_eq!(raw.settings_version, 0);
    }

    #[test]
    fn a_stored_dock_icon_cannot_come_back() {
        // 1.0.0 and 1.0.1 both wrote show_in_dock into everybody's file,
        // and turning it on cost the overlay: a Dock icon makes this a
        // regular app, which owns a Space of its own and so is never
        // drawn over another app's full-screen one. The key is dead now,
        // so an old file has to load without it and save without it
        // rather than the app refusing to read the file at all.
        let path = scratch("stale-dock-key");
        std::fs::write(&path, "opacity = 85\nshow_in_dock = true\n").unwrap();

        let loaded = load_from(&path);
        assert_eq!(loaded.opacity, 85, "the rest of the file still has to load");

        save_to(&path, &loaded).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            !written.contains("show_in_dock"),
            "saving has to drop the dead key rather than write it back"
        );
    }

    #[test]
    fn a_size_below_the_usable_minimum_is_raised() {
        // A window narrower than this wraps every agent's interface into
        // nonsense, so a typed 1 has to become something usable rather
        // than being taken at its word.
        assert_eq!(
            sane_panel_size(1, 1),
            (MIN_PANEL_WIDTH, MIN_PANEL_HEIGHT),
            "a tiny request has to be raised to something usable"
        );
        // A reasonable one is left exactly alone.
        assert_eq!(sane_panel_size(900, 700), (900, 700));
    }

    #[test]
    fn storing_a_size_leaves_the_rest_of_the_settings_alone() {
        // A size arrives from dragging a window edge, which can happen
        // while the sheet is open, so it must not write back a stale copy
        // of everything else.
        let path = scratch("panel-size");
        let mut settings = Settings::default();
        settings.terminal.font_size = 19;
        settings.hotkey = "CmdOrCtrl+Shift+J".into();
        save_to(&path, &settings).unwrap();

        // save_panel_size reads from the real config path, so drive the
        // same read-modify-write by hand against this file.
        let mut stored = load_from(&path);
        let (w, h) = sane_panel_size(900, 700);
        stored.window.panel_width = w;
        stored.window.panel_height = h;
        save_to(&path, &stored).unwrap();

        let after = load_from(&path);
        assert_eq!(after.window.panel_width, 900);
        assert_eq!(after.window.panel_height, 700);
        assert_eq!(after.terminal.font_size, 19, "an unrelated setting changed");
        assert_eq!(after.hotkey, "CmdOrCtrl+Shift+J");
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
            },
            ..Settings::default()
        };
        let cfg = settings.choreo();
        assert!(!cfg.expand_when_wanted);
        assert_eq!(cfg.collapse_delay_ms, 250);
        assert!(!cfg.cues.glow);
        assert!(cfg.cues.sound);
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
