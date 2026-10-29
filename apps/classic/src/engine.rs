//! The engine: it draws nothing itself. The feeder prerenders every pixel as
//! RGB565 and writes a manifest (manifest.rs); this only copies rectangles,
//! which is why it runs at about 1% CPU. Two modes:
//!
//! - **slideshow**: pages that rotate with a 90s wipe. A page may have
//!   animated sprites, actors (sprite sheets moving along a path), a clock,
//!   a scrolling ticker, light layers (glow, glowseq), a panning panorama
//!   and a logo intro.
//! - **screens**: interactive pages (Home, Music, Settings): a background,
//!   panes that show the frame matching a state variable, tap zones with
//!   actions, and a volume slider.
//!
//! The top button toggles slideshow and Home; so does tapping the logo box
//! (the top-left 64x40) on a page. Screens fall back to the slideshow after
//! 60 s without a touch. In night mode, opening screen X shows X_night if
//! the feeder made one, and pages with a pagenight file switch too.
//!
//! Night mode has three settings (state `nightmode`): 0 auto (follow the
//! sun lines), 1 always on, 2 always off. A NIGHT tap flips it for now,
//! until the next sunrise or sunset (12 h without sun data).
//!
//! State variables (for panes): music, station, night, nightmode, pin,
//! page, secs. music, station, night, nightmode, the volume and the
//! slideshow timing persist (wx_music and so on, in /psp on the Chumby;
//! the names the C engine used, so a Chumby keeps its settings).
//!
//! Actions (tap zones): slideshow, screen NAME, chumby (exit: the session
//! hands the Chumby back to its own software), music_toggle, station N,
//! next_station, night, nightmode, pin, next_app, prev_app, secs_up,
//! secs_down (next_app and prev_app step whole apps with pageapp), and app
//! NAME: open apps/NAME from the neochumby tree. classic is a start app
//! (proposal 0002): it asks the session (session.rs) to open an app, or for
//! CHUMBY MODE, and exits; the top button arrives from the session too.
//!
//! Control: lines appended to the control file run within 0.2 s:
//! show SCREEN | page N | pin on|off | night on|off |
//! nightmode auto|on|off|next | tap X Y | button | stats | bench on|off |
//! reload | drop [all] | fx N|NAME|next|random | fx loop on|all|off | app NAME.
//! stats (and every 5 s while bench is on) writes the stats file. Both are
//! in a private directory, /tmp/neochumby-classic/ (`ctl`, `stat`), unless
//! NEOCHUMBY_CLASSIC_CTL or NEOCHUMBY_CLASSIC_STAT name others. NEOCHUMBY_CLASSIC_CACHE_KB
//! sets the cache's budget.
//!
//! The manifest and pictures come from a server and are not trusted: lines
//! with numbers that make no sense are skipped (manifest.rs), no file may be
//! bigger than the cache's budget, and the cache never grows past
//! [`ceiling`] even for what is on screen.
//!
//! Live device values (values.rs: the WiFi bars, the IP address, uptime...)
//! are read here, every 5 s while the view on screen uses one, and drawn as
//! panes (VAR `wifi`) or `live` text boxes; only boxes whose text changed
//! are redrawn.
//!
//! Pictures load lazily into a byte-budgeted cache (cache.rs): a page's or
//! screen's files are read when it is shown, and the likely next ones are
//! prefetched by the loader thread (loader.rs) in each frame's idle time.
//!
//! The picture directory: if DIR/current names a subdirectory (one word,
//! "a" or "b"), the set lives there and a push switches sets by rewriting
//! `current` (FAT has no atomic rename of a directory). Otherwise DIR holds
//! manifest.txt itself. Until there is a usable manifest, a waiting screen
//! shows and the directory is looked at again every second.

use std::collections::VecDeque;
use std::path::PathBuf;

use crate::cache::{self, Asset, Cache, State};
use crate::draw::{KEY, add_light, sin};
use crate::loader::{self, Fail, Loader};
use crate::logo::{self, FX_LOOP_GAP, FX_OUT_GAP, FX_SHINE, Logo};
use crate::manifest::{self, Align, FPS, H, Ix, MAXASSET, Manifest, NFX, Size, Slider, Target, W, fx_parse};
use crate::plat::{Plat, Touch, local_hms};
use crate::scan::{atoi, scan};
use crate::session::{Ask, Heard, Session, valid_app};
use crate::state::{self, mtime, read_int, read_nofollow, resolve_dir, save_int};
use crate::values::{self, Key};

const WIPE_FRAMES: i32 = 10;
const SCREEN_IDLE_MS: i64 = 60000;
/// The page wipe; a logo intro starts after it.
const FX_WIPE_MS: i64 = WIPE_FRAMES as i64 * 1000 / FPS as i64;
/// Rebuild the sky and glow layers every 2nd frame (12.5 fps).
const GLOW_DIV: i64 = 2;
const MAXPF: usize = 10;
const WIN_MS: i64 = 5000;
/// Taps closer than this are one tap: the resistive panel can report one
/// tap as two.
const TAP_ECHO_MS: i64 = 350;
const N: usize = (W * H) as usize;
/// How often device values are read while the view uses them.
const VALUES_MS: i64 = 5000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// The clock and ticker in the current theme: always kept.
    Global,
    Page,
    Screen,
}

#[derive(Default)]
struct Stats {
    hits: u32,
    misses: u32,
    prefetched: u32,
    bad: u32,
    evicted: u32,
    chunks: u32,
    read_bytes: u64,
    prefetch_bytes: u64,
    chunk_us_max: i64,
    chunk_us_avg: i64,
    trans_ms: i64,
    trans_need_ms: i64,
    load_us_max: i64,
    load_us_avg: i64,
    gsq_frames: u32,
    gsq_late: u32,
    trans_hit: bool,
    trans_what: String,
}

#[derive(Default, Clone, Copy)]
struct Win {
    start: i64,
    frames: i64,
    work_min: i64,
    work_max: i64,
    work_sum: i64,
    gap_max: i64,
}

/// The logo intro now running.
struct FxRun {
    fx: i32,
    last: i32,
    /// An intro asked for (`fx NAME`) while the page wasn't up; -2 none.
    pend: i32,
    /// fx loop: 1 replays it, 2 plays every intro in turn.
    looping: i32,
    seed: u32,
    rng: u32,
    t0: i64,
}

#[derive(Default)]
struct FxCur {
    active: bool,
    fx: i32,
    frames: u32,
    sum: i64,
    lsum: i64,
    max: i64,
    lmax: i64,
    t0: i64,
    t1: i64,
}

struct FxLast {
    fx: i32,
    frames: u32,
    avg: i64,
    max: i64,
    fps_x10: i64,
    logo_avg: i64,
    logo_max: i64,
}

/// The file the loader is reading now. `asset` None: no longer wanted
/// (evicted, or a new manifest), dropped when it comes.
struct Inflight {
    asset: Option<usize>,
    t0: i64,
}

/// The waiting screen's built-in choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fallback {
    ChumbyMode,
    Apps,
}

/// load() found no usable manifest: the last good set (if any) stays up.
#[derive(Debug, PartialEq, Eq)]
pub struct NoManifest;

pub struct Engine<P: Plat> {
    pub plat: P,
    /// The session's pipe, if the session started us.
    session: Option<Box<dyn Session>>,
    /// The session opened us for a button press: start on Home.
    reason_button: bool,
    /// An app has been asked for (or CHUMBY MODE): leave at the frame's end.
    leave: bool,
    m: Manifest,
    c: Cache,
    back: Vec<u16>,
    /// A black background, for a page or screen whose file is bad.
    blank: Vec<u16>,
    /// The sky and light layers of a glowing page, kept between rebuilds.
    glowbuf: Vec<u16>,
    glow_key: i64,
    glow_page: i32,
    /// Each glow's lit spans, for static screen-sized layers.
    glow_runs: Vec<Option<Vec<u32>>>,
    logo_ix: Option<logo::Index>,
    gsq_shown: Vec<i32>,

    base_dir: PathBuf,
    dir: PathBuf,
    loaded_dir: PathBuf,
    manifest_mtime: i64,
    hashes: Option<Vec<u8>>,

    /// -1: the slideshow; else the screen shown.
    scr: i32,
    /// The screen's name without _night.
    scr_base: String,
    page: i32,
    music_on: i32,
    station: i32,
    night: i32,
    pin: i32,
    vol: i32,
    clock_left: i32,
    nightmode: i32,
    secs_override: i32,
    night_override_until: i64,
    /// The page a pagetap left, for the screen's slideshow action.
    ret_page: i32,

    fxr: FxRun,
    fxs_cur: FxCur,
    fxs_last: FxLast,
    fx_logo_us: i64,
    fx_hold_us: i64,

    loader: Option<Loader>,
    inflight: Option<Inflight>,
    pf: Vec<(Kind, i32)>,
    pf_stalled: bool,
    s: Stats,

    wipe: i32,
    page_start: i64,
    last_touch: i64,
    bench: bool,
    /// The slideshow page on screen; -1 while a screen is.
    ss_page: i32,
    win: Win,
    lastwin: Win,
    inj: VecDeque<Touch>,
    inj_button: bool,
    want_reload: bool,

    /// The device values last read, by Key::index; None: missing.
    vals: [Option<String>; values::KEYS.len()],
    /// When they were last read, in ms.
    vals_t: i64,
    /// Each live box's text as drawn; None: not drawn yet.
    live_shown: Vec<Option<String>>,
    ctl_path: Option<PathBuf>,
    stat_path: Option<PathBuf>,
    /// No usable manifest yet: the waiting screen is up.
    waiting: bool,
    /// On the waiting screen: the built-in menu's choice awaiting its
    /// second tap, and until when (ms).
    armed: Option<(Fallback, i64)>,

    // The frame loop's own.
    tx: i32,
    ty: i32,
    dragging: bool,
    button_last: bool,
    last_tap: i64,
    last_check: i64,
    next: i64,
    frame_prev: i64,
    frame_no: u32,
    last_sun_check: i64,
    last_sec: i64,
}

/// The most memory the cache may hold, whatever is on screen: twice its
/// budget, and at least 8 MB (the aurora page alone is about 2 MB).
pub fn ceiling(budget: usize) -> usize {
    (budget * 2).max(8 << 20)
}

/// A file named by `var`, else `name` in state::RUN_DIR (state::run_dir).
fn run_path(var: &str, name: &str) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(var) {
        return Some(PathBuf::from(p));
    }
    let d = state::run_dir();
    if d.is_none() {
        eprintln!("{} isn't a directory of ours: no {name} file", state::RUN_DIR);
    }
    Some(d?.join(name))
}

impl<P: Plat> Engine<P> {
    pub fn new(plat: P, base_dir: PathBuf) -> Engine<P> {
        // At most 1 GB: far more than the Chumby has, and it keeps every
        // picture's offsets inside 32 bits.
        let budget = std::env::var("NEOCHUMBY_CLASSIC_CACHE_KB")
            .ok()
            .map(|v| atoi(v.as_bytes()))
            .filter(|&k| (512..=1 << 20).contains(&k))
            .unwrap_or(4096);
        Engine {
            plat,
            session: None,
            reason_button: false,
            leave: false,
            m: Manifest::default(),
            c: Cache::new(budget as usize * 1024),
            back: vec![0; N],
            blank: vec![0; N],
            glowbuf: vec![0; N],
            glow_key: -1,
            glow_page: -1,
            glow_runs: Vec::new(),
            logo_ix: None,
            gsq_shown: Vec::new(),
            dir: base_dir.clone(),
            base_dir,
            loaded_dir: PathBuf::new(),
            manifest_mtime: 0,
            hashes: None,
            scr: -1,
            scr_base: String::new(),
            page: 0,
            music_on: 0,
            station: 0,
            night: 0,
            pin: 0,
            vol: 65,
            clock_left: W,
            nightmode: 0,
            secs_override: 0,
            night_override_until: 0,
            ret_page: -1,
            fxr: FxRun { fx: 0, last: -1, pend: -2, looping: 0, seed: 0, rng: 0, t0: 0 },
            fxs_cur: FxCur::default(),
            fxs_last: FxLast { fx: -1, frames: 0, avg: 0, max: 0, fps_x10: 0, logo_avg: 0, logo_max: 0 },
            fx_logo_us: 0,
            fx_hold_us: 0,
            loader: None,
            inflight: None,
            pf: Vec::new(),
            pf_stalled: false,
            s: Stats::default(),
            wipe: 0,
            page_start: 0,
            last_touch: 0,
            bench: false,
            ss_page: -1,
            win: Win::default(),
            lastwin: Win::default(),
            inj: VecDeque::new(),
            inj_button: false,
            want_reload: false,

            vals: Default::default(),
            vals_t: 0,
            live_shown: Vec::new(),
            ctl_path: run_path("NEOCHUMBY_CLASSIC_CTL", "ctl"),
            stat_path: run_path("NEOCHUMBY_CLASSIC_STAT", "stat"),
            waiting: false,
            armed: None,
            tx: 0,
            ty: 0,
            dragging: false,
            button_last: false,
            last_tap: 0,
            last_check: 0,
            next: 0,
            frame_prev: 0,
            frame_no: 0,
            last_sun_check: 0,
            last_sec: 0,
        }
    }

    fn us(&self) -> i64 {
        self.plat.now_us()
    }

    fn ms(&self) -> i64 {
        self.plat.now_us() / 1000
    }

    fn npages(&self) -> i32 {
        self.m.pages.len() as i32
    }

    // ------------------------------------------------------------ loading

    /// Frees an asset's memory and what was derived from it.
    fn evict(&mut self, i: usize) {
        self.loader_forget(Some(i));
        self.pre_evict(i);
        self.c.evict(i);
        self.s.evicted += 1;
    }

    fn make_room(&mut self, n: usize, force: bool) -> bool {
        let mut ev = Vec::new();
        let ok = self.c.make_room(n, force, &mut ev);
        for i in ev {
            self.loader_forget(Some(i));
            self.pre_evict(i);
            self.s.evicted += 1;
        }
        ok
    }

    /// Reads an asset now, in this thread.
    fn load_now(&mut self, i: usize) {
        if matches!(self.c.state(i), State::Ready | State::Bad) {
            return;
        }
        let path = self.dir.join(&self.c.assets[i].name);
        let size = self.c.assets[i].size;
        self.c.start_load(i);
        match loader::read_file(&path, size, false) {
            Ok(d) => {
                self.s.read_bytes += size as u64;
                self.c.finish_load(i, d);
                self.post_load(i);
            }
            Err(e) => {
                match e {
                    Fail::Open => eprintln!("cannot read {}", self.c.assets[i].name),
                    Fail::Memory => eprintln!("no memory for {} ({size} bytes)", self.c.assets[i].name),
                    _ => eprintln!("short file {} (want {size} bytes)", self.c.assets[i].name),
                }
                self.c.mark_bad(i);
                self.s.bad += 1;
            }
        }
    }

    fn loader_dead(&mut self) {
        eprintln!("loader thread gone: loading synchronously from now on");
        self.loader = None;
        if let Some(Inflight { asset: Some(i), .. }) = self.inflight.take() {
            self.c.drop_data(i);
            self.c.assets[i].state = State::Empty;
        }
    }

