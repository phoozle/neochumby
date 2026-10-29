//! AirPlay's companion: its server half, which neochumbyd runs for each
//! Chumby that opens it (the Chumby's half is main.rs and screen.rs). An
//! AirPlay receiver named after the Chumby ("kitchen_chumby" plays as
//! "Kitchen Chumby"), shairport-sync writing raw PCM (S16LE, stereo,
//! 44.1 kHz) to its stdout, which goes to the Chumby in frames
//! (neochumby_airplay::frames): the audio, and when it stops; the
//! receiver's name; and, from shairport-sync's metadata pipe, the track,
//! its cover art (scaled here for the screen) and where it has got to
//! (track.rs). The Chumby's play/pause, next and previous come back on
//! stdin and go to the sender as DACP requests (dacp.rs).
//!
//! shairport-sync announces itself through the host's avahi. From the
//! environment: NEOCHUMBY_CHUMBY and NEOCHUMBY_STATE (neochumbyd's), and
//! NEOCHUMBY_AIRPLAY_PORT, the base of the host's AirPlay ports (7350):
//! RTSP on base + slot, ten UDP ports from base + 10 + 10 * slot, for
//! slots 0 to 7. A Chumby's slot comes from its name, so its receiver
//! keeps its ports from one run to the next and phones that cached them
//! still find it; slots are claimed with lock files in the state
//! directory. shairport-sync is found on PATH, or NEOCHUMBY_SHAIRPORT.
//! Its metadata pipe is <name>.metadata in the state directory; one built
//! without metadata support still plays, without the track's details.
//! Exits when its stdin closes: the Chumby has gone.

mod art;
mod dacp;
mod meta;
mod track;

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::time::{Duration, Instant};

use neochumby_airplay::frames::{AUDIO, Control, END, Frame};
use neochumby_proto::{frame, valid_name};
use neochumby_sys::process::{Signal, kill};

use crate::track::Tracker;

const SLOTS: usize = 8;
/// How long without audio before the Chumby is told it has stopped.
const QUIET: Duration = Duration::from_secs(1);
/// How long shairport-sync has to make its metadata pipe.
const PIPE_WAIT: Duration = Duration::from_secs(10);

/// shairport-sync, asked to stop when dropped (so it withdraws its
/// announcement) and killed if it has not within two seconds.
struct Receiver(Child);

