//! manifest.txt, as the feeder writes it: one directive per line, every
//! picture a file in the same directory. The format is the C engine's
//! (wxshow.c), read the same way, so existing feeder output works as it is:
//!
//! ```text
//! page N FILE SECS [noclock] [noticker] [fullframe]
//! pagenight N FILE          the page's dark theme, drawn instead at night
//! pagetap N SCREEN          a tap on page N opens SCREEN (else: next page)
//! pageapp N APP             page N belongs to app number APP (0, 1... in
//!                           play order); Home's < > and NEXT step whole apps
//! sprite PAGE FILE W H FRAMES X Y FPS
//! actor PAGE FILE W H FRAMES FPS PATHFILE NPOINTS   path: int16 x,y per frame at 25 fps
//! clock FILE CELLW CELLH RIGHT Y W0..W14            glyphs "0123456789: APM"
//! clocknight FILE           the clock's cells for night pages
//! ticker FILE W H Y SPEED
//! tickernight FILE          the ticker strip for night pages
//! screen NAME FILE
//! pane TARGET FILE W H X Y VAR FRAMES           TARGET: SCREEN, page:N, pagenight:N
//! hot SCREEN X Y W H ACTION [ARG]
//! slider SCREEN EMPTY FULL KNOB W H X Y T0 T1 KW KH
//! station N ID URL
//! sun RISE SET              epoch seconds, one line per day
//! secsopts S1 S2 ...        the slideshow timings Settings steps through
//! logofx PAGE FILE W H X Y [FX [MODE [HOLD]]]
//! scroller PAGE FILE W H Y SPEED AMP
//! bgscroll PAGE FILE W SPEED
//! glow PAGE FILE W H Y SPEED [FADE_MS PHASE_MS SWAY_PX SWAY_MS [X0 [ON_MS]]]
//! glowseq PAGE PREFIX NFRAMES FPS R G B BRIGHT     frames PREFIX_000.l8 ...
//! livefont NAME FILE CELLW CELLH [W0..W78]       a glyph strip for live text
//! live TARGET KEY FONT X Y W H ALIGN BG          a device value as text
//! ```
//!
//! The live values (`live`, and `wifi`/`wifi_pct` as a pane's VAR) are
//! specified exactly in the live-values spec the feeder works from;
//! values.rs reads them. A page's `pagenight:N` panes and live boxes are
//! drawn instead of its `page:N` ones while it shows its pagenight file,
//! if it has any.
//!
//! A line that doesn't parse, or names a file that is missing or the wrong
//! size, is skipped. Pages are numbered in the order they come; the N in
//! `page N` is not used, and `pagenight`/`pagetap` refer to that order. A
//! screen must come before its panes, tap zones and slider.

use crate::scan::{V, atoi, contains, lines, scan};
use crate::values::{Key, NGLYPHS};

pub const W: i32 = 320;
pub const H: i32 = 240;
pub const FPS: i32 = 25;
pub const FRAME_BYTES: usize = (W * H * 2) as usize;

// The limits: how many of each thing a manifest may have. Lines past a limit
// are skipped, so a hostile manifest can't grow the engine without bound.
// Every list is a Vec, so what a manifest doesn't use costs nothing; the
// costs below are the most a full manifest can hold, beyond its pictures
// (which the cache's budget bounds). engine-limits.md, in the notes the
// feeder works from, has the same numbers.
//
// The C engine had fixed arrays: 48 pages, 160 screens, 128 panes, 768 tap
// zones, 640 files and 23-byte names. A channel with every widget needs
// about 153 screens, so these are raised; a feeder written for the C
// engine's limits works as before.

/// Slideshow pages (each about 100 bytes).
pub const MAXP: usize = 128;
/// Sprites, over all pages (about 50 bytes each).
pub const MAXS: usize = 256;
/// Actors (about 70 bytes each).
pub const MAXA: usize = 128;
/// Screens, and sliders (about 80 bytes each, with the name).
pub const MAXSCR: usize = 512;
/// Panes, on screens and pages (about 100 bytes each).
pub const MAXPANE: usize = 512;
/// Tap zones (about 120 bytes each, with the action and argument).
pub const MAXHOT: usize = 2048;
/// Music stations (each a URL of up to 255 bytes).
pub const MAXST: usize = 64;
/// Files a manifest names (about 150 bytes each, with the name, plus its
/// picture while it is in the cache).
pub const MAXASSET: usize = 4096;
/// Sunrise/sunset lines.
pub const MAXSUN: usize = 32;
/// Slideshow timings Settings steps through.
pub const MAXSECS: usize = 12;
/// Glow layers (each a few hundred bytes of lit spans when indexed).
pub const MAXGLOW: usize = 32;
/// Glowseq clips, and frames in each.
pub const MAXGSQ: usize = 8;
pub const MAXGSQ_FRAMES: i32 = 1000;
/// The longest name of a screen, font or tap-zone argument, in bytes (the
/// `%48s` in the formats below; the C engine's was 23). The tests check
/// the formats against it.
#[cfg_attr(not(test), allow(dead_code))]
pub const NAME_LEN: usize = 48;
/// The longest file name, in bytes (`%96s`; the C engine's was 63).
#[cfg_attr(not(test), allow(dead_code))]
pub const FILE_LEN: usize = 96;
/// The largest manifest.txt and hashes.txt read, in bytes.
pub const MAX_MANIFEST_BYTES: u64 = 2 << 20;

/// The logo intros, in the order the manifest, `fx N` and the stats file number them.
pub const FX_NAMES: [&str; 10] =
    ["fly", "print", "wobble", "zoom", "drop", "dissolve", "raster", "decrunch", "spin", "octopus"];
pub const NFX: i32 = FX_NAMES.len() as i32;