    /// An asset being evicted (all of them: a manifest being replaced): if
    /// it is on its way, it is dropped when it comes.
    fn loader_forget(&mut self, i: Option<usize>) {
        if let Some(f) = &mut self.inflight
            && (i.is_none() || f.asset == i)
        {
            f.asset = None;
        }
    }

    /// Takes in what the loader has sent, waiting until `deadline_us` (or,
    /// with `block`, until the file is complete). When the file in flight
    /// has finished: whether it was still wanted.
    fn loader_pump(&mut self, deadline_us: i64, block: bool) -> Option<bool> {
        let l = self.loader.as_ref()?;
        self.inflight.as_ref()?;
        let timeout = if block {
            -1
        } else {
            let left = deadline_us - self.us();
            if left < 1500 { 0 } else { (left / 1000 - 1) as i32 }
        };
        let t0 = self.us();
        let rep = match l.recv(timeout) {
            Err(_) => {
                self.loader_dead();
                return None;
            }
            Ok(None) => return None,
            Ok(Some(r)) => r,
        };
        let dt = self.us() - t0;
        self.s.chunks += 1;
        self.s.chunk_us_max = self.s.chunk_us_max.max(dt);
        self.s.chunk_us_avg += (dt - self.s.chunk_us_avg) / 8;
        let f = self.inflight.take().expect("in flight");
        let Some(i) = f.asset.filter(|&i| i == rep.id) else { return Some(false) };
        match rep.data {
            Ok(d) => {
                let size = self.c.assets[i].size;
                self.c.finish_load(i, d);
                self.s.read_bytes += size as u64;
                self.s.prefetch_bytes += size as u64;
                let lt = self.us() - f.t0;
                self.s.load_us_max = self.s.load_us_max.max(lt);
                self.s.load_us_avg =
                    if self.s.load_us_avg != 0 { self.s.load_us_avg + (lt - self.s.load_us_avg) / 16 } else { lt };
                self.post_load(i);
            }
            Err(e) => {
                let name = &self.c.assets[i].name;
                match e {
                    Fail::Open => eprintln!("cannot read {name}"),
                    Fail::Size(n) => eprintln!("bad file {name} ({n} bytes, want {})", self.c.assets[i].size),
                    Fail::Short => eprintln!("short file {name}"),
                    Fail::Memory => eprintln!("no memory for {name}"),
                }
                self.c.mark_bad(i);
                self.s.bad += 1;
            }
        }
        Some(true)
    }

    fn loader_request(&mut self, i: usize) -> bool {
        let Some(l) = &self.loader else { return false };
        let a = &self.c.assets[i];
        if !l.request(i, self.dir.join(&a.name), a.size) {
            self.loader_dead();
            return false;
        }
        self.inflight = Some(Inflight { asset: Some(i), t0: self.us() });
        true
    }

    // ------------------------------------------------------------ night variants

    /// Whether page p is drawn from its pagenight file now.
    fn pnight(&self, p: i32) -> bool {
        self.night != 0 && p >= 0 && p < self.npages() && self.m.pages[p as usize].nbg.is_some()
    }

    /// The background page p draws: its night file at night, unless that
    /// is bad and the day one isn't.
    fn page_bgi(&self, p: i32) -> Ix {
        let pg = &self.m.pages[p as usize];
        if !self.pnight(p) {
            return pg.bg;
        }
        let n = pg.nbg.expect("night page");
        if self.c.state(n) == State::Bad && self.c.px(pg.bg).is_some() { pg.bg } else { pg.nbg }
    }

    fn clk_px(&self, p: i32) -> Ix {
        if self.pnight(p) && self.m.clock.pxn.is_some() { self.m.clock.pxn } else { self.m.clock.px }
    }

    fn tick_px(&self, p: i32) -> Ix {
        if self.pnight(p) && self.m.ticker.pxn.is_some() { self.m.ticker.pxn } else { self.m.ticker.px }
    }

    // ------------------------------------------------------------ groups

    /// Everything one page or screen draws (G_GLOBAL: the clock and ticker
    /// in the current theme), in order, repeats included.
    fn group(&self, kind: Kind, id: i32) -> Vec<usize> {
        let m = &self.m;
        let mut v: Vec<usize> = Vec::new();
        let mut a = |x: Ix| v.extend(x);
        match kind {
            Kind::Global => {
                let night = self.night != 0;
                a(if night && m.clock.pxn.is_some() { m.clock.pxn } else { m.clock.px });
                a(if night && m.ticker.pxn.is_some() { m.ticker.pxn } else { m.ticker.px });
            }
            Kind::Screen => {
                if id < 0 || id as usize >= m.screens.len() {
                    return v;
                }
                let id = id as usize;
                a(Some(m.screens[id].bg));
                for p in m.panes.iter().filter(|p| p.on == Target::Screen(id)) {
                    a(Some(p.px));
                }
                for l in m.lives.iter().filter(|l| l.on == Target::Screen(id)) {
                    a(Some(m.fonts[l.font].px));
                }
                for s in m.sliders.iter().filter(|s| s.scr == id) {
                    a(Some(s.empty));
                    a(Some(s.full));
                    a(Some(s.knob));
                }
            }
            Kind::Page => {
                if id < 0 || id >= self.npages() {
                    return v;
                }
                let pg = &m.pages[id as usize];
                a(self.page_bgi(id));
                a(pg.pano);
                if !pg.noclock {
                    a(self.clk_px(id));
                }
                if !pg.noticker {
                    a(self.tick_px(id));
                }
                for s in m.sprites.iter().filter(|s| s.page == id) {
                    a(Some(s.px));
                }
                for ac in m.actors.iter().filter(|ac| ac.page == id) {
                    a(Some(ac.px));
                    a(Some(ac.path));
                }
                for g in m.glows.iter().filter(|g| g.page == id) {
                    a(Some(g.px));
                }
                if m.logo.page == id {
                    a(m.logo.px);
                }
                if m.scroller.page == id {
                    a(m.scroller.px);
                }
                let on = self.page_target(id);
                for p in m.panes.iter().filter(|p| p.on == on) {
                    a(Some(p.px));
                }
                for l in m.lives.iter().filter(|l| l.on == on) {
                    a(Some(m.fonts[l.font].px));
                }
            }
        }
        v
    }

    fn protect(&mut self, kind: Kind, id: i32) {
        for i in self.group(kind, id) {
            self.c.protect(i);
        }
    }

    fn cur_kind(&self) -> Kind {
        if self.scr >= 0 { Kind::Screen } else { Kind::Page }
    }

    fn cur_id(&self) -> i32 {
        if self.scr >= 0 { self.scr } else { self.page }
    }

    /// Loads everything a page or screen needs now, blocking only for what
    /// prefetching didn't finish.
    fn need(&mut self, kind: Kind, id: i32) {
        let t0 = self.us();
        self.c.new_round();
        self.protect(Kind::Global, 0);
        self.protect(kind, id);
        let mut missed = false;
        let all: Vec<usize> = self.group(Kind::Global, 0).into_iter().chain(self.group(kind, id)).collect();
        for i in all {
            if matches!(self.c.state(i), State::Ready | State::Bad) {
                self.s.hits += 1;
                continue;
            }
            self.s.misses += 1;
            missed = true;
            // Already on its way: wait for it.
            while self.loader.is_some() && self.inflight.as_ref().is_some_and(|f| f.asset == Some(i)) {
                self.loader_pump(0, true);
            }
            if matches!(self.c.state(i), State::Ready | State::Bad) {
                continue;
            }
            if self.c.state(i) == State::Empty {
                let size = self.c.assets[i].size;
                self.make_room(size, true);
                // What is on screen may go over budget, never over the
                // ceiling: past that, it is left out.
                if self.c.bytes + size > ceiling(self.c.budget) {
                    eprintln!("{} left out: the cache is at its ceiling", self.c.assets[i].name);
                    continue;
                }
            }
            self.load_now(i);
        }
        let now = self.ms();
        for i in self.group(kind, id).into_iter().chain(self.group(Kind::Global, 0)) {
            self.c.assets[i].used = now;
        }
        self.s.trans_need_ms = (self.us() - t0) / 1000;
        self.s.trans_hit = !missed;
    }

    fn pf_add(&mut self, kind: Kind, id: i32) {
        if id < 0 || self.pf.len() >= MAXPF || (kind == self.cur_kind() && id == self.cur_id()) {
            return;
        }
        if !self.pf.contains(&(kind, id)) {
            self.pf.push((kind, id));
        }
    }

    /// What is likely next, most likely first.
    fn plan_prefetch(&mut self) {
        self.pf.clear();
        self.pf_stalled = false;
        let n = self.npages();
        if n == 0 {
            return;
        }
        if self.scr < 0 {
            self.pf_add(Kind::Page, (self.page + 1) % n); // the next slideshow page
            let tap = self.m.pages[self.page as usize].tap.clone();
            if !tap.is_empty() {
                let s = self.resolve_screen(&tap); // a tap on it
                self.pf_add(Kind::Screen, s);
            }
            let s = self.resolve_screen("home"); // the top button, the logo box
            self.pf_add(Kind::Screen, s);
        } else {
            // Where this screen's tap zones lead.
            let mut k = 0;
            while k < self.m.hots.len() && self.pf.len() < MAXPF {
                let h = &self.m.hots[k];
                k += 1;
                if h.scr as i32 != self.scr {
                    continue;
                }
                match h.act.as_str() {
                    "screen" => {
                        let s = self.resolve_screen(&h.name.clone());
                        self.pf_add(Kind::Screen, s);
                    }
                    "slideshow" => self.pf_add(Kind::Page, self.page),
                    "next_app" => self.pf_add(Kind::Page, self.app_step(1)),
                    "prev_app" => self.pf_add(Kind::Page, self.app_step(-1)),
                    _ => {}
                }
            }
            self.pf_add(Kind::Page, self.page); // the idle timeout goes back to the slideshow
        }
    }

    /// The frame a glowseq on screen wants next (current, then the one
    /// after). Frames it no longer needs are dropped at once: only the
    /// shown, current and next stay.
    fn prefetch_extra(&mut self) -> Option<usize> {
        if self.scr >= 0 {
            return None;
        }
        let t = self.ms();
        for g in 0..self.m.gsqs.len() {
            if self.m.gsqs[g].page != self.page {
                continue;
            }
            let n = self.m.gsqs[g].frames.len();
            let f = self.gsq_frame(g, t) as usize;
            let want = [self.m.gsqs[g].frames[f], self.m.gsqs[g].frames[(f + 1) % n]];
            let shown = self.gsq_shown[g];
            let keep = if shown >= 0 { self.m.gsqs[g].frames[shown as usize] } else { None };
            // By asset, not frame number: identical frames share one.
            for q in 0..n {
                let Some(i) = self.m.gsqs[g].frames[q] else { continue };
                if want.contains(&Some(i)) || keep == Some(i) {
                    continue;
                }
                let flying = self.inflight.as_ref().is_some_and(|f| f.asset == Some(i));
                if self.c.holds(i) && !flying {
                    self.evict(i);
                }
            }
            for w in want {
                let Some(i) = w else { continue };
                if matches!(self.c.state(i), State::Ready | State::Bad) {
                    continue;
                }
                if self.inflight.as_ref().is_some_and(|f| f.asset == Some(i)) {
                    return None;
                }
                self.c.new_round();
                self.protect(Kind::Global, 0);
                self.protect(Kind::Page, self.page);
                for x in want.into_iter().flatten().chain(keep) {
                    self.c.protect(x);
                }
                let size = self.c.assets[i].size;
                self.make_room(size, true);
                if self.c.bytes + size > ceiling(self.c.budget) {
                    return None;
                }
                self.c.assets[i].used = t;
                return Some(i);
            }
        }
        None
    }

    /// Spends the rest of this frame (until `deadline_us`) taking in what
    /// the loader sends, asking for the next file the moment one finishes.
    /// On the host, everything asked for is finished before the frame ends.
    fn prefetch_idle(&mut self, deadline_us: i64) {
        let sync = self.plat.sync_prefetch();
        loop {
            if self.loader.is_none() {
                return;
            }
            if self.inflight.is_none() {
                let mut next = self.prefetch_extra();
                let mut k = 0;
                while next.is_none() && k < self.pf.len() && !self.pf_stalled {
                    let (kind, id) = self.pf[k];
                    let first = self
                        .group(kind, id)
                        .into_iter()
                        .find(|&i| matches!(self.c.state(i), State::Empty | State::Loading));
                    let Some(c) = first else {
                        k += 1; // group k complete: on to the next
                        continue;
                    };
                    // Room for it, evicting only what is neither shown nor
                    // more likely next.
                    self.c.new_round();
                    self.protect(Kind::Global, 0);
                    self.protect(self.cur_kind(), self.cur_id());
                    for j in 0..=k {
                        let (kj, ij) = self.pf[j];
                        self.protect(kj, ij);
                    }
                    if !self.make_room(self.c.assets[c].size, false) {
                        self.pf_stalled = true;
                        break;
                    }
                    self.c.assets[c].used = self.ms() - 1; // fresh, but older than what is on screen
                    next = Some(c);
                }
                let Some(i) = next else { return };
                self.c.start_load(i);
                if !self.loader_request(i) {
                    return;
                }
            }
            match self.loader_pump(deadline_us, sync) {
                Some(wanted) => {
                    if wanted {
                        self.s.prefetched += 1;
                    }
                }
                None => return,
            }
        }
    }
}

/// A background, or black if its file is bad.
fn bg_of<'a>(c: &'a Cache, blank: &'a [u16], i: Ix) -> &'a [u16] {
    c.px(i).unwrap_or(blank)
}

/// Copies the back buffer's rectangle to the screen, clipped.
fn blit(fb: &mut [u16], back: &[u16], mut x: i32, mut y: i32, mut w: i32, mut h: i32) {
    if x < 0 {
        w += x;
        x = 0;
    }
    if y < 0 {
        h += y;
        y = 0;
    }
    w = w.min(W - x);
    h = h.min(H - y);
    if w <= 0 || h <= 0 {
        return;
    }
    for r in y..y + h {
        let o = (r * W + x) as usize;
        fb[o..o + w as usize].copy_from_slice(&back[o..o + w as usize]);
    }
}

/// A keyed copy of one frame of a sheet into the back buffer, clipped;
/// the gaps are filled from `bg` (None: left as they are).
#[allow(clippy::too_many_arguments)]
fn put_keyed(back: &mut [u16], sheet: Option<&[u16]>, w: i32, h: i32, frame: i32, x: i32, y: i32, bg: Option<&[u16]>) {
    let Some(sheet) = sheet else { return };
    for r in 0..h {
        let yy = y + r;
        if !(0..H).contains(&yy) {
            continue;
        }
        let src = ((frame * h + r) * w) as usize;
        for c in 0..w {
            let xx = x + c;
            if !(0..W).contains(&xx) {
                continue;
            }
            let o = (yy * W + xx) as usize;
            let p = sheet[src + c as usize];
            if p != KEY {
                back[o] = p;
            } else if let Some(bg) = bg {
                back[o] = bg[o];
            }
        }
    }
}

fn restore(back: &mut [u16], bg: &[u16], x: i32, y: i32, w: i32, h: i32) {
    for r in 0..h {
        let yy = y + r;
        if !(0..H).contains(&yy) {
            continue;
        }
        for c in 0..w {
            let xx = x + c;
            if (0..W).contains(&xx) {
                let o = (yy * W + xx) as usize;
                back[o] = bg[o];
            }
        }
    }
}

