//! The status bar widget layer, ported from `st_lib.c` (linuxdoom-1.10).
//!
//! These are the building blocks the bar is made of: the number widgets
//! (`STlib_initNum`/`STlib_updateNum`), the percent widgets
//! (`STlib_initPercent`), the multi-icon widgets (`STlib_initMultIcon`,
//! used for the face, the weapon-owned icons and the key cards) and the
//! binary-icon widget (`STlib_initBinIcon`, used for the arms panel
//! background).
//!
//! Drawing mechanics, ported 1:1:
//!
//! * patches are drawn with origin semantics (`x - leftoffset`,
//!   `y - topoffset`), see [`draw_patch`];
//! * before anything is redrawn, the widget's rectangle is first copied
//!   from the off-screen background buffer (`V_CopyRect` from `BG`), which
//!   is what makes the numbers "erase" — the background holds the STBAR
//!   artwork (and, in net games, the face background) with the widget
//!   areas blank;
//! * widgets remember their last drawn value (`oldnum`/`oldinum`/`oldval`)
//!   and skip redraws when nothing changed. Vanilla actually redraws the
//!   number widgets every tic (the `oldnum` field is stored but never
//!   consulted); we honor the field's obvious purpose and skip unchanged
//!   redraws — the output is identical, just without the useless writes.
//!
//! All coordinates are in framebuffer pixels; the status bar lives in
//! `y` 168..200 (`ST_Y`).

use crate::errors::{EngineError, EngineResult};
use crate::gfx::patch::Patch;
use crate::render::IndexedFramebuffer;
use std::sync::Arc;

use super::ST_Y;

/// Height of the status bar strip in pixels (`32 * SCREEN_MUL`, `SCREEN_MUL`
/// is 1). The off-screen background buffer is this many rows tall.
pub const ST_HEIGHT: usize = 32;
/// Width of the status bar strip; same as the screen width.
pub const ST_WIDTH: usize = 320;
/// Size of the off-screen background buffer in bytes.
pub const STRIP_BYTES: usize = ST_WIDTH * ST_HEIGHT;

/// A palette-index pixel buffer patches can be drawn into.
///
/// Implemented by [`IndexedFramebuffer`] (the real screen) and
/// [`StripCanvas`] (the off-screen 320x32 background the bar artwork is
/// rendered into before being blitted onto the screen).
pub trait PatchCanvas {
    /// Canvas dimensions as `(width, height)` in pixels.
    fn canvas_size(&self) -> (usize, usize);
    /// The raw row-major pixel buffer, `width * height` bytes.
    fn buffer_mut(&mut self) -> &mut [u8];

    /// Writes `index` at `(x, y)`; out-of-bounds writes are silently
    /// clipped (the status bar deliberately clips instead of the
    /// RANGECHECK `I_Error` the original compiled in).
    fn put_pixel(&mut self, x: i32, y: i32, index: u8) {
        let (w, h) = self.canvas_size();
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            return;
        }
        self.buffer_mut()[y as usize * w + x as usize] = index;
    }
}

impl PatchCanvas for IndexedFramebuffer {
    fn canvas_size(&self) -> (usize, usize) {
        (crate::render::SCREEN_WIDTH, crate::render::SCREEN_HEIGHT)
    }

    fn buffer_mut(&mut self) -> &mut [u8] {
        self.as_mut_slice()
    }
}

/// A borrowed 320x32 strip used as the off-screen status bar background.
///
/// The strip holds the bar artwork with the widget areas blank; the
/// widgets "erase" themselves by copying their rectangle from here before
/// redrawing. See `ST_refreshBackground` in the original.
pub struct StripCanvas<'a> {
    buffer: &'a mut [u8],
}

