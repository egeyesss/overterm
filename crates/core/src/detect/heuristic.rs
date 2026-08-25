//! Fallback detection that works on any CLI with no integration at all.
//!
//! Busy is inferred from a burst of output. Done is inferred from silence:
//! once output has been quiet for a while and the screen row under the
//! cursor looks like a prompt, the command is considered finished.
//!
//! Output is fed through a terminal emulator (vt100) and the prompt check
//! runs against the resulting screen grid, anchored at the cursor. The
//! byte stream alone cannot answer "is a prompt showing": full-screen
//! TUIs repaint their input row on every frame, so the raw tail may end
//! with whatever region happened to paint last, while rows from old
//! frames linger earlier in the stream. The grid gives the actual screen,
//! and the cursor parks on the input row exactly when a program is ready
//! for input: a shell rests it after the prompt, and a silently running
//! command leaves it on the empty line below the echoed command.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use regex::Regex;

use super::{Adapter, Signal};

pub struct HeuristicConfig {
    /// Bytes within `burst_window` that count as a burst of output.
    pub burst_bytes: usize,
    pub burst_window: Duration,
    /// Silence required before the screen is checked for a prompt.
    pub quiet: Duration,
    /// Pattern that decides whether a row looks like a prompt.
    pub prompt_pattern: Regex,
    /// Rows to scan, from the cursor row upward.
    pub prompt_scan_rows: u16,
    /// Pattern that marks the screen as still working even though output
    /// paused. Needed for TUIs that stream in batches: between batches
    /// the cursor rests in their input box, which would otherwise read
    /// as a prompt. Claude Code shows "esc to interrupt" in its status
    /// area for the entire time it works.
    pub busy_pattern: Regex,
    /// Bottom rows of the screen scanned for `busy_pattern`.
    pub busy_scan_rows: u16,
    /// Initial terminal size for the screen model. Live sessions pass the
    /// real size and update it through `resize`.
    pub cols: u16,
    pub rows: u16,
}

impl Default for HeuristicConfig {
    fn default() -> Self {
        Self {
            burst_bytes: 200,
            burst_window: Duration::from_millis(300),
            quiet: Duration::from_millis(400),
            // Shell prompts ending in a marker character, plus the input
            // row Claude Code draws when it is ready (a "❯" or ">" with
            // nothing or box borders after it).
            prompt_pattern: Regex::new(r"[❯$%#]\s*$|^\s*[❯>]\s|╰")
                .expect("default prompt pattern is valid"),
            prompt_scan_rows: 2,
            busy_pattern: Regex::new(r"esc to interrupt").expect("default busy pattern is valid"),
            busy_scan_rows: 8,
            cols: 100,
            rows: 30,
        }
    }
}

/// Counts audible bells reported by the terminal parser.
#[derive(Default)]
struct BellCounter {
    bells: usize,
}

impl vt100::Callbacks for BellCounter {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bells += 1;
    }
}

pub struct HeuristicAdapter {
    cfg: HeuristicConfig,
    /// Screen model built from the output stream.
    parser: vt100::Parser<BellCounter>,
    seen_bells: usize,
    /// Recent output chunk sizes for burst detection.
    window: VecDeque<(Instant, usize)>,
    last_output: Option<Instant>,
    /// Output has arrived since the last quiescence conclusion, so a
    /// quiet-plus-prompt check is worth running.
    waiting_for_quiet: bool,
    /// A burst signal was already emitted for the current activity.
    burst_announced: bool,
    /// Signature of the screen after the last chunk, used to tell real
    /// display activity from output that changes nothing visible.
    screen_sig: u64,
    /// Set by OVERTERM_DETECT_DEBUG. Logs the screen state to stderr when
    /// the session is quiet but the prompt check keeps failing.
    debug: bool,
    last_debug: Option<Instant>,
}

impl HeuristicAdapter {
    pub fn new(cfg: HeuristicConfig) -> Self {
        let parser =
            vt100::Parser::new_with_callbacks(cfg.rows, cfg.cols, 0, BellCounter::default());
        let mut adapter = Self {
            cfg,
            parser,
            seen_bells: 0,
            window: VecDeque::new(),
            last_output: None,
            waiting_for_quiet: false,
            burst_announced: false,
            screen_sig: 0,
            debug: std::env::var_os("OVERTERM_DETECT_DEBUG").is_some(),
            last_debug: None,
        };
        adapter.screen_sig = adapter.screen_signature();
        adapter
    }

