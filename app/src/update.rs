//! Noticing that a newer build exists, and running the installer.
//!
//! The app has always had an upgrade path: `install.sh` downloads the
//! newest release, checks it against its checksum and swaps the bundle.
//! What was missing is the noticing, so this asks GitHub what the newest
//! release is, stores the answer, and hands the settings sheet a line it
//! can type into a session.
//!
//! Nothing here ever interrupts. A check that fails is silent, and the
//! only thing a check that succeeds puts in front of anybody is a dot on
//! the settings control.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};

use crate::settings;

/// What the frontend hears when a check has finished.
pub const EVENT_UPDATE: &str = "overterm://update";

/// The newest release, as JSON.
///
/// Release candidates are published as prereleases and GitHub leaves
/// those out of this endpoint, so nothing here has to know what a
/// prerelease looks like.
const LATEST_RELEASE: &str = "https://api.github.com/repos/egeyesss/overterm/releases/latest";

/// Where somebody reads what changed.
const RELEASES_PAGE: &str = "https://github.com/egeyesss/overterm/releases/latest";

/// The installer, which is also the upgrade path and the same line the
/// README gives.
const INSTALL_SCRIPT: &str = "https://raw.githubusercontent.com/egeyesss/overterm/main/install.sh";

/// How long a stored answer is good for.
///
/// Unauthenticated GitHub allows sixty requests an hour from one address.
/// Once a launch and no more often than this leaves that limit alone
/// however the app is used, and a release is not something that needs
/// noticing within the hour.
const CHECK_EVERY_SECS: u64 = 6 * 60 * 60;

/// How long the request may take before it is abandoned.
///
/// It runs on a thread of its own, so this is not holding anything up. It
/// is here so a connection that hangs cannot leave a process behind for
/// the rest of the session.
const CHECK_TIMEOUT_SECS: &str = "10";

/// The Homebrew cask's token, which is also the directory it records the
/// install in.
const CASK: &str = "overterm";

/// What the bundle is called, for when the running copy cannot be found
/// on disk. `open` takes a name as happily as a path.
const APP_NAME: &str = "oTerm";

/// What the interface needs to know about releases.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// The version this build reports.
    pub current: String,
    /// The newest release the last check found. Empty until one succeeds.
    pub latest: String,
    /// Whether `latest` is a later version than `current`.
    pub available: bool,
    /// Whether the notice for this particular version has been put away.
    /// A later release says so again rather than staying quiet for good.
    pub dismissed: bool,
    /// Whether Homebrew installed this copy, in which case the installer
    /// must not be run over the top of it.
    pub homebrew: bool,
    /// Whether this build has a way to install an update at all.
    pub installable: bool,
    /// Where to read about the release.
    pub release_url: String,
}

/// Whether this build has an installer to run.
///
/// `install.sh` refuses anywhere but macOS, and the releases it downloads
/// are macOS bundles, so there is nothing for a check to offer elsewhere.
/// A port that publishes builds of its own is where this stops being
/// true, and this is the one place that has to change.
fn installable() -> bool {
    cfg!(target_os = "macos")
}

/// The current state, worked out from what the last check stored.
fn status_from(stored: &settings::Settings) -> UpdateStatus {
    // The same answer the interface already shows at the foot of the
    // sheet, rather than a second reading of the crate version that
    // could one day disagree with it.
    let current = crate::app_version().to_string();
    let available = is_newer(&stored.latest_version, &current);
    // Put away for one version only: a release later than the one that
    // was dismissed is news again.
    let dismissed = available && !is_newer(&stored.latest_version, &stored.update_notice_seen);
    UpdateStatus {
        current,
        latest: stored.latest_version.clone(),
        available,
        dismissed,
        homebrew: managed_by_homebrew(),
        installable: installable(),
        release_url: RELEASES_PAGE.to_string(),
    }
}

/// Look for a newer release, once, without holding up the launch.
///
/// Every failure along the way is silent. No network, a rate limit and
/// GitHub being down are all ordinary, and none of them is worth saying
/// anything about over a terminal somebody is working in.
pub fn check_in_background<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    std::thread::spawn(move || {
        if let Some(status) = check_now() {
            let _ = app.emit(EVENT_UPDATE, status);
        }
    });
}

/// Ask GitHub, if asking is due, and store what came back.
///
/// `None` means nothing happened and there is nothing to tell the
/// interface, which reads the stored answer on its own anyway.
fn check_now() -> Option<UpdateStatus> {
    let stored = settings::load();
    if !installable() || !stored.check_for_updates || !due(&stored, now_secs()) {
        return None;
    }
    // Stored either way. A check that failed still counts as having
    // asked, or a machine with no network would ask again on every
    // launch for as long as it has none.
    store(ask_github().as_deref())
}