/// "random" is -1, a name or number its index, anything else -2.
pub fn fx_parse(a: &str) -> i32 {
    if a == "random" {
        return -1;
    }
    if a.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        let n = atoi(a.as_bytes());
        return if n < NFX { n } else { -2 };
    }
    FX_NAMES.iter().position(|&n| n == a).map_or(-2, |i| i as i32)
}

/// An asset: an index into the cache.
pub type Ix = Option<usize>;

/// Where a pane or a live box is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Screen(usize),
    /// A slideshow page, by its index.
    Page(usize),
    /// A slideshow page while it shows its pagenight file.
    PageNight(usize),
}

/// Live-text fonts, and live boxes.
pub const MAXFONT: usize = 32;
pub const MAXLIVE: usize = 512;

/// A `livefont`: a strip of the charset's glyphs (values::CHARSET).
#[derive(Clone, Debug)]
pub struct Font {
    pub name: String,
    pub px: usize,
    pub cw: i32,
    pub ch: i32,
    /// Each glyph's width, in charset order (all CELLW if none were given).
    pub wd: Vec<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
    Center,
}

/// A `live` box: a device value drawn as text.
#[derive(Clone, Debug)]
pub struct Live {
    pub on: Target,
    pub key: Key,
    /// An index into the manifest's fonts.
    pub font: usize,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub align: Align,
    /// What the box is cleared to: a colour, or None for the target's own
    /// background picture.
    pub bg: Option<u16>,
}

#[derive(Clone, Debug, Default)]
pub struct Page {
    pub bg: Ix,
    pub secs: i32,
    pub noclock: bool,
    pub noticker: bool,
    pub fullframe: bool,
    pub pano: Ix,
    pub pano_w: i32,
    pub pano_speed: i32,
    pub nbg: Ix,
    pub tap: String,
    /// The app it belongs to (pageapp), or -1: an app of its own.
    pub app: i32,
}