/// The panorama, panned for time t, wrapping seamlessly.
fn pan_into_back(back: &mut [u16], pano: &[u16], pano_w: i32, t: i64, speed: i32) {
    let mut off = ((t * FPS as i64 / 1000) * speed as i64 / 4 % pano_w as i64) as i32;
    if off < 0 {
        off += pano_w;
    }
    let first = (pano_w - off).min(W) as usize;
    for r in 0..H as usize {
        let src = &pano[r * pano_w as usize..];
        let dst = &mut back[r * W as usize..(r + 1) * W as usize];
        dst[..first].copy_from_slice(&src[off as usize..off as usize + first]);
        if first < W as usize {
            dst[first..].copy_from_slice(&src[..W as usize - first]);
        }
    }
}

fn rect_ok(x: i32, y: i32, w: i32, h: i32) -> bool {
    let (x, y, w, h) = (x as i64, y as i64, w as i64, h as i64);
    x >= 0 && y >= 0 && w >= 0 && h >= 0 && x + w <= W as i64 && y + h <= H as i64
}

/// A static screen-sized light layer is mostly black, and scanning all of
/// it on every rebuild cost more than blending the lit pixels: its lit
/// spans (start | len << 17), listed once.
fn glow_spans(px: &[u16]) -> Vec<u32> {
    let mut r = Vec::new();
    for y in 0..H {
        let mut x = 0;
        while x < W {
            if px[(y * W + x) as usize] == 0 {
                x += 1;
                continue;
            }
            let s = x;
            while x < W && px[(y * W + x) as usize] != 0 {
                x += 1;
            }
            r.push((y * W + s) as u32 | ((x - s) as u32) << 17);
        }
    }
    r
}

impl<P: Plat> Engine<P> {
    // ------------------------------------------------------------ derived data

    fn index_glow(&mut self, g: usize) {
        self.glow_runs[g] = None;
        let gl = &self.m.glows[g];
        let Some(px) = self.c.px(Some(gl.px)) else { return };
        if gl.speed != 0 || gl.sway_ms != 0 || gl.x0 != 0 || gl.y != 0 || gl.w != W || gl.h != H {
            return;
        }
        self.glow_runs[g] = Some(glow_spans(px));
    }

    fn post_load(&mut self, i: usize) {
        for g in 0..self.m.glows.len() {
            if self.m.glows[g].px == i {
                self.index_glow(g);
            }
        }
        if self.m.logo.px == Some(i) {
            let l = &self.m.logo;
            self.logo_ix = self.c.px(l.px).map(|px| logo::index(px, l.w, l.h));
        }
    }

    fn pre_evict(&mut self, i: usize) {
        for g in 0..self.m.glows.len() {
            if self.m.glows[g].px == i {
                self.glow_runs[g] = None;
            }
        }
        if self.m.logo.px == Some(i) {
            self.logo_ix = None;
        }
        // The cached sky used it: rebuild (not for glowseq frames, which
        // come and go all the time).
        if self.glow_page >= 0 && self.glow_page < self.npages() {
            let pg = &self.m.pages[self.glow_page as usize];
            let used = pg.bg == Some(i)
                || pg.pano == Some(i)
                || self.m.glows.iter().any(|g| g.page == self.glow_page && g.px == i);
            if used {
                self.glow_key = -1;
            }
        }
    }

    // ------------------------------------------------------------ the manifest

    /// Reads the manifest of the current set, keeping in memory every file
    /// it shares with the last one. Err if it has no usable page: the last
    /// good set then stays up.
    pub fn load(&mut self) -> Result<(), NoManifest> {
        let newdir = resolve_dir(&self.base_dir);
        let mpath = newdir.join("manifest.txt");
        let meta = std::fs::metadata(&mpath);
        let text = meta
            .as_ref()
            .ok()
            .filter(|m| m.len() <= manifest::MAX_MANIFEST_BYTES)
            .and_then(|_| std::fs::read(&mpath).ok())
            .filter(|t| t.len() as u64 <= manifest::MAX_MANIFEST_BYTES);
        let size_of = |n: &str| std::fs::metadata(newdir.join(n)).ok().map(|m| m.len());
        let Some(text) = text.filter(|t| manifest::usable(t, size_of)) else {
            if !self.waiting {
                eprintln!("no usable manifest in {}", newdir.display());
            }
            return Err(NoManifest);
        };
        let hpath = newdir.join("hashes.txt");
        let hashes = std::fs::metadata(&hpath)
            .ok()
            .filter(|m| m.len() <= manifest::MAX_MANIFEST_BYTES)
            .and_then(|_| std::fs::read(&hpath).ok());

        // Parse into a new table first; nothing of the running set changes
        // until the new one is known to have a page.
        let budget = self.c.budget as u64;
        // Indices, so registering thousands of files stays linear: content
        // ids from hashes.txt, the new table's files, and the old table's
        // files still in memory (to keep the first match).
        let md5 = hashes.as_deref().map(cache::md5_index).unwrap_or_default();
        let mut have: std::collections::HashMap<(u64, usize), usize> = Default::default();
        let mut olds: std::collections::HashMap<(u64, usize), std::collections::VecDeque<usize>> = Default::default();
        for (i, o) in self.c.assets.iter().enumerate() {
            if o.state == State::Ready {
                olds.entry((o.key, o.size)).or_default().push_back(i);
            }
        }
        let mut assets: Vec<Asset> = Vec::new();
        // For each new asset, the old one whose data it keeps.
        let mut keep: Vec<Option<usize>> = Vec::new();
        let m = manifest::parse(&text, |name, expect| {
            let path = newdir.join(name);
            let st = std::fs::metadata(&path).ok().filter(|s| s.is_file() && s.len() > 0);
            // The right size, and never more than the whole cache.
            let fits = |s: &std::fs::Metadata| {
                s.len() <= budget
                    && match expect {
                        Size::Exactly(n) => s.len() == n as u64,
                        Size::Any => true,
                        Size::Invalid => false,
                    }
            };
            let Some(st) = st.filter(fits) else {
                let got = std::fs::metadata(&path).map_or(-1, |s| s.len() as i64);
                eprintln!("bad file {name} ({got} bytes, want {expect:?}, at most {budget})");
                return None;
            };
            let size = st.len() as usize;
            let key = match md5.get(name) {
                Some(&k) if k != 0 => k,
                _ => cache::file_key(name, st.len(), mtime(&st)),
            };
            if let Some(&i) = have.get(&(key, size)) {
                return Some(i);
            }
            if assets.len() >= MAXASSET {
                return None;
            }
            assets.push(Asset::new(name, key, size));
            keep.push(olds.get_mut(&(key, size)).and_then(|q| q.pop_front()));
            have.insert((key, size), assets.len() - 1);
            Some(assets.len() - 1)
        });
        if m.pages.is_empty() {
            // usable() saw a page, but parsing kept none (a limit reached
            // first, or a file changed between the two looks): keep the
            // last good set up.
            eprintln!("manifest {}: no page kept; staying with the last one", newdir.display());
            return Err(NoManifest);
        }

        // Commit: the new set, keeping unchanged files from the old.
        self.dir = newdir;
        self.manifest_mtime = meta.as_ref().map(mtime).unwrap_or(0);
        self.loaded_dir = self.dir.clone();
        self.hashes = hashes;
        self.loader_forget(None);
        self.glow_runs.clear();
        self.logo_ix = None;
        self.glow_key = -1;
        let mut old = std::mem::replace(&mut self.c.assets, assets);
        for (a, k) in self.c.assets.iter_mut().zip(keep) {
            if let Some(o) = k.map(|i| &mut old[i]) {
                a.data = o.data.take();
                a.state = State::Ready;
                a.used = o.used;
                a.ser = o.ser;
                o.state = State::Empty;
            }
        }
        // Whatever the new manifest didn't keep is gone.
        for o in &old {
            if o.data.is_some() || o.state == State::Loading {
                self.c.bytes -= o.size;
            }
        }
        drop(old);
        self.m = m;
        self.glow_runs = vec![None; self.m.glows.len()];
        self.live_shown = vec![None; self.m.lives.len()];
        self.gsq_shown = vec![-1; self.m.gsqs.len()];
        // Derived data for the files that were kept.
        for i in 0..self.c.assets.len() {
            if self.c.state(i) == State::Ready {
                self.post_load(i);
            }
        }
        eprintln!(
            "manifest {}: {} pages, {} screens, {} files, {} KB ({} KB kept in memory)",
            self.dir.display(),
            self.m.pages.len(),
            self.m.screens.len(),
            self.c.assets.len(),
            self.c.total() / 1024,
            self.c.bytes / 1024
        );
        Ok(())
    }

    // ------------------------------------------------------------ drawing

    fn blit(&mut self, x: i32, y: i32, w: i32, h: i32) {
        blit(self.plat.fb(), &self.back, x, y, w, h);
    }

    fn draw_clock(&mut self) {
        let p = self.page;
        let cpi = self.clk_px(p);
        let k = &self.m.clock;
        // Only a strip the size of this clock's cells (a clocknight sized
        // for another clock is ignored).
        let Some(cp) = self.c.px(cpi).filter(|p| p.len() as u64 >= k.cw as u64 * k.ch as u64 * 15) else { return };
        if self.m.pages[p as usize].noclock || k.right > W || k.y < 0 || k.y + k.ch > H {
            return;
        }
        let bg = bg_of(&self.c, &self.blank, self.page_bgi(p));
        let (hr, mi, se) = local_hms(self.plat.wall());
        let h12 = if hr % 12 != 0 { hr % 12 } else { 12 };
        let s = format!("{h12}:{mi:02}:{se:02} {}", if hr < 12 { "AM" } else { "PM" });
        const GLYPHS: &str = "0123456789: APM";
        let gi = |c: char| GLYPHS.find(c).unwrap_or(0);
        let total: i32 = s.chars().map(|c| k.wd[gi(c)]).sum();
        let mut x = k.right - total;
        let left = x.min(self.clock_left); // also clear a previously wider time
        if left < 0 {
            return;
        }
        for r in 0..k.ch {
            let o = ((k.y + r) * W + left) as usize;
            let n = (k.right - left) as usize;
            self.back[o..o + n].copy_from_slice(&bg[o..o + n]);
        }
        for c in s.chars() {
            let g = gi(c) as i32;
            let w = k.wd[g as usize].min(k.cw);
            for r in 0..k.ch {
                let o = ((k.y + r) * W + x) as usize;
                let src = ((g * k.ch + r) * k.cw) as usize;
                self.back[o..o + w as usize].copy_from_slice(&cp[src..src + w as usize]);
            }
            x += w;
        }
        let (y, ch, right) = (k.y, k.ch, k.right);
        self.blit(left, y, right - left, ch);
        self.clock_left = right - total;
    }

    fn draw_ticker(&mut self, t: i64) {
        let k = &self.m.ticker;
        let Some(tp) = self.c.px(self.tick_px(self.page)).filter(|p| p.len() as u64 >= k.w as u64 * k.h as u64) else {
            return;
        };
        if k.w <= W || self.m.pages[self.page as usize].noticker || k.y < 0 || k.y + k.h > H {
            return;
        }
        let span = (k.w - W) as i64;
        let mut off = t * k.speed as i64 * FPS as i64 / 1000 % span;
        if off < 0 {
            off += span;
        }
        for r in 0..k.h {
            let o = ((k.y + r) * W) as usize;
            let s = (r as i64 * k.w as i64 + off) as usize;
            self.back[o..o + W as usize].copy_from_slice(&tp[s..s + W as usize]);
        }
        let (y, h) = (k.y, k.h);
        self.blit(0, y, W, h);
    }

    /// Where each actor on this page is now.
    fn actors_place(&mut self, t: i64) {
        let n = t * FPS as i64 / 1000;
        for a in &mut self.m.actors {
            if a.page != self.page {
                continue;
            }
            let Some(path) = self.c.px(Some(a.path)) else { continue };
            let p = (n % a.n as i64) as usize;
            a.x = path[p * 2] as i16 as i32;
            a.y = path[p * 2 + 1] as i16 as i32;
        }
    }

    fn actor_frame(a: &manifest::Actor, t: i64) -> i32 {
        (t * a.fps as i64 / 1000).rem_euclid(a.frames as i64) as i32
    }

    fn fxs_commit(&mut self) {
        let c = &self.fxs_cur;
        if c.frames > 0 {
            self.fxs_last = FxLast {
                fx: c.fx,
                frames: c.frames,
                avg: c.sum / c.frames as i64,
                max: c.max,
                fps_x10: if c.t1 > c.t0 { (c.frames as i64 - 1) * 10000 / (c.t1 - c.t0) } else { 0 },
                logo_avg: c.lsum / c.frames as i64,
                logo_max: c.lmax,
            };
        }
        self.fxs_cur = FxCur::default();
    }

    fn fx_rand(&mut self) -> u32 {
        let us = self.us();
        logo::rand(&mut self.fxr.rng, us)
    }

    /// Starts an intro at t: `which` >= 0 forces one; -1 is the manifest's
    /// choice, else random (never the same twice running).
    fn fx_start(&mut self, t: i64, which: i32) {
        self.fxs_commit();
        let mut which = if which < 0 { self.m.logo.force } else { which };
        if !(0..NFX).contains(&which) {
            which = (self.fx_rand() % NFX as u32) as i32;
            if which == self.fxr.last {
                which = (which + 1 + (self.fx_rand() % (NFX as u32 - 1)) as i32) % NFX;
            }
        }
        self.fxr.fx = which;
        self.fxr.last = which;
        self.fxr.t0 = t;
        self.fxr.seed = self.fx_rand();
    }

    fn draw_logofx(&mut self, t: i64) {
        let l = &self.m.logo;
        if l.page != self.page || self.c.px(l.px).is_none() {
            return;
        }
        let Some(ix) = &self.logo_ix else { return };
        if ix.ry.is_empty() || ix.ng == 0 {
            return;
        }
        let mut tc = t - self.fxr.t0;
        let mut d = Logo::dur(ix, self.fxr.fx);
        // What to draw: Some(time into the intro), or the shine, or still.
        enum Draw {
            Intro(i64),
            Shine(i64),
            Still,
            Nothing,
        }
        let mut what = None;
        if self.fxr.looping != 0 {
            // Benching (fx loop): again, or the next one.
            if tc >= d + FX_SHINE + FX_LOOP_GAP {
                let next = if self.fxr.looping == 2 { (self.fxr.fx + 1) % NFX } else { self.fxr.fx };
                self.fx_start(t, next);
                tc = 0;
                d = Logo::dur(self.logo_ix.as_ref().expect("index"), self.fxr.fx);
            }
        } else if !self.m.logo.once {
            // The intro, the shine, a hold, the intro backwards, a gap.
            let hold_end = d + FX_SHINE + self.m.logo.hold_ms as i64;
            let out_end = hold_end + d;
            if tc >= out_end + FX_OUT_GAP {
                self.fx_start(t, -1);
                tc = 0;
                d = Logo::dur(self.logo_ix.as_ref().expect("index"), self.fxr.fx);
            } else if tc >= out_end {
                what = Some(Draw::Nothing); // the gap: sky only
            } else if tc >= hold_end {
                self.fxs_cur.active = true;
                what = Some(Draw::Intro(d - (tc - hold_end))); // time reversed
            }
        }
        let what = what.unwrap_or_else(|| {
            if tc < d {
                if tc < 0 {
                    tc = 0;
                } else {
                    self.fxs_cur.active = true; // this frame's work counts towards the intro
                }
                Draw::Intro(tc)
            } else if tc - d < FX_SHINE {
                Draw::Shine(tc - d)
            } else {
                Draw::Still
            }
        });
        let l = &self.m.logo;
        let lg = Logo {
            px: self.c.px(l.px).expect("logo"),
            ix: self.logo_ix.as_ref().expect("index"),
            x: l.x,
            y: l.y,
            w: l.w,
            h: l.h,
            seed: self.fxr.seed,
        };
        match what {
            Draw::Intro(tc) => lg.run(&mut self.back, self.fxr.fx, tc),
            Draw::Shine(tc) => lg.shine(&mut self.back, tc),
            Draw::Still => lg.sharp(&mut self.back),
            Draw::Nothing => {}
        }
    }

