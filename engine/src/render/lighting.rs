//! The lighting lookup tables, ported from `R_InitColormaps`
//! (`r_data.c`) and `R_InitLightTables` / the `scalelight` setup in
//! `R_ExecuteSetViewSize` (`r_main.c`).
//!
//! The original engine builds two lookup tables at startup and picks a
//! COLORMAP entry from them per wall/plane/sprite:
//!
//! * `zlight[LIGHTLEVELS][MAXLIGHTZ]` — from a view distance, selects a
//!   colormap level. Built once at startup.
//! * `scalelight[LIGHTLEVELS][MAXLIGHTSCALE]` — from a projected scale,
//!   selects a colormap level. Built per view size (depends on
//!   `viewwidth << detailshift`).
//!
//! Both store *indices into the COLORMAP lump* (0..=31), not pointers.
//! The original stores `lighttable_t*` into the `malloc`'d COLORMAP
//! buffer; indices are type-safe, copy cheaply and stay valid if the
//! lump is re-decoded. Resolving an index against the decoded colormaps
//! is the caller's job (one array lookup).
//!
//! Note on `R_ScaleLightFrac`: the original linuxdoom-1.10 does not
//! define it — sprite/wall lighting goes through the same
//! `(lightlevel >> LIGHTSEGSHIFT) + extralight` row selection with
//! clamping in four places (`r_segs.c` ×2, `r_plane.c`, `r_things.c`
//! ×2). That row selection is ported here as [`select_row`] instead of
//! pretending the symbol exists.

use crate::fixed::Fixed;
use crate::gfx::palette::NUM_DIMINISHING_COLORMAPS;

use super::framebuffer::SCREEN_WIDTH;

/// Number of light levels (`LIGHTLEVELS`, 16).
pub const LIGHTLEVELS: usize = 16;
/// `LIGHTSEGSHIFT`: rows of the light tables per sector light level step.
pub const LIGHTSEGSHIFT: u32 = 4;
/// `MAXLIGHTSCALE`: columns of the scalelight table.
pub const MAXLIGHTSCALE: usize = 48;
/// `LIGHTSCALESHIFT`: shift from a scale fixed_t to a scalelight index.
pub const LIGHTSCALESHIFT: u32 = 12;
/// `MAXLIGHTZ`: columns of the zlight table.
pub const MAXLIGHTZ: usize = 128;
/// `LIGHTZSHIFT`: shift from a distance fixed_t to a zlight index.
pub const LIGHTZSHIFT: u32 = 20;
/// `DISTMAP`: quantization divisor between distance/scale and colormap level.
pub const DISTMAP: i32 = 2;
/// `NUMCOLORMAPS` from the original, aliased to the decoder constant.
pub const NUMCOLORMAPS: usize = NUM_DIMINISHING_COLORMAPS;

/// The computed light tables, as colormap indices into COLORMAP.
///
/// `zlight(i, j)` / `scalelight(i, j)` return values in
/// `0..NUMCOLORMAPS`. Built with [`LightTables::build`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightTables {
    zlight: [[usize; MAXLIGHTZ]; LIGHTLEVELS],
    scalelight: [[usize; MAXLIGHTSCALE]; LIGHTLEVELS],
}

impl LightTables {
    /// Builds both tables.
    ///
    /// `scaled_view_width` is `viewwidth << detailshift` — 320 on the
    /// classic high-detail 200p screen (vanilla passes
    /// `viewwidth << detailshift`, not `scaledviewwidth`, into the
    /// formula, but they are numerically identical).
    ///
    /// The formulas are a literal port of the original fixed-point math:
    ///
    /// * `scale = FixedDiv((SCREENWIDTH/2) << FRACBITS, (j+1) << LIGHTZSHIFT)`,
    ///   then `scale >>= LIGHTSCALESHIFT`, `level = startmap - scale / DISTMAP`;
    /// * `level = startmap - j * SCREENWIDTH / scaled_view_width / DISTMAP`,
    ///
    /// each clamped to `0..NUMCOLORMAPS - 1`.
    pub fn build(scaled_view_width: i32) -> Self {
        assert!(scaled_view_width > 0, "scaled view width must be positive");

        let mut zlight = [[0usize; MAXLIGHTZ]; LIGHTLEVELS];
        let mut scalelight = [[0usize; MAXLIGHTSCALE]; LIGHTLEVELS];

        for row in 0..LIGHTLEVELS {
            let startmap = ((LIGHTLEVELS - 1 - row) * 2) * NUMCOLORMAPS / LIGHTLEVELS;

            // R_InitLightTables
            for (j, cell) in zlight[row].iter_mut().enumerate() {
                let scale = Fixed::from_int(SCREEN_WIDTH as i32 / 2)
                    / Fixed::from_raw((j as i32 + 1) << LIGHTZSHIFT);
                let scale = Fixed::from_raw(scale.to_raw() >> LIGHTSCALESHIFT);
                let level = startmap as i32 - scale.to_raw() / DISTMAP;
                *cell = clamp_colormap_level(level);
            }

            // scalelight, the per-view-size part of R_ExecuteSetViewSize
            for (j, cell) in scalelight[row].iter_mut().enumerate() {
                let level = startmap as i32
                    - (j as i32) * SCREEN_WIDTH as i32 / scaled_view_width / DISTMAP;
                *cell = clamp_colormap_level(level);
            }
        }

        Self { zlight, scalelight }
    }

