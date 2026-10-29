//! `neochumby-classic render`: the engine without a Chumby. It runs the real
//! frame loop on a virtual clock (time moves only when a frame sleeps, 40
//! ms a frame) and writes the screen at the times asked for, so the same
//! feeder output and script always give the same pictures. For checking
//! designs and the engine itself on any computer:
//!
//! ```text
//! neochumby-classic render DIR PREFIX [--at MS,MS...] [--taps "MS:X,Y MS:b ..."]
//!     [--ctl "MS:LINE;MS:LINE..."] [--wall EPOCH] [--state DIR]
//! ```
//!
//! - `--at`: write the screen as PREFIX_<MS>.raw (320x240 RGB565, as
//!   /dev/fb0 holds it; dev/fb2png.py makes a PNG of one) at the first frame
//!   at or after each time, then stop. Default 1000.
//! - `--taps`: at MS, a tap at X,Y (pressed and released in one frame), or
//!   `b` for the top button.
//! - `--ctl`: at MS, a line written to the control file (NEOCHUMBY_CLASSIC_CTL,
//!   else /tmp/neochumby-classic/ctl; read every
//!   5th frame), e.g. `3000:show home;5000:night on`.
//! - `--wall`: the wall clock at the start (the clock, night mode), default
//!   now. Local time is TZ's.
//! - `--state`: where wx_music and so on are kept, default a temporary
//!   directory.
//! - `--values "wifi=3,ip=192.0.2.7,uptime=3d 4h"`: fake live device
//!   values, by key; `--device-root DIR`: read the others from copies of
//!   /proc, /etc and /psp files under DIR. Otherwise they are missing (`?`).
//!
//! Times are from the start, in ms. Prefetching finishes within its frame
//! here, so what is drawn never depends on how fast the disk is.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use crate::engine::Engine;
use crate::manifest::{H, W};
use crate::plat::{Plat, Touch, wall_now};

/// Where the virtual monotonic clock starts, in µs: any value works, as
/// long as runs being compared use the same.
pub const T0_US: i64 = 10_000_000_000;

pub struct Host {
    fb: Vec<u16>,
    t: i64,
    wall0: i64,
    prefix: String,
    snaps: VecDeque<i64>,
    taps: VecDeque<(i64, i32, i32)>,
    up: Option<(i32, i32)>,
    buttons: VecDeque<i64>,
    pressed: bool,
    ctl: VecDeque<(i64, String)>,
    ctl_path: PathBuf,
    state: PathBuf,
    /// Fake device values (`--values`), by key name.
    pub values: HashMap<String, String>,
    /// A directory of /proc, /etc and /psp files to read the others from.
    pub device_root: Option<PathBuf>,
}

impl Host {
    /// A screen to `prefix`_<ms>.raw at each of `snaps` (ms from the start).
    pub fn new(prefix: &str, mut snaps: Vec<i64>, wall: i64, state: PathBuf) -> Host {
        snaps.sort();
        Host {
            fb: vec![0; (W * H) as usize],
            t: T0_US,
            wall0: wall,
            prefix: prefix.to_string(),
            snaps: snaps.into(),
            taps: VecDeque::new(),
            up: None,
            buttons: VecDeque::new(),
            pressed: false,
            ctl: VecDeque::new(),
            values: HashMap::new(),
            device_root: None,
            ctl_path: std::env::var_os("NEOCHUMBY_CLASSIC_CTL")
                .map_or_else(|| PathBuf::from("/tmp/neochumby-classic/ctl"), PathBuf::from),
            state,
        }
    }

    fn elapsed(&self) -> i64 {
        (self.t - T0_US) / 1000
    }
}

impl Plat for Host {
    fn fb(&mut self) -> &mut [u16] {
        &mut self.fb
    }