    /// Adds the light layers onto the back buffer, per RGB565 channel,
    /// saturating; black adds nothing.
    fn draw_glows(&mut self, t: i64) {
        let n = t * FPS as i64 / 1000;
        for (gi, g) in self.m.glows.iter().enumerate() {
            if g.page != self.page {
                continue;
            }
            let Some(gpx) = self.c.px(Some(g.px)) else { continue };
            let w = g.w as i64;
            // x0: where the screen starts in the strip.
            let mut off = (n * g.speed as i64 / 4 % w) as i32 + g.x0;
            // Optional sway: back and forth with the surface swell.
            if g.sway_ms != 0 {
                off += g.sway_px * sin((t % g.sway_ms as i64 * 256 / g.sway_ms as i64) as i32) / 127;
            }
            off %= g.w;
            if off < 0 {
                off += g.w;
            }
            // Optional fade: brightness 0..8 eighths on a slow cycle.
            let mut lvl = 8;
            if g.fade_ms != 0 && g.on_ms != 0 {
                // Lit only for ON_MS of each FADE_MS cycle, easing in and
                // out at the ends: several layers on one cycle, phased
                // apart, are one ray that moves now and then.
                let u = (t + g.phase_ms as i64) % g.fade_ms as i64;
                let on = g.on_ms as i64;
                if u >= on {
                    continue;
                }
                let edge = (on / 2).min(1500);
                let e = u.min(on - u);
                if e < edge {
                    let sv = sin((e * 64 / edge) as i32); // a quarter wave, 0..127
                    lvl = sv * sv * 8 / (127 * 127);
                    if lvl == 0 {
                        continue;
                    }
                }
            } else if g.fade_ms != 0 {
                // sin^2: lingers dark, swells, dies.
                let sv = sin(((t + g.phase_ms as i64) % g.fade_ms as i64 * 128 / g.fade_ms as i64) as i32);
                lvl = sv * sv * 8 / (127 * 127);
                if lvl == 0 {
                    continue;
                }
            }
            if let Some(runs) = &self.glow_runs[gi] {
                for &r in runs {
                    let s = (r & 0x1FFFF) as usize;
                    let e = s + (r >> 17) as usize;
                    for (d, &b) in self.back[s..e].iter_mut().zip(&gpx[s..e]) {
                        add_light(d, b, lvl);
                    }
                }
                continue;
            }
            for r in 0..g.h {
                let y = g.y + r;
                if !(0..H).contains(&y) {
                    continue;
                }
                let src = &gpx[(r * g.w) as usize..((r + 1) * g.w) as usize];
                let dst = &mut self.back[(y * W) as usize..((y + 1) * W) as usize];
                let mut sx = off as usize;
                for d in dst.iter_mut() {
                    if sx == g.w as usize {
                        sx = 0;
                    }
                    if src[sx] != 0 {
                        add_light(d, src[sx], lvl);
                    }
                    sx += 1;
                }
            }
        }
    }

    fn draw_scroller(&mut self, t: i64) {
        let s = &self.m.scroller;
        let Some(spx) = self.c.px(s.px) else { return };
        if s.page != self.page || s.w <= W {
            return;
        }
        let span = (s.w - W) as i64;
        let mut off = (t * s.speed as i64 * FPS as i64 / 1000 % span) as i32;
        if off < 0 {
            off += span as i32;
        }
        let ph = (t / 6) as i32;
        for x in 0..W {
            let dy = s.amp * sin(x * 3 + ph) / 127;
            for r in 0..s.h {
                let c = spx[(r * s.w + off + x) as usize];
                let y = s.y + dy + r;
                if c != KEY && (0..H).contains(&y) {
                    self.back[(y * W + x) as usize] = c;
                }
            }
        }
    }

    fn page_has_glows(&self) -> bool {
        self.m.glows.iter().any(|g| g.page == self.page) || self.m.gsqs.iter().any(|g| g.page == self.page)
    }

    fn gsq_frame(&self, g: usize, t: i64) -> i32 {
        let q = &self.m.gsqs[g];
        (t * q.mfps as i64 / 1_000_000 % q.frames.len() as i64) as i32
    }

    /// One frame of each glowseq on this page, added under the glow layers.
    /// Every span is bounds checked: a truncated or garbled frame draws
    /// what is valid and stops. Until the current frame has arrived, the
    /// last one drawn stays up; a bad frame draws nothing.
    fn draw_glowseq(&mut self, t: i64) {
        for g in 0..self.m.gsqs.len() {
            if self.m.gsqs[g].page != self.page {
                continue;
            }
            let f = self.gsq_frame(g, t);
            let idx = self.m.gsqs[g].frames[f as usize];
            self.s.gsq_frames += 1;
            match idx.map(|i| self.c.state(i)) {
                Some(State::Ready) => self.gsq_shown[g] = f,
                None | Some(State::Bad) => continue,
                _ => self.s.gsq_late += 1,
            }
            if self.gsq_shown[g] < 0 {
                continue;
            }
            let q = &self.m.gsqs[g];
            let Some((d, sz)) = self.c.bytes(q.frames[self.gsq_shown[g] as usize]) else { continue };
            if sz < 4 {
                continue;
            }
            let b = |o: usize| cache::byte(d, o);
            let word = |o: usize| u32::from_le_bytes([b(o), b(o + 1), b(o + 2), b(o + 3)]);
            let cnt = word(0);
            let mut off = 4;
            let mut k = 0;
            while k < cnt && off + 4 <= sz {
                let hd = word(off);
                let (start, len) = ((hd & 0x1FFFF) as usize, (hd >> 17) as usize);
                off += 4;
                let (wu, nu) = (W as usize, N);
                if len == 0 || len > wu || start + len > nu || start % wu + len > wu || off + len > sz {
                    break;
                }
                for j in 0..len {
                    let v = b(off + j);
                    if v != 0 {
                        add_light(&mut self.back[start + j], q.lut[v as usize], 8);
                    }
                }
                off += len;
                k += 1;
            }
        }
    }

    /// The whole slideshow page into the back buffer.
    fn compose_page(&mut self, t: i64) {
        let p = self.page as usize;
        let bgi = self.page_bgi(self.page);
        let (pano, pano_w, pano_speed) = (self.m.pages[p].pano, self.m.pages[p].pano_w, self.m.pages[p].pano_speed);
        if self.page_has_glows() {
            // Cached: blending is the expensive part.
            let key = t * FPS as i64 / 1000 / GLOW_DIV;
            if key != self.glow_key || self.glow_page != self.page {
                let tk = key * GLOW_DIV * 1000 / FPS as i64;
                // A panning base under the light layers.
                if let Some(px) = self.c.px(pano) {
                    pan_into_back(&mut self.back, px, pano_w, tk, pano_speed);
                } else {
                    self.back.copy_from_slice(bg_of(&self.c, &self.blank, bgi));
                }
                self.draw_glowseq(tk);
                self.draw_glows(tk);
                self.glowbuf.copy_from_slice(&self.back);
                self.glow_key = key;
                self.glow_page = self.page;
            } else {
                self.back.copy_from_slice(&self.glowbuf);
            }
        } else if let Some(px) = self.c.px(pano) {
            pan_into_back(&mut self.back, px, pano_w, t, pano_speed);
        } else {
            self.back.copy_from_slice(bg_of(&self.c, &self.blank, bgi));
        }
        let bg = bg_of(&self.c, &self.blank, bgi);
        for s in self.m.sprites.iter_mut().filter(|s| s.page == self.page) {
            s.shown = (t * s.fps as i64 / 1000 % s.frames as i64) as i32;
            put_keyed(&mut self.back, self.c.px(Some(s.px)), s.w, s.h, s.shown, s.x, s.y, Some(bg));
        }
        self.actors_place(t);
        for a in self.m.actors.iter_mut().filter(|a| a.page == self.page) {
            a.shown_f = Self::actor_frame(a, t);
            a.shown_x = a.x;
            a.shown_y = a.y;
            put_keyed(&mut self.back, self.c.px(Some(a.px)), a.w, a.h, a.shown_f, a.x, a.y, None);
        }
        // Panes and live boxes on the page.
        let on = self.page_target(self.page);
        for p in 0..self.m.panes.len() {
            if self.m.panes[p].on == on {
                self.draw_pane(p);
            }
        }
        self.draw_lives(on);
        let l0 = self.us();
        let was = self.fxs_cur.active;
        self.draw_logofx(t);
        self.fx_logo_us = self.us() - l0;
        if !self.fxs_cur.active && !was && self.m.logo.page == self.page {
            self.fx_hold_us += (self.fx_logo_us - self.fx_hold_us) / 16;
        }
        self.draw_scroller(t);
    }

    /// Advances the animated things on the current page; only changed
    /// rectangles reach the screen.
    fn animate_page(&mut self, t: i64) {
        if self.m.pages[self.page as usize].fullframe {
            // Everything overlaps: redraw it all.
            self.compose_page(t);
            self.blit(0, 0, W, H);
            return;
        }
        let bgi = self.page_bgi(self.page);
        let bg = bg_of(&self.c, &self.blank, bgi);
        let fb = self.plat.fb();
        for s in self.m.sprites.iter_mut().filter(|s| s.page == self.page) {
            let f = (t * s.fps as i64 / 1000 % s.frames as i64) as i32;
            if f != s.shown {
                s.shown = f;
                put_keyed(&mut self.back, self.c.px(Some(s.px)), s.w, s.h, f, s.x, s.y, Some(bg));
                blit(fb, &self.back, s.x, s.y, s.w, s.h);
            }
        }
        self.actors_place(t);
        let changed = self.m.actors.iter().any(|a| {
            a.page == self.page && (a.x != a.shown_x || a.y != a.shown_y || Self::actor_frame(a, t) != a.shown_f)
        });
        if !changed {
            return;
        }
        // Actors may overlap: erase all the old rectangles, draw all anew,
        // and copy each old and new one's union.
        let bg = bg_of(&self.c, &self.blank, bgi);
        for a in self.m.actors.iter().filter(|a| a.page == self.page) {
            restore(&mut self.back, bg, a.shown_x, a.shown_y, a.w, a.h);
        }
        for a in self.m.actors.iter().filter(|a| a.page == self.page) {
            put_keyed(&mut self.back, self.c.px(Some(a.px)), a.w, a.h, Self::actor_frame(a, t), a.x, a.y, None);
        }
        let fb = self.plat.fb();
        for a in self.m.actors.iter_mut().filter(|a| a.page == self.page) {
            let (x0, y0) = (a.x.min(a.shown_x), a.y.min(a.shown_y));
            let (x1, y1) = (a.x.max(a.shown_x) + a.w, a.y.max(a.shown_y) + a.h);
            blit(fb, &self.back, x0, y0, x1 - x0, y1 - y0);
            a.shown_x = a.x;
            a.shown_y = a.y;
            a.shown_f = Self::actor_frame(a, t);
        }
    }

    // ------------------------------------------------------------ screens

    fn var_value(&self, v: &str) -> i32 {
        match v {
            "music" => self.music_on,
            "station" => self.station,
            "night" => self.night,
            "nightmode" => self.nightmode,
            "pin" => self.pin,
            "page" => self.page,
            "app" => match self.m.pages.get(self.page as usize) {
                Some(p) if p.app >= 0 => p.app,
                _ => self.page,
            },
            "wifi" | "wifi_pct" => values::number(self.value(Key::parse(v).expect("a key"))),
            "secs" => {
                // 0 = auto, else 1 + the nearest option.
                let o = &self.m.secs_opt;
                if self.secs_override <= 0 || o.is_empty() {
                    return 0;
                }
                let mut best = 0;
                for i in 1..o.len() {
                    if (o[i] - self.secs_override).abs() < (o[best] - self.secs_override).abs() {
                        best = i;
                    }
                }
                best as i32 + 1
            }
            _ => 0,
        }
    }

    fn pane_frame(&self, p: usize) -> i32 {
        let pn = &self.m.panes[p];
        self.var_value(&pn.var).clamp(0, pn.frames - 1)
    }

    fn draw_pane(&mut self, p: usize) {
        let f = self.pane_frame(p);
        let pn = &mut self.m.panes[p];
        pn.shown = f;
        let Some(px) = self.c.px(Some(pn.px)) else { return };
        if !rect_ok(pn.x, pn.y, pn.w, pn.h) {
            return;
        }
        for r in 0..pn.h {
            let o = ((pn.y + r) * W + pn.x) as usize;
            let s = ((f * pn.h + r) * pn.w) as usize;
            self.back[o..o + pn.w as usize].copy_from_slice(&px[s..s + pn.w as usize]);
        }
    }

    fn draw_slider(&mut self, s: &Slider) {
        let (Some(empty), Some(full), Some(knob)) =
            (self.c.px(Some(s.empty)), self.c.px(Some(s.full)), self.c.px(Some(s.knob)))
        else {
            return;
        };
        if !rect_ok(s.x, s.y, s.w, s.h) || s.t0 < 0 || s.t1 > s.w {
            return;
        }
        let kx = s.t0 + (s.t1 - s.t0) * self.vol / 100;
        let ky = (s.h - s.kh) / 2;
        for r in 0..s.h {
            let o = ((s.y + r) * W + s.x) as usize;
            let row = (r * s.w) as usize;
            self.back[o..o + s.w as usize].copy_from_slice(&empty[row..row + s.w as usize]);
            let (a, b) = (s.t0 as usize, kx as usize);
            if b > a {
                self.back[o + a..o + b].copy_from_slice(&full[row + a..row + b]);
            }
        }
        for r in 0..s.kh {
            for c in 0..s.kw {
                let k = knob[(r * s.kw + c) as usize];
                let (x, y) = (kx - s.kw / 2 + c, ky + r);
                if k != KEY && (0..s.w).contains(&x) && (0..s.h).contains(&y) {
                    self.back[((s.y + y) * W + s.x + x) as usize] = k;
                }
            }
        }
    }

    fn screen_slider(&self) -> Option<Slider> {
        self.m.sliders.iter().find(|s| s.scr as i32 == self.scr).cloned()
    }

    fn compose_screen(&mut self) {
        let bg = self.m.screens[self.scr as usize].bg;
        self.back.copy_from_slice(bg_of(&self.c, &self.blank, Some(bg)));
        let on = Target::Screen(self.scr as usize);
        for p in 0..self.m.panes.len() {
            if self.m.panes[p].on == on {
                self.draw_pane(p);
            }
        }
        if let Some(s) = self.screen_slider() {
            self.draw_slider(&s);
        }
        self.draw_lives(on);
    }

