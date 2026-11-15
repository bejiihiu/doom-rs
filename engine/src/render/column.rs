//! The low-level column drawers, ported from `r_draw.c`.
//!
//! A column is a vertical run of scaled texels drawn into the indexed
//! framebuffer. The original engine routes every column through `dc_*`
//! globals; here the state is explicit in [`ColumnParams`] / the function
//! arguments, because globals would make the 1:1 port unreadable and would
//! force `&mut` ceremony everywhere. The texture sampling itself goes
//! through [`ColumnSource`] so walls (128-texel columns with `& 127`
//! wrap), sprites and masked mid-textures can each hook in their own
//! sampler later without touching the drawers.
//!
//! Coordinate conventions match the original exactly:
//!
//! * `frac = texturemid + (y - center_y) * iscale` — computed from the
//!   *unclipped* `y_lo`, like the original.
//! * the source row is `frac >> FRACBITS` (arithmetic, so negative rows
//!   wrap into `0..127` under `& 127`).
//! * the drawers iterate from `y_lo` to `y_hi` inclusive, i.e.
//!   `y_hi - y_lo + 1` pixels, `do { ... } while (count--)` in C terms.
//!
//! Out-of-framebuffer rows/columns are skipped rather than crashing the
//! process — the original would march off the lump in `malloc`'d memory
//! and corrupt who knows what. Skipping is the only visible behavior
//! change for inputs the original never produced on purpose.

use crate::fixed::Fixed;
use crate::gfx::palette::Colormap;

use super::framebuffer::{IndexedFramebuffer, SCREEN_HEIGHT, SCREEN_WIDTH, step_frac_n};

/// Source of texels for one column.
///
/// `row` is the source row in texels, `frac >> FRACBITS` — it may be
/// negative and it is never pre-masked, exactly like the original hands
/// `frac >> FRACBITS` (possibly `& 127` inside the indexing) to the source
/// pointer. Implementations decide how to wrap or clamp.
pub trait ColumnSource {
    /// The texel at `row`.
    fn texel(&self, row: i32) -> u8;
}

/// A column that wraps with a bit mask, the wall case: `row & mask`.
///
/// The original hard-codes `& 127` against columns from 128-pixel tall
/// wall textures. [`WrappedColumn::wall`] encodes exactly that; the masked
/// constructor is for callers with their own power-of-two columns.
#[derive(Clone, Copy)]
pub struct WrappedColumn<'a> {
    bytes: &'a [u8],
    mask: i32,
}

impl<'a> WrappedColumn<'a> {
    /// A wall column of exactly 128 texels, wrapped with `& 127` — the
    /// original `dc_source[(frac >> FRACBITS) & 127]`.
    pub const fn wall(column: &'a [u8; 128]) -> Self {
        Self {
            bytes: column,
            mask: 127,
        }
    }

    /// A wrapped column with a caller-provided mask.
    ///
    /// # Panics (at construction)
    /// If `mask` is negative or `mask as usize >= bytes.len()` the wrap
    /// cannot stay inside `bytes`; that is a programming error, not data,
    /// so we assert instead of silently sampling garbage.
    pub const fn masked(bytes: &'a [u8], mask: i32) -> Self {
        assert!(mask >= 0, "mask must be non-negative");
        assert!(
            (mask as usize) < bytes.len(),
            "mask must stay inside the column"
        );
        Self { bytes, mask }
    }
}

impl ColumnSource for WrappedColumn<'_> {
    #[inline]
    fn texel(&self, row: i32) -> u8 {
        self.bytes[(row & self.mask) as usize]
    }
}

/// A column with no wrap — the sprite/translation case. The original reads
/// `frac >> FRACBITS` unchecked; for out-of-range rows it landed in
/// adjacent memory, which is garbage either way. We return 0 (palette
/// index 0) instead of reading out of bounds; in-range rows behave exactly
/// like the original.
#[derive(Clone, Copy)]
pub struct UnmaskedColumn<'a> {
    bytes: &'a [u8],
}

