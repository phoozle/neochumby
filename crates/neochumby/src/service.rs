//! An app's `exec` and its own service, talking: a local socket the
//! service listens on and the exec connects to, carrying frames
//! (neochumby_proto::frame's u32 big-endian length, then the bytes) both
//! ways. AirPlay's screen uses it to hear what its service hears from the
//! companion, and to send its buttons back.
//!
//! ```ignore
//! // The service:
//! let l = neochumby::service::listen(neochumby::service!())?;
//! if let Some(mut screen) = neochumby::service::accept(&l)? { screen.send(b"hello")?; }
//! // The exec:
//! let mut service = neochumby::service::connect(neochumby::service!())?;
//! for frame in service.recv()? { /* ... */ }
//! ```
//!
//! The socket is a Unix socket in Linux's abstract namespace, named after
//! the app: `neochumby-<app>`, its crate's name, which
//! [`service!`](crate::service!) takes as it compiles, so apps don't pick
//! names of their own. It needs no directory both can write, so it works
//! whether the two run as root or jailed with a /tmp each (where an app's
//! exec and its service share a uid). Elsewhere (tests on a Mac) it is a
//! file in the temporary directory.
//!
//! **Any process can reach an abstract name, and bind it.** So each end
//! checks that the other runs as its own user (SO_PEERCRED), and a peer
//! that doesn't is refused: another app can't pose as the service or the
//! exec. What the check can't stop is another app binding the name before
//! the service does. Then the service can't listen, and the exec can't
//! connect, as that app runs as another user. An app that must work
//! regardless shouldn't rely on this socket alone.
//!
//! Neither end waits on the other: the Chumby's kernel lacks the futex
//! operations std's threads would wait on, so each runs one loop. Reads
//! never wait, and writes wait at most [`WRITE`], after which the caller
//! should drop the peer (the exec connects again).

use std::io::{self, Write};
use std::os::fd::AsFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

use neochumby_proto::frame::MAX_FRAME;
use neochumby_sys::fd::recv_nowait;

/// The longest a write to the other end may take.
pub const WRITE: Duration = Duration::from_millis(500);

/// `service!()`: the compiling app's socket, for [`listen`] and
/// [`connect`]: `neochumby-<app>`, from its crate's name.
#[macro_export]
macro_rules! service {
    () => {
        $crate::service::App::__from_crate(env!("CARGO_PKG_NAME"))
    };
}

/// An app's service socket, as [`service!`](crate::service!) names it.
#[derive(Clone, Copy, Debug)]
pub struct App {
    name: &'static str,
}

impl App {
    /// For [`service!`](crate::service!): the crate's name, which the
    /// manifest tool holds to `neochumby-<app>`.
    #[doc(hidden)]
    pub const fn __from_crate(name: &'static str) -> App {
        App { name }
    }

    /// The socket's name: `neochumby-<app>`.
    pub fn name(&self) -> &'static str {
        self.name
    }
}

#[cfg(target_os = "linux")]
fn address(name: &str) -> io::Result<std::os::unix::net::SocketAddr> {
    use std::os::linux::net::SocketAddrExt;
    std::os::unix::net::SocketAddr::from_abstract_name(name)
}

#[cfg(not(target_os = "linux"))]
fn path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(name)
}

/// The service's end: accepts the exec ([`accept`]) without waiting.
pub fn listen(app: App) -> io::Result<UnixListener> {
    #[cfg(target_os = "linux")]
    let l = UnixListener::bind_addr(&address(app.name)?)?;
    #[cfg(not(target_os = "linux"))]
    let l = {
        let _ = std::fs::remove_file(path(app.name));
        UnixListener::bind(path(app.name))?
    };
    l.set_nonblocking(true)?;
    Ok(l)
}

/// The exec's end.
pub fn connect(app: App) -> io::Result<Peer> {
    #[cfg(target_os = "linux")]
    let s = UnixStream::connect_addr(&address(app.name)?)?;
    #[cfg(not(target_os = "linux"))]
    let s = UnixStream::connect(path(app.name))?;
    Peer::new(s)
}

