//! The classic service: what has to keep going while classic isn't in
//! front, decided from the same files the app writes.
//!
//! - **Night and the backlight.** The schedule (wx_nightmode: auto, on,
//!   off), the sun lines in the manifest, and a NIGHT tap's override
//!   (wx_night, wx_night_until): state::night_now, as the app decides it.
//!   The app sets the backlight while it runs; this keeps it right after,
//!   for example dimming at sunset during a game.
//! - **Music.** wx_music, wx_station and wx_volume, and the stations in the
//!   manifest: btplay plays while the speaker is held unmuted
//!   (`neochumby::audio::Unmuted`). The speaker goes to the newest player:
//!   when another app asks for it (a game's sound), the music stops and the
//!   speaker is given up; it starts again once the speaker is free.
//!
//! **Nothing is left behind.** On SIGTERM (CHUMBY MODE, or the session
//! stopping it) the service first puts the backlight at its day level (one
//! /proc write, before anything slow), then stops the stream and gives the
//! speaker back. A kill it can't catch is cleaned up when it next starts:
//! while btplay plays for it, a record in the run directory
//! (`service-playing`, with the volume from before) says so, and a new run
//! stops that stream; it puts the speaker back too, but only if no other
//! player holds it now.
//!
//! The device's side (the backlight, btplay, the speaker handover) is
//! behind [`Effects`], so all of this runs, and is tested, on a host.
//!
//! It reads the session's `front` notices, and logs which app is in front.
//! It never asks the session to open classic: the top button already
//! brings classic back.

use std::path::{Path, PathBuf};

use crate::state::{self, mtime, read_int, resolve_dir};

/// How often the files are looked at, in ms.
pub const POLL_MS: u32 = 500;

/// What the service does to the device: behind a trait, so that the
/// music's state machine and the cleanup are tested on a host.
pub trait Effects {
    fn backlight(&mut self, night: bool);
    /// Starts btplay's stream.
    fn start_player(&mut self, url: &str);
    /// Stops btplay's stream.
    fn stop_player(&mut self);
    /// The master volume now, if it can be read.
    fn volume(&mut self) -> Option<u32>;
    fn set_volume(&mut self, v: u32);
    /// Takes the speaker at a volume (audio::Unmuted::at).
    fn take_speaker(&mut self, vol: u32) -> Take;
    /// Gives it back: muted, the volume as it was (dropping the Unmuted).
    fn release_speaker(&mut self);
    /// Whether another player has asked for the speaker we hold.
    fn yield_requested(&mut self) -> bool;
    /// Whether no other live player holds the speaker or wants it.
    fn speaker_free(&mut self) -> bool;
    /// Withdraws our request for the speaker.
    fn withdraw(&mut self);
    /// Mutes the speaker and sets the volume back, without the handover:
    /// for a killed run's leftovers.
    fn speaker_back(&mut self, volume_before: Option<u32>);
}

/// What taking the speaker did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Take {
    Taken,
    /// Another player holds it and has been asked to yield: try again soon.
    Asked,
    /// It failed otherwise: try again later.
    Failed,
}

/// The wait before trying again after a failure that isn't the handover's.
pub const RETRY_MS: i64 = 5000;

/// The music, through the speaker handover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Music {
    Off,
    /// Wants to play, and asked for the speaker (or failed to take it): the
    /// volume read before the first try, and when to try again.
    Asked {
        before: Option<u32>,
        retry_at: i64,
    },
    Playing {
        url: String,
        vol: u32,
    },
    /// Gave the speaker to another player; waiting until it is free.
    Yielded,
}

/// The record of a stream the service started, in its run directory.
const MARK: &str = "service-playing";

/// Notes that btplay is playing for this service, and the master volume it
/// found (None: it couldn't read it).
pub fn mark_playing(dir: &Path, volume_before: Option<u32>) {
    let v = volume_before.map_or("-".to_string(), |v| v.to_string());
    let _ = state::write_setting(&dir.join(MARK), format!("playing {v}\n").as_bytes());
}

/// The stream has stopped and the speaker is back.
pub fn clear_mark(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(MARK));
}

