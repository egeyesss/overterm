//! Record a timestamped PTY transcript for use as a detection fixture.
//!
//! Usage:
//!   record <out.ndjson> <script.txt> <timeout_ms> <cmd> [args...]
//!
//! The script file drives input. Each non-empty line is:
//!   <t_ms> <text>
//! where text supports \r \n \t and \xNN escapes. At t_ms after spawn the
//! text is written to the child and logged as an input event. All output
//! is logged with its arrival time. Recording ends when the child exits
//! or the timeout is reached.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use overterm_core::detect::replay::{Dir, Event, write_fixture};
use overterm_core::session::{PtySession, SpawnConfig};

fn unescape(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('r') => out.push(b'\r'),
            Some('n') => out.push(b'\n'),
            Some('t') => out.push(b'\t'),
            Some('x') => {
                let hi = chars.next().expect("\\x needs two hex digits");
                let lo = chars.next().expect("\\x needs two hex digits");
                let byte = u8::from_str_radix(&format!("{hi}{lo}"), 16).expect("bad \\x escape");
                out.push(byte);
            }
            Some(other) => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
            None => {}
        }
    }
    out
}

fn parse_script(path: &Path) -> Vec<(u64, Vec<u8>)> {
    let text = std::fs::read_to_string(path).expect("read script");
    let mut entries = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (t, rest) = line.split_once(' ').expect("script line: <t_ms> <text>");
        entries.push((t.parse::<u64>().expect("bad t_ms"), unescape(rest)));
    }
    entries.sort_by_key(|&(t, _)| t);
    entries
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        eprintln!("usage: record <out.ndjson> <script.txt> <timeout_ms> <cmd> [args...]");
        std::process::exit(2);
    }
    let out_path = PathBuf::from(&args[0]);
    let script = parse_script(Path::new(&args[1]));
    let timeout = Duration::from_millis(args[2].parse().expect("bad timeout"));
    let command = args[3].clone();
    let cmd_args = args[4..].to_vec();

    let config = SpawnConfig {
        command: Some(command),
        args: cmd_args,
        cwd: Some(std::env::current_dir().expect("cwd")),
        cols: 100,
        rows: 30,
        ..Default::default()
    };
    let (mut session, output) = PtySession::spawn(config).expect("spawn");
    let start = Instant::now();

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let mut reader = output.reader;
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    let mut events: Vec<Event> = Vec::new();
    let mut script_iter = script.into_iter().peekable();
    let mut child_gone = false;

    while start.elapsed() < timeout && !child_gone {
        while let Some(&(t, _)) = script_iter.peek() {
            if start.elapsed() >= Duration::from_millis(t) {
                let (t, bytes) = script_iter.next().unwrap();
                session.write(&bytes).expect("write input");
                events.push(Event {
                    t_ms: t,
                    dir: Dir::Input,
                    bytes,
                });
            } else {
                break;
            }
        }
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(chunk) => events.push(Event {
                t_ms: start.elapsed().as_millis() as u64,
                dir: Dir::Output,
                bytes: chunk,
            }),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => child_gone = true,
        }
    }

    let _ = session.kill();
    write_fixture(&out_path, &events).expect("write fixture");
    eprintln!(
        "recorded {} events over {} ms -> {}",
        events.len(),
        start.elapsed().as_millis(),
        out_path.display()
    );
}
