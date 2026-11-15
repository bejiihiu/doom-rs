//! The horizontal span drawers (floor/ceiling texture slices), ported
//! from `r_draw.c`.
//!
//! Floors and ceilings are constant-z horizontal spans. The original maps
//! them with two fixed-point texture coordinates stepped per pixel:
//! `spanx`/`spany` style mapping but expressed as `ds_xfrac`/`ds_yfrac`
//! accumulated by `ds_xstep`/`ds_ystep`. The inner loop is deliberately
//! kept simple — one texel fetch + one colormap lookup per pixel, like the
//! original.
//!
//! Spans are inclusive in both x bounds (`count = ds_x2 - ds_x1` iterates
//! `x2 - x1 + 1` times). Out-of-framebuffer pixels are skipped rather than
//! corrupting memory; the `frac` accumulators keep stepping regardless,
//! matching the original's do-while counting.

use crate::fixed::Fixed;
use crate::gfx::flat::Flat;
use crate::gfx::palette::Colormap;

use super::framebuffer::{IndexedFramebuffer, pixel_addr};

/// Parameters for the span drawers (`ds_*` in the original).
#[derive(Clone, Copy)]
pub struct SpanParams<'a> {
    /// The colormap applied to each texel (`ds_colormap`).
    pub colormap: &'a Colormap,
    /// Screen row of the span (`ds_y`).
    pub y: i32,
    /// Left column, inclusive (`ds_x1`).
    pub x_lo: i32,
    /// Right column, inclusive (`ds_x2`).
    pub x_hi: i32,
    /// Texture U coordinate at `x_lo` (`ds_xfrac`).
    pub x_frac: Fixed,
    /// Texture V coordinate at `x_lo` (`ds_yfrac`).
    pub y_frac: Fixed,
    /// U step per pixel (`ds_xstep`).
    pub x_step: Fixed,
    /// V step per pixel (`ds_ystep`).
    pub y_step: Fixed,
}

/// The flat texel index for (`x_frac`, `y_frac`), the original
/// `((yfrac>>(16-6))&(63*64)) + ((xfrac>>16)&63)` — both halves masked into
/// the 64x64 tile.
#[inline]
fn flat_spot(x_frac: Fixed, y_frac: Fixed) -> usize {
    let v = (y_frac.to_raw() >> 10) & (63 * 64);
    let u = (x_frac.to_raw() >> 16) & 63;
    (v + u) as usize
}

/// Draws a horizontal floor/ceiling span (`R_DrawSpan`).
pub fn draw_span(fb: &mut IndexedFramebuffer, flat: &Flat, params: &SpanParams<'_>) {
    let &SpanParams {
        colormap,
        y,
        x_lo,
        x_hi,
        x_frac,
        y_frac,
        x_step,
        y_step,
    } = params;

    if x_hi < x_lo {
        // vanilla RANGECHECKs this; an inverted span is a caller bug —
        // drawing nothing is the safe reading, and it cannot match the
        // original's one-pixel write anyway.
        return;
    }

    let mut x_frac = x_frac;
    let mut y_frac = y_frac;
    let pixels = fb.as_mut_slice();
    let flat_pixels = flat.pixels();

    for x in x_lo..=x_hi {
        if let Some(addr) = pixel_addr(x, y) {
            let spot = flat_spot(x_frac, y_frac);
            pixels[addr] = colormap.lookup(flat_pixels[spot]);
        }
        x_frac += x_step;
        y_frac += y_step;
    }
}

/// Blocky/low detail span (`R_DrawSpanLow`).
///
/// The original does `ds_x1 <<= 1; ds_x2 <<= 1` and paints every pixel
/// twice. Identical here, except the caller's bounds are not clobbered.
pub fn draw_span_low(fb: &mut IndexedFramebuffer, flat: &Flat, params: &SpanParams<'_>) {
    let &SpanParams {
        colormap,
        y,
        x_lo,
        x_hi,
        x_frac,
        y_frac,
        x_step,
        y_step,
    } = params;

    let x_lo = x_lo * 2;
    let x_hi = x_hi * 2;
    if x_hi < x_lo {
        return;
    }

    let mut x_frac = x_frac;
    let mut y_frac = y_frac;
    let pixels = fb.as_mut_slice();
    let flat_pixels = flat.pixels();

    // vanilla: count = x2 - x1 (already doubled); do..while(count--)
    // paints count+1 double-pixels starting at the doubled x1
    let count = x_hi - x_lo;
    for i in 0..=count {
        let x = x_lo + i * 2;
        if let Some(addr) = pixel_addr(x, y) {
            let spot = flat_spot(x_frac, y_frac);
            let texel = colormap.lookup(flat_pixels[spot]);
            pixels[addr] = texel;
            if let Some(addr2) = pixel_addr(x + 1, y) {
                pixels[addr2] = texel;
            }
        }
        x_frac += x_step;
        y_frac += y_step;
    }
}