/// The service's music, and where its record goes (None: no private run
/// directory, so no record).
pub struct Player {
    pub music: Music,
    pub run: Option<PathBuf>,
}

impl Player {
    pub fn new(run: Option<PathBuf>, fx: &mut impl Effects) -> Player {
        // Started again while another player has the speaker (a restart
        // during a game): wait for it rather than take it.
        let music = if fx.speaker_free() { Music::Off } else { Music::Yielded };
        Player { music, run }
    }

    fn mark(&self, before: Option<u32>) {
        if let Some(r) = &self.run {
            mark_playing(r, before);
        }
    }

    fn unmark(&self) {
        if let Some(r) = &self.run {
            clear_mark(r);
        }
    }

    /// Stops the stream and gives the speaker back.
    fn stop(&mut self, fx: &mut impl Effects) {
        fx.stop_player();
        fx.release_speaker();
        self.unmark();
    }

    /// One try at taking the speaker and starting `url`.
    fn start(&mut self, fx: &mut impl Effects, url: &str, vol: u32, before: Option<u32>, now: i64) -> Music {
        match fx.take_speaker(vol) {
            Take::Taken => {
                self.mark(before);
                fx.start_player(url);
                Music::Playing { url: url.to_string(), vol }
            }
            Take::Asked => Music::Asked { before, retry_at: now },
            Take::Failed => Music::Asked { before, retry_at: now + RETRY_MS },
        }
    }

    /// One step towards what is wanted, at `now` (ms).
    pub fn step(&mut self, fx: &mut impl Effects, want: Option<(String, u32)>, now: i64) {
        let music = std::mem::replace(&mut self.music, Music::Off);
        self.music = match (music, want) {
            (Music::Playing { .. }, None) => {
                self.stop(fx);
                Music::Off
            }
            // Another player wants the speaker: stop and give it up, before
            // anything else (a new station included, which would ask it to
            // yield back).
            (Music::Playing { .. }, Some(_)) if fx.yield_requested() => {
                self.stop(fx);
                Music::Yielded
            }
            (Music::Playing { url, vol }, Some((u, v))) if url == u => {
                if vol != v {
                    fx.set_volume(v);
                }
                Music::Playing { url, vol: v }
            }
            (Music::Playing { .. }, Some((u, v))) => {
                // Another station: stop, let go, and take it again.
                self.stop(fx);
                let before = fx.volume();
                self.start(fx, &u, v, before, now)
            }
            (Music::Yielded, Some((u, v))) => {
                if fx.speaker_free() {
                    let before = fx.volume();
                    self.start(fx, &u, v, before, now)
                } else {
                    Music::Yielded
                }
            }
            (Music::Asked { before, retry_at }, Some((u, v))) => {
                if now >= retry_at {
                    self.start(fx, &u, v, before, now)
                } else {
                    Music::Asked { before, retry_at }
                }
            }
            (Music::Off, Some((u, v))) => {
                let before = fx.volume();
                self.start(fx, &u, v, before, now)
            }
            (Music::Asked { .. }, None) => {
                fx.withdraw();
                Music::Off
            }
            (Music::Yielded | Music::Off, None) => Music::Off,
        };
    }

    /// On SIGTERM: the day backlight first (one quick write, so it happens
    /// even if what follows is slow and the session's SIGKILL comes), then
    /// the music stopped and the speaker given back.
    pub fn shutdown(&mut self, fx: &mut impl Effects) {
        fx.backlight(false);
        self.step(fx, None, 0);
        self.unmark();
    }
}

/// On start: if a previous run was killed while playing, stop its stream;
/// and if no other player holds the speaker now, mute it and put the
/// volume back. True if there was one.
pub fn recover(dir: &Path, fx: &mut impl Effects) -> bool {
    let Some(m) = state::read_nofollow(&dir.join(MARK), 64) else { return false };
    let text = String::from_utf8_lossy(&m);
    let before = text.trim().strip_prefix("playing ").and_then(|v| v.parse().ok());
    fx.stop_player();
    if fx.speaker_free() {
        fx.speaker_back(before);
    }
    clear_mark(dir);
    true
}

