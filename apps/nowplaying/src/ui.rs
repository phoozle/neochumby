//! The now-playing screen, drawn into a [`Canvas`] the size of the
//! Chumby's screen (320x240 RGB565) that the screen app copies to
//! /dev/fb0 whole, so a redraw never flickers. Kept apart from the device
//! so that the layout is drawn and checked on the host too.
//!
//! Playing: the cover on the left, the title (two lines at most), artist,
//! album and sender beside it; the progress bar with the time gone and the
//! track's length; previous, play/pause and next along the bottom, dimmed
//! when the sender takes no remote control. Idle: "Ready for AirPlay" and
//! the name to pick on the phone, or, without the companion or the airplay
//! app's feed, that it's waiting for them.
//!
//! Text is ISO 8859-1 (embedded-graphics' fonts); a character outside it
//! shows as '?'. Everything is integer arithmetic.

use std::convert::Infallible;

use embedded_graphics::mono_font::iso_8859_1::{FONT_6X10, FONT_7X14, FONT_7X14_BOLD, FONT_10X20};
use embedded_graphics::mono_font::{MonoFont, MonoTextStyle};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::pixelcolor::raw::RawU16;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle, Triangle};
use embedded_graphics::text::{Baseline, Text};

use crate::frames::{COVER_SIDE, Control, Play};
use crate::nowplaying::{NowPlaying, clock};

pub const W: usize = 320;
pub const H: usize = 240;

pub const fn rgb(r: u8, g: u8, b: u8) -> u16 {
    ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3)
}

const BG: u16 = rgb(16, 16, 32);
const TEXT: u16 = rgb(235, 235, 235);
const DIM: u16 = rgb(160, 160, 180);
const FAINT: u16 = rgb(100, 100, 120);
const ACCENT: u16 = rgb(255, 200, 120);
const BAR: u16 = rgb(60, 60, 84);
const BUTTON: u16 = rgb(40, 40, 64);
const BUTTON_LIT: u16 = rgb(96, 96, 128);
const NOTE: u16 = rgb(70, 70, 96);

const MARGIN: i32 = 10;
const COVER: i32 = COVER_SIDE as i32;
/// Where the words beside the cover start, and how wide they may run.
const TEXT_X: i32 = MARGIN + COVER + 12;
const TEXT_W: i32 = W as i32 - MARGIN - TEXT_X;
const BAR_Y: i32 = 148;
const BAR_H: i32 = 6;
const TIMES_Y: i32 = BAR_Y + BAR_H + 4;
const BUTTON_Y: i32 = 176;
const BUTTON_H: i32 = 56;
const BUTTON_W: i32 = 94;
const BUTTON_GAP: i32 = (W as i32 - 2 * MARGIN - 3 * BUTTON_W) / 2;

/// The buttons, left to right.
pub const BUTTONS: [Control; 3] = [Control::Previous, Control::PlayPause, Control::Next];

/// The screen's pixels, RGB565, a row at a time.
pub struct Canvas {
    px: Vec<u16>,
}

impl Canvas {
    pub fn new() -> Canvas {
        Canvas { px: vec![BG; W * H] }
    }

    pub fn pixels(&self) -> &[u16] {
        &self.px
    }

    pub fn pixel(&self, x: usize, y: usize) -> u16 {
        self.px[y * W + x]
    }

    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: u16) {
        let _ =
            self.fill_solid(&Rectangle::new(Point::new(x, y), Size::new(w.max(0) as u32, h.max(0) as u32)), colour(c));
    }
}

impl Default for Canvas {
    fn default() -> Canvas {
        Canvas::new()
    }
}