/// Whether enough time has passed to ask again.
fn due(stored: &settings::Settings, now: u64) -> bool {
    // The second half is not the same test twice: a stored time in the
    // future means the clock moved backwards, and without this the app
    // would wait for the clock to catch up rather than asking again.
    stored.last_update_check > now || now - stored.last_update_check >= CHECK_EVERY_SECS
}

/// Ask GitHub what the newest release is called.
///
/// Through curl rather than an HTTP client of our own. The only one in
/// the tree arrives through Tauri with no TLS backend turned on, so one
/// request every six hours would otherwise cost the app a whole TLS
/// stack. curl ships with macOS, `install.sh` already refuses to run
/// without it, and this is an unauthenticated GET of a public document.
fn ask_github() -> Option<String> {
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            CHECK_TIMEOUT_SECS,
            // The API refuses a request that does not name its caller.
            "-H",
            concat!("User-Agent: oTerm/", env!("CARGO_PKG_VERSION")),
            "-H",
            "Accept: application/vnd.github+json",
            LATEST_RELEASE,
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    tag_from(&String::from_utf8_lossy(&output.stdout))
}

/// The version out of a releases answer, or nothing if it does not look
/// like one.
///
/// Read as a `Value` rather than a struct: the answer carries several
/// dozen fields and this wants exactly one of them.
fn tag_from(body: &str) -> Option<String> {
    let body: serde_json::Value = serde_json::from_str(body).ok()?;
    let version = release_version(body.get("tag_name")?.as_str()?);
    if version.is_empty() {
        return None;
    }
    Some(version)
}

/// A tag as a version. Releases are tagged `v1.0.3` and the app reports
/// `1.0.3`, so one of the two has to give.
fn release_version(tag: &str) -> String {
    let tag = tag.trim();
    tag.strip_prefix('v').unwrap_or(tag).to_string()
}

/// Write down what a check found, and answer with the state it leaves.
///
/// Read from disk and written straight back, the way a stored window size
/// is: a check finishes whenever the network gets round to it, which can
/// be while somebody has the settings sheet open, and it must not be able
/// to undo what they just changed in it.
fn store(latest: Option<&str>) -> Option<UpdateStatus> {
    let path = settings::path()?;
    let mut stored = settings::load_from(&path);
    stored.last_update_check = now_secs();
    if let Some(latest) = latest {
        stored.latest_version = latest.to_string();
    }
    // Reported and then ignored. The answer is still true even if it
    // could not be written down; it only costs the next launch a request.
    if let Err(e) = settings::save_to(&path, &stored) {
        eprintln!("[update] could not store the check: {e}");
    }
    Some(status_from(&stored))
}

fn now_secs() -> u64 {
    let epoch = SystemTime::now().duration_since(UNIX_EPOCH);
    epoch.map_or(0, |since| since.as_secs())
}

/// Whether `candidate` is a later version than `current`.
///
/// Compared a number at a time rather than as text, because as text
/// `1.0.10` sorts before `1.0.9` and an update would go unnoticed for
/// every release after the ninth.
fn is_newer(candidate: &str, current: &str) -> bool {
    let candidate = numbers(candidate);
    let current = numbers(current);
    // Nothing to compare. An empty string reads as version zero, which
    // is what a settings file that has never held a check says.
    if candidate.iter().all(|part| *part == 0) {
        return false;
    }
    for i in 0..candidate.len().max(current.len()) {
        // A version with fewer parts than the other is padded rather than
        // cut short, so 1.1 is newer than 1.0.9 and not older than it.
        let a = candidate.get(i).copied().unwrap_or(0);
        let b = current.get(i).copied().unwrap_or(0);
        if a != b {
            return a > b;
        }
    }
    false
}

/// The numbers in a version, one per dot-separated part.
///
/// Only the digits a part starts with are read. A prerelease tag should
/// never arrive here, since the endpoint above excludes them, and taking
/// the digits is what keeps an unexpected one from reading as zero and
/// hiding a release that really is newer.
fn numbers(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        })
        .collect()
}

/// Whether Homebrew installed this copy.
///
/// The cask moves the bundle into `/Applications` like the script does,
/// so the bundle itself says nothing. What says so is the record brew
/// keeps beside it, and running the script over the top would leave that
/// record describing a copy brew no longer controls.
///
/// Looked for rather than asked of `brew`, which is a process spawn and a
/// question about PATH for something that is one stat call.
fn managed_by_homebrew() -> bool {
    caskroom_entry_in(&homebrew_prefixes())
}

fn caskroom_entry_in(prefixes: &[PathBuf]) -> bool {
    prefixes.iter().any(|prefix| caskroom(prefix).is_dir())
}

/// Where brew records a cask it has installed.
fn caskroom(prefix: &Path) -> PathBuf {
    prefix.join("Caskroom").join(CASK)
}

