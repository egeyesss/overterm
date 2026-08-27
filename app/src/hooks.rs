//! Putting OverTerm's hooks into the Claude Code settings file.
//!
//! Each hook is a one-line shell command that prints a fixed JSON object
//! and exits. Claude reads the `terminalSequence` out of it and writes
//! that sequence to the terminal, where the detector picks it up. So the
//! whole integration is four entries in a JSON file, with no daemon, no
//! port, and no path to a binary that breaks when the app moves.
//!
//! The file belongs to the user and usually has their own hooks in it, so
//! everything here goes through `serde_json::Value` rather than a typed
//! struct: a struct would silently drop every setting it does not model.

use std::path::{Path, PathBuf};

use overterm_core::detect::hook::{
    EVENT_PERMISSION, EVENT_STOP, EVENT_STOP_FAILURE, EVENT_SUBMIT, MARKER_PREFIX, marker,
};
use serde_json::{Map, Value, json};

/// Claude Code events we ask to hear about, and what each one means to
/// the detector.
///
/// None of them take a matcher. `UserPromptSubmit` and `Stop` have no
/// matcher support at all, and for the other two we want every case.
const HOOKS: [(&str, &str); 4] = [
    ("UserPromptSubmit", EVENT_SUBMIT),
    ("Stop", EVENT_STOP),
    ("StopFailure", EVENT_STOP_FAILURE),
    ("PermissionRequest", EVENT_PERMISSION),
];

/// Seconds claude may wait for one of our hooks. It prints one line and
/// exits, so anything approaching this has gone wrong and claude should
/// stop waiting rather than hold up the session.
const TIMEOUT_SECS: u64 = 5;

/// Where Claude Code keeps the settings that apply to every project.
pub fn settings_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude").join("settings.json"))
}

/// What one hook prints on stdout.
fn hook_output(event: &str) -> String {
    let sequence = String::from_utf8(marker(event)).expect("markers are text");
    json!({ "terminalSequence": sequence }).to_string()
}

/// The command a hook entry runs.
fn hook_command(event: &str) -> String {
    // `%s` rather than letting printf read the format string, so the
    // backslash escapes reach claude's JSON parser untouched. The JSON
    // has no single quotes in it, so one pair wraps it safely.
    let output = hook_output(event);
    debug_assert!(!output.contains('\''), "would break the shell quoting");
    format!("printf '%s' '{output}'")
}

/// How our own entries are found again later.
///
/// The command spells the leading escape byte as a JSON escape rather
/// than writing it raw, so the tag is the marker without it.
fn marker_tag() -> &'static str {
    std::str::from_utf8(&MARKER_PREFIX[1..]).expect("the marker prefix is text")
}

fn is_ours(handler: &Value) -> bool {
    handler["command"]
        .as_str()
        .is_some_and(|command| command.contains(marker_tag()))
}

fn read(path: &Path) -> Result<Value, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        // No settings file yet is not a problem; claude reads one that
        // appears later.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    let settings: Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not valid JSON: {e}", path.display()))?;
    if !settings.is_object() {
        return Err(format!("{} is not a JSON object", path.display()));
    }
    Ok(settings)
}

/// Write through a temporary file in the same directory, because claude
/// watches this one and a half-written settings file is worse than an
/// unchanged one.
fn write(path: &Path, settings: &Value) -> Result<(), String> {
    let dir = path.parent().ok_or("settings path has no directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let mut text =
        serde_json::to_string_pretty(settings).map_err(|e| format!("cannot serialise: {e}"))?;
    text.push('\n');
    let staged = path.with_extension("json.overterm");
    std::fs::write(&staged, text).map_err(|e| format!("cannot write {}: {e}", staged.display()))?;
    std::fs::rename(&staged, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

/// Take out every entry of ours, and only ours. Reports whether it found
/// any.
///
/// Containers we empty are cleaned up; containers that were already empty
/// belong to whoever wrote them and are left exactly as they are.
fn strip(settings: &mut Value) -> bool {
    let Some(root) = settings.as_object_mut() else {
        return false;
    };
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };

    let mut removed = false;
    let mut emptied: Vec<String> = Vec::new();
    for (event, groups) in hooks.iter_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        let had = groups.len();
        groups.retain_mut(|group| {
            let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let before = handlers.len();
            handlers.retain(|handler| !is_ours(handler));
            if handlers.len() == before {
                return true;
            }
            removed = true;
            // The group is only ours to delete if we are what emptied it.
            !handlers.is_empty()
        });
        if had > 0 && groups.is_empty() {
            emptied.push(event.clone());
        }
    }
    for event in emptied {
        hooks.remove(&event);
    }
    if removed && hooks.is_empty() {
        root.remove("hooks");
    }
    removed
}

fn add(settings: &mut Value) {
    let root = settings.as_object_mut().expect("checked when read");
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(hooks) = hooks.as_object_mut() else {
        return;
    };
    for (event, ours) in HOOKS {
        let groups = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()));
        // Some other shape under an event name we know is not ours to
        // reinterpret, so leave it be.
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        groups.push(json!({
            "hooks": [{
                "type": "command",
                "command": hook_command(ours),
                "timeout": TIMEOUT_SECS,
            }],
        }));
    }
}