/// What the service should be doing now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wants {
    pub night: bool,
    /// The stream and volume (0-100) to play, or None for silence.
    pub music: Option<(String, u32)>,
}

/// The manifest's sun and station lines, read again when the set changes.
#[derive(Default)]
pub struct Feed {
    dir: PathBuf,
    mtime: i64,
    len: u64,
    pub sun: Vec<(i64, i64)>,
    pub stations: Vec<String>,
}

impl Feed {
    /// Re-reads the manifest if `current` points elsewhere or it changed.
    pub fn refresh(&mut self, base: &Path) {
        let dir = resolve_dir(base);
        let path = dir.join("manifest.txt");
        let Ok(m) = std::fs::metadata(&path) else { return };
        // The size too: a second push within the same second has the same
        // mtime.
        if dir == self.dir && mtime(&m) == self.mtime && m.len() == self.len {
            return;
        }
        // Read with a cap: the file can grow after the look.
        if let Some(text) = state::read_nofollow(&path, crate::manifest::MAX_MANIFEST_BYTES) {
            (self.sun, self.stations) = state::feed(&text);
            self.dir = dir;
            self.mtime = mtime(&m);
            self.len = m.len();
        }
    }
}

/// What the settings in `st` (the /psp directory) and the feed ask for at
/// `now`.
pub fn wants(st: &Path, feed: &Feed, now: i64) -> Wants {
    let mode = read_int(&st.join("wx_nightmode"), 0).clamp(0, 2);
    let last = read_int(&st.join("wx_night"), 0) == 1;
    let until = state::read_time(&st.join("wx_night_until"));
    let night = state::night_now(mode, &feed.sun, now, until, last);
    let music = (read_int(&st.join("wx_music"), 0) == 1)
        .then(|| {
            let station = read_int(&st.join("wx_station"), 0);
            let url = feed.stations.get(usize::try_from(station).ok()?).filter(|u| !u.is_empty())?;
            let vol = read_int(&st.join("wx_volume"), 65);
            let vol = if (0..=100).contains(&vol) { vol } else { 65 };
            Some((url.clone(), vol as u32))
        })
        .flatten();
    Wants { night, music }
}

#[cfg(target_arch = "arm")]
pub mod device {
    //! The service on the Chumby.

    use std::path::Path;
    use std::process::Command;
    use std::sync::atomic::Ordering;

    use neochumby::audio::{self, Unmuted};
    use neochumby::session::{Notice, Pipe};

    use super::{Effects, Feed, POLL_MS, Player, Take, Wants, recover, wants};

    /// /proc/sys/sense1/brightness, 0..65535 (as the app sets it).
    const NIGHT_BRIGHTNESS: u32 = 9000;

    /// Runs a command line through sh, its arguments as $1...: a URL from
    /// the manifest is never parsed by the shell.
    fn sh(script: &str, args: &[&str]) {
        let _ = Command::new("sh").arg("-c").arg(script).arg("sh").args(args).status();
    }

    /// The real device: btplay, the backlight, and the speaker held.
    #[derive(Default)]
    struct Device {
        speaker: Option<Unmuted>,
    }

    impl Effects for Device {
        fn backlight(&mut self, night: bool) {
            let v = if night { NIGHT_BRIGHTNESS } else { 65535 };
            let _ = std::fs::write("/proc/sys/sense1/brightness", format!("{v}\n"));
        }
        fn start_player(&mut self, url: &str) {
            // The stock player's daemon, as the C engine started it.
            sh("btplay --start-daemon \"$1\" >/dev/null 2>&1 &", &[url]);
        }
        fn stop_player(&mut self) {
            sh("btplay --passthru=stop >/dev/null 2>&1", &[]);
        }
        fn volume(&mut self) -> Option<u32> {
            audio::volume()
        }
        fn set_volume(&mut self, v: u32) {
            if let Err(e) = audio::set_volume(v) {
                eprintln!("neochumby-classic-service: volume: {e}");
            }
        }
        fn take_speaker(&mut self, vol: u32) -> Take {
            match Unmuted::at(vol) {
                Ok(u) => {
                    self.speaker = Some(u);
                    Take::Taken
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Take::Asked,
                Err(e) => {
                    eprintln!("neochumby-classic-service: speaker: {e}");
                    Take::Failed
                }
            }
        }
        fn release_speaker(&mut self) {
            self.speaker = None;
        }
        fn yield_requested(&mut self) -> bool {
            self.speaker.as_ref().is_some_and(Unmuted::yield_requested)
        }
        fn speaker_free(&mut self) -> bool {
            audio::free()
        }
        fn withdraw(&mut self) {
            audio::withdraw();
        }
        fn speaker_back(&mut self, volume_before: Option<u32>) {
            let _ = audio::mute(true);
            if let Some(v) = volume_before {
                let _ = audio::set_volume(v);
            }
        }
    }

