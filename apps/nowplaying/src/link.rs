//! Local sockets on the Chumby: between this app's service and its screen
//! (the service passes on what the companion says about the track, and the
//! screen's buttons come back; crate::frames, framed as
//! neochumby_proto::frame), and from the airplay app's service, which hands
//! out AirPlay's metadata ([`AIRPLAY`]), as only it may hold its companion.
//!
//! The socket is a Unix socket in Linux's abstract namespace, named
//! [`NAME`], rather than a file: it needs no directory both can write, so
//! it works whether the two run as root (today) or jailed with a /tmp each
//! (#45, where an app's exec and its service share a uid). Any process
//! can reach an abstract name, so this app's two ends check that the other
//! runs as their user. The airplay app runs as a user of its own, and only
//! sends, so its feed isn't checked. Elsewhere (tests on a Mac) it is a file in the temporary
//! directory.
//!
//! Neither end waits on the other: the Chumby's kernel lacks the futex
//! operations std's threads would wait on, so each runs one loop. Reads
//! never wait, and writes wait at most [`WRITE`], after which the peer is
//! dropped (the screen connects again, and is sent everything afresh).

use std::io::{self, Write};
use std::os::fd::AsFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

use neochumby_sys::fd::recv_nowait;

use crate::frames::Reader;

pub const NAME: &str = "neochumby-nowplaying";
/// The airplay app's metadata feed (apps/airplay's FEED).
pub const AIRPLAY: &str = "neochumby-airplay-meta";

/// The longest a write to the other end may take.
const WRITE: Duration = Duration::from_millis(500);

#[cfg(target_os = "linux")]
fn address(name: &str) -> io::Result<std::os::unix::net::SocketAddr> {
    use std::os::linux::net::SocketAddrExt;
    std::os::unix::net::SocketAddr::from_abstract_name(name)
}

#[cfg(not(target_os = "linux"))]
fn path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(name)
}

/// The service's end: accepts screens, without waiting.
pub fn listen(name: &str) -> io::Result<UnixListener> {
    #[cfg(target_os = "linux")]
    let l = UnixListener::bind_addr(&address(name)?)?;
    #[cfg(not(target_os = "linux"))]
    let l = {
        let _ = std::fs::remove_file(path(name));
        UnixListener::bind(path(name))?
    };
    l.set_nonblocking(true)?;
    Ok(l)
}

fn stream(name: &str) -> io::Result<UnixStream> {
    #[cfg(target_os = "linux")]
    return UnixStream::connect_addr(&address(name)?);
    #[cfg(not(target_os = "linux"))]
    return UnixStream::connect(path(name));
}

/// The screen's end.
pub fn connect(name: &str) -> io::Result<Peer> {
    Peer::new(stream(name)?)
}

/// The airplay app's feed: read only, from another app's user.
pub fn feed(name: &str) -> io::Result<Peer> {
    let s = stream(name)?;
    s.set_write_timeout(Some(WRITE))?;
    Ok(Peer { s, frames: Reader::default() })
}

/// A screen the listener has, if one is waiting to connect.
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
                format!("{NAME}: a peer running as uid {theirs}, not {ours}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::{Control, Frame};

    #[test]
    fn frames_both_ways() {
        let name = format!("neochumby-nowplaying-test-{}", std::process::id());
        let l = listen(&name).unwrap();
        assert!(accept(&l).unwrap().is_none());
        let mut screen = connect(&name).unwrap();
        let mut service = loop {
            if let Some(p) = accept(&l).unwrap() {
                break p;
            }
        };
        assert!(service.recv().unwrap().is_empty());

        service.send(&Frame::Name("Den".into()).encode()).unwrap();
        service.send(&Frame::Link(true).encode()).unwrap();
        let got: Vec<Frame> = screen.recv().unwrap().iter().filter_map(|f| Frame::parse(f)).collect();
        assert_eq!(got, [Frame::Name("Den".into()), Frame::Link(true)]);

        screen.send(&Frame::Control(Control::Next).encode()).unwrap();
        assert_eq!(service.recv().unwrap(), [Frame::Control(Control::Next).encode()]);

        drop(screen);
        assert!(service.recv().is_err());
    }
}
