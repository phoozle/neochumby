//! Now playing's service, on the Chumby (the screen is screen.rs, the
//! companion companion/). It runs for the whole session, so that the
//! screen, whenever it's opened, knows what's playing.
//!
//! - **From the airplay app:** AirPlay's metadata, as its service hands it
//!   out on a local socket (neochumby_nowplaying::link::AIRPLAY), since only
//!   that service may hold the airplay companion. It goes on, unread, to
//!   this app's own companion on neochumbyd, which parses it and scales the
//!   cover art (the ARM926 has no FPU).
//! - **From the companion:** the receiver's name, the track, its cover and
//!   the play state (neochumby_nowplaying::frames). The latest of each is
//!   kept (neochumby_nowplaying::hub) and passed to the screen over a local
//!   socket of this app's own (neochumby_nowplaying::link::NAME).
//! - **From the screen:** play/pause, next and previous, on to the
//!   companion, which asks the sender.
//!
//! The session opens the companion for it (neochumby::companion); it opens
//! it again, backing off, whenever the companion goes, and tries the
//! airplay app's socket each second until it's there. One loop, without
//! threads (the Chumby's kernel lacks the futex calls std's would wait on).

use std::io;
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use neochumby::companion::{self, Link};
use neochumby_nowplaying::frames::{Frame, META};
use neochumby_nowplaying::hub::Hub;
use neochumby_nowplaying::link::{self, Peer};

/// The longest the companion is waited on before the sockets are looked
/// at again.
const TICK: Duration = Duration::from_millis(50);
/// How often to look for the airplay app's socket while it isn't there.
const FEED_RETRY: Duration = Duration::from_secs(1);

/// The screens connected to this service: one, usually, or none.
struct Screens {
    listener: Option<UnixListener>,
    peers: Vec<Peer>,
}

impl Screens {
    fn new() -> Screens {
        let listener =
            link::listen(link::NAME).inspect_err(|e| eprintln!("neochumby-nowplaying: the screen's socket: {e}")).ok();
        Screens { listener, peers: Vec::new() }
    }

    /// Takes in screens that have connected, telling each everything, and
    /// returns the frames the screens have sent. Never waits, but on a
    /// screen slow to take what it's sent.
    fn poll(&mut self, hub: &Hub) -> Vec<Vec<u8>> {
        if let Some(l) = &self.listener {
            // A few at a time: an error that persists can't hold the loop.
            for _ in 0..4 {
                match link::accept(l) {
                    Ok(Some(mut p)) => {
                        if hub.replay().iter().all(|f| p.send(f).is_ok()) {
                            self.peers.push(p);
                        }
                    }
                    Ok(None) => break,
                    Err(e) => eprintln!("neochumby-nowplaying: a screen: {e}"),
                }
            }
        }
        let mut got = Vec::new();
        self.peers.retain_mut(|p| p.recv().map(|f| got.extend(f)).is_ok());
        got
    }

    /// Sends `f` to every screen, dropping any that can't take it.
    fn send(&mut self, f: &[u8]) {
        self.peers.retain_mut(|p| p.send(f).is_ok());
    }
}

/// The companion, while it's open, and when to try it again while not.
struct Companion {
    link: Option<(Link, Instant)>,
    retry_at: Instant,
    backoff: u64,
}

impl Companion {
    fn open(&mut self) {
        if self.link.is_some() || Instant::now() < self.retry_at {
            return;
        }
        match companion::open(neochumby::companion!()) {
            Ok(l) => self.link = Some((l, Instant::now())),
            Err(e) => self.failed(e),
        }
    }

    /// It has gone, or couldn't be opened: wait longer each time, up to a
    /// minute, unless it had lasted a minute.
    fn failed(&mut self, e: io::Error) {
        eprintln!("neochumby-nowplaying: the companion: {e}");
        if self.link.take().is_some_and(|(_, since)| since.elapsed() > Duration::from_secs(60)) {
            self.backoff = 2;
        }
        self.retry_at = Instant::now() + Duration::from_secs(self.backoff);
        self.backoff = (self.backoff * 2).min(60);
    }

    fn send(&mut self, f: &[u8]) {
        if let Some((l, _)) = self.link.as_mut()
            && let Err(e) = l.send(f)
        {
            self.failed(e);
        }
    }
}

fn main() {
    neochumby::debug::install("neochumby-nowplaying");
    let mut screens = Screens::new();
    let mut hub = Hub::default();
    let mut companion = Companion { link: None, retry_at: Instant::now(), backoff: 2 };
    let mut feed: Option<Peer> = None;
    let mut feed_at = Instant::now();
    loop {
        companion.open();
        if feed.is_none() && Instant::now() >= feed_at {
            match link::feed(link::AIRPLAY) {
                Ok(p) => {
                    eprintln!("neochumby-nowplaying: the airplay app's metadata: connected");
                    feed = Some(p);
                }
                Err(_) => feed_at = Instant::now() + FEED_RETRY,
            }
        }
        if let Some(f) = hub.link(companion.link.is_some() && feed.is_some()) {
            screens.send(&f);
        }

        // AirPlay's metadata, on to the companion as it came.
        if let Some(p) = feed.as_mut() {
            match p.recv() {
                Ok(frames) => {
                    for f in frames.iter().filter(|f| f.first() == Some(&META)) {
                        companion.send(f);
                    }
                }
                Err(e) => {
                    eprintln!("neochumby-nowplaying: the airplay app's metadata: {e}");
                    feed = None;
                    feed_at = Instant::now() + FEED_RETRY;
                }
            }
        }

        // The screens' buttons, on to the companion.
        for f in screens.poll(&hub) {
            if let Some(Frame::Control(_)) = Frame::parse(&f) {
                companion.send(&f);
            }
        }

        // What the companion says, kept and passed on.
        match companion.link.as_mut().map(|(l, _)| l.recv_timeout(TICK)) {
            Some(Ok(Some(f))) => {
                if hub.update(&f) {
                    screens.send(&f);
                }
            }
            Some(Ok(None)) => {}
            Some(Err(e)) => companion.failed(e),
            None => neochumby::sleep_ms(TICK.as_millis() as u32),
        }
    }
}