/// Add our entries, replacing any older copy of them. Reports whether the
/// file changed.
pub fn install(path: &Path) -> Result<bool, String> {
    let mut settings = read(path)?;
    let before = settings.clone();
    // Strip first so running this twice leaves one copy, and so entries
    // written by an older build are replaced rather than doubled up.
    strip(&mut settings);
    add(&mut settings);
    if settings == before {
        return Ok(false);
    }
    write(path, &settings)?;
    Ok(true)
}

/// Take our entries out and leave the rest of the file as it was. Reports
/// whether the file changed.
pub fn uninstall(path: &Path) -> Result<bool, String> {
    let mut settings = read(path)?;
    if !strip(&mut settings) {
        return Ok(false);
    }
    write(path, &settings)?;
    Ok(true)
}

/// Whether every entry we install is present and current.
pub fn installed(path: &Path) -> Result<bool, String> {
    let settings = read(path)?;
    Ok(HOOKS.iter().all(|(event, ours)| {
        let wanted = json!(hook_command(ours));
        settings["hooks"][event].as_array().is_some_and(|groups| {
            groups.iter().any(|group| {
                group["hooks"]
                    .as_array()
                    .is_some_and(|handlers| handlers.iter().any(|h| h["command"] == wanted))
            })
        })
    }))
}

/// Put the hooks in place the first time the app runs, and leave that
/// file alone from then on.
///
/// Doing it on every launch would undo someone taking the entries out,
/// which is worse than never having offered: it is their config file. A
/// launch that fails to install does not count as the first one, so a
/// settings file that was mid-edit gets another chance.
fn install_once(claude: &Path, config: &Path) -> Result<bool, String> {
    let mut settings = crate::settings::load_from(config);
    if settings.claude_hooks_installed {
        return Ok(false);
    }
    // Plenty of people will never run Claude Code, and creating a config
    // directory for a tool someone does not have is not ours to do. They
    // may install it later, so this does not count as the launch that
    // set the hooks up, and a launch after they do will pick it up.
    if !claude.parent().is_some_and(Path::exists) {
        return Ok(false);
    }
    let changed = install(claude)?;
    settings.claude_hooks_installed = true;
    if let Err(e) = crate::settings::save_to(config, &settings) {
        // The hooks are in. Not being able to write that down means the
        // next launch checks again and finds nothing to do.
        eprintln!("[hooks] installed but could not record it: {e}");
    }
    Ok(changed)
}

/// Take the hook entries out on the way to being uninstalled.
///
/// macOS runs nothing of ours when an app is dragged to the trash, so a
/// package manager has to call this. Removing the app also forgets that
/// the setup was ever done, because being uninstalled is not the same as
/// choosing to go without the hooks: install it again and it should set
/// itself up again.
pub fn uninstall_on_removal() -> Result<bool, String> {
    let claude = settings_path().ok_or("no home directory to find the settings file in")?;
    let config = crate::settings::path().ok_or("no home directory to keep settings in")?;
    uninstall_once(&claude, &config)
}

fn uninstall_once(claude: &Path, config: &Path) -> Result<bool, String> {
    let removed = uninstall(claude)?;
    let mut settings = crate::settings::load_from(config);
    if settings.claude_hooks_installed {
        settings.claude_hooks_installed = false;
        crate::settings::save_to(config, &settings)?;
    }
    Ok(removed)
}

