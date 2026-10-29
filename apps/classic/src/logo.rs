//! The logo intros (`logofx`): each time the logo's page comes on, a
//! demoscene-style intro builds the logo, a shine sweeps across it, and it
//! holds still at its home position. Every intro's last frame is exactly
//! the held logo, so nothing jumps; run with time reversed, an intro is its
//! own outro, at the same cost per frame.
//!
//! Everything is drawn from tables made once when the logo is loaded: its
//! opaque runs row by row (the sharp logo is a few hundred copies), its
//! 4x4 blocks, its glyphs (column groups split by empty columns: the
//! octopus, then c h u m b y), and a random threshold per 2x2 cell.
//! Integer maths only.

use crate::draw::{KEY, hash, rgb565, sin};
#[cfg(test)]
use crate::manifest::NFX;
use crate::manifest::{FX_NAMES, H, W};

const B: i32 = 4;
const MAXG: usize = 16;

pub const FX_FLY: i32 = 0;
pub const FX_PRINT: i32 = 1;
pub const FX_WOBBLE: i32 = 2;
pub const FX_ZOOM: i32 = 3;
pub const FX_DROP: i32 = 4;
pub const FX_DISSOLVE: i32 = 5;
pub const FX_RASTER: i32 = 6;
pub const FX_DECRUNCH: i32 = 7;
pub const FX_SPIN: i32 = 8;
pub const FX_OCTOPUS: i32 = 9;

pub const FX_SHINE: i64 = 900;
pub const FX_LOOP_GAP: i64 = 1500;
/// Empty sky between an outro and the next intro.
pub const FX_OUT_GAP: i64 = 700;
const FX_PRINT_BLINK: i64 = 900;
const FX_DROP_STAGGER: i64 = 140;
const FX_DROP_FALL: i64 = 750;
const FX_OCTO_SWIM: i64 = 1300;
const FX_OCTO_POP0: i64 = 1000;
const FX_OCTO_STAGGER: i64 = 140;
const FX_OCTO_POP: i64 = 420;

/// 0..256 to 256ths, 33 points: easeOutBounce, and a damped spring that
/// overshoots to 1.18.
const BOUNCE: [i32; 33] = [
    0, 2, 8, 17, 30, 47, 68, 93, 121, 153, 189, 229, 248, 230, 215, 203, 196, 192, 193, 197, 204, 216, 231, 250, 249,
    243, 240, 241, 246, 255, 253, 252, 256,
];
const SPRING: [i32; 33] = [
    0, 50, 105, 159, 207, 245, 274, 292, 301, 303, 300, 294, 286, 277, 269, 262, 256, 252, 250, 249, 249, 249, 250,
    252, 253, 254, 255, 256, 257, 257, 257, 257, 256,
];

pub fn name(fx: i32) -> &'static str {
    FX_NAMES.get(fx as usize).copied().unwrap_or("-")
}

/// What the intros need, derived from the logo's pixels.
#[derive(Default)]
pub struct Index {
    /// The 4x4 blocks: colour and position.
    pub bc: Vec<u16>,
    pub bx: Vec<i32>,
    pub by: Vec<i32>,
    /// The logo's main colour: flying and mosaic blocks use it, so the
    /// eyes and anti-aliased edges don't show as stray squares.
    pub solid: u16,
    /// Opaque runs, in row order.
    pub ry: Vec<i32>,
    pub rx: Vec<i32>,
    pub rl: Vec<i32>,
    pub ng: i32,
    pub gx0: [i32; MAXG],
    pub gx1: [i32; MAXG],
    pub gy0: [i32; MAXG],
    pub gy1: [i32; MAXG],
    /// The rows the wordmark spans (the print cursor's height).
    pub ty0: i32,
    pub ty1: i32,
    /// A dissolve threshold per 2x2 cell, 1..255.
    pub thr: Vec<u8>,
}

