//! Floors and ceilings: the horizontal span renderer, ported from
//! `r_plane.c` (`R_ClearPlanes`, `R_MapPlane`, `R_MakeSpans`,
//! `R_DrawPlanes`).
//!
//! Walls mark the floor/ceiling band of every column on a shared flat as a
//! [`Visplane`](super::planes::Visplane) — one `(top, bottom)` row pair per
//! column. This module sweeps every visplane after the BSP pass and paints
//! constant-z horizontal spans of the flat texture through the existing
//! [`draw_span`] / [`draw_span_low`] drawers. Sky planes are not drawn here;
//! they are reported through [`PlaneRenderReport::sky_planes_deferred`] so
//! the sky pass can pick them up.
//!
//! Layout notes:
//!
//! * [`PlaneProjection`] holds `yslope[]` and `distscale[]`, the per-view
//!   tables the original builds in `R_SetViewSize`. Build one per view
//!   size and reuse it for every frame.
//! * The C `cachedheight`/`cacheddistance`/`cachedxstep`/`cachedystep`
//!   globals (file-scope, zero-initialized) live inside [`render_planes`]
//!   and are re-created per frame, matching their zero-init lifetime.
//! * `basexscale`/`baseyscale` are the per-frame `R_ClearPlanes` part of
//!   the texture setup. The clip-array half of `R_ClearPlanes`
//!   (`floorclip`/`ceilingclip`) lives in the wall frame's `clear`.
//! * Marker rows are bytes, so rows `0..=255` are "possible" even though
//!   only `0..view_height` can actually be marked; the slope tables and
//!   the row caches are sized to 256 entries to stay total for any byte
//!   the marker logic could emit. The original indexed
//!   `yslope[0..SCREENHEIGHT]` with the same bytes and happened to stay
//!   in bounds for view heights of 200.
//! * `spanstart` is a plain `[i32; 256]` initialized to zero, like the C
//!   global.
//! * `flattranslation` (the flat global-animation remap) is the identity
//!   here; global animation lands with the specials/animation work.
//! * `R_InitPlanes` was a no-op in the original ("Doh!") and has no
//!   counterpart.

use std::collections::HashMap;

use crate::errors::{EngineError, EngineResult};
use crate::fixed::{FRACBITS, Fixed};
use crate::gfx::flat::Flat;
use crate::resources::ResourceManager;
use crate::tables::{Angle, FINE_COSINE, FINE_SINE};

use super::framebuffer::IndexedFramebuffer;
use super::lighting::{LightTables, select_row};
use super::planes::PlaneSet;
use super::span::{SpanParams, draw_span, draw_span_low};
use super::wall::WallView;

/// Rows that can hold a marker byte. See the module docs for why this is
/// not `SCREEN_HEIGHT`.
const SLOPE_LEN: usize = 256;

/// `yslope[]` and `distscale[]` for one view size.
///
/// Ported from the plane half of `R_SetViewSize` (`r_main.c`):
///
/// * `yslope[i] = FixedDiv((viewwidth << detailshift) / 2 * FRACUNIT,
///   abs(((i - viewheight / 2) << FRACBITS) + FRACUNIT / 2))` for rows
///   `0..256` (not just `view_height`; see the module docs),
/// * `distscale[i] = FixedDiv(FRACUNIT, abs(finecosine[xtoviewangle[i] >>
///   ANGLETOFINESHIFT]))` for columns `0..view_width`.
#[derive(Clone, Debug)]
pub struct PlaneProjection {
    yslope: Vec<Fixed>,
    distscale: Vec<Fixed>,
}

impl PlaneProjection {
    /// Builds the tables for the view size and tables `view` carries.
    pub fn build(view: &WallView<'_>) -> Self {
        let view_width = view.view_width;
        let view_height = view.view_height;
        let scaled_half = Fixed::from_int((view_width << view.detail_shift) / 2);
        let yslope = (0..SLOPE_LEN)
            .map(|i| {
                let dy = ((i as i64 - i64::from(view_height / 2)) << FRACBITS) + 0x8000;
                scaled_half / Fixed::from_raw(dy.abs() as i32)
            })
            .collect();

        let distscale = (0..view_width)
            .map(|i| {
                let fine = view.projection.at(i).fine_angle();
                Fixed::ONE / FINE_COSINE[fine].abs()
            })
            .collect();

        Self { yslope, distscale }
    }

    /// `yslope[row]`; rows beyond `view_height` are defined (see module docs).
    #[inline]
    pub fn yslope(&self, row: i32) -> Fixed {
        self.yslope[row as usize]
    }

    /// `distscale[column]`.
    #[inline]
    pub fn distscale(&self, column: i32) -> Fixed {
        self.distscale[column as usize]
    }
}

/// The flat namespace (`F_START`..`F_END`) with names resolved, ported
/// from `R_InitFlats` / `R_FlatNumForName` (`r_data.c`) with the flat
/// animation remap left as the identity.
///
/// Flats are indexed by their number in the namespace — exactly what the
/// original stores in `visplane_t::picnum` (`R_FlatNumForName` returns
/// `lump - firstflat`) and what `R_DrawPlanes` feeds to
/// `firstflat + flattranslation[picnum]`. All flats are decoded once, at
/// build time (64x64 palette indices each; this mirrors how the texture
/// set pre-decodes its patches).
#[derive(Debug)]
pub struct FlatSet {
    flats: Vec<Flat>,
    names: HashMap<String, u32>,
}

impl FlatSet {
    /// Loads every flat in the lump namespace. Fails when the WAD has no
    /// usable `F_START`/`F_END` markers or a flat is malformed.
    pub fn from_resources(resources: &ResourceManager) -> EngineResult<Self> {
        let (first, last) = match (resources.first_flat(), resources.last_flat()) {
            (Some(first), Some(last)) if first <= last => (first, last),
            (Some(_), Some(_)) => {
                return Err(EngineError::Resource(
                    "flat namespace is empty (F_START after F_END)".into(),
                ));
            }
            _ => {
                return Err(EngineError::Resource(
                    "no flat namespace markers (F_START/F_END) in the WAD".into(),
                ));
            }
        };

        let entries = resources.wads().entries();
        let mut flats = Vec::with_capacity(last - first + 1);
        let mut names = HashMap::with_capacity(last - first + 1);
        for (offset, index) in (first..=last).enumerate() {
            let data = resources.lump_data_index(index)?;
            flats.push(Flat::parse(data)?);
            let name = String::from_utf8_lossy(entries[index].name.trimmed()).to_ascii_uppercase();
            names.entry(name).or_insert(offset as u32);
        }

        Ok(Self { flats, names })
    }