#[derive(Clone, Debug)]
pub struct Sprite {
    pub page: i32,
    pub w: i32,
    pub h: i32,
    pub frames: i32,
    pub x: i32,
    pub y: i32,
    pub fps: i32,
    pub shown: i32,
    pub px: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Actor {
    pub page: i32,
    pub w: i32,
    pub h: i32,
    pub frames: i32,
    pub fps: i32,
    pub n: i32,
    pub x: i32,
    pub y: i32,
    pub shown_x: i32,
    pub shown_y: i32,
    pub shown_f: i32,
    pub px: usize,
    pub path: usize,
}

#[derive(Clone, Debug)]
pub struct Screen {
    pub name: String,
    pub bg: usize,
}

#[derive(Clone, Debug)]
pub struct Pane {
    pub on: Target,
    pub w: i32,
    pub h: i32,
    pub x: i32,
    pub y: i32,
    pub frames: i32,
    pub shown: i32,
    pub var: String,
    pub px: usize,
}

#[derive(Clone, Debug)]
pub struct Hot {
    pub scr: usize,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub arg: i32,
    pub act: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Slider {
    pub scr: usize,
    pub w: i32,
    pub h: i32,
    pub x: i32,
    pub y: i32,
    pub t0: i32,
    pub t1: i32,
    pub kw: i32,
    pub kh: i32,
    pub empty: usize,
    pub full: usize,
    pub knob: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Clock {
    pub px: Ix,
    pub pxn: Ix,
    pub cw: i32,
    pub ch: i32,
    pub right: i32,
    pub y: i32,
    pub wd: [i32; 15],
}

#[derive(Clone, Debug, Default)]
pub struct Ticker {
    pub px: Ix,
    pub pxn: Ix,
    pub w: i32,
    pub h: i32,
    pub y: i32,
    pub speed: i32,
}

#[derive(Clone, Debug)]
pub struct Logo {
    pub page: i32,
    pub px: Ix,
    pub w: i32,
    pub h: i32,
    pub x: i32,
    pub y: i32,
    /// An intro forced by the manifest, or -1 for random.
    pub force: i32,
    pub once: bool,
    pub hold_ms: i32,
}

impl Default for Logo {
    fn default() -> Logo {
        Logo { page: -1, px: None, w: 0, h: 0, x: 0, y: 0, force: -1, once: false, hold_ms: 4000 }
    }
}

#[derive(Clone, Debug)]
pub struct Scroller {
    pub page: i32,
    pub px: Ix,
    pub w: i32,
    pub h: i32,
    pub y: i32,
    pub speed: i32,
    pub amp: i32,
}

impl Default for Scroller {
    fn default() -> Scroller {
        Scroller { page: -1, px: None, w: 0, h: 0, y: 0, speed: 0, amp: 0 }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Glow {
    pub page: i32,
    pub w: i32,
    pub h: i32,
    pub y: i32,
    pub speed: i32,
    pub px: usize,
    pub fade_ms: i32,
    pub phase_ms: i32,
    pub sway_px: i32,
    pub sway_ms: i32,
    pub x0: i32,
    pub on_ms: i32,
}

#[derive(Clone, Debug)]
pub struct GlowSeq {
    pub page: i32,
    /// Frames per 1000 s.
    pub mfps: i32,
    /// Each frame's asset; identical frames share one.
    pub frames: Vec<Ix>,
    /// Intensity to RGB565, so a pixel is a lookup and an add.
    pub lut: [u16; 256],
}

#[derive(Clone, Debug, Default)]
pub struct Manifest {
    pub pages: Vec<Page>,
    pub sprites: Vec<Sprite>,
    pub actors: Vec<Actor>,
    pub screens: Vec<Screen>,
    pub panes: Vec<Pane>,
    pub hots: Vec<Hot>,
    pub sliders: Vec<Slider>,
    /// URLs by station number; "" where none was given.
    pub stations: Vec<String>,
    pub sun: Vec<(i64, i64)>,
    pub secs_opt: Vec<i32>,
    pub clock: Clock,
    pub ticker: Ticker,
    pub logo: Logo,
    pub scroller: Scroller,
    pub glows: Vec<Glow>,
    pub gsqs: Vec<GlowSeq>,
    pub fonts: Vec<Font>,
    pub lives: Vec<Live>,
    /// Screen names to indices (the first screen of a name), so lookups
    /// stay fast with hundreds of screens and thousands of tap zones.
    pub screen_ix: std::collections::HashMap<String, usize>,
}

impl Manifest {
    /// A pane's or live box's target: a screen declared so far, or
    /// `page:N` / `pagenight:N` for a page declared so far.
    pub fn target(&self, s: &str) -> Option<Target> {
        let page = |n: &str| {
            let i: usize = n.parse().ok().filter(|_| n.bytes().all(|b| b.is_ascii_digit()))?;
            (i < self.pages.len()).then_some(i)
        };
        if let Some(n) = s.strip_prefix("page:") {
            return page(n).map(Target::Page);
        }
        if let Some(n) = s.strip_prefix("pagenight:") {
            return page(n).map(Target::PageNight);
        }
        self.find_screen(s).map(Target::Screen)
    }

    pub fn find_screen(&self, name: &str) -> Option<usize> {
        self.screen_ix.get(name).copied()
    }
}

/// "12" or "12.5" to 12000 or 12500; -1 if it isn't a number. Integer only:
/// the Chumby has no FPU.
pub fn parse_milli(t: &str) -> i32 {
    let mut v: i64 = 0;
    let mut frac: i32 = -1;
    for c in t.bytes() {
        if c == b'.' && frac < 0 {
            frac = 0;
            continue;
        }
        if !c.is_ascii_digit() || v > 1_000_000 {
            return -1;
        }
        if frac >= 0 {
            if frac == 3 {
                continue;
            }
            frac += 1;
        }
        v = v * 10 + (c - b'0') as i64;
    }
    for _ in frac.max(0)..3 {
        v *= 10;
    }
    v as i32
}

/// Whether a manifest has a page whose file is there and the right size:
/// one that hasn't is ignored, and the last good set stays up.
pub fn usable(m: &[u8], size_of: impl Fn(&str) -> Option<u64>) -> bool {
    m.split(|&b| b == b'\n').any(|l| {
        let v = scan(l, "page %d %96s %d");
        v.len() == 3 && size_of(v[1].s()) == Some(FRAME_BYTES as u64)
    })
}

/// The size a file must have.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Size {
    Exactly(usize),
    /// Any size (glowseq frames); the cache still caps it at its budget.
    Any,
    /// The manifest's numbers make no sense (zero, negative, overflowing):
    /// no file can match.
    Invalid,
}

/// The bytes a picture of these dimensions takes: their product, checked.
/// The manifest comes from a server, so it is not trusted to keep its
/// numbers sane.
fn bytes(dims: &[i32]) -> Size {
    let mut n: usize = 1;
    for &d in dims {
        let Some(m) = usize::try_from(d).ok().filter(|&d| d > 0).and_then(|d| n.checked_mul(d)) else {
            return Size::Invalid;
        };
        n = m;
    }
    Size::Exactly(n)
}

/// A position, offset or small factor: the drawing code adds and
/// multiplies these in 32 bits, so they stay well inside that.
fn coord(v: i32) -> bool {
    (-32768..=32767).contains(&v)
}

fn coords(v: &[V], k: &[usize]) -> bool {
    k.iter().all(|&i| coord(v[i].i()))
}

/// Reads a manifest. `asset` registers a file and returns its index: given
/// the name and the size it must have, None if it is missing or the wrong
/// size (or too big to hold).
pub fn parse(m: &[u8], mut asset: impl FnMut(&str, Size) -> Ix) -> Manifest {
    let mut o = Manifest::default();
    for line in lines(m) {
        let sc = |fmt: &str| scan(line, fmt);
        if let Some(v) = some(sc("page %d %96s %d"), 3).filter(|_| o.pages.len() < MAXP) {
            let p = Page {
                bg: asset(v[1].s(), Size::Exactly(FRAME_BYTES)),
                // At least a second: 0 or less would change pages every frame.
                secs: v[2].i().max(1),
                noclock: contains(line, "noclock"),
                noticker: contains(line, "noticker"),
                fullframe: contains(line, "fullframe"),
                pano: None,
                pano_w: 0,
                pano_speed: 0,
                nbg: None,
                tap: String::new(),
                app: -1,
            };
            if p.bg.is_some() {
                o.pages.push(p);
            }
        } else if let Some(v) = some(sc("pagenight %d %96s"), 2).filter(|v| idx(v[0].i(), o.pages.len())) {
            o.pages[v[0].i() as usize].nbg = asset(v[1].s(), Size::Exactly(FRAME_BYTES));
        } else if let Some(v) = some(sc("pageapp %d %d"), 2).filter(|v| idx(v[0].i(), o.pages.len()) && v[1].i() >= 0) {
            o.pages[v[0].i() as usize].app = v[1].i();
        } else if let Some(v) = some(sc("pagetap %d %48s"), 2).filter(|v| idx(v[0].i(), o.pages.len())) {
            o.pages[v[0].i() as usize].tap = v[1].s().to_string();
        } else if let Some(v) = some(sc("clocknight %96s"), 1).filter(|_| o.clock.px.is_some()) {
            let c = &o.clock;
            o.clock.pxn = asset(v[0].s(), bytes(&[c.cw, c.ch, 15, 2]));
        } else if let Some(v) = some(sc("tickernight %96s"), 1).filter(|_| o.ticker.px.is_some()) {
            let t = &o.ticker;
            o.ticker.pxn = asset(v[0].s(), bytes(&[t.w, t.h, 2]));
        } else if let Some(v) = some(sc("sprite %d %96s %d %d %d %d %d %d"), 8).filter(|v| {
            o.sprites.len() < MAXS
                && v[2].i() > 0
                && v[3].i() > 0
                && v[4].i() > 0
                && v[7].i() > 0
                && coords(v, &[2, 3, 5, 6])
        }) {
            let (w, h, frames) = (v[2].i(), v[3].i(), v[4].i());
            if let Some(px) = asset(v[1].s(), bytes(&[w, h, frames, 2])) {
                o.sprites.push(Sprite {
                    page: v[0].i(),
                    w,
                    h,
                    frames,
                    x: v[5].i(),
                    y: v[6].i(),
                    fps: v[7].i(),
                    shown: -1,
                    px,
                });
            }
        } else if let Some(v) = some(sc("actor %d %96s %d %d %d %d %96s %d"), 8).filter(|v| {
            // fps may be 0 (a still frame), never negative.
            o.actors.len() < MAXA
                && v[2].i() > 0
                && v[3].i() > 0
                && v[4].i() > 0
                && v[5].i() >= 0
                && v[7].i() > 0
                && coords(v, &[2, 3])
        }) {
            let (w, h, frames, n) = (v[2].i(), v[3].i(), v[4].i(), v[7].i());
            let px = asset(v[1].s(), bytes(&[w, h, frames, 2]));
            let path = asset(v[6].s(), bytes(&[n, 4]));
            if let (Some(px), Some(path)) = (px, path) {
                o.actors.push(Actor {
                    page: v[0].i(),
                    w,
                    h,
                    frames,
                    fps: v[5].i(),
                    n,
                    shown_f: -1,
                    px,
                    path,
                    ..Actor::default()
                });
            }
        } else if let Some(v) =
            some(sc("clock %96s %d %d %d %d"), 5).filter(|v| v[1].i() > 0 && v[2].i() > 0 && coords(v, &[1, 2, 3, 4]))
        {
            // The widths follow the sixth space.
            let mut wd = [0i32; 15];
            let mut p = Some(0usize);
            for _ in 0..6 {
                p = p.and_then(|p| next_space(line, p));
            }
            let mut n = 0;
            while let Some(q) = p
                && n < 15
            {
                wd[n] = atoi(&line[q + 1..]);
                n += 1;
                p = next_space(line, q);
            }
            let (cw, ch) = (v[1].i(), v[2].i());
            // A glyph is at most a cell wide.
            for w in &mut wd {
                *w = (*w).clamp(0, cw);
            }
            o.clock = Clock {
                px: asset(v[0].s(), bytes(&[cw, ch, 15, 2])),
                // A clocknight before it was sized for another clock.
                pxn: None,
                cw,
                ch,
                right: v[3].i(),
                y: v[4].i(),
                wd,
            };
        } else if let Some(v) =
            some(sc("ticker %96s %d %d %d %d"), 5).filter(|v| v[1].i() > 0 && v[2].i() > 0 && coords(v, &[2, 3, 4]))
        {
            let (w, h) = (v[1].i(), v[2].i());
            o.ticker = Ticker { px: asset(v[0].s(), bytes(&[w, h, 2])), pxn: None, w, h, y: v[3].i(), speed: v[4].i() };
        } else if let Some(v) = some(sc("screen %48s %96s"), 2).filter(|_| o.screens.len() < MAXSCR) {
            if let Some(bg) = asset(v[1].s(), Size::Exactly(FRAME_BYTES)) {
                o.screen_ix.entry(v[0].s().to_string()).or_insert(o.screens.len());
                o.screens.push(Screen { name: v[0].s().to_string(), bg });
            }
        } else if let Some(v) = some(sc("pane %48s %96s %d %d %d %d %11s %d"), 8).filter(|v| {
            o.panes.len() < MAXPANE && v[2].i() > 0 && v[3].i() > 0 && v[7].i() > 0 && coords(v, &[2, 3, 4, 5])
        }) {
            let (w, h, frames) = (v[2].i(), v[3].i(), v[7].i());
            if let Some(on) = o.target(v[0].s())
                && let Some(px) = asset(v[1].s(), bytes(&[w, h, frames, 2]))
            {
                o.panes.push(Pane {
                    on,
                    w,
                    h,
                    x: v[4].i(),
                    y: v[5].i(),
                    frames,
                    shown: -1,
                    var: v[6].s().to_string(),
                    px,
                });
            }
        } else if let Some(v) =
            some(sc("hot %48s %d %d %d %d %15s"), 6).filter(|v| o.hots.len() < MAXHOT && coords(v, &[1, 2, 3, 4]))
        {
            let arg = scan(line, "hot %*s %*d %*d %*d %*d %*s %48s");
            let name = arg.first().map(|a| a.s().to_string()).unwrap_or_default();
            if let Some(scr) = o.find_screen(v[0].s()) {
                o.hots.push(Hot {
                    scr,
                    x: v[1].i(),
                    y: v[2].i(),
                    w: v[3].i(),
                    h: v[4].i(),
                    arg: if name.is_empty() { 0 } else { atoi(name.as_bytes()) },
                    act: v[5].s().to_string(),
                    name,
                });
            }
        } else if let Some(v) = some(sc("slider %48s %96s %96s %255s %d %d %d %d %d %d %d %d"), 12).filter(|v| {
            o.sliders.len() < MAXSCR
                && v[4].i() > 0
                && v[5].i() > 0
                && v[10].i() > 0
                && v[11].i() > 0
                && v[9].i() > v[8].i()
                && coords(v, &[4, 5, 6, 7, 8, 9, 10, 11])
        }) {
            let (w, h, kw, kh) = (v[4].i(), v[5].i(), v[10].i(), v[11].i());
            if let Some(scr) = o.find_screen(v[0].s()) {
                let empty = asset(v[1].s(), bytes(&[w, h, 2]));
                let full = asset(v[2].s(), bytes(&[w, h, 2]));
                let knob = asset(v[3].s(), bytes(&[kw, kh, 2]));
                if let (Some(empty), Some(full), Some(knob)) = (empty, full, knob) {
                    o.sliders.push(Slider {
                        scr,
                        w,
                        h,
                        x: v[6].i(),
                        y: v[7].i(),
                        t0: v[8].i(),
                        t1: v[9].i(),
                        kw,
                        kh,
                        empty,
                        full,
                        knob,
                    });
                }
            }
        } else if let Some(v) = some(sc("bgscroll %d %96s %d %d"), 4)
            .filter(|v| idx(v[0].i(), o.pages.len()) && v[2].i() > W && coords(v, &[3]))
        {
            let p = &mut o.pages[v[0].i() as usize];
            p.pano = asset(v[1].s(), bytes(&[v[2].i(), H, 2]));
            p.pano_w = v[2].i();
            p.pano_speed = v[3].i();
        } else if let Some(v) = some(sc("logofx %d %96s %d %d %d %d"), 6)
            .filter(|v| v[2].i() > 0 && v[3].i() > 0 && coords(v, &[2, 3, 4, 5]))
        {
            let (w, h) = (v[2].i(), v[3].i());
            o.logo.px = asset(v[1].s(), bytes(&[w, h, 2]));
            if o.logo.px.is_some() {
                let opt = scan(line, "logofx %*d %*s %*d %*d %*d %*d %23s %23s %d");
                let mut force = opt.first().map_or(-1, |s| fx_parse(s.s()));
                if force < -1 {
                    force = -1; // an unknown name: random
                }
                let hold = opt.get(2).map_or(0, V::i);
                o.logo = Logo {
                    page: v[0].i(),
                    px: o.logo.px,
                    w,
                    h,
                    x: v[4].i(),
                    y: v[5].i(),
                    force,
                    once: opt.get(1).is_some_and(|m| m.s() == "once"),
                    hold_ms: if (1..=600).contains(&hold) { hold * 1000 } else { 4000 },
                };
            }
        } else if let Some(v) = some(sc("glow %d %96s %d %d %d %d"), 6)
            .filter(|v| o.glows.len() < MAXGLOW && v[2].i() > 0 && v[3].i() > 0 && coords(v, &[2, 3, 4, 5]))
        {
            let (w, h) = (v[2].i(), v[3].i());
            let opt: Vec<i32> = scan(line, "glow %*d %*s %*d %*d %*d %*d %d %d %d %d %d %d").iter().map(V::i).collect();
            let o6 = |k: usize| opt.get(k).copied().unwrap_or(0);
            // Cycle lengths can't be negative; the sway and the start in
            // the strip are offsets.
            let sane = o6(0) >= 0 && o6(3) >= 0 && o6(5) >= 0 && coord(o6(2)) && coord(o6(4));
            if sane && let Some(px) = asset(v[1].s(), bytes(&[w, h, 2])) {
                o.glows.push(Glow {
                    page: v[0].i(),
                    w,
                    h,
                    y: v[4].i(),
                    speed: v[5].i(),
                    px,
                    fade_ms: o6(0),
                    phase_ms: o6(1),
                    sway_px: o6(2),
                    sway_ms: o6(3),
                    x0: o6(4),
                    on_ms: o6(5),
                });
            }
        } else if let Some((v, mfps)) = some(sc("glowseq %d %88s %d %15s %d %d %d %d"), 8)
            .map(|v| {
                let m = parse_milli(v[3].s());
                (v, m)
            })
            .filter(|(v, m)| {
                o.gsqs.len() < MAXGSQ && v[2].i() > 0 && v[2].i() <= MAXGSQ_FRAMES && *m > 0 && *m <= FPS * 1000
            })
        {
            let n = v[2].i();
            let frames: Vec<Ix> = (0..n).map(|q| asset(&format!("{}_{q:03}.l8", v[1].s()), Size::Any)).collect();
            if frames.iter().any(Option::is_some) {
                let (r, g, b) = (v[4].i(), v[5].i(), v[6].i());
                let bright = v[7].i().clamp(0, 255);
                let mut lut = [0u16; 256];
                for (i, e) in lut.iter_mut().enumerate() {
                    let i = i as i32;
                    // In 64 bits: the manifest's R, G and B aren't bounded.
                    let c = |c: i32| (c as i64 * i as i64 / 255 * bright as i64 / 255).clamp(0, 255) as i32;
                    *e = ((c(r) >> 3) << 11 | (c(g) >> 2) << 5 | c(b) >> 3) as u16;
                }
                o.gsqs.push(GlowSeq { page: v[0].i(), mfps, frames, lut });
            }
        } else if let Some(v) = some(sc("scroller %d %96s %d %d %d %d %d"), 7)
            .filter(|v| v[2].i() > 0 && v[3].i() > 0 && coords(v, &[3, 4, 5, 6]))
        {
            let (w, h) = (v[2].i(), v[3].i());
            o.scroller.px = asset(v[1].s(), bytes(&[w, h, 2]));
            if o.scroller.px.is_some() {
                o.scroller =
                    Scroller { page: v[0].i(), px: o.scroller.px, w, h, y: v[4].i(), speed: v[5].i(), amp: v[6].i() };
            }
        } else if let Some(v) = some(sc("livefont %48s %96s %d %d"), 4)
            .filter(|v| o.fonts.len() < MAXFONT && (1..=64).contains(&v[2].i()) && (1..=64).contains(&v[3].i()))
        {
            let (cw, ch) = (v[2].i(), v[3].i());
            // The widths: none, or one per glyph; anything else skips it.
            let rest: Vec<&[u8]> = line.split(|b| b.is_ascii_whitespace()).filter(|t| !t.is_empty()).skip(5).collect();
            let wd: Option<Vec<i32>> = match rest.len() {
                0 => Some(vec![cw; NGLYPHS]),
                NGLYPHS => rest
                    .iter()
                    .map(|t| std::str::from_utf8(t).ok()?.parse::<i32>().ok().map(|w| w.clamp(0, cw)))
                    .collect(),
                _ => None,
            };
            if let Some(wd) = wd
                && let Some(px) = asset(v[1].s(), bytes(&[cw, ch, NGLYPHS as i32, 2]))
            {
                o.fonts.push(Font { name: v[0].s().to_string(), px, cw, ch, wd });
            }
        } else if let Some(v) = some(sc("live %48s %15s %48s %d %d %d %d %7s %7s"), 9).filter(|v| {
            let (x, y, w, h) = (v[3].i(), v[4].i(), v[5].i(), v[6].i());
            o.lives.len() < MAXLIVE
                && x >= 0
                && y >= 0
                && w >= 1
                && h >= 1
                && x as i64 + w as i64 <= W as i64
                && y as i64 + h as i64 <= H as i64
        }) {
            let align = match v[7].s() {
                "left" => Some(Align::Left),
                "right" => Some(Align::Right),
                "center" => Some(Align::Center),
                _ => None,
            };
            let bg = match v[8].s() {
                "bg" => Some(None),
                c => c
                    .strip_prefix('#')
                    .filter(|h| h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .map(|c| Some(crate::draw::rgb565((c >> 16) as i32, (c >> 8 & 255) as i32, (c & 255) as i32))),
            };
            let font = o.fonts.iter().rposition(|f| f.name == v[2].s());
            if let (Some(on), Some(key), Some(font), Some(align), Some(bg)) =
                (o.target(v[0].s()), Key::parse(v[1].s()), font, align, bg)
            {
                o.lives.push(Live { on, key, font, x: v[3].i(), y: v[4].i(), w: v[5].i(), h: v[6].i(), align, bg });
            }
        } else if let Some(v) = some(sc("sun %ld %ld"), 2).filter(|_| o.sun.len() < MAXSUN) {
            o.sun.push((v[0].l(), v[1].l()));
        } else if line.starts_with(b"secsopts ") {
            // Every number after a space; zero and less are left out.
            let mut q = Some(8);
            while let Some(p) = q
                && p < line.len()
                && o.secs_opt.len() < MAXSECS
            {
                let s = atoi(&line[p + 1..]);
                if s > 0 {
                    o.secs_opt.push(s);
                }
                q = next_space(line, p);
            }
        } else if let Some(v) = some(sc("station %d %23s %255s"), 3).filter(|v| idx(v[0].i(), MAXST)) {
            let k = v[0].i() as usize;
            if o.stations.len() <= k {
                o.stations.resize(k + 1, String::new());
            }
            o.stations[k] = v[2].s().to_string();
        }
    }
    o
}

fn some(v: Vec<V>, n: usize) -> Option<Vec<V>> {
    (v.len() == n).then_some(v)
}

fn idx(i: i32, len: usize) -> bool {
    i >= 0 && (i as usize) < len
}

/// strchr(p + 1, ' '): the next space after position p.
fn next_space(line: &[u8], p: usize) -> Option<usize> {
    line.get(p + 1..)?.iter().position(|&b| b == b' ').map(|k| p + 1 + k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Registers names as assets in order; files listed in `missing` fail.
    struct Files {
        names: Vec<(String, usize)>,
        missing: Vec<&'static str>,
    }

    impl Files {
        fn new(missing: &[&'static str]) -> Files {
            Files { names: Vec::new(), missing: missing.to_vec() }
        }

        fn parse(&mut self, m: &str) -> Manifest {
            parse(m.as_bytes(), |name, size| {
                let size = match size {
                    Size::Exactly(n) => n,
                    Size::Any => 0,
                    Size::Invalid => return None,
                };
                if self.missing.contains(&name) {
                    return None;
                }
                if let Some(i) = self.names.iter().position(|(n, _)| n == name) {
                    return Some(i);
                }
                self.names.push((name.to_string(), size));
                Some(self.names.len() - 1)
            })
        }

        fn size(&self, name: &str) -> usize {
            let m: HashMap<_, _> = self.names.iter().cloned().collect();
            m[name]
        }
    }

    const FEED: &str = "# wxfeed\nversion 1\nsun 100 200\nsun 300 400\n\
        page 0 weather_current.raw 12\n\
        pagenight 0 weather_current_night.raw\n\
        sprite 0 icon0.raw 44 44 8 50 128 8\n\
        page 3 news.raw 12 noclock noticker\n\
        pagetap 1 news_l0\n\
        page 11 aurora_bg.raw 24 noclock noticker fullframe\n\
        glowseq 2 aurora_seq 3 12.5 150 235 220 170\n\
        bgscroll 2 aurora_base.raw 1280 2\n\
        glow 2 aurora_main.raw 960 136 104 4\n\
        glow 2 rays.raw 320 240 0 0 9000 0 0 0 0 3000\n\
        logofx 2 aurora_logo.raw 223 64 48 88 spin once 9\n\
        screen home home.raw\n\
        pane home home_card.raw 180 150 8 22 page 12\n\
        pane nowhere x.raw 10 10 0 0 page 1\n\
        hot home 22 60 154 110 slideshow\n\
        hot home 260 116 48 48 screen music\n\
        hot home 0 0 10 10 station 3\n\
        slider home e.raw f.raw k.raw 120 26 196 22 12 106 24 24\n\
        secsopts 8 12 0 20\n\
        station 0 groovesalad http://example.org/groovesalad\n\
        station 2 lush http://example.org/lush\n\
        clock clock.raw 14 16 314 22 9 9 9 9 9 9 9 9 9 9 6 5 11 10 13\n\
        ticker ticker.raw 5709 24 216 2\n\
        clocknight clock_night.raw\n\
        tickernight ticker_night.raw\n";

    #[test]
    fn feeder_manifest() {
        let mut f = Files::new(&[]);
        let m = f.parse(FEED);
        assert_eq!(m.pages.len(), 3);
        assert_eq!(m.pages[0].secs, 12);
        assert!(m.pages[0].nbg.is_some());
        assert!(!m.pages[0].noclock && m.pages[1].noclock && m.pages[1].noticker && !m.pages[1].fullframe);
        assert_eq!(m.pages[1].tap, "news_l0");
        assert!(m.pages[2].fullframe);
        assert_eq!((m.pages[2].pano_w, m.pages[2].pano_speed), (1280, 2));
        assert_eq!(f.size("aurora_base.raw"), 1280 * 240 * 2);
        assert_eq!(m.sprites.len(), 1);
        assert_eq!((m.sprites[0].x, m.sprites[0].y, m.sprites[0].fps), (50, 128, 8));
        assert_eq!(f.size("icon0.raw"), 44 * 44 * 8 * 2);
        assert_eq!(m.sun, vec![(100, 200), (300, 400)]);
        assert_eq!(m.gsqs.len(), 1);
        assert_eq!(m.gsqs[0].mfps, 12500);
        assert_eq!(m.gsqs[0].frames.len(), 3);
        assert_eq!(f.size("aurora_seq_002.l8"), 0, "glowseq frames have no fixed size");
        assert_eq!(m.gsqs[0].lut[0], 0);
        assert_eq!(
            m.gsqs[0].lut[255],
            ((150 * 170 / 255) >> 3 << 11 | (235 * 170 / 255) >> 2 << 5 | (220 * 170 / 255) >> 3) as u16
        );
        assert_eq!(m.glows.len(), 2);
        assert_eq!(
            (m.glows[0].w, m.glows[0].h, m.glows[0].y, m.glows[0].speed, m.glows[0].fade_ms),
            (960, 136, 104, 4, 0)
        );
        assert_eq!((m.glows[1].fade_ms, m.glows[1].on_ms), (9000, 3000));
        assert_eq!((m.logo.page, m.logo.force, m.logo.once, m.logo.hold_ms), (2, 8, true, 9000));
        assert_eq!(m.screens.len(), 1);
        assert_eq!(m.panes.len(), 1, "a pane for an unknown screen is skipped");
        assert_eq!(m.panes[0].var, "page");
        assert_eq!(m.hots.len(), 3);
        assert_eq!((m.hots[0].act.as_str(), m.hots[0].name.as_str()), ("slideshow", ""));
        assert_eq!((m.hots[1].act.as_str(), m.hots[1].name.as_str()), ("screen", "music"));
        assert_eq!(m.hots[2].arg, 3);
        assert_eq!(m.sliders.len(), 1);
        assert_eq!((m.sliders[0].t0, m.sliders[0].t1, m.sliders[0].kw), (12, 106, 24));
        assert_eq!(m.secs_opt, vec![8, 12, 20]);
        assert_eq!(
            m.stations,
            vec!["http://example.org/groovesalad".to_string(), String::new(), "http://example.org/lush".into()]
        );
        assert_eq!(m.clock.wd, [9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 6, 5, 11, 10, 13]);
        assert_eq!((m.clock.cw, m.clock.ch, m.clock.right, m.clock.y), (14, 16, 314, 22));
        assert!(m.clock.pxn.is_some() && m.ticker.pxn.is_some());
        assert_eq!(f.size("clock_night.raw"), 14 * 16 * 15 * 2);
        assert_eq!((m.ticker.w, m.ticker.h, m.ticker.y, m.ticker.speed), (5709, 24, 216, 2));
    }

    #[test]
    fn bad_lines_and_files_are_skipped() {
        let mut f = Files::new(&["gone.raw", "k.raw"]);
        let m = f.parse(
            "page 0 gone.raw 12\n\
             pagenight 0 x.raw\n\
             page 1 ok.raw 8\n\
             pagenight 5 y.raw\n\
             sprite 0 s.raw 0 44 8 50 128 8\n\
             clocknight c.raw\n\
             garbage\n\
             screen s s.raw\n\
             slider s e.raw f.raw k.raw 120 26 196 22 12 106 24 24\n\
             slider s e.raw f.raw g.raw 120 26 196 22 106 12 24 24\n\
             logofx 0 l.raw 10 10 0 0 nonsense\n\
             glowseq 0 q 2 30 1 1 1 1\n\
             bgscroll 0 p.raw 320 2\n",
        );
        assert_eq!(m.pages.len(), 1, "a page whose file is missing is left out");
        assert!(m.pages[0].nbg.is_none(), "pagenight before its page refers to nothing");
        assert!(m.sprites.is_empty());
        assert!(m.clock.pxn.is_none(), "clocknight needs a clock");
        assert!(m.sliders.is_empty());
        assert_eq!(m.logo.force, -1, "an unknown intro is random");
        assert!(m.gsqs.is_empty(), "more than 25 fps");
        assert!(m.pages[0].pano.is_none(), "a panorama must be wider than the screen");
    }

    #[test]
    fn live_values_and_page_panes() {
        let w79 = vec!["5"; 79].join(" ");
        let mut f = Files::new(&[]);
        let m = f.parse(&format!(
            "page 0 p.raw 12\npage 1 q.raw 12\npageapp 1 3\npageapp 7 1\nscreen home h.raw\n\
             pane home bars.raw 24 16 290 4 wifi 5\n\
             pane page:1 bars.raw 24 16 290 4 wifi 5\n\
             pane pagenight:0 bars.raw 24 16 290 4 wifi 5\n\
             pane page:2 bars.raw 24 16 290 4 wifi 5\n\
             pane page:x bars.raw 24 16 290 4 wifi 5\n\
             live home ip small 200 22 116 12 right bg\n\
             livefont small font.raw 7 12\n\
             livefont wide font2.raw 9 12 {w79}\n\
             livefont bad font3.raw 9 12 5 5 5\n\
             livefont big font4.raw 65 12\n\
             live home ip small 200 22 116 12 right bg\n\
             live page:0 uptime wide 4 226 80 12 center #0080ff\n\
             live pagenight:1 name small 0 0 320 240 left #000000\n\
             live home ip small 200 22 121 12 right bg\n\
             live home ip small -1 22 10 12 right bg\n\
             live home ip small 0 0 0 12 right bg\n\
             live home nokey small 0 0 10 12 left bg\n\
             live home ip nofont 0 0 10 12 left bg\n\
             live home ip small 0 0 10 12 middle bg\n\
             live home ip small 0 0 10 12 left #12345\n\
             live home ip small 0 0 10 12 left #12345g\n\
             live nowhere ip small 0 0 10 12 left bg\n"
        ));
        assert_eq!((m.pages[0].app, m.pages[1].app), (-1, 3), "pageapp for a page not declared is skipped");
        let on: Vec<Target> = m.panes.iter().map(|p| p.on).collect();
        assert_eq!(on, vec![Target::Screen(0), Target::Page(1), Target::PageNight(0)]);
        assert_eq!(m.fonts.len(), 2, "a font with 3 widths, or cells over 64, is skipped");
        assert_eq!(m.fonts[0].wd, vec![7; 79], "no widths: monospace");
        assert_eq!(m.fonts[1].wd, vec![5; 79]);
        assert_eq!(f.size("font.raw"), 7 * 12 * 79 * 2);
        assert_eq!(m.lives.len(), 3, "a live box before its font, outside the screen or malformed is skipped");
        let l = &m.lives[0];
        assert_eq!(
            (l.on, l.key, l.font, l.x, l.y, l.w, l.h, l.align, l.bg),
            (Target::Screen(0), Key::Ip, 0, 200, 22, 116, 12, Align::Right, None)
        );
        assert_eq!(
            (m.lives[1].key, m.lives[1].align, m.lives[1].bg),
            (Key::Uptime, Align::Center, Some(crate::draw::rgb565(0, 0x80, 0xff)))
        );
        assert_eq!((m.lives[2].on, m.lives[2].key, m.lives[2].w), (Target::PageNight(1), Key::Name, 320));
    }

    #[test]
    fn limits() {
        let mut s = String::new();
        for i in 0..MAXP + 12 {
            s.push_str(&format!("page {i} p{i}.raw 5\n"));
        }
        for i in 0..MAXSCR + 40 {
            s.push_str(&format!("screen s{i} s{i}.raw\n"));
        }
        for i in 0..MAXHOT + 100 {
            s.push_str(&format!("hot s{} 0 0 10 10 screen s{}\n", i % MAXSCR, i % 7));
        }
        let t = std::time::Instant::now();
        let m = Files::new(&[]).parse(&s);
        assert!(t.elapsed().as_secs() < 2, "parsing stays fast");
        assert_eq!((m.pages.len(), m.screens.len()), (MAXP, MAXSCR));
        assert_eq!(m.hots.len(), MAXHOT);
        assert_eq!(m.find_screen("s0"), Some(0));
        assert_eq!(m.find_screen(&format!("s{MAXSCR}")), None);
        // Names up to NAME_LEN bytes.
        let long = "n".repeat(NAME_LEN);
        let m = Files::new(&[]).parse(&format!(
            "page 0 p.raw 5\nscreen {long} a.raw\nscreen {long}x b.raw\npagetap 0 {long}\n\
             hot {long} 0 0 1 1 screen {long}\n"
        ));
        // A longer name is cut at NAME_LEN, as sscanf cuts it (and the rest
        // is read as the next field).
        assert!(m.screens.iter().all(|s| s.name.len() <= NAME_LEN));
        assert_eq!(m.find_screen(&long), Some(0));
        assert_eq!(m.pages[0].tap, long);
        assert_eq!(m.hots[0].name, long);
        let file = format!("{}.raw", "f".repeat(FILE_LEN - 4));
        let m = Files::new(&[]).parse(&format!("page 0 {file} 5\n"));
        assert_eq!(m.pages.len(), 1);
    }

    #[test]
    fn numbers() {
        assert_eq!(parse_milli("12"), 12000);
        assert_eq!(parse_milli("12.5"), 12500);
        assert_eq!(parse_milli("0.0405"), 40);
        assert_eq!(parse_milli("1x"), -1);
        assert_eq!(fx_parse("random"), -1);
        assert_eq!(fx_parse("spin"), 8);
        assert_eq!(fx_parse("3"), 3);
        assert_eq!(fx_parse("12"), -2);
        assert_eq!(fx_parse("nope"), -2);
    }

    #[test]
    fn usable_needs_a_full_page() {
        let sizes = |n: &str| match n {
            "a.raw" => Some(FRAME_BYTES as u64),
            "b.raw" => Some(10),
            _ => None,
        };
        assert!(usable(b"screen x y\npage 0 a.raw 5\n", sizes));
        assert!(!usable(b"page 0 b.raw 5\npage 1 c.raw 5\n", sizes));
    }
}