pub fn index(px: &[u16], b: i32, c: i32) -> Index {
    let p = |x: i32, y: i32| px[(y * b + x) as usize];
    let mut ix = Index::default();
    let mut yy = 0;
    while yy + B <= c {
        let mut xx = 0;
        while xx + B <= b {
            let col = p(xx + B / 2, yy + B / 2);
            if col != KEY {
                ix.bc.push(col);
                ix.bx.push(xx);
                ix.by.push(yy);
            }
            xx += B;
        }
        yy += B;
    }
    let mut best = 0;
    ix.solid = ix.bc.first().copied().unwrap_or(0);
    for i in (0..ix.bc.len()).step_by(2) {
        let n = (0..ix.bc.len()).step_by(2).filter(|&j| ix.bc[j] == ix.bc[i]).count();
        if n > best {
            best = n;
            ix.solid = ix.bc[i];
        }
    }
    for y in 0..c {
        let mut x = 0;
        while x < b {
            if p(x, y) == KEY {
                x += 1;
                continue;
            }
            let s0 = x;
            while x < b && p(x, y) != KEY {
                x += 1;
            }
            ix.ry.push(y);
            ix.rx.push(s0);
            ix.rl.push(x - s0);
        }
    }
    // Glyphs: groups of occupied columns; slivers (the faint (R)) join the
    // glyph before.
    let merge_sliver = |ix: &mut Index| {
        let g = (ix.ng - 1) as usize;
        if ix.ng > 1 && ix.gx1[g] - ix.gx0[g] < 3 {
            ix.gx1[g - 1] = ix.gx1[g];
            ix.gy0[g - 1] = ix.gy0[g - 1].min(ix.gy0[g]);
            ix.gy1[g - 1] = ix.gy1[g - 1].max(ix.gy1[g]);
            ix.ng -= 1;
        }
    };
    for x in 0..b {
        let (mut y0, mut y1) = (-1, -1);
        for y in 0..c {
            if p(x, y) != KEY {
                if y0 < 0 {
                    y0 = y;
                }
                y1 = y;
            }
        }
        if y0 < 0 {
            continue;
        }
        let mut g = ix.ng - 1;
        if g < 0 || ix.gx1[g as usize] < x - 1 {
            // A gap before this column: a new glyph (merging a sliver
            // before it back first, which frees its slot).
            if g > 0 && ix.gx1[g as usize] - ix.gx0[g as usize] < 3 {
                merge_sliver(&mut ix);
            }
            if (ix.ng as usize) < MAXG {
                g = ix.ng;
                ix.ng += 1;
                let gu = g as usize;
                ix.gx0[gu] = x;
                ix.gy0[gu] = y0;
                ix.gy1[gu] = y1;
            }
        }
        let gu = g as usize;
        ix.gx1[gu] = x;
        ix.gy0[gu] = ix.gy0[gu].min(y0);
        ix.gy1[gu] = ix.gy1[gu].max(y1);
    }
    if ix.ng > 0 {
        merge_sliver(&mut ix);
    }
    ix.ty0 = c;
    ix.ty1 = 0;
    for g in (if ix.ng > 1 { 1 } else { 0 })..ix.ng as usize {
        ix.ty0 = ix.ty0.min(ix.gy0[g]);
        ix.ty1 = ix.ty1.max(ix.gy1[g]);
    }
    let cells = ((b + 1) / 2 * ((c + 1) / 2)) as u32;
    ix.thr = (0..cells).map(|i| (1 + hash(i.wrapping_mul(2654435761).wrapping_add(77)) % 255) as u8).collect();
    ix
}

fn ease_out3(p: i32) -> i32 {
    let q = 256 - p;
    256 - q * q / 256 * q / 256
}

fn tab_ease(tab: &[i32; 33], p: i32) -> i32 {
    if p <= 0 {
        return tab[0];
    }
    if p >= 256 {
        return tab[32];
    }
    let i = (p >> 3) as usize;
    tab[i] + (tab[i + 1] - tab[i]) * (p & 7) / 8
}

fn prog(tc: i64, d: i64) -> i32 {
    if tc >= d {
        256
    } else if tc <= 0 {
        0
    } else {
        (tc * 256 / d) as i32
    }
}

fn fill_rect(back: &mut [u16], mut x: i32, mut y: i32, mut w: i32, mut h: i32, c: u16) {
    if x < 0 {
        w += x;
        x = 0;
    }
    if y < 0 {
        h += y;
        y = 0;
    }
    if x + w > W {
        w = W - x;
    }
    if y + h > H {
        h = H - y;
    }
    for r in y..y + h {
        for q in x..x + w {
            back[(r * W + q) as usize] = c;
        }
    }
}

