//! An app's companion: its server half (the manifest's `companion=`), which
//! the paired neochumbyd runs for this Chumby. [`open`] connects to it;
//! frames then go both ways on the [`Link`] it returns, until the companion
//! goes.
//!
//! An app names its own with [`companion!`](crate::companion!), checked
//! against the app's Cargo.toml as it compiles: an app without a
//! `companion`, or without `neochumbyd = "required"` or `"optional"`, does
//! not compile, as neochumbyd would have nothing to run for it.
//!
//! ```ignore
//! let mut link = neochumby::companion::open(neochumby::companion!())?;
//! while let Some(frame) = link.recv()? { /* ... */ }
//! ```
//!
//! # Through the session
//!
//! Apps don't hold the Chumby's key: the session does, and opens the
//! connection for them. An app with a companion gets a socket from the
//! session as NEOCHUMBY_COMPANION, and the session opens the companion of
//! the app it started, named by the app's directory, whatever the app
//! asks for; so an app reaches only its own companion. Run by hand (no
//! session), an app connects itself, with the key, as root.
//!
//! On that socket each message is a tag byte, a u32 big-endian length and
//! that many bytes:
//! - the app's `o`: open the companion, closing any open already; the
//!   session answers `o` once it is open, or `e` and why not.
//! - `d`, either way: one frame for, or from, the companion.
//! - the session's `c`: the companion has gone; the app's: close it.

use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::fd::{OwnedFd, RawFd};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use neochumby_proto::frame::MAX_FRAME;
use neochumby_proto::{Identity, Msg, Session, unhex_key};
use neochumby_sys::fd;

use crate::conf::{self, Paired};

/// The variable holding the socket the session gave an app with a
/// companion.
pub const ENV: &str = "NEOCHUMBY_COMPANION";

/// How long connecting to the neochumbyd may take.
const CONNECT: Duration = Duration::from_secs(10);

/// The relay's tags (see the module's docs).
pub const OPEN: u8 = b'o';
pub const ERROR: u8 = b'e';
pub const DATA: u8 = b'd';
pub const CLOSED: u8 = b'c';

/// An app's companion, as neochumbyd knows it (apps/<app> in the tree).
/// Made only by [`companion!`](crate::companion!).
#[derive(Clone, Copy, Debug)]
pub struct Companion {
    app: &'static str,
}

impl Companion {
    /// For [`companion!`](crate::companion!), which has checked `app`.
    #[doc(hidden)]
    pub const fn __checked(app: &'static str) -> Companion {
        Companion { app }
    }

    /// The app's name: its directory in the tree.
    pub fn app(&self) -> &'static str {
        self.app
    }
}

/// An open companion. Dropping it closes it.
pub struct Link(Inner);

enum Inner {
    Relayed(UnixStream, Inbox),
    Direct(Box<Session>),
}

/// Opens `companion`: through the session if it started this app, or
/// itself, with the key, if not.
pub fn open(companion: Companion) -> io::Result<Link> {
    if let Some(mut sock) = handed_over() {
        write(&mut sock, OPEN, &[])?;
        // Whatever came of a link opened before is read past.
        let mut inbox = Inbox::default();
        loop {
            match inbox.next(&mut sock)? {
                None => return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the session has gone")),
                Some((OPEN, _)) => return Ok(Link(Inner::Relayed(sock, inbox))),
                Some((ERROR, why)) => return Err(io::Error::other(String::from_utf8_lossy(&why).into_owned())),
                Some(_) => {}
            }
        }
    }
    let p = conf::paired().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "not paired"))?;
    let me = Identity::from_secret(&p.secret)?;
    connect(companion.app, &p, &me).map(|s| Link(Inner::Direct(Box::new(s))))
}

impl Link {
    /// Sends the companion one frame, of at most
    /// [`MAX_FRAME`](neochumby_proto::frame::MAX_FRAME) bytes.
    pub fn send(&mut self, frame: &[u8]) -> io::Result<()> {
        match &mut self.0 {
            Inner::Relayed(sock, _) => write(sock, DATA, frame),
            Inner::Direct(s) => s.send(&Msg::Data(frame.to_vec())),
        }
    }

    /// The companion's next frame, or None once it has gone.
    pub fn recv(&mut self) -> io::Result<Option<Vec<u8>>> {
        match &mut self.0 {
            Inner::Relayed(sock, inbox) => loop {
                match inbox.next(sock)? {
                    None | Some((CLOSED, _)) => return Ok(None),
                    Some((DATA, f)) => return Ok(Some(f)),
                    Some(_) => {}
                }
            },
            Inner::Direct(s) => loop {
                match s.recv()? {
                    Msg::Data(f) => return Ok(Some(f)),
                    Msg::Closed => return Ok(None),
                    _ => {}
                }
            },
        }
    }