/// Where Homebrew might be installed.
///
/// Its own variable first, then the two standard prefixes: Apple silicon
/// installs under `/opt/homebrew` and Intel under `/usr/local`.
fn homebrew_prefixes() -> Vec<PathBuf> {
    let mut prefixes = Vec::new();
    if let Some(prefix) = std::env::var_os("HOMEBREW_PREFIX") {
        prefixes.push(PathBuf::from(prefix));
    }
    prefixes.push(PathBuf::from("/opt/homebrew"));
    prefixes.push(PathBuf::from("/usr/local"));
    prefixes
}

/// The bundle this process is running from.
///
/// `open` wants the `.app`, and the executable inside one is three levels
/// down it. A development build is not in a bundle at all, which is why
/// this can answer nothing.
fn bundle_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .find(|dir| dir.extension().is_some_and(|kind| kind == "app"))
        .map(Path::to_path_buf)
}

/// The line typed into a session to install an update.
///
/// `trap '' HUP` is the part that has to be there. The script asks the
/// app to quit before it swaps the bundle, and the app quitting closes
/// the terminal this is running in, which delivers SIGHUP to everything
/// in it. An ignored signal survives both fork and exec, so trapping it
/// here covers curl, the script, and everything the script runs: the
/// bundle is replaced even though the terminal it started from is gone.
/// Without it the script dies somewhere between removing the old copy and
/// moving the new one in, and that leaves no app installed at all.
///
/// `open` at the end is what brings the app back, and it runs whether or
/// not the script succeeded: a failed download never quits anything, so
/// there it merely raises the window that is already there.
fn command_line(bundle: Option<&Path>) -> String {
    let app = bundle
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|path| is_quotable(path))
        .unwrap_or_else(|| APP_NAME.to_string());
    format!("sh -c \"trap '' HUP; curl -fsSL {INSTALL_SCRIPT} | sh; open -a '{app}'\"")
}

/// Whether a path can sit inside the quotes above and still mean itself.
///
/// A bundle somewhere with one of these in the path is not a case worth
/// handling; it is a case worth not getting wrong, and the app's own name
/// is a good enough answer for `open`.
fn is_quotable(path: &str) -> bool {
    !path.contains(['\'', '"', '$', '`', '\\', '\n'])
}

/// What the interface knows about releases, without asking anybody.
#[tauri::command]
pub fn update_status() -> UpdateStatus {
    status_from(&settings::load())
}

/// Put the notice away until there is a newer release than this one.
#[tauri::command]
pub fn dismiss_update_notice() -> Result<(), String> {
    let path = settings::path().ok_or("no home directory to store settings in")?;
    let mut stored = settings::load_from(&path);
    if stored.update_notice_seen == stored.latest_version {
        return Ok(());
    }
    stored.update_notice_seen = stored.latest_version.clone();
    settings::save_to(&path, &stored)
}

