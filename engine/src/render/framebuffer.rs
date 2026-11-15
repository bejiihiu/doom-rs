//! The software renderer's indexed (palette-index) framebuffer, ported
//! from the screen buffer handling in `v_video.c` / `r_draw.c`.
//!
//! The classic engine pokes an 8-bit, palette-indexed linear buffer and
//! only applies the palette at the very end (via `I_FinishUpdate`). This
//! type does the same: it holds raw palette indices, nothing is converted
//! until [`IndexedFramebuffer::to_rgba_bytes`] is called.
//!
//! Screen geometry is fixed at 320x200, the original VGA mode 13h the
//! engine targets. [`SCREEN_WIDTH`] / [`SCREEN_HEIGHT`] are used by the
//! column/span drawers and the view/window code; `R_InitBuffer`'s
//! `ylookup`/`columnofs` tables are deliberately not ported — the buffer is
//! linear, so row lookup is just a multiply, and the "subwindow" offsets
//! belong to the window-drawing layer (agent 24+/border code), not to the
//! core framebuffer.

use crate::gfx::palette::Palette;

/// Horizonal resolution of the classic engine's screen.
pub const SCREEN_WIDTH: usize = 320;
/// Vertical resolution of the classic engine's screen.
pub const SCREEN_HEIGHT: usize = 200;
/// Total pixel count in the framebuffer.
pub const SCREEN_PIXELS: usize = SCREEN_WIDTH * SCREEN_HEIGHT;

/// Index into the flat pixel array for `(x, y)`, or `None` out of bounds.
///
/// Shared by the column/span drawers: they draw with clipping rather than
/// crashing, and always with the same bounds rule.
pub(crate) fn pixel_addr(x: i32, y: i32) -> Option<usize> {
    if (0..SCREEN_WIDTH as i32).contains(&x) && (0..SCREEN_HEIGHT as i32).contains(&y) {
        Some(y as usize * SCREEN_WIDTH + x as usize)
    } else {
        None
    }
}

/// An 8-bit, palette-indexed 320x200 framebuffer.
///
/// The buffer is a flat row-major array of palette indices, exactly the
/// layout `screens[0]` had in the original engine. `new` zeroes the buffer
/// (the original `malloc` left it undefined; zeroing makes framebuffer
/// tests deterministic and costs nothing measurable).
#[derive(Clone, PartialEq, Eq)]
pub struct IndexedFramebuffer {
    pixels: [u8; SCREEN_PIXELS],
}

impl IndexedFramebuffer {
    /// A zeroed framebuffer.
    pub const fn new() -> Self {
        Self {
            pixels: [0; SCREEN_PIXELS],
        }
    }

    /// Fills the whole buffer with `index`.
    pub fn clear(&mut self, index: u8) {
        self.pixels.fill(index);
    }

    /// The pixel at `(x, y)`, or `None` when out of bounds.
    pub fn get(&self, x: usize, y: usize) -> Option<u8> {
        (x < SCREEN_WIDTH && y < SCREEN_HEIGHT).then(|| self.pixels[y * SCREEN_WIDTH + x])
    }

    /// Sets the pixel at `(x, y)`; out-of-bounds writes are ignored.
    pub fn set(&mut self, x: usize, y: usize, index: u8) {
        if x < SCREEN_WIDTH && y < SCREEN_HEIGHT {
            self.pixels[y * SCREEN_WIDTH + x] = index;
        }
    }

    /// Row `y` as a slice, or `None` when out of bounds.
    pub fn row(&self, y: usize) -> Option<&[u8]> {
        (y < SCREEN_HEIGHT).then(|| &self.pixels[y * SCREEN_WIDTH..(y + 1) * SCREEN_WIDTH])
    }

    /// Row `y` as a mutable slice, or `None` when out of bounds.
    pub fn row_mut(&mut self, y: usize) -> Option<&mut [u8]> {
        (y < SCREEN_HEIGHT).then(|| &mut self.pixels[y * SCREEN_WIDTH..(y + 1) * SCREEN_WIDTH])
    }

    /// The flat pixel array.
    pub const fn as_slice(&self) -> &[u8] {
        &self.pixels
    }

    /// The flat pixel array, mutable.
    pub const fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    /// Copies every pixel from `src` into `self`.
    ///
    /// Replaces the original's `memcpy` screens: both buffers must be a
    /// full screen, so the copy is one `memcpy` under the hood.
    pub fn blit_from(&mut self, src: &Self) {
        self.pixels.copy_from_slice(&src.pixels);
    }

    /// Applies `palette` and writes RGBX pixels into `dest`.
    ///
    /// Each output pixel is packed `0x00RRGGBB` — the format `softbuffer`
    /// expects as `XRGB8` on little-endian hosts (and what "RGBX" means in
    /// most engine contexts). The destination slice must be exactly
    /// [`SCREEN_PIXELS`] long; the type system enforces the size, so a
    /// mismatched buffer is a compile error, not a runtime panic.
    pub fn apply_palette(&self, palette: &Palette, dest: &mut [u32; SCREEN_PIXELS]) {
        for (out, &index) in dest.iter_mut().zip(self.pixels.iter()) {
            let [r, g, b] = palette.color(index);
            *out = ((r as u32) << 16) | ((g as u32) << 8) | b as u32;
        }
    }