    fn screen_signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::hash::DefaultHasher::new();
        let screen = self.parser.screen();
        screen.contents().hash(&mut hasher);
        screen.cursor_position().hash(&mut hasher);
        screen.size().hash(&mut hasher);
        hasher.finish()
    }

    fn expire_window(&mut self, now: Instant) {
        while let Some(&(t, _)) = self.window.front() {
            if now.duration_since(t) > self.cfg.burst_window {
                self.window.pop_front();
            } else {
                break;
            }
        }
        if self.window.is_empty() {
            self.burst_announced = false;
        }
    }

    fn window_bytes(&self) -> usize {
        self.window.iter().map(|&(_, n)| n).sum()
    }

    fn row_text(&self, row: u16) -> String {
        let screen = self.parser.screen();
        let (_, cols) = screen.size();
        let mut text = String::new();
        for col in 0..cols {
            if let Some(cell) = screen.cell(row, col) {
                text.push_str(cell.contents());
            }
        }
        text
    }

    fn prompt_at_cursor(&self) -> bool {
        let (cursor_row, _) = self.parser.screen().cursor_position();
        let first = cursor_row.saturating_sub(self.cfg.prompt_scan_rows - 1);
        (first..=cursor_row).any(|row| {
            let text = self.row_text(row);
            !text.trim().is_empty() && self.cfg.prompt_pattern.is_match(&text)
        })
    }

    fn busy_hint_on_screen(&self) -> bool {
        let (rows, _) = self.parser.screen().size();
        let first = rows.saturating_sub(self.cfg.busy_scan_rows);
        (first..rows).any(|row| self.cfg.busy_pattern.is_match(&self.row_text(row)))
    }
}

impl Adapter for HeuristicAdapter {
    fn feed(&mut self, bytes: &[u8], now: Instant) -> Vec<Signal> {
        let mut signals = Vec::new();

        self.parser.process(bytes);
        let bells = self.parser.callbacks().bells;
        for _ in self.seen_bells..bells {
            signals.push(Signal::Bell);
        }
        self.seen_bells = bells;

        // Output that leaves the screen untouched is not activity. Claude
        // polls the terminal with cursor position queries several times a
        // second while idle, and counting those kept the quiet timer from
        // ever expiring. Identical repaints fall away for the same reason.
        let sig = self.screen_signature();
        if sig == self.screen_sig {
            return signals;
        }
        self.screen_sig = sig;

        self.expire_window(now);
        self.window.push_back((now, bytes.len()));
        self.last_output = Some(now);
        self.waiting_for_quiet = true;

        if !self.burst_announced && self.window_bytes() >= self.cfg.burst_bytes {
            self.burst_announced = true;
            signals.push(Signal::OutputBurst);
        }

        signals
    }

    fn tick(&mut self, now: Instant) -> Vec<Signal> {
        self.expire_window(now);
        if !self.waiting_for_quiet {
            return Vec::new();
        }
        let Some(last) = self.last_output else {
            return Vec::new();
        };
        if now.duration_since(last) < self.cfg.quiet {
            return Vec::new();
        }
        if self.busy_hint_on_screen() {
            if self.debug
                && self
                    .last_debug
                    .is_none_or(|t| now.duration_since(t) > Duration::from_secs(3))
            {
                self.last_debug = Some(now);
                eprintln!("[detect] quiet but busy hint on screen, holding");
            }
            return Vec::new();
        }
        if self.prompt_at_cursor() {
            self.waiting_for_quiet = false;
            self.burst_announced = false;
            return vec![Signal::Quiescence {
                quiet_ms: self.cfg.quiet.as_millis() as u64,
            }];
        }
        if self.debug
            && self
                .last_debug
                .is_none_or(|t| now.duration_since(t) > Duration::from_secs(3))
        {
            self.last_debug = Some(now);
            let (cur_row, cur_col) = self.parser.screen().cursor_position();
            let (rows, cols) = self.parser.screen().size();
            eprintln!(
                "[detect] quiet {}ms, no prompt. grid {rows}x{cols}, cursor ({cur_row},{cur_col})",
                now.duration_since(last).as_millis()
            );
            let first = cur_row.saturating_sub(3);
            for row in first..=cur_row.min(rows - 1) {
                eprintln!("[detect]   row {row}: {:?}", self.row_text(row).trim_end());
            }
        }
        Vec::new()
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.parser.screen_mut().set_size(rows, cols);
    }