    /// Redraws the panes whose state changed.
    fn refresh_panes(&mut self) {
        if self.scr < 0 {
            return;
        }
        for p in 0..self.m.panes.len() {
            let pn = &self.m.panes[p];
            if pn.on == Target::Screen(self.scr as usize) && self.pane_frame(p) != pn.shown {
                self.draw_pane(p);
                let pn = &self.m.panes[p];
                let (x, y, w, h) = (pn.x, pn.y, pn.w, pn.h);
                self.blit(x, y, w, h);
            }
        }
    }

    fn find_screen(&self, name: &str) -> i32 {
        self.m.find_screen(name).map_or(-1, |i| i as i32)
    }

    /// The screen show(name) would open: its _night variant at night.
    fn resolve_screen(&self, name: &str) -> i32 {
        let alt = if self.night != 0 { self.find_screen(&format!("{name}_night")) } else { -1 };
        if alt >= 0 { alt } else { self.find_screen(name) }
    }
}

impl<P: Plat> Engine<P> {
    // ------------------------------------------------------------ state

    fn st(&self, name: &str) -> PathBuf {
        self.plat.state_path(name)
    }

    fn station_url(&self) -> Option<String> {
        self.m.stations.get(self.station as usize).filter(|u| !u.is_empty()).cloned()
    }

    fn music_cmd(&mut self, on: bool) {
        let url = if on { self.station_url() } else { None };
        self.plat.music(url.as_deref(), self.vol);
    }

    fn set_music(&mut self, on: i32) {
        self.music_on = on;
        save_int(&self.st("wx_music"), on);
        self.music_cmd(on != 0);
    }

    fn set_night(&mut self, on: i32) {
        self.night = on;
        save_int(&self.st("wx_night"), on);
        self.plat.backlight(on != 0);
    }

    /// How long the current page shows: the Settings override, else its own.
    fn page_ms(&self) -> i64 {
        (if self.secs_override > 0 { self.secs_override } else { self.m.pages[self.page as usize].secs }) as i64 * 1000
    }

    fn step_secs(&mut self, d: i32) {
        let k = (self.var_value("secs") + d).clamp(0, self.m.secs_opt.len() as i32);
        self.secs_override = if k > 0 { self.m.secs_opt[k as usize - 1] } else { 0 };
        save_int(&self.st("wx_secs"), self.secs_override);
    }

    fn go_slideshow(&mut self, t: i64) {
        if self.m.pages.is_empty() {
            return;
        }
        let t0 = self.us();
        self.ret_page = -1;
        // The logo's page coming on (not a reload while it is up): a new
        // intro, after the wipe.
        if self.page == self.m.logo.page && !(self.scr < 0 && self.ss_page == self.page) {
            let which = if self.fxr.pend >= -1 { self.fxr.pend } else { -1 };
            self.fx_start(t + FX_WIPE_MS, which);
            self.fxr.pend = -2;
        }
        self.ss_page = self.page;
        self.scr = -1;
        self.clock_left = W;
        self.need(Kind::Page, self.page);
        self.read_values(t);
        self.compose_page(t);
        self.wipe = 1;
        self.plan_prefetch();
        self.s.trans_ms = (self.us() - t0) / 1000;
        self.s.trans_what = format!("page {}", self.page);
    }

    fn go_screen(&mut self, s: i32) {
        if s < 0 {
            return;
        }
        let t0 = self.us();
        self.scr = s;
        self.ss_page = -1;
        self.wipe = 0;
        self.need(Kind::Screen, s);
        let t = self.ms();
        self.read_values(t);
        self.compose_screen();
        self.blit(0, 0, W, H);
        self.plan_prefetch();
        self.s.trans_ms = (self.us() - t0) / 1000;
        self.s.trans_what = self.m.screens[s as usize].name.clone();
    }

    fn next_sun_change(&self, now: i64) -> i64 {
        state::next_sun_change(&self.m.sun, now)
    }

    /// Opens a screen by its base name: its _night variant at night.
    fn show(&mut self, name: &str) {
        self.scr_base = name.to_string();
        let s = self.resolve_screen(name);
        self.go_screen(s);
    }

    /// The current slideshow page again, at once (night mode changed its
    /// theme): no wipe.
    fn redraw_page(&mut self, t: i64) {
        if self.scr >= 0 || self.m.pages.is_empty() {
            return;
        }
        self.glow_key = -1;
        self.need(Kind::Page, self.page);
        self.compose_page(t);
        if self.wipe != 0 {
            return; // the running wipe shows the new one
        }
        self.blit(0, 0, W, H);
        self.clock_left = W;
        self.draw_clock();
        self.draw_ticker(t);
    }

    /// Night on or off, and everything on screen (and queued for
    /// prefetching) in the new theme.
    fn apply_night(&mut self, on: i32) {
        if on == self.night {
            return;
        }
        self.set_night(on);
        if self.scr >= 0 {
            let base = self.scr_base.clone();
            self.show(&base); // swap to the other theme
        } else {
            let t = self.ms();
            self.redraw_page(t);
            self.plan_prefetch(); // this page's and Home's other theme
        }
    }

    /// What night mode wants now, ignoring a tap's override; -1: no
    /// opinion (auto, without sun data).
    fn night_wanted(&self, now: i64) -> i32 {
        state::night_wanted(self.nightmode, &self.m.sun, now)
    }

    fn set_nightmode(&mut self, m: i32) {
        self.nightmode = m.rem_euclid(3);
        save_int(&self.st("wx_nightmode"), self.nightmode);
        self.set_override(0); // a new mode drops a tap's override
        let w = self.night_wanted(self.plat.wall());
        if w >= 0 {
            self.apply_night(w);
        }
    }

    fn set_page(&mut self, p: i32, t: i64) {
        let n = self.npages();
        if n == 0 {
            return;
        }
        self.ret_page = -1; // Home's < >: the page chosen wins
        self.page = (p % n + n) % n;
        if self.scr < 0 {
            self.go_slideshow(t);
        }
        self.page_start = t;
    }

    /// A tap zone's action. True: exit.
    fn do_action(&mut self, h: usize, t: i64) -> bool {
        let (act, name, arg) = {
            let h = &self.m.hots[h];
            (h.act.clone(), h.name.clone(), h.arg)
        };
        let ns = self.m.stations.len() as i32;
        match act.as_str() {
            "slideshow" => {
                // Back to the page a pagetap came from.
                if self.ret_page >= 0 && self.ret_page < self.npages() {
                    self.page = self.ret_page;
                }
                self.ret_page = -1;
                self.go_slideshow(t);
                self.page_start = t;
            }
            "screen" => self.show(&name),
            "chumby" => return true,
            "app" if !name.is_empty() => self.open_app(&name),
            "music_toggle" => self.set_music((self.music_on == 0) as i32),
            "station" if arg >= 0 && arg < ns => {
                self.station = arg;
                save_int(&self.st("wx_station"), self.station);
                self.set_music(1);
            }
            "next_station" if ns > 0 => {
                self.station = (self.station + 1) % ns;
                save_int(&self.st("wx_station"), self.station);
                if self.music_on != 0 {
                    self.music_cmd(true);
                }
            }
            "night" => {
                // For now: until the next sunrise or sunset.
                // wx_night first, then how long it holds: the service never
                // sees the override with the old state.
                let until = self.next_sun_change(self.plat.wall());
                self.apply_night((self.night == 0) as i32);
                self.set_override(until);
            }
            "nightmode" => self.set_nightmode(self.nightmode + 1),
            "pin" => self.pin = (self.pin == 0) as i32,
            "secs_up" => self.step_secs(1),
            "secs_down" => self.step_secs(-1),
            "next_app" => self.set_page(self.app_step(1), t),
            "prev_app" => self.set_page(self.app_step(-1), t),
            _ => {}
        }
        self.refresh_panes();
        false
    }

    /// A NIGHT tap's override, until `until` (0: none). Kept in
    /// wx_night_until too, for the service's backlight.
    fn set_override(&mut self, until: i64) {
        self.night_override_until = until;
        state::save_time(&self.st("wx_night_until"), until);
    }

    /// The waiting screen, with the built-in menu: a start app always
    /// offers CHUMBY MODE and a way to the other apps (proposal 0002), even
    /// before the feeder has sent the screens that do.
    fn wait_screen(&mut self) {
        let dir = self.base_dir.display().to_string();
        self.plat.message(&[
            "Waiting for pictures",
            &format!("in {dir}"),
            "",
            "Tap the top half for CHUMBY MODE,",
            "the bottom half for the apps.",
        ]);
    }

    /// The waiting screen's taps: one tap picks (top half: CHUMBY MODE;
    /// bottom: the apps, in neochumby-home), a second in the same half
    /// within 10 s does it, anywhere else goes back.
    fn fallback_menu(&mut self, t: i64) {
        if self.armed.is_some_and(|(_, until)| t > until) {
            self.armed = None;
            self.wait_screen();
        }
        while let Some(ev) = self.next_touch() {
            let Touch::Up(_, y) = ev else { continue };
            if t - self.last_tap < TAP_ECHO_MS {
                continue;
            }
            self.last_tap = t;
            let pick = if y < H / 2 { Fallback::ChumbyMode } else { Fallback::Apps };
            match self.armed.take() {
                Some((a, _)) if a == pick => {
                    match pick {
                        Fallback::ChumbyMode => self.ask_and_leave(Ask::ChumbyMode),
                        Fallback::Apps => self.open_app("home"),
                    }
                    if !self.leave {
                        self.wait_screen();
                    }
                    return;
                }
                Some(_) => self.wait_screen(),
                None => {
                    self.armed = Some((pick, t + 10_000));
                    self.plat.message(match pick {
                        Fallback::ChumbyMode => {
                            &["CHUMBY MODE?", "", "Tap the top half again", "to leave neochumby;", "elsewhere to stay."]
                        }
                        Fallback::Apps => {
                            &["The apps?", "", "Tap the bottom half again", "to open them;", "elsewhere to stay."]
                        }
                    });
                }
            }
        }
    }

    /// Asks the session for something, and leaves once it has asked.
    fn ask_and_leave(&mut self, a: Ask) {
        match self.session.as_mut().map(|s| s.ask(&a)) {
            Some(true) => self.leave = true,
            Some(false) => eprintln!("couldn't ask the session for {a:?}"),
            None => eprintln!("{a:?}: not started by the session"),
        }
    }

    /// Gives the engine the session's pipe; `reason_button`: the session
    /// opened it for a button press (NEOCHUMBY_REASON=button).
    pub fn set_session(&mut self, s: Box<dyn Session>, reason_button: bool) {
        self.session = Some(s);
        self.reason_button = reason_button;
    }

    /// Asks the session to open an app, and leaves once it has asked.
    fn open_app(&mut self, name: &str) {
        if !valid_app(name) {
            eprintln!("app: {name:?} isn't an app name");
            return;
        }
        match self.session.as_mut().map(|s| s.ask(&Ask::Open(name.to_string()))) {
            Some(true) => {
                eprintln!("app: asked for {name}");
                self.leave = true;
            }
            Some(false) => eprintln!("app: couldn't ask the session for {name}"),
            None => eprintln!("app: {name}: not started by the session, so nothing can open it"),
        }
    }

    /// Frees the picture cache: everything but what is on screen, or with
    /// `all`, everything (in the background, nothing is on screen). What is
    /// needed later is read again.
    pub fn drop_cache(&mut self, all: bool) {
        self.c.new_round();
        if !all {
            self.protect(Kind::Global, 0);
            self.protect(self.cur_kind(), self.cur_id());
        } else {
            self.loader_forget(None);
            self.glow_key = -1;
        }
        for i in 0..self.c.assets.len() {
            if self.c.holds(i) && (all || !self.c.protected(i)) {
                self.evict(i);
            }
        }
        if all {
            self.pf.clear();
        } else {
            self.plan_prefetch();
        }
    }

    // ------------------------------------------------------------ stats and control

    fn frame_stat(&mut self, work: i64, gap: i64, now: i64) {
        let w = &mut self.win;
        if w.start == 0 {
            w.start = now;
            w.work_min = 1 << 30;
        }
        w.frames += 1;
        w.work_sum += work;
        w.work_min = w.work_min.min(work);
        w.work_max = w.work_max.max(work);
        w.gap_max = w.gap_max.max(gap);
        if self.fxs_cur.active {
            // A logo intro drew this frame.
            let c = &mut self.fxs_cur;
            if c.frames == 0 {
                c.t0 = now;
                c.fx = self.fxr.fx;
            }
            c.frames += 1;
            c.sum += work;
            c.t1 = now;
            c.lsum += self.fx_logo_us;
            c.max = c.max.max(work);
            c.lmax = c.lmax.max(self.fx_logo_us);
            c.active = false;
        } else if self.fxs_cur.frames > 0 {
            self.fxs_commit();
        }
    }

    fn rss_kb() -> i64 {
        let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        s.find("VmRSS:").map_or(-1, |p| atoi(&s.as_bytes()[p + 6..]) as i64)
    }

    /// The stats file: key=value lines, replaced whole.
    fn write_stats(&self, why: &str) {
        let lw = &self.lastwin;
        let span = if lw.start != 0 { WIN_MS } else { 0 };
        let s = &self.s;
        let fx = &self.fxs_last;
        let out = format!(
            "why={why}\npid={}\nloader_pid={}\ntime={}\nmode={}\nview={}\npage={}\nnight={}\nnightmode={}\npin={}\nbench={}\n\
             fps_x10={}\nframe_work_us_min={}\nframe_work_us_avg={}\nframe_work_us_max={}\nframe_gap_us_max={}\n\
             rss_kb={}\ncache_kb={}\ncache_budget_kb={}\nassets_ready={}\nassets={}\nset_kb={}\n\
             hits={}\nmisses={}\nprefetched={}\nprefetch_chunks={}\nprefetch_kb={}\nread_kb={}\nevicted={}\nbad={}\n\
             chunk_us_avg={}\nchunk_us_max={}\nprefetch_pending={}\nload_us_avg={}\nload_us_max={}\nglowseq_frames={}\n\
             glowseq_late={}\nlast_transition={}\nlast_transition_ms={}\nlast_transition_load_ms={}\n\
             last_transition_hit={}\ndir={}\nfx_page={}\nfx={}\nfx_loop={}\nfx_force={}\nfx_last={}\nfx_last_frames={}\n\
             fx_last_work_us_avg={}\nfx_last_work_us_max={}\nfx_last_fps_x10={}\nfx_last_logo_us_avg={}\n\
             fx_last_logo_us_max={}\nfx_hold_logo_us={}\n",
            std::process::id(),
            // The loader is a thread of this process.
            if self.loader.is_some() { std::process::id() as i64 } else { -1 },
            self.plat.wall(),
            if self.scr >= 0 { "screen" } else { "slideshow" },
            if self.scr >= 0 { self.m.screens[self.scr as usize].name.as_str() } else { "-" },
            self.page,
            self.night,
            ["auto", "on", "off"][self.nightmode.clamp(0, 2) as usize],
            self.pin,
            self.bench as i32,
            if span != 0 && lw.frames != 0 { lw.frames * 10000 / span } else { 0 },
            if lw.frames != 0 { lw.work_min } else { 0 },
            if lw.frames != 0 { lw.work_sum / lw.frames } else { 0 },
            lw.work_max,
            lw.gap_max,
            Self::rss_kb(),
            self.c.bytes / 1024,
            self.c.budget / 1024,
            self.c.ready(),
            self.c.assets.len(),
            self.c.total() / 1024,
            s.hits,
            s.misses,
            s.prefetched,
            s.chunks,
            s.prefetch_bytes / 1024,
            s.read_bytes / 1024,
            s.evicted,
            s.bad,
            s.chunk_us_avg,
            s.chunk_us_max,
            (!self.pf.is_empty() && !self.pf_stalled) as i32,
            s.load_us_avg,
            s.load_us_max,
            s.gsq_frames,
            s.gsq_late,
            s.trans_what,
            s.trans_ms,
            s.trans_need_ms,
            s.trans_hit as i32,
            self.loaded_dir.display(),
            self.m.logo.page,
            logo::name(self.fxr.fx),
            self.fxr.looping,
            if self.m.logo.force >= 0 { logo::name(self.m.logo.force) } else { "random" },
            logo::name(fx.fx),
            fx.frames,
            fx.avg,
            fx.max,
            fx.fps_x10,
            fx.logo_avg,
            fx.logo_max,
            self.fx_hold_us,
        );
        let Some(path) = &self.stat_path else { return };
        let _ = state::write_setting(path, out.as_bytes());
    }

