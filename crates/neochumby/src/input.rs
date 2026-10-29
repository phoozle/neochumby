//! The Chumby Classic's inputs: the resistive touchscreen (TSC2100, evdev on
//! /dev/input/event0), the squeeze switch (/dev/switch, one byte) and the
//! accelerometer (/dev/accel, 56-byte records). Neither Chumby driver
//! supports poll(), so everything is read non-blocking, each tick.
//!
//! A touch can also be made up: evdev takes input_event records written to
//! /dev/input/event0 and hands them to every reader as if the touchscreen
//! had sent them ([`touch_down`], [`touch_up`], at [`Calibration::raw`]).
//! The developer link does this for `neochumbyd tap`.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::RawFd;
use std::os::unix::fs::OpenOptionsExt;

use neochumby_sys::fd;

use crate::fb;

/// The touchscreen's input device.
pub const TOUCHSCREEN: &str = "/dev/input/event0";
/// The stock firmware's touchscreen calibration ([`Calibration`]).
pub const CALIBRATION: &str = "/psp/ts_settings";

/// The variable holding the top button's descriptor, which the session
/// hands to an app whose manifest says `button = true`.
pub const BUTTON_ENV: &str = "NEOCHUMBY_BUTTON";

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_PRESSURE: u16 = 0x18;
const BTN_TOUCH: u16 = 0x14a;
/// What a made-up touch reports as its pressure; anything above 0 is down.
const PRESSURE: i32 = 200;

fn open_nonblock(path: &str) -> Option<File> {
    OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path).ok()
}

/// Raw touchscreen coordinates to screen pixels, from the stock firmware's
/// own calibration in /psp/ts_settings: "x0,xspan,y0,yspan,swap" with a
/// negative span for an inverted axis.
#[derive(Clone, Copy, Debug)]
pub struct Calibration {
    x0: i32,
    xspan: i32,
    y0: i32,
    yspan: i32,
    swap: bool,
}

impl Calibration {
    pub fn load() -> Calibration {
        let fallback = Calibration { x0: 169, xspan: 3733, y0: 4082, yspan: -4054, swap: false };
        let Ok(s) = std::fs::read_to_string(CALIBRATION) else { return fallback };
        let v: Vec<i32> = s.trim().split(',').filter_map(|f| f.trim().parse().ok()).collect();
        match v[..] {
            [x0, xspan, y0, yspan, swap] if xspan != 0 && yspan != 0 => {
                Calibration { x0, xspan, y0, yspan, swap: swap != 0 }
            }
            _ => fallback,
        }
    }

    pub fn map(&self, rx: i32, ry: i32) -> (i32, i32) {
        let (rx, ry) = if self.swap { (ry, rx) } else { (rx, ry) };
        let x = (rx - self.x0) * fb::W as i32 / self.xspan;
        let y = (ry - self.y0) * fb::H as i32 / self.yspan;
        (x.clamp(0, fb::W as i32 - 1), y.clamp(0, fb::H as i32 - 1))
    }

    /// The raw reading [`Calibration::map`] takes to pixel (x, y): the
    /// pixel's middle, so that `map` (which rounds toward zero) comes back
    /// to it for any span of at least two raw steps a pixel, as every real
    /// calibration is (about twelve on the Classic).
    pub fn raw(&self, x: i32, y: i32) -> (i32, i32) {
        let a = self.x0 + (2 * x + 1) * self.xspan / (2 * fb::W as i32);
        let b = self.y0 + (2 * y + 1) * self.yspan / (2 * fb::H as i32);
        if self.swap { (b, a) } else { (a, b) }
    }
}

/// One of the 2.6.16 kernel's 32-bit input_event records: timeval (2 x
/// u32), type u16, code u16, value i32, little-endian. Written ones carry
/// no time: the kernel stamps them.
fn event(ty: u16, code: u16, value: i32) -> [u8; 16] {
    let mut e = [0u8; 16];
    e[8..10].copy_from_slice(&ty.to_le_bytes());
    e[10..12].copy_from_slice(&code.to_le_bytes());
    e[12..16].copy_from_slice(&value.to_le_bytes());
    e
}

/// A raw coordinate far from `v`, still in the 12-bit range.
fn elsewhere(v: i32) -> i32 {
    if v < 2048 { v + 2048 } else { v - 2048 }
}