    /// The raw zlight row, values as colormap indices.
    pub const fn zlight_row(&self, row: usize) -> &[usize; MAXLIGHTZ] {
        &self.zlight[row]
    }

    /// The raw scalelight row, values as colormap indices.
    pub const fn scalelight_row(&self, row: usize) -> &[usize; MAXLIGHTSCALE] {
        &self.scalelight[row]
    }

    /// Colormap level for `(light row, distance index)`.
    ///
    /// `z` is clamped to `MAXLIGHTZ - 1`, the same clamp the original
    /// callers apply (`if (index >= MAXLIGHTZ) index = MAXLIGHTZ - 1;`).
    pub const fn zlight(&self, row: usize, z: usize) -> usize {
        self.zlight[row][clamp(z, MAXLIGHTZ)]
    }

    /// Colormap level for `(light row, scale index)`.
    ///
    /// `scale` is clamped to `MAXLIGHTSCALE - 1`, like the original.
    pub const fn scalelight(&self, row: usize, scale: usize) -> usize {
        self.scalelight[row][clamp(scale, MAXLIGHTSCALE)]
    }

    /// The `Z`/`distance >> LIGHTZSHIFT` → zlight index mapping,
    /// clamped the way `R_MapPlane` does it.
    pub const fn distance_to_z(distance: Fixed) -> usize {
        clamp((distance.to_raw() >> LIGHTZSHIFT) as usize, MAXLIGHTZ)
    }

    /// The `scale >> LIGHTSCALESHIFT` → scalelight index mapping,
    /// clamped the way the wall and sprite drawers do it.
    pub const fn scale_to_index(scale: Fixed) -> usize {
        clamp((scale.to_raw() >> LIGHTSCALESHIFT) as usize, MAXLIGHTSCALE)
    }
}

/// `scalelightfixed`: every scale maps to the same (fixed) colormap
/// level — used when the player wears a fixed colormap (e.g. radiation
/// suit). `level` is an index into the COLORMAP lump.
pub fn scalelight_fixed(level: usize) -> [usize; MAXLIGHTSCALE] {
    [clamp_colormap_level(level as i32); MAXLIGHTSCALE]
}

/// Row selection shared by walls, flats and sprites:
/// `(lightlevel >> LIGHTSEGSHIFT) + extralight`, clamped to
/// `0..LIGHTLEVELS - 1`.
///
/// The original inlines this in four places and also accepts negative
/// `extralight`; we take the same signed inputs and clamp.
pub fn select_row(lightlevel: i32, extralight: i32) -> usize {
    let row = (lightlevel >> LIGHTSEGSHIFT) + extralight;
    if row < 0 {
        0
    } else if row >= LIGHTLEVELS as i32 {
        LIGHTLEVELS - 1
    } else {
        row as usize
    }
}

#[inline]
const fn clamp(value: usize, max: usize) -> usize {
    if value >= max { max - 1 } else { value }
}