/// An exec that has connected, if one is waiting. A peer of another user
/// is an error, and is dropped.
pub fn accept(l: &UnixListener) -> io::Result<Option<Peer>> {
    match l.accept() {
        Ok((s, _)) => {
            // An accepted socket doesn't inherit O_NONBLOCK on Linux, but
            // does elsewhere; reads here go through recv_nowait anyway.
            s.set_nonblocking(false)?;
            Peer::new(s).map(Some)
        }
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(e) => Err(e),
    }
}

/// The other end, checked to run as this process's user.
pub struct Peer {
    s: UnixStream,
    frames: Reader,
}

impl Peer {
    fn new(s: UnixStream) -> io::Result<Peer> {
        let theirs = neochumby_sys::net::peer_uid(&s)?;
        let ours = neochumby_sys::process::uid();
        if theirs != ours {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("a peer running as uid {theirs}, not {ours}"),
            ));
        }
        s.set_write_timeout(Some(WRITE))?;
        Ok(Peer { s, frames: Reader::default() })
    }

    /// The frames that have arrived, without waiting; an error once the
    /// other end has gone.
    pub fn recv(&mut self) -> io::Result<Vec<Vec<u8>>> {
        let mut buf = [0u8; 4096];
        loop {
            match recv_nowait(self.s.as_fd(), &mut buf) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the other end has gone")),
                Ok(n) => self.frames.push(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        let mut out = Vec::new();
        while let Some(f) = self.frames.frame()? {
            out.push(f);
        }
        Ok(out)
    }

    /// Sends one frame, waiting at most [`WRITE`]. After an error the
    /// stream may hold part of a frame: drop the peer.
    pub fn send(&mut self, frame: &[u8]) -> io::Result<()> {
        neochumby_proto::frame::write(&mut self.s, frame)?;
        self.s.flush()
    }
}

/// Frames out of a byte stream that arrives in pieces (a socket read
/// without waiting): neochumby_proto::frame's u32 big-endian length, then
/// the frame.
#[derive(Default)]
pub struct Reader {
    buf: Vec<u8>,
}

impl Reader {
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// The next whole frame, if one has arrived; an error for a length
    /// over MAX_FRAME, after which the stream is no use.
    pub fn frame(&mut self) -> io::Result<Option<Vec<u8>>> {
        let Some(len) = self.buf.first_chunk::<4>() else { return Ok(None) };
        let n = u32::from_be_bytes(*len) as usize;
        if n > MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too long"));
        }
        if self.buf.len() < 4 + n {
            return Ok(None);
        }
        let f = self.buf[4..4 + n].to_vec();
        self.buf.drain(..4 + n);
        Ok(Some(f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_both_ways() {
        let name: &'static str = Box::leak(format!("neochumby-service-test-{}", std::process::id()).into_boxed_str());
        let app = App::__from_crate(name);
        let l = listen(app).unwrap();
        assert!(accept(&l).unwrap().is_none());
        let mut exec = connect(app).unwrap();
        let mut service = loop {
            if let Some(p) = accept(&l).unwrap() {
                break p;
            }
        };
        assert!(service.recv().unwrap().is_empty());
        service.send(b"one").unwrap();
        service.send(b"").unwrap();
        assert_eq!(exec.recv().unwrap(), [b"one".to_vec(), vec![]]);
        exec.send(b"back").unwrap();
        assert_eq!(service.recv().unwrap(), [b"back".to_vec()]);
        drop(exec);
        assert!(service.recv().is_err());
    }

    #[test]
    fn the_name_is_the_crates() {
        assert_eq!(crate::service!().name(), "neochumby");
    }

    #[test]
    fn the_reader_takes_frames_in_pieces() {
        let mut stream = Vec::new();
        neochumby_proto::frame::write(&mut stream, b"ab").unwrap();
        neochumby_proto::frame::write(&mut stream, b"c").unwrap();
        let mut r = Reader::default();
        let mut got = Vec::new();
        for b in &stream {
            r.push(std::slice::from_ref(b));
            while let Some(f) = r.frame().unwrap() {
                got.push(f);
            }
        }
        assert_eq!(got, [b"ab".to_vec(), b"c".to_vec()]);
        let mut r = Reader::default();
        r.push(&(MAX_FRAME as u32 + 1).to_be_bytes());
        assert!(r.frame().is_err());
    }
}