impl<'a> StripCanvas<'a> {
    /// Wraps `buffer`, which must be exactly [`STRIP_BYTES`] long.
    ///
    /// # Panics
    /// Panics when the buffer is the wrong size; this is a programmer
    /// error, not a data-dependent condition.
    pub fn new(buffer: &'a mut [u8]) -> Self {
        assert_eq!(
            buffer.len(),
            STRIP_BYTES,
            "status bar background strip must be exactly 320x32"
        );
        Self { buffer }
    }
}

impl PatchCanvas for StripCanvas<'_> {
    fn canvas_size(&self) -> (usize, usize) {
        (ST_WIDTH, ST_HEIGHT)
    }

    fn buffer_mut(&mut self) -> &mut [u8] {
        self.buffer
    }
}

/// `V_DrawPatch`: draws a patch at origin `(x, y)`.
///
/// The patch origin is shifted by its `leftoffset`/`topoffset` the same
/// way the original does it. Pixels that fall outside the canvas are
/// clipped per-pixel; the original required the whole patch to fit and
/// otherwise drew nothing at all (RANGECHECK) or corrupted memory
/// (no RANGECHECK) — clipping is the sane middle ground.
pub fn draw_patch(canvas: &mut impl PatchCanvas, patch: &Patch, x: i32, y: i32) {
    let base_x = x - i32::from(patch.header.left_offset);
    let top_y = y - i32::from(patch.header.top_offset);
    for (col, column) in patch.columns.iter().enumerate() {
        let col_x = base_x + col as i32;
        for post in &column.posts {
            let row = top_y + i32::from(post.top_delta);
            for (i, &value) in post.pixels.iter().enumerate() {
                canvas.put_pixel(col_x, row + i as i32, value);
            }
        }
    }
}

/// Copies a rectangle from the off-screen background strip onto the
/// framebuffer. The `src_*` coordinates are in strip space (the bar lives
/// at `y` 0..32 there); `dst_*` in screen space.
///
/// The source rectangle is clipped to the strip and the destination to
/// the framebuffer - a widget whose area pokes out of the bar (wide
/// digit patches, say) erases only what actually exists, the same
/// per-pixel clipping philosophy [`draw_patch`] uses. The original
/// aborted on out-of-strip rects in RANGECHECK builds and read/wrote
/// adjacent memory otherwise; neither is a behavior worth keeping.
#[allow(clippy::too_many_arguments)]
pub(crate) fn copy_rect_from_strip(
    fb: &mut IndexedFramebuffer,
    strip: &[u8],
    src_x: i32,
    src_y: i32,
    width: i32,
    height: i32,
    dst_x: i32,
    dst_y: i32,
) -> EngineResult<()> {
    if width < 0 || height < 0 {
        // a widget asking for a negative-size erase is a bug, not data
        return Err(EngineError::Resource(format!(
            "status bar erase rect has negative size: {width}x{height}"
        )));
    }
    // clip the source rectangle against the strip; the destination
    // shifts along so the two stay in lockstep
    let mut src_x = src_x;
    let mut src_y = src_y;
    let mut dst_x = dst_x;
    let mut dst_y = dst_y;
    let mut width = width;
    let mut height = height;
    if src_x < 0 {
        let skip = -src_x;
        width -= skip;
        src_x = 0;
        dst_x += skip;
    }
    if src_y < 0 {
        let skip = -src_y;
        height -= skip;
        src_y = 0;
        dst_y += skip;
    }
    width = width.min(ST_WIDTH as i32 - src_x);
    height = height.min(ST_HEIGHT as i32 - src_y);
    if width <= 0 || height <= 0 {
        // the whole rectangle sits outside the strip; nothing to restore
        return Ok(());
    }
    for row in 0..height {
        let dst_row = dst_y + row;
        if dst_row < 0 || dst_row >= crate::render::SCREEN_HEIGHT as i32 {
            continue;
        }
        let src_off = (src_y + row) as usize * ST_WIDTH + src_x as usize;
        for col in 0..width {
            let dst_col = dst_x + col;
            if dst_col < 0 || dst_col >= crate::render::SCREEN_WIDTH as i32 {
                continue;
            }
            fb.set(
                dst_col as usize,
                dst_row as usize,
                strip[src_off + col as usize],
            );
        }
    }
    Ok(())
}