    /// Applies `palette` and returns the RGBX pixels (`0x00RRGGBB`).
    ///
    /// One allocation per call — use [`IndexedFramebuffer::apply_palette`]
    /// with a reused destination on the hot path instead.
    pub fn to_rgba_bytes(&self, palette: &Palette) -> Vec<u32> {
        let mut out = vec![0u32; SCREEN_PIXELS];
        self.apply_palette(palette, dest_slice_mut(&mut out));
        out
    }
}

impl Default for IndexedFramebuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for IndexedFramebuffer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("IndexedFramebuffer")
            .field("width", &SCREEN_WIDTH)
            .field("height", &SCREEN_HEIGHT)
            .finish_non_exhaustive()
    }
}

/// Converts a `Vec<u32>` known to be [`SCREEN_PIXELS`] long into the fixed
/// array reference `apply_palette` takes. The length was just set by the
/// caller, so the conversion cannot fail; a plain `expect` documents the
/// invariant instead of hiding it.
fn dest_slice_mut(out: &mut [u32]) -> &mut [u32; SCREEN_PIXELS] {
    out.try_into()
        .expect("dest was allocated with SCREEN_PIXELS elements")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_buffer_is_zeroed() {
        let fb = IndexedFramebuffer::new();
        assert_eq!(fb.as_slice().len(), SCREEN_PIXELS);
        assert!(fb.as_slice().iter().all(|&p| p == 0));
    }

    #[test]
    fn clear_fills_with_index() {
        let mut fb = IndexedFramebuffer::new();
        fb.clear(0xAB);
        assert!(fb.as_slice().iter().all(|&p| p == 0xAB));
        fb.clear(0);
        assert!(fb.as_slice().iter().all(|&p| p == 0));
    }

    #[test]
    fn set_and_get_round_trip() {
        let mut fb = IndexedFramebuffer::new();
        fb.set(0, 0, 1);
        fb.set(SCREEN_WIDTH - 1, SCREEN_HEIGHT - 1, 2);
        fb.set(100, 50, 3);
        assert_eq!(fb.get(0, 0), Some(1));
        assert_eq!(fb.get(SCREEN_WIDTH - 1, SCREEN_HEIGHT - 1), Some(2));
        assert_eq!(fb.get(100, 50), Some(3));
    }

    #[test]
    fn out_of_bounds_access_is_none_and_ignored() {
        let mut fb = IndexedFramebuffer::new();
        fb.set(SCREEN_WIDTH, 0, 9);
        fb.set(0, SCREEN_HEIGHT, 9);
        fb.set(usize::MAX, usize::MAX, 9);
        assert_eq!(fb.get(SCREEN_WIDTH, 0), None);
        assert_eq!(fb.get(0, SCREEN_HEIGHT), None);
        assert_eq!(fb.get(usize::MAX, usize::MAX), None);
        // nothing was written: buffer is still all zeroes
        assert!(fb.as_slice().iter().all(|&p| p == 0));
    }

    #[test]
    fn row_views_match_flat_buffer() {
        let mut fb = IndexedFramebuffer::new();
        for y in 0..SCREEN_HEIGHT {
            for x in 0..SCREEN_WIDTH {
                fb.set(x, y, ((x + y * 3) & 0xFF) as u8);
            }
        }
        for y in 0..SCREEN_HEIGHT {
            let row = fb.row(y).unwrap();
            assert_eq!(row.len(), SCREEN_WIDTH);
            for (x, &px) in row.iter().enumerate() {
                assert_eq!(px, ((x + y * 3) & 0xFF) as u8);
            }
        }
        assert!(fb.row(SCREEN_HEIGHT).is_none());
        assert!(fb.row_mut(SCREEN_HEIGHT).is_none());
    }

    #[test]
    fn blit_from_copies_everything() {
        let mut a = IndexedFramebuffer::new();
        let mut b = IndexedFramebuffer::new();
        a.clear(0x11);
        b.clear(0x22);
        b.blit_from(&a);
        assert_eq!(a, b);
    }

    #[test]
    fn apply_palette_packs_rrggbb() {
        let palette = Palette {
            colors: [[0, 0, 0]; 256],
        };
        let mut colors = palette.colors;
        colors[7] = [0x12, 0x34, 0x56];
        let palette = Palette { colors };

        let mut fb = IndexedFramebuffer::new();
        fb.clear(7);
        let mut dest = [0u32; SCREEN_PIXELS];
        fb.apply_palette(&palette, &mut dest);

        assert_eq!(dest[0], 0x0012_3456);
        assert_eq!(dest[SCREEN_PIXELS - 1], 0x0012_3456);
    }

    #[test]
    fn apply_palette_distinguishes_indices() {
        let mut colors = [[0u8; 3]; 256];
        for (i, color) in colors.iter_mut().enumerate() {
            *color = [i as u8, 0, 0];
        }
        let palette = Palette { colors };

        let mut fb = IndexedFramebuffer::new();
        fb.set(0, 0, 0);
        fb.set(1, 0, 255);

        let rgba = fb.to_rgba_bytes(&palette);
        assert_eq!(rgba[0], 0x0000_0000);
        assert_eq!(rgba[1], 0x00FF_0000);
        assert_eq!(rgba.len(), SCREEN_PIXELS);
    }
}
