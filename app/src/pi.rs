//! Putting OverTerm's extension into Pi's extensions directory.
//!
//! Pi loads any `.ts` or `.js` file it finds under
//! `~/.pi/agent/extensions`, so the integration is one file dropped in a
//! directory. Nothing to register, no settings file to edit, and nothing
//! that breaks when the app moves: the extension is carried inside the
//! binary and written out from there.
//!
//! The shape of the rules is the same as the Claude Code hooks in
//! `hooks.rs`, and for the same reasons: the directory belongs to the
//! user, so this writes through a temporary file, goes in once on the
//! first launch that manages it, and never puts back what somebody
//! removed. Only the mechanics differ, since one edits JSON and this one
//! writes a file.

use std::path::{Path, PathBuf};

/// The extension itself, carried in the binary rather than looked up
/// beside it. An app bundle gets moved, copied and run from a disk image,
/// and a path worked out at runtime is wrong in at least one of those.
const EXTENSION: &str = include_str!("../pi-extension/overterm.js");

/// What the file is called once it is in place. Ours by name, so finding
/// it again later needs no marker of its own.
const FILE_NAME: &str = "overterm.js";

/// Where Pi keeps everything that applies to every project.
///
/// Its presence is also how the app decides Pi is installed at all.
/// Creating this for somebody who has never run Pi is not ours to do.
pub fn agent_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".pi").join("agent"))
}

pub fn extensions_dir() -> Option<PathBuf> {
    agent_dir().map(|dir| dir.join("extensions"))
}

/// Whether a file at our path is one of ours.
///
/// Checked on the content rather than trusted from the name, so a file
/// somebody else put there under the same name is left alone instead of
/// being deleted on our way out.
fn is_ours(text: &str) -> bool {
    text.contains(marker_tag())
}

/// The part of the marker that is in the extension as plain text.
///
/// The extension spells the leading escape byte as a JSON-style escape
/// rather than putting a control character in a source file, so the tag
/// is the marker prefix without it.
fn marker_tag() -> &'static str {
    let prefix = overterm_core::detect::hook::MARKER_PREFIX;
    std::str::from_utf8(&prefix[1..]).expect("the marker prefix is text")
}