    fn end_window(&mut self, now: i64) {
        if self.win.start != 0 && now - self.win.start >= WIN_MS {
            self.lastwin = self.win;
            self.win = Win::default();
            if self.bench {
                self.write_stats("bench");
            }
        }
    }

    /// The next touch event: injected ones (`tap X Y`) first.
    fn next_touch(&mut self) -> Option<Touch> {
        self.inj.pop_front().or_else(|| self.plat.touch())
    }

    /// One control line; anything malformed is ignored.
    pub fn ctl_line(&mut self, l: &[u8], t: i64) {
        let v = scan(l, "%15s %48s");
        let Some(w) = v.first().map(|w| w.s().to_string()) else { return };
        let a1 = v.get(1).map(|a| a.s().to_string()).unwrap_or_default();
        self.last_touch = t; // a remote poke counts as a touch
        let n = self.npages();
        match w.as_str() {
            "show" if !a1.is_empty() => self.show(&a1),
            "page" if !a1.is_empty() && n > 0 => {
                let p = atoi(a1.as_bytes());
                if p >= 0 && p < n {
                    self.page = p;
                    self.scr = -1;
                    self.go_slideshow(t);
                    self.page_start = t;
                }
            }
            "pin" => {
                self.pin = match a1.as_str() {
                    "on" => 1,
                    "off" => 0,
                    _ => (self.pin == 0) as i32,
                };
                self.refresh_panes();
            }
            "night" => {
                let on = match a1.as_str() {
                    "on" => 1,
                    "off" => 0,
                    _ => (self.night == 0) as i32,
                };
                if on != self.night {
                    let until = self.next_sun_change(self.plat.wall());
                    self.apply_night(on);
                    self.set_override(until);
                }
            }
            "nightmode" => {
                let m = match a1.as_str() {
                    "auto" => 0,
                    "on" => 1,
                    "off" => 2,
                    _ => self.nightmode + 1,
                };
                self.set_nightmode(m);
                self.refresh_panes();
            }
            "tap" => {
                let xy = scan(l, "%*s %d %d");
                if xy.len() == 2 {
                    let (x, y) = (xy[0].i(), xy[1].i());
                    if (0..W).contains(&x) && (0..H).contains(&y) {
                        self.inj.push_back(Touch::Move(x, y));
                        self.inj.push_back(Touch::Up(x, y));
                    }
                }
            }
            "button" => self.inj_button = true,
            "stats" => self.write_stats("stats"),
            "bench" => {
                self.bench = a1 != "off";
                if self.bench {
                    // Fresh counters for the run.
                    let s = &mut self.s;
                    (s.hits, s.misses, s.prefetched, s.bad, s.evicted, s.chunks) = (0, 0, 0, 0, 0, 0);
                    (s.read_bytes, s.prefetch_bytes) = (0, 0);
                    s.chunk_us_max = 0;
                    (s.load_us_max, s.load_us_avg) = (0, 0);
                    (s.gsq_frames, s.gsq_late) = (0, 0);
                }
                self.write_stats("bench");
            }
            "reload" => self.want_reload = true,
            "fx" if !a1.is_empty() && self.m.logo.page >= 0 && self.m.logo.page < n => {
                if a1 == "loop" {
                    // fx loop on|all|off
                    let a2 = scan(l, "%*s %*s %7s");
                    self.fxr.looping = match a2.first().map(|a| a.s()) {
                        Some("all") => 2,
                        Some("off") => 0,
                        _ => 1,
                    };
                    return;
                }
                let mut k = if a1 == "next" { (self.fxr.fx + 1) % NFX } else { fx_parse(&a1) };
                if k == -1 {
                    k = (self.fx_rand() % NFX as u32) as i32; // random, even over a forced one
                }
                if k < 0 {
                    return;
                }
                if self.scr < 0 && self.page == self.m.logo.page {
                    self.fx_start(t, k);
                } else {
                    self.fxr.pend = k;
                    self.page = self.m.logo.page;
                    self.scr = -1;
                    self.ss_page = -1;
                    self.go_slideshow(t);
                    self.page_start = t;
                }
            }
            // Testing: forget all but what is on screen (`drop all`: and that,
            // until it is next drawn).
            "drop" => self.drop_cache(a1 == "all"),
            "app" if !a1.is_empty() => self.open_app(&a1),
            _ => {}
        }
    }

    /// Runs the lines appended to the control file since the last look.
    fn poll_ctl(&mut self, t: i64) {
        let Some(path) = &self.ctl_path else { return };
        if std::fs::symlink_metadata(path).is_err() {
            return;
        }
        let work = path.with_extension("work");
        // Whoever appends next starts a new file.
        if std::fs::rename(path, &work).is_err() {
            return;
        }
        let buf = read_nofollow(&work, 8192);
        let _ = std::fs::remove_file(&work);
        for l in crate::scan::lines(&buf.unwrap_or_default()) {
            self.ctl_line(l, t);
        }
    }

    // ------------------------------------------------------------ the frame loop

    /// Loads the manifest and the persisted state, and shows the first
    /// page. Err if there is nothing to show.
    pub fn start(&mut self) {
        self.loader = Loader::start();
        if self.loader.is_none() {
            eprintln!("no loader thread: loading synchronously");
        }
        if self.load().is_ok() {
            self.begin();
            return;
        }
        // Nothing to show yet (the pictures haven't been delivered): say so,
        // and look again every second.
        self.waiting = true;
        self.wait_screen();
        let now = self.ms();
        self.last_check = now;
        self.next = now;
    }

    /// The persisted state, and the first page.
    fn begin(&mut self) {
        self.vol = read_int(&self.st("wx_volume"), 65);
        if !(0..=100).contains(&self.vol) {
            self.vol = 65;
        }
        self.station = read_int(&self.st("wx_station"), 0);
        if self.station < 0 || self.station >= (self.m.stations.len() as i32).max(1) {
            self.station = 0;
        }
        self.nightmode = read_int(&self.st("wx_nightmode"), 0);
        if !(0..=2).contains(&self.nightmode) {
            self.nightmode = 0;
        }
        // A NIGHT tap's override still running; else the mode, or the sun;
        // else as last time (state::night_now, as the service decides it).
        self.night_override_until = state::read_time(&self.st("wx_night_until"));
        let last = read_int(&self.st("wx_night"), 0) == 1;
        let night = state::night_now(self.nightmode, &self.m.sun, self.plat.wall(), self.night_override_until, last);
        self.set_night(night as i32);
        self.music_on = (read_int(&self.st("wx_music"), 0) == 1) as i32;
        self.secs_override = read_int(&self.st("wx_secs"), 0);
        if !(0..=3600).contains(&self.secs_override) {
            self.secs_override = 0;
        }
        if self.music_on != 0 {
            self.music_cmd(true); // what was playing last time
        }
        let now = self.ms();
        self.last_check = now;
        self.next = now;
        self.frame_prev = self.us();
        self.page_start = now;
        self.last_touch = now;
        self.go_slideshow(now);
        // Opened for a button press the session couldn't pass on: the
        // press means Home, as from the slideshow.
        if std::mem::take(&mut self.reason_button) && self.find_screen("home") >= 0 {
            self.show("home");
        }
    }

    /// Runs frames until an exit (Home's CHUMBY) or, on the host, the end
    /// of the script.
    pub fn run(&mut self) {
        while self.frame() {}
    }

    /// One frame. False: stop.
    fn frame(&mut self) -> bool {
        let f0 = self.us();
        let t = self.ms();

        if self.waiting {
            // Control commands still work (stats, reload...); with no
            // pages, those that need one do nothing.
            self.frame_no = self.frame_no.wrapping_add(1);
            if self.frame_no.is_multiple_of(5) {
                self.poll_ctl(t);
            }
            self.fallback_menu(t);
            if self.leave {
                return false;
            }
            if t - self.last_check >= 1000 || std::mem::take(&mut self.want_reload) {
                self.last_check = t;
                if self.load().is_ok() {
                    self.waiting = false;
                    self.armed = None;
                    self.inj.clear();
                    self.inj_button = false;
                    self.begin();
                    return true;
                }
            }
            let more = self.plat.present();
            self.plat.sleep_us(1_000_000 / FPS as i64);
            return more;
        }

        self.frame_no = self.frame_no.wrapping_add(1);
        if self.frame_no.is_multiple_of(5) {
            self.poll_ctl(t); // 5 Hz: one stat() on tmpfs
        }
        if self.leave {
            return false; // an app was asked for
        }

        // The top button: slideshow <-> Home on each press. It is
        // level-triggered: act on the 0 -> 1 edge.
        // From the session, a press is a `button` line: the same as a press.
        if let Some(s) = &mut self.session {
            for h in s.heard() {
                match h {
                    Heard::Button => self.inj_button = true,
                }
            }
        }
        let bl = self.plat.button();
        if (bl == Some(true) && !self.button_last) || self.inj_button {
            self.inj_button = false;
            self.last_touch = t;
            if self.scr < 0 {
                self.show("home");
            } else {
                self.go_slideshow(t);
            }
        }
        if let Some(b) = bl {
            self.button_last = b;
        }

        while let Some(ev) = self.next_touch() {
            self.last_touch = t;
            let (tx, ty) = match ev {
                Touch::Move(x, y) | Touch::Up(x, y) => (x, y),
            };
            (self.tx, self.ty) = (tx, ty);
            if let Touch::Move(..) = ev {
                if let Some(s) = self.screen_slider().filter(|_| self.scr >= 0)
                    && ty >= s.y - 6
                    && ty < s.y + s.h + 6
                    && tx >= s.x + s.t0 - 14
                    && tx <= s.x + s.t1 + 14
                {
                    self.vol = ((tx - s.x - s.t0) * 100 / (s.t1 - s.t0)).clamp(0, 100);
                    self.dragging = true;
                    self.draw_slider(&s);
                    self.blit(s.x, s.y, s.w, s.h);
                }
                continue;
            }
            // Released.
            if self.dragging {
                // A volume drag finished.
                self.dragging = false;
                self.plat.volume(self.vol);
                save_int(&self.st("wx_volume"), self.vol);
            } else if t - self.last_tap < TAP_ECHO_MS {
                // One tap can arrive as two; ignore the echo (on slideshow
                // pages too, where it used to skip two pages).
            } else if self.scr >= 0 {
                self.last_tap = t;
                let hit = self.m.hots.iter().position(|h| {
                    h.scr as i32 == self.scr && tx >= h.x && tx < h.x + h.w && ty >= h.y && ty < h.y + h.h
                });
                if let Some(h) = hit
                    && self.do_action(h, t)
                {
                    // The service stops the music (on the session's SIGTERM).
                    println!("chumby: back to the Chumby");
                    self.set_night(0);
                    if let Some(s) = &mut self.session {
                        s.ask(&Ask::ChumbyMode);
                    }
                    return false;
                }
            } else if tx < 64 && ty < 40 && self.find_screen("home") >= 0 {
                self.last_tap = t;
                self.show("home"); // the logo box
            } else if let Some(tap) = self.m.pages.get(self.page as usize).map(|p| p.tap.clone())
                && !tap.is_empty()
                && self.find_screen(&tap) >= 0
            {
                self.last_tap = t; // its echo must not hit the screen's tap zones
                self.ret_page = self.page;
                self.show(&tap); // pagetap: this page's app
            } else {
                self.last_tap = t;
                self.page_start = 0; // anywhere else: the next page
            }
        }

        if self.leave {
            return false; // an app was asked for
        }

        // New pictures? (a new manifest, or `current` naming another set)
        if t - self.last_check > 5000 || self.want_reload {
            let nd = resolve_dir(&self.base_dir);
            let changed = std::fs::metadata(nd.join("manifest.txt"))
                .is_ok_and(|m| mtime(&m) != self.manifest_mtime || nd != self.loaded_dir);
            if self.want_reload || changed {
                if nd == self.base_dir {
                    self.plat.sleep_us(300_000); // the old layout: let the push finish
                }
                let cur = if self.scr >= 0 { Some(self.scr_base.clone()) } else { None };
                if self.load().is_ok() {
                    self.page %= self.npages();
                    if self.station >= self.m.stations.len() as i32 {
                        self.station = 0;
                    }
                    match cur.filter(|c| self.find_screen(c) >= 0) {
                        Some(c) => self.show(&c),
                        None => self.go_slideshow(t),
                    }
                }
            }
            self.want_reload = false;
            self.last_check = t;
        }

        // Night mode: auto follows the sun, on and off hold it; a NIGHT tap
        // overrides either until the next sun change.
        let wall = self.plat.wall();
        if wall != self.last_sun_check {
            self.last_sun_check = wall;
            let dk = self.night_wanted(wall);
            if dk >= 0 && wall >= self.night_override_until && dk != self.night {
                self.apply_night(dk);
            }
        }

        if self.scr >= 0 {
            if !self.bench && t - self.last_touch > SCREEN_IDLE_MS {
                self.go_slideshow(t);
            }
        } else if !self.m.pages.is_empty() {
            if self.wipe == 0 && self.pin == 0 && t - self.page_start > self.page_ms() {
                self.page = (self.page + 1) % self.npages();
                self.go_slideshow(t);
            }
            if self.wipe != 0 {
                // A 90s top-down wipe.
                self.blit(0, 0, W, H * self.wipe / WIPE_FRAMES);
                self.wipe += 1;
                if self.wipe > WIPE_FRAMES {
                    self.wipe = 0;
                    self.page_start = t;
                }
            } else {
                self.animate_page(t);
                let w = self.plat.wall();
                if w != self.last_sec {
                    self.last_sec = w;
                    self.draw_clock();
                }
            }
            self.draw_ticker(t);
        }
        if self.scr >= 0 || (self.wipe == 0 && !self.m.pages.is_empty()) {
            if t - self.vals_t >= VALUES_MS {
                self.read_values(t);
            }
            self.live_update();
        }

        let more = self.plat.present();
        let f1 = self.us();
        self.frame_stat(f1 - f0, f0 - self.frame_prev, t);
        self.frame_prev = f0;
        self.end_window(t);
        if !more {
            return false;
        }

        self.next += 1000 / FPS as i64;
        self.prefetch_idle(self.next * 1000); // read ahead in what is left of the frame
        let sleep = self.next - self.ms();
        if sleep > 0 {
            self.plat.sleep_us(sleep * 1000);
        } else {
            self.next = self.ms(); // fell behind: don't try to catch up
        }
        true
    }
}