    fn is_working(&self, now: Instant) -> bool {
        // A working indicator on screen is the strongest evidence there
        // is, and it is the whole reason `busy_pattern` exists.
        if self.busy_hint_on_screen() {
            return true;
        }
        // Otherwise the screen has to still be changing. A dialog waiting
        // for an answer paints once and then goes still, while work that
        // produces output keeps repainting.
        self.last_output
            .is_some_and(|t| now.duration_since(t) < self.cfg.quiet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    fn adapter() -> HeuristicAdapter {
        HeuristicAdapter::new(HeuristicConfig::default())
    }

    #[test]
    fn a_still_screen_is_not_working() {
        // A program that printed a question and is waiting for an answer.
        // The state machine still says Busy, but there is no evidence of
        // work, so the window must not hide the question.
        let base = Instant::now();
        let mut a = adapter();
        a.feed(
            b"Do you trust the files in this folder?\r\n  1. Yes\r\n",
            at(base, 0),
        );
        assert!(a.is_working(at(base, 100)));
        assert!(!a.is_working(at(base, 600)));
        assert!(!a.is_working(at(base, 30_000)));
    }

    #[test]
    fn a_working_hint_counts_even_while_output_pauses() {
        // Claude streams in batches with pauses between them, and keeps
        // its interrupt hint in the status area at the bottom of the
        // screen the whole time it works.
        let base = Instant::now();
        let mut a = adapter();
        let mut screen = "thinking...".to_string();
        screen.push_str(&"\r\n".repeat(28));
        screen.push_str("(esc to interrupt)");
        a.feed(screen.as_bytes(), at(base, 0));
        assert!(a.is_working(at(base, 5_000)));
    }

    #[test]
    fn burst_fires_once_per_activity() {
        let base = Instant::now();
        let mut a = adapter();
        let big = vec![b'x'; 300];
        let s1 = a.feed(&big, at(base, 0));
        assert!(s1.contains(&Signal::OutputBurst));
        let s2 = a.feed(&big, at(base, 100));
        assert!(!s2.contains(&Signal::OutputBurst));
    }

    #[test]
    fn quiet_with_cursor_on_prompt_emits_quiescence() {
        let base = Instant::now();
        let mut a = adapter();
        a.feed(b"file1\r\nfile2\r\nuser@mac ~ $ ", at(base, 0));
        assert!(a.tick(at(base, 100)).is_empty());
        let signals = a.tick(at(base, 500));
        assert_eq!(signals, vec![Signal::Quiescence { quiet_ms: 400 }]);
        // Continued silence must not repeat the conclusion.
        assert!(a.tick(at(base, 900)).is_empty());
    }

    #[test]
    fn quiet_without_prompt_stays_silent() {
        let base = Instant::now();
        let mut a = adapter();
        a.feed(b"downloading part 3 of 7", at(base, 0));
        assert!(a.tick(at(base, 1000)).is_empty());
    }

    #[test]
    fn silently_running_command_is_not_done() {
        let base = Instant::now();
        let mut a = adapter();
        // Command echoed, then it runs producing no output. The cursor
        // sits on the empty line below the echo, so no conclusion.
        a.feed(b"user@mac ~ $ sleep 3\r\n", at(base, 0));
        assert!(a.tick(at(base, 600)).is_empty());
        assert!(a.tick(at(base, 2000)).is_empty());
        // The prompt returns and the cursor rests on it.
        a.feed(b"user@mac ~ $ ", at(base, 3000));
        let signals = a.tick(at(base, 3500));
        assert_eq!(signals, vec![Signal::Quiescence { quiet_ms: 400 }]);
    }

    #[test]
    fn stale_prompt_rows_above_do_not_trigger() {
        let base = Instant::now();
        let mut a = adapter();
        // An empty enter leaves a bare prompt row on screen, then a
        // silent command runs. The bare row two lines up must not count.
        a.feed(b"user@mac ~ $ \r\nuser@mac ~ $ sleep 3\r\n", at(base, 0));
        assert!(a.tick(at(base, 600)).is_empty());
    }

    #[test]
    fn tui_input_row_matches_regardless_of_paint_order() {
        let base = Instant::now();
        let mut a = adapter();
        // A TUI paints its input row, then repaints a region above it,
        // leaving the cursor back in the input row. In the linear stream
        // the input row is not last, but on the grid it is at the cursor.
        let frame = concat!(
            "\x1b[2J",             // clear screen
            "\x1b[10;1H\u{276f} ", // input row
            "\x1b[3;1Hstreamed answer text ends here",
            "\x1b[10;3H", // cursor back into the input row
        );
        a.feed(frame.as_bytes(), at(base, 0));
        let signals = a.tick(at(base, 500));
        assert_eq!(signals, vec![Signal::Quiescence { quiet_ms: 400 }]);
    }

    #[test]
    fn busy_hint_suppresses_quiescence_between_stream_batches() {
        let base = Instant::now();
        let mut a = adapter();
        // A TUI with the cursor resting in its input box while its status
        // row still says it is working. Output pauses between batches.
        let frame = concat!(
            "\x1b[2J",
            "\x1b[25;1H\u{276f} ",
            "\x1b[27;1H\u{2733} Reticulating\u{2026} (esc to interrupt)",
            "\x1b[25;3H",
        );
        a.feed(frame.as_bytes(), at(base, 0));
        assert!(a.tick(at(base, 1500)).is_empty());
        // The hint clears once the work is finished.
        a.feed(b"\x1b[27;1H\x1b[2K\x1b[25;3H", at(base, 2000));
        let signals = a.tick(at(base, 2500));
        assert_eq!(signals, vec![Signal::Quiescence { quiet_ms: 400 }]);
    }

    #[test]
    fn cursor_position_queries_do_not_reset_the_quiet_timer() {
        let base = Instant::now();
        let mut a = adapter();
        a.feed(b"user@mac ~ $ ", at(base, 0));
        // Claude polls the terminal for its cursor position several times
        // a second even when idle. The queries paint nothing, so they must
        // not count as output activity.
        for i in 1..10u64 {
            a.feed(b"\x1b[?6n", at(base, i * 200));
        }
        let signals = a.tick(at(base, 1900));
        assert_eq!(signals, vec![Signal::Quiescence { quiet_ms: 400 }]);
    }

    #[test]
    fn identical_repaint_does_not_reset_the_quiet_timer() {
        let base = Instant::now();
        let mut a = adapter();
        let frame = "\x1b[2J\x1b[5;1Hstatus line\x1b[10;1H\u{276f} "
            .as_bytes()
            .to_vec();
        a.feed(&frame, at(base, 0));
        a.feed(&frame, at(base, 300));
        a.feed(&frame, at(base, 600));
        let signals = a.tick(at(base, 700));
        assert_eq!(signals, vec![Signal::Quiescence { quiet_ms: 400 }]);
    }

    #[test]
    fn title_sequence_bell_does_not_signal() {
        let base = Instant::now();
        let mut a = adapter();
        let signals = a.feed(b"\x1b]0;my-shell\x07$ ", at(base, 0));
        assert!(!signals.contains(&Signal::Bell));
    }

    #[test]
    fn bare_bell_signals() {
        let base = Instant::now();
        let mut a = adapter();
        let signals = a.feed(b"\x07", at(base, 0));
        assert!(signals.contains(&Signal::Bell));
    }

    #[test]
    fn resize_keeps_the_screen_model_usable() {
        let base = Instant::now();
        let mut a = adapter();
        a.feed(b"user@mac ~ $ ", at(base, 0));
        a.resize(120, 40);
        a.feed(b"", at(base, 100));
        let signals = a.tick(at(base, 600));
        assert_eq!(signals, vec![Signal::Quiescence { quiet_ms: 400 }]);
    }
}
