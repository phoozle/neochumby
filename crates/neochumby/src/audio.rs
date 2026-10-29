//! The Chumby Classic's speaker: /dev/dsp (OSS), the amplifier's mute
//! (/proc/sys/sense1/spkmute; the boot leaves it on) and the stock
//! firmware's master volume (0-100, through /usr/bin/chumby_set_volume).
//!
//! [`Unmuted`] is the volume and the mute alone, for a process that has
//! another program play (the stock btplay, which opens /dev/dsp itself);
//! [`Speaker`] is that plus /dev/dsp, for one that writes PCM itself;
//! [`Stream`] is a speaker for a game, which mixes as it goes and must not
//! wait on the speaker, or stop when it is someone else's.
//!
//! # The handover
//!
//! /dev/dsp can be open in only one process at a time, so the newest player
//! takes the speaker: whoever holds it is asked to yield, stops its player
//! and drops its guard, and may start again once the speaker is free.
//! Processes agree on this through two files in /tmp/neochumby:
//! - `speaker`: the holder's pid. Creating an [`Unmuted`] or a [`Speaker`]
//!   writes it, failing if another live process's pid is there; dropping
//!   the guard removes it. A dead process's claim is ignored and replaced.
//! - `speaker.yield`: the pid of the process asking for the speaker. A
//!   newcomer that finds the speaker held writes it, and its guard's
//!   creation fails with [`io::ErrorKind::WouldBlock`]; it tries again
//!   soon, which renews the request, and taking the speaker removes it. A
//!   request counts while its process is alive and for five seconds after
//!   it was last written. While one is pending, a second newcomer waits
//!   (also `WouldBlock`) and asks the first once that one holds the speaker.
//!
//! The holder polls its guard's `yield_requested` as it plays and, when it
//! is true, stops its player (and waits for it to exit, if it has another
//! program play) before dropping the guard. A process that yielded doesn't
//! ask for the speaker back: it waits until [`free`] says no one else holds
//! it or has asked for it. A newcomer that gives up calls [`withdraw`].
//!
//! A [`Speaker`] is written as an [`io::Write`] of interleaved 16-bit
//! little-endian PCM, rather than through cpal, the crate most Rust audio
//! (rodio, kira, Bevy's audio) is built on. cpal isn't an option yet:
//! - On Linux it depends on the `alsa` crate unconditionally, even for a
//!   custom host, and that links libasound, which the Chumby's static musl
//!   binaries don't have.
//! - It has no OSS host. One could be written with its `custom` feature,
//!   but its own hosts run streams on threads synchronised with std's
//!   `Mutex`, whose futex flags the stock 2.6.16 kernel lacks, so it would
//!   need a futex-free design.
//! - rodio, the usual way to play through it, mixes in floating point,
//!   which the FPU-less ARM926 can only emulate.
//!
//! If cpal makes `alsa` optional, an OSS custom host would let those crates
//! play here.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::process::Command;
use std::time::{Duration, Instant};

use neochumby_sys::oss::{self, AFMT_S16_LE, Setting};
use neochumby_sys::speaker::{Handover, Take};

use crate::debug;
use crate::session::{Pipe, Request};

const DSP: &str = "/dev/dsp";
const SPEAKER_MUTE: &str = "/proc/sys/sense1/spkmute";
/// With no argument it prints the master volume; with --master_volume N it
/// sets it.
const SET_VOLUME: &str = "/usr/bin/chumby_set_volume";

/// Mutes or unmutes the speaker amplifier. Whatever plays unmutes it while
/// it does, and mutes it again after.
pub fn mute(mute: bool) -> io::Result<()> {
    std::fs::write(SPEAKER_MUTE, if mute { "1" } else { "0" })
}