/// What a finger going down at raw (rx, ry) reports, to write to
/// [`TOUCHSCREEN`]. The input core passes on a coordinate only when it
/// changes (beyond the driver's fuzz), so a reader opened since the last
/// touch there would never learn where this one is: each coordinate is
/// first sent somewhere else, and readers take the last before EV_SYN.
/// Both the pressure and BTN_TOUCH, as the touchscreen's driver may report
/// either.
pub fn touch_down(rx: i32, ry: i32) -> Vec<u8> {
    [
        event(EV_ABS, ABS_X, elsewhere(rx)),
        event(EV_ABS, ABS_Y, elsewhere(ry)),
        event(EV_ABS, ABS_X, rx),
        event(EV_ABS, ABS_Y, ry),
        event(EV_ABS, ABS_PRESSURE, PRESSURE),
        event(EV_KEY, BTN_TOUCH, 1),
        event(EV_SYN, 0, 0),
    ]
    .concat()
}

/// What the finger coming up reports, to write to [`TOUCHSCREEN`].
pub fn touch_up() -> Vec<u8> {
    [event(EV_ABS, ABS_PRESSURE, 0), event(EV_KEY, BTN_TOUCH, 0), event(EV_SYN, 0, 0)].concat()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Touch {
    /// Finger down or moved, in screen pixels, with the raw reading.
    Down {
        x: i32,
        y: i32,
        raw: (i32, i32),
    },
    Up,
}

pub struct Touchscreen {
    file: Option<File>,
    cal: Calibration,
    /// The last raw X and Y, once each has been reported since opening.
    /// evdev sends a coordinate only when it changes, so a reader opened
    /// during a steady touch (as the next app is, after the corner hold)
    /// sees presses before any position, and must not take one for (0, 0).
    raw: (Option<i32>, Option<i32>),
    pressed: bool,
    was_pressed: bool,
}

impl Touchscreen {
    pub fn open() -> Touchscreen {
        Touchscreen::with(open_nonblock(TOUCHSCREEN), Calibration::load())
    }

    fn with(file: Option<File>, cal: Calibration) -> Touchscreen {
        Touchscreen { file, cal, raw: (None, None), pressed: false, was_pressed: false }
    }

    /// Every touch report since the last call, in order.
    pub fn poll(&mut self) -> Vec<Touch> {
        let mut out = Vec::new();
        let mut buf = [0u8; 16 * 32];
        while let Some(Ok(n)) = self.file.as_mut().map(|f| f.read(&mut buf)) {
            if n == 0 {
                break;
            }
            self.take(&buf[..n], &mut out);
        }
        out
    }

    /// The touches in `records`, input_events as [`event`] lays them out.
    fn take(&mut self, records: &[u8], out: &mut Vec<Touch>) {
        for e in records.as_chunks::<16>().0 {
            let ty = u16::from_le_bytes([e[8], e[9]]);
            let code = u16::from_le_bytes([e[10], e[11]]);
            let value = i32::from_le_bytes([e[12], e[13], e[14], e[15]]);
            match (ty, code) {
                (EV_ABS, ABS_X) => self.raw.0 = Some(value),
                (EV_ABS, ABS_Y) => self.raw.1 = Some(value),
                (EV_ABS, ABS_PRESSURE) => self.pressed = value > 0,
                (EV_KEY, BTN_TOUCH) => self.pressed = value != 0,
                (EV_SYN, _) => {
                    if let (true, (Some(rx), Some(ry))) = (self.pressed, self.raw) {
                        let (x, y) = self.cal.map(rx, ry);
                        out.push(Touch::Down { x, y, raw: (rx, ry) });
                        self.was_pressed = true;
                    } else if !self.pressed && self.was_pressed {
                        out.push(Touch::Up);
                        self.was_pressed = false;
                    }
                }
                _ => {}
            }
        }
    }
}

/// The squeeze sensor in the case: true while squeezed.
pub struct Squeeze(Option<File>);

impl Squeeze {
    /// The switch the session handed over ([`BUTTON_ENV`]), else
    /// /dev/switch. Only one process at a time can have /dev/switch open,
    /// and the session keeps it, so an app that wants it says `button =
    /// true` in its manifest.
    pub fn open() -> Squeeze {
        let var = std::env::var(BUTTON_ENV).ok();
        Squeeze(handed_over(var.as_deref()).or_else(|| open_nonblock("/dev/switch")))
    }

    pub fn squeezed(&mut self) -> bool {
        let mut b = [0u8; 1];
        matches!(self.0.as_mut().map(|f| f.read(&mut b)), Some(Ok(1))) && b[0] != 0
    }
}

/// A copy of the switch's descriptor from the session (`var`, the value of
/// [`BUTTON_ENV`]), so that opening it twice doesn't close it twice.
/// Neither is passed on to programs this one runs.
fn handed_over(var: Option<&str>) -> Option<File> {
    let n: RawFd = var?.parse().ok()?;
    fd::handed_over(n).map(File::from)
}

/// One accelerometer record: 14 little-endian u32s, the first a version (3),
/// the second a timestamp, then instantaneous x/y/z, averaged x/y/z and
/// more. About 2048 is zero; 1 g is roughly 830 counts.
pub struct Accel(Option<File>);

impl Accel {
    pub fn open() -> Accel {
        Accel(open_nonblock("/dev/accel"))
    }

    /// The averaged x, y, z, centred on zero.
    pub fn read(&mut self) -> Option<[i32; 3]> {
        let mut b = [0u8; 56];
        let n = self.0.as_mut()?.read(&mut b).ok()?;
        if n < 32 {
            return None;
        }
        let word = |i: usize| u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]) as i32;
        Some([word(5) - 2048, word(6) - 2048, word(7) - 2048])
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;

    use super::*;

    /// The stock calibration (the fallback), and others: swapped axes,
    /// neither inverted, both, and a narrow one.
    fn calibrations() -> [Calibration; 5] {
        let c = |x0, xspan, y0, yspan, swap| Calibration { x0, xspan, y0, yspan, swap };
        [
            c(169, 3733, 4082, -4054, false),
            c(4000, -3800, 150, 3900, true),
            c(100, 3900, 120, 3850, false),
            c(3990, -3870, 4010, -3900, true),
            c(1000, 640, 1000, -480, false),
        ]
    }

    #[test]
    fn pixels_go_to_raw_and_back() {
        for cal in calibrations() {
            for x in 0..fb::W as i32 {
                for y in (0..fb::H as i32).step_by(7).chain([fb::H as i32 - 1]) {
                    let (rx, ry) = cal.raw(x, y);
                    assert!((0..4096).contains(&rx) && (0..4096).contains(&ry), "{cal:?} ({x}, {y}): ({rx}, {ry})");
                    assert_eq!(cal.map(rx, ry), (x, y), "{cal:?}");
                }
            }
        }
    }

    #[test]
    fn a_made_up_touch_reads_as_a_real_one() {
        for cal in calibrations() {
            let mut t = Touchscreen::with(None, cal);
            let mut got = Vec::new();
            let (rx, ry) = cal.raw(10, 200);
            t.take(&touch_down(rx, ry), &mut got);
            t.take(&touch_up(), &mut got);
            // Again at the same place, as a reader opened since sees it.
            let mut fresh = Touchscreen::with(None, cal);
            fresh.take(&touch_down(rx, ry), &mut got);
            assert_eq!(
                got,
                [Touch::Down { x: 10, y: 200, raw: (rx, ry) }, Touch::Up, Touch::Down { x: 10, y: 200, raw: (rx, ry) }]
            );
            assert!(touch_down(rx, ry).len().is_multiple_of(16) && touch_up().len() == 48);
        }
        assert!(Touchscreen::with(None, calibrations()[0]).poll().is_empty());
    }

    #[test]
    fn squeeze_takes_the_button_it_was_handed() {
        let switch = File::open("/dev/null").unwrap();
        let var = switch.as_raw_fd().to_string();
        let mut a = Squeeze(handed_over(Some(&var)));
        let b = Squeeze(handed_over(Some(&var)));
        let fd = |s: &Squeeze| s.0.as_ref().map(|f| f.as_raw_fd());
        // Each a copy of it, not /dev/switch (which a host hasn't got).
        assert!(fd(&a).is_some() && fd(&b).is_some() && fd(&a) != fd(&b));
        assert!(!a.squeezed());
        drop(a);
        drop(b);
        // Still open: the copies were closed, not the one handed over.
        assert!(switch.metadata().is_ok());
        assert!(handed_over(Some("x")).is_none() && handed_over(None).is_none());
    }
}