/// Blits the whole background strip onto the framebuffer at `y = ST_Y`.
pub(crate) fn blit_strip(fb: &mut IndexedFramebuffer, strip: &[u8]) {
    debug_assert_eq!(strip.len(), STRIP_BYTES);
    for row in 0..ST_HEIGHT {
        if let Some(dst) = fb.row_mut(ST_Y as usize + row) {
            dst.copy_from_slice(&strip[row * ST_WIDTH..(row + 1) * ST_WIDTH]);
        }
    }
}

/// Copies the rectangle `(x, y)` with `w` x `h` size from the strip to the
/// same screen position (`y` shifted down by `ST_Y`). This is how widgets
/// restore their area to the bar background before redrawing.
pub(crate) fn erase_rect(
    fb: &mut IndexedFramebuffer,
    strip: &[u8],
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> EngineResult<()> {
    copy_rect_from_strip(fb, strip, x, y - ST_Y, w, h, x, y)
}

/// Number widget (`st_number_t`): a right-justified number drawn with a
/// digit patch table (STTNUM* for the tall numbers, STYSNUM* for the
/// short yellow ammo/max-ammo numbers).
///
/// `num` values of [`super::NOT_AVAILABLE`] (1994) leave the area blank;
/// leading zeros are chopped, and a single `0` is drawn for zero — both
/// ported from `STlib_drawNum`.
pub struct NumberWidget {
    /// Right edge of the number (the anchor everything is justified to).
    pub x: i32,
    /// Baseline row of the digits.
    pub y: i32,
    /// Maximum number of digits.
    pub width: i32,
    /// Last value drawn; the dirty flag.
    pub oldnum: i32,
}

impl NumberWidget {
    /// `STlib_initNum`.
    pub fn new(x: i32, y: i32, width: i32) -> Self {
        Self {
            x,
            y,
            width,
            oldnum: 0,
        }
    }

    /// `STlib_updateNum` + `STlib_drawNum`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw(
        &mut self,
        fb: &mut IndexedFramebuffer,
        strip: &[u8],
        digits: &[Arc<Patch>; 10],
        minus: &Patch,
        num: i32,
        on: bool,
        refresh: bool,
    ) -> EngineResult<()> {
        if !on {
            return Ok(());
        }
        // unlike vanilla we actually skip the redraw when nothing changed;
        // the pixels would be identical anyway
        if !refresh && self.oldnum == num {
            return Ok(());
        }
        self.oldnum = num;

        let w = i32::from(digits[0].header.width);
        let h = i32::from(digits[0].header.height);
        let mut numdigits = self.width;
        let mut num = num;

        let neg = num < 0;
        if neg {
            if numdigits == 2 && num < -9 {
                num = -9;
            } else if numdigits == 3 && num < -99 {
                num = -99;
            }
            num = -num;
        }

        // clear the area from the background copy first
        let x = self.x - numdigits * w;
        copy_rect_from_strip(fb, strip, x, self.y - ST_Y, w * numdigits, h, x, self.y)?;

        // a non-number (1994) leaves the area blank
        if num == super::NOT_AVAILABLE {
            return Ok(());
        }

        let mut x = self.x;

        // special case: draw a single 0
        if num == 0 {
            draw_patch(fb, &digits[0], x - w, self.y);
        }

        while num != 0 && numdigits > 0 {
            x -= w;
            draw_patch(fb, &digits[(num % 10) as usize], x, self.y);
            num /= 10;
            numdigits -= 1;
        }

        if neg {
            draw_patch(fb, minus, x - 8, self.y);
        }
        Ok(())
    }
}

/// Percent widget (`st_percent_t`): a tall number plus the tall percent
/// sign patch drawn at the number's right edge.
pub struct PercentWidget {
    /// The wrapped number widget.
    pub number: NumberWidget,
}