impl DrawTarget for Canvas {
    type Color = Rgb565;
    type Error = Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb565>>>(&mut self, pixels: I) -> Result<(), Infallible> {
        for Pixel(p, c) in pixels {
            if let (Ok(x @ 0..W), Ok(y @ 0..H)) = (usize::try_from(p.x), usize::try_from(p.y)) {
                self.px[y * W + x] = RawU16::from(c).into_inner();
            }
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, c: Rgb565) -> Result<(), Infallible> {
        let area = area.intersection(&self.bounding_box());
        if let Some(end) = area.bottom_right() {
            let c = RawU16::from(c).into_inner();
            for y in area.top_left.y as usize..=end.y as usize {
                self.px[y * W + area.top_left.x as usize..=y * W + end.x as usize].fill(c);
            }
        }
        Ok(())
    }
}

impl OriginDimensions for Canvas {
    fn size(&self) -> Size {
        Size::new(W as u32, H as u32)
    }
}

fn colour(c: u16) -> Rgb565 {
    Rgb565::from(RawU16::new(c))
}

/// The button at (x, y), if any.
pub fn hit(x: i32, y: i32) -> Option<Control> {
    if !(BUTTON_Y..BUTTON_Y + BUTTON_H).contains(&y) {
        return None;
    }
    BUTTONS.into_iter().enumerate().find_map(|(i, c)| {
        let left = button_x(i);
        (left..left + BUTTON_W).contains(&x).then_some(c)
    })
}

fn button_x(i: usize) -> i32 {
    MARGIN + i as i32 * (BUTTON_W + BUTTON_GAP)
}

/// Draws `np` as it stands at `now_ms`, with `pressed` lit.
pub fn draw(c: &mut Canvas, np: &NowPlaying, now_ms: u64, pressed: Option<Control>) {
    c.px.fill(BG);
    if np.idle() {
        idle(c, np);
    } else {
        playing(c, np, now_ms, pressed);
    }
}

fn idle(c: &mut Canvas, np: &NowPlaying) {
    let (head, lines): (&str, &[&str]) = if !np.linked {
        (
            "Waiting for AirPlay",
            &["This shows what the AIRPLAY", "app plays, once it and this", "Chumby's server answer."],
        )
    } else {
        ("Ready for AirPlay", &["Play from an iPhone, iPad or Mac,", "and choose this Chumby", "as the speaker."])
    };
    let mut y = 54;
    centred(c, head, &FONT_10X20, if np.linked { TEXT } else { DIM }, y);
    y += 30;
    if np.linked && !np.name.is_empty() {
        for line in wrap(&np.name, 28, 2) {
            centred(c, &line, &FONT_10X20, ACCENT, y);
            y += 22;
        }
    }
    y += 16;
    for line in lines {
        centred(c, line, &FONT_7X14, FAINT, y);
        y += 16;
    }
}

fn playing(c: &mut Canvas, np: &NowPlaying, now_ms: u64, pressed: Option<Control>) {
    cover(c, np);

    // The words beside the cover, from the top; what doesn't fit is cut.
    let mut y = MARGIN;
    let t = &np.track;
    let title = if t.title.is_empty() { "AirPlay" } else { &t.title };
    for line in wrap(title, chars(&FONT_10X20, TEXT_W), 2) {
        text(c, &line, &FONT_10X20, TEXT, TEXT_X, y);
        y += 22;
    }
    y += 4;
    for (field, font, ink) in [(&t.artist, &FONT_7X14_BOLD, ACCENT), (&t.album, &FONT_7X14, DIM)] {
        if !field.is_empty() {
            for line in wrap(field, chars(font, TEXT_W), 2) {
                text(c, &line, font, ink, TEXT_X, y);
                y += 16;
            }
            y += 2;
        }
    }
    if !t.source.is_empty() && y <= MARGIN + COVER - 10 {
        let from = format!("from {}", t.source);
        let line = wrap(&from, chars(&FONT_6X10, TEXT_W), 1).pop().unwrap_or_default();
        text(c, &line, &FONT_6X10, FAINT, TEXT_X, MARGIN + COVER - 10);
    }

    // The progress bar, and the times under it.
    let s = np.state;
    let elapsed = np.elapsed_ms(now_ms);
    let bar_w = W as i32 - 2 * MARGIN;
    c.rect(MARGIN, BAR_Y, bar_w, BAR_H, BAR);
    if s.duration_ms > 0 {
        let done = (elapsed as u64 * bar_w as u64 / s.duration_ms as u64) as i32;
        c.rect(MARGIN, BAR_Y, done.min(bar_w), BAR_H, ACCENT);
    }
    text(c, &clock(elapsed), &FONT_6X10, DIM, MARGIN, TIMES_Y);
    if s.duration_ms > 0 {
        let total = clock(s.duration_ms);
        let x = W as i32 - MARGIN - width(&total, &FONT_6X10);
        text(c, &total, &FONT_6X10, DIM, x, TIMES_Y);
    }
    if s.play == Play::Paused {
        centred(c, "PAUSED", &FONT_6X10, ACCENT, TIMES_Y);
    }

    for (i, b) in BUTTONS.into_iter().enumerate() {
        button(c, i, b, s.play == Play::Playing, s.controls, pressed == Some(b));
    }
}

/// The cover art, centred in its box, or a drawn note when there is none.
fn cover(c: &mut Canvas, np: &NowPlaying) {
    let art = &np.cover;
    if art.width == 0 || art.height == 0 {
        c.rect(MARGIN, MARGIN, COVER, COVER, BAR);
        // A quaver: a head, a stem and a flag.
        let (x, y) = (MARGIN + 48, MARGIN + 30);
        c.rect(x + 22, y, 6, 60, NOTE);
        c.rect(x + 28, y, 18, 6, NOTE);
        c.rect(x + 40, y + 6, 6, 14, NOTE);
        c.rect(x, y + 50, 28, 20, NOTE);
        return;
    }
    let (w, h) = (art.width as i32, art.height as i32);
    let (x0, y0) = (MARGIN + (COVER - w) / 2, MARGIN + (COVER - h) / 2);
    for row in 0..h {
        let y = (y0 + row) as usize;
        let src = &art.pixels[row as usize * w as usize..][..w as usize];
        c.px[y * W + x0 as usize..][..w as usize].copy_from_slice(src);
    }
}

fn button(c: &mut Canvas, i: usize, b: Control, playing: bool, enabled: bool, lit: bool) {
    let x = button_x(i);
    c.rect(x, BUTTON_Y, BUTTON_W, BUTTON_H, if lit { BUTTON_LIT } else { BUTTON });
    let ink = colour(if enabled { TEXT } else { FAINT });
    let fill = PrimitiveStyle::with_fill(ink);
    let (cx, cy) = (x + BUTTON_W / 2, BUTTON_Y + BUTTON_H / 2);
    let tri = |c: &mut Canvas, a: (i32, i32), b: (i32, i32), d: (i32, i32)| {
        let _ =
            Triangle::new(Point::new(a.0, a.1), Point::new(b.0, b.1), Point::new(d.0, d.1)).into_styled(fill).draw(c);
    };
    let ink = if enabled { TEXT } else { FAINT };
    match b {
        Control::PlayPause | Control::Pause | Control::Play if playing => {
            c.rect(cx - 10, cy - 12, 7, 24, ink);
            c.rect(cx + 3, cy - 12, 7, 24, ink);
        }
        Control::PlayPause | Control::Pause | Control::Play => {
            tri(c, (cx - 9, cy - 13), (cx - 9, cy + 13), (cx + 13, cy))
        }
        Control::Next => {
            tri(c, (cx - 16, cy - 10), (cx - 16, cy + 10), (cx - 1, cy));
            tri(c, (cx - 1, cy - 10), (cx - 1, cy + 10), (cx + 14, cy));
            c.rect(cx + 14, cy - 10, 4, 21, ink);
        }
        Control::Previous => {
            c.rect(cx - 18, cy - 10, 4, 21, ink);
            tri(c, (cx + 1, cy - 10), (cx + 1, cy + 10), (cx - 14, cy));
            tri(c, (cx + 16, cy - 10), (cx + 16, cy + 10), (cx + 1, cy));
        }
    }
}

fn text(c: &mut Canvas, s: &str, font: &MonoFont<'_>, ink: u16, x: i32, y: i32) {
    let style = MonoTextStyle::new(font, colour(ink));
    let _ = Text::with_baseline(s, Point::new(x, y), style, Baseline::Top).draw(c);
}

fn centred(c: &mut Canvas, s: &str, font: &MonoFont<'_>, ink: u16, y: i32) {
    text(c, s, font, ink, (W as i32 - width(s, font)) / 2, y);
}

fn width(s: &str, font: &MonoFont<'_>) -> i32 {
    s.chars().count() as i32 * (font.character_size.width + font.character_spacing) as i32
}

/// How many of `font`'s characters fit in `px`.
fn chars(font: &MonoFont<'_>, px: i32) -> usize {
    (px / (font.character_size.width + font.character_spacing) as i32).max(1) as usize
}

/// `s` in lines of at most `cols` characters, broken between words where
/// it can be, and at most `lines` of them: the last one cut short with
/// "..." if the rest didn't fit.
pub fn wrap(s: &str, cols: usize, lines: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut cut = false;
    for word in s.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        loop {
            let len = line.chars().count();
            let gap = usize::from(len > 0);
            if len + gap + word.len() <= cols {
                if gap == 1 {
                    line.push(' ');
                }
                line.extend(&word);
                break;
            }
            // A word longer than a line is broken; others go to the next.
            if len == 0 {
                line.extend(&word[..cols]);
                word.drain(..cols);
            }
            out.push(std::mem::take(&mut line));
            if out.len() == lines {
                cut = true;
                break;
            }
        }
        if cut {
            break;
        }
    }
    if !cut && !line.is_empty() {
        out.push(line);
    }
    if cut && let Some(last) = out.last_mut() {
        let keep: String = last.chars().take(cols.saturating_sub(3)).collect();
        *last = format!("{}...", keep.trim_end());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::{Cover, Frame, State, Track};

    fn np(play: Play, controls: bool) -> NowPlaying {
        let mut np = NowPlaying::default();
        np.apply(Frame::Link(true), 0);
        np.apply(Frame::Name("Kitchen Chumby".into()), 0);
        np.apply(
            Frame::Track(Track {
                title: "A Song With Quite a Long Title".into(),
                artist: "The Test Pattern Band".into(),
                album: "Frames and Pixels".into(),
                source: "Someone's iPhone".into(),
            }),
            0,
        );
        np.apply(Frame::State(State { play, controls, elapsed_ms: 61_000, duration_ms: 244_000 }), 0);
        np
    }

    /// A made-up cover: four flat quarters, so where each lands is easy to
    /// check.
    fn quarters(side: u16) -> Cover {
        let half = side / 2;
        let pixels = (0..side)
            .flat_map(|y| {
                (0..side).map(move |x| match (x < half, y < half) {
                    (true, true) => rgb(255, 0, 0),
                    (false, true) => rgb(0, 255, 0),
                    (true, false) => rgb(0, 0, 255),
                    (false, false) => rgb(255, 255, 255),
                })
            })
            .collect();
        Cover { width: side, height: side, pixels }
    }

    /// Whether any pixel in the box differs from the background.
    fn inked(c: &Canvas, x: i32, y: i32, w: i32, h: i32) -> bool {
        (y..y + h).any(|y| (x..x + w).any(|x| c.pixel(x as usize, y as usize) != BG))
    }

    #[test]
    fn the_buttons_are_where_they_are_drawn() {
        assert_eq!(hit(MARGIN + 1, BUTTON_Y + 1), Some(Control::Previous));
        assert_eq!(hit(160, 200), Some(Control::PlayPause));
        assert_eq!(hit(W as i32 - MARGIN - 1, BUTTON_Y + BUTTON_H - 1), Some(Control::Next));
        assert_eq!(hit(160, BUTTON_Y - 1), None);
        assert_eq!(hit(MARGIN + BUTTON_W + 1, 200), None, "the gap between buttons");
        assert!(button_x(2) + BUTTON_W <= W as i32 - MARGIN + 1);
    }

    #[test]
    fn now_playing_lays_out_cover_words_progress_and_buttons() {
        let mut state = np(Play::Playing, true);
        state.apply(Frame::Cover(quarters(COVER_SIDE)), 0);
        let mut c = Canvas::new();
        draw(&mut c, &state, 0, None);
        // The cover's quarters, in place.
        assert_eq!(c.pixel(MARGIN as usize + 1, MARGIN as usize + 1), rgb(255, 0, 0));
        assert_eq!(c.pixel((MARGIN + COVER - 2) as usize, MARGIN as usize + 1), rgb(0, 255, 0));
        assert_eq!(c.pixel(MARGIN as usize + 1, (MARGIN + COVER - 2) as usize), rgb(0, 0, 255));
        // Words beside it, two lines of title among them.
        assert!(inked(&c, TEXT_X, MARGIN, TEXT_W, 20));
        assert!(inked(&c, TEXT_X, MARGIN + 22, TEXT_W, 20));
        // 61 s of 244 s: a quarter of the bar is lit.
        let lit = (MARGIN..W as i32 - MARGIN).filter(|&x| c.pixel(x as usize, BAR_Y as usize) == ACCENT).count();
        assert_eq!(lit, (61 * 300 / 244) as usize);
        // The pause sign on the middle button, in full ink.
        let (cx, cy) = (button_x(1) + BUTTON_W / 2, BUTTON_Y + BUTTON_H / 2);
        assert_eq!(c.pixel((cx - 7) as usize, cy as usize), TEXT);
        assert_eq!(c.pixel(cx as usize, cy as usize), BUTTON);
    }

    #[test]
    fn progress_moves_on_its_own_while_playing() {
        let state = np(Play::Playing, true);
        let mut a = Canvas::new();
        let mut b = Canvas::new();
        draw(&mut a, &state, 0, None);
        draw(&mut b, &state, 60_000, None);
        let lit = |c: &Canvas| (0..W).filter(|&x| c.pixel(x, BAR_Y as usize) == ACCENT).count();
        assert!(lit(&b) > lit(&a));
    }

    #[test]
    fn paused_without_controls_dims_the_buttons_and_shows_play() {
        let state = np(Play::Paused, false);
        let mut c = Canvas::new();
        draw(&mut c, &state, 0, Some(Control::Next));
        let (cx, cy) = (button_x(1) + BUTTON_W / 2, BUTTON_Y + BUTTON_H / 2);
        assert_eq!(c.pixel(cx as usize, cy as usize), FAINT, "a play triangle, dimmed");
        assert_eq!(c.pixel(button_x(2) as usize + 1, BUTTON_Y as usize + 1), BUTTON_LIT);
        assert!(inked(&c, 140, TIMES_Y, 40, 10), "PAUSED");
    }

    #[test]
    fn no_cover_draws_a_note() {
        let state = np(Play::Playing, true);
        let mut c = Canvas::new();
        draw(&mut c, &state, 0, None);
        assert_eq!(c.pixel(MARGIN as usize + 1, MARGIN as usize + 1), BAR);
        assert!((0..COVER).any(|d| c.pixel((MARGIN + d) as usize, (MARGIN + 90) as usize) == NOTE));
    }

    #[test]
    fn idle_names_the_receiver() {
        let mut state = NowPlaying::default();
        let mut waiting = Canvas::new();
        draw(&mut waiting, &state, 0, None);
        state.apply(Frame::Link(true), 0);
        state.apply(Frame::Name("Kitchen Chumby".into()), 0);
        let mut ready = Canvas::new();
        draw(&mut ready, &state, 0, None);
        assert_ne!(waiting.pixels(), ready.pixels());
        // The name, in the accent colour, under the heading.
        assert!((0..W).any(|x| (84..104).any(|y| ready.pixel(x, y) == ACCENT)));
        // No buttons when idle.
        assert!(!inked(&ready, MARGIN, BUTTON_Y + 20, W as i32 - 2 * MARGIN, 20));
    }

    #[test]
    fn wrapping() {
        assert_eq!(wrap("one two three", 7, 3), ["one two", "three"]);
        assert_eq!(wrap("one two three four", 7, 2), ["one two", "thre..."]);
        assert_eq!(wrap("abcdefghij", 4, 3), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("abcdefghijklmnop", 4, 2), ["abcd", "e..."]);
        assert_eq!(wrap("", 4, 2), Vec::<String>::new());
        assert_eq!(wrap("  spaced   out ", 20, 1), ["spaced out"]);
    }

    /// With NEOCHUMBY_NOWPLAYING_SHOTS=DIR, the screens as raw RGB565 files
    /// (320x240, little-endian), for dev/fb2png.py: evidence of the layout
    /// with made-up data and a generated cover.
    #[test]
    fn shots() {
        let Some(dir) = std::env::var_os("NEOCHUMBY_NOWPLAYING_SHOTS") else { return };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let save = |name: &str, c: &Canvas| {
            let bytes: Vec<u8> = c.pixels().iter().flat_map(|p| p.to_le_bytes()).collect();
            std::fs::write(dir.join(format!("{name}.raw")), bytes).unwrap();
        };
        let mut c = Canvas::new();
        let mut state = NowPlaying::default();
        draw(&mut c, &state, 0, None);
        save("1-waiting", &c);
        state.apply(Frame::Link(true), 0);
        state.apply(Frame::Name("Kitchen Chumby".into()), 0);
        draw(&mut c, &state, 0, None);
        save("2-ready", &c);
        let mut state = np(Play::Playing, true);
        state.apply(Frame::Cover(quarters(COVER_SIDE)), 0);
        draw(&mut c, &state, 0, None);
        save("3-playing", &c);
        let state = np(Play::Paused, true);
        draw(&mut c, &state, 0, Some(Control::PlayPause));
        save("4-paused-no-cover", &c);
    }
}
