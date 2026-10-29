//! neochumby: what the Chumby boots to, run by the boot hook from the tree.
//! The session: it runs the apps' background services, restarting them
//! when they exit (services.rs), and runs one app at a time in front,
//! full-screen: first the start app (start.rs), then whatever the apps ask
//! for over their request pipe (neochumby::session), going back to the
//! start app whenever one exits (run.rs). On CHUMBY MODE it stops the
//! services and exits, and the boot carries on with the Chumby's own
//! software.
//!
//! On a paired Chumby, its output also goes to its neochumbyd, which can
//! ask it to restart, or for its screen, and, once its owner allows it,
//! tap its screen and press its top button (devlink.rs). A restart stops
//! everything as CHUMBY MODE does, then runs the tree's `neochumby` in
//! this process's place: the newest tree, deployed since boot.
//!
//! Holding the top-left corner of the screen for three seconds, whatever
//! is running, ends the app in front and whatever it started, and opens
//! neochumby-home: the way back from an app that has gone wrong. The top
//! button is the session's, to hand over or to return to the start app
//! (button.rs).
//!
//!   neochumby                 the session
//!   neochumby sensors         prints the touchscreen, squeeze and
//!                             accelerometer
//!   neochumby check-set DIR   for the boot hook: exits 0 if the synced set
//!                             in DIR is complete (neochumby_sync)
//!
//! With `delivery=sync` in its settings, the session also runs
//! neochumby-sync from the tree as a service of its own (restarted like
//! the apps' services), which keeps a copy of the tree on the stick for the
//! boot hook to run. A tree made for a stick has no neochumby-sync, and
//! needs none. The session also keeps the boot hook in
//! /psp up to date with the tree's (conf::refresh_hook).
//!
//! Apps live beside it, in apps/<name>/ (neochumby::apps); each is its own
//! binary, paged in over NFS only when run. What it runs is described in
//! child.rs.

mod button;
mod child;
mod companion;
mod devlink;
mod jail;
mod network;
mod run;
mod services;
mod start;

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use neochumby::apps::{self, App, Needs};
use neochumby::audio;
use neochumby::conf;
use neochumby::fb::{Fb, rgb};
use neochumby::input::{self, Touch};
use neochumby::settings;

/// The escape hatch: a touch held this long in this corner. The resistive
/// touchscreen reports brief releases during a steady hold; one shorter
/// than ESCAPE_GRACE does not end it.
const ESCAPE_HOLD: Duration = Duration::from_secs(3);
const ESCAPE_GRACE: Duration = Duration::from_millis(400);
const ESCAPE_CORNER: i32 = 60;

fn sensors() {
    let mut touch = input::Touchscreen::open();
    let mut squeeze = input::Squeeze::open();
    let mut accel = input::Accel::open();
    let mut tick = 0u32;
    loop {
        for t in touch.poll() {
            println!("{t:?}");
        }
        if tick.is_multiple_of(10) {
            println!("squeeze={} accel={:?}", squeeze.squeezed(), accel.read());
        }
        tick += 1;
        neochumby::sleep_ms(30);
    }
}

/// Sets the flag when a touch is held in the top-left corner for
/// [`ESCAPE_HOLD`]. Input devices give every reader every event, so this
/// takes nothing from whatever else is reading the touchscreen.
fn watch_escape(flag: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut touch = input::Touchscreen::open();
        let mut held_since: Option<Instant> = None;
        let mut released_at: Option<Instant> = None;
        loop {
            for t in touch.poll() {
                match t {
                    Touch::Down { x, y, .. } if x < ESCAPE_CORNER && y < ESCAPE_CORNER => {
                        if held_since.is_none() {
                            eprintln!("neochumby: escape: held at ({x}, {y})");
                            held_since = Some(Instant::now());
                        }
                        released_at = None;
                    }
                    Touch::Down { .. } => {
                        held_since = None;
                        released_at = None;
                    }
                    Touch::Up => {
                        if held_since.is_some() {
                            released_at.get_or_insert_with(Instant::now);
                        }
                    }
                }
            }
            if released_at.is_some_and(|t| t.elapsed() > ESCAPE_GRACE) {
                held_since = None;
                released_at = None;
            }
            if released_at.is_none() && held_since.is_some_and(|t| t.elapsed() >= ESCAPE_HOLD) {
                eprintln!("neochumby: escape");
                flag.store(true, Ordering::SeqCst);
                held_since = None;
            }
            neochumby::sleep_ms(50);
        }
    });
}

/// How often the session tries /dev/switch again while it hasn't got it
/// (another process had it at boot, say).
const BUTTON_RETRY: Duration = Duration::from_secs(5);

/// The top button, opened once for the whole session (button.rs); `again`
/// when retrying, which logs only success.
fn open_button(again: bool) -> Option<File> {
    let open = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open("/dev/switch");
    // Passed on only to an app with `button = true`.
    match open.and_then(|f| neochumby_sys::fd::set_cloexec(f.as_fd(), true).map(|()| f)) {
        Ok(f) => {
            if again {
                eprintln!("neochumby: /dev/switch: open now");
            }
            Some(f)
        }
        Err(e) => {
            if !again {
                eprintln!("neochumby: /dev/switch: {e}; trying again every {} s", BUTTON_RETRY.as_secs());
            }
            None
        }
    }
}