impl Drop for Receiver {
    fn drop(&mut self) {
        let _ = kill(self.0.id(), Signal::Term);
        let deadline = Instant::now() + Duration::from_secs(2);
        while matches!(self.0.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// What the companion's loop waits on, from its threads.
enum Event {
    /// Audio from shairport-sync.
    Pcm(Vec<u8>),
    /// shairport-sync's stdout closed: it has exited.
    Exited,
    /// An item from the metadata pipe.
    Meta(meta::Item),
    /// A button on the Chumby.
    Control(Control),
    /// The DACP port a command found for the sender with this DACP-ID.
    Port(String, u16),
    /// The Chumby has gone (neochumbyd closed stdin).
    Gone,
}

/// "kitchen_chumby" → "Kitchen Chumby".
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

/// Where `name`'s slot search starts: FNV-1a of the name.
fn home_slot(name: &str) -> usize {
    let h = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3));
    (h % SLOTS as u64) as usize
}

/// A slot, held while the file stays open.
fn try_slot(dir: &Path, slot: usize) -> Option<File> {
    let f = File::create(dir.join(format!("slot-{slot}.lock"))).ok()?;
    f.try_lock().is_ok().then_some(f)
}

/// The Chumby's own slot, waiting a while for it (its last receiver may
/// still be stopping), else the next free one.
fn claim_slot(dir: &Path, name: &str) -> io::Result<(usize, File)> {
    let home = home_slot(name);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(f) = try_slot(dir, home) {
            return Ok((home, f));
        }
        if Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    (1..SLOTS)
        .map(|i| (home + i) % SLOTS)
        .find_map(|s| try_slot(dir, s).map(|f| (s, f)))
        .ok_or_else(|| io::Error::other("no AirPlay slot free"))
}

/// `s` as a libconfig string, quoted.
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// shairport-sync's configuration for this Chumby's receiver.
fn config(name: &str, port: usize, udp_base: usize, pipe: &Path) -> String {
    format!(
        "general = {{\n  name = {};\n  port = {port};\n  udp_port_base = {udp_base};\n  udp_port_range = 10;\n  \
         output_backend = \"stdout\";\n  mdns_backend = \"avahi\";\n}};\n\
         sessioncontrol = {{\n  session_timeout = 60;\n}};\n\
         metadata = {{\n  enabled = \"yes\";\n  include_cover_art = \"yes\";\n  \
         cover_art_cache_directory = \"\";\n  pipe_name = {};\n  pipe_timeout = 5000;\n}};\n",
        quoted(&display_name(name)),
        quoted(&pipe.to_string_lossy()),
    )
}

/// Reads shairport-sync's metadata pipe for as long as the companion runs.
/// shairport-sync makes the FIFO as it starts, and writes to it only while
/// it has a reader; it's opened for writing too, so it never reads as
/// ended between streams.
fn read_metadata(pipe: PathBuf, tx: SyncSender<Event>) {
    let deadline = Instant::now() + PIPE_WAIT;
    let mut f = loop {
        if std::fs::metadata(&pipe).is_ok_and(|m| m.file_type().is_fifo()) {
            match OpenOptions::new().read(true).write(true).open(&pipe) {
                Ok(f) => break f,
                Err(e) => {
                    eprintln!("neochumby-airplay companion: {}: {e}", pipe.display());
                    return;
                }
            }
        }
        if Instant::now() > deadline {
            eprintln!(
                "neochumby-airplay companion: shairport-sync made no metadata pipe (built without metadata?): \
                 playing without the track's details or remote control"
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut parser = meta::Parser::default();
    let mut buf = vec![0u8; 65536];
    while let Ok(n @ 1..) = f.read(&mut buf) {
        for item in parser.push(&buf[..n]) {
            if tx.send(Event::Meta(item)).is_err() {
                return;
            }
        }
    }
}

/// Sends a button's command to the sender, on a thread of its own: a
/// lookup and a request can take a couple of seconds, and the audio
/// mustn't wait.
fn control(remote: &dacp::Remote, c: Control, tx: SyncSender<Event>) {
    if !remote.ready() {
        eprintln!("neochumby-airplay companion: {}: this sender takes no remote control", dacp::command(c));
        return;
    }
    let remote = remote.clone();
    std::thread::spawn(move || match dacp::send(&remote, c) {
        Ok(port) => {
            if remote.port != Some(port) {
                let _ = tx.send(Event::Port(remote.id.clone(), port));
            }
        }
        Err(e) => eprintln!("neochumby-airplay companion: {}: {e}", dacp::command(c)),
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
    let dir = PathBuf::from(std::env::var_os("NEOCHUMBY_STATE").ok_or_else(|| io::Error::other("no NEOCHUMBY_STATE"))?);
    let base: usize = std::env::var("NEOCHUMBY_AIRPLAY_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(7350);
    let shairport = std::env::var_os("NEOCHUMBY_SHAIRPORT").unwrap_or_else(|| "shairport-sync".into());

    let (slot, _lock) = claim_slot(&dir, &name)?;
    let conf = dir.join(format!("{name}.conf"));
    let pipe = dir.join(format!("{name}.metadata"));
    // A pipe or file left by an earlier run: shairport-sync makes it anew.
    let _ = std::fs::remove_file(&pipe);
    std::fs::write(&conf, config(&name, base + slot, base + 10 + 10 * slot, &pipe))?;
    let mut child = Command::new(shairport).arg("-c").arg(&conf).stdin(Stdio::null()).stdout(Stdio::piped()).spawn()?;
    let mut pcm_out = child.stdout.take().expect("piped");
    let _receiver = Receiver(child);
    eprintln!("neochumby-airplay: receiver \"{}\" for {name} on port {}", display_name(&name), base + slot);

    let (tx, rx) = mpsc::sync_channel::<Event>(64);
    let pcm_tx = tx.clone();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 16384];
        while let Ok(n @ 1..) = pcm_out.read(&mut buf) {
            if pcm_tx.send(Event::Pcm(buf[..n].to_vec())).is_err() {
                return;
            }
        }
        let _ = pcm_tx.send(Event::Exited);
    });
    let meta_tx = tx.clone();
    std::thread::spawn(move || read_metadata(pipe, meta_tx));
    // The Chumby's buttons; its going (neochumbyd closes our stdin) ends
    // the run.
    let in_tx = tx.clone();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        while let Ok(Some(f)) = frame::read(&mut stdin) {
            if let Some(Frame::Control(c)) = Frame::parse(&f) {
                let _ = in_tx.send(Event::Control(c));
            }
        }
        let _ = in_tx.send(Event::Gone);
    });

    let mut out = io::stdout().lock();
    frame::write(&mut out, &Frame::Name(display_name(&name)).encode())?;
    let mut tracker = Tracker::new(Instant::now());
    let mut playing = false;
    let mut last_pcm = Instant::now();
    loop {
        let event = rx.recv_timeout(Duration::from_millis(250));
        let now = Instant::now();
        match event {
            Ok(Event::Pcm(pcm)) => {
                for chunk in pcm.chunks(frame::MAX_FRAME - 1) {
                    let mut f = Vec::with_capacity(1 + chunk.len());
                    f.push(AUDIO);
                    f.extend_from_slice(chunk);
                    frame::write(&mut out, &f)?;
                }
                last_pcm = now;
                if !playing {
                    playing = true;
                    send(&mut out, tracker.audio(true, now))?;
                }
            }
            Ok(Event::Meta(item)) => send(&mut out, tracker.item(&item, now))?,
            Ok(Event::Control(c)) => control(&tracker.remote, c, tx.clone()),
            Ok(Event::Port(id, port)) => tracker.found_port(&id, port),
            Ok(Event::Gone) => return Ok(()),
            Ok(Event::Exited) | Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("shairport-sync exited"));
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if playing && now.saturating_duration_since(last_pcm) >= QUIET {
            frame::write(&mut out, &[END])?;
            playing = false;
            send(&mut out, tracker.audio(false, now))?;
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("neochumby-airplay companion: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn names_and_slots() {
        assert_eq!(super::display_name("kitchen_chumby"), "Kitchen Chumby");
        assert_eq!(super::display_name("den-2"), "Den 2");
        assert!(super::home_slot("kitchen_chumby") < super::SLOTS);
    }

    #[test]
    fn the_config_turns_metadata_on() {
        let c = super::config("den", 7351, 7370, Path::new("/var/lib/n\"d/den.metadata"));
        assert!(c.contains("name = \"Den\";"), "{c}");
        assert!(c.contains("port = 7351;"), "{c}");
        assert!(c.contains("metadata = {\n  enabled = \"yes\";\n  include_cover_art = \"yes\";"), "{c}");
        assert!(c.contains("pipe_name = \"/var/lib/n\\\"d/den.metadata\";"), "{c}");
    }
}