#[cfg(test)]
mod tests {
    use super::super::SCREEN_WIDTH;
    use super::*;

    fn identity_colormap() -> Colormap {
        Colormap(core::array::from_fn(|i| i as u8))
    }

    fn counting_flat() -> Flat {
        // texel at (x, y) holds (y * 64 + x) as u8
        Flat(core::array::from_fn(|i| i as u8))
    }

    fn params(colormap: &Colormap, y: i32, x_lo: i32, x_hi: i32) -> SpanParams<'_> {
        SpanParams {
            colormap,
            y,
            x_lo,
            x_hi,
            x_frac: Fixed::ZERO,
            y_frac: Fixed::ZERO,
            x_step: Fixed::ONE,
            y_step: Fixed::ZERO,
        }
    }

    #[test]
    fn flat_spot_matches_original_formula() {
        // spot = ((yfrac >> 10) & 4032) + ((xfrac >> 16) & 63)
        assert_eq!(flat_spot(Fixed::ZERO, Fixed::ZERO), 0);
        // x_frac = 1.0, y_frac = 0.0 -> u = 1, v = 0
        assert_eq!(flat_spot(Fixed::from_int(1), Fixed::ZERO), 1);
        // x_frac = 0.0, y_frac = 1.0 -> v = 1 << 6 = 64
        assert_eq!(flat_spot(Fixed::ZERO, Fixed::from_int(1)), 64);
        // 65.0 in y wraps the row mask: (65 << 6) & 4032 = 4160 & 4032 = 64
        assert_eq!(
            flat_spot(Fixed::ZERO, Fixed::from_int(65)),
            (65 << 6) & 4032
        );
        // u mask bit: 64.0 in x wraps to 0
        assert_eq!(flat_spot(Fixed::from_int(64), Fixed::ZERO), 0);
    }

    #[test]
    fn span_writes_expected_texels() {
        let map = identity_colormap();
        let flat = counting_flat();
        let mut fb = IndexedFramebuffer::new();
        // flat[i] == i, identity colormap, steps of 1 texel in u only
        let p = params(&map, 100, 0, 9);
        draw_span(&mut fb, &flat, &p);
        for x in 0..=9usize {
            assert_eq!(fb.get(x, 100), Some(x as u8));
        }
        // beyond x_hi and off-row pixels stay zero
        assert_eq!(fb.get(10, 100), Some(0));
        assert_eq!(fb.get(0, 99), Some(0));
        assert_eq!(fb.get(0, 101), Some(0));
    }

    #[test]
    fn span_walks_diagonally_with_slope() {
        let map = identity_colormap();
        let flat = counting_flat();
        let mut fb = IndexedFramebuffer::new();
        // y step = 0.5 texel per pixel: v accumulates every other pixel
        let p = SpanParams {
            colormap: &map,
            y: 40,
            x_lo: 0,
            x_hi: 3,
            x_frac: Fixed::ZERO,
            y_frac: Fixed::ZERO,
            x_step: Fixed::ONE,
            y_step: Fixed::from_raw(0x8000), // 0.5
        };
        draw_span(&mut fb, &flat, &p);
        // x = 0: u=0, v=0 -> flat[0]
        assert_eq!(fb.get(0, 40), Some(0));
        // x = 1: u=1, v still 0 (half a texel) -> flat[1]
        assert_eq!(fb.get(1, 40), Some(1));
        // x = 2: u=2, v = 1 whole row (64 bytes) -> flat[64 + 2] = 66
        assert_eq!(fb.get(2, 40), Some(66));
        // x = 3: u=3, y_frac = 1.5 -> v stays 64 (fraction's byte bits are
        // masked out by the 4032 row mask) -> flat[64 + 3] = 67
        assert_eq!(fb.get(3, 40), Some(67));
    }

    #[test]
    fn span_wraps_flat_coordinates_at_64() {
        let map = identity_colormap();
        let flat = counting_flat();
        let mut fb = IndexedFramebuffer::new();
        // start at x_frac = 64: u wraps to 0
        let p = SpanParams {
            colormap: &map,
            y: 20,
            x_lo: 0,
            x_hi: 1,
            x_frac: Fixed::from_int(64),
            y_frac: Fixed::ZERO,
            x_step: Fixed::from_int(65),
            y_step: Fixed::ZERO,
        };
        draw_span(&mut fb, &flat, &p);
        // first pixel: x_frac = 64 -> u wraps to 0 -> flat[0]
        assert_eq!(fb.get(0, 20), Some(0));
        // second pixel: x_frac = 129 -> u = 129 & 63 = 1 -> flat[1]
        assert_eq!(fb.get(1, 20), Some(1));
    }

    #[test]
    fn span_out_of_framebuffer_is_clipped() {
        let map = identity_colormap();
        let flat = counting_flat();
        let mut fb = IndexedFramebuffer::new();
        // negative x clipped, frac still advances; draws x = 0..=2
        let p = params(&map, 7, -2, 2);
        draw_span(&mut fb, &flat, &p);
        assert_eq!(fb.get(0, 7), Some(2)); // frac advanced past the two clipped pixels
        assert_eq!(fb.get(1, 7), Some(3));
        assert_eq!(fb.get(2, 7), Some(4));
        assert_eq!(fb.get(0, 8), Some(0), "other rows untouched");
    }

    #[test]
    fn inverted_span_draws_nothing() {
        let map = identity_colormap();
        let flat = counting_flat();
        let mut fb = IndexedFramebuffer::new();
        let p = params(&map, 10, 5, 4);
        draw_span(&mut fb, &flat, &p);
        assert!(fb.as_slice().iter().all(|&b| b == 0));
    }

    #[test]
    fn single_pixel_span() {
        let map = identity_colormap();
        let flat = counting_flat();
        let mut fb = IndexedFramebuffer::new();
        let p = params(&map, 42, 17, 17);
        draw_span(&mut fb, &flat, &p);
        assert_eq!(fb.get(17, 42), Some(0));
        assert_eq!(fb.get(16, 42), Some(0));
        assert_eq!(fb.get(18, 42), Some(0));
    }

    #[test]
    fn span_low_doubles_pixels() {
        let map = identity_colormap();
        let flat = counting_flat();
        let mut fb = IndexedFramebuffer::new();
        let p = params(&map, 60, 0, 1);
        draw_span_low(&mut fb, &flat, &p);
        // x = (0,1) doubled to (0,2), count = 2 -> three double pixels:
        // starts at 0, 2, 4 -> pixels 0..5
        for (x, v) in [(0usize, 0u8), (1, 0), (2, 1), (3, 1), (4, 2), (5, 2)] {
            assert_eq!(fb.get(x, 60), Some(v), "x {x}");
        }
        assert_eq!(fb.get(6, 60), Some(0), "past the doubled span untouched");
    }

    #[test]
    fn span_y_out_of_framebuffer_is_clipped() {
        let map = identity_colormap();
        let flat = counting_flat();
        let mut fb = IndexedFramebuffer::new();
        let p = params(&map, 500, 0, 10);
        draw_span(&mut fb, &flat, &p);
        assert!(fb.as_slice().iter().all(|&b| b == 0));
    }

    #[test]
    fn span_colormap_is_applied() {
        let mut raw = [0u8; 256];
        for (i, b) in raw.iter_mut().enumerate() {
            *b = i as u8;
        }
        raw[200] = 42;
        let map = Colormap(raw);
        let mut bytes = [0u8; 4096];
        bytes[0] = 200;
        let flat = Flat(bytes);
        let mut fb = IndexedFramebuffer::new();
        let p = params(&map, 3, 0, 0);
        draw_span(&mut fb, &flat, &p);
        assert_eq!(fb.get(0, 3), Some(42));
    }

    #[test]
    fn screen_width_constant_is_320() {
        assert_eq!(SCREEN_WIDTH, 320);
    }
}
