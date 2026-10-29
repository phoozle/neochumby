//! AirPlay's Chumby half: the service (the companion, companion/, is
//! neochumbyd's; the now-playing screen is screen.rs).
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
//! It is the only one to open the companion (a second connection would
//! start a second receiver), so it is also the now-playing screen's way to
//! it: what the companion says about the track (neochumby_airplay::frames)
//! goes on to the screen over a local socket (neochumby_airplay::link),
//! and the screen's buttons come back the same way and go on to the
//! companion. A screen that connects is told everything at once
//! (neochumby_airplay::hub). When a stream begins from stopped, it asks the
//! session to open the screen, which the session allows over the start app
//! only.
//!
//! As a music app (#54), it takes the session's `pause` and `resume`: the
//! sender is asked to pause or play, and the audio is dropped meanwhile in
//! case it can't be.
//!
//! Connects to the paired neochumbyd (/psp/neochumby.conf) and checks its
//! key against the one pinned there; reconnects, backing off, whenever the
//! connection goes. The screens are looked after meanwhile.

use std::io::{self, Write};
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use neochumby::audio::{self, Speaker};
use neochumby::companion;
use neochumby::conf;
use neochumby::session::{Notice, Pipe, Request};
use neochumby_airplay::frames::{AUDIO, Control, END, Frame};
use neochumby_airplay::hub::Hub;
use neochumby_airplay::link::{self, Peer};
use neochumby_proto::{Identity, Msg, Session};

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
/// The longest the companion's connection is waited on before the screens
/// are looked at again.
const TICK: Duration = Duration::from_millis(50);

/// The now-playing screens connected to this service: one, usually, or
/// none.
struct Screens {
    listener: Option<UnixListener>,
    peers: Vec<Peer>,
}

impl Screens {
    fn new() -> Screens {
        let listener = link::listen(link::NAME)
            .inspect_err(|e| eprintln!("neochumby-airplay: the screen's socket: {e}; playing without it"))
            .ok();
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
                    Err(e) => eprintln!("neochumby-airplay: a screen: {e}"),
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

/// The session's request pipe: for opening the screen when a stream
/// begins, the app it says is in front, and, as a music app, `pause` and
/// `resume` (the start app's MUTE, relayed by the session; #54).
struct Front {
    pipe: Option<Pipe>,
    app: Option<String>,
    /// Paused by the session: the audio is dropped until `resume`, whether
    /// or not the sender takes remote control.
    held: bool,
    /// `pause` and `resume` as commands for the sender, not yet sent.
    asked: Vec<Control>,
}

impl Front {
    fn listen(&mut self) {
        let Some(p) = self.pipe.as_mut() else { return };
        for line in p.receive().lines {
            // `pause` and `resume` are read as lines, which a session
            // without music apps never sends (its Notice doesn't have them
            // yet).
            match line.as_str() {
                "pause" => {
                    self.held = true;
                    self.asked.push(Control::Pause);
                }
                "resume" => {
                    self.held = false;
                    self.asked.push(Control::Play);
                }
                _ => {
                    if let Some(Notice::Front(app)) = Notice::parse(&line) {
                        self.app = Some(app);
                    }
                }
            }
        }
    }

    /// Asks for this app's screen in front, unless it's there already. The
    /// session decides: over the start app, yes; over anything else, no.
    fn open_screen(&mut self) {
        let me = neochumby::companion!().app();
        self.listen();
        if self.app.as_deref() == Some(me) {
            return;
        }
        if let Some(p) = self.pipe.as_mut()
            && !p.try_send(&Request::Open(me.into()).line())
        {
            eprintln!("neochumby-airplay: couldn't ask the session for the screen");
        }
    }
}

/// The screens' buttons, on to the companion: only CONTROL frames.
fn controls(frames: Vec<Vec<u8>>, s: &mut Session) -> io::Result<()> {
    for f in frames {
        if let Some(Frame::Control(_)) = Frame::parse(&f) {
            s.send(&Msg::Data(f))?;
        }
    }
    Ok(())
}

fn play(mut s: Session, screens: &mut Screens, hub: &mut Hub, front: &mut Front) -> io::Result<()> {
    let mut speaker: Option<Speaker> = None;
    // While the speaker is someone else's: when to try it again.
    let mut busy_until: Option<Instant> = None;
    // This stream gave the speaker up to a newer player: it takes it back
    // only once the speaker is free, rather than asking for it.
    let mut yielded = false;
    loop {
        let got = screens.poll(hub);
        controls(got, &mut s)?;
        front.listen();
        for c in std::mem::take(&mut front.asked) {
            s.send(&Msg::Data(Frame::Control(c).encode()))?;
        }
        let frame = match s.recv_timeout(TICK)? {
            Some(Msg::Data(f)) => f,
            Some(Msg::Closed) => return Err(io::Error::other("the companion has gone")),
            _ => continue,
        };
        match frame.split_first() {
            Some((&AUDIO, _)) if front.held => {
                // Paused by the session: silent, and the speaker free.
                if speaker.take().is_some() {
                    audio::withdraw();
                }
            }
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
            Some((&END, _)) => {
                speaker = None;
                busy_until = None;
                yielded = false;
                audio::withdraw();
            }
            // What the companion says about the track: kept, and passed on.
            Some(_) => {
                let u = hub.update(&frame);
                if u.forward {
                    screens.send(&frame);
                }
                if u.started {
                    front.open_screen();
                }
            }
            None => {}
        }
    }
}

fn main() {
    neochumby::debug::install("neochumby-airplay");
    let Some(p) = conf::paired() else {
        eprintln!("neochumby-airplay: not paired");
        std::process::exit(1);
    };
    let me = match Identity::from_secret(&p.secret) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("neochumby-airplay: {}: {e}", conf::KEY);
            std::process::exit(1);
        }
    };
    let mut screens = Screens::new();
    let mut hub = Hub::default();
    let mut front = Front { pipe: Pipe::from_env(), app: None, held: false, asked: Vec::new() };
    let mut backoff = 2;
    loop {
        let started = Instant::now();
        let r = companion::open(neochumby::companion!(), &p, &me).and_then(|s| {
            screens.send(&hub.link(true));
            play(s, &mut screens, &mut hub, &mut front)
        });
        // The stream has gone with the connection: so has any wish for the
        // speaker, and what the screen showed.
        audio::withdraw();
        if hub.linked() {
            screens.send(&hub.link(false));
        }
        if let Err(e) = r {
            eprintln!("neochumby-airplay: {e}");
        }
        // A connection that lasted was a good one: start backing off again.
        if started.elapsed() > Duration::from_secs(60) {
            backoff = 2;
        }
        // The wait, with the screens looked after: they're told there's no
        // companion, and their buttons have nowhere to go.
        let until = Instant::now() + Duration::from_secs(backoff);
        while Instant::now() < until {
            screens.poll(&hub);
            front.listen();
            front.asked.clear();
            neochumby::sleep_ms(100);
        }
        backoff = (backoff * 2).min(60);
    }
}
