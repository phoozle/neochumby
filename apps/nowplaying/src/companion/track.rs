//! What's playing, from shairport-sync's metadata (handed on by the
//! airplay app): the frames to send the Chumby as each item comes
//! (crate::frames).
//!
//! - **The track:** the sender sends its details in a bundle between
//!   `mdst` and `mden`: `minm` (title), `asar` (artist), `asal` (album),
//!   `astm` (length, ms). A bundle that differs from the track is a new
//!   track. `snam` is the sender's name.
//! - **Cover art:** `PICT`, scaled here (art.rs). It may come just before
//!   or after its track's bundle; a new track without one within a couple
//!   of seconds clears the old.
//! - **Where it is:** `prgr` is three RTP times at 44.1 kHz, the track's
//!   start, now and its end, so the time gone and the length.
//! - **Playing or not:** `pbeg` begins a stream, `pend` ends it; `pfls`
//!   (a flush: a pause, or a seek) and `paus` pause; `prsm` and `pres`
//!   resume.
//! - **Remote control:** `clip`, `daid`, `acre` and `dapo` (dacp.rs).

use std::time::{Duration, Instant};

use neochumby_nowplaying::frames::{COVER_SIDE, Cover, Frame, Play, State, Track};

use crate::art;
use crate::dacp::Remote;
use crate::meta::Item;

/// RTP's clock for AirPlay audio.
const RTP_RATE: u64 = 44_100;
/// How close a picture must come before its track's details to be its.
const COVER_WINDOW: Duration = Duration::from_secs(2);

pub struct Tracker {
    track: Track,
    /// The bundle arriving, between `mdst` and `mden`.
    pending: Option<Track>,
    play: Play,
    /// The time gone at `since`; it runs on from there while playing.
    elapsed_ms: u64,
    since: Instant,
    duration_ms: u32,
    /// When cover art last came, and whether the Chumby has some now.
    cover_at: Option<Instant>,
    covered: bool,
    pub remote: Remote,
}

impl Tracker {
    pub fn new(now: Instant) -> Tracker {
        Tracker {
            track: Track::default(),
            pending: None,
            play: Play::Stopped,
            elapsed_ms: 0,
            since: now,
            duration_ms: 0,
            cover_at: None,
            covered: false,
            remote: Remote::default(),
        }
    }

    fn elapsed(&self, now: Instant) -> u64 {
        let mut e = self.elapsed_ms;
        if self.play == Play::Playing {
            e += now.saturating_duration_since(self.since).as_millis() as u64;
        }
        if self.duration_ms > 0 { e.min(self.duration_ms as u64) } else { e }
    }

    /// The STATE frame, now.
    pub fn state(&self, now: Instant) -> Frame {
        Frame::State(State {
            play: self.play,
            controls: self.remote.ready(),
            elapsed_ms: self.elapsed(now).min(u32::MAX as u64) as u32,
            duration_ms: self.duration_ms,
        })
    }

    fn set_play(&mut self, play: Play, now: Instant) -> bool {
        if play == self.play {
            return false;
        }
        self.elapsed_ms = self.elapsed(now);
        self.since = now;
        self.play = play;
        true
    }

    /// The sender's DACP port, looked up for its DACP-ID `id`.
    pub fn found_port(&mut self, id: &str, port: u16) {
        if self.remote.id == id {
            self.remote.port = Some(port);
        }
    }