impl PercentWidget {
    /// `STlib_initPercent` (width is fixed at 3).
    pub fn new(x: i32, y: i32) -> Self {
        Self {
            number: NumberWidget::new(x, y, 3),
        }
    }

    /// `STlib_updatePercent`. The percent sign only gets (re)drawn on a
    /// full refresh; the number honors the usual dirty flag.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw(
        &mut self,
        fb: &mut IndexedFramebuffer,
        strip: &[u8],
        digits: &[Arc<Patch>; 10],
        minus: &Patch,
        percent: &Patch,
        num: i32,
        on: bool,
        refresh: bool,
    ) -> EngineResult<()> {
        if refresh && on {
            draw_patch(fb, percent, self.number.x, self.number.y);
        }
        self.number.draw(fb, strip, digits, minus, num, on, refresh)
    }
}

/// Multi-icon widget (`st_multicon_t`): draws one of `icons` depending on
/// an external index — the face, the weapon-ownership numbers and the key
/// cards. Index `-1` means "draw nothing".
pub struct MultIconWidget {
    /// Center anchor of the icon.
    pub x: i32,
    /// Vertical anchor of the icon.
    pub y: i32,
    /// Last icon index drawn; `-1` means none.
    pub oldinum: i32,
}

impl MultIconWidget {
    /// `STlib_initMultIcon`.
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y, oldinum: -1 }
    }

    /// `STlib_updateMultIcon`.
    ///
    /// `inum` selects the icon; `-1` draws nothing (the vanilla behavior,
    /// including that switching *to* `-1` does not erase). An index out of
    /// bounds is an error, not a panic — the game layer passed us
    /// something bogus.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw(
        &mut self,
        fb: &mut IndexedFramebuffer,
        strip: &[u8],
        icons: &[Arc<Patch>],
        inum: i32,
        on: bool,
        refresh: bool,
    ) -> EngineResult<()> {
        if !on || inum == -1 || (!refresh && self.oldinum == inum) {
            return Ok(());
        }
        if inum < 0 || inum as usize >= icons.len() {
            return Err(EngineError::Resource(format!(
                "status bar icon index {inum} out of bounds ({} icons)",
                icons.len()
            )));
        }
        if self.oldinum != -1 {
            let old = &icons[self.oldinum as usize];
            let x = self.x - i32::from(old.header.left_offset);
            let y = self.y - i32::from(old.header.top_offset);
            erase_rect(
                fb,
                strip,
                x,
                y,
                i32::from(old.header.width),
                i32::from(old.header.height),
            )?;
        }
        draw_patch(fb, &icons[inum as usize], self.x, self.y);
        self.oldinum = inum;
        Ok(())
    }
}

/// Binary-icon widget (`st_binicon_t`): draws the icon while `val` is
/// true (the arms panel background in single player), erases it otherwise.
pub struct BinIconWidget {
    /// Center anchor of the icon.
    pub x: i32,
    /// Vertical anchor of the icon.
    pub y: i32,
    /// Last value drawn.
    pub oldval: bool,
}

impl BinIconWidget {
    /// `STlib_initBinIcon`.
    pub fn new(x: i32, y: i32) -> Self {
        Self {
            x,
            y,
            oldval: false,
        }
    }

