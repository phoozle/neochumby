//! A log as the screen shows it: rows of at most [`COLS`] characters, cut
//! from its lines (a long line wraps), each marked with what its line says
//! (an error, a warning). Bytes arrive as they come, in any pieces; the
//! last line, until its "\n", is shown as far as it goes. Terminal escapes
//! (colours, cursor movement) are left out, tabs become spaces, and what
//! isn't text becomes '?'. Only the last [`KEEP`] rows are kept.
//!
//! Rows are numbered from the first ever pushed, so a place in the log
//! stays put as old rows go.

use std::collections::VecDeque;

/// The screen's width in the 5x8 font.
pub const COLS: usize = 64;
/// Rows kept: about 380 KB of text at most.
pub const KEEP: usize = 6000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Plain,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub text: String,
    pub level: Level,
}

#[derive(Default)]
pub struct Text {
    rows: VecDeque<Row>,
    /// Rows dropped from the front: the first kept row's number.
    dropped: u64,
    /// The last line, not yet ended, and how many rows it has on screen.
    partial: Vec<u8>,
    partial_rows: usize,
}

impl Text {
    pub fn new() -> Text {
        Text::default()
    }

    pub fn clear(&mut self) {
        *self = Text::default();
    }

    /// The first row kept, and one past the last.
    pub fn first(&self) -> u64 {
        self.dropped
    }

    pub fn end(&self) -> u64 {
        self.dropped + self.rows.len() as u64
    }

    pub fn get(&self, row: u64) -> Option<&Row> {
        self.rows.get(usize::try_from(row.checked_sub(self.dropped)?).ok()?)
    }

    /// More of the log.
    pub fn push(&mut self, bytes: &[u8]) {
        self.unshow_partial();
        self.partial.extend_from_slice(bytes);
        let mut rest = std::mem::take(&mut self.partial);
        let mut start = 0;
        while let Some(n) = rest[start..].iter().position(|&b| b == b'\n') {
            self.add(&rest[start..start + n]);
            start += n + 1;
        }
        rest.drain(..start);
        self.partial = rest;
        if !self.partial.is_empty() {
            let before = self.rows.len();
            let partial = std::mem::take(&mut self.partial);
            self.add(&partial);
            self.partial = partial;
            self.partial_rows = self.rows.len() - before;
        }
        let over = self.rows.len().saturating_sub(KEEP);
        if over > 0 {
            self.rows.drain(..over);
            self.dropped += over as u64;
            self.partial_rows = self.partial_rows.min(self.rows.len());
        }
    }

    /// A line of our own (a note, an error), as if the log had said it.
    pub fn note(&mut self, line: &str) {
        self.unshow_partial();
        let partial = std::mem::take(&mut self.partial);
        self.add(line.as_bytes());
        self.push(&partial);
    }

    fn unshow_partial(&mut self) {
        let n = std::mem::take(&mut self.partial_rows);
        self.rows.truncate(self.rows.len() - n);
    }

    /// One line, as rows.
    fn add(&mut self, line: &[u8]) {
        let line = clean(line);
        let level = level(&line);
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            self.rows.push_back(Row { text: String::new(), level });
        }
        for piece in chars.chunks(COLS) {
            self.rows.push_back(Row { text: piece.iter().collect(), level });
        }
    }
}

/// A line's text: escapes left out, tabs expanded, the rest made printable.
/// After a carriage return only what follows it shows, as on a terminal
/// (progress bars write over themselves).
fn clean(line: &[u8]) -> String {
    let line = String::from_utf8_lossy(line);
    let line = line.trim_end_matches('\r');
    let line = line.rsplit('\r').next().unwrap_or("");
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut col = 0;
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI: parameters and intermediates, then a final byte.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ESC \.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            chars.next_if_eq(&'\\');
                            break;
                        }
                    }
                }
                // Two-byte escapes, and charset choices (ESC ( B).
                Some('(' | ')') => {
                    chars.next();
                }
                _ => {}
            },
            '\t' => {
                let n = 8 - col % 8;
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            c if c.is_control() => {}
            c => {
                // The font has Latin-1 only.
                out.push(if (c as u32) < 0x100 { c } else { '?' });
                col += 1;
            }
        }
    }
    out
}

/// What a line says, by the words in it.
fn level(line: &str) -> Level {
    let l = line.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| l.contains(w));
    if has(&["error", "fail", "panic", "fatal", "crit", "oops", "segfault", "killed"]) {
        Level::Error
    } else if has(&["warn"]) {
        Level::Warning
    } else {
        Level::Plain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(t: &Text) -> Vec<String> {
        (t.first()..t.end()).map(|r| t.get(r).unwrap().text.clone()).collect()
    }

    #[test]
    fn lines_arrive_in_any_pieces() {
        let mut t = Text::new();
        t.push(b"one\ntw");
        assert_eq!(rows(&t), ["one", "tw"]);
        t.push(b"o\nthree");
        assert_eq!(rows(&t), ["one", "two", "three"]);
        t.push(b"\n");
        assert_eq!(rows(&t), ["one", "two", "three"]);
        t.push(b"\n");
        assert_eq!(rows(&t), ["one", "two", "three", ""]);
    }

    #[test]
    fn long_lines_wrap() {
        let mut t = Text::new();
        t.push(format!("{}\n", "x".repeat(COLS * 2 + 3)).as_bytes());
        assert_eq!(rows(&t).iter().map(String::len).collect::<Vec<_>>(), [COLS, COLS, 3]);
        // A partial line that grows past the width rewraps.
        t.push(&[b'y'; COLS]);
        t.push(b"yy");
        assert_eq!(rows(&t).len(), 5);
        assert_eq!(t.get(4).unwrap().text, "yy");
    }

    #[test]
    fn escapes_tabs_and_returns() {
        assert_eq!(clean(b"\x1b[1;31merror\x1b[0m: x"), "error: x");
        assert_eq!(clean(b"\x1b]0;title\x07after"), "after");
        assert_eq!(clean(b"a\tb"), "a       b");
        assert_eq!(clean(b"10%\r50%\r100%\r"), "100%");
        assert_eq!(clean(b"bell\x07 \xff \xe2\x96\x88"), "bell ? ?");
    }

    #[test]
    fn levels() {
        assert_eq!(level("neochumby-doom: no IWAD found"), Level::Plain);
        assert_eq!(level("mount FAILED"), Level::Error);
        assert_eq!(level("warning: low space"), Level::Warning);
    }

    #[test]
    fn notes_go_before_a_partial_line() {
        let mut t = Text::new();
        t.push(b"done\nhalf");
        t.note("logs: a note");
        assert_eq!(rows(&t), ["done", "logs: a note", "half"]);
    }

    #[test]
    fn old_rows_go_and_numbers_stay() {
        let mut t = Text::new();
        for i in 0..KEEP + 10 {
            t.push(format!("{i}\n").as_bytes());
        }
        assert_eq!(t.first(), 10);
        assert_eq!(t.end(), KEEP as u64 + 10);
        assert_eq!(t.get(10).unwrap().text, "10");
        assert!(t.get(9).is_none());
    }
}
