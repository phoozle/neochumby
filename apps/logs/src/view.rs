//! The screen: a bar along the top naming the log shown, with ◀ and ▶ at
//! its ends for the one before and after; the log, [`LINES`] rows of the
//! 5x8 font; and four buttons along the bottom: TOP, PAGE UP, PAGE DOWN
//! and END. Dragging the log scrolls it. At its end, it follows: new lines
//! scroll it on, until it is scrolled back. END is lit while it follows.

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::iso_8859_1::{FONT_5X8, FONT_6X10};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle, Triangle};
use embedded_graphics::text::{Alignment, Baseline, Text as Label, TextStyleBuilder};

use crate::text::{Level, Text};

pub const W: i32 = 320;
pub const H: i32 = 240;
/// The bar along the top, and the buttons along the bottom.
pub const BAR: i32 = 20;
pub const BUTTONS: i32 = 28;
pub const LINE_H: i32 = 8;
/// The log's rows on screen.
pub const LINES: u64 = ((H - BAR - BUTTONS) / LINE_H) as u64;
/// Where the log's rows start, centred in the space between.
const TOP_Y: i32 = BAR + (H - BAR - BUTTONS - LINES as i32 * LINE_H) / 2;
/// ◀ and ▶: this wide, at the bar's ends.
const ARROW: i32 = 56;
/// How far a touch may move and still be a tap.
const SLOP: i32 = 6;

const BG: Rgb565 = Rgb565::new(1, 2, 2);
const FG: Rgb565 = Rgb565::new(24, 48, 24);
const WARNING: Rgb565 = Rgb565::new(31, 50, 6);
const ERROR: Rgb565 = Rgb565::new(31, 22, 18);
const BAR_BG: Rgb565 = Rgb565::new(5, 11, 13);
const BUTTON: Rgb565 = Rgb565::new(4, 8, 10);
const LIT: Rgb565 = Rgb565::new(6, 30, 12);
const LABEL: Rgb565 = Rgb565::WHITE;

/// What a tap asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Previous,
    Next,
    Top,
    PageUp,
    PageDown,
    End,
}

/// A touch, in screen pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Touch {
    Down(i32, i32),
    Up,
}

/// Where the log is scrolled to, and the touch under way.
pub struct View {
    /// The row at the top of the screen.
    pub top: u64,
    /// At the end, following what comes.
    pub follow: bool,
    /// Where the touch went down, and the top then; whether it has moved
    /// beyond a tap.
    touch: Option<(i32, i32, u64, bool)>,
}

impl Default for View {
    fn default() -> View {
        View::new()
    }
}

impl View {
    pub fn new() -> View {
        View { top: 0, follow: true, touch: None }
    }

    fn last_top(text: &Text) -> u64 {
        text.end().saturating_sub(LINES).max(text.first())
    }

    /// Scrolled to `top`, kept within the log; at its end, following.
    fn scroll_to(&mut self, text: &Text, top: u64) {
        let last = View::last_top(text);
        self.top = top.clamp(text.first(), last);
        self.follow = self.top == last;
    }

    /// Brought up to date with the text: on to its end while following, and
    /// never above what is kept. True if the top row changed.
    pub fn settle(&mut self, text: &Text) -> bool {
        let was = self.top;
        if self.follow {
            self.top = View::last_top(text);
        } else {
            self.scroll_to(text, self.top);
        }
        self.top != was
    }

