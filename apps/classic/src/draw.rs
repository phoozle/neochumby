//! Pixel arithmetic shared by the drawing code: integer only, as the
//! Chumby has no FPU.

/// The transparent colour in sprite sheets and logos (magenta).
pub const KEY: u16 = 0xF81F;

/// A sine wave, 256 steps a turn, amplitude 127.
#[rustfmt::skip]
pub const SIN_TAB: [i8; 256] = [
    0, 3, 6, 9, 12, 16, 19, 22, 25, 28, 31, 34, 37, 40, 43, 46,
    49, 51, 54, 57, 60, 63, 65, 68, 71, 73, 76, 78, 81, 83, 85, 88,
    90, 92, 94, 96, 98, 100, 102, 104, 106, 107, 109, 111, 112, 113, 115, 116,
    117, 118, 120, 121, 122, 122, 123, 124, 125, 125, 126, 126, 126, 127, 127, 127,
    127, 127, 127, 127, 126, 126, 126, 125, 125, 124, 123, 122, 122, 121, 120, 118,
    117, 116, 115, 113, 112, 111, 109, 107, 106, 104, 102, 100, 98, 96, 94, 92,
    90, 88, 85, 83, 81, 78, 76, 73, 71, 68, 65, 63, 60, 57, 54, 51,
    49, 46, 43, 40, 37, 34, 31, 28, 25, 22, 19, 16, 12, 9, 6, 3,
    0, -3, -6, -9, -12, -16, -19, -22, -25, -28, -31, -34, -37, -40, -43, -46,
    -49, -51, -54, -57, -60, -63, -65, -68, -71, -73, -76, -78, -81, -83, -85, -88,
    -90, -92, -94, -96, -98, -100, -102, -104, -106, -107, -109, -111, -112, -113, -115, -116,
    -117, -118, -120, -121, -122, -122, -123, -124, -125, -125, -126, -126, -126, -127, -127, -127,
    -127, -127, -127, -127, -126, -126, -126, -125, -125, -124, -123, -122, -122, -121, -120, -118,
    -117, -116, -115, -113, -112, -111, -109, -107, -106, -104, -102, -100, -98, -96, -94, -92,
    -90, -88, -85, -83, -81, -78, -76, -73, -71, -68, -65, -63, -60, -57, -54, -51,
    -49, -46, -43, -40, -37, -34, -31, -28, -25, -22, -19, -16, -12, -9, -6, -3,
];

pub fn sin(i: i32) -> i32 {
    SIN_TAB[(i & 255) as usize] as i32
}

pub fn rgb565(r: i32, g: i32, b: i32) -> u16 {
    ((r >> 3) << 11 | (g >> 2) << 5 | b >> 3) as u16
}

/// A 32-bit integer hash (for the intros' pseudo-random patterns).
pub fn hash(mut a: u32) -> u32 {
    a ^= a >> 16;
    a = a.wrapping_mul(0x7feb352d);
    a ^= a >> 15;
    a = a.wrapping_mul(0x846ca68b);
    a ^= a >> 16;
    a
}

/// Adds b, scaled by lvl/8, onto d, per RGB565 channel, saturating.
#[inline]
pub fn add_light(d: &mut u16, mut b: u16, lvl: i32) {
    if lvl < 8 {
        // Scale: a sum of masked shifts.
        let mut s = 0u16;
        if lvl & 4 != 0 {
            s = s.wrapping_add((b >> 1) & 0x7BEF);
        }
        if lvl & 2 != 0 {
            s = s.wrapping_add((b >> 2) & 0x39E7);
        }
        if lvl & 1 != 0 {
            s = s.wrapping_add((b >> 3) & 0x18E3);
        }
        b = s;
        if b == 0 {
            return;
        }
    }
    // All three channels at once: spread G into the top half (0x07E0F81F)
    // so each field has a spare bit above it, add, then turn each overflow
    // bit into a full field.
    let a32 = (*d as u32 | (*d as u32) << 16) & 0x07E0F81F;
    let b32 = (b as u32 | (b as u32) << 16) & 0x07E0F81F;
    let mut sum = a32 + b32;
    let of = sum & 0x08010020;
    let rb = of & 0x00010020;
    let gg = of & 0x08000000;
    sum = (sum | (rb - (rb >> 5)) | (gg - (gg >> 6))) & 0x07E0F81F;
    *d = (sum | sum >> 16) as u16;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_saturates_per_channel() {
        let mut d = rgb565(200, 100, 0);
        add_light(&mut d, rgb565(100, 100, 100), 8);
        assert_eq!(d, rgb565(255, 200, 96));
        let mut d = 0;
        add_light(&mut d, 0xFFFF, 0);
        assert_eq!(d, 0);
        let mut d = 0;
        add_light(&mut d, rgb565(128, 128, 128), 4);
        assert_eq!(d, rgb565(64, 64, 64));
    }

    #[test]
    fn table() {
        assert_eq!(sin(64), 127);
        assert_eq!(sin(192), -127);
        assert_eq!(sin(256 + 1), sin(1));
        assert_eq!(sin(-1), sin(255));
    }
}