    pub fn run() -> i32 {
        neochumby::debug::install("neochumby-classic-service");
        // SIGTERM only sets this: the loop ends, and the service tidies up.
        let term = neochumby_sys::signal::catch_term();
        let mut fx = Device::default();
        let run = crate::state::run_dir();
        if run.as_deref().is_some_and(|r| recover(r, &mut fx)) {
            eprintln!("neochumby-classic-service: stopped the stream a killed run left playing");
        }
        let base = crate::picture_dir(None);
        let st = Path::new("/psp");
        let mut pipe = Pipe::from_env();
        let mut feed = Feed::default();
        let mut night: Option<bool> = None;
        let mut player = Player::new(run, &mut fx);
        let t0 = std::time::Instant::now();
        while !term.load(Ordering::Relaxed) {
            if let Some(p) = &mut pipe {
                let r = p.receive();
                for l in &r.lines {
                    if let Some(Notice::Front(app)) = Notice::parse(l) {
                        eprintln!("neochumby-classic-service: {app} in front");
                    }
                }
                if r.closed {
                    break; // the session has gone
                }
            }
            feed.refresh(&base);
            let Wants { night: n, music } = wants(st, &feed, crate::plat::wall_now());
            if night != Some(n) {
                fx.backlight(n);
                night = Some(n);
            }
            player.step(&mut fx, music, t0.elapsed().as_millis() as i64);
            neochumby::sleep_ms(POLL_MS);
        }
        player.shutdown(&mut fx);
        neochumby::debug::dump();
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device that records what is done to it; the speaker's answers are
    /// set by the test.
    #[derive(Default)]
    struct Log {
        done: Vec<String>,
        /// What take_speaker answers (None: Taken).
        take: Option<Take>,
        free: bool,
        yield_asked: bool,
    }

    impl Effects for Log {
        fn backlight(&mut self, night: bool) {
            self.done.push(format!("backlight {}", if night { "night" } else { "day" }));
        }
        fn start_player(&mut self, url: &str) {
            self.done.push(format!("play {url}"));
        }
        fn stop_player(&mut self) {
            self.done.push("stop".into());
        }
        fn volume(&mut self) -> Option<u32> {
            self.done.push("volume?".into());
            Some(50)
        }
        fn set_volume(&mut self, v: u32) {
            self.done.push(format!("volume {v}"));
        }
        fn take_speaker(&mut self, vol: u32) -> Take {
            self.done.push(format!("take {vol}"));
            self.take.unwrap_or(Take::Taken)
        }
        fn release_speaker(&mut self) {
            self.done.push("release".into());
            self.yield_asked = false;
        }
        fn yield_requested(&mut self) -> bool {
            self.yield_asked
        }
        fn speaker_free(&mut self) -> bool {
            self.free
        }
        fn withdraw(&mut self) {
            self.done.push("withdraw".into());
        }
        fn speaker_back(&mut self, v: Option<u32>) {
            self.done.push(format!("speaker back {v:?}"));
        }
    }

    impl Log {
        fn take(&mut self) -> Vec<String> {
            std::mem::take(&mut self.done)
        }
    }

    const A: &str = "http://example.org/a";
    const B: &str = "http://example.org/b";

    fn want(url: &str, vol: u32) -> Option<(String, u32)> {
        Some((url.to_string(), vol))
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("classic-svc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn music_plays_changes_and_stops() {
        let run = dir("music");
        let mut fx = Log { free: true, ..Log::default() };
        let mut p = Player::new(Some(run.clone()), &mut fx);
        p.step(&mut fx, want(A, 30), 0);
        assert_eq!(fx.take(), ["volume?", "take 30", "play http://example.org/a"]);
        p.step(&mut fx, want(A, 30), 0);
        assert!(fx.take().is_empty(), "nothing to do");
        p.step(&mut fx, want(A, 40), 0);
        assert_eq!(fx.take(), ["volume 40"]);
        p.step(&mut fx, want(B, 40), 0);
        assert_eq!(fx.take(), ["stop", "release", "volume?", "take 40", "play http://example.org/b"]);
        assert!(state::read_nofollow(&run.join(MARK), 64).is_some(), "a record while it plays");
        p.step(&mut fx, None, 0);
        assert_eq!(fx.take(), ["stop", "release"]);
        assert_eq!(p.music, Music::Off);
        assert!(!recover(&run, &mut Log::default()), "no record once stopped");
        std::fs::remove_dir_all(&run).unwrap();
    }

    #[test]
    fn music_yields_and_resumes_only_when_free() {
        let mut fx = Log { free: true, ..Log::default() };
        let mut p = Player::new(None, &mut fx);
        p.step(&mut fx, want(A, 30), 0);
        fx.take();
        // A game asks for the speaker: stop and give it up.
        fx.yield_asked = true;
        fx.free = false;
        p.step(&mut fx, want(A, 30), 0);
        assert_eq!(fx.take(), ["stop", "release"]);
        assert_eq!(p.music, Music::Yielded);
        // While the game has it, nothing, even for a new station.
        p.step(&mut fx, want(B, 30), 0);
        assert!(fx.take().is_empty());
        // Free again: the new station plays.
        fx.free = true;
        p.step(&mut fx, want(B, 30), 0);
        assert_eq!(fx.take(), ["volume?", "take 30", "play http://example.org/b"]);
    }

    #[test]
    fn a_yield_comes_before_a_station_change() {
        let mut fx = Log { free: true, ..Log::default() };
        let mut p = Player::new(None, &mut fx);
        p.step(&mut fx, want(A, 30), 0);
        fx.take();
        fx.yield_asked = true;
        fx.free = false;
        p.step(&mut fx, want(B, 30), 0);
        assert_eq!(fx.take(), ["stop", "release"], "no take: that would ask the game to yield back");
        assert_eq!(p.music, Music::Yielded);
    }

    #[test]
    fn asking_for_the_speaker_waits_backs_off_and_withdraws() {
        let mut fx = Log { free: true, take: Some(Take::Asked), ..Log::default() };
        let mut p = Player::new(None, &mut fx);
        p.step(&mut fx, want(A, 30), 0);
        assert_eq!(fx.take(), ["volume?", "take 30"]);
        // Still asked: tried again at once, without reading the volume again.
        p.step(&mut fx, want(A, 30), 500);
        assert_eq!(fx.take(), ["take 30"]);
        // Another failure backs off.
        fx.take = Some(Take::Failed);
        p.step(&mut fx, want(A, 30), 1000);
        assert_eq!(fx.take(), ["take 30"]);
        p.step(&mut fx, want(A, 30), 1500);
        assert!(fx.take().is_empty(), "backing off");
        fx.take = Some(Take::Taken);
        p.step(&mut fx, want(A, 30), 1000 + RETRY_MS);
        assert_eq!(fx.take(), ["take 30", "play http://example.org/a"]);
        // No longer wanted while asking: the request is withdrawn.
        let mut fx = Log { free: true, take: Some(Take::Asked), ..Log::default() };
        let mut p = Player::new(None, &mut fx);
        p.step(&mut fx, want(A, 30), 0);
        fx.take();
        p.step(&mut fx, None, 0);
        assert_eq!(fx.take(), ["withdraw"]);
        // Started while another player has the speaker: wait for it.
        let mut fx = Log::default();
        let mut p = Player::new(None, &mut fx);
        p.step(&mut fx, want(A, 30), 0);
        assert!(fx.take().is_empty());
        assert_eq!(p.music, Music::Yielded);
    }

    #[test]
    fn sigterm_sets_the_day_backlight_first() {
        let run = dir("term");
        let mut fx = Log { free: true, ..Log::default() };
        let mut p = Player::new(Some(run.clone()), &mut fx);
        p.step(&mut fx, want(A, 30), 0);
        fx.take();
        p.shutdown(&mut fx);
        assert_eq!(fx.take(), ["backlight day", "stop", "release"]);
        assert!(!recover(&run, &mut Log::default()), "no record left");
        // And while waiting for the speaker: the day backlight, then withdraw.
        let mut fx = Log { free: true, take: Some(Take::Asked), ..Log::default() };
        let mut p = Player::new(None, &mut fx);
        p.step(&mut fx, want(A, 30), 0);
        fx.take();
        p.shutdown(&mut fx);
        assert_eq!(fx.take(), ["backlight day", "withdraw"]);
        std::fs::remove_dir_all(&run).unwrap();
    }

    #[test]
    fn a_killed_run_is_cleaned_up() {
        let run = dir("run");
        let mut fx = Log::default();
        assert!(!recover(&run, &mut fx), "nothing was playing");
        assert!(fx.take().is_empty());
        // Killed while playing, and the speaker is free: stop the stream,
        // and put the speaker back at the volume from before.
        mark_playing(&run, Some(40));
        fx.free = true;
        assert!(recover(&run, &mut fx));
        assert_eq!(fx.take(), ["stop", "speaker back Some(40)"]);
        assert!(!recover(&run, &mut fx), "only once");
        // Another player has the speaker now: only the stale stream stops.
        mark_playing(&run, None);
        fx.free = false;
        assert!(recover(&run, &mut fx));
        assert_eq!(fx.take(), ["stop"]);
        std::fs::remove_dir_all(&run).unwrap();
    }

    #[test]
    fn decides_night_and_music_from_the_files() {
        let pics = dir("pics");
        std::fs::write(
            pics.join("manifest.txt"),
            "sun 100 200\nstation 0 a http://example.org/a\nstation 1 b http://example.org/b\n",
        )
        .unwrap();
        let st = dir("psp");
        let mut feed = Feed::default();
        feed.refresh(&pics);
        assert_eq!(feed.sun, vec![(100, 200)]);
        // Nothing set: auto, by the sun; no music.
        assert_eq!(wants(&st, &feed, 150), Wants { night: false, music: None });
        assert!(wants(&st, &feed, 250).night);
        // Always on; a NIGHT tap's override wins until its end.
        state::save_int(&st.join("wx_nightmode"), 1);
        assert!(wants(&st, &feed, 150).night);
        state::save_int(&st.join("wx_night"), 0);
        state::save_time(&st.join("wx_night_until"), 180);
        assert!(!wants(&st, &feed, 150).night);
        assert!(wants(&st, &feed, 180).night);
        // Music: station 1 at the saved volume; a bad volume is 65; a
        // station without a URL plays nothing.
        state::save_int(&st.join("wx_music"), 1);
        state::save_int(&st.join("wx_station"), 1);
        state::save_int(&st.join("wx_volume"), 30);
        assert_eq!(wants(&st, &feed, 150).music, Some((B.into(), 30)));
        state::save_int(&st.join("wx_volume"), 900);
        assert_eq!(wants(&st, &feed, 150).music, Some((B.into(), 65)));
        state::save_int(&st.join("wx_station"), 5);
        assert_eq!(wants(&st, &feed, 150).music, None);
        // A new manifest is read again, even within the same second (its
        // size differs).
        std::fs::write(pics.join("manifest.txt"), "sun 300 400\n").unwrap();
        feed.refresh(&pics);
        assert_eq!(feed.sun, vec![(300, 400)]);
        std::fs::remove_dir_all(&pics).unwrap();
        std::fs::remove_dir_all(&st).unwrap();
    }
}