    fn present(&mut self) -> bool {
        let e = self.elapsed();
        while let Some((at, line)) = self.ctl.front().cloned()
            && at <= e
        {
            self.ctl.pop_front();
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&self.ctl_path);
            if let Ok(mut f) = f {
                let _ = writeln!(f, "{line}");
            }
        }
        while let Some(&at) = self.snaps.front()
            && at <= e
        {
            self.snaps.pop_front();
            let path = format!("{}_{at}.raw", self.prefix);
            let bytes: Vec<u8> = self.fb.iter().flat_map(|p| p.to_le_bytes()).collect();
            if let Err(err) = std::fs::write(&path, bytes) {
                eprintln!("render: {path}: {err}");
            }
        }
        !self.snaps.is_empty()
    }

    fn touch(&mut self) -> Option<Touch> {
        if let Some((x, y)) = self.up.take() {
            return Some(Touch::Up(x, y));
        }
        let &(at, x, y) = self.taps.front()?;
        if at > self.elapsed() {
            return None;
        }
        self.taps.pop_front();
        self.up = Some((x, y));
        Some(Touch::Move(x, y))
    }

    fn button(&mut self) -> Option<bool> {
        // One frame pressed, then released.
        if self.pressed {
            self.pressed = false;
        } else if self.buttons.front().is_some_and(|&at| at <= self.elapsed()) {
            self.buttons.pop_front();
            self.pressed = true;
        }
        Some(self.pressed)
    }

    fn backlight(&mut self, night: bool) {
        eprintln!("[backlight] {}", if night { "dim" } else { "full" });
    }

    fn music(&mut self, url: Option<&str>, vol: i32) {
        match url {
            Some(u) => eprintln!("[music] play {u} at {vol}%"),
            None => eprintln!("[music] stop"),
        }
    }

    fn volume(&mut self, vol: i32) {
        eprintln!("[music] volume {vol}%");
    }

    fn state_path(&self, name: &str) -> PathBuf {
        self.state.join(name)
    }

    fn now_us(&self) -> i64 {
        self.t
    }

    fn wall(&self) -> i64 {
        self.wall0 + (self.t - T0_US) / 1_000_000
    }

    fn sleep_us(&mut self, us: i64) {
        self.t += us.max(0);
    }

    fn message(&mut self, lines: &[&str]) {
        self.fb.fill(0);
        eprintln!("[screen] {}", lines.join(" "));
    }

    fn device_value(&mut self, key: crate::values::Key) -> Option<String> {
        if let Some(v) = self.values.get(key.name()) {
            return Some(v.clone());
        }
        crate::values::read(self.device_root.as_deref()?, key)
    }

    fn sync_prefetch(&self) -> bool {
        true
    }
}

fn usage() -> i32 {
    eprintln!(
        "usage: neochumby-classic render DIR PREFIX [--at MS,MS...] [--taps \"MS:X,Y MS:b\"] \
         [--ctl \"MS:LINE;...\"] [--wall EPOCH] [--state DIR] [--values \"KEY=VALUE,...\"] [--device-root DIR]"
    );
    2
}

fn ms_of(s: &str) -> Option<i64> {
    s.trim().parse().ok()
}

/// Runs `render`; the exit code.
pub fn render(args: &[String]) -> i32 {
    let (Some(dir), Some(prefix)) = (args.first(), args.get(1)) else { return usage() };
    let mut at = "1000".to_string();
    let (mut taps, mut ctl, mut wall, mut state) = (String::new(), String::new(), None, None);
    let (mut vals, mut root) = (String::new(), None);
    let mut i = 2;
    while i < args.len() {
        let Some(v) = args.get(i + 1).cloned() else { return usage() };
        match args[i].as_str() {
            "--at" => at = v,
            "--taps" => taps = v,
            "--ctl" => ctl = v,
            "--wall" => wall = v.parse().ok(),
            "--state" => state = Some(PathBuf::from(v)),
            "--values" => vals = v,
            "--device-root" => root = Some(PathBuf::from(v)),
            _ => return usage(),
        }
        i += 2;
    }
    let snaps: Vec<i64> = at.split(',').filter_map(ms_of).collect();
    let state =
        state.unwrap_or_else(|| std::env::temp_dir().join(format!("neochumby-classic-state-{}", std::process::id())));
    let mut h = Host::new(prefix, snaps, wall.unwrap_or_else(wall_now), state);
    h.device_root = root;
    for kv in vals.split(',').filter(|s| !s.is_empty()) {
        let Some((k, v)) = kv.split_once('=') else { return usage() };
        h.values.insert(k.trim().to_string(), v.to_string());
    }
    for item in taps.split_whitespace() {
        let Some((t, what)) = item.split_once(':') else { return usage() };
        let Some(t) = ms_of(t) else { return usage() };
        if what == "b" {
            h.buttons.push_back(t);
        } else if let Some((x, y)) = what.split_once(',')
            && let (Ok(x), Ok(y)) = (x.parse(), y.parse())
        {
            h.taps.push_back((t, x, y));
        } else {
            return usage();
        }
    }
    for item in ctl.split(';').filter(|s| !s.trim().is_empty()) {
        let Some((t, line)) = item.split_once(':') else { return usage() };
        let Some(t) = ms_of(t) else { return usage() };
        h.ctl.push_back((t, line.trim().to_string()));
    }
    if let Err(e) = std::fs::create_dir_all(&h.state) {
        eprintln!("render: {}: {e}", h.state.display());
        return 1;
    }
    let mut e = Engine::new(h, PathBuf::from(dir));
    e.start();
    e.run();
    0
}