impl<'a> UnmaskedColumn<'a> {
    /// Wraps `bytes` as an unwrapped column.
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }
}

impl ColumnSource for UnmaskedColumn<'_> {
    #[inline]
    fn texel(&self, row: i32) -> u8 {
        usize::try_from(row)
            .ok()
            .and_then(|r| self.bytes.get(r))
            .copied()
            .unwrap_or(0)
    }
}

/// Parameters for the textured column drawers (`dc_*` in the original).
#[derive(Clone, Copy)]
pub struct ColumnParams<'a> {
    /// The colormap applied to each texel (`dc_colormap`).
    pub colormap: &'a Colormap,
    /// Screen column (`dc_x`).
    pub x: i32,
    /// Top row to draw, inclusive (`dc_yl`).
    pub y_lo: i32,
    /// Bottom row to draw, inclusive (`dc_yh`).
    pub y_hi: i32,
    /// Texture coordinate step per row (`dc_iscale`).
    pub iscale: Fixed,
    /// Texture coordinate of the vertical center (`dc_texturemid`).
    pub texturemid: Fixed,
    /// View center y (`centery` in the original; `viewheight / 2`).
    pub center_y: i32,
}

/// The texel row for a texture coordinate: `frac >> FRACBITS`, kept in a
/// local so the hot loop can step it directly.
#[inline]
fn step_texel_row(frac: Fixed) -> i32 {
    frac.to_raw() >> crate::fixed::FRACBITS
}

/// Draws a scaled texture column (`R_DrawColumn`).
///
/// Every pixel is re-indexed through `colormap`, exactly like the original
/// `dc_colormap[dc_source[(frac>>FRACBITS)&127]]`.
pub fn draw_column(
    fb: &mut IndexedFramebuffer,
    source: &impl ColumnSource,
    params: &ColumnParams<'_>,
) {
    let &ColumnParams {
        colormap,
        x,
        y_lo,
        y_hi,
        iscale,
        texturemid,
        center_y,
    } = params;

    if x < 0 || x >= SCREEN_WIDTH as i32 {
        return;
    }
    if y_hi < y_lo {
        return;
    }
    // clip to the visible rows; vanilla crashed (or wandered into who-knows
    // what malloc'd memory) on out-of-range rows, we draw the visible part
    let first = y_lo.max(0);
    let last = y_hi.min(SCREEN_HEIGHT as i32 - 1);
    if first > last {
        return;
    }

    let mut frac = texturemid + iscale * Fixed::from_int(y_lo - center_y);
    frac = step_frac_n(frac, iscale, i64::from(first - y_lo));
    let x_addr = x as usize;
    let pixels = fb.as_mut_slice();

    for y in first..=last {
        let addr = y as usize * SCREEN_WIDTH + x_addr;
        let texel = source.texel(step_texel_row(frac));
        pixels[addr] = colormap.lookup(texel);
        frac += iscale;
    }
}

/// Blocky/low detail column (`R_DrawColumnLow`).
///
/// The original does `dc_x <<= 1` and paints two adjacent columns with the
/// same texels — the "double pixel" mode for `detaillevel = 1`. Same here,
/// except the caller's `x` is not clobbered (the original mutated the
/// global `dc_x` as a side effect, which nobody relied on).
///
/// Known-dodgy mode upstream ("Hack. Does not work correctly."), kept for
/// detail-level parity.
pub fn draw_column_low(
    fb: &mut IndexedFramebuffer,
    source: &impl ColumnSource,
    params: &ColumnParams<'_>,
) {
    let &ColumnParams {
        colormap,
        x,
        y_lo,
        y_hi,
        iscale,
        texturemid,
        center_y,
    } = params;

    let x0 = x * 2;
    if x0 < 0 || x0 >= SCREEN_WIDTH as i32 {
        return;
    }
    if y_hi < y_lo {
        return;
    }
    let first = y_lo.max(0);
    let last = y_hi.min(SCREEN_HEIGHT as i32 - 1);
    if first > last {
        return;
    }

    let mut frac = texturemid + iscale * Fixed::from_int(y_lo - center_y);
    frac = step_frac_n(frac, iscale, i64::from(first - y_lo));
    let x_addr = x0 as usize;
    let second_fits = x_addr + 1 < SCREEN_WIDTH;
    let pixels = fb.as_mut_slice();

    for y in first..=last {
        let addr = y as usize * SCREEN_WIDTH + x_addr;
        let texel = source.texel(step_texel_row(frac));
        let lit = colormap.lookup(texel);
        pixels[addr] = lit;
        if second_fits {
            pixels[addr + 1] = lit;
        }
        frac += iscale;
    }
}