impl<P: Plat> Engine<P> {
    // ------------------------------------------------------------ pageapp

    /// Which app a page belongs to (pageapp); a page without one is an app
    /// of its own.
    fn app_of(&self, p: i32) -> i32 {
        let a = self.m.pages[p as usize].app;
        if a >= 0 { a } else { 1000 + p }
    }

    /// The first page of the next (1) or previous (-1) app.
    fn app_step(&self, dir: i32) -> i32 {
        let n = self.npages();
        if n == 0 {
            return 0;
        }
        let cur = self.app_of(self.page);
        let mut i = self.page;
        for _ in 0..n {
            i = (i + dir + n) % n;
            if self.app_of(i) != cur {
                break;
            }
        }
        if self.app_of(i) == cur {
            return (self.page + dir + n) % n; // one app only
        }
        // Back to that app's first page.
        let mut k = 0;
        while k < n && self.app_of((i + n - 1) % n) == self.app_of(i) {
            i = (i + n - 1) % n;
            k += 1;
        }
        i
    }

    // ------------------------------------------------------------ live values

    /// What page p's panes and live boxes are drawn for now: its pagenight
    /// ones while it shows its night file, if it has any.
    fn page_target(&self, p: i32) -> Target {
        let p = p.max(0) as usize;
        let night = Target::PageNight(p);
        let has = self.m.panes.iter().any(|x| x.on == night) || self.m.lives.iter().any(|l| l.on == night);
        if self.pnight(p as i32) && has { night } else { Target::Page(p) }
    }

    /// What the view on screen draws panes and boxes for.
    fn view(&self) -> Option<Target> {
        if self.scr >= 0 {
            Some(Target::Screen(self.scr as usize))
        } else if self.m.pages.is_empty() {
            None
        } else {
            Some(self.page_target(self.page))
        }
    }

    /// The device values the view uses.
    fn view_keys(&self) -> Vec<Key> {
        let Some(on) = self.view() else { return Vec::new() };
        let mut k: Vec<Key> = self
            .m
            .panes
            .iter()
            .filter(|p| p.on == on)
            .filter_map(|p| Key::parse(&p.var).filter(|k| k.numeric()))
            .chain(self.m.lives.iter().filter(|l| l.on == on).map(|l| l.key))
            .collect();
        let mut seen = Vec::new();
        k.retain(|x| {
            let new = !seen.contains(x);
            seen.push(*x);
            new
        });
        k
    }

    /// Reads the values the view uses (none if it uses none).
    fn read_values(&mut self, t: i64) {
        self.vals_t = t;
        for k in self.view_keys() {
            self.vals[k.index()] = self.plat.device_value(k);
        }
    }

    fn value(&self, k: Key) -> Option<&str> {
        self.vals[k.index()].as_deref()
    }

    /// Redraws the panes and boxes in view whose device value changed.
    fn live_update(&mut self) {
        let Some(on) = self.view() else { return };
        for p in 0..self.m.panes.len() {
            let pn = &self.m.panes[p];
            if pn.on == on && Key::parse(&pn.var).is_some_and(|k| k.numeric()) && self.pane_frame(p) != pn.shown {
                self.draw_pane(p);
                let pn = &self.m.panes[p];
                let (x, y, w, h) = (pn.x, pn.y, pn.w, pn.h);
                self.blit(x, y, w, h);
            }
        }
        for i in 0..self.m.lives.len() {
            let l = &self.m.lives[i];
            if l.on == on && self.live_shown[i].as_deref() != Some(self.value(l.key).unwrap_or("?")) {
                self.draw_live(i);
                let l = &self.m.lives[i];
                let (x, y, w, h) = (l.x, l.y, l.w, l.h);
                self.blit(x, y, w, h);
            }
        }
    }

    fn draw_lives(&mut self, on: Target) {
        for i in 0..self.m.lives.len() {
            if self.m.lives[i].on == on {
                self.draw_live(i);
            }
        }
    }

