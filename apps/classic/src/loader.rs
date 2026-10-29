//! The loader: a thread that reads files for the engine, so a slow read
//! never stalls a frame. Reads from the Chumby's USB stick block for a long
//! time (about 175 ms for a full screen, a cold open up to 370 ms), longer
//! than a 40 ms frame, so prefetching happens here while the engine draws,
//! and the engine takes what has arrived in each frame's idle time.
//!
//! Requests and replies cross between the threads through
//! neochumby_sys::chan, not std's channels: std's blocking waits use futex
//! flags that the Chumby's 2.6.16 kernel doesn't have.
//!
//! If the thread's channels fail, the engine reads synchronously instead. A
//! panic in the thread is not caught: release builds abort on panic, so it
//! would end the process like a panic anywhere else. Nothing here panics on
//! bad input: a missing, short or wrong-sized file, or a buffer that can't
//! be allocated, comes back as a [`Fail`].

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use neochumby_sys::chan::{self, Receiver, Sender};

use crate::cache;

/// Why a file couldn't be read.
#[derive(Debug, PartialEq)]
pub enum Fail {
    Open,
    /// Its size isn't the size the manifest was read with.
    Size(u64),
    /// It got shorter while it was being read.
    Short,
    /// There wasn't the memory to hold it.
    Memory,
}

pub struct Reply {
    pub id: usize,
    pub data: Result<Vec<u16>, Fail>,
}

struct Req {
    id: usize,
    path: PathBuf,
    size: usize,
}

pub struct Loader {
    req: Sender<Req>,
    rep: Receiver<Reply>,
}

/// Reads the first `size` bytes of a file, as little-endian 16-bit words
/// (the pictures are RGB565); with `exact`, it must be that size. (The
/// loader checks; a synchronous read, like the C engine's, takes the start
/// of a file that has grown.) It reads in chunks straight into the words,
/// so a file is in memory once.
pub fn read_file(path: &std::path::Path, size: usize, exact: bool) -> Result<Vec<u16>, Fail> {
    let mut f = File::open(path).map_err(|_| Fail::Open)?;
    let len = f.metadata().map_err(|_| Fail::Open)?.len();
    if exact && len != size as u64 {
        return Err(Fail::Size(len));
    }
    let mut buf = cache::alloc(size).ok_or(Fail::Memory)?;
    let mut chunk = [0u8; 16384];
    let mut got = 0;
    while got < size {
        let want = (size - got).min(chunk.len());
        let n = match f.read(&mut chunk[..want]) {
            Ok(0) | Err(_) => return Err(Fail::Short),
            Ok(n) => n,
        };
        cache::put_bytes(&mut buf, got, &chunk[..n]);
        got += n;
    }
    Ok(buf)
}

impl Loader {
    pub fn start() -> Option<Loader> {
        let (req, req_rx) = chan::channel::<Req>().ok()?;
        let (rep_tx, rep) = chan::channel::<Reply>().ok()?;
        std::thread::Builder::new()
            .name("classicloader".into())
            .spawn(move || {
                while let Ok(r) = req_rx.recv(-1) {
                    let Some(r) = r else { continue };
                    let reply = Reply { id: r.id, data: read_file(&r.path, r.size, true) };
                    if rep_tx.send(reply).is_err() {
                        return;
                    }
                }
            })
            .ok()?;
        Some(Loader { req, rep })
    }

    /// Asks for a file. False if the thread has gone.
    pub fn request(&self, id: usize, path: PathBuf, size: usize) -> bool {
        self.req.send(Req { id, path, size }).is_ok()
    }

    /// Waits up to `timeout_ms` (-1: for ever) for a reply. Err if the
    /// thread has gone.
    pub fn recv(&self, timeout_ms: i32) -> Result<Option<Reply>, chan::Disconnected> {
        self.rep.recv(timeout_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_in_the_background() {
        let dir = std::env::temp_dir().join(format!("wx-loader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a"), [1u8, 0, 2, 0, 3]).unwrap();
        let l = Loader::start().unwrap();
        assert!(l.request(7, dir.join("a"), 5));
        let r = l.recv(5000).unwrap().unwrap();
        assert_eq!(r.id, 7);
        let d = r.data.unwrap();
        assert_eq!((0..5).map(|k| cache::byte(&d, k)).collect::<Vec<_>>(), vec![1, 0, 2, 0, 3]);
        assert!(l.request(8, dir.join("a"), 4));
        assert_eq!(l.recv(5000).unwrap().unwrap().data.unwrap_err(), Fail::Size(5));
        assert!(l.request(9, dir.join("missing"), 4));
        assert_eq!(l.recv(5000).unwrap().unwrap().data.unwrap_err(), Fail::Open);
        assert!(l.recv(0).unwrap().is_none(), "nothing more");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