    /// Takes in one metadata item; the frames it makes.
    pub fn item(&mut self, i: &Item, now: Instant) -> Vec<Frame> {
        let mut out = Vec::new();
        let ready = self.remote.ready();
        match (&i.kind, &i.code) {
            (b"core", b"minm" | b"asar" | b"asal") => {
                let pending = self.pending.is_some();
                let t = self.pending.as_mut().unwrap_or(&mut self.track);
                let field = match &i.code {
                    b"minm" => &mut t.title,
                    b"asar" => &mut t.artist,
                    _ => &mut t.album,
                };
                *field = i.text();
                if !pending {
                    out.push(Frame::Track(self.track.clone()));
                }
            }
            (b"core", b"astm") => {
                if let Ok(ms) = <[u8; 4]>::try_from(&i.data[..]) {
                    self.duration_ms = u32::from_be_bytes(ms);
                }
            }
            (b"ssnc", b"mdst") => {
                self.pending = Some(Track { source: self.track.source.clone(), ..Track::default() });
            }
            (b"ssnc", b"mden") => {
                if let Some(t) = self.pending.take()
                    && !t.is_empty()
                    && t != self.track
                {
                    let new =
                        (&t.title, &t.artist, &t.album) != (&self.track.title, &self.track.artist, &self.track.album);
                    self.track = t;
                    out.push(Frame::Track(self.track.clone()));
                    if new {
                        // A new track starts from the top until prgr says.
                        self.elapsed_ms = 0;
                        self.since = now;
                        let recent = self.cover_at.is_some_and(|at| now.saturating_duration_since(at) < COVER_WINDOW);
                        if self.covered && !recent {
                            self.covered = false;
                            out.push(Frame::Cover(Cover::default()));
                        }
                    }
                }
                out.push(self.state(now));
            }
            (b"ssnc", b"PICT") => {
                let cover = art::thumbnail(&i.data, COVER_SIDE).unwrap_or_default();
                if cover.width == 0 && !i.data.is_empty() {
                    eprintln!(
                        "neochumby-nowplaying companion: cover art that isn't a JPEG or PNG ({} bytes)",
                        i.data.len()
                    );
                }
                self.covered = cover.width > 0;
                self.cover_at = Some(now);
                out.push(Frame::Cover(cover));
            }
            (b"ssnc", b"prgr") => {
                if let Some((elapsed, duration)) = progress(&i.text()) {
                    self.elapsed_ms = elapsed;
                    self.since = now;
                    self.duration_ms = duration;
                    out.push(self.state(now));
                }
            }
            (b"ssnc", b"pbeg" | b"prsm" | b"pres") => {
                if self.set_play(Play::Playing, now) {
                    out.push(self.state(now));
                }
            }
            (b"ssnc", b"pfls" | b"paus") => {
                if self.play == Play::Playing && self.set_play(Play::Paused, now) {
                    out.push(self.state(now));
                }
            }
            (b"ssnc", b"pend") => {
                self.set_play(Play::Stopped, now);
                self.track = Track::default();
                self.elapsed_ms = 0;
                self.duration_ms = 0;
                self.remote = Remote::default();
                out.push(Frame::Track(Track::default()));
                if self.covered {
                    self.covered = false;
                    out.push(Frame::Cover(Cover::default()));
                }
                out.push(self.state(now));
            }
            (b"ssnc", b"snam") => {
                self.track.source = i.text();
                if !self.track.is_empty() {
                    out.push(Frame::Track(self.track.clone()));
                }
            }
            (b"ssnc", b"clip") => self.remote.ip = i.text(),
            (b"ssnc", b"daid") => {
                let id = i.text();
                if id != self.remote.id {
                    self.remote.port = None;
                }
                self.remote.id = id;
            }
            (b"ssnc", b"acre") => self.remote.token = i.text(),
            (b"ssnc", b"dapo") => self.remote.port = i.text().trim().parse().ok().filter(|&p| p != 0),
            _ => {}
        }
        if self.remote.ready() != ready && !out.iter().any(|f| matches!(f, Frame::State(_))) {
            out.push(self.state(now));
        }
        out
    }
}

/// `prgr`'s "start/now/end" RTP times: the time gone and the length, in ms.
/// RTP times wrap at 2^32, so the differences are taken that way.
fn progress(s: &str) -> Option<(u64, u32)> {
    let mut f = s.trim().split('/').map(|v| v.parse::<u32>().ok());
    let (start, now, end) = (f.next()??, f.next()??, f.next()??);
    let ms = |frames: u32| frames as u64 * 1000 / RTP_RATE;
    let duration = ms(end.wrapping_sub(start));
    let elapsed = ms(now.wrapping_sub(start)).min(duration);
    Some((elapsed, duration.min(u32::MAX as u64) as u32))
}

#[cfg(all(test, not(target_arch = "arm")))]
mod tests {
    use super::*;
    use crate::meta::Parser;

    fn item(kind: &[u8; 4], code: &[u8; 4], data: &[u8]) -> Item {
        Item { kind: *kind, code: *code, data: data.to_vec() }
    }

    fn states(frames: &[Frame]) -> Vec<State> {
        frames.iter().filter_map(|f| if let Frame::State(s) = f { Some(*s) } else { None }).collect()
    }

