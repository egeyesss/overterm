//! Reading exact state changes out of the terminal stream itself.
//!
//! Claude Code hooks can return a `terminalSequence`, which claude then
//! writes to the terminal on the hook's behalf. A hook that returns one
//! fixed escape sequence per event turns every event into a marker in the
//! byte stream this crate already reads. The session a marker belongs to
//! is the session it arrives on, so there is nothing to listen on,
//! nothing to authenticate and nothing to route. It also survives the app
//! restarting, and reaches us from the far end of an ssh connection.
//!
//! Claude only passes OSC 0, 1, 2, 9, 99, 777 and a bare bell through to
//! the terminal; anything else in that field is dropped. Of those, 777 is
//! the one no terminal acts on unless the payload names its `notify`
//! module. `overterm` does not, so the marker is invisible everywhere
//! except here.

use std::time::Instant;

use super::{Adapter, Signal};

/// Everything before the event name. `ESC ] 777 ;` opens the sequence.
pub const MARKER_PREFIX: &[u8] = b"\x1b]777;overterm;";

/// Bell closes it. Claude writes the string as given, and what we install
/// ends in a bell, so the other legal terminator is not worth the code:
/// treating an escape as the start of one would let a run of junk swallow
/// the marker after it.
const MARKER_END: u8 = 0x07;

const ESC: u8 = 0x1b;

/// Longest event name accepted before a candidate is given up on. Keeps a
/// stray `ESC ] 777 ;` in someone else's output from collecting bytes
/// forever.
const MAX_EVENT: usize = 32;

/// The user handed the program something to do.
pub const EVENT_SUBMIT: &str = "submit";
/// The program finished the work it was given.
pub const EVENT_STOP: &str = "stop";
/// The work ended on an error instead of an answer.
pub const EVENT_STOP_FAILURE: &str = "stop-failure";
/// The program is waiting on the user.
pub const EVENT_PERMISSION: &str = "permission";

/// Every event a hook is installed for, in the order they are installed.
pub const EVENTS: [&str; 4] = [
    EVENT_SUBMIT,
    EVENT_STOP,
    EVENT_STOP_FAILURE,
    EVENT_PERMISSION,
];

/// The bytes a hook puts on the wire for `event`.
///
/// The installer writes this sequence into the settings file and the
/// adapter reads it back off the terminal, so both sides are built from
/// here and cannot drift apart.
pub fn marker(event: &str) -> Vec<u8> {
    let mut bytes = MARKER_PREFIX.to_vec();
    bytes.extend_from_slice(event.as_bytes());
    bytes.push(MARKER_END);
    bytes
}

/// Turns hook markers in the output stream into signals.
///
/// Holds no clock and no transport, so it is exactly as testable as the
/// byte slices fed to it.
#[derive(Default)]
pub struct HookAdapter {
    /// A marker seen so far, empty unless part way through one. Kept
    /// across calls because a marker can be split over two reads.
    partial: Vec<u8>,
}

impl HookAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, byte: u8) -> Option<Signal> {
        if self.partial.is_empty() {
            if byte == ESC {
                self.partial.push(byte);
            }
            return None;
        }

        // Still working through the fixed part.
        if self.partial.len() < MARKER_PREFIX.len() {
            if byte == MARKER_PREFIX[self.partial.len()] {
                self.partial.push(byte);
            } else {
                self.restart(byte);
            }
            return None;
        }

        // Into the event name.
        match byte {
            MARKER_END => {
                let event = self.partial.split_off(MARKER_PREFIX.len());
                self.partial.clear();
                signal_for(&event)
            }
            ESC => {
                self.restart(byte);
                None
            }
            _ if self.partial.len() - MARKER_PREFIX.len() >= MAX_EVENT => {
                self.restart(byte);
                None
            }
            _ => {
                self.partial.push(byte);
                None
            }
        }
    }

    /// Give up on the candidate. The byte that broke it can still be the
    /// start of the next one.
    fn restart(&mut self, byte: u8) {
        self.partial.clear();
        if byte == ESC {
            self.partial.push(byte);
        }
    }
}

fn signal_for(event: &[u8]) -> Option<Signal> {
    // An event this build does not know is dropped, so a newer OverTerm
    // can install more of them without confusing an older one.
    match std::str::from_utf8(event).ok()? {
        EVENT_SUBMIT => Some(Signal::HookSubmit),
        EVENT_STOP => Some(Signal::HookStop),
        EVENT_STOP_FAILURE => Some(Signal::HookStopFailure),
        EVENT_PERMISSION => Some(Signal::HookNotification),
        _ => None,
    }
}

impl Adapter for HookAdapter {
    fn feed(&mut self, bytes: &[u8], _now: Instant) -> Vec<Signal> {
        bytes.iter().filter_map(|&byte| self.push(byte)).collect()
    }

    fn tick(&mut self, _now: Instant) -> Vec<Signal> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(adapter: &mut HookAdapter, bytes: &[u8]) -> Vec<Signal> {
        adapter.feed(bytes, Instant::now())
    }