/// Length of the fuzz offset table (`FUZZTABLE`).
pub const FUZZ_TABLE_LEN: usize = 50;
/// The fuzz offset step: plus/minus one screen row.
pub const FUZZ_OFFSET: i32 = SCREEN_WIDTH as i32;

/// Neighbor offsets used by the fuzzy column drawer (`fuzzoffset`),
/// ported byte-for-byte from the original.
///
/// The values are ±`SCREENWIDTH` (one framebuffer row), i.e. the
/// spectre/invisible-player shimmer samples pixels one row up/down in the
/// same column. Sign order matters; do not sort or dedupe.
pub const FUZZ_OFFSETS: [i32; FUZZ_TABLE_LEN] = [
    FUZZ_OFFSET,
    -FUZZ_OFFSET,
    FUZZ_OFFSET,
    -FUZZ_OFFSET,
    FUZZ_OFFSET,
    FUZZ_OFFSET,
    -FUZZ_OFFSET,
    FUZZ_OFFSET,
    FUZZ_OFFSET,
    -FUZZ_OFFSET,
    FUZZ_OFFSET,
    FUZZ_OFFSET,
    FUZZ_OFFSET,
    -FUZZ_OFFSET,
    FUZZ_OFFSET,
    FUZZ_OFFSET,
    FUZZ_OFFSET,
    -FUZZ_OFFSET,
    -FUZZ_OFFSET,
    -FUZZ_OFFSET,
    -FUZZ_OFFSET,
    FUZZ_OFFSET,
    -FUZZ_OFFSET,
    -FUZZ_OFFSET,
    FUZZ_OFFSET,
    FUZZ_OFFSET,
    FUZZ_OFFSET,
    FUZZ_OFFSET,
    -FUZZ_OFFSET,
    FUZZ_OFFSET,
    -FUZZ_OFFSET,
    FUZZ_OFFSET,
    FUZZ_OFFSET,
    -FUZZ_OFFSET,
    -FUZZ_OFFSET,
    FUZZ_OFFSET,
    FUZZ_OFFSET,
    -FUZZ_OFFSET,
    -FUZZ_OFFSET,
    -FUZZ_OFFSET,
    -FUZZ_OFFSET,
    FUZZ_OFFSET,
    FUZZ_OFFSET,
    FUZZ_OFFSET,
    FUZZ_OFFSET,
    -FUZZ_OFFSET,
    FUZZ_OFFSET,
    FUZZ_OFFSET,
    -FUZZ_OFFSET,
    FUZZ_OFFSET,
];

/// Mutable state for the fuzzy column drawer (`fuzzpos`).
///
/// The original keeps `fuzzpos` as a file-scope global that every fuzz
/// column advances, so consecutive spectre columns produce a rolling
/// dither. This struct holds that state explicitly; create one per view
/// (vanilla resets it nowhere — it just keeps rolling) or per column if
/// you need determinism.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FuzzState {
    pos: usize,
}

impl FuzzState {
    /// A fresh state at position 0.
    pub const fn new() -> Self {
        Self { pos: 0 }
    }

    /// The position into [`FUZZ_OFFSETS`] the next fuzz pixel will use.
    pub const fn position(&self) -> usize {
        self.pos
    }

