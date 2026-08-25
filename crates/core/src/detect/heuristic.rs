//! Fallback detection that works on any CLI with no integration at all.
//!
//! Busy is inferred from a burst of output. Done is inferred from silence:
//! once output has been quiet for a while and the visible tail of the
//! screen looks like a prompt, the command is considered finished. The
//! prompt check exists for commands that run silently (a bare `sleep`
//! produces no burst and no output, so silence alone proves nothing until
//! the prompt reappears). Full-screen TUIs with spinners repaint
//! constantly while working, so for them the silence condition does most
//! of the discrimination.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use regex::Regex;

use super::ansi::AnsiFilter;
use super::{Adapter, Signal};

pub struct HeuristicConfig {
    /// Bytes within `burst_window` that count as a burst of output.
    pub burst_bytes: usize,
    pub burst_window: Duration,
    /// Silence required before the tail is checked for a prompt.
    pub quiet: Duration,
    /// Pattern that decides whether the tail looks like a prompt.
    pub prompt_pattern: Regex,
    /// How many trailing non-empty lines to scan for the prompt pattern.
    pub prompt_scan_lines: usize,
    /// Cap on the stripped tail kept for prompt matching.
    pub tail_chars: usize,
}

impl Default for HeuristicConfig {
    fn default() -> Self {
        Self {
            burst_bytes: 200,
            burst_window: Duration::from_millis(300),
            quiet: Duration::from_millis(400),
            // Shell prompts ending in a marker character, plus the input
            // box Claude Code draws when it is ready for input.
            prompt_pattern: Regex::new(r"(?m)[❯$%#]\s*$|^\s*>\s|╰|\? for shortcuts")
                .expect("default prompt pattern is valid"),
            prompt_scan_lines: 6,
            tail_chars: 4000,
        }
    }
}

pub struct HeuristicAdapter {
    cfg: HeuristicConfig,
    filter: AnsiFilter,
    /// Stripped text tail of the stream, capped at `tail_chars`.
    tail: String,
    /// Recent output chunk sizes for burst detection.
    window: VecDeque<(Instant, usize)>,
    last_output: Option<Instant>,
    /// Output has arrived since the last quiescence conclusion, so a
    /// quiet-plus-prompt check is worth running.
    waiting_for_quiet: bool,
    /// A burst signal was already emitted for the current activity.
    burst_announced: bool,
}

impl HeuristicAdapter {
    pub fn new(cfg: HeuristicConfig) -> Self {
        Self {
            cfg,
            filter: AnsiFilter::new(),
            tail: String::new(),
            window: VecDeque::new(),
            last_output: None,
            waiting_for_quiet: false,
            burst_announced: false,
        }
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

    fn tail_looks_like_prompt(&self) -> bool {
        // Split on carriage returns as well as newlines. Full-screen TUIs
        // position rows with cursor moves and \r instead of \n, so a
        // newline-only split lumps many painted rows into one huge line
        // and hides the prompt row behind whatever painted after it.
        let scan: Vec<&str> = self
            .tail
            .split(['\n', '\r'])
            .rev()
            .filter(|l| !l.trim().is_empty())
            .take(self.cfg.prompt_scan_lines)
            .collect();
        scan.iter()
            .any(|line| self.cfg.prompt_pattern.is_match(line))
    }
}

impl Adapter for HeuristicAdapter {
    fn feed(&mut self, bytes: &[u8], now: Instant) -> Vec<Signal> {
        let mut signals = Vec::new();

        let filtered = self.filter.push(bytes);
        for _ in 0..filtered.bells {
            signals.push(Signal::Bell);
        }

        self.tail.push_str(&filtered.plain);
        if self.tail.len() > self.cfg.tail_chars {
            let cut = self.tail.len() - self.cfg.tail_chars;
            // Cut on a character boundary.
            let cut = (cut..self.tail.len())
                .find(|&i| self.tail.is_char_boundary(i))
                .unwrap_or(0);
            self.tail.drain(..cut);
        }

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
        if self.tail_looks_like_prompt() {
            self.waiting_for_quiet = false;
            self.burst_announced = false;
            return vec![Signal::Quiescence {
                quiet_ms: self.cfg.quiet.as_millis() as u64,
            }];
        }
        Vec::new()
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
    fn quiet_with_prompt_tail_emits_quiescence() {
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
    fn quiet_without_prompt_tail_stays_silent() {
        let base = Instant::now();
        let mut a = adapter();
        a.feed(b"downloading part 3 of 7", at(base, 0));
        assert!(a.tick(at(base, 1000)).is_empty());
    }

    #[test]
    fn prompt_reappearing_after_silent_command_is_caught() {
        let base = Instant::now();
        let mut a = adapter();
        // Echo of a command that then runs silently for two seconds.
        a.feed(b"$ sleep 2\r\n", at(base, 0));
        assert!(a.tick(at(base, 600)).is_empty());
        // The prompt returns with a tiny chunk, far below burst size.
        a.feed(b"$ ", at(base, 2000));
        let signals = a.tick(at(base, 2500));
        assert_eq!(signals, vec![Signal::Quiescence { quiet_ms: 400 }]);
    }

    #[test]
    fn claude_style_input_box_matches_prompt_pattern() {
        let base = Instant::now();
        let mut a = adapter();
        a.feed(
            "answer text\r\n\u{256d}\u{2500}\u{2500}\u{256e}\r\n\u{2502} > \u{2502}\r\n\u{2570}\u{2500}\u{2500}\u{256f}\r\n  ? for shortcuts\r\n"
                .as_bytes(),
            at(base, 0),
        );
        let signals = a.tick(at(base, 500));
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
}
