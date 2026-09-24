//! Listing the user's Codex threads for the picker.
//!
//! The desktop app's bus has no request for this, so it goes through
//! `codex app-server`, the documented API. `thread/list` only reads the
//! state database, so running a second server for it does not compete with
//! the desktop app for any thread.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

const LIST_TIMEOUT: Duration = Duration::from_secs(10);
const THREAD_LIMIT: u64 = 30;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    pub id: String,
    pub title: String,
    /// Last component of the thread's working directory.
    pub workspace: Option<String>,
    /// Unix seconds.
    pub updated_at: Option<u64>,
}

/// Where the `codex` binary is, looked for in the app bundles first.
///
/// The person this is for may only have the desktop app, which carries the
/// CLI inside its bundle and never puts it on PATH. An app launched from
/// Finder also gets a minimal PATH, so a Homebrew or npm install is looked
/// for by directory as well.
pub fn codex_binary(home: Option<&Path>, path_var: Option<&OsStr>) -> Option<PathBuf> {
    const BUNDLED: &str = "Contents/Resources/codex";
    let app_dirs = home
        .map(|home| home.join("Applications"))
        .into_iter()
        .chain([PathBuf::from("/Applications")]);
    let bundled = app_dirs
        .flat_map(|dir| ["Codex.app", "ChatGPT.app"].map(|app| dir.join(app).join(BUNDLED)));
    let on_path = path_var
        .map(|paths| std::env::split_paths(paths).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .chain([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ])
        .map(|dir| dir.join("codex"));
    bundled.chain(on_path).find(|candidate| candidate.is_file())
}

pub fn list_threads(
    binary: &Path,
    search_term: Option<&str>,
) -> Result<Vec<ThreadSummary>, String> {
    let mut child = Command::new(binary)
        .args(["app-server", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", binary.display()))?;
    let result = converse(&mut child, search_term);
    // Closing stdin is how app-server is asked to leave; the kill is for
    // one that does not.
    drop(child.stdin.take());
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn converse(child: &mut Child, search_term: Option<&str>) -> Result<Vec<ThreadSummary>, String> {
    let mut stdin = child.stdin.take().ok_or("app-server has no stdin")?;
    let stdout = child.stdout.take().ok_or("app-server has no stdout")?;
    let (lines, incoming) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + LIST_TIMEOUT;
    let mut send = |message: Value| {
        writeln!(stdin, "{message}")
            .and_then(|_| stdin.flush())
            .map_err(|e| format!("app-server closed its input: {e}"))
    };
    let reply_to = |id: u64| -> Result<Value, String> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = incoming
                .recv_timeout(remaining)
                .map_err(|_| "app-server did not answer in time".to_string())?;
            // Notifications arrive in between and are not ours to read.
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                let why = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                return Err(format!("app-server refused: {why}"));
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    };

    send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "clientInfo": {"name": "overterm", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"experimentalApi": false},
        }}),
    )?;
    reply_to(1)?;
    send(json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}))?;
    let mut params = json!({
        "limit": THREAD_LIMIT,
        "archived": false,
        "sortKey": "updated_at",
        "useStateDbOnly": true,
    });
    if let Some(search_term) = search_term.filter(|term| !term.is_empty()) {
        params["searchTerm"] = json!(search_term);
    }
    send(json!({"jsonrpc": "2.0", "id": 2, "method": "thread/list", "params": params}))?;
    let listed = reply_to(2)?;
    Ok(listed
        .get("data")
        .and_then(Value::as_array)
        .map(|threads| threads.iter().filter_map(summary).collect())
        .unwrap_or_default())
}