/// The master volume (0-100), or None if it can't be read.
pub fn volume() -> Option<u32> {
    let out = Command::new(SET_VOLUME).output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Sets the master volume (0-100).
pub fn set_volume(v: u32) -> io::Result<()> {
    Command::new(SET_VOLUME).arg("--master_volume").arg(v.to_string()).output().map(|_| ())
}

/// Whether the speaker is free for this process: no other live process
/// holds it or has asked for it. A process that yielded the speaker waits
/// for this before taking it again.
pub fn free() -> bool {
    Handover::new().free()
}

/// The live process holding the speaker, if another one does.
pub fn holder() -> Option<u32> {
    Handover::new().holder()
}

/// Withdraws this process's request for the speaker, if it has one: for a
/// newcomer that no longer wants it.
pub fn withdraw() {
    Handover::new().withdraw();
}

/// The speaker unmuted and at a volume while held; dropping it mutes the
/// speaker and puts the volume back. Only a drop restores them, so a process
/// killed while holding it leaves the speaker unmuted and at its volume.
pub struct Unmuted {
    // Dropped first: the volume is put back before the speaker is given up.
    _volume: Volume,
    claim: Claim,
}

impl Unmuted {
    /// Takes the speaker, then sets the master volume to `volume` and
    /// unmutes, remembering the volume to put back. If another process
    /// holds the speaker, asks it to yield and fails with
    /// [`io::ErrorKind::WouldBlock`], changing nothing: try again soon.
    pub fn at(volume: u32) -> io::Result<Unmuted> {
        let claim = Claim::take()?;
        Ok(Unmuted { _volume: Volume::at(volume), claim })
    }

    /// Whether another process has asked for the speaker: if so, stop
    /// playing and drop this.
    pub fn yield_requested(&self) -> bool {
        self.claim.yield_requested()
    }
}

/// The speaker, held: /dev/dsp set for 16-bit little-endian PCM, and the
/// speaker unmuted and at a volume while open, as [`Unmuted`]. Dropping it
/// restores the speaker, closes /dev/dsp, then gives the speaker up.
pub struct Speaker {
    // In this order: the speaker is muted before /dev/dsp closes, and
    // /dev/dsp is closed before the next player is let take it.
    _volume: Volume,
    dsp: File,
    claim: Claim,
    channels: u32,
    rate: u32,
}

impl Speaker {
    /// Takes the speaker, opens /dev/dsp for `channels` channels at `rate`
    /// Hz, then sets the master volume to `volume` and unmutes. If another
    /// process holds the speaker, asks it to yield and fails with
    /// [`io::ErrorKind::WouldBlock`]: try again soon. Fails, changing
    /// nothing, if /dev/dsp is busy (with a program outside the handover)
    /// or refuses the format.
    pub fn open(channels: u32, rate: u32, volume: u32) -> io::Result<Speaker> {
        let claim = Claim::take()?;
        let dsp = OpenOptions::new().write(true).open(DSP)?;
        oss::set(&dsp, Setting::Format, AFMT_S16_LE)?;
        let channels = oss::set(&dsp, Setting::Channels, channels as i32)? as u32;
        let rate = oss::set(&dsp, Setting::Rate, rate as i32)? as u32;
        Ok(Speaker { _volume: Volume::at(volume), dsp, claim, channels, rate })
    }

    /// Whether another process has asked for the speaker: if so, stop
    /// writing and drop this.
    pub fn yield_requested(&self) -> bool {
        self.claim.yield_requested()
    }

    /// The channels the driver took: the ones asked for, or near them.
    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// The rate the driver took, in Hz: the one asked for, or near it.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// The driver's buffer: how much is waiting to play, and how much more
    /// can be written without blocking.
    pub fn space(&self) -> io::Result<oss::Space> {
        oss::space(&self.dsp)
    }
}

/// Having asked for the speaker, or given it up: when to look again. Soon,
/// as a holder that was asked only has to stop its player.
const ASKED_RETRY: Duration = Duration::from_millis(250);
/// /dev/dsp busy with a program outside the handover, or missing: when to
/// try again.
const BUSY_RETRY: Duration = Duration::from_secs(2);

/// A game's sound: 16-bit PCM written as it is mixed, a little ahead of
/// what plays, and played on the speaker whenever this process can have it.
///
/// The speaker is taken when the stream is made and yielded to a newer
/// player (as AirPlay's are, see the handover above); while it is someone
/// else's, or /dev/dsp can't be opened, what is written is dropped and the
/// clock stands in for the speaker, so the game goes on as if it played.
/// Writes never block: what /dev/dsp has no room for waits here for the next
/// write or [`Stream::played`]. The speaker plays at least [`DEVICE_RATE`],
/// and the stream is resampled to whatever rate the driver takes.
///
/// The Chumby's driver plays nothing of a fragment (8 KiB, whatever is
/// asked) until it is full. So the bytes in the last, part-filled one count
/// as played: the game mixes on past them, and the fragment fills. At
/// DEVICE_RATE a fragment is under 50 ms of sound, which is what that costs.
pub struct Stream {
    channels: u32,
    rate: u32,
    volume: u32,
    speaker: Option<Speaker>,
    /// Frames written since the stream began, played or dropped.
    written: u64,
    /// What /dev/dsp had no room for yet, in its format.
    pending: Vec<u8>,
    /// Where the speaker can't say what it has played: the frames played at
    /// an instant, from which the clock counts.
    clock: Option<(Instant, u64)>,
    resample: Resample,
    retry: Option<Instant>,
    /// The speaker went to a newer player: it is taken back only once free.
    yielded: bool,
    /// Bytes written to /dev/dsp since it was opened: where in a fragment
    /// the next byte falls.
    sent: u64,
    /// What [`Stream::played`] last said.
    played: u64,
}

/// The least rate the speaker plays a [`Stream`] at, in Hz.
const DEVICE_RATE: u32 = 44100;

impl Stream {
    /// A stream of `channels` channels at `rate` Hz, played at the master
    /// volume `volume` (0-100) while it has the speaker.
    pub fn new(channels: u32, rate: u32, volume: u32) -> Stream {
        let mut s = Stream {
            channels,
            rate,
            volume,
            speaker: None,
            written: 0,
            pending: Vec::new(),
            clock: None,
            resample: Resample::new(channels as usize),
            retry: None,
            yielded: false,
            sent: 0,
            played: 0,
        };
        s.poll();
        s
    }

    /// Frames written so far.
    pub fn written(&self) -> u64 {
        self.written
    }

    /// Frames played so far, never more than [`Stream::written`]: a stream
    /// that has run dry plays the next frame written at once.
    pub fn played(&mut self) -> u64 {
        let p = self.measure();
        // Never back: a fragment that fills takes what it held off the
        // count, and an engine that reads a position going back (Quake's)
        // takes it for a pass of its ring.
        self.played = self.played.max(p).min(self.written);
        self.played
    }

    fn measure(&mut self) -> u64 {
        self.poll();
        self.flush();
        let frame = 2 * self.channels as u64;
        let device = self.speaker.as_ref().map(|sp| (sp.rate() as u64, sp.space()));
        if let Some((dev_rate, Ok(space))) = device {
            self.clock = None;
            // What waits in a part-filled fragment won't play until it is
            // full: it counts as played, so more is mixed to fill it.
            let held = if space.fragment > 0 { (self.sent % space.fragment as u64) as usize } else { 0 };
            let waiting = (space.queued.saturating_sub(held) + self.pending.len()) as u64 / frame;
            return self.written.saturating_sub(waiting * self.rate as u64 / dev_rate);
        }
        let (since, from) = *self.clock.get_or_insert((Instant::now(), self.written));
        let played = from + since.elapsed().as_micros() as u64 * self.rate as u64 / 1_000_000;
        if played > self.written {
            self.clock = Some((Instant::now(), self.written));
        }
        played.min(self.written)
    }

    /// Writes interleaved frames: to the speaker if this process has it,
    /// otherwise nowhere.
    pub fn write(&mut self, pcm: &[i16]) {
        self.written += (pcm.len() / self.channels as usize) as u64;
        let Some(sp) = self.speaker.as_ref() else { return };
        if sp.rate() == self.rate {
            self.pending.extend(pcm.iter().flat_map(|s| s.to_le_bytes()));
        } else {
            let step = ((self.rate as u64) << 16) / sp.rate() as u64;
            self.resample.run(pcm, step as u32, &mut self.pending);
        }
        self.flush();
    }

    /// Writes what /dev/dsp has room for, or all of it if the driver can't
    /// say how much room it has.
    fn flush(&mut self) {
        let Some(sp) = self.speaker.as_mut() else {
            self.pending.clear();
            return;
        };
        let frame = 2 * self.channels as usize;
        let n = match sp.space() {
            Ok(space) => space.free.min(self.pending.len()) / frame * frame,
            Err(_) => self.pending.len(),
        };
        if n == 0 {
            return;
        }
        if let Err(e) = sp.write_all(&self.pending[..n]) {
            eprintln!("{}: {DSP}: {e}", debug::name());
            self.drop_speaker();
            self.retry = Some(Instant::now() + BUSY_RETRY);
            return;
        }
        self.pending.drain(..n);
        self.sent += n as u64;
    }

    /// Yields the speaker if a newer player has asked for it, and takes it
    /// (back) when it's time to try.
    fn poll(&mut self) {
        let now = Instant::now();
        if self.speaker.as_ref().is_some_and(Speaker::yield_requested) {
            eprintln!("{}: yielding the speaker to a newer player", debug::name());
            self.drop_speaker();
            self.yielded = true;
            self.retry = Some(now + ASKED_RETRY);
        }
        if self.speaker.is_some() || self.retry.is_some_and(|t| now < t) {
            return;
        }
        if self.yielded && !free() {
            self.retry = Some(now + ASKED_RETRY);
            return;
        }
        let first = self.retry.is_none();
        match Speaker::open(self.channels, self.rate.max(DEVICE_RATE), self.volume) {
            Ok(sp) if sp.channels() == self.channels => {
                self.speaker = Some(sp);
                self.sent = 0;
                self.resample = Resample::new(self.channels as usize);
                self.retry = None;
                self.yielded = false;
            }
            Ok(sp) => {
                if first {
                    eprintln!(
                        "{}: {DSP} took {} channels, not {}; silent",
                        debug::name(),
                        sp.channels(),
                        self.channels
                    );
                }
                self.retry = Some(now + BUSY_RETRY);
            }
            Err(e) => {
                if first {
                    eprintln!("{}: the speaker: {e}; silent until it is free", debug::name());
                }
                let wait = if e.kind() == io::ErrorKind::WouldBlock { ASKED_RETRY } else { BUSY_RETRY };
                self.retry = Some(now + wait);
            }
        }
    }

    fn drop_speaker(&mut self) {
        self.speaker = None;
        self.pending.clear();
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.drop_speaker();
        withdraw();
    }
}

/// Linear interpolation from one rate to another, in 16.16 fixed point (the
/// ARM926 has no FPU), carrying the last frame from one call to the next.
struct Resample {
    last: Vec<i16>,
    /// Where the next output frame is, from `last`, in input frames.
    pos: u32,
}

impl Resample {
    fn new(channels: usize) -> Resample {
        Resample { last: vec![0; channels], pos: 0 }
    }

    /// Appends `pcm` at the new rate to `out`, as little-endian bytes;
    /// `step` is input frames per output frame, in 16.16.
    fn run(&mut self, pcm: &[i16], step: u32, out: &mut Vec<u8>) {
        let ch = self.last.len();
        let frames = pcm.len() / ch;
        if frames == 0 {
            return;
        }
        // Frame 0 is `last`, frame i > 0 is pcm's i-1.
        let at = |i: usize, c: usize| if i == 0 { self.last[c] } else { pcm[(i - 1) * ch + c] } as i32;
        while ((self.pos >> 16) as usize) < frames {
            let i = (self.pos >> 16) as usize;
            let f = (self.pos & 0xffff) as i64;
            for c in 0..ch {
                let (a, b) = (at(i, c), at(i + 1, c));
                let s = a + (((b - a) as i64 * f) >> 16) as i32;
                out.extend((s as i16).to_le_bytes());
            }
            self.pos += step;
        }
        self.pos -= (frames as u32) << 16;
        self.last.copy_from_slice(&pcm[(frames - 1) * ch..frames * ch]);
    }
}

/// This process's claim on the speaker, given up when dropped.
struct Claim(Handover);

impl Claim {
    fn take() -> io::Result<Claim> {
        let h = Handover::new();
        match h.take()? {
            Take::Taken => Ok(Claim(h)),
            Take::Asked { holder } => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("the speaker is process {holder}'s; asked it to yield"),
            )),
            Take::Waiting { asker } => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("process {asker} has asked for the speaker first"),
            )),
        }
    }

    fn yield_requested(&self) -> bool {
        self.0.yield_requested()
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// The master volume set and the speaker unmuted while held; dropping it
/// mutes the speaker and puts the volume back.
///
/// Only root can mute or set the volume (the kernel checks the mute on
/// every write), so a jailed app asks the session to, over its request
/// pipe (`speaker <volume>`, `speaker off`); the session puts back the
/// volume the Chumby booted with.
enum Volume {
    Set { before: Option<u32> },
    Asked(Pipe),
    Unasked,
}

impl Volume {
    fn at(volume: u32) -> Volume {
        if !neochumby_sys::process::is_root() {
            let Some(mut pipe) = Pipe::from_env() else {
                report("the speaker", Err(io::Error::other("not root, and no session to ask")));
                return Volume::Unasked;
            };
            report("asking the session for the speaker", pipe.send(&Request::Speaker(Some(volume)).line()));
            return Volume::Asked(pipe);
        }
        let before = self::volume();
        report(SET_VOLUME, set_volume(volume));
        report(SPEAKER_MUTE, mute(false));
        Volume::Set { before }
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        match self {
            Volume::Set { before } => {
                report(SPEAKER_MUTE, mute(true));
                if let Some(v) = *before {
                    report(SET_VOLUME, set_volume(v));
                }
            }
            Volume::Asked(pipe) => {
                report("giving the speaker back to the session", pipe.send(&Request::Speaker(None).line()));
            }
            Volume::Unasked => {}
        }
    }
}

impl Write for Speaker {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.dsp.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.dsp.flush()
    }
}

// The volume and the mute are best effort: the audio plays either way.
fn report(what: &str, r: io::Result<()>) {
    if let Err(e) = r {
        eprintln!("{}: {what}: {e}", debug::name());
    }
}