    /// Number of flats in the namespace (`numflats`).
    pub fn len(&self) -> usize {
        self.flats.len()
    }

    /// Always false after a successful [`FlatSet::from_resources`].
    pub fn is_empty(&self) -> bool {
        self.flats.is_empty()
    }

    /// The decoded flat for the namespace-relative `picnum`, if it exists.
    pub fn flat(&self, picnum: u32) -> Option<&Flat> {
        self.flats.get(picnum as usize)
    }

    /// `R_FlatNumForName`: the namespace-relative number of the flat
    /// `raw_name` (case insensitive, NUL-padded). Pass the sky flat's name
    /// (`F_SKY1`) to get the `skyflatnum` a caller can hand to
    /// [`render_planes`] as `sky_picnum`.
    pub fn num_for_name(&self, raw_name: &[u8; 8]) -> Option<u32> {
        let end = raw_name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(raw_name.len());
        let name = String::from_utf8_lossy(&raw_name[..end]).to_ascii_uppercase();
        self.names.get(&name).copied()
    }
}

/// What [`render_planes`] did, for callers that want to know.
#[derive(Debug, Default)]
pub struct PlaneRenderReport {
    /// Regular flat planes that went through the scan loop.
    pub planes_drawn: u32,
    /// `R_MapPlane` calls (each one row of a span).
    pub spans_drawn: u32,
    /// Sky planes (`picnum == sky_picnum`) skipped for the sky pass.
    pub sky_planes_deferred: u32,
    /// Non-fatal problems: missing flats, missing colormaps, ...
    pub errors: Vec<EngineError>,
}

/// Per-frame scratch for the plane sweep — the C file-scope globals of
/// `r_plane.c` rolled into one owning struct.
struct PlaneRaster<'a> {
    fb: &'a mut IndexedFramebuffer,
    view: &'a WallView<'a>,
    projection: &'a PlaneProjection,
    basexscale: Fixed,
    baseyscale: Fixed,
    cached_height: [Fixed; SLOPE_LEN],
    cached_distance: [Fixed; SLOPE_LEN],
    cached_xstep: [Fixed; SLOPE_LEN],
    cached_ystep: [Fixed; SLOPE_LEN],
    // per-plane state, switched in the outer loop
    planeheight: Fixed,
    light_row: usize,
    flat: Option<&'a Flat>,
    spans: u32,
}

impl PlaneRaster<'_> {
    /// `R_MapPlane`: span setup for one row and hand the span to the
    /// drawer.
    fn map_plane(&mut self, y: i32, x1: i32, x2: i32) {
        let Some(flat) = self.flat else {
            // the outer loop guarantees this is never hit
            tracing::error!("R_MapPlane called without a flat bound");
            return;
        };
        let yi = y as usize;

        let (distance, xstep, ystep) = if self.planeheight != self.cached_height[yi] {
            self.cached_height[yi] = self.planeheight;
            let distance = self.planeheight * self.projection.yslope(y);
            self.cached_distance[yi] = distance;
            self.cached_xstep[yi] = distance * self.basexscale;
            self.cached_ystep[yi] = distance * self.baseyscale;
            (distance, self.cached_xstep[yi], self.cached_ystep[yi])
        } else {
            (
                self.cached_distance[yi],
                self.cached_xstep[yi],
                self.cached_ystep[yi],
            )
        };

        let length = distance * self.projection.distscale(x1);
        let angle = (self.view.view_angle + self.view.projection.at(x1)).fine_angle();
        let x_frac = self.view.view_x + FINE_COSINE[angle] * length;
        let y_frac = -self.view.view_y - FINE_SINE[angle] * length;

        // colormaps are never empty here (guarded in render_planes), but
        // index through get() so a bad fixedcolormap level cannot panic
        let colormaps = self.view.colormaps;
        let colormap = match self.view.fixed_colormap {
            Some(level) => colormaps
                .get(level.min(colormaps.len() - 1))
                .expect("colormaps non-empty (guarded)"),
            None => {
                let index = LightTables::distance_to_z(distance);
                let level = self
                    .view
                    .light
                    .zlight(self.light_row, index)
                    .min(colormaps.len() - 1);
                colormaps.get(level).expect("colormaps non-empty (guarded)")
            }
        };

        let params = SpanParams {
            colormap,
            y,
            x_lo: x1,
            x_hi: x2,
            x_frac,
            y_frac,
            x_step: xstep,
            y_step: ystep,
        };
        if self.view.detail_shift == 0 {
            draw_span(self.fb, flat, &params);
        } else {
            draw_span_low(self.fb, flat, &params);
        }
        self.spans += 1;
    }
}

/// `R_MakeSpans`, byte-exact port of the original state machine.
fn make_spans(
    raster: &mut PlaneRaster<'_>,
    spanstart: &mut [i32; SLOPE_LEN],
    x: i32,
    t1: u8,
    b1: u8,
    t2: u8,
    b2: u8,
) {
    let (mut t1, mut b1, mut t2, mut b2) =
        (i32::from(t1), i32::from(b1), i32::from(t2), i32::from(b2));

    while t1 < t2 && t1 <= b1 {
        raster.map_plane(t1, spanstart[t1 as usize], x - 1);
        t1 += 1;
    }
    while b1 > b2 && b1 >= t1 {
        raster.map_plane(b1, spanstart[b1 as usize], x - 1);
        b1 -= 1;
    }
    while t2 < t1 && t2 <= b2 {
        spanstart[t2 as usize] = x;
        t2 += 1;
    }
    while b2 > b1 && b2 >= t2 {
        spanstart[b2 as usize] = x;
        b2 -= 1;
    }
}