    /// A touch: dragging scrolls (true when it did), and a tap on a
    /// button is its action, which [`act`](View::act) carries out unless
    /// it is ◀ or ▶ (those are the caller's).
    pub fn touch(&mut self, t: Touch, text: &Text) -> (bool, Option<Action>) {
        match (t, self.touch) {
            (Touch::Down(x, y), None) => {
                self.touch = Some((x, y, self.top, false));
                (false, None)
            }
            (Touch::Down(x, y), Some((x0, y0, top0, moved))) => {
                let moved = moved || (x - x0).abs() > SLOP || (y - y0).abs() > SLOP;
                self.touch = Some((x0, y0, top0, moved));
                if !moved || !in_log(y0) {
                    return (false, None);
                }
                // The text follows the finger.
                let rows = (y0 - y) / LINE_H;
                let was = self.top;
                self.scroll_to(text, top0.saturating_add_signed(rows as i64));
                (self.top != was, None)
            }
            (Touch::Up, Some((x, y, _, false))) => {
                self.touch = None;
                (false, hit(x, y))
            }
            (Touch::Up, _) => {
                self.touch = None;
                (false, None)
            }
        }
    }

    /// Scrolls as `a` says; ◀ and ▶ do nothing here.
    pub fn act(&mut self, a: Action, text: &Text) {
        match a {
            Action::Top => self.scroll_to(text, 0),
            Action::PageUp => self.scroll_to(text, self.top.saturating_sub(LINES - 1)),
            Action::PageDown => self.scroll_to(text, self.top + LINES - 1),
            Action::End => self.scroll_to(text, u64::MAX),
            Action::Previous | Action::Next => {}
        }
    }
}

fn in_log(y: i32) -> bool {
    (BAR..H - BUTTONS).contains(&y)
}

/// The button at (x, y), if any.
pub fn hit(x: i32, y: i32) -> Option<Action> {
    if y < BAR {
        return if x < ARROW {
            Some(Action::Previous)
        } else if x >= W - ARROW {
            Some(Action::Next)
        } else {
            None
        };
    }
    if y >= H - BUTTONS {
        return Some(BUTTON_ROW[(x.clamp(0, W - 1) * BUTTON_ROW.len() as i32 / W) as usize].1);
    }
    None
}

const BUTTON_ROW: [(&str, Action); 4] =
    [("TOP", Action::Top), ("PAGE UP", Action::PageUp), ("PAGE DOWN", Action::PageDown), ("END", Action::End)];

/// The bar's words: the log's name, and where it is (CHUMBY or HOST, its
/// place among them all).
pub struct Bar<'a> {
    pub name: &'a str,
    pub place: &'a str,
}

/// The bar and the buttons, which change only with the log shown or with
/// following.
pub fn draw_frame<D: DrawTarget<Color = Rgb565>>(d: &mut D, bar: &Bar, follow: bool) -> Result<(), D::Error> {
    Rectangle::new(Point::zero(), Size::new(W as u32, BAR as u32))
        .into_styled(PrimitiveStyle::with_fill(BAR_BG))
        .draw(d)?;
    let arrow = PrimitiveStyle::with_fill(LABEL);
    let (mid, half) = (BAR / 2, 5);
    let l = ARROW / 2;
    Triangle::new(Point::new(l - half, mid), Point::new(l + half, mid - half), Point::new(l + half, mid + half))
        .into_styled(arrow)
        .draw(d)?;
    let r = W - ARROW / 2;
    Triangle::new(Point::new(r + half, mid), Point::new(r - half, mid - half), Point::new(r - half, mid + half))
        .into_styled(arrow)
        .draw(d)?;
    let centred = TextStyleBuilder::new().alignment(Alignment::Center).baseline(Baseline::Middle).build();
    let name: String = bar.name.chars().take(((W - 2 * ARROW) / 6) as usize).collect();
    Label::with_text_style(&name, Point::new(W / 2, 6), MonoTextStyle::new(&FONT_6X10, LABEL), centred).draw(d)?;
    Label::with_text_style(bar.place, Point::new(W / 2, 15), MonoTextStyle::new(&FONT_5X8, FG), centred).draw(d)?;

    let w = W / BUTTON_ROW.len() as i32;
    for (i, (label, action)) in BUTTON_ROW.iter().enumerate() {
        let x = i as i32 * w;
        let lit = follow && *action == Action::End;
        Rectangle::new(Point::new(x + 1, H - BUTTONS + 2), Size::new(w as u32 - 2, BUTTONS as u32 - 3))
            .into_styled(PrimitiveStyle::with_fill(if lit { LIT } else { BUTTON }))
            .draw(d)?;
        Label::with_text_style(
            label,
            Point::new(x + w / 2, H - BUTTONS / 2),
            MonoTextStyle::new(&FONT_6X10, LABEL),
            centred,
        )
        .draw(d)?;
    }
    Ok(())
}