/// The speaker as the session sets it for the apps: a master volume
/// (None: leave it), unmuted or not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Speaker {
    volume: Option<u32>,
    unmuted: bool,
}

impl Speaker {
    /// As one word for an atomic: the volume in the low byte (0xff for
    /// none), unmuted in bit 8.
    fn word(self) -> u32 {
        self.volume.map_or(0xff, |v| v.min(100)) | u32::from(self.unmuted) << 8
    }

    fn from_word(w: u32) -> Speaker {
        Speaker { volume: Some(w & 0xff).filter(|v| *v <= 100), unmuted: w & 0x100 != 0 }
    }
}

/// What the volume thread is asked for and has done, as [`Speaker::word`]s.
/// audio::set_volume runs the stock chumby_set_volume, which the loop
/// mustn't wait on, and one thread applying the latest keeps the order.
/// (Atomics, not a Mutex: std's waits on futex flags 2.6.16 lacks.)
struct Volumes {
    want: AtomicU32,
    done: AtomicU32,
}

impl Volumes {
    /// Starts the thread; nothing is asked of it yet.
    fn start() -> Arc<Volumes> {
        let v = Arc::new(Volumes { want: AtomicU32::new(u32::MAX), done: AtomicU32::new(u32::MAX) });
        let t = v.clone();
        std::thread::spawn(move || {
            loop {
                let w = t.want.load(Ordering::SeqCst);
                if w == t.done.load(Ordering::SeqCst) {
                    neochumby::sleep_ms(20);
                    continue;
                }
                let s = Speaker::from_word(w);
                if let Some(v) = s.volume
                    && let Err(e) = audio::set_volume(v)
                {
                    eprintln!("neochumby: setting the volume: {e}");
                }
                // Unmuted only if that is still what's wanted: a reset that
                // came while the volume was being set has muted it.
                let mute = !(s.unmuted && t.want.load(Ordering::SeqCst) == w);
                match audio::mute(mute) {
                    Ok(()) => eprintln!(
                        "neochumby: the speaker: {}, volume {:?}",
                        if mute { "muted" } else { "unmuted" },
                        s.volume
                    ),
                    Err(e) => eprintln!("neochumby: the speaker's mute: {e}"),
                }
                t.done.store(w, Ordering::SeqCst);
            }
        });
        v
    }
}

/// The Chumby, as the session's loop sees it.
struct Chumby {
    escape: Arc<AtomicBool>,
    boot_volume: Option<u32>,
    speaker: Arc<Volumes>,
    button: Option<File>,
    /// When the session last tried to open the button, while it hasn't got
    /// it; None once it has closed it for good.
    button_tried: Option<Instant>,
    /// Set by the developer link.
    restart: Option<Arc<AtomicBool>>,
    /// The top button as neochumbyd holds it, through the developer link.
    remote: Option<Arc<devlink::Remote>>,
}

impl run::Device for Chumby {
    fn escaped(&mut self) -> bool {
        self.escape.swap(false, Ordering::SeqCst)
    }

    /// Mutes at once (a write to /proc); the volume thread puts the boot's
    /// volume back. As the session ends, it waits for that, up to `wait`.
    fn reset_speaker(&mut self, wait: Duration) {
        if let Err(e) = audio::mute(true) {
            eprintln!("neochumby: muting the speaker: {e}");
        }
        let want = Speaker { volume: self.boot_volume, unmuted: false }.word();
        self.speaker.want.store(want, Ordering::SeqCst);
        let deadline = Instant::now() + wait;
        while self.speaker.done.load(Ordering::SeqCst) != want && Instant::now() < deadline {
            neochumby::sleep_ms(20);
        }
    }

    fn unmute(&mut self, volume: u32) {
        self.speaker.want.store(Speaker { volume: Some(volume), unmuted: true }.word(), Ordering::SeqCst);
    }

    fn speaker_holder(&self) -> Option<u32> {
        audio::holder()
    }

    fn button(&mut self) -> Option<bool> {
        if self.button.is_none()
            && let Some(t) = self.button_tried
            && t.elapsed() >= BUTTON_RETRY
        {
            self.button = open_button(true);
            self.button_tried = Some(Instant::now());
        }
        let mut b = [0u8; 1];
        let real = match self.button.as_mut().map(|f| f.read(&mut b)) {
            Some(Ok(1)) => Some(b[0] != 0),
            _ => None,
        };
        // Read only while the app in front hasn't the button (run.rs), so
        // a press from neochumbyd can reach it.
        let remote = self.remote.as_ref().map(|r| {
            r.handed_over.store(false, Ordering::SeqCst);
            r.button.load(Ordering::SeqCst)
        });
        button::level(real, remote)
    }

