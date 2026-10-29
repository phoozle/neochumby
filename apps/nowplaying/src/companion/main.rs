//! Now playing's companion: its server half, which neochumbyd runs for each
//! Chumby that opens it (the Chumby's half is main.rs and screen.rs). It
//! does what the Chumby shouldn't: its service passes on AirPlay's metadata
//! as the airplay app hands it out (shairport-sync's metadata pipe,
//! unparsed), and this parses it (meta.rs), keeps track of what's playing
//! (track.rs), scales the cover art for the screen (art.rs; the ARM926 has
//! no FPU) and sends the result back (neochumby_nowplaying::frames). The
//! screen's play/pause, next and previous go to the sender as DACP
//! requests (dacp.rs), from this host, as the sender's address and token
//! are in the metadata.
//!
//! From the environment, NEOCHUMBY_CHUMBY (neochumbyd's), for the name the
//! receiver goes by. Exits when its stdin closes: the Chumby has gone.

mod art;
mod dacp;
mod meta;
mod track;

use std::io;
use std::sync::mpsc::{self, SyncSender};
use std::time::Instant;

use neochumby_nowplaying::frames::{Control, Frame, META};
use neochumby_proto::{frame, valid_name};

use crate::track::Tracker;

/// What the companion's loop waits on, from its threads.
enum Event {
    /// AirPlay's metadata, as it came.
    Meta(Vec<u8>),
    /// A button on the Chumby.
    Control(Control),
    /// The DACP port a command found for the sender with this DACP-ID.
    Port(String, u16),
    /// The Chumby has gone (neochumbyd closed stdin).
    Gone,
}

/// "kitchen_chumby" → "Kitchen Chumby", as the airplay app names its
/// receiver.
fn display_name(name: &str) -> String {
    name.split(['_', '-'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Sends a button's command to the sender, on a thread of its own: a
/// lookup and a request can take a couple of seconds.
fn control(remote: &dacp::Remote, c: Control, tx: SyncSender<Event>) {
    if !remote.ready() {
        eprintln!("neochumby-nowplaying companion: {}: this sender takes no remote control", dacp::command(c));
        return;
    }
    let remote = remote.clone();
    std::thread::spawn(move || match dacp::send(&remote, c) {
        Ok(port) => {
            if remote.port != Some(port) {
                let _ = tx.send(Event::Port(remote.id.clone(), port));
            }
        }
        Err(e) => eprintln!("neochumby-nowplaying companion: {}: {e}", dacp::command(c)),
    });
}

fn send(out: &mut impl io::Write, frames: Vec<Frame>) -> io::Result<()> {
    frames.iter().try_for_each(|f| frame::write(out, &f.encode()))
}

fn run() -> io::Result<()> {
    let name = std::env::var("NEOCHUMBY_CHUMBY")
        .ok()
        .filter(|n| valid_name(n))
        .ok_or_else(|| io::Error::other("no NEOCHUMBY_CHUMBY"))?;

    let (tx, rx) = mpsc::sync_channel::<Event>(64);
    let in_tx = tx.clone();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        while let Ok(Some(f)) = frame::read(&mut stdin) {
            let event = match f.split_first() {
                Some((&META, bytes)) => Event::Meta(bytes.to_vec()),
                _ => match Frame::parse(&f) {
                    Some(Frame::Control(c)) => Event::Control(c),
                    _ => continue,
                },
            };
            if in_tx.send(event).is_err() {
                return;
            }
        }
        let _ = in_tx.send(Event::Gone);
    });

    let mut out = io::stdout().lock();
    frame::write(&mut out, &Frame::Name(display_name(&name)).encode())?;
    let mut parser = meta::Parser::default();
    let mut tracker = Tracker::new(Instant::now());
    loop {
        let Ok(event) = rx.recv() else { return Ok(()) };
        let now = Instant::now();
        match event {
            Event::Meta(bytes) => {
                for item in parser.push(&bytes) {
                    send(&mut out, tracker.item(&item, now))?;
                }
            }
            Event::Control(c) => control(&tracker.remote, c, tx.clone()),
            Event::Port(id, port) => tracker.found_port(&id, port),
            Event::Gone => return Ok(()),
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("neochumby-nowplaying companion: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn names() {
        assert_eq!(super::display_name("kitchen_chumby"), "Kitchen Chumby");
        assert_eq!(super::display_name("den-2"), "Den 2");
    }
}