fn summary(thread: &Value) -> Option<ThreadSummary> {
    let text = |key: &str| thread.get(key).and_then(Value::as_str);
    let title = text("name")
        .filter(|name| !name.trim().is_empty())
        .or_else(|| text("preview").and_then(|preview| preview.lines().next()))
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("Untitled thread");
    Some(ThreadSummary {
        id: text("id")?.to_owned(),
        title: title.chars().take(120).collect(),
        workspace: text("cwd")
            .and_then(|cwd| Path::new(cwd).file_name())
            .map(|name| name.to_string_lossy().into_owned()),
        updated_at: thread.get("updatedAt").and_then(Value::as_u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("overterm-codex-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_bundle_in_the_home_applications_folder_is_found() {
        let home = scratch("home-bundle");
        let bundled = home.join("Applications/ChatGPT.app/Contents/Resources/codex");
        executable(&bundled);
        assert_eq!(codex_binary(Some(&home), None), Some(bundled));
    }

    #[test]
    fn falls_back_to_path_when_no_bundle_exists() {
        let home = scratch("path-fallback");
        let bin = home.join("bin");
        executable(&bin.join("codex"));
        let path_var = std::env::join_paths([bin.clone()]).unwrap();
        // Only meaningful where /Applications has no Codex bundle; on a
        // machine that has one, the bundle rightly wins.
        let found = codex_binary(Some(&home), Some(&path_var)).unwrap();
        assert!(found == bin.join("codex") || found.starts_with("/Applications"));
    }

    #[test]
    fn nothing_found_is_none() {
        let home = scratch("nothing");
        let empty = std::env::join_paths([home.join("empty")]).unwrap();
        let found = codex_binary(Some(&home), Some(&empty));
        assert!(found.is_none() || found.unwrap().starts_with("/Applications"));
    }

    #[test]
    fn summary_prefers_the_name_then_the_first_line_of_the_preview() {
        let named = summary(&json!({
            "id": "t1", "name": "Test PR", "preview": "ignored", "cwd": "/Users/someone/work/project",
            "updatedAt": 1790197668
        }))
        .unwrap();
        assert_eq!(
            named,
            ThreadSummary {
                id: "t1".into(),
                title: "Test PR".into(),
                workspace: Some("project".into()),
                updated_at: Some(1790197668),
            }
        );
        let unnamed =
            summary(&json!({"id": "t2", "name": null, "preview": "fix the build\nsecond line"}))
                .unwrap();
        assert_eq!(unnamed.title, "fix the build");
        assert_eq!(
            summary(&json!({"id": "t3"})).unwrap().title,
            "Untitled thread"
        );
        assert!(summary(&json!({"name": "no id"})).is_none());
    }

    /// A stand-in app-server: answers the handshake and one `thread/list`,
    /// and only after reading each request, so it cannot race the client.
    #[cfg(unix)]
    #[test]
    fn lists_threads_through_a_fake_app_server() {
        let dir = scratch("fake-server");
        let script = dir.join("codex");
        std::fs::write(
            &script,
            concat!(
                "#!/bin/sh\n",
                "read _; printf '%s\\n' '{\"method\":\"remoteControl/status/changed\",\"params\":{}}' ",
                "'{\"id\":1,\"result\":{\"platformOs\":\"macos\"}}'\n",
                "read _; read _; printf '%s\\n' '{\"id\":2,\"result\":{\"data\":[",
                "{\"id\":\"t1\",\"name\":\"Test PR\",\"cwd\":\"/w/project\",\"updatedAt\":5}]}}'\n",
                "read _\n"
            ),
        )
        .unwrap();
        executable_mode(&script);
        let threads = list_threads(&script, None).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].title, "Test PR");
    }

    #[cfg(unix)]
    #[test]
    fn an_error_reply_is_reported() {
        let dir = scratch("fake-error");
        let script = dir.join("codex");
        std::fs::write(
            &script,
            concat!(
                "#!/bin/sh\n",
                "read _; printf '%s\\n' '{\"id\":1,\"result\":{}}'\n",
                "read _; read _; printf '%s\\n' '{\"id\":2,\"error\":{\"message\":\"state db locked\"}}'\n",
            ),
        )
        .unwrap();
        executable_mode(&script);
        let error = list_threads(&script, None).unwrap_err();
        assert!(error.contains("state db locked"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn passes_a_search_term_to_the_thread_list_request() {
        let dir = scratch("search-server");
        let script = dir.join("codex");
        std::fs::write(
            &script,
            concat!(
                "#!/bin/sh\n",
                "read _; printf '%s\\n' '{\"id\":1,\"result\":{}}'\n",
                "read _; read request\n",
                "case \"$request\" in\n",
                "  *searchTerm*older*) printf '%s\\n' '{\"id\":2,\"result\":{\"data\":[]}}' ;;\n",
                "  *) printf '%s\\n' '{\"id\":2,\"error\":{\"message\":\"search term missing\"}}' ;;\n",
                "esac\n",
                "read _\n"
            ),
        )
        .unwrap();
        executable_mode(&script);
        assert!(list_threads(&script, Some("older")).unwrap().is_empty());
    }

    #[cfg(unix)]
    fn executable_mode(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}