    /// The recorded sample, an item a second.
    #[test]
    fn the_recorded_sample_makes_these_frames() {
        let items = Parser::default().push(include_bytes!("testdata/session.xml"));
        let t0 = Instant::now();
        let mut t = Tracker::new(t0);
        let mut frames = Vec::new();
        for (n, i) in items.iter().enumerate() {
            frames.push(t.item(i, t0 + Duration::from_secs(n as u64)));
        }
        let track = Track {
            title: "Sample Track (Live)".into(),
            artist: "Les Échantillons".into(),
            album: "Made-Up Recordings, Vol. 2".into(),
            source: "Test iPhone".into(),
        };
        // Once clip, daid and acre are in, the sender can be controlled.
        assert_eq!(states(&frames[3]), [State { play: Play::Stopped, controls: true, elapsed_ms: 0, duration_ms: 0 }]);
        // pbeg plays.
        assert_eq!(states(&frames[4])[0].play, Play::Playing);
        // Nothing until the bundle ends, then the track and its length.
        assert!(frames[6..10].iter().all(Vec::is_empty));
        assert_eq!(frames[10][0], Frame::Track(track.clone()));
        assert_eq!(states(&frames[10])[0].duration_ms, 215_000);
        // prgr: 30 s in, of 215.
        assert_eq!(
            states(&frames[11]),
            [State { play: Play::Playing, controls: true, elapsed_ms: 30_000, duration_ms: 215_000 }]
        );
        // The flush, two seconds after prgr: paused at 32 s.
        assert_eq!(
            states(&frames[13]),
            [State { play: Play::Paused, controls: true, elapsed_ms: 32_000, duration_ms: 215_000 }]
        );
        assert_eq!(states(&frames[14])[0].play, Play::Playing);
        // pend: nothing playing, and no remote.
        assert_eq!(frames[15][0], Frame::Track(Track::default()));
        assert_eq!(states(&frames[15]), [State::default()]);
    }

    #[test]
    fn the_port_comes_from_dapo_or_a_lookup() {
        let now = Instant::now();
        let mut t = Tracker::new(now);
        t.item(&item(b"ssnc", b"daid", b"AB"), now);
        t.item(&item(b"ssnc", b"dapo", b"3689"), now);
        assert_eq!(t.remote.port, Some(3689));
        t.item(&item(b"ssnc", b"dapo", b"0"), now);
        assert_eq!(t.remote.port, None, "withdrawn");
        t.found_port("CD", 5000);
        assert_eq!(t.remote.port, None, "another sender's");
        t.found_port("AB", 5000);
        assert_eq!(t.remote.port, Some(5000));
        t.item(&item(b"ssnc", b"daid", b"CD"), now);
        assert_eq!(t.remote.port, None, "a new sender");
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([10, 200, 30]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn bundle(t: &mut Tracker, title: &str, now: Instant) -> Vec<Frame> {
        t.item(&item(b"ssnc", b"mdst", b""), now);
        t.item(&item(b"core", b"minm", title.as_bytes()), now);
        t.item(&item(b"ssnc", b"mden", b""), now)
    }

    fn covers(frames: &[Frame]) -> Vec<(u16, u16)> {
        frames.iter().filter_map(|f| if let Frame::Cover(c) = f { Some((c.width, c.height)) } else { None }).collect()
    }

    #[test]
    fn cover_art_follows_the_track() {
        let t0 = Instant::now();
        let mut t = Tracker::new(t0);
        assert!(covers(&bundle(&mut t, "One", t0)).is_empty());
        assert_eq!(covers(&t.item(&item(b"ssnc", b"PICT", &png(300, 300)), t0)), [(128, 128)]);
        // The same track again keeps it.
        assert!(covers(&bundle(&mut t, "One", t0 + Duration::from_secs(5))).is_empty());
        // A picture just before its track's details is that track's.
        let t1 = t0 + Duration::from_secs(10);
        t.item(&item(b"ssnc", b"PICT", &png(60, 30)), t1);
        assert!(covers(&bundle(&mut t, "Two", t1 + Duration::from_millis(500))).is_empty());
        // A new track with none clears it.
        assert_eq!(covers(&bundle(&mut t, "Three", t1 + Duration::from_secs(60))), [(0, 0)]);
        // Art that can't be decoded is none.
        assert_eq!(covers(&t.item(&item(b"ssnc", b"PICT", b"junk"), t1)), [(0, 0)]);
    }

    #[test]
    fn progress_strings() {
        assert_eq!(progress("1000/45100/442000"), Some((1000, 10_000)));
        // Across RTP's wrap.
        let start = u32::MAX - 44_099;
        assert_eq!(progress(&format!("{start}/44100/{}", 44_100 * 3)), Some((2000, 4000)));
        assert_eq!(progress("1/2"), None);
        assert_eq!(progress("a/b/c"), None);
    }
}