/// The log's rows from the view's top.
pub fn draw_log<D: DrawTarget<Color = Rgb565>>(d: &mut D, text: &Text, view: &View) -> Result<(), D::Error> {
    Rectangle::new(Point::new(0, BAR), Size::new(W as u32, (H - BAR - BUTTONS) as u32))
        .into_styled(PrimitiveStyle::with_fill(BG))
        .draw(d)?;
    for i in 0..LINES {
        let Some(row) = text.get(view.top + i) else { break };
        let colour = match row.level {
            Level::Plain => FG,
            Level::Warning => WARNING,
            Level::Error => ERROR,
        };
        let at = Point::new(0, TOP_Y + i as i32 * LINE_H);
        Label::with_baseline(&row.text, at, MonoTextStyle::new(&FONT_5X8, colour), Baseline::Top).draw(d)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: usize) -> Text {
        let mut t = Text::new();
        for i in 0..lines {
            t.push(format!("line {i}\n").as_bytes());
        }
        t
    }

    #[test]
    fn follows_the_end_until_scrolled_back() {
        let mut t = text(100);
        let mut v = View::new();
        v.settle(&t);
        assert_eq!(v.top, 100 - LINES);
        v.act(Action::PageUp, &t);
        assert!(!v.follow);
        let top = v.top;
        t.push(b"more\n");
        v.settle(&t);
        assert_eq!(v.top, top);
        v.act(Action::End, &t);
        assert!(v.follow);
        t.push(b"more\n");
        v.settle(&t);
        assert_eq!(v.top, 102 - LINES);
    }

    #[test]
    fn a_short_log_stays_at_the_top() {
        let t = text(3);
        let mut v = View::new();
        v.settle(&t);
        assert_eq!(v.top, 0);
        v.act(Action::PageDown, &t);
        assert_eq!(v.top, 0);
        assert!(v.follow);
    }

    #[test]
    fn dragging_scrolls_and_taps_press() {
        let t = text(200);
        let mut v = View::new();
        v.settle(&t);
        let end = v.top;
        // Down the screen: back through the log, a row each LINE_H.
        assert_eq!(v.touch(Touch::Down(100, 60), &t), (false, None));
        assert_eq!(v.touch(Touch::Down(100, 60 + 10 * LINE_H), &t), (true, None));
        assert_eq!(v.top, end - 10);
        assert!(!v.follow);
        // A drag ends without pressing anything.
        assert_eq!(v.touch(Touch::Up, &t), (false, None));
        // A tap on END.
        v.touch(Touch::Down(300, H - 5), &t);
        assert_eq!(v.touch(Touch::Up, &t), (false, Some(Action::End)));
        v.touch(Touch::Down(5, 5), &t);
        assert_eq!(v.touch(Touch::Up, &t).1, Some(Action::Previous));
        v.touch(Touch::Down(W - 5, 5), &t);
        assert_eq!(v.touch(Touch::Up, &t).1, Some(Action::Next));
        // A tap on the log itself does nothing.
        v.touch(Touch::Down(100, 100), &t);
        assert_eq!(v.touch(Touch::Up, &t).1, None);
    }

    #[test]
    fn buttons() {
        assert_eq!(hit(10, H - 10), Some(Action::Top));
        assert_eq!(hit(100, H - 10), Some(Action::PageUp));
        assert_eq!(hit(200, H - 10), Some(Action::PageDown));
        assert_eq!(hit(319, H - 1), Some(Action::End));
        assert_eq!(hit(160, 5), None);
    }
}