/// One logo on its page, ready to draw.
pub struct Logo<'a> {
    pub px: &'a [u16],
    pub ix: &'a Index,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub seed: u32,
}

impl Logo<'_> {
    fn print_slot(&self, g: i32) -> i64 {
        if g == 0 && self.ix.gx1[0] - self.ix.gx0[0] > 40 { 460 } else { 210 }
    }

    /// How long an intro takes, in ms.
    pub fn dur(ix: &Index, fx: i32) -> i64 {
        let slot = |g: i32| if g == 0 && ix.gx1[0] - ix.gx0[0] > 40 { 460 } else { 210 };
        match fx {
            FX_FLY => 2400,
            FX_PRINT => (0..ix.ng).map(slot).sum::<i64>() + FX_PRINT_BLINK,
            FX_WOBBLE => 2200,
            FX_ZOOM => 1500,
            FX_DROP => (ix.ng - 1).max(0) as i64 * FX_DROP_STAGGER + FX_DROP_FALL,
            FX_DISSOLVE => 1600,
            FX_RASTER => 1900,
            FX_DECRUNCH => 1700,
            FX_SPIN => 2200,
            FX_OCTOPUS => {
                if ix.ng > 1 {
                    FX_OCTO_POP0 + (ix.ng - 2) as i64 * FX_OCTO_STAGGER + FX_OCTO_POP
                } else {
                    FX_OCTO_SWIM
                }
            }
            _ => 0,
        }
    }

    /// The sharp logo's columns [c0,c1) of rows [r0,r1), drawn (ox,oy) away
    /// from home, clipped.
    #[allow(clippy::too_many_arguments)]
    pub fn part(&self, back: &mut [u16], c0: i32, c1: i32, r0: i32, r1: i32, ox: i32, oy: i32) {
        let ix = self.ix;
        for k in 0..ix.ry.len() {
            let y = ix.ry[k];
            if y < r0 {
                continue;
            }
            if y >= r1 {
                break;
            }
            let yy = self.y + oy + y;
            if !(0..H).contains(&yy) {
                continue;
            }
            let (mut a, mut b) = (ix.rx[k], ix.rx[k] + ix.rl[k]);
            a = a.max(c0);
            b = b.min(c1);
            let mut xx = self.x + ox + a;
            if xx < 0 {
                a -= xx;
                xx = 0;
            }
            if xx + (b - a) > W {
                b = a + W - xx;
            }
            if b > a {
                let d = (yy * W + xx) as usize;
                let s = (y * self.w + a) as usize;
                let n = (b - a) as usize;
                back[d..d + n].copy_from_slice(&self.px[s..s + n]);
            }
        }
    }

    /// The whole sharp logo at home.
    pub fn sharp(&self, back: &mut [u16]) {
        self.part(back, 0, self.w, 0, self.h, 0, 0);
    }

    /// The logo at m x m pixel blocks of its main colour.
    fn mosaic(&self, back: &mut [u16], m: i32) {
        if m <= 1 {
            self.sharp(back);
            return;
        }
        let (w, h) = (self.w, self.h);
        let mut yy = 0;
        while yy < h {
            let mut xx = 0;
            while xx < w {
                let sx = if xx + m / 2 < w { xx + m / 2 } else { w - 1 };
                let sy = if yy + m / 2 < h { yy + m / 2 } else { h - 1 };
                if self.px[(sy * w + sx) as usize] != KEY {
                    let cw = if xx + m <= w { m } else { w - xx };
                    let ch = if yy + m <= h { m } else { h - yy };
                    fill_rect(back, self.x + xx, self.y + yy, cw, ch, self.ix.solid);
                }
                xx += m;
            }
            yy += m;
        }
    }

    /// Pixels fly in from all over, then resolve.
    fn fly(&self, back: &mut [u16], tc: i64) {
        let ix = self.ix;
        if tc < 1800 {
            let e = ease_out3(prog(tc, 1800));
            for i in 0..ix.bc.len() {
                let r = hash((i as u32).wrapping_mul(2654435761).wrapping_add(self.seed));
                let sx = (r % (W as u32 + 80)) as i32 - 40;
                let sy = ((r >> 12) % (H as u32 + 80)) as i32 - 40;
                let (tx, ty) = (self.x + ix.bx[i], self.y + ix.by[i]);
                fill_rect(back, sx + (tx - sx) * e / 256, sy + (ty - sy) * e / 256, B, B, ix.solid);
            }
        } else {
            self.mosaic(back, B - ((tc - 1800) * B as i64 / 600) as i32); // 4 -> 3 -> 2 -> sharp
        }
    }

    /// A dot-matrix printer: glyph by glyph, row by row.
    fn print(&self, back: &mut [u16], tc: i64) {
        let ix = self.ix;
        let mut u = tc;
        let mut g = 0;
        while g < ix.ng && u >= self.print_slot(g) {
            u -= self.print_slot(g);
            g += 1;
        }
        // The glyphs already printed.
        self.part(back, 0, if g < ix.ng { ix.gx0[g as usize] } else { self.w }, 0, self.h, 0, 0);
        let cx;
        let mut on = true;
        if g < ix.ng {
            let gu = g as usize;
            let slot = self.print_slot(g) - 50; // 50 ms for the carriage to move
            let (y0, y1) = (ix.gy0[gu], ix.gy1[gu] + 1);
            let head = if u >= slot { y1 } else { y0 + ((y1 - y0) as i64 * u / slot) as i32 };
            self.part(back, ix.gx0[gu], ix.gx1[gu] + 1, 0, head, 0, 0);
            if u < slot {
                // The print head: a bright bar on the row being printed.
                let bw = ix.gx1[gu] - ix.gx0[gu] + 5;
                fill_rect(back, self.x + ix.gx0[gu] - 2, self.y + head, bw, 2, rgb565(170, 255, 235));
            }
            cx = ix.gx1[gu] + 4;
        } else {
            cx = ix.gx1[(ix.ng - 1) as usize] + 4;
            on = (u / 150) & 1 == 0 && u < FX_PRINT_BLINK - 150; // blinks, then goes
        }
        if on {
            fill_rect(back, self.x + cx, self.y + ix.ty0, 7, ix.ty1 - ix.ty0 + 1, ix.solid);
        }
    }

    /// A sine scroller: slides in wobbling, and settles.
    fn wobble(&self, back: &mut [u16], tc: i64) {
        let ix = self.ix;
        let p = prog(tc, 2200);
        let e = ease_out3(p);
        let q = 256 - p;
        let ox = (W - self.x) * (256 - e) / 256;
        let amp = 26 * q / 256 * q / 256; // the wobble dies after the slide has stopped
        let ph = (tc / 3) as i32;
        let mut dy = vec![0i32; self.w.max(W) as usize];
        for (c, d) in dy.iter_mut().enumerate().take(self.w.min(W) as usize) {
            *d = amp * sin(c as i32 * 5 - ph) / 127;
        }
        for k in 0..ix.ry.len() {
            let (y, a, b) = (ix.ry[k], ix.rx[k], ix.rx[k] + ix.rl[k]);
            for x in a..b {
                let xx = self.x + ox + x;
                let yy = self.y + y + dy[x as usize];
                if (0..W).contains(&xx) && (0..H).contains(&yy) {
                    back[(yy * W + xx) as usize] = self.px[(y * self.w + x) as usize];
                }
            }
        }
    }

    /// Grows from a dot on a spring, overshoots, settles.
    fn zoom(&self, back: &mut [u16], tc: i64) {
        let s = tab_ease(&SPRING, prog(tc, 1500));
        let (mut dw, mut dh) = (self.w * s / 256, self.h * s / 256);
        if dw < 1 || dh < 1 {
            return;
        }
        dw = dw.min(W);
        dh = dh.min(H);
        let x0 = self.x + self.w / 2 - dw / 2;
        let y0 = self.y + self.h / 2 - dh / 2;
        let sx = (self.w << 16) / dw;
        let sy = (self.h << 16) / dh;
        let i0 = if x0 < 0 { -x0 } else { 0 };
        let i1 = if x0 + dw > W { W - x0 } else { dw };
        let mut cm = vec![0i32; W as usize];
        let mut acc = i0 * sx;
        for c in cm.iter_mut().take(i1.max(0) as usize).skip(i0 as usize) {
            *c = acc >> 16;
            acc += sx;
        }
        let mut acc = 0;
        for j in 0..dh {
            let yy = y0 + j;
            if (0..H).contains(&yy) {
                let src = ((acc >> 16) * self.w) as usize;
                for i in i0..i1 {
                    let c = self.px[src + cm[i as usize] as usize];
                    if c != KEY {
                        back[(yy * W + x0 + i) as usize] = c;
                    }
                }
            }
            acc += sy;
        }
    }

    /// Glyph by glyph, dropped from above, bouncing.
    fn drop(&self, back: &mut [u16], tc: i64) {
        let ix = self.ix;
        for g in 0..ix.ng {
            let u = tc - g as i64 * FX_DROP_STAGGER;
            if u < 0 {
                break;
            }
            let b = tab_ease(&BOUNCE, prog(u, FX_DROP_FALL));
            let gu = g as usize;
            let oy = -(self.y + self.h + 8) * (256 - b) / 256;
            self.part(back, ix.gx0[gu], ix.gx1[gu] + 1, 0, self.h, 0, oy);
        }
    }

    /// A chunky 2x2 random reveal; new cells glint.
    fn dissolve(&self, back: &mut [u16], tc: i64) {
        let ix = self.ix;
        let p = (tc * 288 / 1600) as i32;
        let cw = (self.w + 1) / 2;
        let glint = rgb565(150, 255, 235);
        for k in 0..ix.ry.len() {
            let (y, a, b) = (ix.ry[k], ix.rx[k], ix.rx[k] + ix.rl[k]);
            let yy = self.y + y;
            if !(0..H).contains(&yy) {
                continue;
            }
            let th = ((y >> 1) * cw) as usize;
            for x in a..b {
                let d = p - ix.thr[th + (x >> 1) as usize] as i32;
                let xx = self.x + x;
                if d >= 0 && (0..W).contains(&xx) {
                    back[(yy * W + xx) as usize] = if d < 24 { glint } else { self.px[(y * self.w + x) as usize] };
                }
            }
        }
    }

    /// Two copper bars sweep down and leave the logo behind.
    fn raster(&self, back: &mut [u16], tc: i64) {
        let u = tc.min(1500);
        let cy = self.y - 30 + ((self.h + 60) as i64 * u / 1500) as i32;
        let rows = cy - self.y;
        if rows > 0 {
            self.part(back, 0, self.w, 0, rows.min(self.h), 0, 0);
        }
        let lvl = if tc < 1500 { 8 } else { 8 - ((tc - 1500) * 8 / 400) as i32 };
        copper_bar(back, cy - 18, 150, 220, 70, lvl); // lime, trailing
        copper_bar(back, cy, 140, 245, 235, lvl); // aqua, leading
    }

    /// An Amiga decrunch: flickering colour bars resolve, glitching.
    fn decrunch(&self, back: &mut [u16], tc: i64) {
        const PAL: [u16; 8] = [0xFFFF, 0x8F9C, 0x2C70, 0xA6E7, 0xFF2F, 0xF3D9, 0x547F, 0x0946];
        let x0 = (self.x - 12).max(0);
        let x1 = (self.x + self.w + 12).min(W);
        let fr = tc / 40;
        for r in -6..self.h + 6 {
            let yy = self.y + r;
            if !(0..H).contains(&yy) {
                continue;
            }
            let res = 150 + (hash(((r + 6) as u32).wrapping_mul(7919).wrapping_add(self.seed)) % 1350) as i64;
            if tc < res {
                let hv = (((r + 6) >> 1) as u32).wrapping_mul(131) ^ (fr as u32).wrapping_mul(0x9E3779B9) ^ self.seed;
                let c = PAL[(hash(hv) & 7) as usize];
                if x1 > x0 {
                    back[(yy * W + x0) as usize..(yy * W + x1) as usize].fill(c);
                }
            } else if r >= 0 && r < self.h {
                let jit = if tc - res < 160 {
                    (hash((r as u32).wrapping_mul(31).wrapping_add(fr as u32)) % 13) as i32 - 6
                } else {
                    0
                };
                self.part(back, 0, self.w, r, r + 1, jit, 0);
            }
        }
    }

    /// A rotozoom: spins in from a dot, two turns.
    fn spin(&self, back: &mut [u16], tc: i64) {
        let e = ease_out3(prog(tc, 2200));
        let ang = (256 - e) * 2; // sin units, 256 a turn
        let s = 20 + 236 * e / 256; // scale, in 256ths
        let (sn, cs) = (sin(ang), sin(ang + 64));
        // Source pixels per screen pixel per unit of sin, 16.16.
        let k = ((65536i64 * 256) / (127 * s as i64)) as i32;
        let (dux, dvx, duy, dvy) = (cs * k, -sn * k, sn * k, cs * k);
        let (hw, hh) = (self.w / 2, self.h / 2);
        let (cx, cy) = (self.x + hw, self.y + hh);
        let (as_, ac) = (sn.abs(), cs.abs());
        let ex = (ac * hw + as_ * hh) * s / (127 * 256) + 2;
        let ey = (as_ * hw + ac * hh) * s / (127 * 256) + 2;
        let xa = (cx - ex).max(0);
        let xb = if cx + ex >= W { W - 1 } else { cx + ex };
        for yy in cy - ey..=cy + ey {
            if !(0..H).contains(&yy) {
                continue;
            }
            let (dx, dy) = (xa - cx, yy - cy);
            let mut u = dx.wrapping_mul(dux).wrapping_add(dy.wrapping_mul(duy)).wrapping_add((hw << 16) + 0x8000);
            let mut v = dx.wrapping_mul(dvx).wrapping_add(dy.wrapping_mul(dvy)).wrapping_add((hh << 16) + 0x8000);
            for xx in xa..=xb {
                let (su, sv) = ((u >> 16) as u32, (v >> 16) as u32);
                if su < self.w as u32 && sv < self.h as u32 {
                    let c = self.px[(sv * self.w as u32 + su) as usize];
                    if c != KEY {
                        back[(yy * W + xx) as usize] = c;
                    }
                }
                u = u.wrapping_add(dux);
                v = v.wrapping_add(dvx);
            }
        }
    }

    /// The octopus swims in, then the letters pop up.
    fn octopus(&self, back: &mut [u16], tc: i64) {
        let ix = self.ix;
        let e = ease_out3(prog(tc, FX_OCTO_SWIM));
        let ox = -(self.x + ix.gx1[0] + 6) * (256 - e) / 256;
        let oy = 12 * sin((tc * 256 / 500) as i32) / 127 * (256 - e) / 256;
        self.part(back, ix.gx0[0], ix.gx1[0] + 1, 0, self.h, ox, oy);
        for g in 1..ix.ng {
            // Each rises out of the baseline, springing.
            let u = tc - FX_OCTO_POP0 - (g - 1) as i64 * FX_OCTO_STAGGER;
            if u < 0 {
                break;
            }
            let gu = g as usize;
            let b = tab_ease(&SPRING, prog(u, FX_OCTO_POP));
            let dy = 18 * (256 - b) / 256;
            let r1 = if dy > 0 { ix.gy1[gu] + 1 - dy } else { self.h };
            if r1 > 0 {
                self.part(back, ix.gx0[gu], ix.gx1[gu] + 1, 0, r1, 0, dy);
            }
        }
    }

    /// The held logo, a shine sweeping across it.
    pub fn shine(&self, back: &mut [u16], tc: i64) {
        let ix = self.ix;
        self.sharp(back);
        let u = -30 + (tc * (self.w + 60) as i64 / FX_SHINE) as i32;
        for k in 0..ix.ry.len() {
            let (y, a, b) = (ix.ry[k], ix.rx[k], ix.rx[k] + ix.rl[k]);
            // The band: x + y/2 - u in [0, 10).
            let s0 = (u - y / 2).max(a);
            let s1 = (u - y / 2 + 10).min(b);
            let yy = self.y + y;
            if !(0..H).contains(&yy) {
                continue;
            }
            for x in s0..s1 {
                let xx = self.x + x;
                if (0..W).contains(&xx) {
                    // Halfway to white.
                    back[(yy * W + xx) as usize] = ((self.px[(y * self.w + x) as usize] & 0xF7DE) >> 1) + 0x7BEF;
                }
            }
        }
    }

    pub fn run(&self, back: &mut [u16], fx: i32, tc: i64) {
        match fx {
            FX_FLY => self.fly(back, tc),
            FX_PRINT => self.print(back, tc),
            FX_WOBBLE => self.wobble(back, tc),
            FX_ZOOM => self.zoom(back, tc),
            FX_DROP => self.drop(back, tc),
            FX_DISSOLVE => self.dissolve(back, tc),
            FX_RASTER => self.raster(back, tc),
            FX_DECRUNCH => self.decrunch(back, tc),
            FX_SPIN => self.spin(back, tc),
            FX_OCTOPUS => self.octopus(back, tc),
            _ => {}
        }
    }
}

