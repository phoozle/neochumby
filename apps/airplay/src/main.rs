//! AirPlay's Chumby half (the companion, companion.rs, is neochumbyd's).
//!
//! AirPlay on the Chumby's speaker, as a background service: it opens this
//! app's companion on the paired neochumbyd, which runs an AirPlay receiver
//! named after this Chumby while it is connected and sends what it plays
//! here as PCM ([0] + PCM; [1] when it stops), which goes to /dev/dsp. The
//! speaker is held only while something plays, so apps can have it
//! otherwise. A new stream takes the speaker from whoever holds it, and
//! yields it to a newer player (see `neochumby::audio`); what arrives while
//! the speaker is someone else's is dropped, the connection kept, and the
//! stream plays again once the speaker is free.
//!
//! The companion's metadata frames ([2], shairport-sync's metadata pipe,
//! unparsed) go to whatever connects to the abstract Unix socket [`FEED`]:
//! a now-playing app (apps/nowplaying), since only this service may hold
//! the companion. Nothing is read back, and a reader too slow to keep up is
//! dropped.
//!
//! The session opens the companion for it (neochumby::companion); it opens
//! it again, backing off, whenever the companion goes.

use std::io::{self, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::time::{Duration, Instant};

use neochumby::audio::{self, Speaker};
use neochumby::companion::{self, Link};

// The companion's frames.
const AUDIO: u8 = 0;
const END: u8 = 1;
const META: u8 = 2;

/// Where the metadata goes.
pub const FEED: &str = "neochumby-airplay-meta";

/// The master volume while AirPlay plays. The phone's volume is applied to
/// the audio itself (by shairport-sync), so this only has to be loud enough
/// for the phone's maximum to be the speaker's.
const PLAY_VOLUME: u32 = 90;

/// The speaker for AirPlay's stream: 16-bit stereo at 44.1 kHz.
fn open_speaker() -> io::Result<Speaker> {
    Speaker::open(2, 44100, PLAY_VOLUME)
}

/// Having asked for the speaker, or given it up: when to look again. Soon,
/// as a holder that was asked only has to stop its player.
const ASKED_RETRY: Duration = Duration::from_millis(250);
/// /dev/dsp busy with a program outside the handover: when to try again.
const BUSY_RETRY: Duration = Duration::from_secs(2);

/// The readers of the metadata.
struct Feed {
    listener: Option<UnixListener>,
    readers: Vec<UnixStream>,
}

impl Feed {
    fn new() -> Feed {
        let l = SocketAddr::from_abstract_name(FEED).and_then(|a| UnixListener::bind_addr(&a));
        let l = l.and_then(|l| l.set_nonblocking(true).map(|()| l));
        Feed { listener: l.inspect_err(|e| eprintln!("neochumby-airplay: {FEED}: {e}")).ok(), readers: Vec::new() }
    }

    fn send(&mut self, frame: &[u8]) {
        while let Some(Ok((r, _))) = self.listener.as_ref().map(UnixListener::accept) {
            if r.set_nonblocking(false).and_then(|()| r.set_write_timeout(Some(Duration::from_millis(500)))).is_ok() {
                self.readers.push(r);
            }
        }
        self.readers.retain_mut(|r| neochumby_proto::frame::write(r, frame).is_ok());
    }
}

fn play(mut link: Link, feed: &mut Feed) -> io::Result<()> {
    let mut speaker: Option<Speaker> = None;
    // While the speaker is someone else's: when to try it again.
    let mut busy_until: Option<Instant> = None;
    // This stream gave the speaker up to a newer player: it takes it back
    // only once the speaker is free, rather than asking for it.
    let mut yielded = false;
    loop {
        let Some(frame) = link.recv()? else {
            return Err(io::Error::other("the companion has gone"));
        };
        match frame.split_first() {
            Some((&AUDIO, pcm)) => {
                if speaker.as_ref().is_some_and(Speaker::yield_requested) {
                    eprintln!("neochumby-airplay: yielding the speaker to a newer player");
                    speaker = None;
                    yielded = true;
                    busy_until = Some(Instant::now() + ASKED_RETRY);
                }
                if speaker.is_none() && busy_until.is_none_or(|t| Instant::now() >= t) {
                    if yielded && !audio::free() {
                        busy_until = Some(Instant::now() + ASKED_RETRY);
                    } else {
                        match open_speaker() {
                            Ok(sp) => {
                                speaker = Some(sp);
                                busy_until = None;
                                yielded = false;
                            }
                            Err(e) => {
                                if busy_until.is_none() {
                                    eprintln!("neochumby-airplay: the speaker: {e}; dropping audio until it is free");
                                }
                                let wait = if e.kind() == io::ErrorKind::WouldBlock { ASKED_RETRY } else { BUSY_RETRY };
                                busy_until = Some(Instant::now() + wait);
                            }
                        }
                    }
                }
                if let Some(sp) = speaker.as_mut()
                    && let Err(e) = sp.write_all(pcm)
                {
                    eprintln!("neochumby-airplay: /dev/dsp: {e}");
                    speaker = None;
                }
            }
            Some((&META, _)) => feed.send(&frame),
            Some((&END, _)) => {
                speaker = None;
                busy_until = None;
                yielded = false;
                audio::withdraw();
            }
            _ => {}
        }
    }
}

fn main() {
    neochumby::debug::install("neochumby-airplay");
    let mut feed = Feed::new();
    let mut backoff = 2;
    loop {
        let started = Instant::now();
        let r = companion::open(neochumby::companion!()).and_then(|l| play(l, &mut feed));
        // The stream has gone with the connection: so has any wish for the
        // speaker.
        audio::withdraw();
        if let Err(e) = r {
            eprintln!("neochumby-airplay: {e}");
        }
        // A connection that lasted was a good one: start backing off again.
        if started.elapsed() > Duration::from_secs(60) {
            backoff = 2;
        }
        neochumby::sleep_ms(backoff * 1000);
        backoff = (backoff * 2).min(60);
    }
}