/// `R_DrawPlanes`: paint every flat visplane.
///
/// `planes` is the frame's [`PlaneSet`] — this call writes the `minx - 1`
/// and `maxx + 1` sentinels the scan fences on, so it takes it mutably.
/// `sky_picnum` is the namespace-relative number of the sky flat
/// (`FlatSet::num_for_name(b"F_SKY1\0\0\0\0\0\0")`); planes with that
/// `picnum` are counted and skipped here — they belong to the sky pass.
/// Pass `None` when the sky is not rendered by the caller.
///
/// This is the exact analogue of the original's end-of-frame sweep: call
/// it once, after the BSP/wall pass has marked all visplanes and before
/// the framebuffer leaves the renderer.
pub fn render_planes(
    fb: &mut IndexedFramebuffer,
    planes: &mut PlaneSet,
    view: &WallView<'_>,
    projection: &PlaneProjection,
    flats: &FlatSet,
    sky_picnum: Option<u32>,
) -> PlaneRenderReport {
    let mut report = PlaneRenderReport::default();

    if view.colormaps.is_empty() {
        report.errors.push(EngineError::Resource(
            "no COLORMAP colormaps loaded; nothing to shade spans with".into(),
        ));
        return report;
    }

    // R_ClearPlanes, the texture half: basexscale/baseyscale and the
    // zeroed row cache. (Clip arrays: the wall frame's clear handles it.)
    let fine = (view.view_angle - Angle::ANG90).fine_angle();
    let basexscale = FINE_COSINE[fine] / view.center_x_frac;
    let baseyscale = -FINE_SINE[fine] / view.center_x_frac;

    let mut raster = PlaneRaster {
        fb,
        view,
        projection,
        basexscale,
        baseyscale,
        cached_height: [Fixed::ZERO; SLOPE_LEN],
        cached_distance: [Fixed::ZERO; SLOPE_LEN],
        cached_xstep: [Fixed::ZERO; SLOPE_LEN],
        cached_ystep: [Fixed::ZERO; SLOPE_LEN],
        planeheight: Fixed::ZERO,
        light_row: 0,
        flat: None,
        spans: 0,
    };
    let mut spanstart = [0i32; SLOPE_LEN];

    for index in 0..planes.len() {
        let Some(plane) = planes.get(index) else {
            continue;
        };
        let (height, picnum, lightlevel, minx, maxx) = (
            plane.height,
            plane.picnum,
            plane.lightlevel,
            plane.minx,
            plane.maxx,
        );
        if minx > maxx {
            continue;
        }
        if sky_picnum == Some(picnum) {
            report.sky_planes_deferred += 1;
            continue;
        }
        let Some(flat) = flats.flat(picnum) else {
            report.errors.push(EngineError::Resource(format!(
                "flat picnum {picnum} is outside the flat namespace; plane skipped"
            )));
            continue;
        };

        raster.planeheight = (height - view.view_z).abs();
        raster.light_row = select_row(lightlevel, view.extralight);
        raster.flat = Some(flat);

        // fence the scan edges; top[minx - 1]/top[maxx + 1] are the
        // sentinel cells the scan reads at the boundaries
        if let Some(plane) = planes.get_mut(index) {
            plane.write_top_sentinel(maxx + 1);
            plane.write_top_sentinel(minx - 1);
        }

        let stop = maxx + 1;
        for x in minx..=stop {
            let Some(plane) = planes.get(index) else {
                break;
            };
            let (t1, b1, t2, b2) = (
                plane.top_at(x - 1),
                plane.bottom_at(x - 1),
                plane.top_at(x),
                plane.bottom_at(x),
            );
            make_spans(&mut raster, &mut spanstart, x, t1, b1, t2, b2);
        }
        raster.flat = None;
        report.planes_drawn += 1;
    }

    report.spans_drawn = raster.spans;
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bsp::ViewTables;
    use crate::fixed::FRACUNIT;
    use crate::gfx::palette::Colormap;
    use crate::render::lighting::NUMCOLORMAPS;
    use crate::render::wall::WallProjection;
    use crate::resources::ResourceManager;
    use crate::tables::FINE_COSINE;
    use crate::wad::WadFile;
    use crate::wad::testutil::build_wad;

    const VIEW_WIDTH: i32 = 64;
    const VIEW_HEIGHT: i32 = 40;

    // flat byte patterns, distinct enough to tell apart on sight
    fn floor1_pixel(i: usize) -> u8 {
        (i.wrapping_mul(41).wrapping_add(7)) as u8
    }
    fn ceil3_pixel(i: usize) -> u8 {
        !(i as u8)
    }
    const SKY_PIXEL: u8 = 0xEE;

    fn build_plane_wad() -> Vec<u8> {
        // PNAMES/TEXTURE1 exist so the wall texture set can be built; the
        // plane pass never samples them
        let mut pnames = Vec::new();
        pnames.extend_from_slice(&1i32.to_le_bytes());
        let mut name = [0u8; 8];
        name[..6].copy_from_slice(b"STRIPE");
        pnames.extend_from_slice(&name);

        // 64x64 fully covered patch; texel value == column index
        let stripe = {
            let mut lump = Vec::new();
            lump.extend_from_slice(&64i16.to_le_bytes());
            lump.extend_from_slice(&64i16.to_le_bytes());
            lump.extend_from_slice(&0i16.to_le_bytes());
            lump.extend_from_slice(&0i16.to_le_bytes());
            let column_ptrs_ofs = 8usize;
            let mut ptrs = Vec::with_capacity(64 * 4);
            let mut data = Vec::new();
            for c in 0..64i32 {
                ptrs.extend_from_slice(
                    &((column_ptrs_ofs + 64 * 4 + data.len()) as i32).to_le_bytes(),
                );
                data.extend_from_slice(&0u8.to_le_bytes()); // topdelta
                data.extend_from_slice(&64u8.to_le_bytes()); // length
                data.push(0);
                data.extend_from_slice(&[c as u8; 64]);
                data.push(0);
                data.push(0xff);
            }
            lump.extend_from_slice(&ptrs);
            lump.extend_from_slice(&data);
            lump
        };

        let mut texture1 = Vec::new();
        texture1.extend_from_slice(&1i32.to_le_bytes());
        let mut def = Vec::new();
        let mut tname = [0u8; 8];
        tname[..6].copy_from_slice(b"WALL_A");
        def.extend_from_slice(&tname);
        def.extend_from_slice(&0i32.to_le_bytes()); // masked
        def.extend_from_slice(&64i16.to_le_bytes());
        def.extend_from_slice(&64i16.to_le_bytes());
        def.extend_from_slice(&0i32.to_le_bytes()); // obsolete column directory
        def.extend_from_slice(&1i16.to_le_bytes()); // patch count
        def.extend_from_slice(&0i16.to_le_bytes()); // originx
        def.extend_from_slice(&0i16.to_le_bytes()); // originy
        def.extend_from_slice(&0i16.to_le_bytes()); // patch index
        def.extend_from_slice(&0i16.to_le_bytes()); // stepdir
        def.extend_from_slice(&0i16.to_le_bytes()); // colormap
        let def_off: i32 = 4 + 4;
        texture1.extend_from_slice(&def_off.to_le_bytes());
        texture1.extend_from_slice(&def);

        let floor1: Vec<u8> = (0..4096usize).map(floor1_pixel).collect();
        let ceil3: Vec<u8> = (0..4096usize).map(ceil3_pixel).collect();
        let sky = vec![SKY_PIXEL; 4096];

        build_wad(
            b"IWAD",
            &[
                ("PNAMES", &pnames),
                ("TEXTURE1", &texture1),
                ("STRIPE", &stripe),
                ("F_START", &[]),
                ("FLOOR1", &floor1),
                ("CEIL3", &ceil3),
                ("F_SKY1", &sky),
                ("F_END", &[]),
            ],
        )
    }

    fn test_manager() -> ResourceManager {
        ResourceManager::new(WadFile::from_bytes(build_plane_wad()).unwrap(), Vec::new())
    }

    /// Every colormap is the identity, all 33 diminishing levels.
    fn identity_colormaps() -> Vec<Colormap> {
        (0..NUMCOLORMAPS)
            .map(|_| Colormap(core::array::from_fn(|i| i as u8)))
            .collect()
    }

    /// Level `m` paints everything to `m` (distance lighting becomes a
    /// constant stamp, which makes the chosen level observable).
    fn level_colormaps() -> Vec<Colormap> {
        (0..NUMCOLORMAPS)
            .map(|m| Colormap([m as u8; 256]))
            .collect()
    }

    struct TestKit {
        tables: ViewTables,
        projection: WallProjection,
        light: crate::render::lighting::LightTables,
        textures: crate::render::textures::TextureSet,
        colormaps: Vec<Colormap>,
    }

    impl TestKit {
        fn new() -> Self {
            let tables = ViewTables::new(VIEW_WIDTH, Fixed::from_int(VIEW_WIDTH / 2));
            let projection = WallProjection::build(&tables, VIEW_WIDTH);
            Self {
                tables,
                projection,
                light: crate::render::lighting::LightTables::build(VIEW_WIDTH),
                textures: crate::render::textures::TextureSet::from_resources(&test_manager())
                    .unwrap(),
                colormaps: identity_colormaps(),
            }
        }
    }

    fn test_view<'a>(
        kit: &'a TestKit,
        view_angle: Angle,
        view_z: Fixed,
        extralight: i32,
        fixed_colormap: Option<usize>,
    ) -> WallView<'a> {
        WallView {
            tables: &kit.tables,
            projection: &kit.projection,
            light: &kit.light,
            textures: &kit.textures,
            colormaps: &kit.colormaps,
            view_x: Fixed::from_int(128),
            view_y: Fixed::from_int(64),
            view_z,
            view_angle,
            view_cos: FINE_COSINE[view_angle.fine_angle()],
            view_sin: crate::tables::FINE_SINE[view_angle.fine_angle()],
            view_width: VIEW_WIDTH,
            view_height: VIEW_HEIGHT,
            center_y: VIEW_HEIGHT / 2,
            center_x_frac: Fixed::from_int(VIEW_WIDTH / 2),
            center_y_frac: Fixed::from_int(VIEW_HEIGHT / 2),
            projection_distance: Fixed::from_int(VIEW_WIDTH / 2),
            extralight,
            fixed_colormap,
            detail_shift: 0,
            sky_flat: Some(*b"F_SKY1\0\0"),
        }
    }

    // ------------------------------------------------------------------
    // fixed-point oracles (plain i64 math, deliberately not via Fixed)
    // ------------------------------------------------------------------

    fn fmul(a: i64, b: i64) -> i64 {
        (a * b) >> 16
    }

    fn fdiv(a: i64, b: i64) -> i64 {
        if (a.abs() >> 14) >= b.abs() {
            return if (a ^ b) < 0 {
                i32::MIN as i64
            } else {
                i32::MAX as i64
            };
        }
        (a << 16) / b
    }

    fn flat_spot(x_frac: i64, y_frac: i64) -> usize {
        (((y_frac >> 10) & 4032) + ((x_frac >> 16) & 63)) as usize
    }

    /// The view constants the R_MapPlane chain needs, in one bundle so the
    /// oracle doesn't grow an argument per field.
    #[derive(Clone, Copy)]
    struct OracleView {
        angle: Angle,
        x: i64,
        y: i64,
        center_x_frac: i64,
        scaled_half: i64,
    }

    impl OracleView {
        /// High detail: 32-wide half-width slope numerator, 32<<16 center.
        const HD: Self = Self {
            angle: Angle::ANG90,
            x: 128 << 16,
            y: 64 << 16,
            center_x_frac: 32 << 16,
            scaled_half: 32 << 16,
        };
    }

    /// `flat_spot` at `(col, row)` of a flat run that opened at
    /// `open_col`. Re-derives the whole `R_ClearPlanes` + `R_MapPlane`
    /// chain in plain i64: the span samples from the opening column's
    /// ds_xfrac/ds_yfrac, stepping `ds_xstep`/`ds_ystep` per pixel.
    /// `xto` is the view's `xtoviewangle` table; `cfg` carries the rest.
    fn oracle_spot(
        xto: &WallProjection,
        cfg: OracleView,
        planeheight: i64,
        open_col: i32,
        col: i32,
        row: i32,
    ) -> usize {
        let OracleView {
            angle: view_angle,
            x: view_x,
            y: view_y,
            center_x_frac: center_x_frac_raw,
            scaled_half: scaled_half_raw,
        } = cfg;
        let center = VIEW_HEIGHT as i64 / 2;
        let dy = (((row as i64) - center) << 16) + 0x8000;
        let yslope = fdiv(scaled_half_raw, dy.abs());
        let distance = fmul(planeheight, yslope);

        // R_ClearPlanes: basexscale/baseyscale from (viewangle - ANG90)
        let clear_fine = (view_angle - Angle::ANG90).fine_angle();
        let basexscale = fdiv(FINE_COSINE[clear_fine].to_raw() as i64, center_x_frac_raw);
        let baseyscale = fdiv(
            -crate::tables::FINE_SINE[clear_fine].to_raw() as i64,
            center_x_frac_raw,
        );
        let xstep = fmul(distance, basexscale);
        let ystep = fmul(distance, baseyscale);

        // R_MapPlane at the opening column
        let angle_open = view_angle.to_raw().wrapping_add(xto.at(open_col).to_raw());
        let fi = (angle_open >> 19) as usize;
        let cosadj = FINE_COSINE[xto.at(open_col).fine_angle()].abs().to_raw() as i64;
        let dist_open = fdiv(FRACUNIT as i64, cosadj);
        let length = fmul(distance, dist_open);
        let mut x_frac = view_x + fmul(FINE_COSINE[fi].to_raw() as i64, length);
        let mut y_frac = -view_y - fmul(crate::tables::FINE_SINE[fi].to_raw() as i64, length);

        for _ in open_col..col {
            x_frac = x_frac.wrapping_add(xstep);
            y_frac = y_frac.wrapping_add(ystep);
        }
        flat_spot(x_frac, y_frac)
    }

    fn render_floor(
        kit: &TestKit,
        set: &mut PlaneSet,
        flats: &FlatSet,
        view_angle: Angle,
        view_z: Fixed,
        fixed_colormap: Option<usize>,
    ) -> (IndexedFramebuffer, PlaneRenderReport) {
        let view = test_view(kit, view_angle, view_z, 0, fixed_colormap);
        let projection = PlaneProjection::build(&view);
        let mut fb = IndexedFramebuffer::new();
        let report = render_planes(&mut fb, set, &view, &projection, flats, Some(2));
        (fb, report)
    }

    fn floor_flat_bytes() -> Vec<u8> {
        (0..4096usize).map(floor1_pixel).collect()
    }

    fn ceil3_flat_bytes() -> Vec<u8> {
        (0..4096usize).map(ceil3_pixel).collect()
    }

    // ------------------------------------------------------------------
    // PlaneProjection (yslope[] / distscale[])
    // ------------------------------------------------------------------

    #[test]
    fn yslope_boundary_rows_match_formula() {
        let kit = TestKit::new();
        let view = test_view(&kit, Angle::ZERO, Fixed::from_int(100), 0, None);
        let proj = PlaneProjection::build(&view);

        // the very center row: dy == 0.5, yslope == 64.0 exactly
        assert_eq!(proj.yslope(VIEW_HEIGHT / 2), Fixed::from_int(VIEW_WIDTH));
        // one row up: symmetric 0.5 offset, same value
        assert_eq!(
            proj.yslope(VIEW_HEIGHT / 2 - 1),
            Fixed::from_int(VIEW_WIDTH)
        );

        // top and bottom rows, against the raw formula in plain i64
        let scaled_half = ((VIEW_WIDTH as i64) / 2) << 16;
        let top_dy = (((0 - VIEW_HEIGHT as i64 / 2) << 16) + 0x8000).abs();
        let top = ((scaled_half << 16) / top_dy) as i32;
        assert_eq!(proj.yslope(0).to_raw(), top);

        let bot_dy = (((VIEW_HEIGHT as i64 - 1 - VIEW_HEIGHT as i64 / 2) << 16) + 0x8000).abs();
        let bot = ((scaled_half << 16) / bot_dy) as i32;
        assert_eq!(proj.yslope(VIEW_HEIGHT - 1).to_raw(), bot);

        // sane and finite everywhere, including rows outside the view
        for row in 0..256 {
            assert!(proj.yslope[row] > Fixed::ZERO, "row {row}");
        }
    }

    #[test]
    fn yslope_increases_toward_center() {
        let kit = TestKit::new();
        let view = test_view(&kit, Angle::ZERO, Fixed::from_int(100), 0, None);
        let proj = PlaneProjection::build(&view);
        // |dy| grows away from the center row, so yslope shrinks there
        assert!(proj.yslope(0) < proj.yslope(10));
        assert!(proj.yslope(10) < proj.yslope(19));
        assert!(proj.yslope(39) < proj.yslope(30));
        assert!(proj.yslope(30) < proj.yslope(20));
    }

    #[test]
    fn odd_view_height_produces_no_zero_slope() {
        let tables = ViewTables::new(32, Fixed::from_int(16));
        let odd_projection = WallProjection::build(&tables, 32);
        let kit = TestKit::new();
        let base = test_view(&kit, Angle::ZERO, Fixed::from_int(100), 0, None);
        let view = WallView {
            view_height: 41,
            center_y: 20,
            center_y_frac: Fixed::from_int(20),
            view_width: 32,
            center_x_frac: Fixed::from_int(16),
            projection_distance: Fixed::from_int(16),
            projection: &odd_projection,
            ..base
        };
        let proj = PlaneProjection::build(&view);
        // 41 / 2 == 20, so row 20 lands exactly on the zero offset (+0.5)
        assert!(proj.yslope(20) > Fixed::ZERO);
        for row in 0..256 {
            assert!(proj.yslope[row] > Fixed::ZERO, "row {row}");
        }
    }

    #[test]
    fn distscale_center_column_is_unit() {
        let kit = TestKit::new();
        let view = test_view(&kit, Angle::ZERO, Fixed::from_int(100), 0, None);
        let proj = PlaneProjection::build(&view);
        // center column: angle 0 -> cos == FRACUNIT -> distscale == 1 (within 1 ulp)
        let unit = proj.distscale(VIEW_WIDTH / 2);
        assert!((unit.to_raw() - FRACUNIT).unsigned_abs() <= 1);
        // edges see a larger factor than the center (the FOV edge angle is
        // clipangle, i.e. ~45 degrees for a 90-degree FOV)
        assert!(proj.distscale(0) > Fixed::ONE);
        assert!(proj.distscale(VIEW_WIDTH - 1) > Fixed::ONE);
        assert!(proj.distscale(0) > proj.distscale(VIEW_WIDTH / 2));
    }

    #[test]
    fn distscale_matches_table_ratio() {
        let kit = TestKit::new();
        let view = test_view(&kit, Angle::ZERO, Fixed::from_int(100), 0, None);
        let proj = PlaneProjection::build(&view);
        for col in [0, 1, 17, 32, 47, 63] {
            let fine = kit.projection.at(col).fine_angle();
            let cosadj = crate::tables::FINE_COSINE[fine].abs();
            assert_eq!(proj.distscale(col), Fixed::ONE / cosadj, "col {col}");
        }
    }

    // ------------------------------------------------------------------
    // FlatSet
    // ------------------------------------------------------------------

    #[test]
    fn flat_set_resolves_namespace_indices() {
        let flats = FlatSet::from_resources(&test_manager()).unwrap();
        assert_eq!(flats.len(), 3);
        assert!(!flats.is_empty());
        // FLOOR1=0, CEIL3=1, F_SKY1=2 in namespace order
        assert_eq!(flats.flat(0).unwrap().pixels()[..], floor_flat_bytes()[..]);
        assert_eq!(flats.flat(1).unwrap().pixels()[..], ceil3_flat_bytes()[..]);
        assert_eq!(flats.flat(3), None);
        // name lookup, case insensitive, namespace-relative
        assert_eq!(flats.num_for_name(b"FLOOR1\0\0"), Some(0));
        assert_eq!(flats.num_for_name(b"ceil3\0\0\0"), Some(1));
        assert_eq!(flats.num_for_name(b"F_SKY1\0\0"), Some(2));
        assert_eq!(flats.num_for_name(b"NOSUCH\0\0"), None);
    }

    #[test]
    fn flat_set_requires_namespace_markers() {
        let wad = build_wad(b"IWAD", &[("FLOOR1", &[0u8; 4096])]);
        let manager = ResourceManager::new(WadFile::from_bytes(wad).unwrap(), Vec::new());
        assert!(FlatSet::from_resources(&manager).is_err());
    }

    #[test]
    fn flat_set_rejects_malformed_flat() {
        let wad = build_wad(
            b"IWAD",
            &[("F_START", &[]), ("BROKEN", &[0u8; 100]), ("F_END", &[])],
        );
        let manager = ResourceManager::new(WadFile::from_bytes(wad).unwrap(), Vec::new());
        assert!(FlatSet::from_resources(&manager).is_err());
    }

    // ------------------------------------------------------------------
    // R_DrawPlanes end to end
    // ------------------------------------------------------------------

    const FLOOR1_PIC: u32 = 0;
    const CEIL3_PIC: u32 = 1;
    const SKY_PIC: u32 = 2;

    /// Marks the floor/ceiling band for `cols` on a fresh visplane. Mirrors
    /// the BSP flow: `R_FindPlane` -> `R_CheckPlane` (which sets the
    /// minx/maxx range) -> per-column `mark`.
    fn mark_flat(
        set: &mut PlaneSet,
        height: Fixed,
        picnum: u32,
        lightlevel: i32,
        cols: std::ops::RangeInclusive<i32>,
        top: i32,
        bottom: i32,
    ) -> usize {
        let start = *cols.start();
        let stop = *cols.end();
        let plane = set.find_plane(height, picnum, lightlevel, false).unwrap();
        // if this exact band was already claimed for another column range,
        // R_CheckPlane splits; tests never hit it, but keep the flow honest
        let plane = set.check_plane(plane, start, stop).unwrap();
        for x in cols {
            set.get_mut(plane).unwrap().mark(x, top, bottom);
        }
        plane
    }

    fn mark_floor(
        set: &mut PlaneSet,
        picnum: u32,
        cols: std::ops::RangeInclusive<i32>,
        top: i32,
        bottom: i32,
    ) -> usize {
        mark_flat(set, Fixed::ZERO, picnum, 192, cols, top, bottom)
    }

    fn mark_ceiling(
        set: &mut PlaneSet,
        picnum: u32,
        cols: std::ops::RangeInclusive<i32>,
        top: i32,
        bottom: i32,
    ) -> usize {
        mark_flat(set, Fixed::from_int(128), picnum, 128, cols, top, bottom)
    }

    #[test]
    fn flat_span_pixels_match_oracle() {
        let kit = TestKit::new();
        let flats = FlatSet::from_resources(&test_manager()).unwrap();
        let mut set = PlaneSet::new();
        mark_floor(&mut set, FLOOR1_PIC, 5..=10, 25, 35);
        let (fb, report) = render_floor(
            &kit,
            &mut set,
            &flats,
            Angle::ANG90,
            Fixed::from_int(100),
            None,
        );

        assert_eq!(report.planes_drawn, 1);
        assert_eq!(report.spans_drawn, 11); // rows 25..=35, one span per row
        assert_eq!(report.sky_planes_deferred, 0);
        assert!(report.errors.is_empty());

        let floor = floor_flat_bytes();
        for row in 25..=35 {
            for col in 5..=10 {
                let spot = oracle_spot(&kit.projection, OracleView::HD, 100 << 16, 5, col, row);
                assert_eq!(
                    fb.get(col as usize, row as usize),
                    Some(floor[spot]),
                    "col {col} row {row}"
                );
            }
        }
        // outside the marked band the framebuffer stays black
        for col in 5..=10 {
            assert_eq!(fb.get(col as usize, 24), Some(0));
            assert_eq!(fb.get(col as usize, 36), Some(0));
        }
        for row in 25..=35 {
            assert_eq!(fb.get(4, row as usize), Some(0));
            assert_eq!(fb.get(11, row as usize), Some(0));
        }
    }

    #[test]
    fn flat_span_pixels_match_oracle_at_ang45() {
        let kit = TestKit::new();
        let flats = FlatSet::from_resources(&test_manager()).unwrap();
        let mut set = PlaneSet::new();
        mark_floor(&mut set, FLOOR1_PIC, 5..=10, 25, 35);
        let angle = Angle::ANG45;
        let (fb, report) = render_floor(&kit, &mut set, &flats, angle, Fixed::from_int(100), None);
        assert_eq!(report.spans_drawn, 11);
        let floor = floor_flat_bytes();
        for row in [25, 30, 35] {
            for col in 5..=10 {
                let spot = oracle_spot(
                    &kit.projection,
                    OracleView {
                        angle,
                        ..OracleView::HD
                    },
                    100 << 16,
                    5,
                    col,
                    row,
                );
                assert_eq!(
                    fb.get(col as usize, row as usize),
                    Some(floor[spot]),
                    "col {col} row {row}"
                );
            }
        }
    }

    #[test]
    fn flat_span_pixels_match_oracle_at_zero_angle() {
        let kit = TestKit::new();
        let flats = FlatSet::from_resources(&test_manager()).unwrap();
        let mut set = PlaneSet::new();
        mark_floor(&mut set, FLOOR1_PIC, 5..=10, 25, 35);
        // looking straight east; the view position is off-origin so the
        // ds_yfrac negation actually matters
        let (fb, report) = render_floor(
            &kit,
            &mut set,
            &flats,
            Angle::ZERO,
            Fixed::from_int(100),
            None,
        );
        assert_eq!(report.spans_drawn, 11);
        let floor = floor_flat_bytes();
        for row in [25, 30, 35] {
            for col in 5..=10 {
                let spot = oracle_spot(
                    &kit.projection,
                    OracleView {
                        angle: Angle::ZERO,
                        ..OracleView::HD
                    },
                    100 << 16,
                    5,
                    col,
                    row,
                );
                assert_eq!(
                    fb.get(col as usize, row as usize),
                    Some(floor[spot]),
                    "col {col} row {row}"
                );
            }
        }
    }

    #[test]
    fn ceiling_plane_above_view_paints_with_its_flat() {
        let kit = TestKit::new();
        let flats = FlatSet::from_resources(&test_manager()).unwrap();
        let mut set = PlaneSet::new();
        // plane at z = 128, view at z = 100 -> planeheight == 28
        mark_ceiling(&mut set, CEIL3_PIC, 5..=10, 5, 12);
        let (fb, report) = render_floor(
            &kit,
            &mut set,
            &flats,
            Angle::ANG90,
            Fixed::from_int(100),
            None,
        );
        assert_eq!(report.planes_drawn, 1);
        assert_eq!(report.spans_drawn, 8);

        let ceil = ceil3_flat_bytes();
        for row in 5..=12 {
            for col in 5..=10 {
                let spot = oracle_spot(&kit.projection, OracleView::HD, 28 << 16, 5, col, row);
                assert_eq!(
                    fb.get(col as usize, row as usize),
                    Some(ceil[spot]),
                    "col {col} row {row}"
                );
            }
        }
        assert_eq!(fb.get(5, 13), Some(0));
        assert_eq!(fb.get(5, 4), Some(0));
    }

    #[test]
    fn zlight_row_and_distance_index_pick_the_colormap() {
        let mut kit = TestKit::new();
        kit.colormaps = level_colormaps();
        let flats = FlatSet::from_resources(&test_manager()).unwrap();
        let mut set = PlaneSet::new();
        mark_floor(&mut set, FLOOR1_PIC, 5..=10, 25, 35); // lightlevel 192 -> row 12

        let (fb, report) = render_floor(
            &kit,
            &mut set,
            &flats,
            Angle::ANG90,
            Fixed::from_int(100),
            None,
        );
        assert!(report.errors.is_empty());

        let center = VIEW_HEIGHT as i64 / 2;
        for row in 25..=35 {
            let dy = (((row as i64) - center) << 16) + 0x8000;
            let yslope = fdiv(((VIEW_WIDTH as i64) / 2) << 16, dy.abs());
            let distance = fmul(100 << 16, yslope);
            let z =
                ((distance >> 20).min((crate::render::lighting::MAXLIGHTZ - 1) as i64)) as usize;
            let level = kit.light.zlight(12, z);
            for col in 5..=10 {
                // distance does not depend on the column, so the whole row shares the level
                assert_eq!(
                    fb.get(col as usize, row as usize),
                    Some(level as u8),
                    "col {col} row {row}"
                );
            }
        }
    }

    #[test]
    fn fixed_colormap_overrides_zlight() {
        let mut kit = TestKit::new();
        kit.colormaps = level_colormaps();
        let flats = FlatSet::from_resources(&test_manager()).unwrap();
        let mut set = PlaneSet::new();
        mark_floor(&mut set, FLOOR1_PIC, 5..=10, 25, 35);
        let (fb, report) = render_floor(
            &kit,
            &mut set,
            &flats,
            Angle::ANG90,
            Fixed::from_int(100),
            Some(9),
        );
        assert!(report.errors.is_empty());
        for row in 25..=35 {
            for col in 5..=10 {
                assert_eq!(fb.get(col as usize, row as usize), Some(9));
            }
        }
    }

    #[test]
    fn planeheight_zero_marks_every_pixel_at_the_view_spot() {
        let kit = TestKit::new();
        let flats = FlatSet::from_resources(&test_manager()).unwrap();
        let mut set = PlaneSet::new();
        // height == view_z: planeheight == 0, ds_xstep/ds_ystep/distance == 0
        let plane = set
            .find_plane(Fixed::from_int(100), FLOOR1_PIC, 192, false)
            .unwrap();
        let plane = set.check_plane(plane, 5, 10).unwrap();
        for x in 5..=10 {
            set.get_mut(plane).unwrap().mark(x, 30, 32);
        }
        let (fb, report) = render_floor(
            &kit,
            &mut set,
            &flats,
            Angle::ANG90,
            Fixed::from_int(100),
            None,
        );
        assert_eq!(report.spans_drawn, 3);

        // vanilla's zero-initialized row cache makes distance == 0 the very
        // first time too, so every pixel lands on the view spot
        let spot = flat_spot(128 << 16, -64 << 16);
        let expected = floor_flat_bytes()[spot];
        for row in 30..=32 {
            for col in 5..=10 {
                assert_eq!(fb.get(col as usize, row as usize), Some(expected));
            }
        }
    }

    #[test]
    fn disjoint_column_regions_paint_independently() {
        let kit = TestKit::new();
        let flats = FlatSet::from_resources(&test_manager()).unwrap();
        let mut set = PlaneSet::new();
        mark_floor(&mut set, FLOOR1_PIC, 5..=7, 25, 27);
        mark_floor(&mut set, FLOOR1_PIC, 12..=14, 25, 27); // same plane, unmarked gap
        let (fb, report) = render_floor(
            &kit,
            &mut set,
            &flats,
            Angle::ANG90,
            Fixed::from_int(100),
            None,
        );
        assert_eq!(report.planes_drawn, 1);
        assert_eq!(report.spans_drawn, 6); // 3 rows x 2 runs

        for row in 25..=27 {
            for col in 8..=11 {
                assert_eq!(fb.get(col as usize, row as usize), Some(0), "gap col {col}");
            }
        }
    }

    #[test]
    fn two_planes_same_columns_keep_their_flats() {
        let kit = TestKit::new();
        let flats = FlatSet::from_resources(&test_manager()).unwrap();
        let mut set = PlaneSet::new();
        mark_floor(&mut set, FLOOR1_PIC, 5..=10, 25, 35); // 11 rows
        mark_ceiling(&mut set, CEIL3_PIC, 5..=10, 5, 9); // 5 rows
        let (fb, report) = render_floor(
            &kit,
            &mut set,
            &flats,
            Angle::ANG90,
            Fixed::from_int(100),
            None,
        );
        assert_eq!(report.planes_drawn, 2);
        assert_eq!(report.spans_drawn, 16);

        let floor = floor_flat_bytes();
        let ceil = ceil3_flat_bytes();
        for row in 25..=35 {
            let spot = oracle_spot(&kit.projection, OracleView::HD, 100 << 16, 5, 5, row);
            assert_eq!(fb.get(5, row as usize), Some(floor[spot]));
        }
        for row in 5..=9 {
            let spot = oracle_spot(&kit.projection, OracleView::HD, 28 << 16, 5, 5, row);
            assert_eq!(fb.get(5, row as usize), Some(ceil[spot]));
        }
        for row in [10, 20, 24, 36] {
            assert_eq!(fb.get(5, row as usize), Some(0));
        }
    }

    #[test]
    fn sky_planes_are_deferred_untouched() {
        let kit = TestKit::new();
        let flats = FlatSet::from_resources(&test_manager()).unwrap();
        let mut set = PlaneSet::new();
        // one sky ceiling (picnum == F_SKY1's namespace index, height/light collapsed)
        let sky = set
            .find_plane(Fixed::from_int(384), SKY_PIC, 200, true)
            .unwrap();
        let sky = set.check_plane(sky, 0, 7).unwrap();
        for x in 0..8 {
            set.get_mut(sky).unwrap().mark(x, 0, 10);
        }
        mark_floor(&mut set, FLOOR1_PIC, 5..=10, 25, 35);

        let view = test_view(&kit, Angle::ANG90, Fixed::from_int(100), 0, None);
        let projection = PlaneProjection::build(&view);
        let mut fb = IndexedFramebuffer::new();
        let report = render_planes(&mut fb, &mut set, &view, &projection, &flats, Some(SKY_PIC));

        assert_eq!(report.sky_planes_deferred, 1);
        assert_eq!(report.planes_drawn, 1);
        // the sky region stays untouched for the sky pass to claim
        for row in 0..=10 {
            for col in 0..8 {
                assert_eq!(fb.get(col, row as usize), Some(0));
            }
        }
        assert_eq!(report.spans_drawn, 11);
    }

    #[test]
    fn unknown_flat_picnum_is_an_error_not_a_crash() {
        let kit = TestKit::new();
        let flats = FlatSet::from_resources(&test_manager()).unwrap();
        let mut set = PlaneSet::new();
        mark_floor(&mut set, 999, 5..=10, 25, 35);
        let (fb, report) = render_floor(
            &kit,
            &mut set,
            &flats,
            Angle::ANG90,
            Fixed::from_int(100),
            None,
        );
        assert_eq!(report.planes_drawn, 0);
        assert_eq!(report.spans_drawn, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(fb.as_slice().iter().all(|&b| b == 0));
    }

    #[test]
    fn missing_colormaps_abort_before_painting() {
        let mut kit = TestKit::new();
        kit.colormaps = Vec::new();
        let flats = FlatSet::from_resources(&test_manager()).unwrap();
        let mut set = PlaneSet::new();
        mark_floor(&mut set, FLOOR1_PIC, 5..=10, 25, 35);
        let (fb, report) = render_floor(
            &kit,
            &mut set,
            &flats,
            Angle::ANG90,
            Fixed::from_int(100),
            None,
        );
        assert_eq!(report.planes_drawn, 0);
        assert_eq!(report.spans_drawn, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(fb.as_slice().iter().all(|&b| b == 0));
    }

    #[test]
    fn untouched_plane_is_skipped_silently() {
        let kit = TestKit::new();
        let flats = FlatSet::from_resources(&test_manager()).unwrap();
        let mut set = PlaneSet::new();
        // find but never mark a plane: minx > maxx -> the scan skips it
        let _ = set.find_plane(Fixed::ZERO, FLOOR1_PIC, 192, false);
        let (fb, report) = render_floor(
            &kit,
            &mut set,
            &flats,
            Angle::ANG90,
            Fixed::from_int(100),
            None,
        );
        assert_eq!(report.planes_drawn, 0);
        assert!(report.errors.is_empty());
        assert!(fb.as_slice().iter().all(|&b| b == 0));
    }

    #[test]
    fn scan_fences_edges_with_sentinels() {
        let kit = TestKit::new();
        let flats = FlatSet::from_resources(&test_manager()).unwrap();
        let mut set = PlaneSet::new();
        let plane = mark_floor(&mut set, FLOOR1_PIC, 5..=10, 25, 35);
        let _ = render_floor(
            &kit,
            &mut set,
            &flats,
            Angle::ANG90,
            Fixed::from_int(100),
            None,
        );
        let plane = set.get(plane).unwrap();
        assert_eq!(plane.top_at(4), 0xff);
        assert_eq!(plane.top_at(11), 0xff);
    }

    // low detail halves the logical view width (vanilla viewwidth =
    // scaledviewwidth >> detailshift) and the drawer doubles every pixel;
    // the view geometry follows the same rule, 32 logical columns on the
    // 64-pixel buffer
    #[test]
    fn low_detail_doubles_pixels_like_vanilla() {
        let tables = ViewTables::new(32, Fixed::from_int(16));
        let xto = WallProjection::build(&tables, 32);
        let base_kit = TestKit::new();
        let flats = FlatSet::from_resources(&test_manager()).unwrap();
        let mut set = PlaneSet::new();
        mark_floor(&mut set, FLOOR1_PIC, 5..=10, 30, 30); // one row only

        let view = WallView {
            tables: &tables,
            projection: &xto,
            view_width: 32,
            center_x_frac: Fixed::from_int(16),
            projection_distance: Fixed::from_int(16),
            detail_shift: 1,
            ..test_view(&base_kit, Angle::ANG90, Fixed::from_int(100), 0, None)
        };
        let plane_projection = PlaneProjection::build(&view);
        let mut fb = IndexedFramebuffer::new();
        let report = render_planes(
            &mut fb,
            &mut set,
            &view,
            &plane_projection,
            &flats,
            Some(SKY_PIC),
        );
        assert_eq!(report.spans_drawn, 1);
        // vanilla R_DrawSpanLow doubles the span bounds (ds_x1 <<= 1) and the
        // do..while paints count + 1 double pixels, so the marked run 5..=10
        // covers double pixels 10..31, not 10..21
        for pair in [
            [10, 11],
            [12, 13],
            [14, 15],
            [16, 17],
            [18, 19],
            [20, 21],
            [22, 23],
            [24, 25],
            [26, 27],
            [28, 29],
            [30, 31],
        ] {
            assert_eq!(fb.get(pair[0], 30), fb.get(pair[1], 30), "pair {pair:?}");
        }
        // one texel sample per double pixel, starting at the run's opening
        // column: pair k holds the same spot as the k-th step from there
        let low = OracleView {
            angle: Angle::ANG90,
            x: 128 << 16,
            y: 64 << 16,
            center_x_frac: 16 << 16,
            scaled_half: 32 << 16,
        };
        let spot5 = oracle_spot(&xto, low, 100 << 16, 5, 5, 30);
        assert_eq!(fb.get(10, 30), Some(floor_flat_bytes()[spot5]));
        let spot10 = oracle_spot(&xto, low, 100 << 16, 5, 10, 30);
        assert_eq!(fb.get(20, 30), Some(floor_flat_bytes()[spot10]));
        // untouched columns stay black
        assert_eq!(fb.get(9, 30), Some(0));
        assert_eq!(fb.get(32, 30), Some(0));
        assert_eq!(fb.get(0, 30), Some(0));
    }
}