    /// `STlib_updateBinIcon`.
    pub(crate) fn draw(
        &mut self,
        fb: &mut IndexedFramebuffer,
        strip: &[u8],
        icon: &Patch,
        val: bool,
        on: bool,
        refresh: bool,
    ) -> EngineResult<()> {
        if !on || (!refresh && self.oldval == val) {
            return Ok(());
        }
        if val {
            draw_patch(fb, icon, self.x, self.y);
        } else {
            let x = self.x - i32::from(icon.header.left_offset);
            let y = self.y - i32::from(icon.header.top_offset);
            erase_rect(
                fb,
                strip,
                x,
                y,
                i32::from(icon.header.width),
                i32::from(icon.header.height),
            )?;
        }
        self.oldval = val;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::patch::decode_patch;

    /// 2x2 patch, every pixel = 9.
    fn two_by_two() -> Patch {
        #[rustfmt::skip]
        let bytes: Vec<u8> = vec![
            2, 0, // width
            2, 0, // height
            0, 0, // left offset
            0, 0, // top offset
            16, 0, 0, 0, // column 0 ptr
            23, 0, 0, 0, // column 1 ptr
            // column 0: one post, rows 0..2
            0, 2, 0, 9, 9, 0, 0xff,
            // column 1: one post, rows 0..2
            0, 2, 0, 9, 9, 0, 0xff,
        ];
        decode_patch(&bytes).unwrap()
    }

    #[test]
    fn copy_rect_clips_to_strip_bounds() {
        let mut strip = [0u8; STRIP_BYTES];
        strip[0] = 0x11; // src (x=0, y=0)
        strip[1] = 0x22; // src (x=1, y=0)
        strip[31 * ST_WIDTH + 5] = 0xBB; // bottom strip row

        // left clip: src (-2,0) w=4 -> src x=0..2 lands in dst x=102..104
        let mut fb = IndexedFramebuffer::new();
        if let Some(row) = fb.row_mut(168) {
            row.fill(0xEE);
        }
        copy_rect_from_strip(&mut fb, &strip, -2, 0, 4, 1, 100, 168).unwrap();
        assert_eq!(fb.get(102, 168), Some(0x11));
        assert_eq!(fb.get(103, 168), Some(0x22));
        assert_eq!(fb.get(104, 168), Some(0xEE));

        // bottom clip: src y=30 h=4 -> only rows 30..32 (strip is 32 tall)
        let mut fb = IndexedFramebuffer::new();
        copy_rect_from_strip(&mut fb, &strip, 5, 30, 1, 4, 200, 168).unwrap();
        assert_eq!(fb.get(200, 168), Some(0)); // src row 30
        assert_eq!(fb.get(200, 169), Some(0xBB)); // src row 31
        assert_eq!(fb.get(200, 170), Some(0)); // beyond the clip, untouched

        // fully outside the strip is a no-op, not an error
        copy_rect_from_strip(&mut fb, &strip, 400, 0, 4, 4, 0, 168).unwrap();
        copy_rect_from_strip(&mut fb, &strip, 0, 40, 4, 4, 0, 168).unwrap();
        // negative size is a bug, reported as such
        assert!(copy_rect_from_strip(&mut fb, &strip, 0, 0, -1, 4, 0, 0).is_err());
        assert!(copy_rect_from_strip(&mut fb, &strip, 0, 0, 4, 4, 0, 0).is_ok());
    }

    #[test]
    fn copy_rect_copies_pixels_verbatim() {
        let mut fb = IndexedFramebuffer::new();
        let mut strip = [0u8; STRIP_BYTES];
        strip[3 * ST_WIDTH + 7] = 77;
        copy_rect_from_strip(&mut fb, &strip, 5, 2, 4, 4, 100, 150).unwrap();
        assert_eq!(fb.get(102, 151), Some(77));
    }

    #[test]
    fn draw_patch_clips_out_of_bounds_pixels() {
        let patch = two_by_two();
        let mut fb = IndexedFramebuffer::new();
        draw_patch(&mut fb, &patch, -1, -1);
        assert_eq!(fb.get(0, 0), Some(9)); // only the bottom-right pixel lands
        assert_eq!(fb.get(1, 0), Some(0));
        assert_eq!(fb.get(0, 1), Some(0));
    }

    #[test]
    fn draw_patch_at_origin() {
        let patch = two_by_two();
        let mut fb = IndexedFramebuffer::new();
        draw_patch(&mut fb, &patch, 10, 100);
        assert_eq!(fb.get(10, 100), Some(9));
        assert_eq!(fb.get(11, 101), Some(9));
        assert_eq!(fb.get(12, 102), Some(0));
    }
}
