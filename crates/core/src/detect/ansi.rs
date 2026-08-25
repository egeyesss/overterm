//! Streaming ANSI/VT escape-sequence filter.
//!
//! The detector needs two things from raw PTY output: the plain text (for
//! prompt matching) and real bell characters. A naive scan for 0x07 gives
//! constant false bells because OSC sequences (window title updates, which
//! shells emit on every prompt) are terminated by BEL. This filter tracks
//! escape-sequence state across chunk boundaries so both are extracted
//! correctly no matter how the stream is split.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ParseState {
    Ground,
    /// Saw ESC, waiting to learn the sequence type.
    Escape,
    /// Inside CSI (ESC [ ...), ends on a byte in 0x40..=0x7E.
    Csi,
    /// Inside OSC (ESC ] ...), ends on BEL or ST (ESC \).
    Osc,
    /// Saw ESC inside an OSC; ESC \ is the ST terminator.
    OscEscape,
    /// Two-byte sequences like charset selection (ESC ( B).
    EscapeIntermediate,
}

/// Result of pushing one chunk of output through the filter.
pub struct Filtered {
    /// Printable text and line breaks with all escape sequences removed.
    pub plain: String,
    /// Count of bell characters that appeared outside escape sequences.
    pub bells: usize,
}

/// Incremental filter. Feed it output chunks in order; state carries over
/// between calls so sequences split across reads are still handled.
#[derive(Default)]
pub struct AnsiFilter {
    state: Option<ParseState>,
    /// Pending bytes of a UTF-8 character split across chunks.
    utf8_partial: Vec<u8>,
}

impl AnsiFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) -> Filtered {
        let mut plain_bytes: Vec<u8> = Vec::with_capacity(bytes.len());
        let mut bells = 0usize;
        let mut state = self.state.unwrap_or(ParseState::Ground);

        for &b in bytes {
            state = match state {
                ParseState::Ground => match b {
                    0x1b => ParseState::Escape,
                    0x07 => {
                        bells += 1;
                        ParseState::Ground
                    }
                    b'\n' | b'\r' | b'\t' => {
                        plain_bytes.push(b);
                        ParseState::Ground
                    }
                    0x00..=0x1f => ParseState::Ground,
                    _ => {
                        plain_bytes.push(b);
                        ParseState::Ground
                    }
                },
                ParseState::Escape => match b {
                    b'[' => ParseState::Csi,
                    b']' => ParseState::Osc,
                    b'(' | b')' | b'#' | b'%' => ParseState::EscapeIntermediate,
                    // Single-byte sequences like ESC 7, ESC 8, ESC =, ESC M.
                    _ => ParseState::Ground,
                },
                ParseState::Csi => {
                    if (0x40..=0x7e).contains(&b) {
                        ParseState::Ground
                    } else {
                        ParseState::Csi
                    }
                }
                ParseState::Osc => match b {
                    0x07 => ParseState::Ground,
                    0x1b => ParseState::OscEscape,
                    _ => ParseState::Osc,
                },
                ParseState::OscEscape => match b {
                    b'\\' => ParseState::Ground,
                    // Stray ESC inside OSC data; stay in the sequence.
                    _ => ParseState::Osc,
                },
                ParseState::EscapeIntermediate => ParseState::Ground,
            };
        }

        self.state = Some(state);

        // Re-attach any UTF-8 tail held over from the previous chunk, then
        // hold back a new incomplete tail so we never emit broken characters.
        let mut assembled = std::mem::take(&mut self.utf8_partial);
        assembled.extend_from_slice(&plain_bytes);
        let valid_up_to = match std::str::from_utf8(&assembled) {
            Ok(_) => assembled.len(),
            Err(e) => e.valid_up_to(),
        };
        let tail = assembled.split_off(valid_up_to);
        // A long invalid tail is real garbage rather than a split character.
        if tail.len() < 4 {
            self.utf8_partial = tail;
        }
        let plain = String::from_utf8_lossy(&assembled).into_owned();

        Filtered { plain, bells }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(input: &[u8]) -> (String, usize) {
        let mut f = AnsiFilter::new();
        let out = f.push(input);
        (out.plain, out.bells)
    }

    #[test]
    fn plain_text_passes_through() {
        let (text, bells) = strip(b"hello world\r\n");
        assert_eq!(text, "hello world\r\n");
        assert_eq!(bells, 0);
    }

    #[test]
    fn csi_color_codes_are_removed() {
        let (text, _) = strip(b"\x1b[1;32mgreen\x1b[0m done");
        assert_eq!(text, "green done");
    }

    #[test]
    fn osc_title_bell_is_not_a_bell() {
        // Shells send this on every prompt to set the window title.
        let (text, bells) = strip(b"\x1b]0;user@host: ~\x07$ ");
        assert_eq!(text, "$ ");
        assert_eq!(bells, 0);
    }

    #[test]
    fn osc_with_st_terminator() {
        let (text, bells) = strip(b"\x1b]0;title\x1b\\after");
        assert_eq!(text, "after");
        assert_eq!(bells, 0);
    }

    #[test]
    fn bare_bell_is_counted() {
        let (text, bells) = strip(b"before\x07after");
        assert_eq!(text, "beforeafter");
        assert_eq!(bells, 1);
    }

    #[test]
    fn sequences_split_across_chunks() {
        let mut f = AnsiFilter::new();
        let a = f.push(b"ok\x1b[3");
        let b = f.push(b"1mred\x1b[0m\x1b]0;ti");
        let c = f.push(b"tle\x07done");
        assert_eq!(a.plain, "ok");
        assert_eq!(b.plain, "red");
        assert_eq!(c.plain, "done");
        assert_eq!(a.bells + b.bells + c.bells, 0);
    }

    #[test]
    fn utf8_split_across_chunks() {
        let mut f = AnsiFilter::new();
        let bytes = "box ╰─╯".as_bytes();
        let a = f.push(&bytes[..5]);
        let b = f.push(&bytes[5..]);
        assert_eq!(format!("{}{}", a.plain, b.plain), "box ╰─╯");
    }
}
