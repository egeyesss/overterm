//! Record a timestamped PTY transcript for use as a detection fixture.
//!
//! Usage:
//!   record <out.ndjson> <script.txt> <timeout_ms> <cols> <rows> <cmd> [args...]
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
    if args.len() < 6 {
        eprintln!(
            "usage: record <out.ndjson> <script.txt> <timeout_ms> <cols> <rows> <cmd> [args...]"
        );
        std::process::exit(2);
    }
    let out_path = PathBuf::from(&args[0]);
    let script = parse_script(Path::new(&args[1]));
    let timeout = Duration::from_millis(args[2].parse().expect("bad timeout"));
    let cols: u16 = args[3].parse().expect("bad cols");
    let rows: u16 = args[4].parse().expect("bad rows");
    let command = args[5].clone();
    let cmd_args = args[6..].to_vec();

    let config = SpawnConfig {
        command: Some(command),
        args: cmd_args,
        cwd: Some(std::env::current_dir().expect("cwd")),
        cols,
        rows,
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
    // Bytes of the current script entry still to be typed. Input goes out
    // one byte per loop pass (~25ms apart) so it reads as human typing.
    // TUIs treat a whole line arriving in one chunk as a paste, and paste
    // handling can swallow the trailing enter instead of submitting.
    let mut pending: std::collections::VecDeque<u8> = std::collections::VecDeque::new();
    let mut child_gone = false;

    while start.elapsed() < timeout && !child_gone {
        if pending.is_empty()
            && let Some(&(t, _)) = script_iter.peek()
            && start.elapsed() >= Duration::from_millis(t)
        {
            let (_, bytes) = script_iter.next().unwrap();
            pending.extend(bytes);
        }
        if let Some(byte) = pending.pop_front() {
            session.write(&[byte]).expect("write input");
            events.push(Event {
                t_ms: start.elapsed().as_millis() as u64,
                dir: Dir::Input,
                bytes: vec![byte],
            });
        }
        match rx.recv_timeout(Duration::from_millis(25)) {
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
