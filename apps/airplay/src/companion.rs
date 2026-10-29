//! AirPlay's companion: its server half, which neochumbyd runs for each
//! Chumby that opens it (the Chumby's half is main.rs). An AirPlay receiver
//! named after the Chumby ("kitchen_chumby" plays as "Kitchen Chumby"),
//! shairport-sync writing raw PCM (S16LE, stereo, 44.1 kHz) to its stdout,
//! which goes to the Chumby in frames:
//!
//!   [0] + PCM    audio, as it comes
//!   [1]          the audio has stopped for now: the speaker can go
//!   [2] + bytes  shairport-sync's metadata pipe (<name>.metadata in the
//!                state directory), as it comes, unparsed: for a
//!                now-playing app on the Chumby (the service hands it on)
//!
//! shairport-sync announces itself through the host's avahi. From the
//! environment: NEOCHUMBY_CHUMBY and NEOCHUMBY_STATE (neochumbyd's), and
//! NEOCHUMBY_AIRPLAY_PORT, the base of the host's AirPlay ports (7350):
//! RTSP on base + slot, ten UDP ports from base + 10 + 10 * slot, for
//! slots 0 to 7. A Chumby's slot comes from its name, so its receiver
//! keeps its ports from one run to the next and phones that cached them
//! still find it; slots are claimed with lock files in the state
//! directory. shairport-sync is found on PATH, or NEOCHUMBY_SHAIRPORT.
//! Exits when its stdin closes: the Chumby has gone.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use neochumby_proto::{frame, valid_name};
use neochumby_sys::process::{Signal, kill};

const SLOTS: usize = 8;
const META: u8 = 2;
const AUDIO: u8 = 0;
const END: u8 = 1;

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
    // shairport-sync makes the pipe anew.
    let _ = std::fs::remove_file(&pipe);
    std::fs::write(
        &conf,
        format!(
            "general = {{\n  name = \"{}\";\n  port = {};\n  udp_port_base = {};\n  udp_port_range = 10;\n  \
             output_backend = \"stdout\";\n  mdns_backend = \"avahi\";\n}};\n\
             sessioncontrol = {{\n  session_timeout = 60;\n}};\n\
             metadata = {{\n  enabled = \"yes\";\n  include_cover_art = \"yes\";\n  pipe_name = \"{}\";\n}};\n",
            display_name(&name),
            base + slot,
            base + 10 + 10 * slot,
            pipe.display(),
        ),
    )?;
    let mut child = Command::new(shairport).arg("-c").arg(&conf).stdin(Stdio::null()).stdout(Stdio::piped()).spawn()?;
    let mut pcm_out = child.stdout.take().expect("piped");
    let _receiver = Receiver(child);
    eprintln!("neochumby-airplay: receiver \"{}\" for {name} on port {}", display_name(&name), base + slot);

    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(16);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 16384];
        while let Ok(n) = pcm_out.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    // The metadata, once shairport-sync has made its pipe (one built
    // without metadata never does). Opened for writing too, so it doesn't
    // read as ended between streams.
    std::thread::spawn(move || {
        for _ in 0..100 {
            if pipe.exists() {
                let Ok(mut f) = OpenOptions::new().read(true).write(true).open(&pipe) else { return };
                let mut buf = vec![0u8; 16384];
                while let Ok(n @ 1..) = f.read(&mut buf) {
                    if frame::write(&mut io::stdout(), &[&[META], &buf[..n]].concat()).is_err() {
                        return;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    // The Chumby going (neochumbyd closes our stdin) ends the run.
    let (gone_tx, gone) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        while let Ok(Some(_)) = frame::read(&mut stdin) {}
        let _ = gone_tx.send(());
    });

    // Not locked for good: the metadata's thread writes whole frames too.
    let mut out = io::stdout();
    let mut playing = false;
    loop {
        if gone.try_recv().is_ok() {
            return Ok(());
        }
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(pcm) => {
                for chunk in pcm.chunks(frame::MAX_FRAME - 1) {
                    let mut f = Vec::with_capacity(1 + chunk.len());
                    f.push(AUDIO);
                    f.extend_from_slice(chunk);
                    frame::write(&mut out, &f)?;
                }
                playing = true;
            }
            Err(RecvTimeoutError::Timeout) => {
                if playing {
                    frame::write(&mut out, &[END])?;
                    playing = false;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Err(io::Error::other("shairport-sync exited")),
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
    #[test]
    fn names_and_slots() {
        assert_eq!(super::display_name("kitchen_chumby"), "Kitchen Chumby");
        assert_eq!(super::display_name("den-2"), "Den 2");
        assert!(super::home_slot("kitchen_chumby") < super::SLOTS);
    }
}