    fn next_offset(&mut self) -> i32 {
        let offset = FUZZ_OFFSETS[self.pos];
        self.pos += 1;
        if self.pos == FUZZ_TABLE_LEN {
            self.pos = 0;
        }
        offset
    }
}

/// Draws a "fuzz" column (`R_DrawFuzzColumn`).
///
/// The spectre/invisibility effect: colormap 6 is applied to the pixel one
/// framebuffer row above or below the current one, so fuzzy monsters look
/// smudged. `fuzz` advances on every drawn pixel, exactly like the
/// original's global `fuzzpos`.
///
/// The border clamp matches the original: `dc_yl == 0` becomes 1, and
/// `dc_yh == view_height - 1` becomes `view_height - 2`, because the fuzz
/// must read a neighbor that exists. Note the original mutated the global
/// `dc_yl`/`dc_yh`; ours clamps local copies only, which nobody in the
/// call chain can observe.
///
/// Pixels whose fuzz neighbor lands outside the framebuffer get black
/// (palette index 0) as their neighbor. The original would read whatever
/// `malloc` left next to the view buffer; index 0 is the same
/// "unspecified" answer without touching out-of-bounds memory.
pub fn draw_fuzz_column(
    fb: &mut IndexedFramebuffer,
    fuzz_colormap: &Colormap,
    fuzz: &mut FuzzState,
    x: i32,
    y_lo: i32,
    y_hi: i32,
    view_height: i32,
) {
    let mut yl = y_lo;
    let mut yh = y_hi;

    // Adjust borders. Low...
    if yl == 0 {
        yl = 1;
    }
    // .. and high.
    if yh == view_height - 1 {
        yh = view_height - 2;
    }

    if x < 0 || x >= SCREEN_WIDTH as i32 {
        return;
    }
    yl = yl.max(0);
    yh = yh.min(SCREEN_HEIGHT as i32 - 1);
    if yl > yh {
        return;
    }

    let x_addr = x as usize;
    let pixels = fb.as_mut_slice();
    for y in yl..=yh {
        let addr = y as usize * SCREEN_WIDTH + x_addr;
        let offset = fuzz.next_offset();
        // offsets are multiples of SCREEN_WIDTH, so addr + offset lands in
        // the same column one row up/down and never wraps a row
        let neighbor = addr as i64 + i64::from(offset);
        let neighbor_pixel = usize::try_from(neighbor)
            .ok()
            .and_then(|n| pixels.get(n))
            .copied()
            .unwrap_or(0);
        pixels[addr] = fuzz_colormap.lookup(neighbor_pixel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::palette::FUZZ_COLORMAP;

    fn identity_colormap() -> Colormap {
        Colormap(core::array::from_fn(|i| i as u8))
    }

    fn counting_column() -> [u8; 128] {
        // texel i holds index i
        core::array::from_fn(|i| i as u8)
    }

    fn params(colormap: &Colormap, x: i32, y_lo: i32, y_hi: i32) -> ColumnParams<'_> {
        ColumnParams {
            colormap,
            x,
            y_lo,
            y_hi,
            iscale: Fixed::ONE,
            texturemid: Fixed::ZERO,
            center_y: 0,
        }
    }

    #[test]
    fn column_writes_expected_texels() {
        let map = identity_colormap();
        let column = counting_column();
        let src = WrappedColumn::wall(&column);
        let mut fb = IndexedFramebuffer::new();
        let p = params(&map, 3, 0, 199);
        draw_column(&mut fb, &src, &p);

        for y in 0..200usize {
            // iscale = 1.0, texturemid = 0, center_y = 0 -> row == y, wrapped to 128
            assert_eq!(fb.get(3, y), Some((y & 127) as u8), "row {y}");
            // nothing is ever drawn to x = 7 in this test
            assert_eq!(fb.get(7, y), Some(0), "neighbors untouched");
        }
    }

    #[test]
    fn column_wraps_texture_with_mask() {
        let map = identity_colormap();
        let column = counting_column();
        let src = WrappedColumn::wall(&column);
        let mut fb = IndexedFramebuffer::new();
        // double rate: texel row = 2*y, wrapped at 127 like (frac >> 16) & 127
        let p = ColumnParams {
            colormap: &map,
            x: 0,
            y_lo: 0,
            y_hi: 100,
            iscale: Fixed::from_int(2),
            texturemid: Fixed::ZERO,
            center_y: 0,
        };
        draw_column(&mut fb, &src, &p);
        for y in 0..=100usize {
            assert_eq!(fb.get(0, y), Some(((2 * y) & 127) as u8), "row {y}");
        }
    }

    #[test]
    fn column_negative_frac_wraps_like_original() {
        let map = identity_colormap();
        let column = counting_column();
        let src = WrappedColumn::wall(&column);
        let mut fb = IndexedFramebuffer::new();
        // texturemid = -2.0: row = y - 2, and y = 0 must wrap to 126 (& 127),
        // not panic and not saturate
        let p = ColumnParams {
            colormap: &map,
            x: 5,
            y_lo: 0,
            y_hi: 4,
            iscale: Fixed::ONE,
            texturemid: Fixed::from_int(-2),
            center_y: 0,
        };
        draw_column(&mut fb, &src, &p);
        assert_eq!(fb.get(5, 0), Some(126));
        assert_eq!(fb.get(5, 1), Some(127));
        assert_eq!(fb.get(5, 2), Some(0));
        assert_eq!(fb.get(5, 3), Some(1));
        assert_eq!(fb.get(5, 4), Some(2));
    }

    #[test]
    fn column_uses_fractional_texturemid() {
        let map = identity_colormap();
        let column = counting_column();
        let src = WrappedColumn::wall(&column);
        let mut fb = IndexedFramebuffer::new();
        // texturemid = 0.5, iscale = 1.0 -> rows floor(0.5 + y) == y
        let p = ColumnParams {
            colormap: &map,
            x: 8,
            y_lo: 0,
            y_hi: 10,
            iscale: Fixed::ONE,
            texturemid: Fixed::from_raw(0x8000), // 0.5
            center_y: 0,
        };
        draw_column(&mut fb, &src, &p);
        for y in 0..=10usize {
            assert_eq!(fb.get(8, y), Some((y & 127) as u8));
        }
    }

    #[test]
    fn column_colormap_is_applied() {
        // a colormap that swaps two indices; everything else passes through
        let mut raw = [0u8; 256];
        for (i, b) in raw.iter_mut().enumerate() {
            *b = i as u8;
        }
        raw[10] = 77;
        let map = Colormap(raw);
        let column = counting_column();
        let src = WrappedColumn::wall(&column);
        let mut fb = IndexedFramebuffer::new();
        let p = params(&map, 1, 10, 10);
        draw_column(&mut fb, &src, &p);
        assert_eq!(fb.get(1, 10), Some(77));
    }

    #[test]
    fn column_low_doubles_pixels() {
        let map = identity_colormap();
        let column = counting_column();
        let src = WrappedColumn::wall(&column);
        let mut fb = IndexedFramebuffer::new();
        let p = params(&map, 3, 0, 5);
        draw_column_low(&mut fb, &src, &p);

        // x = 3 -> columns 6 and 7 get the same texels
        for y in 0..=5usize {
            let expected = Some((y & 127) as u8);
            assert_eq!(fb.get(6, y), expected, "first double row {y}");
            assert_eq!(fb.get(7, y), expected, "second double row {y}");
        }
        assert_eq!(fb.get(5, 0), Some(0), "column before the pair untouched");
        assert_eq!(fb.get(8, 0), Some(0), "column after the pair untouched");
    }

    #[test]
    fn empty_or_inverted_range_draws_nothing() {
        let map = identity_colormap();
        let column = counting_column();
        let src = WrappedColumn::wall(&column);
        let mut fb = IndexedFramebuffer::new();
        let p = params(&map, 3, 100, 99);
        draw_column(&mut fb, &src, &p);
        assert!(fb.as_slice().iter().all(|&b| b == 0));

        let p = params(&map, 3, 5, 5);
        draw_column(&mut fb, &src, &p);
        assert_eq!(fb.get(3, 5), Some(5));
    }

    #[test]
    fn out_of_bounds_column_is_skipped_not_fatal() {
        let map = identity_colormap();
        let column = counting_column();
        let src = WrappedColumn::wall(&column);
        let mut fb = IndexedFramebuffer::new();
        let p = params(&map, 320, 0, 10);
        draw_column(&mut fb, &src, &p);
        let p = params(&map, -1, 0, 10);
        draw_column(&mut fb, &src, &p);
        assert!(fb.as_slice().iter().all(|&b| b == 0));
    }

    #[test]
    fn column_clips_rows_above_framebuffer() {
        let map = identity_colormap();
        let column = counting_column();
        let src = WrappedColumn::wall(&column);
        let mut fb = IndexedFramebuffer::new();
        // negative y_lo: rows -4..-1 skipped, frac jumps; row 0 gets texel 0
        let p = ColumnParams {
            colormap: &map,
            x: 2,
            y_lo: -4,
            y_hi: 2,
            iscale: Fixed::ONE,
            texturemid: Fixed::ZERO,
            center_y: 0,
        };
        draw_column(&mut fb, &src, &p);
        assert_eq!(fb.get(2, 0), Some(0));
        assert_eq!(fb.get(2, 1), Some(1));
        assert_eq!(fb.get(2, 2), Some(2));
    }

    #[test]
    fn column_clips_rows_below_framebuffer() {
        let map = identity_colormap();
        let column = counting_column();
        let src = WrappedColumn::wall(&column);
        let mut fb = IndexedFramebuffer::new();
        // y_hi far beyond the screen must return quickly, not hang
        let p = ColumnParams {
            colormap: &map,
            x: 2,
            y_lo: 0,
            y_hi: i32::MAX,
            iscale: Fixed::ONE,
            texturemid: Fixed::ZERO,
            center_y: 0,
        };
        draw_column(&mut fb, &src, &p);
        assert_eq!(fb.get(2, 199), Some((199 & 127) as u8));
    }

    #[test]
    fn unmasked_column_returns_zero_out_of_range() {
        let bytes = [10u8, 20, 30];
        let src = UnmaskedColumn::new(&bytes);
        assert_eq!(src.texel(0), 10);
        assert_eq!(src.texel(2), 30);
        assert_eq!(src.texel(3), 0);
        assert_eq!(src.texel(-1), 0);
    }

    #[test]
    fn wrapped_column_masked_constructor_validates() {
        let bytes = [0u8; 4];
        let src = WrappedColumn::masked(&bytes, 3);
        assert_eq!(src.texel(5), bytes[1]);
        let result = std::panic::catch_unwind(|| WrappedColumn::masked(&bytes, 4));
        assert!(
            result.is_err(),
            "mask outside the column must assert at construction"
        );
    }

    #[test]
    fn fuzz_offsets_match_original_pattern() {
        // byte-for-byte port of the FUZZTABLE sign pattern
        let pattern: String = FUZZ_OFFSETS
            .iter()
            .map(|&o| if o == FUZZ_OFFSET { '+' } else { '-' })
            .collect();
        assert_eq!(
            pattern,
            "+-+-++-++-+++-+++----+--++++-+-++--++----++++-++-+"
        );
        assert_eq!(FUZZ_OFFSETS.len(), FUZZ_TABLE_LEN);
        assert!(
            FUZZ_OFFSETS
                .iter()
                .all(|&o| o == FUZZ_OFFSET || o == -FUZZ_OFFSET)
        );
    }

    #[test]
    fn fuzz_column_deterministic_at_fixed_pos() {
        let fuzz_map = Colormap(core::array::from_fn(|i| 255 - i as u8));
        let mut fb = IndexedFramebuffer::new();
        for (i, v) in fb.as_mut_slice().iter_mut().enumerate() {
            *v = (i * 31 + 7) as u8;
        }

        let mut fuzz = FuzzState::new();
        let seed: Vec<u8> = fb.as_slice().to_vec();
        draw_fuzz_column(&mut fb, &fuzz_map, &mut fuzz, 10, 5, 8, 200);

        // hand-computed chain; the buffer is read after every pixel, so
        // in-chain overwrites matter:
        //   s(i)  = seed byte at flat index i
        //   row 5: FUZZ_OFFSETS[0] = +320 -> reads row 6 (not yet drawn)
        //   row 6: FUZZ_OFFSETS[1] = -320 -> reads row 5 (already drawn)
        //   row 7: FUZZ_OFFSETS[2] = +320 -> reads row 8 (not yet drawn)
        //   row 8: FUZZ_OFFSETS[3] = -320 -> reads row 7 (already drawn)
        let s = |y: usize| seed[y * SCREEN_WIDTH + 10];
        let inv = |v: u8| fuzz_map.lookup(v); // 255 - v
        let expect = [inv(s(6)), inv(inv(s(6))), inv(s(8)), inv(inv(s(8)))];
        for (i, y) in (5..=8).enumerate() {
            assert_eq!(fb.get(10, y), Some(expect[i]), "row {y}");
        }
        assert_eq!(fuzz.position(), 4);

        // determinism: same start state, same writes
        let mut fb2 = IndexedFramebuffer::new();
        fb2.as_mut_slice().copy_from_slice(&seed);
        let mut fuzz2 = FuzzState::new();
        draw_fuzz_column(&mut fb2, &fuzz_map, &mut fuzz2, 10, 5, 8, 200);
        assert_eq!(fb, fb2);
    }

    #[test]
    fn fuzz_fuzz_colormap_constant_is_6() {
        assert_eq!(FUZZ_COLORMAP, 6);
    }

    #[test]
    fn fuzz_border_clamps() {
        let fuzz_map = identity_colormap();
        let mut fb = IndexedFramebuffer::new();

        // y_lo == 0 clamps to 1: row 0 must not be drawn
        fb.clear(7);
        let mut fuzz = FuzzState::new();
        draw_fuzz_column(&mut fb, &fuzz_map, &mut fuzz, 4, 0, 3, 200);
        assert_eq!(fb.get(4, 0), Some(7), "row 0 kept its clear value");
        assert_eq!(
            fb.get(4, 1),
            Some(7),
            "row 1 drawn via the identity neighbor"
        );

        // yh == view_height - 1 clamps to view_height - 2
        let mut fb = IndexedFramebuffer::new();
        for (i, v) in fb.as_mut_slice().iter_mut().enumerate() {
            *v = (i & 0xFF) as u8 | 1;
        }
        let mut fuzz = FuzzState::new();
        // yh == view_height - 1 -> clamps to view_height - 2 = 99
        draw_fuzz_column(&mut fb, &fuzz_map, &mut fuzz, 4, 98, 100, 101);
        // yh = view_height - 2 = 99, so row 100 must keep its seed
        assert_eq!(
            fb.get(4, 100),
            Some((((100 * SCREEN_WIDTH + 4) & 0xFF) | 1) as u8)
        );
        // row 99 was drawn with the (already overwritten) row 98 neighbor:
        // row 98 got row 99's seed, so row 99 reads that same value back
        let expected = (((99 * SCREEN_WIDTH + 4) & 0xFF) | 1) as u8;
        assert_eq!(fb.get(4, 99), Some(expected));
    }

    #[test]
    fn fuzz_x_out_of_bounds_draws_nothing() {
        let fuzz_map = identity_colormap();
        let mut fb = IndexedFramebuffer::new();
        let mut fuzz = FuzzState::new();
        draw_fuzz_column(&mut fb, &fuzz_map, &mut fuzz, 320, 0, 10, 200);
        assert!(fb.as_slice().iter().all(|&b| b == 0));
        assert_eq!(fuzz.position(), 0, "no pixels, no fuzzpos advance");
    }
}