/// A copper bar: 15 rows, brightest in the middle, added across the screen.
fn copper_bar(back: &mut [u16], cy: i32, r: i32, g: i32, b: i32, lvl: i32) {
    for i in -7..=7 {
        let yy = cy + i;
        if !(0..H).contains(&yy) || lvl <= 0 {
            continue;
        }
        let v = sin((i + 8) * 8); // half a sine: 0..127..0
        let c = rgb565(r * v / 127, g * v / 127, b * v / 127);
        if c == 0 {
            continue;
        }
        for d in &mut back[(yy * W) as usize..((yy + 1) * W) as usize] {
            crate::draw::add_light(d, c, lvl);
        }
    }
}

/// xorshift32, for picking intros.
pub fn rand(state: &mut u32, seed_us: i64) -> u32 {
    if *state == 0 {
        *state = seed_us as u32 | 1;
    }
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    *state
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A logo: a 40x20 block (the "octopus"), a gap, two 8x12 letters and a
    /// 2-wide sliver after the last.
    fn logo() -> (Vec<u16>, i32, i32) {
        let (w, h) = (80, 20);
        let mut px = vec![KEY; (w * h) as usize];
        let mut fill = |x0: i32, x1: i32, y0: i32, y1: i32, c: u16| {
            for y in y0..y1 {
                for x in x0..x1 {
                    px[(y * w + x) as usize] = c;
                }
            }
        };
        fill(0, 44, 0, 20, 0x1234);
        fill(48, 56, 6, 18, 0x1234);
        fill(60, 68, 4, 18, 0x1234);
        fill(70, 72, 2, 4, 0xFFFF);
        (px, w, h)
    }

    #[test]
    fn glyphs_and_runs() {
        let (px, w, h) = logo();
        let ix = index(&px, w, h);
        assert_eq!(ix.ng, 3, "the sliver joins the last letter");
        assert_eq!((ix.gx0[0], ix.gx1[0]), (0, 43));
        assert_eq!((ix.gx0[1], ix.gx1[1]), (48, 55));
        assert_eq!((ix.gx0[2], ix.gx1[2], ix.gy0[2]), (60, 71, 2));
        assert_eq!((ix.ty0, ix.ty1), (2, 17));
        assert_eq!(ix.solid, 0x1234);
        assert_eq!(ix.ry.len(), 20 + 12 + 14 + 2);
        assert!(ix.thr.iter().all(|&t| t >= 1));
        assert_eq!(Logo::dur(&ix, FX_PRINT), 460 + 210 + 210 + 900);
        assert_eq!(Logo::dur(&ix, FX_OCTOPUS), 1000 + 140 + 420);
    }

    #[test]
    fn every_intro_ends_on_the_sharp_logo() {
        let (px, w, h) = logo();
        let ix = index(&px, w, h);
        let l = Logo { px: &px, ix: &ix, x: 100, y: 100, w, h, seed: 12345 };
        let mut want = vec![0u16; (W * H) as usize];
        l.sharp(&mut want);
        for fx in 0..NFX {
            let mut got = vec![0u16; (W * H) as usize];
            l.run(&mut got, fx, Logo::dur(&ix, fx));
            assert!(got == want, "{} doesn't end on the logo", name(fx));
            // And nothing drawn mid-way escapes the screen.
            for t in (0..Logo::dur(&ix, fx)).step_by(37) {
                l.run(&mut got, fx, t);
            }
        }
    }
}
