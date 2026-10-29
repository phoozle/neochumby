//! Now playing's screen, the app's `exec`: the track's title,
//! artist, album and cover art, a progress bar, and previous, play/pause
//! and next; or, with nothing playing, "Ready for AirPlay" and the name to
//! choose on the phone (neochumby_nowplaying::ui draws it).
//!
//! It knows only what the service tells it, over the local socket
//! (neochumby_nowplaying::link): the service has the companion. A button goes
//! back the same way, to the sender as a remote-control command; play/pause
//! shows its effect at once, until the sender's own report a second or two
//! later. Without the service (it starts with the session, and only on a
//! paired Chumby) it says it's waiting, and tries again each second. It
//! never opens itself: it's opened from the app list like any app.
//!
//! One loop, without threads (the Chumby's kernel lacks the futex calls
//! std's would wait on): the socket and the touchscreen are read without
//! waiting, and the screen is drawn when something changed, and each
//! half-second while the progress moves. The top button and the corner
//! hold are the session's, as for any app.

use std::time::{Duration, Instant};

use neochumby::fb::Fb;
use neochumby::input::{Touch, Touchscreen};
use neochumby_nowplaying::frames::{Control, Frame, Play};
use neochumby_nowplaying::link::{self, Peer};
use neochumby_nowplaying::nowplaying::NowPlaying;
use neochumby_nowplaying::ui::{self, Canvas};

/// How often the loop runs.
const TICK_MS: u32 = 30;
/// How often the progress is redrawn while playing.
const PROGRESS: Duration = Duration::from_millis(500);
/// A press this soon after the last is the touchscreen's echo
/// (docs/hardware.md), not a second press: play/pause would undo itself.
const ECHO: Duration = Duration::from_millis(350);
/// How long to wait before trying the service again.
const RETRY: Duration = Duration::from_secs(1);

fn main() {
    neochumby::debug::install("neochumby-nowplaying");
    let mut fb = Fb::open().expect("/dev/fb0");
    let mut touch = Touchscreen::open();
    let mut canvas = Canvas::new();
    let mut np = NowPlaying::default();
    let clock = Instant::now();
    let now_ms = || clock.elapsed().as_millis() as u64;

    let mut service: Option<Peer> = None;
    let mut retry_at = Instant::now();
    let mut pressed: Option<Control> = None;
    let mut last_press: Option<Instant> = None;
    let mut drawn: Option<Instant> = None;
    let mut dirty = true;
    loop {
        if service.is_none() && Instant::now() >= retry_at {
            match link::connect(link::NAME) {
                Ok(p) => service = Some(p),
                Err(_) => retry_at = Instant::now() + RETRY,
            }
        }
        if let Some(p) = service.as_mut() {
            match p.recv() {
                Ok(frames) => {
                    for f in frames.iter().filter_map(|f| Frame::parse(f)) {
                        dirty |= np.apply(f, now_ms());
                    }
                }
                Err(e) => {
                    eprintln!("neochumby-nowplaying: the service: {e}");
                    service = None;
                    retry_at = Instant::now() + RETRY;
                    dirty |= np.apply(Frame::Link(false), now_ms());
                }
            }
        }

        for t in touch.poll() {
            match t {
                Touch::Down { x, y, .. } => {
                    let hit = if np.idle() { None } else { ui::hit(x, y) };
                    if hit != pressed {
                        pressed = hit;
                        dirty = true;
                    }
                }
                Touch::Up => {
                    let Some(c) = pressed.take() else { continue };
                    dirty = true;
                    if last_press.is_some_and(|t| t.elapsed() < ECHO) || !np.state.controls {
                        continue;
                    }
                    last_press = Some(Instant::now());
                    let sent = service.as_mut().map(|p| p.send(&Frame::Control(c).encode()));
                    match sent {
                        Some(Ok(())) => np.pressed(c, now_ms()),
                        Some(Err(e)) => {
                            eprintln!("neochumby-nowplaying: the service: {e}");
                            service = None;
                        }
                        None => {}
                    }
                }
            }
        }

        let moving = np.state.play == Play::Playing && !np.idle();
        if dirty || (moving && drawn.is_none_or(|t| t.elapsed() >= PROGRESS)) {
            ui::draw(&mut canvas, &np, now_ms(), pressed);
            fb.pixels().copy_from_slice(canvas.pixels());
            drawn = Some(Instant::now());
            dirty = false;
        }
        neochumby::sleep_ms(TICK_MS);
    }
}
