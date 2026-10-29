//! Following a log, as `tail -f` does: a file, from its last [`TAIL`]
//! bytes, read again from its start if it shrinks or is replaced (rotated);
//! or a ring such as the kernel's, read whole each time, of which only what
//! is new is kept ([`after`]).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use crate::TAIL;

/// The most one [`Tail::poll`] reads, so that a big file is taken in
/// pieces while the screen keeps up.
const STEP: usize = 64 * 1024;

pub struct Tail {
    path: PathBuf,
    /// The file, its inode and how far it has been read.
    open: Option<(File, u64, u64)>,
    /// It has been read once (so a new one is read from its start).
    seen: bool,
    /// It couldn't be read, and that has been said.
    complained: bool,
}

impl Tail {
    pub fn new(path: impl Into<PathBuf>) -> Tail {
        Tail { path: path.into(), open: None, seen: false, complained: false }
    }

    /// What is new since the last call: at first, the file's end. A note
    /// says when it can't be read, or has started again.
    pub fn poll(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let now = std::fs::metadata(&self.path);
        let same = matches!((&self.open, &now), (Some((_, ino, pos)), Ok(m)) if m.ino() == *ino && m.len() >= *pos);
        if !same {
            if self.open.take().is_some() {
                out.extend_from_slice(format!("-- {} started again --\n", self.path.display()).as_bytes());
            }
            match File::open(&self.path).and_then(|f| f.metadata().map(|m| (f, m))) {
                Ok((f, m)) => {
                    let from = if self.seen { 0 } else { m.len().saturating_sub(TAIL) };
                    self.open = Some((f, m.ino(), from));
                    self.seen = true;
                    self.complained = false;
                }
                Err(e) => {
                    if !self.complained {
                        out.extend_from_slice(format!("logs: {}: {e}\n", self.path.display()).as_bytes());
                        self.complained = true;
                        // One that appears later is new: all of it.
                        self.seen = true;
                    }
                    return out;
                }
            }
        }
        if let Some((f, _, pos)) = self.open.as_mut() {
            let start = out.len();
            out.resize(start + STEP, 0);
            let n = f.seek(SeekFrom::Start(*pos)).and_then(|_| f.read(&mut out[start..]));
            match n {
                Ok(n) => {
                    out.truncate(start + n);
                    *pos += n as u64;
                }
                Err(e) => {
                    out.truncate(start);
                    out.extend_from_slice(format!("logs: {}: {e}\n", self.path.display()).as_bytes());
                    self.open = None;
                }
            }
        }
        out
    }
}

/// What a ring read again (`new`) has that it didn't when read before
/// (`old`): what follows old's last KB in it. None if it no longer holds
/// that (the ring has gone round past it): then it is all new.
pub fn after<'a>(old: &[u8], new: &'a [u8]) -> Option<&'a [u8]> {
    if old.is_empty() {
        return Some(new);
    }
    let key = &old[old.len().saturating_sub(1024)..];
    let at = new.windows(key.len()).rposition(|w| w == key)?;
    Some(&new[at + key.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_a_file() {
        let dir = std::env::temp_dir().join(format!("neochumby-logs-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.log");
        let mut t = Tail::new(&p);
        assert!(String::from_utf8(t.poll()).unwrap().contains("x.log"));
        assert_eq!(t.poll(), b"");
        // One that appears is read from its start.
        std::fs::write(&p, "a\n").unwrap();
        assert_eq!(t.poll(), b"a\n");
        assert_eq!(t.poll(), b"");
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        std::io::Write::write_all(&mut f, b"b\n").unwrap();
        assert_eq!(t.poll(), b"b\n");
        // Truncated: from the start again, saying so.
        std::fs::write(&p, "c\n").unwrap();
        let s = String::from_utf8(t.poll()).unwrap();
        assert!(s.contains("started again") && s.ends_with("c\n"), "{s}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_big_file_from_its_end() {
        let dir = std::env::temp_dir().join(format!("neochumby-logs-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("big.log");
        std::fs::write(&p, vec![b'x'; TAIL as usize * 3]).unwrap();
        assert_eq!(Tail::new(&p).poll().len(), TAIL as usize);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn what_a_ring_adds() {
        assert_eq!(after(b"", b"one\n"), Some(&b"one\n"[..]));
        assert_eq!(after(b"one\n", b"one\ntwo\n"), Some(&b"two\n"[..]));
        assert_eq!(after(b"one\ntwo\n", b"two\nthree\n"), None);
        assert_eq!(after(b"one\n", b"one\n"), Some(&b""[..]));
    }
}