/// The command that installs an update, for the sheet to run in a session.
///
/// Refused rather than adapted in the two cases where running it would be
/// wrong. Neither is reachable from the interface, which offers no button
/// in either case, so this is the second answer rather than the first.
#[tauri::command]
pub fn update_command() -> Result<String, String> {
    if !installable() {
        return Err("the installer only runs on macOS".into());
    }
    if managed_by_homebrew() {
        return Err(format!(
            "Homebrew installed this copy, so run brew upgrade --cask {CASK} to update it"
        ));
    }
    Ok(command_line(bundle_path().as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_later_release_is_noticed() {
        assert!(is_newer("1.0.4", "1.0.3"));
        assert!(is_newer("1.1.0", "1.0.9"));
        assert!(is_newer("2.0.0", "1.9.9"));
    }

    #[test]
    fn the_same_release_is_not_an_update() {
        assert!(!is_newer("1.0.3", "1.0.3"));
        assert!(!is_newer("1.0.2", "1.0.3"));
        assert!(!is_newer("0.9.9", "1.0.0"));
    }

    #[test]
    fn versions_are_compared_as_numbers_and_not_as_text() {
        // The whole reason this is not a string comparison: as text
        // "1.0.10" sorts before "1.0.9", so every release after the ninth
        // would go unnoticed.
        assert!(is_newer("1.0.10", "1.0.9"));
        assert!(!is_newer("1.0.9", "1.0.10"));
        assert!(is_newer("1.10.0", "1.9.0"));
    }

    #[test]
    fn a_version_with_fewer_parts_is_padded_rather_than_cut_short() {
        assert!(is_newer("1.1", "1.0.9"));
        assert!(!is_newer("1.0", "1.0.1"));
        assert!(!is_newer("1.0", "1.0.0"));
    }

    #[test]
    fn nothing_stored_yet_is_not_an_update() {
        // What a settings file that has never held a successful check
        // says, and it must not read as a release older than every real
        // one and so as news.
        assert!(!is_newer("", "1.0.3"));
        assert!(!is_newer("not a version", "1.0.3"));
    }

    #[test]
    fn the_leading_v_on_a_tag_is_dropped() {
        // Releases are tagged v1.0.3 and the app reports 1.0.3, so
        // comparing the two as they come would make every release look
        // like nonsense.
        assert_eq!(release_version("v1.0.4"), "1.0.4");
        assert_eq!(release_version(" v1.0.4\n"), "1.0.4");
        assert_eq!(release_version("1.0.4"), "1.0.4");
    }

    #[test]
    fn the_tag_is_read_out_of_a_releases_answer() {
        // Trimmed from a real one. The answer carries several dozen
        // fields and this wants the one.
        let body = r#"{
            "url": "https://api.github.com/repos/egeyesss/overterm/releases/1",
            "tag_name": "v1.0.4",
            "name": "1.0.4",
            "prerelease": false
        }"#;
        assert_eq!(tag_from(body).as_deref(), Some("1.0.4"));
    }

    #[test]
    fn an_answer_that_is_not_a_release_is_no_answer() {
        // What a rate limit replies with, what a proxy serving a login
        // page replies with, and what a connection cut halfway leaves.
        assert_eq!(tag_from(r#"{"message": "API rate limit exceeded"}"#), None);
        assert_eq!(tag_from("<html>not json</html>"), None);
        assert_eq!(tag_from(r#"{"tag_name": "v"}"#), None);
    }

    #[test]
    fn a_check_is_due_once_the_stored_one_is_old_enough() {
        let mut stored = settings::Settings::default();
        assert!(due(&stored, 1_000_000), "a file with no check in it yet");

        stored.last_update_check = 1_000_000;
        assert!(!due(&stored, 1_000_000 + CHECK_EVERY_SECS - 1));
        assert!(due(&stored, 1_000_000 + CHECK_EVERY_SECS));
    }

    #[test]
    fn a_clock_that_went_backwards_does_not_stop_the_checks() {
        // Otherwise a machine whose clock was wrong and then corrected
        // would never ask again until real time caught up with the
        // stored one, which can be years.
        let stored = settings::Settings {
            last_update_check: 2_000_000,
            ..settings::Settings::default()
        };
        assert!(due(&stored, 1_000_000));
    }

    #[test]
    fn the_status_says_nothing_is_available_until_a_check_has_run() {
        let status = status_from(&settings::Settings::default());
        assert_eq!(status.current, crate::app_version());
        assert!(status.latest.is_empty());
        assert!(!status.available);
        assert!(!status.dismissed, "there is nothing to have put away");
    }

    #[test]
    fn a_newer_release_reads_as_available_until_it_is_put_away() {
        let mut stored = settings::Settings {
            latest_version: "99.0.0".into(),
            ..settings::Settings::default()
        };
        let status = status_from(&stored);
        assert!(status.available);
        assert!(!status.dismissed);

        stored.update_notice_seen = "99.0.0".into();
        assert!(status_from(&stored).dismissed, "put away for this version");

        // And said again when there is something newer to say.
        stored.latest_version = "99.0.1".into();
        assert!(!status_from(&stored).dismissed);
    }

    #[test]
    fn the_update_command_outlives_the_app_it_replaces() {
        // The failure this is guarding: install.sh quits oTerm before it
        // swaps the bundle, the quit closes the terminal the script is
        // running in, and without an ignored SIGHUP the script dies
        // between removing the old copy and moving the new one in.
        let line = command_line(Some(Path::new("/Applications/oTerm.app")));
        assert!(line.contains("trap '' HUP"), "got: {line}");
        assert!(line.contains(INSTALL_SCRIPT), "got: {line}");
        assert!(
            line.contains("open -a '/Applications/oTerm.app'"),
            "the app has to come back once the swap is done: {line}"
        );
    }

    #[test]
    fn a_path_that_would_break_the_quoting_falls_back_to_the_name() {
        let line = command_line(Some(Path::new("/tmp/it's here/oTerm.app")));
        assert!(line.contains("open -a 'oTerm'"), "got: {line}");
        // A development build is not in a bundle at all.
        assert!(command_line(None).contains("open -a 'oTerm'"));
    }

    #[test]
    fn homebrew_is_recognised_by_the_record_it_keeps() {
        let dir = std::env::temp_dir().join("overterm-update-test");
        let prefix = dir.join("prefix");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&prefix).expect("create scratch prefix");
        let prefixes = vec![prefix.clone()];

        assert!(!caskroom_entry_in(&prefixes), "no cask, so no brew");

        std::fs::create_dir_all(caskroom(&prefix)).expect("create the cask entry");
        assert!(caskroom_entry_in(&prefixes));
    }
}