fn read(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Write through a temporary file in the same directory, because Pi reads
/// this one and a half-written extension is worse than no extension.
fn write(path: &Path) -> Result<(), String> {
    let dir = path.parent().ok_or("extension path has no directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let staged = path.with_extension("js.overterm");
    std::fs::write(&staged, EXTENSION)
        .map_err(|e| format!("cannot write {}: {e}", staged.display()))?;
    std::fs::rename(&staged, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

/// Put the extension in place, replacing an older copy of ours. Reports
/// whether anything changed.
pub fn install(dir: &Path) -> Result<bool, String> {
    let path = dir.join(FILE_NAME);
    if let Some(existing) = read(&path)? {
        if existing == EXTENSION {
            return Ok(false);
        }
        // Something else's file under our name. Replacing it would be
        // taking someone's work, and there is nowhere else for ours to
        // go, so this stops and says so.
        if !is_ours(&existing) {
            return Err(format!(
                "{} is already there and is not ours to replace",
                path.display()
            ));
        }
    }
    write(&path)?;
    Ok(true)
}

/// Take our extension out and leave anything else alone. Reports whether
/// anything changed.
pub fn uninstall(dir: &Path) -> Result<bool, String> {
    let path = dir.join(FILE_NAME);
    let Some(existing) = read(&path)? else {
        return Ok(false);
    };
    if !is_ours(&existing) {
        return Ok(false);
    }
    std::fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
    Ok(true)
}

/// Whether the extension in place is the one this build ships.
pub fn installed(dir: &Path) -> Result<bool, String> {
    Ok(read(&dir.join(FILE_NAME))?.is_some_and(|text| text == EXTENSION))
}

/// Put the extension in place the first time the app runs, and leave that
/// directory alone from then on.
fn install_once(extensions: &Path, config: &Path) -> Result<bool, String> {
    let mut settings = crate::settings::load_from(config);
    if settings.pi_extension_installed {
        return Ok(false);
    }
    // Plenty of people will never run Pi. Its agent directory is the one
    // thing that says otherwise, and it is the parent of the directory
    // written to, which may not exist yet even when Pi is installed.
    if !extensions.parent().is_some_and(Path::exists) {
        return Ok(false);
    }
    let changed = install(extensions)?;
    settings.pi_extension_installed = true;
    if let Err(e) = crate::settings::save_to(config, &settings) {
        // The extension is in. Not being able to write that down means
        // the next launch checks again and finds nothing to do.
        eprintln!("[pi] installed but could not record it: {e}");
    }
    Ok(changed)
}

fn uninstall_once(extensions: &Path, config: &Path) -> Result<bool, String> {
    let removed = uninstall(extensions)?;
    let mut settings = crate::settings::load_from(config);
    if settings.pi_extension_installed {
        settings.pi_extension_installed = false;
        crate::settings::save_to(config, &settings)?;
    }
    Ok(removed)
}

pub fn install_on_first_run() -> Result<bool, String> {
    let extensions = extensions_dir().ok_or("no home directory to find Pi in")?;
    let config = crate::settings::path().ok_or("no home directory to keep settings in")?;
    install_once(&extensions, &config)
}

/// Take the extension out on the way to being uninstalled.
///
/// macOS runs nothing of ours when an app is dragged to the trash, so a
/// package manager has to call this. Removing the app also forgets that
/// the setup was ever done, because being uninstalled is not the same as
/// choosing to go without it.
pub fn uninstall_on_removal() -> Result<bool, String> {
    let extensions = extensions_dir().ok_or("no home directory to find Pi in")?;
    let config = crate::settings::path().ok_or("no home directory to keep settings in")?;
    uninstall_once(&extensions, &config)
}

fn at_extensions<T>(action: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
    let dir = extensions_dir().ok_or("no home directory to find Pi in")?;
    action(&dir)
}

#[tauri::command]
pub fn install_pi_extension() -> Result<bool, String> {
    at_extensions(install)
}

#[tauri::command]
pub fn uninstall_pi_extension() -> Result<bool, String> {
    at_extensions(uninstall)
}

#[tauri::command]
pub fn pi_extension_installed() -> Result<bool, String> {
    at_extensions(installed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use overterm_core::detect::Adapter;
    use overterm_core::detect::hook::{EVENT_PERMISSION, EVENT_STOP, EVENT_SUBMIT, HookAdapter};
    use std::time::Instant;

    /// A directory of its own per test, since these all write.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("overterm-pi-test")
            .join(name)
            .join("extensions");
        let _ = std::fs::remove_dir_all(dir.parent().expect("has a parent"));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn scratch_config(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("overterm-pi-test");
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        let path = dir.join(format!("{name}.toml"));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// The events the extension reports, and what each one means here.
    const REPORTED: [&str; 3] = [EVENT_SUBMIT, EVENT_STOP, EVENT_PERMISSION];

    #[test]
    fn what_we_install_is_what_the_detector_reads() {
        // The two halves of this integration meet nowhere else: one is a
        // JavaScript file, the other reads bytes off a terminal. If they
        // ever disagree nothing is detected, and every test on either
        // side still passes.
        assert!(
            EXTENSION.contains(r#"const MARKER_PREFIX = "\u001b]777;overterm;";"#),
            "the extension no longer spells the marker the detector expects"
        );
        assert!(
            EXTENSION.contains(r#"const MARKER_END = "\u0007";"#),
            "the extension no longer ends a marker with a bell"
        );

        // And what it builds from those really does read back as a signal.
        for event in REPORTED {
            assert!(
                EXTENSION.contains(&format!("report(\"{event}\")")),
                "the extension never reports {event}"
            );
            let mut adapter = HookAdapter::new();
            let marker = overterm_core::detect::hook::marker(event);
            assert_eq!(
                adapter.feed(&marker, Instant::now()).len(),
                1,
                "{event}: the adapter did not read it back"
            );
        }
    }

    #[test]
    fn the_extension_is_a_file_pi_will_look_at() {
        // Pi picks up `.ts` and `.js` and ignores everything else, so a
        // rename here is an integration that silently never loads.
        assert!(FILE_NAME.ends_with(".js"), "{FILE_NAME} is not a .js file");
        assert!(
            EXTENSION.contains("export default"),
            "an extension has to default-export its registration function"
        );
    }

    #[test]
    fn a_subagent_is_kept_quiet() {
        // Pi runs subagents as their own processes on the same terminal.
        // Without this the window comes back every time one finishes.
        assert!(
            EXTENSION.contains("PI_SUBAGENT_DEPTH"),
            "the subagent guard is gone, so every subagent turn reports"
        );
    }

    #[test]
    fn installing_into_an_empty_directory_works() {
        let dir = scratch("empty");
        assert!(install(&dir).expect("install"));
        assert!(installed(&dir).expect("check"));
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE_NAME)).expect("read back"),
            EXTENSION
        );
    }

    #[test]
    fn installing_twice_writes_once() {
        let dir = scratch("idempotent");
        assert!(install(&dir).expect("first"));
        assert!(
            !install(&dir).expect("second"),
            "the second install should find nothing to do"
        );
    }

    #[test]
    fn uninstall_takes_it_out_and_says_so() {
        let dir = scratch("removal");
        install(&dir).expect("install");
        assert!(uninstall(&dir).expect("uninstall"));
        assert!(!dir.join(FILE_NAME).exists());
        assert!(!uninstall(&dir).expect("again"), "nothing left to remove");
        assert!(!installed(&dir).expect("check"));
    }

    #[test]
    fn somebody_elses_extensions_are_left_alone() {
        let dir = scratch("neighbours");
        std::fs::write(dir.join("theirs.ts"), "export default () => {};").expect("write");
        install(&dir).expect("install");
        uninstall(&dir).expect("uninstall");
        assert!(
            dir.join("theirs.ts").exists(),
            "removed an extension that was not ours"
        );
    }

    #[test]
    fn a_file_of_theirs_under_our_name_is_neither_replaced_nor_removed() {
        // Unlikely, and the alternative is deleting somebody's work.
        let dir = scratch("their-name-clash");
        let theirs = "export default function () {}\n";
        std::fs::write(dir.join(FILE_NAME), theirs).expect("write");

        assert!(install(&dir).is_err(), "overwrote a file that was not ours");
        assert!(!uninstall(&dir).expect("uninstall"));
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE_NAME)).expect("read"),
            theirs
        );
    }

    #[test]
    fn an_older_copy_of_ours_is_replaced() {
        let dir = scratch("stale");
        std::fs::write(
            dir.join(FILE_NAME),
            "// old\nconst MARKER_PREFIX = \"\\u001b]777;overterm;\";\n",
        )
        .expect("write");
        assert!(install(&dir).expect("install"));
        assert!(installed(&dir).expect("check"));
    }

    #[test]
    fn it_goes_in_once_and_stays_out_once_removed() {
        // Putting back what somebody deleted from their own extensions
        // directory, every time the app starts, is worse than never
        // having set it up for them.
        let dir = scratch("first-run");
        let config = scratch_config("first-run");

        assert!(install_once(&dir, &config).expect("first launch"));
        assert!(installed(&dir).expect("check"));

        uninstall(&dir).expect("the user takes it out again");
        assert!(!install_once(&dir, &config).expect("a later launch"));
        assert!(
            !installed(&dir).expect("check"),
            "the app put back what the user removed"
        );
    }

    #[test]
    fn nothing_is_created_for_somebody_who_does_not_use_pi() {
        // Anyone can run any CLI in this terminal. Conjuring a config
        // directory for a tool they have never installed is not something
        // a terminal gets to do.
        let root = std::env::temp_dir().join("overterm-pi-test/no-pi-here");
        let _ = std::fs::remove_dir_all(&root);
        let extensions = root.join("extensions");
        let config = scratch_config("no-pi");

        assert!(!install_once(&extensions, &config).expect("first launch"));
        assert!(
            !root.exists(),
            "created a directory for a tool that is not there"
        );
        assert!(
            !crate::settings::load_from(&config).pi_extension_installed,
            "must stay ready to try again if they install Pi later"
        );

        // And when they do install it, a later launch picks it up.
        std::fs::create_dir_all(&root).expect("pi gets installed");
        assert!(install_once(&extensions, &config).expect("a later launch"));
        assert!(installed(&extensions).expect("check"));
    }

    #[test]
    fn removing_the_app_takes_the_extension_with_it() {
        let dir = scratch("app-removal");
        let config = scratch_config("app-removal");
        install_once(&dir, &config).expect("first launch");

        assert!(uninstall_once(&dir, &config).expect("removal"));
        assert!(!installed(&dir).expect("check"));
        // Being uninstalled is not the same as choosing to go without it,
        // so installing the app again sets it up again.
        assert!(!crate::settings::load_from(&config).pi_extension_installed);
        assert!(install_once(&dir, &config).expect("a fresh install"));
    }
}
