//! Recording and replaying PTY transcripts.
//!
//! A transcript is NDJSON, one event per line, with millisecond offsets
//! from session start and hex payloads:
//!
//! ```text
//! {"t":120,"d":"o","x":"6c730d0a"}
//! {"t":903,"d":"i","x":"0d"}
//! ```
//!
//! Replay drives a `Detector` with a synthetic clock, so tests run the
//! full recorded timeline in microseconds and stay deterministic.

use std::path::Path;
use std::time::{Duration, Instant};

use super::{Detector, StateChange};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Output,
    Input,
    /// Terminal resize; payload is ASCII "<cols>x<rows>".
    Resize,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub t_ms: u64,
    pub dir: Dir,
    pub bytes: Vec<u8>,
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("odd hex length".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// Write one event as an NDJSON line. Used by the fixture writer and by
/// live session capture.
pub fn append_event(out: &mut impl std::io::Write, ev: &Event) -> std::io::Result<()> {
    let d = match ev.dir {
        Dir::Output => "o",
        Dir::Input => "i",
        Dir::Resize => "r",
    };
    writeln!(
        out,
        "{}",
        serde_json::json!({ "t": ev.t_ms, "d": d, "x": hex_encode(&ev.bytes) })
    )
}

pub fn write_fixture(path: &Path, events: &[Event]) -> std::io::Result<()> {
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    for ev in events {
        append_event(&mut out, ev)?;
    }
    Ok(())
}

pub fn read_fixture(path: &Path) -> Result<Vec<Event>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut events = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("line {}: {e}", i + 1))?;
        let dir = match v["d"].as_str() {
            Some("o") => Dir::Output,
            Some("i") => Dir::Input,
            Some("r") => Dir::Resize,
            other => return Err(format!("line {}: bad direction {other:?}", i + 1)),
        };
        events.push(Event {
            t_ms: v["t"].as_u64().ok_or(format!("line {}: bad t", i + 1))?,
            dir,
            bytes: hex_decode(v["x"].as_str().ok_or(format!("line {}: bad x", i + 1))?)
                .map_err(|e| format!("line {}: {e}", i + 1))?,
        });
    }
    Ok(events)
}

/// Feed a recorded timeline through a detector, ticking every `tick_ms`
/// of transcript time, plus `trailing_ms` of silence at the end so a
/// final quiescence conclusion can fire. Each returned change is paired
/// with the transcript time at which it happened.
pub fn replay(
    detector: &mut Detector,
    events: &[Event],
    tick_ms: u64,
    trailing_ms: u64,
) -> Vec<(u64, StateChange)> {
    replay_with(detector, events, tick_ms, trailing_ms, |_, _, _| {})
}

/// Replay, calling `probe` with the elapsed time, the matching clock
/// reading and the detector on every tick. Lets a test watch something
/// other than the transitions, such as whether the session reads as
/// working partway through.
pub fn replay_with<F>(
    detector: &mut Detector,
    events: &[Event],
    tick_ms: u64,
    trailing_ms: u64,
    mut probe: F,
) -> Vec<(u64, StateChange)>
where
    F: FnMut(u64, Instant, &Detector),
{
    let base = Instant::now();
    let clock = |ms: u64| base + Duration::from_millis(ms);
    let mut changes: Vec<(u64, StateChange)> = Vec::new();
    let mut ticked_to = 0u64;

    for ev in events {
        while ticked_to + tick_ms <= ev.t_ms {
            ticked_to += tick_ms;
            let t = ticked_to;
            changes.extend(detector.tick(clock(t)).into_iter().map(|c| (t, c)));
            probe(t, clock(t), detector);
        }
        let now = clock(ev.t_ms);
        let produced = match ev.dir {
            Dir::Output => detector.feed_output(&ev.bytes, now),
            Dir::Input => detector.feed_input(&ev.bytes, now),
            Dir::Resize => {
                if let Some((cols, rows)) = parse_resize(&ev.bytes) {
                    detector.resize(cols, rows);
                }
                Vec::new()
            }
        };
        changes.extend(produced.into_iter().map(|c| (ev.t_ms, c)));
    }

    let end = events.last().map(|e| e.t_ms).unwrap_or(0) + trailing_ms;
    while ticked_to + tick_ms <= end {
        ticked_to += tick_ms;
        let t = ticked_to;
        changes.extend(detector.tick(clock(t)).into_iter().map(|c| (t, c)));
        probe(t, clock(t), detector);
    }
    changes
}

/// Decode a resize payload ("<cols>x<rows>").
pub fn parse_resize(bytes: &[u8]) -> Option<(u16, u16)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (cols, rows) = text.split_once('x')?;
    Some((cols.parse().ok()?, rows.parse().ok()?))
}

/// Encode a resize payload.
pub fn resize_payload(cols: u16, rows: u16) -> Vec<u8> {
    format!("{cols}x{rows}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_payload_round_trips() {
        assert_eq!(parse_resize(&resize_payload(120, 38)), Some((120, 38)));
    }

    #[test]
    fn hex_round_trips() {
        let data = vec![0u8, 27, 91, 255, 10];
        assert_eq!(hex_decode(&hex_encode(&data)).unwrap(), data);
    }

    #[test]
    fn fixture_file_round_trips() {
        let dir = std::env::temp_dir().join("overterm-replay-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rt.ndjson");
        let events = vec![
            Event {
                t_ms: 0,
                dir: Dir::Output,
                bytes: b"$ ".to_vec(),
            },
            Event {
                t_ms: 350,
                dir: Dir::Input,
                bytes: b"ls\r".to_vec(),
            },
            Event {
                t_ms: 400,
                dir: Dir::Output,
                bytes: b"\x1b[32mfile\x1b[0m\r\n$ ".to_vec(),
            },
        ];
        write_fixture(&path, &events).unwrap();
        let back = read_fixture(&path).unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(back[1].dir, Dir::Input);
        assert_eq!(back[2].bytes, events[2].bytes);
        std::fs::remove_file(&path).unwrap();
    }
}
