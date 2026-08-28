//! PTY-backed terminal sessions.
//!
//! CLIs detect a non-TTY stdout and drop colors, spinners, and interactivity,
//! so piping is not an option — every session gets a real pseudo-terminal via
//! `portable-pty` (macOS/Linux/Windows-ConPTY behind one API).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;

use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use uuid::Uuid;

/// Sessions export this variable so Claude Code hooks can route their
/// signals back to the session that spawned them.
pub const SESSION_ENV_VAR: &str = "OVERTERM_SESSION";

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("failed to open pty: {0}")]
    OpenPty(#[source] anyhow::Error),
    #[error("failed to spawn command: {0}")]
    Spawn(#[source] anyhow::Error),
    #[error("pty i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to resize pty: {0}")]
    Resize(#[source] anyhow::Error),
}

/// What to run and how big the terminal starts out.
pub struct SpawnConfig {
    /// Program to run. `None` falls back to `$SHELL`, then `/bin/sh`.
    pub command: Option<String>,
    pub args: Vec<String>,
    /// Working directory. `None` falls back to `$HOME`, then `.`.
    pub cwd: Option<PathBuf>,
    /// Extra environment variables on top of the inherited environment.
    pub env: HashMap<String, String>,
    /// Inherited environment variables to remove before spawning.
    pub env_remove: Vec<String>,
    pub cols: u16,
    pub rows: u16,
}

impl Default for SpawnConfig {
    fn default() -> Self {
        Self {
            command: None,
            args: Vec::new(),
            cwd: None,
            env: HashMap::new(),
            env_remove: Vec::new(),
            cols: 80,
            rows: 24,
        }
    }
}

/// A live PTY session. Owns the master side of the PTY, the writer for
/// keystrokes, and a killer handle for the child process.
///
/// Dropping the session closes the master, which delivers SIGHUP to the
/// child's process group — same as closing a terminal tab.
pub struct PtySession {
    id: String,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

/// The read side of a session, handed to whoever pumps output (a reader
/// thread in the app). Split from `PtySession` so reading can block on its
/// own thread while writes and resizes happen elsewhere.
pub struct SessionOutput {
    pub reader: Box<dyn Read + Send>,
    /// Wait on this after the reader hits EOF to reap the child and get its
    /// exit code.
    pub child: Box<dyn Child + Send + Sync>,
}

impl PtySession {
    pub fn spawn(config: SpawnConfig) -> Result<(Self, SessionOutput), SessionError> {
        let id = Uuid::new_v4().to_string();

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: config.rows,
                cols: config.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(SessionError::OpenPty)?;

        let program = config
            .command
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/sh".into());

        let mut cmd = CommandBuilder::new(&program);
        cmd.args(&config.args);
        cmd.cwd(
            config.cwd.unwrap_or_else(|| {
                std::env::var_os("HOME").map_or_else(|| ".".into(), PathBuf::from)
            }),
        );
        // Full-color TUIs (Claude Code included) key off these.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env(SESSION_ENV_VAR, &id);
        for (key, value) in config.env {
            cmd.env(key, value);
        }
        for key in &config.env_remove {
            cmd.env_remove(key);
        }

        let child = pair.slave.spawn_command(cmd).map_err(SessionError::Spawn)?;
        // The child holds its own copy of the slave; dropping ours is what
        // lets the master reader see EOF when the child exits.
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| SessionError::Io(std::io::Error::other(e)))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| SessionError::Io(std::io::Error::other(e)))?;
        let killer = child.clone_killer();

        let session = Self {
            id,
            master: pair.master,
            writer,
            killer,
        };
        Ok((session, SessionOutput { reader, child }))
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Write keystrokes (or pasted text) to the child's stdin.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), SessionError> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Resize the PTY. The kernel delivers SIGWINCH to the child for us.
    /// Process id of whatever currently owns this terminal.
    ///
    /// A session is a shell, and the shell runs other programs in the
    /// same terminal, so this changes as the user starts and leaves
    /// things. It is the shell's own pid while sitting at a prompt and
    /// the agent's while one is running, which is what makes it a way to
    /// tell what a session is doing without asking the program.
    ///
    /// Unix only. Windows has no process group on a console handle, so a
    /// port answers this some other way and gets `None` until it does.
    pub fn foreground_pid(&self) -> Option<i32> {
        #[cfg(unix)]
        {
            self.master.process_group_leader()
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), SessionError> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(SessionError::Resize)
    }

    /// Force-kill the child process.
    pub fn kill(&mut self) -> Result<(), SessionError> {
        self.killer.kill()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// Pump the reader on a thread and collect output until `predicate`
    /// matches or the timeout hits. Returns everything read.
    fn read_until(
        mut reader: Box<dyn Read + Send>,
        timeout: Duration,
        predicate: impl Fn(&[u8]) -> bool,
    ) -> Vec<u8> {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });

        let mut collected = Vec::new();
        let deadline = Instant::now() + timeout;
        while !predicate(&collected) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(chunk) => collected.extend_from_slice(&chunk),
                Err(_) => break,
            }
        }
        collected
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack
            .windows(needle.len())
            .any(|w| w == needle.as_bytes())
    }

    fn sh(script: &str, cols: u16, rows: u16) -> SpawnConfig {
        SpawnConfig {
            command: Some("/bin/sh".into()),
            args: vec!["-c".into(), script.into()],
            cols,
            rows,
            ..Default::default()
        }
    }

    #[test]
    fn child_sees_a_real_tty() {
        // The entire point of the PTY layer: children must believe they are
        // on a terminal, or they drop colors and interactivity.
        let (_session, output) =
            PtySession::spawn(sh("test -t 0 && test -t 1 && echo IS-A-TTY", 80, 24)).unwrap();
        let out = read_until(output.reader, Duration::from_secs(5), |b| {
            contains(b, "IS-A-TTY")
        });
        assert!(
            contains(&out, "IS-A-TTY"),
            "child did not see a tty; output: {:?}",
            String::from_utf8_lossy(&out)
        );
    }

    #[test]
    fn keystrokes_round_trip_through_the_pty() {
        // Interactive shell: write a command in, read its output back.
        let (mut session, output) = PtySession::spawn(sh("cat", 80, 24)).unwrap();
        session.write(b"marker-roundtrip\n").unwrap();
        let out = read_until(output.reader, Duration::from_secs(5), |b| {
            contains(b, "marker-roundtrip")
        });
        assert!(
            contains(&out, "marker-roundtrip"),
            "input never came back; output: {:?}",
            String::from_utf8_lossy(&out)
        );
    }

    #[test]
    fn spawn_size_reaches_the_child() {
        let (_session, output) = PtySession::spawn(sh("stty size", 132, 43)).unwrap();
        let out = read_until(output.reader, Duration::from_secs(5), |b| {
            contains(b, "43 132")
        });
        assert!(
            contains(&out, "43 132"),
            "child saw wrong size; output: {:?}",
            String::from_utf8_lossy(&out)
        );
    }

    #[test]
    fn session_env_var_is_set_to_session_id() {
        let (session, output) =
            PtySession::spawn(sh("echo \"session=$OVERTERM_SESSION\"", 80, 24)).unwrap();
        let expected = format!("session={}", session.id());
        let out = read_until(output.reader, Duration::from_secs(5), |b| {
            contains(b, &expected)
        });
        assert!(
            contains(&out, &expected),
            "OVERTERM_SESSION missing or wrong; output: {:?}",
            String::from_utf8_lossy(&out)
        );
    }

    #[test]
    fn exit_code_is_reported() {
        let (_session, mut output) = PtySession::spawn(sh("exit 3", 80, 24)).unwrap();
        // Drain output so the child is not blocked writing, then reap it.
        let _ = read_until(output.reader, Duration::from_secs(5), |_| false);
        let status = output.child.wait().unwrap();
        assert_eq!(status.exit_code(), 3);
    }
}
