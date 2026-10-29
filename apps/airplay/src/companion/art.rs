//! Cover art for the screen: shairport-sync's PICT (a JPEG or PNG, as the
//! sender gave it) decoded and scaled here, on the server, to fit the
//! screen's cover box, as RGB565. The Chumby's ARM926 has no FPU, and
//! decoding a JPEG there would take seconds.

use neochumby_airplay::frames::Cover;

/// `pict` scaled to fit `side` by `side`, keeping its shape; None if it
/// isn't a picture this can decode.
#[cfg(not(target_arch = "arm"))]
pub fn thumbnail(pict: &[u8], side: u16) -> Option<Cover> {
    use image::imageops::FilterType;

    let img = image::load_from_memory(pict).ok()?.to_rgb8();
    let (w, h) = (img.width().max(1), img.height().max(1));
    let side = side as u32;
    // The longer side to `side`, the other in proportion (at least 1).
    let (tw, th) = if w >= h { (side, (h * side / w).max(1)) } else { ((w * side / h).max(1), side) };
    let small = image::imageops::resize(&img, tw, th, FilterType::Triangle);
    let pixels = small.pixels().map(|p| rgb565(p.0)).collect();
    Some(Cover { width: tw as u16, height: th as u16, pixels })
}

/// The companion only runs on neochumbyd's host; built for the Chumby (as
/// tools/tree builds every binary of the app), it goes without decoders.
#[cfg(target_arch = "arm")]
pub fn thumbnail(_pict: &[u8], _side: u16) -> Option<Cover> {
    None
}

/// 8-bit RGB to RGB565, rounding to the nearest step.
#[cfg_attr(target_arch = "arm", allow(dead_code))]
fn rgb565([r, g, b]: [u8; 3]) -> u16 {
    let q = |v: u8, bits: u32| -> u16 {
        let max = (1u32 << bits) - 1;
        ((v as u32 * max + 127) / 255) as u16
    };
    (q(r, 5) << 11) | (q(g, 6) << 5) | q(b, 5)
}

#[cfg(all(test, not(target_arch = "arm")))]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgb, RgbImage};

    /// A made-up picture: red on the left half, blue on the right.
    fn halves(w: u32, h: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, _| if x < w / 2 { Rgb([255, 0, 0]) } else { Rgb([0, 0, 255]) })
    }

    fn encoded(img: &RgbImage, format: ImageFormat) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    #[test]
    fn png_and_jpeg_come_out_the_box_size() {
        for format in [ImageFormat::Png, ImageFormat::Jpeg] {
            let c = thumbnail(&encoded(&halves(600, 600), format), 128).unwrap();
            assert_eq!((c.width, c.height), (128, 128));
            assert_eq!(c.pixels.len(), 128 * 128);
            // Red on the left, blue on the right (JPEG only near enough).
            let px = |x: usize, y: usize| c.pixels[y * 128 + x];
            let (r, b) = (px(10, 64) >> 11, px(118, 64) & 0x1f);
            assert!(r >= 29 && px(10, 64) & 0x1f <= 2, "{format:?}: {:#06x}", px(10, 64));
            assert!(b >= 29 && px(118, 64) >> 11 <= 2, "{format:?}: {:#06x}", px(118, 64));
        }
    }

    #[test]
    fn the_shape_is_kept() {
        let c = thumbnail(&encoded(&halves(400, 100), ImageFormat::Png), 128).unwrap();
        assert_eq!((c.width, c.height), (128, 32));
        let c = thumbnail(&encoded(&halves(10, 2000), ImageFormat::Png), 128).unwrap();
        assert_eq!((c.width, c.height), (1, 128));
    }

    #[test]
    fn not_a_picture() {
        assert_eq!(thumbnail(b"not a picture", 128), None);
        assert_eq!(thumbnail(&[], 128), None);
    }

    #[test]
    fn colours() {
        assert_eq!(rgb565([255, 255, 255]), 0xffff);
        assert_eq!(rgb565([0, 0, 0]), 0);
        assert_eq!(rgb565([255, 0, 0]), 0xf800);
        assert_eq!(rgb565([0, 255, 0]), 0x07e0);
        assert_eq!(rgb565([128, 128, 128]), (16 << 11) | (32 << 5) | 16);
    }
}