    /// One live box into the back buffer: cleared, then its text.
    fn draw_live(&mut self, i: usize) {
        let l = &self.m.lives[i];
        let text = self.value(l.key).unwrap_or("?").to_string();
        let bgi = match l.on {
            Target::Screen(s) => Some(self.m.screens[s].bg),
            Target::Page(p) | Target::PageNight(p) => self.page_bgi(p as i32),
        };
        let (x, y, w, h) = (l.x, l.y, l.w, l.h);
        for r in y..y + h {
            let o = (r * W + x) as usize;
            let row = &mut self.back[o..o + w as usize];
            match l.bg {
                Some(c) => row.fill(c),
                None => row.copy_from_slice(&bg_of(&self.c, &self.blank, bgi)[o..o + w as usize]),
            }
        }
        let f = &self.m.fonts[l.font];
        if let Some(strip) = self.c.px(Some(f.px)) {
            let glyphs: Vec<usize> = text.chars().map(values::glyph).collect();
            let tw: i32 = glyphs.iter().map(|&g| f.wd[g]).sum();
            let mut pen = match l.align {
                Align::Left => x,
                Align::Right => x + w - tw,
                Align::Center => x + (w - tw) / 2,
            };
            for g in glyphs {
                let gw = f.wd[g];
                for r in 0..f.ch.min(h) {
                    let src = ((g as i32 * f.ch + r) * f.cw) as usize;
                    for c in 0..gw {
                        let xx = pen + c;
                        let p = strip[src + c as usize];
                        if xx >= x && xx < x + w && p != KEY {
                            self.back[((y + r) * W + xx) as usize] = p;
                        }
                    }
                }
                pen += gw;
            }
        }
        self.live_shown[i] = Some(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::Host;
    use std::cell::RefCell;
    use std::path::Path;
    use std::rc::Rc;

    /// An engine on `manifest` and files of the given sizes (filled with a
    /// pattern), with this budget, started.
    fn hostile(tag: &str, manifest: &str, files: &[(&str, usize)], budget_kb: usize) -> (Engine<Host>, PathBuf) {
        let d = std::env::temp_dir().join(format!("wx-hostile-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("p.raw"), vec![0x21u8; manifest::FRAME_BYTES]).unwrap();
        for (name, size) in files {
            let data: Vec<u8> = (0..*size).map(|i| (i * 7 + 3) as u8).collect();
            std::fs::write(d.join(name), data).unwrap();
        }
        std::fs::write(d.join("manifest.txt"), format!("page 0 p.raw 12\nscreen home p.raw\n{manifest}")).unwrap();
        let h = Host::new("unused", vec![1 << 40], 1793150000, d.clone());
        let mut e = Engine::new(h, d.clone());
        e.c.budget = budget_kb * 1024;
        e.start();
        (e, d)
    }

    /// Runs frames through the slideshow, every intro and a screen,
    /// checking the memory rules all along.
    fn survive(e: &mut Engine<Host>, d: &Path) {
        let budget = e.c.budget;
        for k in 0..300 {
            match k {
                10 => e.ctl_line(b"fx loop all", e.ms()),
                100 => e.ctl_line(b"show home", e.ms()),
                120 => e.ctl_line(b"tap 5 5", e.ms()),
                140 => e.ctl_line(b"page 0", e.ms()),
                200 => e.ctl_line(b"night on", e.ms()),
                _ => {}
            }
            assert!(e.frame());
            assert!(e.c.assets.iter().all(|a| a.size <= budget), "an asset over the budget");
            assert!(e.c.bytes <= ceiling(budget), "the cache past its ceiling");
            let held: usize = (0..e.c.assets.len()).filter(|&i| e.c.holds(i)).map(|i| e.c.assets[i].size).sum();
            assert_eq!(held, e.c.bytes, "the cache's count is off");
        }
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn hostile_sizes_overflow_and_negatives() {
        let (e, d) = hostile(
            "sizes",
            "sprite 0 big.raw 65536 32768 1 0 0 8\n\
             sprite 0 neg.raw -4 -4 2 0 0 8\n\
             actor 0 a.raw 4 4 2 -5 path.raw 2\n\
             actor 0 a.raw 4 4 2 0 path.raw 2\n\
             pane home pn.raw 4 4 2147483000 0 page 1\n\
             pane home pn.raw 4 4 316 236 page 1\n\
             hot home 2147483000 0 2147483000 10 slideshow\n\
             slider home e.raw e.raw k.raw 4 4 -2147483000 0 0 3 2 2\n\
             page 1 p.raw 99999999999999\n\
             glow 0 g.raw 4 4 0 1 -9000 0 0 0 0 1\n\
             glow 0 g.raw 4 4 0 1 9000 -5000 3 7 1 1\n\
             scroller 0 s.raw 400 4 230 -3 2147483000\n\
             scroller 0 s.raw 400 4 230 -3 5\n",
            &[
                ("big.raw", 4),
                ("neg.raw", 64),
                ("a.raw", 64),
                ("path.raw", 8),
                ("pn.raw", 32),
                ("e.raw", 32),
                ("k.raw", 8),
                ("g.raw", 32),
                ("s.raw", 400 * 4 * 2),
            ],
            512,
        );
        assert!(e.m.sprites.is_empty(), "overflowing and negative sizes are skipped");
        assert_eq!(e.m.actors.len(), 1, "a negative fps is skipped; 0 is a still frame");
        assert_eq!(e.m.panes.len(), 1);
        assert!(e.m.hots.is_empty() && e.m.sliders.is_empty());
        assert_eq!(e.m.glows.len(), 1, "a negative cycle is skipped");
        assert_eq!(e.m.scroller.amp, 5);
        assert!(!rect_ok(i32::MAX, 0, 10, 10) && !rect_ok(0, 0, -1, 1) && rect_ok(310, 230, 10, 10));
        let mut e = e;
        survive(&mut e, &d);
    }

    #[test]
    fn hostile_clock_and_bigger_than_the_budget() {
        let wide = 30000usize;
        let (mut e, d) = hostile(
            "clock",
            "clock c.raw 4 4 300 2 -100 -5 99999 4 4 4 4 4 4 4 4 4 4 4 4\n\
             bgscroll 0 pano.raw 30000 2\n\
             glowseq 0 g 3 12 255 255 255 255\n\
             logofx 0 logo.raw 32767 1 0 100 spin\n",
            &[
                ("c.raw", 4 * 4 * 15 * 2),
                ("pano.raw", wide * 240 * 2),
                ("g_000.l8", 600 * 1024),
                ("g_001.l8", 5000),
                ("logo.raw", 32767 * 2),
            ],
            512,
        );
        assert_eq!(&e.m.clock.wd[..4], &[0, 0, 4, 4], "glyph widths are clamped to the cell");
        assert!(e.m.pages[0].pano.is_none(), "a panorama bigger than the budget is refused");
        let g = &e.m.gsqs[0];
        assert!(g.frames[0].is_none() && g.frames[1].is_some() && g.frames[2].is_none());
        assert!(e.m.logo.px.is_some());
        survive(&mut e, &d);
    }

    #[test]
    fn hostile_night_strips_after_a_redefinition() {
        // clocknight and tickernight sized for a small clock and ticker,
        // then both redefined bigger; then night.
        let (mut e, d) = hostile(
            "redef",
            "clock c.raw 4 4 300 10 4 4 4 4 4 4 4 4 4 4 4 4 4 4 4\n\
             ticker t.raw 400 4 230 2\n\
             clocknight cn.raw\n\
             tickernight tn.raw\n\
             clock c2.raw 20 20 300 10 20 20 20 20 20 20 20 20 20 20 20 20 20 20 20\n\
             ticker t2.raw 800 8 230 2\n\
             pagenight 0 p.raw\n\
             sprite 0 x.raw 1 1 1 0 0 8\n\
             glowseq 0 g 1 12 99999999 -99999999 2147483647 255\n\
             page 1 p.raw -5\n",
            &[
                ("c.raw", 4 * 4 * 15 * 2),
                ("cn.raw", 4 * 4 * 15 * 2),
                ("t.raw", 400 * 4 * 2),
                ("tn.raw", 400 * 4 * 2),
                ("c2.raw", 20 * 20 * 15 * 2),
                ("t2.raw", 800 * 8 * 2),
                ("x.raw", 2),
                ("g_000.l8", 12),
            ],
            512,
        );
        assert!(e.m.clock.pxn.is_none() && e.m.ticker.pxn.is_none(), "a redefinition drops the night strips");
        assert_eq!(e.m.gsqs[0].lut[255], 0xF800 | 0x001F, "R and B saturate, G is 0");
        assert_eq!(e.m.pages[1].secs, 1, "secs of 0 or less is a second");
        // And if a stale strip got through anyway, drawing checks its size.
        e.m.clock.pxn = e.c.assets.iter().position(|a| a.name == "cn.raw");
        e.m.ticker.pxn = e.c.assets.iter().position(|a| a.name == "tn.raw");
        e.ctl_line(b"night on", e.ms());
        survive(&mut e, &d);
    }

    #[test]
    fn a_reload_without_a_page_keeps_the_last_set() {
        let d = std::env::temp_dir().join(format!("classic-nopage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("p.raw"), vec![0x44u8; manifest::FRAME_BYTES]).unwrap();
        std::fs::write(d.join("manifest.txt"), "page 0 p.raw 12 noclock noticker\nscreen home p.raw\n").unwrap();
        let h = Host::new("unused", vec![1 << 40], 0, d.clone());
        let mut e = Engine::new(h, d.clone());
        e.start();
        for _ in 0..15 {
            assert!(e.frame());
        }
        let bytes = e.c.bytes;
        assert!(bytes > 0);
        // MAXASSET other files, then the only page: usable() sees the page,
        // parsing never keeps it.
        // (glowseq frames: the one kind of file with no lower limit.)
        let mut m = String::new();
        for k in 0..MAXASSET.div_ceil(1000) {
            for f in 0..1000 {
                std::fs::write(d.join(format!("g{k}_{f:03}.l8")), [0u8; 4]).unwrap();
            }
            m.push_str(&format!("glowseq 0 g{k} 1000 12 1 1 1 1\n"));
        }
        m.push_str("page 0 p.raw 12\n");
        std::fs::write(d.join("manifest.txt"), m).unwrap();
        e.ctl_line(b"reload", e.ms());
        for _ in 0..30 {
            assert!(e.frame());
        }
        assert_eq!(e.m.pages.len(), 1, "the last good set stays up");
        assert_eq!(e.m.screens.len(), 1);
        assert_eq!(e.c.assets.len(), 1);
        assert_eq!(e.c.bytes, bytes);
        assert!(e.plat.fb().iter().all(|&p| p == 0x4444));
        e.ctl_line(b"show home", e.ms());
        e.ctl_line(b"page 0", e.ms());
        for _ in 0..30 {
            assert!(e.frame());
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn many_big_files_stay_under_the_ceiling() {
        let mut m = String::new();
        let mut files = Vec::new();
        let names: Vec<String> = (0..24).map(|i| format!("s{i}.raw")).collect();
        for n in &names {
            // 250x250x4 frames x 2 bytes = 500,000 bytes each: 12 MB in all.
            m.push_str(&format!("sprite 0 {n} 250 250 4 0 0 8\n"));
            files.push((n.as_str(), 500_000));
        }
        let (mut e, d) = hostile("many", &m, &files, 512);
        assert_eq!(e.m.sprites.len(), 24);
        assert!(e.c.bytes <= ceiling(e.c.budget));
        survive(&mut e, &d);
    }

    #[test]
    fn waits_for_a_manifest() {
        let d = std::env::temp_dir().join(format!("wx-wait-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let h = Host::new("unused", vec![1 << 40], 0, d.clone());
        let mut e = Engine::new(h, d.clone());
        e.start();
        for _ in 0..30 {
            assert!(e.frame());
        }
        assert!(e.waiting);
        // Control commands are taken while waiting, harmlessly.
        for c in ["page 3", "show home", "night on", "drop all", "pin on", "fx spin", "tap 5 5", "button", "stats"] {
            e.ctl_line(c.as_bytes(), e.ms());
            assert!(e.frame());
        }
        std::fs::write(d.join("p.raw"), vec![0x77u8; manifest::FRAME_BYTES]).unwrap();
        std::fs::write(d.join("manifest.txt"), "page 0 p.raw 12 noclock noticker\n").unwrap();
        for _ in 0..40 {
            assert!(e.frame());
        }
        assert!(!e.waiting, "it starts once the pictures are there");
        assert!(e.plat.fb().iter().all(|&p| p == 0x7777));
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A 2x2 font whose glyph g is solid colour g + 1, except a transparent
    /// top-left pixel.
    fn font() -> Vec<u8> {
        let mut v = Vec::new();
        for g in 0..values::NGLYPHS as u16 {
            for k in 0..4 {
                v.extend_from_slice(&(if k == 0 { KEY } else { g + 1 }).to_le_bytes());
            }
        }
        v
    }

    fn px(e: &mut Engine<Host>, x: i32, y: i32) -> u16 {
        e.plat.fb()[(y * W + x) as usize]
    }

    #[test]
    fn live_values_drawn_and_redrawn() {
        let d = std::env::temp_dir().join(format!("classic-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("p.raw"), vec![0x11u8; manifest::FRAME_BYTES]).unwrap();
        std::fs::write(d.join("pn.raw"), vec![0x22u8; manifest::FRAME_BYTES]).unwrap();
        std::fs::write(d.join("home.raw"), vec![0x33u8; manifest::FRAME_BYTES]).unwrap();
        std::fs::write(d.join("f.raw"), font()).unwrap();
        std::fs::write(d.join("short.raw"), &font()[..100]).unwrap();
        // Five 1x1 frames: the bar count's own colour.
        let bars: Vec<u8> = (0..5u16).flat_map(|f| (0x100 + f).to_le_bytes()).collect();
        std::fs::write(d.join("bars.raw"), bars).unwrap();
        std::fs::write(
            d.join("manifest.txt"),
            "page 0 p.raw 99 noclock noticker\npagenight 0 pn.raw\nscreen home home.raw\n\
             livefont f f.raw 2 2\nlivefont s short.raw 2 2\n\
             pane home bars.raw 1 1 300 0 wifi 5\n\
             live home ip f 10 10 20 2 left bg\n\
             live home ip f 10 20 20 2 right #ff0000\n\
             live home uptime f 10 30 21 2 center bg\n\
             live home ip s 10 40 20 2 left bg\n\
             pane page:0 bars.raw 1 1 0 0 wifi 5\n\
             live page:0 name f 0 10 8 2 left bg\n\
             pane pagenight:0 bars.raw 1 1 5 5 wifi_pct 5\n",
        )
        .unwrap();
        let mut h = Host::new("unused", vec![1 << 40], 1793150000, d.clone());
        h.values.insert("ip".into(), "1.2".into());
        h.values.insert("wifi".into(), "3".into());
        h.values.insert("name".into(), "a?b-z".into());
        let mut e = Engine::new(h, d.clone());
        e.start();
        assert_eq!(e.m.fonts.len(), 1, "a strip of the wrong size is refused");
        assert_eq!(e.m.lives.len(), 4);
        // The page, by day: its page:0 pane and box.
        for _ in 0..12 {
            e.frame();
        }
        assert_eq!(px(&mut e, 0, 0), 0x103, "3 bars");
        let g = |c: char| values::glyph(c) as u16 + 1;
        assert_eq!(px(&mut e, 0, 10), 0x1111, "the glyph's transparent pixel shows the background");
        assert_eq!(px(&mut e, 1, 10), g('a'));
        assert_eq!(px(&mut e, 3, 11), g('?'));
        assert_eq!(px(&mut e, 7, 11), g('-'), "the box cuts the text at its edge");
        assert_eq!(px(&mut e, 8, 11), 0x1111);
        assert_eq!(px(&mut e, 5, 5), 0x1111, "pagenight panes only at night");
        e.ctl_line(b"show home", e.ms());
        e.frame();
        assert_eq!(px(&mut e, 300, 0), 0x103);
        // Left: "1.2" from x=10; then the rest of the box is the background.
        assert_eq!(
            [px(&mut e, 11, 11), px(&mut e, 13, 11), px(&mut e, 15, 11), px(&mut e, 17, 11)],
            [g('1'), g('.'), g('2'), 0x3333]
        );
        // Right: ends at x=29; the box is cleared to red.
        assert_eq!([px(&mut e, 25, 21), px(&mut e, 29, 21), px(&mut e, 23, 21)], [g('1'), g('2'), 0xF800]);
        // Centre, missing: "?" at 10 + (21 - 2) / 2 = 19.
        assert_eq!([px(&mut e, 20, 31), px(&mut e, 18, 31)], [g('?'), 0x3333]);
        // A box whose font was refused is skipped.
        assert_eq!(px(&mut e, 11, 41), 0x3333);
        // New values show within 5 s, and only the changed boxes redraw.
        e.plat.values.insert("ip".into(), "9".into());
        e.plat.values.insert("wifi".into(), "0".into());
        e.plat.fb()[(30 * W + 18) as usize] = 0xBEEF;
        for _ in 0..(VALUES_MS / 40 + 2) {
            e.frame();
        }
        assert_eq!(px(&mut e, 300, 0), 0x100);
        assert_eq!([px(&mut e, 11, 11), px(&mut e, 13, 11)], [g('9'), 0x3333], "the old text is cleared");
        assert_eq!(px(&mut e, 29, 21), g('9'));
        assert_eq!(px(&mut e, 18, 30), 0xBEEF, "an unchanged box isn't redrawn");
        // At night the page has pagenight panes, so they replace its page ones.
        e.ctl_line(b"night on", e.ms());
        e.ctl_line(b"page 0", e.ms());
        for _ in 0..12 {
            e.frame();
        }
        assert_eq!(px(&mut e, 5, 5), 0x100, "wifi_pct missing: frame 0");
        assert_eq!(px(&mut e, 0, 0), 0x2222);
        assert_eq!(px(&mut e, 1, 10), 0x2222, "page:0's box isn't drawn at night");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn pageapp_steps_whole_apps() {
        let d = std::env::temp_dir().join(format!("classic-apps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("p.raw"), vec![0x11u8; manifest::FRAME_BYTES]).unwrap();
        // Pages 0-1 app 0, 2 its own, 3-5 app 2.
        std::fs::write(
            d.join("manifest.txt"),
            "page 0 p.raw 9\npage 1 p.raw 9\npage 2 p.raw 9\npage 3 p.raw 9\npage 4 p.raw 9\npage 5 p.raw 9\n\
             pageapp 0 0\npageapp 1 0\npageapp 3 2\npageapp 4 2\npageapp 5 2\n",
        )
        .unwrap();
        let h = Host::new("unused", vec![1 << 40], 0, d.clone());
        let mut e = Engine::new(h, d.clone());
        e.start();
        let step = |e: &mut Engine<Host>, p: i32, dir: i32| {
            e.page = p;
            e.app_step(dir)
        };
        assert_eq!(step(&mut e, 0, 1), 2);
        assert_eq!(step(&mut e, 1, 1), 2);
        assert_eq!(step(&mut e, 2, 1), 3);
        assert_eq!(step(&mut e, 4, 1), 0, "wraps to the first app");
        assert_eq!(step(&mut e, 4, -1), 2);
        assert_eq!(step(&mut e, 2, -1), 0, "the previous app's first page");
        assert_eq!(step(&mut e, 0, -1), 3);
        e.page = 4;
        assert_eq!(e.var_value("app"), 2);
        e.page = 2;
        assert_eq!(e.var_value("app"), 2, "a page of its own: its index");
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A session that hands out queued lines and records requests.
    #[derive(Clone, Default)]
    struct FakeSession {
        incoming: Rc<RefCell<Vec<Heard>>>,
        asked: Rc<RefCell<Vec<Ask>>>,
    }

    impl Session for FakeSession {
        fn heard(&mut self) -> Vec<Heard> {
            std::mem::take(&mut *self.incoming.borrow_mut())
        }
        fn ask(&mut self, a: &Ask) -> bool {
            self.asked.borrow_mut().push(a.clone());
            true
        }
    }

    /// A small set: a page, Home (one zone opens doom, one is CHUMBY, one
    /// names no app), and the session's fake end.
    fn with_session(tag: &str, reason_button: bool) -> (Engine<Host>, FakeSession, PathBuf) {
        let d = std::env::temp_dir().join(format!("classic-session-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("p.raw"), vec![0x55u8; manifest::FRAME_BYTES]).unwrap();
        std::fs::write(d.join("home.raw"), vec![0xAAu8; manifest::FRAME_BYTES]).unwrap();
        std::fs::write(
            d.join("manifest.txt"),
            "page 0 p.raw 12 noclock noticker\nscreen home home.raw\n\
             hot home 0 0 100 100 app doom\nhot home 100 0 100 100 chumby\nhot home 200 0 100 100 app ../x\n",
        )
        .unwrap();
        let h = Host::new("unused", vec![1 << 40], 0, d.clone());
        let mut e = Engine::new(h, d.clone());
        let s = FakeSession::default();
        e.set_session(Box::new(s.clone()), reason_button);
        e.start();
        (e, s, d)
    }

    /// Frames until the engine leaves (true), or `n` of them (false).
    fn leaves(e: &mut Engine<Host>, n: usize) -> bool {
        (0..n).any(|_| !e.frame())
    }

    #[test]
    fn the_session_button_line_toggles_home() {
        let (mut e, s, d) = with_session("button", false);
        for _ in 0..12 {
            assert!(e.frame());
        }
        assert_eq!(e.scr, -1);
        s.incoming.borrow_mut().push(Heard::Button);
        assert!(e.frame());
        assert_eq!(e.m.screens[e.scr as usize].name, "home", "a press: Home");
        assert!(e.plat.fb().iter().all(|&p| p == 0xAAAA));
        s.incoming.borrow_mut().push(Heard::Button);
        assert!(e.frame());
        assert_eq!(e.scr, -1, "again: the slideshow");
        assert!(s.asked.borrow().is_empty());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn opened_for_the_button_starts_on_home() {
        let (mut e, _s, d) = with_session("reason", true);
        assert!(e.frame());
        assert_eq!(e.m.screens[e.scr as usize].name, "home");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn apps_and_chumby_mode_go_through_the_session() {
        let (mut e, s, d) = with_session("open", false);
        e.ctl_line(b"show home", e.ms());
        assert!(e.frame());
        // A zone naming no valid app asks nothing.
        e.ctl_line(b"tap 250 50", e.ms());
        assert!(!leaves(&mut e, 12));
        assert!(s.asked.borrow().is_empty());
        // The doom zone: `open doom`, then the app leaves.
        e.ctl_line(b"tap 50 50", e.ms());
        assert!(leaves(&mut e, 3), "it exits after asking");
        assert_eq!(*s.asked.borrow(), vec![Ask::Open("doom".into())]);
        std::fs::remove_dir_all(&d).unwrap();

        let (mut e, s, d) = with_session("chumby", false);
        e.ctl_line(b"show home", e.ms());
        assert!(e.frame());
        e.ctl_line(b"tap 150 50", e.ms());
        assert!(leaves(&mut e, 3));
        assert_eq!(*s.asked.borrow(), vec![Ask::ChumbyMode]);
        std::fs::remove_dir_all(&d).unwrap();

        // The control file's `app` asks the same way.
        let (mut e, s, d) = with_session("ctl", false);
        e.ctl_line(b"app quake", e.ms());
        assert!(!e.frame());
        assert_eq!(*s.asked.borrow(), vec![Ask::Open("quake".into())]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_waiting_screen_offers_chumby_mode_and_the_apps() {
        let d = std::env::temp_dir().join(format!("classic-fallback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let run = |taps: &[(i32, i32)]| {
            let h = Host::new("unused", vec![1 << 40], 0, d.clone());
            let mut e = Engine::new(h, d.clone());
            let s = FakeSession::default();
            e.set_session(Box::new(s.clone()), false);
            e.start();
            assert!(e.waiting);
            let mut left = false;
            for &(x, y) in taps {
                let line = format!("tap {x} {y}");
                e.ctl_line(line.as_bytes(), e.ms());
                left |= leaves(&mut e, 15);
            }
            (left, s.asked.borrow().clone())
        };
        assert_eq!(run(&[(10, 10), (300, 20)]), (true, vec![Ask::ChumbyMode]), "two taps up top");
        assert_eq!(run(&[(10, 200), (10, 230)]), (true, vec![Ask::Open("home".into())]), "two below");
        assert_eq!(run(&[(10, 10), (10, 200)]), (false, vec![]), "a tap elsewhere goes back");
        assert_eq!(run(&[(10, 10)]), (false, vec![]), "one tap does nothing yet");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_night_tap_is_kept_for_the_service() {
        let (mut e, _s, d) = with_session("night", false);
        e.ctl_line(b"night on", e.ms());
        assert!(state::read_time(&e.st("wx_night_until")) > 0);
        assert_eq!(read_int(&e.st("wx_night"), 0), 1);
        e.ctl_line(b"nightmode auto", e.ms());
        assert_eq!(state::read_time(&e.st("wx_night_until")), 0, "a new mode drops it");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn sets() {
        let d = std::env::temp_dir().join(format!("wx-sets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("a")).unwrap();
        std::fs::create_dir_all(d.join("b")).unwrap();
        assert_eq!(resolve_dir(&d), d, "nothing anywhere: the directory itself");
        std::fs::write(d.join("b/manifest.txt"), "").unwrap();
        assert_eq!(resolve_dir(&d), d.join("b"), "no current: any complete set");
        std::fs::write(d.join("a/manifest.txt"), "").unwrap();
        std::fs::write(d.join("current"), "b\n").unwrap();
        assert_eq!(resolve_dir(&d), d.join("b"));
        std::fs::write(d.join("current"), " a").unwrap();
        assert_eq!(resolve_dir(&d), d.join("a"));
        std::fs::write(d.join("current"), "c").unwrap();
        assert_eq!(resolve_dir(&d), d.join("a"), "a set that isn't there: the first complete one");
        std::fs::write(d.join("manifest.txt"), "").unwrap();
        assert_eq!(resolve_dir(&d), d, "the old layout wins over a broken pointer");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