    #[test]
    fn every_installed_event_reads_back_as_a_signal() {
        for event in EVENTS {
            let mut a = HookAdapter::new();
            assert_eq!(
                feed(&mut a, &marker(event)).len(),
                1,
                "no signal for {event:?}"
            );
        }
    }

    #[test]
    fn each_event_means_what_it_says() {
        let mut a = HookAdapter::new();
        assert_eq!(
            feed(&mut a, &marker(EVENT_SUBMIT)),
            vec![Signal::HookSubmit]
        );
        assert_eq!(feed(&mut a, &marker(EVENT_STOP)), vec![Signal::HookStop]);
        assert_eq!(
            feed(&mut a, &marker(EVENT_STOP_FAILURE)),
            vec![Signal::HookStopFailure]
        );
        assert_eq!(
            feed(&mut a, &marker(EVENT_PERMISSION)),
            vec![Signal::HookNotification]
        );
    }

    #[test]
    fn a_marker_split_across_reads_still_arrives() {
        // Terminal output arrives in whatever sized chunks the kernel
        // hands over, so a marker is routinely cut in half.
        let bytes = marker(EVENT_STOP);
        for split in 1..bytes.len() {
            let mut a = HookAdapter::new();
            let mut signals = feed(&mut a, &bytes[..split]);
            signals.extend(feed(&mut a, &bytes[split..]));
            assert_eq!(
                signals,
                vec![Signal::HookStop],
                "lost the marker when split at {split}"
            );
        }
    }

    #[test]
    fn a_marker_split_one_byte_at_a_time_still_arrives() {
        let mut a = HookAdapter::new();
        let mut signals = Vec::new();
        for byte in marker(EVENT_SUBMIT) {
            signals.extend(feed(&mut a, &[byte]));
        }
        assert_eq!(signals, vec![Signal::HookSubmit]);
    }

    #[test]
    fn markers_buried_in_ordinary_output_are_found() {
        // What actually arrives: claude repaints its window title and
        // draws its interface around the marker.
        let mut a = HookAdapter::new();
        let mut stream = b"\x1b]0;\xe2\x9c\xb3 Claude Code\x07\x1b[?25l".to_vec();
        stream.extend(marker(EVENT_STOP));
        stream.extend_from_slice(b"\x1b[?2026h\x1b[H\r\x1b[24B");
        assert_eq!(feed(&mut a, &stream), vec![Signal::HookStop]);
    }

    #[test]
    fn back_to_back_markers_both_arrive() {
        let mut a = HookAdapter::new();
        let mut stream = marker(EVENT_PERMISSION);
        stream.extend(marker(EVENT_STOP));
        assert_eq!(
            feed(&mut a, &stream),
            vec![Signal::HookNotification, Signal::HookStop]
        );
    }

    #[test]
    fn other_escape_sequences_are_left_alone() {
        let mut a = HookAdapter::new();
        // A window title, a hyperlink, a different OSC 777 module, and
        // the cursor position query claude sends several times a second.
        let noise = b"\x1b]0;title\x07\x1b]8;;https://example.com\x07\
                      \x1b]777;notify;Someone Else;hello\x07\x1b[?6n";
        assert!(feed(&mut a, noise).is_empty());
    }

    #[test]
    fn an_event_this_build_does_not_know_is_ignored() {
        let mut a = HookAdapter::new();
        assert!(feed(&mut a, &marker("compacted")).is_empty());
        // And the adapter is still able to read the next one.
        assert_eq!(feed(&mut a, &marker(EVENT_STOP)), vec![Signal::HookStop]);
    }

    #[test]
    fn an_unterminated_marker_does_not_collect_bytes_forever() {
        let mut a = HookAdapter::new();
        let mut stream = MARKER_PREFIX.to_vec();
        stream.extend(std::iter::repeat_n(b'x', MAX_EVENT * 4));
        assert!(feed(&mut a, &stream).is_empty());
        assert!(a.partial.len() <= MARKER_PREFIX.len() + MAX_EVENT);
        assert_eq!(feed(&mut a, &marker(EVENT_STOP)), vec![Signal::HookStop]);
    }

    #[test]
    fn a_marker_interrupted_by_another_sequence_is_dropped_not_merged() {
        let mut a = HookAdapter::new();
        let mut stream = MARKER_PREFIX.to_vec();
        stream.extend_from_slice(b"sto");
        stream.extend_from_slice(b"\x1b[?25l");
        stream.extend(marker(EVENT_SUBMIT));
        assert_eq!(feed(&mut a, &stream), vec![Signal::HookSubmit]);
    }

    #[test]
    fn the_adapter_never_claims_to_know_the_session_is_working() {
        // The turn a marker starts is tracked by the detector, which sees
        // every signal. Two copies of that fact could disagree.
        let a = HookAdapter::new();
        assert!(!a.is_working(Instant::now()));
    }
}
