//! Companions, opened by the session for the apps it runs
//! (neochumby::companion has the relay's messages). The key stays with the
//! session: an app gets a socket, and the session opens the companion of
//! the app it started, named by the app's directory, whatever it asks for.
//!
//! Each app with a companion has a thread relaying for it, which ends when
//! the app's end of the socket closes.

use std::io;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use neochumby::companion::{self, CLOSED, DATA, ERROR, Inbox, OPEN};
use neochumby::conf;
use neochumby_proto::{Identity, Msg, Session};
use neochumby_sys::fd;

/// While a companion is open: how long each side is waited on before the
/// other is looked at.
const POLL: Duration = Duration::from_millis(20);

/// A socket for an app named `app`: the session's end, which this relays
/// for in a thread of its own, and the end to give the app.
pub fn relay(app: &str) -> io::Result<UnixStream> {
    let (mine, theirs) = UnixStream::pair()?;
    fd::set_cloexec(mine.as_fd(), true)?;
    fd::set_cloexec(theirs.as_fd(), true)?;
    let app = app.to_string();
    std::thread::spawn(move || {
        let mut relay = Relay { app: &app, sock: mine, inbox: Inbox::default(), open: None };
        if let Err(e) = relay.run() {
            eprintln!("neochumby: {app}'s companion: {e}");
        }
    });
    Ok(theirs)
}

struct Relay<'a> {
    app: &'a str,
    sock: UnixStream,
    inbox: Inbox,
    open: Option<Session>,
}

impl Relay<'_> {
    /// Relays until the app's end closes.
    fn run(&mut self) -> io::Result<()> {
        loop {
            // With nothing open, only the app can say anything.
            self.sock.set_nonblocking(self.open.is_some())?;
            let m = match self.inbox.next(&mut self.sock) {
                Ok(None) => return Ok(()),
                Ok(Some(m)) => Some(m),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => None,
                Err(e) => return Err(e),
            };
            match m {
                Some((OPEN, _)) => {
                    self.open = None;
                    match self.connect() {
                        Ok(s) => {
                            self.open = Some(s);
                            self.send(OPEN, &[])?;
                        }
                        Err(e) => self.send(ERROR, e.to_string().as_bytes())?,
                    }
                }
                Some((DATA, f)) => {
                    if let Some(s) = &mut self.open
                        && s.send(&Msg::Data(f)).is_err()
                    {
                        self.gone()?;
                    }
                }
                Some((CLOSED, _)) => self.open = None,
                Some(_) => {}
                None => self.pass_on()?,
            }
        }
    }

    fn connect(&self) -> io::Result<Session> {
        let p = conf::paired().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "not paired"))?;
        let me = Identity::from_secret(&p.secret)?;
        companion::connect(self.app, &p, &me)
    }

    /// Passes on what the companion has sent, if anything comes soon.
    fn pass_on(&mut self) -> io::Result<()> {
        let Some(s) = &mut self.open else { return Ok(()) };
        match s.recv_timeout(POLL) {
            Ok(Some(Msg::Data(f))) => self.send(DATA, &f),
            Ok(Some(Msg::Closed)) | Err(_) => self.gone(),
            // The settings, after Welcome: not the app's.
            Ok(Some(_)) | Ok(None) => Ok(()),
        }
    }

    /// The companion has gone: tells the app.
    fn gone(&mut self) -> io::Result<()> {
        self.open = None;
        self.send(CLOSED, &[])
    }

    /// Writes to the app, waiting for room: an app that stops reading
    /// holds up only its own companion.
    fn send(&mut self, tag: u8, body: &[u8]) -> io::Result<()> {
        self.sock.set_nonblocking(false)?;
        companion::write(&mut self.sock, tag, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpaired_an_open_is_refused() {
        let mut sock = relay("airplay").unwrap();
        companion::write(&mut sock, DATA, b"to no one").unwrap();
        companion::write(&mut sock, OPEN, &[]).unwrap();
        let (tag, why) = Inbox::default().next(&mut sock).unwrap().unwrap();
        assert_eq!(tag, ERROR, "{}", String::from_utf8_lossy(&why));
    }
}