#[inline]
const fn clamp_colormap_level(level: i32) -> usize {
    if level < 0 {
        0
    } else if level >= NUMCOLORMAPS as i32 {
        NUMCOLORMAPS - 1
    } else {
        level as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zlight_clamps_dark_rows_to_zero() {
        let tables = LightTables::build(320);
        // row 15: startmap = 0; small j makes `startmap - scale/2` negative
        assert_eq!(tables.zlight(15, 0), 0);
        assert_eq!(tables.zlight(15, 127), 0);
    }

    #[test]
    fn zlight_far_end_clamps_bright_rows_to_max() {
        let tables = LightTables::build(320);
        // row 0: startmap = 60; at maximum distance the level exceeds the
        // diminishing colormap count and clamps to NUMCOLORMAPS - 1 = 31
        assert_eq!(tables.zlight(0, 127), 31);
        // ...while near walls land in the fullbright map (vanilla behavior)
        assert_eq!(tables.zlight(0, 0), 0);
    }

    #[test]
    fn zlight_known_value() {
        let tables = LightTables::build(320);
        // row 8: startmap = 28; j = 5:
        //   scale = FixedDiv(160.0, 6<<20) = 109226 (raw) -> >> 12 -> 26
        //   level = 28 - 26 / 2 = 15
        assert_eq!(tables.zlight(8, 5), 15);
    }

    #[test]
    fn zlight_rows_are_non_decreasing() {
        // further away -> darker colormap level (index grows), never wraps back
        let tables = LightTables::build(320);
        for (row, zlight_row) in tables.zlight.iter().enumerate() {
            for w in zlight_row.windows(2) {
                assert!(w[0] <= w[1], "zlight row {row} decreases");
            }
        }
    }

    #[test]
    fn scalelight_known_values() {
        let tables = LightTables::build(320);
        // row 8: startmap = 28; level = 28 - j * 320 / 320 / 2 = 28 - j/2
        assert_eq!(tables.scalelight(8, 0), 28);
        assert_eq!(tables.scalelight(8, 13), 22); // 13 / 2 = 6
        assert_eq!(tables.scalelight(8, 47), 5); // 47 / 2 = 23
        // row 0: startmap = 60; large j clamps to 31
        assert_eq!(tables.scalelight(0, 47), 31);
        // row 15: startmap = 0
        assert_eq!(tables.scalelight(15, 0), 0);
        assert_eq!(tables.scalelight(15, 47), 0);
    }

    #[test]
    fn scalelight_rows_are_non_increasing() {
        // bigger projected scale -> subtract more -> never wraps back
        let tables = LightTables::build(320);
        for row in 0..LIGHTLEVELS {
            let row_data = &tables.scalelight_row(row);
            for w in row_data.windows(2) {
                assert!(w[0] >= w[1], "scalelight row {row} increases at {w:?}");
            }
        }
    }

    #[test]
    fn scalelight_respects_scaled_view_width() {
        // doubled denominator halves the subtraction term
        let wide = LightTables::build(640);
        let base = LightTables::build(320);
        // j = 4: 4*320/320/2 = 2 (320) vs 4*320/640/2 = 1 (640)
        assert_ne!(wide.scalelight(8, 4), base.scalelight(8, 4));
        assert_eq!(base.scalelight(8, 4), 26);
        assert_eq!(wide.scalelight(8, 4), 27);
    }

    #[test]
    fn light_row_selection_matches_inlined_original() {
        // (lightlevel >> 4) + extralight, clamped
        assert_eq!(select_row(128, 0), 8);
        assert_eq!(select_row(255, 0), 15);
        assert_eq!(select_row(255, 20), 15); // clamp high
        assert_eq!(select_row(20, -3), 0); // 1 - 3, clamp low
        assert_eq!(select_row(0, -31), 0); // clamp low hard
        assert_eq!(select_row(255, -1), 14);
    }

    #[test]
    fn distance_and_scale_index_clamps() {
        assert_eq!(LightTables::distance_to_z(Fixed::ZERO), 0);
        assert_eq!(LightTables::distance_to_z(Fixed::from_int(100)), 6); // 100 << 16 >> 20 = 6
        assert_eq!(
            LightTables::distance_to_z(Fixed::from_int(2048)),
            MAXLIGHTZ - 1
        ); // clamp
        assert_eq!(LightTables::scale_to_index(Fixed::ONE), 16); // 65536 >> 12
        assert_eq!(
            LightTables::scale_to_index(Fixed::from_int(10)),
            MAXLIGHTSCALE - 1
        ); // clamp
        assert_eq!(LightTables::scale_to_index(Fixed::ZERO), 0);
    }

    #[test]
    fn scalelight_fixed_fills_one_colormap() {
        let table = scalelight_fixed(7);
        assert!(table.iter().all(|&i| i == 7));
        // out-of-range levels clamp instead of producing a wild index
        let table = scalelight_fixed(999);
        assert!(table.iter().all(|&i| i == NUMCOLORMAPS - 1));
    }

    #[test]
    fn accessors_clamp_indices() {
        let tables = LightTables::build(320);
        assert_eq!(
            tables.zlight(0, MAXLIGHTZ + 100),
            tables.zlight(0, MAXLIGHTZ - 1)
        );
        assert_eq!(
            tables.scalelight(0, MAXLIGHTSCALE + 100),
            tables.scalelight(0, MAXLIGHTSCALE - 1)
        );
    }
}