    /// Asked for only as an app with `button = true` opens (run.rs): from
    /// then until the button is read again, neochumbyd can't press it.
    fn button_fd(&self) -> Option<RawFd> {
        if let Some(r) = &self.remote {
            r.handed_over.store(true, Ordering::SeqCst);
        }
        self.button.as_ref().map(|f| f.as_raw_fd())
    }

    fn close_button(&mut self) {
        self.button = None;
        self.button_tried = None;
    }

    fn restart(&mut self) -> bool {
        self.restart.as_ref().is_some_and(|r| r.swap(false, Ordering::SeqCst))
    }
}

fn session() {
    // First, so that it carries everything the session logs.
    let link = devlink::Link::start();
    let root = apps::root();
    let all = apps::all(&root);
    jail::init(&all);
    match conf::refresh_hook(&root) {
        Ok(true) => eprintln!("neochumby: {} brought up to date from the tree", conf::HOOK),
        Ok(false) => {}
        Err(e) => eprintln!("neochumby: {}: {e}", conf::HOOK),
    }

    // The master volume as the boot set it from /psp/volume: nothing has
    // played yet.
    let boot_volume = audio::volume();

    // The server's `start`, else the local one: a name, trusted only as far
    // as the tree goes (start.rs).
    let settings = settings::load();
    let core = core_services(&root, &settings);
    let local = conf::get(conf::CONF, "start");
    let choice = match settings::get(&settings, "start") {
        Some(s) => Some(("the server's settings", s)),
        None => local.as_deref().map(|s| (conf::CONF, s)),
    };
    let escape = Arc::new(AtomicBool::new(false));
    watch_escape(escape.clone());

    let ended = match start::choose(&all, choice) {
        None => {
            eprintln!("neochumby: no start app (not even {})", start::HOME);
            run::Ended::ChumbyMode
        }
        Some(first) => {
            let chumby = Chumby {
                escape,
                boot_volume,
                speaker: Volumes::start(),
                button: open_button(false),
                button_tried: Some(Instant::now()),
                restart: link.as_ref().map(devlink::Link::restart_flag),
                remote: link.as_ref().map(devlink::Link::remote),
            };
            // On either ending, the loop has closed the button, stopped the
            // services (neochumby-sync with them) and put the speaker back.
            run::Session::new(&all, &core, &root, first, chumby, run::Timing::default()).run()
        }
    };

    if ended == run::Ended::Restart {
        // Everything this process holds is close-on-exec (std opens files
        // and sockets so, and the request pipes are marked so), so the new
        // session starts with what the boot hook gave this one.
        if let Some(link) = link {
            link.release();
        }
        // Just after a deploy, the NFS client may still hold the replaced
        // file's handle: the first try fails as stale, and drops it.
        let exe = root.join("neochumby");
        for _ in 0..5 {
            let e = Command::new(&exe).exec();
            eprintln!("neochumby: restart: {}: {e}", exe.display());
            neochumby::sleep_ms(1000);
        }
        eprintln!("neochumby: restart failed; back to the Chumby");
    }
    if let Ok(mut fb) = Fb::open() {
        fb.clear(rgb(0, 0, 0));
    }
}

/// The session's own services, supervised like the apps' (services.rs) but
/// no app's: neochumby-sync, when this Chumby's settings say
/// `delivery=sync`. It opens no app, so it is never told what is in front
/// and nothing it asks for is done.
fn core_services(root: &Path, settings: &[(String, String)]) -> Vec<App> {
    if settings::get(settings, "delivery") != Some("sync") {
        return Vec::new();
    }
    vec![App {
        dir: root.to_path_buf(),
        name: "neochumby-sync".into(),
        title: None,
        exec: None,
        colour: 0,
        service: Some(root.join("neochumby-sync")),
        needs: Needs::Required,
        start: false,
        interrupt: false,
        button: false,
        // It mounts the stick.
        companion: false,
        access: neochumby::apps::Access { root: true, ..Default::default() },
    }]
}

/// `neochumby check-set DIR`: whether the set in DIR (the stick's
/// neochumby/a or /b) is complete. The boot hook runs it from the set
/// itself, and runs the set only on exit 0.
fn check_set(dir: Option<String>) -> ! {
    let Some(dir) = dir else {
        eprintln!("usage: neochumby check-set DIR");
        std::process::exit(2)
    };
    match neochumby_sync::stick::check_set(Path::new(&dir), &neochumby_sync::LIMITS) {
        Ok(m) => {
            eprintln!("neochumby: {dir}: complete, {} files", m.len());
            std::process::exit(0)
        }
        Err(e) => {
            eprintln!("neochumby: {dir}: not complete: {e}");
            std::process::exit(1)
        }
    }
}

fn main() {
    neochumby::debug::install("neochumby");
    match std::env::args().nth(1).as_deref() {
        None => session(),
        Some("sensors") => sensors(),
        Some("check-set") => check_set(std::env::args().nth(2)),
        Some(other) => {
            eprintln!("neochumby: unknown command {other}");
            std::process::exit(2);
        }
    }
}