    /// The companion's next frame if one comes within `t`, else None; for
    /// an app that sends while it waits. Once the companion has gone it
    /// fails, with [`io::ErrorKind::BrokenPipe`].
    pub fn recv_timeout(&mut self, t: Duration) -> io::Result<Option<Vec<u8>>> {
        let gone = || io::Error::new(io::ErrorKind::BrokenPipe, "the companion has gone");
        match &mut self.0 {
            Inner::Relayed(sock, inbox) => {
                sock.set_read_timeout(Some(t))?;
                let r = loop {
                    match inbox.next(sock) {
                        Ok(None | Some((CLOSED, _))) => break Err(gone()),
                        Ok(Some((DATA, f))) => break Ok(Some(f)),
                        Ok(Some(_)) => {}
                        // What has come of a message waits in the inbox.
                        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                            break Ok(None);
                        }
                        Err(e) => break Err(e),
                    }
                };
                sock.set_read_timeout(None)?;
                r
            }
            Inner::Direct(s) => match s.recv_timeout(t)? {
                Some(Msg::Data(f)) => Ok(Some(f)),
                Some(Msg::Closed) => Err(gone()),
                _ => Ok(None),
            },
        }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        if let Inner::Relayed(sock, _) = &mut self.0 {
            let _ = write(sock, CLOSED, &[]);
        }
    }
}

/// The socket the session gave this process, if it did.
fn handed_over() -> Option<UnixStream> {
    let n: RawFd = std::env::var(ENV).ok()?.parse().ok()?;
    let copy = File::from(fd::handed_over(n)?);
    if !copy.metadata().ok()?.file_type().is_socket() {
        return None;
    }
    Some(UnixStream::from(OwnedFd::from(copy)))
}

/// Connects to the paired neochumbyd, checks its key against the one
/// pinned in `p`, and opens `app`'s companion on it. For the session, and
/// for an app run by hand.
pub fn connect(app: &str, p: &Paired, me: &Identity) -> io::Result<Session> {
    let server_key = unhex_key(&p.server_key)?;
    let addr = (p.server.as_str(), p.api_port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address"))?;
    let tcp = TcpStream::connect_timeout(&addr, CONNECT)?;
    // A neochumbyd gone without a word is noticed in about a minute.
    neochumby_sys::net::keepalive(&tcp, 30, 10, 3);
    let mut s = Session::connect(tcp, me)?;
    if s.peer() != &server_key {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "not the paired neochumbyd's key"));
    }
    s.send(&Msg::Hello { name: conf::name() })?;
    match s.recv()? {
        Msg::Welcome => {}
        Msg::Unpaired => {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "neochumbyd no longer pairs with this Chumby"));
        }
        m => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unexpected {m:?}"))),
    }
    s.send(&Msg::Open { app: app.into() })?;
    Ok(s)
}

/// Writes one relay message.
pub fn write(w: &mut impl Write, tag: u8, body: &[u8]) -> io::Result<()> {
    if body.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too long"));
    }
    let mut m = Vec::with_capacity(5 + body.len());
    m.push(tag);
    m.extend_from_slice(&(body.len() as u32).to_be_bytes());
    m.extend_from_slice(body);
    w.write_all(&m)
}

/// Relay messages as they come off a stream, which may hand them over in
/// pieces (and, if it doesn't wait, none at all for now).
#[derive(Default)]
pub struct Inbox {
    buf: Vec<u8>,
}

impl Inbox {
    /// The next message; None at the end of the stream. A stream that
    /// doesn't wait fails with [`io::ErrorKind::WouldBlock`] if a whole one
    /// hasn't come, keeping what has for the next call.
    pub fn next(&mut self, r: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
        loop {
            if let Some(m) = self.take()? {
                return Ok(Some(m));
            }
            let mut chunk = [0u8; 4096];
            match r.read(&mut chunk) {
                Ok(0) => return Ok(None),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    fn take(&mut self) -> io::Result<Option<(u8, Vec<u8>)>> {
        let Some((&tag, rest)) = self.buf.split_first() else { return Ok(None) };
        let Some(len) = rest.first_chunk::<4>() else { return Ok(None) };
        let n = u32::from_be_bytes(*len) as usize;
        if n > MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too long"));
        }
        if self.buf.len() < 5 + n {
            return Ok(None);
        }
        let body = self.buf[5..5 + n].to_vec();
        self.buf.drain(..5 + n);
        Ok(Some((tag, body)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_come_whole_however_the_stream_cuts_them() {
        let mut bytes = Vec::new();
        write(&mut bytes, DATA, b"hello").unwrap();
        write(&mut bytes, CLOSED, &[]).unwrap();
        let (mut r, mut inbox) = (Trickle(&bytes), Inbox::default());
        assert_eq!(inbox.next(&mut r).unwrap(), Some((DATA, b"hello".to_vec())));
        assert_eq!(inbox.next(&mut r).unwrap(), Some((CLOSED, vec![])));
        assert_eq!(inbox.next(&mut r).unwrap(), None);
    }

    /// Hands over at most three bytes a read.
    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            let n = b.len().min(3).min(self.0.len());
            b[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn a_long_frame_is_refused() {
        assert!(write(&mut Vec::new(), DATA, &vec![0; MAX_FRAME + 1]).is_err());
        let mut bytes = vec![DATA];
        bytes.extend_from_slice(&(MAX_FRAME as u32 + 1).to_be_bytes());
        assert!(Inbox::default().next(&mut &bytes[..]).is_err());
    }
}