/// Where the two files live on this machine.
pub fn install_on_first_run() -> Result<bool, String> {
    let claude = settings_path().ok_or("no home directory to find the settings file in")?;
    let config = crate::settings::path().ok_or("no home directory to keep settings in")?;
    install_once(&claude, &config)
}

fn at_settings<T>(action: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
    let path = settings_path().ok_or("no home directory to find the settings file in")?;
    action(&path)
}

#[tauri::command]
pub fn install_hooks() -> Result<bool, String> {
    at_settings(install)
}

#[tauri::command]
pub fn uninstall_hooks() -> Result<bool, String> {
    at_settings(uninstall)
}

#[tauri::command]
pub fn hooks_installed() -> Result<bool, String> {
    at_settings(installed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use overterm_core::detect::Adapter;
    use overterm_core::detect::hook::{EVENTS, HookAdapter};
    use overterm_core::{Detector, Signal};
    use std::time::Instant;

    /// A settings file of its own per test, since these all write.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("overterm-hooks-test");
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        let path = dir.join(format!("{name}.json"));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn write_settings(path: &Path, text: &str) {
        std::fs::write(path, text).expect("write fixture");
    }

    /// A config file of its own, so the first-run tests do not share one.
    fn scratch_config(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("overterm-hooks-test");
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        let path = dir.join(format!("{name}.toml"));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn read_settings(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).expect("read back"))
            .expect("valid JSON")
    }

    #[test]
    fn what_we_install_is_what_the_detector_reads() {
        // The two halves of the integration meet nowhere else: one writes
        // a shell command into a JSON file, the other reads bytes off a
        // terminal. If they ever disagree, nothing is detected and every
        // test on either side still passes.
        for (_, event) in HOOKS {
            let printed: Value = serde_json::from_str(&hook_output(event)).expect("valid JSON");
            let sequence = printed["terminalSequence"].as_str().expect("a string");

            let mut adapter = HookAdapter::new();
            let signals = adapter.feed(sequence.as_bytes(), Instant::now());
            assert_eq!(signals.len(), 1, "{event}: adapter did not read it back");
        }
    }

    #[test]
    fn every_event_the_detector_knows_is_installed() {
        for event in EVENTS {
            assert!(
                HOOKS.iter().any(|(_, ours)| *ours == event),
                "{event} is understood but never installed, so it can never arrive"
            );
        }
    }

    #[test]
    fn the_printed_json_survives_the_shell_quoting() {
        // Single quotes in the payload would end the quoting early and
        // hand the rest of it to the shell as code.
        for (_, event) in HOOKS {
            assert!(!hook_output(event).contains('\''), "{event}");
            assert!(hook_command(event).ends_with('\''), "{event}");
        }
    }

    #[test]
    fn installing_into_nothing_at_all_works() {
        let path = scratch("missing");
        assert!(install(&path).expect("install"));
        assert!(installed(&path).expect("check"));
        let settings = read_settings(&path);
        for (event, _) in HOOKS {
            assert!(settings["hooks"][event].is_array(), "{event} missing");
        }
    }

    #[test]
    fn installing_twice_leaves_one_copy_and_no_second_write() {
        let path = scratch("idempotent");
        assert!(install(&path).expect("first"));
        let after_first = std::fs::read_to_string(&path).expect("read");

        assert!(
            !install(&path).expect("second"),
            "the second install should find nothing to do"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            after_first,
            "the file was rewritten for no reason"
        );

        let settings = read_settings(&path);
        let groups = settings["hooks"]["Stop"].as_array().expect("an array");
        assert_eq!(groups.len(), 1, "duplicated ourselves: {groups:#?}");
    }

    #[test]
    fn someone_elses_settings_survive_untouched() {
        let path = scratch("preserved");
        write_settings(
            &path,
            r#"{
  "model": "opus",
  "enabledPlugins": { "vercel@claude-plugins-official": true },
  "theme": "dark"
}"#,
        );
        install(&path).expect("install");

        let settings = read_settings(&path);
        assert_eq!(settings["model"], json!("opus"));
        assert_eq!(settings["theme"], json!("dark"));
        assert_eq!(
            settings["enabledPlugins"]["vercel@claude-plugins-official"],
            json!(true)
        );
        // And their key order, which is what anyone reading the file sees.
        let keys: Vec<&str> = settings
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["model", "enabledPlugins", "theme", "hooks"]);
    }

    #[test]
    fn someone_elses_hooks_survive_both_ways() {
        let path = scratch("their-hooks");
        write_settings(
            &path,
            r#"{
  "hooks": {
    "Stop": [{ "hooks": [{ "type": "command", "command": "say done" }] }],
    "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "audit.sh" }] }]
  }
}"#,
        );
        let original = read_settings(&path);

        install(&path).expect("install");
        let settings = read_settings(&path);
        let stop = settings["hooks"]["Stop"].as_array().expect("an array");
        assert_eq!(stop.len(), 2, "theirs should still be there beside ours");
        assert_eq!(stop[0], original["hooks"]["Stop"][0]);

        uninstall(&path).expect("uninstall");
        assert_eq!(
            read_settings(&path),
            original,
            "uninstall should put the file back exactly"
        );
    }

    #[test]
    fn uninstall_leaves_no_empty_containers_of_ours_behind() {
        let path = scratch("tidy");
        write_settings(&path, r#"{ "model": "opus" }"#);
        install(&path).expect("install");
        assert!(uninstall(&path).expect("uninstall"));

        let settings = read_settings(&path);
        assert!(
            settings.get("hooks").is_none(),
            "the hooks key was ours alone and should be gone: {settings:#?}"
        );
        assert_eq!(settings["model"], json!("opus"));
    }

    #[test]
    fn uninstall_keeps_empty_containers_that_were_not_ours() {
        // Someone else's empty group is someone else's business.
        let path = scratch("their-empties");
        write_settings(
            &path,
            r#"{ "hooks": { "Stop": [{ "matcher": "x", "hooks": [] }], "Notification": [] } }"#,
        );
        let original = read_settings(&path);
        install(&path).expect("install");
        uninstall(&path).expect("uninstall");
        assert_eq!(read_settings(&path), original);
    }

    #[test]
    fn uninstalling_when_nothing_is_installed_changes_nothing() {
        let path = scratch("absent");
        write_settings(&path, r#"{ "model": "opus" }"#);
        assert!(!uninstall(&path).expect("uninstall"));
        assert!(!installed(&path).expect("check"));
    }

    #[test]
    fn an_older_entry_of_ours_is_replaced_rather_than_doubled() {
        let path = scratch("stale");
        write_settings(
            &path,
            r#"{ "hooks": { "Stop": [{ "hooks": [{ "type": "command",
                 "command": "printf '%s' '{\"terminalSequence\":\"\\u001b]777;overterm;old\\u0007\"}'" }] }] } }"#,
        );
        assert!(install(&path).expect("install"));

        let settings = read_settings(&path);
        let groups = settings["hooks"]["Stop"].as_array().expect("an array");
        assert_eq!(groups.len(), 1, "left a stale entry behind: {groups:#?}");
        assert!(installed(&path).expect("check"));
    }

    #[test]
    fn a_settings_file_that_is_not_an_object_is_refused() {
        let path = scratch("not-an-object");
        write_settings(&path, "[1, 2, 3]");
        assert!(install(&path).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "[1, 2, 3]",
            "a file we cannot understand must be left alone"
        );
    }

    #[test]
    fn an_empty_file_is_treated_as_empty_settings() {
        let path = scratch("empty");
        write_settings(&path, "\n  \n");
        assert!(install(&path).expect("install"));
        assert!(installed(&path).expect("check"));
    }

    #[test]
    fn the_installed_hooks_drive_the_detector_end_to_end() {
        // Play the markers through in the order a real turn produces
        // them, straight from what the installer writes.
        let mut detector = Detector::new(vec![Box::new(HookAdapter::new())]);
        let now = Instant::now();
        let sequence = |event: &str| {
            let printed: Value = serde_json::from_str(&hook_output(event)).expect("valid JSON");
            printed["terminalSequence"]
                .as_str()
                .expect("a string")
                .as_bytes()
                .to_vec()
        };

        detector.feed_output(&sequence(EVENT_SUBMIT), now);
        assert_eq!(detector.state(), overterm_core::AgentState::Busy);
        assert!(detector.take_submit());
        assert!(detector.is_working(now));

        detector.feed_output(&sequence(EVENT_PERMISSION), now);
        assert_eq!(detector.state(), overterm_core::AgentState::NeedsInput);

        detector.feed_output(&sequence(EVENT_STOP), now);
        assert_eq!(detector.state(), overterm_core::AgentState::Done);
        assert!(!detector.is_working(now));
        // And the guesswork stays switched off while claude is running.
        assert!(detector.precise_source_active(now));
        assert!(
            detector
                .apply(Signal::Quiescence { quiet_ms: 400 }, now)
                .is_none()
        );
    }

    #[test]
    fn the_hooks_go_in_once_and_stay_out_once_removed() {
        // The whole point of doing this once. Putting back what someone
        // deleted from their own config file, every time the app starts,
        // is worse than never having set it up for them.
        let claude = scratch("first-run-claude");
        let config = scratch_config("first-run-config");
        write_settings(&claude, r#"{ "model": "opus" }"#);

        assert!(install_once(&claude, &config).expect("first launch"));
        assert!(installed(&claude).expect("check"));

        uninstall(&claude).expect("the user takes them out again");
        assert!(!installed(&claude).expect("check"));

        assert!(!install_once(&claude, &config).expect("a later launch"));
        assert!(
            !installed(&claude).expect("check"),
            "the app put back what the user removed"
        );
    }

    #[test]
    fn nothing_is_created_for_someone_who_does_not_use_claude_code() {
        // Anyone can download this and run any CLI in it. Conjuring a
        // config directory for a tool they have never installed, on first
        // launch, is not something a terminal gets to do.
        let dir = std::env::temp_dir().join("overterm-hooks-test/no-claude-here");
        let _ = std::fs::remove_dir_all(&dir);
        let claude = dir.join("settings.json");
        let config = scratch_config("no-claude-config");

        assert!(!install_once(&claude, &config).expect("first launch"));
        assert!(
            !dir.exists(),
            "created a directory for a tool that is not there"
        );
        assert!(
            !crate::settings::load_from(&config).claude_hooks_installed,
            "must stay ready to try again if they install it later"
        );

        // And when they do install it, a later launch picks it up.
        std::fs::create_dir_all(&dir).expect("claude gets installed");
        assert!(install_once(&claude, &config).expect("a later launch"));
        assert!(installed(&claude).expect("check"));
    }

    #[test]
    fn a_first_run_that_fails_is_tried_again_next_launch() {
        // A settings file caught mid-edit should not cost the user the
        // integration for good.
        let claude = scratch("retry-claude");
        let config = scratch_config("retry-config");
        write_settings(&claude, "[1, 2, 3]");

        assert!(install_once(&claude, &config).is_err());
        assert!(
            !crate::settings::load_from(&config).claude_hooks_installed,
            "a failed launch must not count as the one that set them up"
        );

        write_settings(&claude, r#"{ "model": "opus" }"#);
        assert!(install_once(&claude, &config).expect("second launch"));
        assert!(installed(&claude).expect("check"));
    }

    #[test]
    fn removing_the_app_takes_the_hook_entries_with_it() {
        // Dragging an app to the trash runs none of its own code, so a
        // package manager has to call this on the way out. Otherwise the
        // entries outlive the thing that reads them.
        let claude = scratch("removal-claude");
        let config = scratch_config("removal-config");
        write_settings(&claude, r#"{ "model": "opus" }"#);
        install_once(&claude, &config).expect("first launch");

        assert!(uninstall_once(&claude, &config).expect("removal"));
        assert!(!installed(&claude).expect("check"));
        assert_eq!(read_settings(&claude)["model"], json!("opus"));

        // Being uninstalled is not the same as choosing to go without
        // them, so installing the app again sets them up again.
        assert!(!crate::settings::load_from(&config).claude_hooks_installed);
        assert!(install_once(&claude, &config).expect("a fresh install"));
        assert!(installed(&claude).expect("check"));
    }

    #[test]
    fn hooks_already_there_still_count_as_set_up() {
        // Whether they arrived from an earlier build or by hand, the
        // first run has nothing left to do and must not keep trying.
        let claude = scratch("already-claude");
        let config = scratch_config("already-config");
        install(&claude).expect("someone else put them there");

        assert!(!install_once(&claude, &config).expect("first launch"));
        assert!(crate::settings::load_from(&config).claude_hooks_installed);
    }
}
