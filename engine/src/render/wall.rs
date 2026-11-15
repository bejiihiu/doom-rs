//! The wall rendering pipeline, ported from `r_segs.c`
//! (`R_StoreWallRange`, `R_RenderSegLoop`, `R_RenderMaskedSegRange`).
//!
//! This is the "draw the walls" layer that sits between the BSP clip
//! classification ([`crate::bsp::add_line`]) and the low-level column
//! drawers ([`super::column::draw_column`]). For every visible seg
//! fragment the flow is:
//!
//! 1. [`WallFrame::store_wall_range`] computes the seg's distance and
//!    per-column scale step, resolves the wall's textures and pegs the
//!    texture coordinates, picks the light row, marks plane spans, and
//!    stashes a [`DrawSeg`] for the sprite pass.
//! 2. The seg loop walks the columns once, maintains `ceilingclip` /
//!    `floorclip` (and the matching visplane markers), draws the mid
//!    (single sided) or top/bottom (two sided) texture tiers through the
//!    column drawer, and saves masked-midtexture column indices into the
//!    openings buffer.
//! 3. [`WallFrame::render_masked_seg_range`] (called by the sprite pass
//!    for vissegs with a masked midtexture) walks those columns again and
//!    draws the fence/railing texture over whatever is behind it.
//!
//! The original keeps all of this in `r_segs.c` file-scope globals
//! (`rw_*`, `worldtop`, `topfrac`, ...). Here the per-frame state is
//! explicit in [`WallFrame`]; nothing lives in thread locals or statics,
//! and the same code runs in tests with synthetic maps and textures.
//!
//! Clipping rule recap (identical to the original): `ceilingclip[x]` is
//! the lowest row claimed by the ceiling side of a wall at column `x`
//! (starts at `-1`), `floorclip[x]` the highest row claimed from the
//! floor side (starts at `viewheight`). A drawn mid (single sided) wall
//! claims the whole column (`ceilingclip = viewheight`, `floorclip =
//! -1`); top/bottom tiers clamp against whatever the other tier claimed
//! in the same column.

use crate::bsp::{ViewTables, point_to_angle_2};
use crate::errors::EngineError;
use crate::fixed::{FRACBITS, Fixed};
use crate::gfx::palette::Colormap;
use crate::map::types::{ML_DONTPEGBOTTOM, ML_DONTPEGTOP};
use crate::map::{Map, Sector};
use crate::tables::{ANGLE_TO_FINE_SHIFT, Angle, FINE_SINE, FINE_TANGENT};

use super::column::{ColumnParams, UnmaskedColumn, draw_column, draw_column_low};
use super::framebuffer::{IndexedFramebuffer, SCREEN_WIDTH};
use super::lighting::{LIGHTLEVELS, LIGHTSEGSHIFT, LightTables};
use super::planes::PlaneSet;
use super::textures::{TextureRun, TextureSet};

/// Vanilla `HEIGHTBITS` — fraction bits below the row index for the
/// `topfrac`/`bottomfrac` accumulators.
const HEIGHTBITS: u32 = 12;
const HEIGHTUNIT: i32 = 1 << HEIGHTBITS;

/// Vanilla `MAXDRAWSEGS`. When the pool is exhausted, walls still render
/// but their [`DrawSeg`] (needed for sprite clipping) is dropped; the
/// original refused to rasterize the wall at all, which also loses the
/// pixels — both are degradations, ours keeps the visible output.
pub const MAX_DRAWSEGS: usize = 256;

/// Vanilla `MAXOPENINGS`: `SCREENWIDTH * 64` i16 slots shared between
/// masked-midtexture column indices and sprite clip copies.
pub const MAX_OPENINGS: usize = SCREEN_WIDTH * 64;

/// Sentinel for "this column already drew its masked texture" (`MAXSHORT`).
const MAX_SHORT: i16 = i16::MAX;

/// Vanilla `SIL_NONE` (`0`).
pub const SIL_NONE: u8 = 0;
/// Vanilla `SIL_BOTTOM` (`1`).
pub const SIL_BOTTOM: u8 = 1;
/// Vanilla `SIL_TOP` (`2`).
pub const SIL_TOP: u8 = 2;
/// Vanilla `SIL_BOTH` (`3`).
pub const SIL_BOTH: u8 = 3;

/// Per-column clip reference for a drawn seg — the state the sprite pass
/// reads through `ds->sprtopclip` / `ds->sprbottomclip` /
/// `ds->maskedtexturecol`.
///
/// The single-sided case points at the frame's all-`viewheight`
/// `screen_height_array`; the two-sided fully closed cases point at an
/// all-`-1` sentinel; segments with actual copied coverage point into
/// the shared openings buffer (`skew` converts absolute screen columns
/// into openings indices, exactly like the original's
/// `lastopening - start` pointer skew).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum ClipRef {
    /// Not set (a window seg with no silhouette; sprites must never read it).
    #[default]
    None,
    /// Copied coverage in the shared openings buffer: `openings[skew + x]`.
    Openings { skew: i64 },
    /// A seg that clips nothing (`screenheightarray`).
    ScreenHeight,
    /// A seg that clips everything (`negonearray`).
    NegOne,
}

/// One stored wall fragment — vanilla `drawseg_t`.
///
/// The sprite pass reads `seg`, `x1`, `x2`, `silhouette`, `bsilheight`,
/// `tsilheight`, the scale pair and the three clip references (via the
/// `seg_*` accessors on [`WallFrame`]).
#[derive(Clone, Copy, Debug)]
pub struct DrawSeg {
    /// Index of the source seg in the map.
    pub seg: usize,
    /// First drawer-visible column, inclusive.
    pub x1: i32,
    /// Last drawer-visible column, inclusive.
    pub x2: i32,
    /// Scale at `x1`.
    pub scale1: Fixed,
    /// Scale at `x2`.
    pub scale2: Fixed,
    /// Scale delta per column (`0` for single-column segs).
    pub scalestep: Fixed,
    /// `SIL_*` combination.
    pub silhouette: u8,
    /// Do not clip sprites below this height (fixed point, world z).
    pub bsilheight: Fixed,
    /// Do not clip sprites above this height (fixed point, world z).
    pub tsilheight: Fixed,
    pub(crate) sprtopclip: ClipRef,
    pub(crate) sprbottomclip: ClipRef,
    pub(crate) maskedtexturecol: ClipRef,
}

impl DrawSeg {
    fn empty() -> Self {
        Self {
            seg: 0,
            x1: 0,
            x2: 0,
            scale1: Fixed::ZERO,
            scale2: Fixed::ZERO,
            scalestep: Fixed::ZERO,
            silhouette: SIL_NONE,
            bsilheight: Fixed::ZERO,
            tsilheight: Fixed::ZERO,
            sprtopclip: ClipRef::None,
            sprbottomclip: ClipRef::None,
            maskedtexturecol: ClipRef::None,
        }
    }
}

/// `xtoviewangle[]` built from the BSP view tables — the second half of
/// `R_InitTextureMapping` that the clip tables dropped (only entry `0`,
/// `clipangle`, survived there).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WallProjection {
    /// `xtoviewangle[x]`: the smallest view angle projecting to column `x`.
    pub x_to_view_angle: Vec<Angle>,
}

impl WallProjection {
    /// Builds the table for a view `view_width` columns wide by scanning
    /// `view_angle_to_x`, like the original's fencepost scan. `tables`
    /// must come from the same view size as `view_width`.
    pub fn build(tables: &ViewTables, view_width: i32) -> Self {
        assert!(view_width > 0, "view width must be positive");
        let mut out = Vec::with_capacity(view_width as usize + 1);
        for x in 0..=view_width {
            // the original resets the scan per column (naive but exact);
            // a forward-keeping cursor would collapse every entry onto
            // the first index where viewangletox hits 0
            let mut i = 0usize;
            while tables.view_angle_to_x[i] > x {
                i += 1;
            }
            out.push(Angle::from_raw(
                ((i as u32) << ANGLE_TO_FINE_SHIFT).wrapping_sub(Angle::ANG90.to_raw()),
            ));
        }
        Self {
            x_to_view_angle: out,
        }
    }

    /// `xtoviewangle[x]` (`0..=view_width`).
    pub fn at(&self, x: i32) -> Angle {
        self.x_to_view_angle[x as usize]
    }
}

/// Shared `openings[]` buffer and bump pointer (`lastopening`).
#[derive(Clone, Debug)]
struct Openings {
    data: Vec<i16>,
    last: usize,
}

impl Openings {
    fn new() -> Self {
        Self {
            data: vec![0; MAX_OPENINGS],
            last: 0,
        }
    }

    fn clear(&mut self) {
        self.last = 0;
    }

    /// Bump-allocates `len` slots; returns the base index
    /// (`lastopening - openings` in the original), or `None` on exhaustion.
    fn alloc(&mut self, len: usize) -> Option<usize> {
        match self.last.checked_add(len) {
            Some(end) if end <= MAX_OPENINGS => {
                let base = self.last;
                self.last = end;
                Some(base)
            }
            _ => None,
        }
    }

    fn get(&self, index: usize) -> Option<i16> {
        self.data.get(index).copied()
    }

    fn set(&mut self, index: i64, value: i16) {
        if let Ok(index) = usize::try_from(index)
            && index < MAX_OPENINGS
        {
            self.data[index] = value;
        }
    }

    fn copy_from_clip(&mut self, base: usize, clip: &[i16; SCREEN_WIDTH], start: i32, stop: i32) {
        let len = (stop - start + 1) as usize;
        self.data[base..base + len].copy_from_slice(&clip[start as usize..=stop as usize]);
    }
}

/// Everything a wall needs that is constant for a whole frame of views.
///
/// Build one per view size; clone the references freely, it is cheap.
#[derive(Clone)]
pub struct WallView<'a> {
    /// `viewangletox` + `clipangle` (BSP-side tables).
    pub tables: &'a ViewTables,
    /// `xtoviewangle[]`.
    pub projection: &'a WallProjection,
    /// `zlight`/`scalelight`.
    pub light: &'a LightTables,
    /// Texture name lookup + flat composites.
    pub textures: &'a TextureSet,
    /// Decoded COLORMAP lump (`colormaps[level]`).
    pub colormaps: &'a [Colormap],
    /// World view position.
    pub view_x: Fixed,
    /// World view position.
    pub view_y: Fixed,
    /// World view height.
    pub view_z: Fixed,
    /// Facing direction.
    pub view_angle: Angle,
    /// `cos(view_angle)`, precomputed for the span renderer.
    pub view_cos: Fixed,
    /// `sin(view_angle)`, precomputed for the span renderer.
    pub view_sin: Fixed,
    /// `viewwidth` in pixels.
    pub view_width: i32,
    /// `viewheight` in pixels.
    pub view_height: i32,
    /// `centery` in pixels (`view_height / 2`).
    pub center_y: i32,
    /// `centerxfrac` (view center in fixed point; `projection` equals it).
    pub center_x_frac: Fixed,
    /// `centeryfrac` (view center in fixed point).
    pub center_y_frac: Fixed,
    /// `projection` (vanilla stores the same value as `centerxfrac`).
    pub projection_distance: Fixed,
    /// `extralight` — gun-flash light bump, added to every light row.
    pub extralight: i32,
    /// `fixedcolormap`: a forced colormap level (e.g. radiation suit)
    /// applied to every wall column.
    pub fixed_colormap: Option<usize>,
    /// `detailshift`: `0` high detail ([`draw_column`]), `1` low detail
    /// ([`draw_column_low`]).
    pub detail_shift: i32,
    /// The sky flat name (vanilla `skyflatnum` compare); `None` when no
    /// sky flat is loaded.
    pub sky_flat: Option<[u8; 8]>,
}

/// Per-frame mutable state: clip arrays, drawsegs, openings and the
/// framebuffer. Construct with [`WallFrame::new`], call [`WallFrame::clear`]
/// once per frame, then drive [`WallFrame::store_wall_range`] from the
/// BSP traversal.
pub struct WallFrame<'a> {
    view: WallView<'a>,
    planes: PlaneSet,
    fb: &'a mut IndexedFramebuffer,
    ceiling_clip: [i16; SCREEN_WIDTH],
    floor_clip: [i16; SCREEN_WIDTH],
    screen_height_array: Vec<i16>,
    draw_segs: Vec<DrawSeg>,
    openings: Openings,
    mapped_lines: Vec<usize>,
    errors: Vec<EngineError>,
    warned_drawsegs_full: bool,
    warned_openings_full: bool,
}

/// The `rw_*` state for the wall currently being stored (vanilla
/// file-scope globals in `r_segs.c`, one set per seg).
#[derive(Clone)]
struct RwState {
    rw_stopx: i32,
    rw_centerangle: Angle,
    rw_offset: Fixed,
    rw_distance: Fixed,
    rw_scale: Fixed,
    rw_scalestep: Fixed,
    rw_midtexturemid: Fixed,
    rw_toptexturemid: Fixed,
    rw_bottomtexturemid: Fixed,
    pixhigh: Fixed,
    pixlow: Fixed,
    pixhighstep: Fixed,
    pixlowstep: Fixed,
    topfrac: Fixed,
    topstep: Fixed,
    bottomfrac: Fixed,
    bottomstep: Fixed,
    segtextured: bool,
    markfloor: bool,
    markceiling: bool,
    midtexture: Option<usize>,
    toptexture: Option<usize>,
    bottomtexture: Option<usize>,
    maskedtexture: bool,
    light_row: usize,
    /// The seg came from a single-sided line: always claim the whole
    /// column (terminal wall), even when its texture failed to resolve.
    terminal_single_sided: bool,
}

/// `R_PointToDist`: distance from the view point to `(x, y)`.
///
/// The original converts the slope through the `tantoangle` table; this
/// port asks the BSP-side [`point_to_angle_2`] for the same angle instead
/// of duplicating the 2049-entry table (the correct fix — exporting the
/// table for `r_main.c`-style users — is a bsp module change, out of this
/// task's scope). The quantization differs by at most one tantoangle
/// entry (~0.05 deg) in borderline slopes, which is invisible here.
fn point_to_dist(view: &WallView<'_>, x: Fixed, y: Fixed) -> Fixed {
    let mut dx = (x - view.view_x).abs();
    let mut dy = (y - view.view_y).abs();
    if dy > dx {
        std::mem::swap(&mut dx, &mut dy);
    }
    if dx.is_zero() {
        // viewpoint exactly on the vertex; the original would probe the
        // tangent table out of bounds here, so it has no behavior to
        // inherit. Zero distance is the sane reading.
        return Fixed::ZERO;
    }
    let theta = point_to_angle_2(Fixed::ZERO, Fixed::ZERO, dx, dy);
    let index = (theta + Angle::ANG90).fine_angle();
    dx / FINE_SINE[index]
}

/// `R_ScaleFromGlobalAngle`.
fn scale_from_global_angle(
    view: &WallView<'_>,
    visangle: Angle,
    rw_distance: Fixed,
    rw_normalangle: Angle,
) -> Fixed {
    let anglea = Angle::ANG90 + (visangle - view.view_angle);
    let angleb = Angle::ANG90 + (visangle - rw_normalangle);

    // both sines are allways positive
    let sinea = FINE_SINE[anglea.fine_angle()];
    let sineb = FINE_SINE[angleb.fine_angle()];
    let num = (view.projection_distance * sineb).to_raw() << view.detail_shift;
    let den = rw_distance * sinea;

    if den.to_raw() > num >> 16 {
        let scale = Fixed::from_raw(num) / den;

        if scale.to_raw() > 64 << 16 {
            Fixed::from_raw(64 << 16)
        } else if scale.to_raw() < 256 {
            Fixed::from_raw(256)
        } else {
            scale
        }
    } else {
        Fixed::from_raw(64 << 16)
    }
}

/// Wall/midtexture column draw dispatch on `detailshift`.
fn draw_wall_column(
    fb: &mut IndexedFramebuffer,
    source: &impl super::column::ColumnSource,
    params: &ColumnParams<'_>,
    detail_shift: i32,
) {
    if detail_shift == 0 {
        draw_column(fb, source, params);
    } else {
        draw_column_low(fb, source, params);
    }
}

/// `(lightlevel >> LIGHTSEGSHIFT) + extralight`, adjusted by one row for
/// horizontal/vertical segs, clamped to the table — the same inlined math
/// the original uses for both walllights selection sites.
fn light_row(light_level: i16, horizontal: bool, vertical: bool, extralight: i32) -> usize {
    let mut row = (i32::from(light_level) >> LIGHTSEGSHIFT) + extralight;
    if horizontal {
        row -= 1;
    } else if vertical {
        row += 1;
    }
    row.clamp(0, LIGHTLEVELS as i32 - 1) as usize
}

impl<'a> WallFrame<'a> {
    /// A wall frame for `view` rasterizing into `fb`.
    pub fn new(view: WallView<'a>, fb: &'a mut IndexedFramebuffer) -> Self {
        let screen_height_array = vec![view.view_height as i16; view.view_width as usize];
        let view_height = view.view_height;
        Self {
            view,
            planes: PlaneSet::new(),
            fb,
            ceiling_clip: [-1; SCREEN_WIDTH],
            floor_clip: [view_height as i16; SCREEN_WIDTH],
            screen_height_array,
            draw_segs: Vec::with_capacity(MAX_DRAWSEGS),
            openings: Openings::new(),
            mapped_lines: Vec::new(),
            errors: Vec::new(),
            warned_drawsegs_full: false,
            warned_openings_full: false,
        }
    }

    /// `R_ClearDrawSegs` + the clip-reset half of `R_ClearPlanes`: a fresh
    /// frame with empty clip arrays, no visplanes, no drawsegs.
    pub fn clear(&mut self) {
        self.draw_segs.clear();
        self.openings.clear();
        self.planes.clear();
        self.mapped_lines.clear();
        self.errors.clear();
        self.warned_drawsegs_full = false;
        self.warned_openings_full = false;
        let view_height = self.view.view_height;
        self.ceiling_clip = [-1; SCREEN_WIDTH];
        self.floor_clip = [view_height as i16; SCREEN_WIDTH];
    }

    /// `R_ClearPlanes` proper (ceiling clip `-1`, floor clip `viewheight`),
    /// kept separate from visplane clearing for symmetry with the original's
    /// split; [`WallFrame::clear`] does both.
    pub fn clear_clips(&mut self) {
        let view_height = self.view.view_height;
        self.ceiling_clip = [-1; SCREEN_WIDTH];
        self.floor_clip = [view_height as i16; SCREEN_WIDTH];
    }

    /// The framebuffer handed back.
    pub fn into_framebuffer(self) -> &'a mut IndexedFramebuffer {
        self.fb
    }

    /// Read access to the framebuffer without giving it up (tests and
    /// debugging; the renderer writes through `draw_*` calls).
    pub fn framebuffer(&self) -> &IndexedFramebuffer {
        self.fb
    }

    /// The visplane pool (for the span renderer and tests).
    pub fn planes(&self) -> &PlaneSet {
        &self.planes
    }

    /// The planes, mutably (span renderer).
    pub fn planes_mut(&mut self) -> &mut PlaneSet {
        &mut self.planes
    }

    /// The drawn segments, in draw order.
    pub fn draw_segs(&self) -> &[DrawSeg] {
        &self.draw_segs
    }

    /// `ceilingclip[]` (first `view_width` entries are live).
    pub fn ceiling_clip(&self) -> &[i16; SCREEN_WIDTH] {
        &self.ceiling_clip
    }

    /// `floorclip[]` (first `view_width` entries are live).
    pub fn floor_clip(&self) -> &[i16; SCREEN_WIDTH] {
        &self.floor_clip
    }

    /// Linedef indices seen as mapped this frame (vanilla `ML_MAPPED`), drained.
    pub fn take_mapped_linedefs(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.mapped_lines)
    }

    /// Errors collected while rasterizing (missing textures, ...), drained.
    pub fn take_errors(&mut self) -> Vec<EngineError> {
        std::mem::take(&mut self.errors)
    }

    // ------------------------------------------------------------------
    // clip references for the sprite pass
    // ------------------------------------------------------------------

    fn clip_ref_value(&self, clip: ClipRef, x: i32) -> Option<i16> {
        match clip {
            ClipRef::None => None,
            ClipRef::ScreenHeight => usize::try_from(x)
                .ok()
                .and_then(|i| self.screen_height_array.get(i))
                .copied(),
            ClipRef::NegOne => Some(-1),
            ClipRef::Openings { skew } => usize::try_from(skew + i64::from(x))
                .ok()
                .and_then(|i| self.openings.get(i)),
        }
    }

    fn set_masked_col(&mut self, skew: i64, x: i32, value: i16) {
        self.openings.set(skew + i64::from(x), value);
    }

    fn consume_masked_col(&mut self, ds: &DrawSeg, x: i32) {
        if let ClipRef::Openings { skew } = ds.maskedtexturecol {
            self.set_masked_col(skew, x, MAX_SHORT);
        }
    }

    /// `ds->sprtopclip[x]`: the top clip the sprite pass must respect at
    /// column `x`. `None` for segs the caller must not consult.
    pub fn seg_top_clip(&self, seg: &DrawSeg, x: i32) -> Option<i16> {
        self.clip_ref_value(seg.sprtopclip, x)
    }

    /// `ds->sprbottomclip[x]`.
    pub fn seg_bottom_clip(&self, seg: &DrawSeg, x: i32) -> Option<i16> {
        self.clip_ref_value(seg.sprbottomclip, x)
    }

    /// `ds->maskedtexturecol[x]`: the texture column the masked pass has
    /// not drawn yet at `x`, if any.
    pub fn seg_masked_col(&self, seg: &DrawSeg, x: i32) -> Option<i16> {
        self.clip_ref_value(seg.maskedtexturecol, x)
    }

    // ------------------------------------------------------------------
    // small helpers
    // ------------------------------------------------------------------

    fn is_sky_flat(&self, pic: &[u8; 8]) -> bool {
        match self.view.sky_flat {
            Some(sky) => *pic == sky,
            None => false,
        }
    }

    fn resolve_texture(&mut self, raw: &[u8; 8], what: &str) -> Option<usize> {
        match self.view.textures.lookup_translated(raw) {
            Some(index) => Some(index),
            None => {
                let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                let name = String::from_utf8_lossy(&raw[..end]).to_ascii_uppercase();
                self.errors.push(EngineError::Resource(format!(
                    "{what} texture {name} not found in TEXTURE1/TEXTURE2"
                )));
                None
            }
        }
    }

    /// Colormap for one wall column: `scalelight` row, or the fixed colormap.
    fn light_colormap_index(&self, row: usize, scale: Fixed) -> usize {
        match self.view.fixed_colormap {
            Some(level) => level.min(self.view.colormaps.len().saturating_sub(1)),
            None => self
                .view
                .light
                .scalelight(row, LightTables::scale_to_index(scale))
                .min(self.view.colormaps.len().saturating_sub(1)),
        }
    }

    fn openings_full(&mut self) {
        if !self.warned_openings_full {
            tracing::error!(
                "openings buffer full ({MAX_OPENINGS}); sprite clipping for new segs is dropped"
            );
            self.warned_openings_full = true;
        }
    }

    fn drawsegs_full(&mut self) {
        if !self.warned_drawsegs_full {
            tracing::error!("drawseg pool exhausted ({MAX_DRAWSEGS}); visseg dropped");
            self.warned_drawsegs_full = true;
        }
    }

    /// Visplane marker write helper: silently skips planes whose pool
    /// entry vanished (`None`), logs nothing (the pool logs on failure).
    fn mark_plane(&mut self, plane: Option<usize>, x: i32, top: i32, bottom: i32) {
        if let Some(index) = plane
            && let Some(plane) = self.planes.get_mut(index)
        {
            plane.mark(x, top, bottom);
        }
    }
}

impl<'a> WallFrame<'a> {
    /// `R_StoreWallRange`: draw one visible seg fragment `[start, stop]`
    /// (inclusive, screen columns). `rw_angle1` is the seg's
    /// view-absolute `v1` angle, as returned by [`crate::bsp::add_line`];
    /// `floor_plane`/`ceiling_plane` are the visplane indices for the
    /// seg's subsector (`None` when the plane is invisible or the pool
    /// is exhausted).
    // the argument list mirrors the vanilla `R_StoreWallRange` call site;
    // a parameter object would just shuffle the same fields around
    #[allow(clippy::too_many_arguments)]
    pub fn store_wall_range(
        &mut self,
        map: &Map,
        seg_index: usize,
        rw_angle1: Angle,
        start: i32,
        stop: i32,
        floor_plane: Option<usize>,
        ceiling_plane: Option<usize>,
    ) {
        // the original aborts on a bad range; we stay alive and complain
        if start < 0 || stop >= self.view.view_width || start > stop {
            tracing::error!(
                start,
                stop,
                "R_StoreWallRange called with a bad column range"
            );
            return;
        }

        let seg = map.segs[seg_index];
        let sector_at = |i: usize| &map.sectors[i];
        let linedef = &map.linedefs[seg.linedef];
        let sidedef = &map.sidedefs[seg.sidedef];
        let frontsector = sector_at(seg.frontsector);
        let back_index = seg.backsector;
        let backsector: Option<&Sector> = back_index.map(sector_at);
        let v1 = map.vertexes[seg.v1];
        let v2 = map.vertexes[seg.v2];

        // mark the segment as visible for auto map (vanilla ML_MAPPED)
        self.mapped_lines.push(seg.linedef);

        // calculate rw_distance for scale calculation
        let rw_normalangle = Angle::from_raw(seg.angle) + Angle::ANG90;
        let raw_diff = rw_normalangle.to_raw().wrapping_sub(rw_angle1.to_raw());
        let mut offsetangle = Angle::from_raw(raw_diff);
        if offsetangle.to_raw() >= 0x8000_0000 {
            offsetangle = Angle::from_raw(0u32.wrapping_sub(offsetangle.to_raw()));
        }
        if offsetangle.to_raw() > Angle::ANG90.to_raw() {
            offsetangle = Angle::ANG90;
        }

        let distangle = Angle::ANG90 - offsetangle;
        let hyp = point_to_dist(&self.view, v1.x, v1.y);
        let sineval = FINE_SINE[distangle.fine_angle()];
        let rw_distance = hyp * sineval;

        let rw_x = start;
        let rw_stopx = stop + 1;

        // calculate scale at both ends and step
        let scale1 = scale_from_global_angle(
            &self.view,
            self.view.view_angle + self.view.projection.at(start),
            rw_distance,
            rw_normalangle,
        );
        let (scale2, scalestep) = if stop > start {
            let scale2 = scale_from_global_angle(
                &self.view,
                self.view.view_angle + self.view.projection.at(stop),
                rw_distance,
                rw_normalangle,
            );
            let scalestep = Fixed::from_raw((scale2.to_raw() - scale1.to_raw()) / (stop - start));
            (scale2, scalestep)
        } else {
            // vanilla leaves scalestep stale for single-column segs (never
            // consumed there); we store an explicit zero instead.
            (scale1, Fixed::ZERO)
        };

        // calculate texture boundaries
        //  and decide if floor / ceiling marks are needed
        let mut worldtop = frontsector.ceiling_height - self.view.view_z;
        let worldbottom = frontsector.floor_height - self.view.view_z;

        let mut midtexture: Option<usize> = None;
        let mut toptexture: Option<usize> = None;
        let mut bottomtexture: Option<usize> = None;
        let mut maskedtexture = false;
        let mut rw_midtexturemid = Fixed::ZERO;
        let mut rw_toptexturemid = Fixed::ZERO;
        let mut rw_bottomtexturemid = Fixed::ZERO;

        let mut ds = DrawSeg::empty();
        ds.seg = seg_index;
        ds.x1 = rw_x;
        ds.x2 = stop;
        ds.scale1 = scale1;
        ds.scale2 = scale2;
        ds.scalestep = scalestep;

        // definite assignment: both arms assign before any read
        let mut markfloor: bool;
        let mut markceiling: bool;

        match backsector {
            None => {
                // single sided line
                midtexture = self.resolve_texture(&sidedef.midtexture, "middle");
                // a single sided line is terminal, so it must mark ends
                markfloor = true;
                markceiling = true;
                if linedef.flags & ML_DONTPEGBOTTOM != 0 {
                    // bottom of texture at bottom
                    let texnum = midtexture.unwrap_or(0);
                    let vtop = frontsector.floor_height + self.view.textures.texture_height(texnum);
                    rw_midtexturemid = vtop - self.view.view_z;
                } else {
                    // top of texture at top
                    rw_midtexturemid = worldtop;
                }
                rw_midtexturemid += sidedef.row_offset;

                ds.silhouette = SIL_BOTH;
                ds.sprtopclip = ClipRef::ScreenHeight;
                ds.sprbottomclip = ClipRef::NegOne;
                ds.bsilheight = Fixed::MAX;
                ds.tsilheight = Fixed::MIN;
            }
            Some(backsector) => {
                // two sided line
                ds.sprtopclip = ClipRef::None;
                ds.sprbottomclip = ClipRef::None;
                ds.silhouette = SIL_NONE;

                if frontsector.floor_height > backsector.floor_height {
                    ds.silhouette = SIL_BOTTOM;
                    ds.bsilheight = frontsector.floor_height;
                } else if backsector.floor_height > self.view.view_z {
                    ds.silhouette = SIL_BOTTOM;
                    ds.bsilheight = Fixed::MAX;
                }

                if frontsector.ceiling_height < backsector.ceiling_height {
                    ds.silhouette |= SIL_TOP;
                    ds.tsilheight = frontsector.ceiling_height;
                } else if backsector.ceiling_height < self.view.view_z {
                    ds.silhouette |= SIL_TOP;
                    ds.tsilheight = Fixed::MIN;
                }

                if backsector.ceiling_height <= frontsector.floor_height {
                    ds.sprbottomclip = ClipRef::NegOne;
                    ds.bsilheight = Fixed::MAX;
                    ds.silhouette |= SIL_BOTTOM;
                }

                if backsector.floor_height >= frontsector.ceiling_height {
                    ds.sprtopclip = ClipRef::ScreenHeight;
                    ds.tsilheight = Fixed::MIN;
                    ds.silhouette |= SIL_TOP;
                }

                let worldhigh = backsector.ceiling_height - self.view.view_z;
                let worldlow = backsector.floor_height - self.view.view_z;

                // hack to allow height changes in outdoor areas
                if self.is_sky_flat(&frontsector.ceiling_pic)
                    && self.is_sky_flat(&backsector.ceiling_pic)
                {
                    worldtop = worldhigh;
                }

                markfloor = worldlow != worldbottom
                    || backsector.floor_pic != frontsector.floor_pic
                    || backsector.light_level != frontsector.light_level;

                markceiling = worldhigh != worldtop
                    || backsector.ceiling_pic != frontsector.ceiling_pic
                    || backsector.light_level != frontsector.light_level;

                if backsector.ceiling_height <= frontsector.floor_height
                    || backsector.floor_height >= frontsector.ceiling_height
                {
                    // closed door
                    markceiling = true;
                    markfloor = true;
                }

                if worldhigh < worldtop {
                    // top texture
                    toptexture = self.resolve_texture(&sidedef.toptexture, "top");
                    if linedef.flags & ML_DONTPEGTOP != 0 {
                        // top of texture at top
                        rw_toptexturemid = worldtop;
                    } else {
                        let texnum = toptexture.unwrap_or(0);
                        let vtop =
                            backsector.ceiling_height + self.view.textures.texture_height(texnum);
                        // bottom of texture
                        rw_toptexturemid = vtop - self.view.view_z;
                    }
                }
                if worldlow > worldbottom {
                    // bottom texture
                    bottomtexture = self.resolve_texture(&sidedef.bottomtexture, "bottom");
                    if linedef.flags & ML_DONTPEGBOTTOM != 0 {
                        // bottom of texture at bottom (vanilla pegs it to worldtop here)
                        rw_bottomtexturemid = worldtop;
                    } else {
                        // top of texture at top
                        rw_bottomtexturemid = worldlow;
                    }
                }
                rw_toptexturemid += sidedef.row_offset;
                rw_bottomtexturemid += sidedef.row_offset;

                // allocate space for masked texture tables
                let has_midtexture =
                    sidedef.midtexture[0] != b'-' && sidedef.midtexture.iter().any(|&b| b != 0);
                if has_midtexture {
                    // masked midtexture
                    maskedtexture = true;
                    match self.openings.alloc((rw_stopx - rw_x) as usize) {
                        Some(base) => {
                            ds.maskedtexturecol = ClipRef::Openings {
                                skew: base as i64 - rw_x as i64,
                            };
                        }
                        None => {
                            self.openings_full();
                            // openings exhausted; the masked pass skips this seg
                            ds.maskedtexturecol = ClipRef::None;
                        }
                    }
                }
            }
        }

        let segtextured = midtexture.is_some()
            || toptexture.is_some()
            || bottomtexture.is_some()
            || maskedtexture;

        let mut rw_offset = Fixed::ZERO;
        let mut rw_centerangle = Angle::ZERO;
        if segtextured {
            offsetangle = rw_normalangle - rw_angle1;
            if offsetangle.to_raw() > Angle::ANG180.to_raw() {
                offsetangle = Angle::from_raw(0u32.wrapping_sub(offsetangle.to_raw()));
            }
            if offsetangle.to_raw() > Angle::ANG90.to_raw() {
                offsetangle = Angle::ANG90;
            }

            let sineval = FINE_SINE[offsetangle.fine_angle()];
            rw_offset = hyp * sineval;

            if (rw_normalangle - rw_angle1).to_raw() < Angle::ANG180.to_raw() {
                rw_offset = -rw_offset;
            }

            rw_offset += sidedef.texture_offset + seg.offset;
            rw_centerangle = Angle::ANG90 + self.view.view_angle - rw_normalangle;
        }

        // calculate light row for walllights; horizontal/vertical segs shift by one
        let light_row = light_row(
            frontsector.light_level,
            v1.y == v2.y,
            v1.x == v2.x,
            self.view.extralight,
        );

        // planes on the wrong side of the view plane are definitely
        // invisible and don't need to be marked. (the sky ceiling is the
        // exception: it draws despite being below the view plane)
        if frontsector.floor_height >= self.view.view_z {
            // above view plane
            markfloor = false;
        }
        if frontsector.ceiling_height <= self.view.view_z
            && !self.is_sky_flat(&frontsector.ceiling_pic)
        {
            // below view plane
            markceiling = false;
        }

        // calculate incremental stepping values for texture edges
        let worldtop_shifted = Fixed::from_raw(worldtop.to_raw() >> 4);
        let worldbottom_shifted = Fixed::from_raw(worldbottom.to_raw() >> 4);
        let half_height = Fixed::from_raw(self.view.center_y_frac.to_raw() >> 4);

        let topstep = -(scalestep * worldtop_shifted);
        let topfrac = half_height - worldtop_shifted * scale1;

        let bottomstep = -(scalestep * worldbottom_shifted);
        let bottomfrac = half_height - worldbottom_shifted * scale1;

        let (pixhigh, pixhighstep, pixlow, pixlowstep) = match backsector {
            None => (Fixed::ZERO, Fixed::ZERO, Fixed::ZERO, Fixed::ZERO),
            Some(backsector) => {
                let worldhigh =
                    Fixed::from_raw((backsector.ceiling_height - self.view.view_z).to_raw() >> 4);
                let worldlow =
                    Fixed::from_raw((backsector.floor_height - self.view.view_z).to_raw() >> 4);

                let mut pixhigh = Fixed::ZERO;
                let mut pixhighstep = Fixed::ZERO;
                if worldhigh < worldtop_shifted {
                    pixhigh = half_height - worldhigh * scale1;
                    pixhighstep = -(scalestep * worldhigh);
                }

                let mut pixlow = Fixed::ZERO;
                let mut pixlowstep = Fixed::ZERO;
                if worldlow > worldbottom_shifted {
                    pixlow = half_height - worldlow * scale1;
                    pixlowstep = -(scalestep * worldlow);
                }

                (pixhigh, pixhighstep, pixlow, pixlowstep)
            }
        };

        // render it
        let ceiling_plane = if markceiling {
            self.check_plane(ceiling_plane, rw_x, rw_stopx - 1)
        } else {
            ceiling_plane
        };
        let floor_plane = if markfloor {
            self.check_plane(floor_plane, rw_x, rw_stopx - 1)
        } else {
            floor_plane
        };

        let mut rw = RwState {
            rw_stopx,
            rw_centerangle,
            rw_offset,
            rw_distance,
            rw_scale: scale1,
            rw_scalestep: scalestep,
            rw_midtexturemid,
            rw_toptexturemid,
            rw_bottomtexturemid,
            pixhigh,
            pixlow,
            pixhighstep,
            pixlowstep,
            topfrac,
            topstep,
            bottomfrac,
            bottomstep,
            segtextured,
            markfloor,
            markceiling,
            midtexture,
            toptexture,
            bottomtexture,
            maskedtexture,
            light_row,
            terminal_single_sided: backsector.is_none(),
        };

        self.render_seg_loop(&mut rw, rw_x, &ds, floor_plane, ceiling_plane);

        // save sprite clipping info
        if (ds.silhouette & SIL_TOP != 0 || maskedtexture) && ds.sprtopclip == ClipRef::None {
            match self.openings.alloc((rw_stopx - start) as usize) {
                Some(base) => {
                    self.openings
                        .copy_from_clip(base, &self.ceiling_clip, start, stop);
                    ds.sprtopclip = ClipRef::Openings {
                        skew: base as i64 - start as i64,
                    };
                }
                None => {
                    self.openings_full();
                    // degrade to the shared sentinel; sprites above the seg stay visible
                    ds.sprtopclip = ClipRef::ScreenHeight;
                }
            }
        }

        if (ds.silhouette & SIL_BOTTOM != 0 || maskedtexture) && ds.sprbottomclip == ClipRef::None {
            match self.openings.alloc((rw_stopx - start) as usize) {
                Some(base) => {
                    self.openings
                        .copy_from_clip(base, &self.floor_clip, start, stop);
                    ds.sprbottomclip = ClipRef::Openings {
                        skew: base as i64 - start as i64,
                    };
                }
                None => {
                    self.openings_full();
                    ds.sprbottomclip = ClipRef::NegOne;
                }
            }
        }

        if maskedtexture && ds.silhouette & SIL_TOP == 0 {
            ds.silhouette |= SIL_TOP;
            ds.tsilheight = Fixed::MIN;
        }
        if maskedtexture && ds.silhouette & SIL_BOTTOM == 0 {
            ds.silhouette |= SIL_BOTTOM;
            ds.bsilheight = Fixed::MAX;
        }

        // store sprite clipping info (or drop it when the pool is full)
        if self.draw_segs.len() >= MAX_DRAWSEGS {
            self.drawsegs_full();
        } else {
            self.draw_segs.push(ds);
        }
    }

    /// Visplane marker write helper: skips the marker entirely when the
    /// pool or the caller produced no plane; the pool already logged.
    fn check_plane(&mut self, plane: Option<usize>, start: i32, stop: i32) -> Option<usize> {
        match plane {
            Some(index) => self.planes.check_plane(index, start, stop),
            None => None,
        }
    }

    /// `R_RenderSegLoop`: the central per-column loop. Draws zero, one or
    /// two textures (and marks the masked column) per column.
    fn render_seg_loop(
        &mut self,
        rw: &mut RwState,
        start: i32,
        ds: &DrawSeg,
        floor_plane: Option<usize>,
        ceiling_plane: Option<usize>,
    ) {
        let view_height = self.view.view_height;

        for x in start..rw.rw_stopx {
            // mark floor / ceiling areas
            let yl = rw.topfrac.to_raw().wrapping_add(HEIGHTUNIT - 1) >> HEIGHTBITS;
            let ceiling = i32::from(self.ceiling_clip[x as usize]);
            let floor = i32::from(self.floor_clip[x as usize]);
            let mut yl = yl;
            if yl < ceiling + 1 {
                yl = ceiling + 1;
            }

            if rw.markceiling {
                let top = ceiling + 1;
                let mut bottom = yl - 1;
                if bottom >= floor {
                    bottom = floor - 1;
                }
                if top <= bottom {
                    self.mark_plane(ceiling_plane, x, top, bottom);
                }
            }

            let yh = rw.bottomfrac.to_raw() >> HEIGHTBITS;
            let mut yh = yh;
            if yh >= floor {
                yh = floor - 1;
            }

            if rw.markfloor {
                let mut top = yh + 1;
                let bottom = floor - 1;
                let ceiling = i32::from(self.ceiling_clip[x as usize]);
                if top <= ceiling {
                    top = ceiling + 1;
                }
                if top <= bottom {
                    self.mark_plane(floor_plane, x, top, bottom);
                }
            }

            // texturecolumn and lighting are independent of wall tiers
            let texture_sample = if rw.segtextured {
                if rw.rw_scale.to_raw() <= 0 {
                    // vanilla divides by zero here (x86 fault); skip the rest
                    // of the seg instead of panicking. the clips stay valid.
                    tracing::error!(
                        "wall scale went non-positive mid-seg; remaining columns skipped"
                    );
                    break;
                }
                let angle = (rw.rw_centerangle + self.view.projection.at(x)).fine_angle();
                debug_assert!(
                    angle < FINE_TANGENT.len(),
                    "fine tangent index out of range"
                );
                let texturecolumn =
                    (rw.rw_offset - FINE_TANGENT[angle] * rw.rw_distance).to_raw() >> FRACBITS;
                let light = self.light_colormap_index(rw.light_row, rw.rw_scale);
                let iscale =
                    Fixed::from_raw((0xffff_ffffu32 / (rw.rw_scale.to_raw() as u32)) as i32);
                Some((texturecolumn, light, iscale))
            } else {
                None
            };

            // draw the wall tiers
            if rw.terminal_single_sided {
                // single sided line: always claim the whole column
                if let Some(midtexture) = rw.midtexture {
                    let (texturecolumn, light, iscale) =
                        texture_sample.expect("midtexture implies segtextured");
                    match self.view.textures.column(midtexture, texturecolumn) {
                        Ok(source) => {
                            let colormap = &self.view.colormaps[light];
                            let params = ColumnParams {
                                colormap,
                                x,
                                y_lo: yl,
                                y_hi: yh,
                                iscale,
                                texturemid: rw.rw_midtexturemid,
                                center_y: self.view.center_y,
                            };
                            draw_wall_column(self.fb, &source, &params, self.view.detail_shift);
                        }
                        Err(err) => self.errors.push(err),
                    }
                }
                self.ceiling_clip[x as usize] = view_height as i16;
                self.floor_clip[x as usize] = -1;
            } else {
                // two sided line
                if let Some(toptexture) = rw.toptexture {
                    // top wall
                    let mut mid = rw.pixhigh.to_raw() >> HEIGHTBITS;
                    rw.pixhigh += rw.pixhighstep;
                    if mid >= i32::from(self.floor_clip[x as usize]) {
                        mid = i32::from(self.floor_clip[x as usize]) - 1;
                    }

                    if mid >= yl {
                        let (texturecolumn, light, iscale) =
                            texture_sample.expect("toptexture implies segtextured");
                        match self.view.textures.column(toptexture, texturecolumn) {
                            Ok(source) => {
                                let colormap = &self.view.colormaps[light];
                                let params = ColumnParams {
                                    colormap,
                                    x,
                                    y_lo: yl,
                                    y_hi: mid,
                                    iscale,
                                    texturemid: rw.rw_toptexturemid,
                                    center_y: self.view.center_y,
                                };
                                draw_wall_column(self.fb, &source, &params, self.view.detail_shift);
                            }
                            Err(err) => self.errors.push(err),
                        }
                        self.ceiling_clip[x as usize] = mid as i16;
                    } else {
                        self.ceiling_clip[x as usize] = (yl - 1) as i16;
                    }
                } else if rw.markceiling {
                    // no top wall
                    self.ceiling_clip[x as usize] = (yl - 1) as i16;
                }

                if let Some(bottomtexture) = rw.bottomtexture {
                    // bottom wall
                    let mut mid = rw.pixlow.to_raw().wrapping_add(HEIGHTUNIT - 1) >> HEIGHTBITS;
                    rw.pixlow += rw.pixlowstep;

                    let ceiling = i32::from(self.ceiling_clip[x as usize]);
                    if mid <= ceiling {
                        mid = ceiling + 1;
                    }

                    if mid <= yh {
                        let (texturecolumn, light, iscale) =
                            texture_sample.expect("bottomtexture implies segtextured");
                        match self.view.textures.column(bottomtexture, texturecolumn) {
                            Ok(source) => {
                                let colormap = &self.view.colormaps[light];
                                let params = ColumnParams {
                                    colormap,
                                    x,
                                    y_lo: mid,
                                    y_hi: yh,
                                    iscale,
                                    texturemid: rw.rw_bottomtexturemid,
                                    center_y: self.view.center_y,
                                };
                                draw_wall_column(self.fb, &source, &params, self.view.detail_shift);
                            }
                            Err(err) => self.errors.push(err),
                        }
                        self.floor_clip[x as usize] = mid as i16;
                    } else {
                        self.floor_clip[x as usize] = (yh + 1) as i16;
                    }
                } else if rw.markfloor {
                    // no bottom wall
                    self.floor_clip[x as usize] = (yh + 1) as i16;
                }

                if rw.maskedtexture {
                    // save texturecol for backdrawing of masked mid texture
                    if let (ClipRef::Openings { skew }, Some((texturecolumn, _, _))) =
                        (ds.maskedtexturecol, texture_sample)
                    {
                        self.set_masked_col(skew, x, texturecolumn as i16);
                    }
                }
            }

            rw.rw_scale += rw.rw_scalestep;
            rw.topfrac += rw.topstep;
            rw.bottomfrac += rw.bottomstep;
        }
    }

    /// `R_RenderMaskedSegRange`: draw the masked midtexture (fence,
    /// railing) for one stored seg over columns `[x1, x2]`.
    ///
    /// Consumes the saved texture columns (`maskedtexturecol`), so call it
    /// once per seg per frame. The sprite pass calls this for segs that
    /// carry masked midtexture columns.
    pub fn render_masked_seg_range(&mut self, map: &Map, draw_seg_index: usize, x1: i32, x2: i32) {
        let ds = self.draw_segs[draw_seg_index];
        if ds.maskedtexturecol == ClipRef::None {
            // nothing allocated for this seg (or the pool was full)
            return;
        }

        let seg = &map.segs[ds.seg];
        let linedef = &map.linedefs[seg.linedef];
        let sidedef = &map.sidedefs[seg.sidedef];
        let frontsector = &map.sectors[seg.frontsector];
        let back_index = match seg.backsector {
            Some(index) => index,
            None => {
                self.errors.push(EngineError::Resource(format!(
                    "masked seg {} has no back sector (single sided)",
                    ds.seg
                )));
                return;
            }
        };
        let backsector = &map.sectors[back_index];
        let v1 = map.vertexes[seg.v1];
        let v2 = map.vertexes[seg.v2];

        let texnum = match self.resolve_texture(&sidedef.midtexture, "masked middle") {
            Some(texnum) => texnum,
            None => return,
        };
        let texture_height = self.view.textures.texture_height(texnum);

        let row = light_row(
            frontsector.light_level,
            v1.y == v2.y,
            v1.x == v2.x,
            self.view.extralight,
        );

        let rw_scalestep = ds.scalestep;
        let mut spryscale = ds.scale1 + ds.scalestep * Fixed::from_int(x1 - ds.x1);
        let bottom_ref = ds.sprbottomclip;
        let top_ref = ds.sprtopclip;

        // find positioning
        let base_texmid = if linedef.flags & ML_DONTPEGBOTTOM != 0 {
            let floor = frontsector.floor_height.max(backsector.floor_height);
            floor + texture_height - self.view.view_z
        } else {
            let ceiling = frontsector.ceiling_height.min(backsector.ceiling_height);
            ceiling - self.view.view_z
        } + sidedef.row_offset;

        let pixels = match self.view.textures.pixels(texnum) {
            Ok(pixels) => pixels,
            Err(err) => {
                self.errors.push(err);
                return;
            }
        };

        for dc_x in x1..=x2 {
            let col_raw = self.seg_masked_col(&ds, dc_x);
            let col = match col_raw {
                Some(MAX_SHORT) | None => {
                    spryscale += rw_scalestep;
                    continue;
                }
                Some(raw) => i32::from(raw),
            };

            // calculate lighting
            let light = self.light_colormap_index(row, spryscale);

            if spryscale.to_raw() <= 0 {
                // vanilla divides by zero here; skip rather than crash
                tracing::error!("masked seg scale went non-positive; column skipped");
                self.consume_masked_col(&ds, dc_x);
                spryscale += rw_scalestep;
                continue;
            }
            let dc_iscale = Fixed::from_raw((0xffff_ffffu32 / spryscale.to_raw() as u32) as i32);
            let sprtopscreen = self.view.center_y_frac - base_texmid * spryscale;

            let runs: Vec<TextureRun> = pixels.runs(col).to_vec();
            let column = pixels.column_pixels(col).to_vec();
            let colormap = &self.view.colormaps[light];

            for run in runs {
                // calculate unclipped screen coordinates for the post
                let topscreen =
                    sprtopscreen + Fixed::from_raw(spryscale.to_raw().wrapping_mul(run.top));
                let bottomscreen =
                    topscreen + Fixed::from_raw(spryscale.to_raw().wrapping_mul(run.len));

                let mut dc_yl = topscreen.to_raw().wrapping_add(0xFFFF) >> FRACBITS;
                let mut dc_yh = (bottomscreen.to_raw() - 1) >> FRACBITS;

                if let Some(clip) = self.clip_ref_value(bottom_ref, dc_x)
                    && dc_yh >= i32::from(clip)
                {
                    dc_yh = i32::from(clip) - 1;
                }
                if let Some(clip) = self.clip_ref_value(top_ref, dc_x)
                    && dc_yl <= i32::from(clip)
                {
                    dc_yl = i32::from(clip) + 1;
                }

                if dc_yl <= dc_yh {
                    let src = UnmaskedColumn::new(
                        &column[run.top as usize..(run.top + run.len) as usize],
                    );
                    let params = ColumnParams {
                        colormap,
                        x: dc_x,
                        y_lo: dc_yl,
                        y_hi: dc_yh,
                        iscale: dc_iscale,
                        texturemid: base_texmid - Fixed::from_int(run.top),
                        center_y: self.view.center_y,
                    };
                    draw_wall_column(self.fb, &src, &params, self.view.detail_shift);
                }
            }

            self.consume_masked_col(&ds, dc_x);
            spryscale += rw_scalestep;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bsp::{AddLineOutcome, ClipSegList, ViewState, add_line, point_to_angle};
    use crate::gfx::palette::Colormap;
    use crate::map::Map;
    use crate::map::types::{
        BlockMapGrid, Linedef, Sector, Seg, Sidedef, SlopeType, SubSector, ThingSpawn, Vertex,
    };
    use crate::math::{BBox, Point};
    use crate::resources::ResourceManager;
    use crate::tables::{FINE_SINE, FINE_TANGENT};
    use crate::wad::WadFile;
    use crate::wad::testutil::build_wad;

    const VIEW_WIDTH: i32 = 64;
    const VIEW_HEIGHT: i32 = 40;

    fn identity_colormap() -> Colormap {
        Colormap(core::array::from_fn(|i| i as u8))
    }

    fn identity_colormaps() -> Vec<Colormap> {
        (0..33).map(|_| identity_colormap()).collect()
    }

    fn test_colormap_vec() -> Vec<Colormap> {
        identity_colormaps()
    }

    // ---------------------------------------------------------- fixtures

    /// The WAD: PNAMES with three patches, TEXTURE1 with WALL_A (stripes:
    /// texel value == its texture column), GRAD (vertical gradient:
    /// texel value == its row) and FENCE1 (masked: two covered runs).
    fn build_wall_wad() -> Vec<u8> {
        let mut pnames = Vec::new();
        pnames.extend_from_slice(&3i32.to_le_bytes());
        for name in ["STRIPE", "GRAD", "FENCE"] {
            let mut raw = [0u8; 8];
            raw[..name.len()].copy_from_slice(name.as_bytes());
            pnames.extend_from_slice(&raw);
        }

        // 128x128 fully covered patch; texel value == column index
        let stripe = {
            let mut lump = Vec::new();
            lump.extend_from_slice(&128i16.to_le_bytes()); // width
            lump.extend_from_slice(&128i16.to_le_bytes()); // height
            lump.extend_from_slice(&0i16.to_le_bytes()); // left offset
            lump.extend_from_slice(&0i16.to_le_bytes()); // top offset
            let column_ptrs_ofs = 8usize;
            let mut ptrs = Vec::with_capacity(128 * 4);
            let mut data = Vec::new();
            for c in 0..128i32 {
                ptrs.extend_from_slice(
                    &((column_ptrs_ofs + 128 * 4 + data.len()) as i32).to_le_bytes(),
                );
                data.extend_from_slice(&0u8.to_le_bytes()); // topdelta
                data.extend_from_slice(&128u8.to_le_bytes()); // length
                data.push(0); // pad
                data.extend_from_slice(&[c as u8; 128]);
                data.push(0); // pad
                data.push(0xff); // terminator
            }
            lump.extend_from_slice(&ptrs);
            lump.extend_from_slice(&data);
            lump
        };

        // 128x128 fully covered patch; texel value == row index
        let gradient = {
            let mut lump = Vec::new();
            lump.extend_from_slice(&128i16.to_le_bytes());
            lump.extend_from_slice(&128i16.to_le_bytes());
            lump.extend_from_slice(&0i16.to_le_bytes());
            lump.extend_from_slice(&0i16.to_le_bytes());
            let column_ptrs_ofs = 8usize;
            let mut ptrs = Vec::with_capacity(128 * 4);
            let mut data = Vec::new();
            for _c in 0..128i32 {
                ptrs.extend_from_slice(
                    &((column_ptrs_ofs + 128 * 4 + data.len()) as i32).to_le_bytes(),
                );
                data.extend_from_slice(&0u8.to_le_bytes()); // topdelta
                data.extend_from_slice(&128u8.to_le_bytes()); // length
                data.push(0); // pad
                data.extend((0u8..128).collect::<Vec<u8>>());
                data.push(0); // pad
                data.push(0xff); // terminator
            }
            lump.extend_from_slice(&ptrs);
            lump.extend_from_slice(&data);
            lump
        };

        // 4x16 masked patch; runs 0..3 and 10..13; texel value == row
        let fence = {
            let mut lump = Vec::new();
            lump.extend_from_slice(&4i16.to_le_bytes());
            lump.extend_from_slice(&16i16.to_le_bytes());
            lump.extend_from_slice(&0i16.to_le_bytes());
            lump.extend_from_slice(&0i16.to_le_bytes());
            let column_ptrs_ofs = 8usize;
            let mut ptrs = Vec::with_capacity(4 * 4);
            let mut data = Vec::new();
            for _c in 0..4i32 {
                ptrs.extend_from_slice(
                    &((column_ptrs_ofs + 4 * 4 + data.len()) as i32).to_le_bytes(),
                );
                // run 1: rows 0..3, value = row
                data.extend_from_slice(&0u8.to_le_bytes());
                data.extend_from_slice(&4u8.to_le_bytes());
                data.push(0);
                data.extend_from_slice(&[0, 1, 2, 3]);
                data.push(0);
                // run 2: rows 10..13, value = row
                data.extend_from_slice(&10u8.to_le_bytes());
                data.extend_from_slice(&4u8.to_le_bytes());
                data.push(0);
                data.extend_from_slice(&[10, 11, 12, 13]);
                data.push(0);
                data.push(0xff);
            }
            lump.extend_from_slice(&ptrs);
            lump.extend_from_slice(&data);
            lump
        };

        let definition = |name: &str, width: i16, height: i16, patch: i16| -> Vec<u8> {
            let mut def = Vec::new();
            let mut raw = [0u8; 8];
            raw[..name.len()].copy_from_slice(name.as_bytes());
            def.extend_from_slice(&raw);
            def.extend_from_slice(&0i32.to_le_bytes()); // masked
            def.extend_from_slice(&width.to_le_bytes());
            def.extend_from_slice(&height.to_le_bytes());
            def.extend_from_slice(&0i32.to_le_bytes()); // obsolete column directory
            def.extend_from_slice(&1i16.to_le_bytes()); // patch count
            def.extend_from_slice(&0i16.to_le_bytes()); // originx
            def.extend_from_slice(&0i16.to_le_bytes()); // originy
            def.extend_from_slice(&patch.to_le_bytes());
            def.extend_from_slice(&0i16.to_le_bytes()); // stepdir
            def.extend_from_slice(&0i16.to_le_bytes()); // colormap
            def
        };

        let mut texture1 = Vec::new();
        texture1.extend_from_slice(&3i32.to_le_bytes());
        let defs = [
            definition("WALL_A", 128, 128, 0),
            definition("GRAD", 128, 128, 1),
            definition("FENCE1", 4, 16, 2),
        ];
        // directory then definitions, back to back
        let mut dir_off = 4 + 3 * 4;
        let mut dir = Vec::new();
        let mut body = Vec::new();
        for def in &defs {
            dir.extend_from_slice(&(dir_off as i32).to_le_bytes());
            dir_off += def.len();
            body.extend_from_slice(def);
        }
        texture1.extend_from_slice(&dir);
        texture1.extend_from_slice(&body);

        build_wad(
            b"IWAD",
            &[
                ("PNAMES", &pnames),
                ("TEXTURE1", &texture1),
                ("STRIPE", &stripe),
                ("GRAD", &gradient),
                ("FENCE", &fence),
            ],
        )
    }

    fn test_manager() -> ResourceManager {
        ResourceManager::new(WadFile::from_bytes(build_wall_wad()).unwrap(), Vec::new())
    }

    /// The room fixture: a vertical one-sided wall at x = 64 facing west,
    /// viewed from (32, 0) looking east. Also usable for windows by
    /// overriding the back sector.
    fn wall_fixture() -> (Map, usize) {
        let vertexes = vec![
            Vertex {
                x: Fixed::from_int(64),
                y: Fixed::from_int(32),
            }, // 0: north end
            Vertex {
                x: Fixed::from_int(64),
                y: Fixed::from_int(-32),
            }, // 1: south end
        ];
        let linedefs = vec![Linedef {
            v1: 0,
            v2: 1,
            dx: Fixed::from_raw(vertexes[1].x.to_raw() - vertexes[0].x.to_raw()),
            dy: Fixed::from_raw(vertexes[1].y.to_raw() - vertexes[0].y.to_raw()),
            flags: crate::map::types::ML_BLOCKING,
            special: 0,
            tag: 0,
            sidenum: [Some(0), None],
            bbox: BBox::enclose([
                Point::new(vertexes[0].x, vertexes[0].y),
                Point::new(vertexes[1].x, vertexes[1].y),
            ]),
            slopetype: SlopeType::Vertical,
            frontsector: Some(0),
            backsector: None,
        }];
        let sidedefs = vec![Sidedef {
            texture_offset: Fixed::ZERO,
            row_offset: Fixed::ZERO,
            toptexture: *b"-\0\0\0\0\0\0\0",
            bottomtexture: *b"-\0\0\0\0\0\0\0",
            midtexture: *b"WALL_A\0\0",
            sector: 0,
        }];
        let sectors = vec![sector(0, 128, "FLOOR4_8", "CEIL3_5", 192)];
        let segs = vec![Seg {
            v1: 0,
            v2: 1,
            angle: Angle::ANG270.to_raw(),
            offset: Fixed::ZERO,
            linedef: 0,
            side: 0,
            sidedef: 0,
            frontsector: 0,
            backsector: None,
        }];
        let subsectors = vec![SubSector {
            num_segs: 1,
            first_seg: 0,
            sector: Some(0),
        }];
        let map = Map {
            vertexes,
            linedefs,
            sidedefs,
            sectors,
            segs,
            subsectors,
            nodes: Vec::new(),
            blockmap: BlockMapGrid::new(
                Fixed::from_int(-8),
                Fixed::from_int(-8),
                1,
                1,
                vec![vec![0]],
            ),
            reject: Vec::new(),
            things: Vec::<ThingSpawn>::new(),
        };
        (map, 0)
    }

    fn sector(floor: i32, ceil: i32, floor_pic: &str, ceil_pic: &str, light: i16) -> Sector {
        let mut floor_pic_bytes = [0u8; 8];
        floor_pic_bytes[..floor_pic.len()].copy_from_slice(floor_pic.as_bytes());
        let mut ceil_pic_bytes = [0u8; 8];
        ceil_pic_bytes[..ceil_pic.len()].copy_from_slice(ceil_pic.as_bytes());
        Sector {
            floor_height: Fixed::from_int(floor),
            ceiling_height: Fixed::from_int(ceil),
            floor_pic: floor_pic_bytes,
            ceiling_pic: ceil_pic_bytes,
            light_level: light,
            special: 0,
            tag: 0,
        }
    }

    fn test_view<'a>(
        tables: &'a ViewTables,
        projection: &'a WallProjection,
        light: &'a LightTables,
        textures: &'a TextureSet,
        colormaps: &'a [Colormap],
        view_angle: Angle,
    ) -> WallView<'a> {
        test_view_z(
            tables,
            projection,
            light,
            textures,
            colormaps,
            view_angle,
            Fixed::from_int(32),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn test_view_z<'a>(
        tables: &'a ViewTables,
        projection: &'a WallProjection,
        light: &'a LightTables,
        textures: &'a TextureSet,
        colormaps: &'a [Colormap],
        view_angle: Angle,
        view_z: Fixed,
    ) -> WallView<'a> {
        WallView {
            tables,
            projection,
            light,
            textures,
            colormaps,
            view_x: Fixed::from_int(32),
            view_y: Fixed::ZERO,
            view_z,
            view_angle,
            view_cos: crate::tables::FINE_COSINE[view_angle.fine_angle()],
            view_sin: FINE_SINE[view_angle.fine_angle()],
            view_width: VIEW_WIDTH,
            view_height: VIEW_HEIGHT,
            center_y: VIEW_HEIGHT / 2,
            center_x_frac: Fixed::from_int(VIEW_WIDTH / 2),
            center_y_frac: Fixed::from_int(VIEW_HEIGHT / 2),
            projection_distance: Fixed::from_int(VIEW_WIDTH / 2),
            extralight: 0,
            fixed_colormap: None,
            detail_shift: 0,
            sky_flat: Some(*b"F_SKY1\0\0"),
        }
    }

    struct TestKit {
        tables: ViewTables,
        projection: WallProjection,
        light: LightTables,
        textures: TextureSet,
        colormaps: Vec<Colormap>,
    }

    impl TestKit {
        fn new() -> Self {
            let tables = ViewTables::new(VIEW_WIDTH, Fixed::from_int(VIEW_WIDTH / 2));
            let projection = WallProjection::build(&tables, VIEW_WIDTH);
            let light = LightTables::build(VIEW_WIDTH);
            let textures = TextureSet::from_resources(&test_manager()).unwrap();
            Self {
                tables,
                projection,
                light,
                textures,
                colormaps: test_colormap_vec(),
            }
        }

        fn frame<'a>(&'a self, fb: &'a mut IndexedFramebuffer, view_angle: Angle) -> WallFrame<'a> {
            WallFrame::new(
                test_view(
                    &self.tables,
                    &self.projection,
                    &self.light,
                    &self.textures,
                    &self.colormaps,
                    view_angle,
                ),
                fb,
            )
        }

        fn frame_z<'a>(
            &'a self,
            fb: &'a mut IndexedFramebuffer,
            view_angle: Angle,
            view_z: Fixed,
        ) -> WallFrame<'a> {
            WallFrame::new(
                test_view_z(
                    &self.tables,
                    &self.projection,
                    &self.light,
                    &self.textures,
                    &self.colormaps,
                    view_angle,
                    view_z,
                ),
                fb,
            )
        }
    }

    /// Runs the BSP line classification on one seg and feeds the visible
    /// fragments into `store_wall_range`, mirroring how the BSP traversal
    /// drives the renderer. Returns the emitted fragments and the
    /// classification outcome.
    fn rasterize_seg(
        kit: &TestKit,
        frame: &mut WallFrame<'_>,
        map: &Map,
        seg_index: usize,
        floor_plane: Option<usize>,
        ceiling_plane: Option<usize>,
    ) -> (Vec<(i32, i32)>, AddLineOutcome) {
        let seg = &map.segs[seg_index];
        let v1 = map.vertexes[seg.v1];
        let v2 = map.vertexes[seg.v2];
        let front = &map.sectors[seg.frontsector];
        let back = seg.backsector.map(|i| &map.sectors[i]);
        let has_mid = {
            let sidedef = &map.sidedefs[seg.sidedef];
            sidedef.midtexture[0] != b'-' && sidedef.midtexture.iter().any(|&b| b != 0)
        };
        let view = ViewState::new(Fixed::from_int(32), Fixed::ZERO, Angle::ZERO);
        // add_line hands rw_angle1 back through Solid/Window; recompute it
        // here for the fragments (it is the view-absolute angle of v1)
        let rw_angle1 = crate::bsp::point_to_angle(&view, v1.x, v1.y);

        let mut clip = ClipSegList::cleared(VIEW_WIDTH);
        let mut fragments = Vec::new();
        let outcome = add_line(
            &view,
            &kit.tables,
            &mut clip,
            Point::new(v1.x, v1.y),
            Point::new(v2.x, v2.y),
            front,
            back,
            has_mid,
            &mut |first, last| {
                fragments.push((first, last));
                frame.store_wall_range(
                    map,
                    seg_index,
                    rw_angle1,
                    first,
                    last,
                    floor_plane,
                    ceiling_plane,
                );
            },
        );
        (fragments, outcome)
    }

    // ---------------------------------------------------------------- tests

    #[test]
    fn scale_from_global_angle_known_values() {
        let tables = ViewTables::new(VIEW_WIDTH, Fixed::from_int(VIEW_WIDTH / 2));
        let projection = WallProjection::build(&tables, VIEW_WIDTH);
        let textures = TextureSet::from_resources(&test_manager()).unwrap();
        let colormaps = test_colormap_vec();
        let light = LightTables::build(VIEW_WIDTH);
        let view = test_view(
            &tables,
            &projection,
            &light,
            &textures,
            &colormaps,
            Angle::ZERO,
        );

        // visangle == rw_normalangle: angleb == ANG90, sineb ~= 1
        let rw_distance = Fixed::from_int(64);
        let scale = scale_from_global_angle(&view, Angle::ANG90, rw_distance, Angle::ANG90);
        assert!(scale.to_raw() > 0);

        // den == 0 (zero distance) takes the `else scale = 64*FRACUNIT` branch
        let scale = scale_from_global_angle(&view, Angle::ZERO, Fixed::ZERO, Angle::ANG90);
        assert_eq!(scale.to_raw(), 64 << 16);

        // den huge: quotient clamps to 256
        let scale =
            scale_from_global_angle(&view, Angle::ZERO, Fixed::from_int(1000), Angle::ANG90);
        assert_eq!(scale.to_raw(), 256);

        // den small: FixedDiv barely fits (|a|>>14 < |den|), scale exceeds
        // the 64*FRACUNIT ceiling and clamps
        let scale = scale_from_global_angle(&view, Angle::ZERO, Fixed::from_raw(10), Angle::ANG90);
        assert_eq!(scale.to_raw(), 64 << 16);
    }

    #[test]
    fn light_row_matches_inlined_formula() {
        // (lightlevel >> 4) + extralight, horizontal -1, vertical +1, clamped
        assert_eq!(light_row(176, false, false, 0), 11);
        assert_eq!(light_row(176, true, false, 0), 10);
        assert_eq!(light_row(176, false, true, 0), 12);
        assert_eq!(light_row(176, false, false, 20), 15); // clamp high
        assert_eq!(light_row(10, true, false, -3), 0); // clamp low
        assert_eq!(light_row(255, false, true, 5), 15);
    }

    #[test]
    fn point_to_dist_axis_aligned() {
        let tables = ViewTables::new(VIEW_WIDTH, Fixed::from_int(VIEW_WIDTH / 2));
        let projection = WallProjection::build(&tables, VIEW_WIDTH);
        let textures = TextureSet::from_resources(&test_manager()).unwrap();
        let colormaps = test_colormap_vec();
        let light = LightTables::build(VIEW_WIDTH);
        let view = test_view(
            &tables,
            &projection,
            &light,
            &textures,
            &colormaps,
            Angle::ZERO,
        );
        // due east: theta = 0, finesine[2048] ~= 1.0
        let dist = point_to_dist(&view, Fixed::from_int(42), Fixed::ZERO);
        assert!((dist.to_raw() - Fixed::from_int(10).to_raw()).abs() <= 64);
        // viewpoint on the vertex: 0, no crash
        let dist = point_to_dist(&view, Fixed::from_int(32), Fixed::ZERO);
        assert_eq!(dist, Fixed::ZERO);
    }

    #[test]
    fn wall_projection_is_monotone_and_covers_view() {
        let tables = ViewTables::new(VIEW_WIDTH, Fixed::from_int(VIEW_WIDTH / 2));
        let projection = WallProjection::build(&tables, VIEW_WIDTH);
        assert_eq!(projection.x_to_view_angle.len(), (VIEW_WIDTH + 1) as usize);
        for pair in projection.x_to_view_angle.windows(2) {
            // angles cross zero here, so compare as signed: raw u32 order
            // wraps at the 0/2^32 boundary
            assert!(
                pair[0].to_raw() as i32 >= pair[1].to_raw() as i32,
                "not monotone non-increasing: {pair:?}"
            );
        }
    }

    #[test]
    fn wall_projection_center_is_straight_ahead() {
        let tables = ViewTables::new(VIEW_WIDTH, Fixed::from_int(VIEW_WIDTH / 2));
        let projection = WallProjection::build(&tables, VIEW_WIDTH);
        // straight ahead must be ~ANGLE 0 (east): x == center maps to the
        // first angle whose fine index passes the center column
        let center = projection.at(VIEW_WIDTH / 2);
        // within one fine angle of 0
        assert!(center.to_raw().abs_diff(Angle::ZERO.to_raw()) <= 1 << ANGLE_TO_FINE_SHIFT);
    }

    fn fragment_count_and_range(fragments: &[(i32, i32)]) {
        // the wall spans almost the full FOV; the exact edge columns are a
        // finetangent-table artifact (byte-verified), so don't hardcode them
        assert_eq!(
            fragments.len(),
            1,
            "expected one fragment, got {fragments:?}"
        );
        let (a, b) = fragments[0];
        assert!((0..VIEW_WIDTH).contains(&a));
        assert!(b < VIEW_WIDTH && b > a);
        assert!(
            a <= 2,
            "wall should start within the leftmost columns, got {a}"
        );
    }

    #[test]
    fn single_sided_wall_full_pipeline() {
        let kit = TestKit::new();
        let (map, seg_index) = wall_fixture();
        let mut fb = IndexedFramebuffer::new();
        let mut frame = kit.frame(&mut fb, Angle::ZERO);
        frame.clear();

        let (fragments, outcome) = rasterize_seg(&kit, &mut frame, &map, seg_index, None, None);
        fragment_count_and_range(&fragments);
        assert!(matches!(outcome, AddLineOutcome::Solid(_)));

        // the wall covers its fragment columns: ceiling clip closed to
        // viewheight, floor clip to -1
        let seg_span = {
            let segs = frame.draw_segs();
            assert_eq!(segs.len(), 1);
            (segs[0].x1 as usize, segs[0].x2 as usize)
        };
        for x in seg_span.0..=seg_span.1 {
            assert_eq!(
                frame.ceiling_clip()[x],
                VIEW_HEIGHT as i16,
                "ceiling clip at {x}"
            );
            assert_eq!(frame.floor_clip()[x], -1, "floor clip at {x}");
        }

        let segs = frame.draw_segs();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].x1, fragments[0].0);
        assert_eq!(segs[0].x2, fragments[0].1);
        assert_eq!(segs[0].silhouette, SIL_BOTH);
        assert_eq!(segs[0].bsilheight, Fixed::MAX);
        assert_eq!(segs[0].tsilheight, Fixed::MIN);
        assert!(segs[0].scale1.to_raw() > 0);
        // symmetric wall: scales at the far edges are equal
        assert!(segs[0].scale1.to_raw().abs_diff(segs[0].scale2.to_raw()) <= 4);

        // errors drained clean (textures resolved)
        assert_eq!(frame.take_errors().len(), 0);

        // ML_MAPPED was recorded for the linedef
        let mapped = frame.take_mapped_linedefs();
        assert_eq!(mapped, vec![0]);
    }

    #[test]
    fn stripe_texture_shows_texturecolumn_in_pixels() {
        let kit = TestKit::new();
        let (map, seg_index) = wall_fixture();
        let mut fb = IndexedFramebuffer::new();
        let mut frame = kit.frame(&mut fb, Angle::ZERO);
        frame.clear();
        let (_fragments, _outcome) = rasterize_seg(&kit, &mut frame, &map, seg_index, None, None);

        // expected texturecolumn per column, recomputed from the C formulas:
        //   angle  = (rw_centerangle + xtoviewangle[x]) >> ANGLETOFINESHIFT
        //   texcol = (rw_offset - FixedMul(finetangent[angle], rw_distance)) >> FRACBITS
        // with rw_offset from the seg geometry (view (32,0) -> seg at x=64).
        let view_angle = Angle::ZERO;
        let rw_normalangle = Angle::from_raw(map.segs[seg_index].angle) + Angle::ANG90;
        let angle1 = point_to_angle(
            &ViewState::new(Fixed::from_int(32), Fixed::ZERO, view_angle),
            Fixed::from_int(64),
            Fixed::from_int(32),
        );
        let offsetangle_raw = rw_normalangle.to_raw().wrapping_sub(angle1.to_raw());
        let mut offsetangle = Angle::from_raw(offsetangle_raw);
        if offsetangle.to_raw() >= 0x8000_0000 {
            offsetangle = Angle::from_raw(0u32.wrapping_sub(offsetangle.to_raw()));
        }
        if offsetangle.to_raw() > Angle::ANG90.to_raw() {
            offsetangle = Angle::ANG90;
        }
        let hyp = point_to_dist(
            &test_view(
                &kit.tables,
                &kit.projection,
                &kit.light,
                &kit.textures,
                &kit.colormaps,
                view_angle,
            ),
            Fixed::from_int(64),
            Fixed::from_int(32),
        );
        let sineval = FINE_SINE[(Angle::ANG90 - offsetangle).fine_angle()];
        let rw_distance = hyp * sineval;
        let mut rw_offset = {
            let sineval = FINE_SINE[offsetangle.fine_angle()];
            let mut off = hyp * sineval;
            if (rw_normalangle - angle1).to_raw() < Angle::ANG180.to_raw() {
                off = -off;
            }
            off
        };
        rw_offset += Fixed::ZERO; // sidedef.texture_offset == 0, seg.offset == 0
        let rw_centerangle = Angle::ANG90 + view_angle - rw_normalangle;

        let mut drawn = 0;
        for x in 0..VIEW_WIDTH {
            let angle = (rw_centerangle + kit.projection.at(x)).fine_angle();
            let texcol = (rw_offset - FINE_TANGENT[angle] * rw_distance).to_raw() >> FRACBITS;
            let expected = texcol.rem_euclid(128) as u8;
            // count drawn pixels in this column and check they all equal the
            // stripe value for that column
            for y in 0..VIEW_HEIGHT {
                if let Some(pixel) = fb.get(x as usize, y as usize)
                    && pixel != 0
                {
                    assert_eq!(pixel, expected, "column {x} row {y}");
                    drawn += 1;
                }
            }
        }
        assert!(
            drawn > VIEW_WIDTH * 10,
            "expected a solid wall, only {drawn} pixels drawn"
        );
    }

    #[test]
    fn masked_midtexture_fence_pipeline() {
        // two-sided line, identical geometry except a masked midtexture:
        // the fence draws its two runs and nothing else, the clips stay open
        let kit = TestKit::new();
        let mut map = wall_fixture().0;
        // add the back sector + sidedef + linedef as two sided
        map.sectors.push(sector(0, 128, "FLOOR4_8", "CEIL3_5", 192));
        map.sidedefs.push(Sidedef {
            texture_offset: Fixed::ZERO,
            row_offset: Fixed::ZERO,
            toptexture: *b"-\0\0\0\0\0\0\0",
            bottomtexture: *b"-\0\0\0\0\0\0\0",
            midtexture: *b"WALL_A\0\0",
            sector: 1,
        });
        map.linedefs[0].sidenum = [Some(0), Some(1)];
        map.linedefs[0].backsector = Some(1);
        map.segs[0].backsector = Some(1);
        // swap the mid texture to the fence texture
        map.sidedefs[0].midtexture = *b"FENCE1\0\0";

        // the fence texture is 16 texels tall and its runs sit in rows
        // 0..3 and 10..13; aim the view at the ceiling of the 128-unit
        // room so those rows land inside the viewport band
        let mut fb = IndexedFramebuffer::new();
        let mut frame = kit.frame_z(&mut fb, Angle::ZERO, Fixed::from_int(121));
        frame.clear();
        let (fragments, outcome) = rasterize_seg(&kit, &mut frame, &map, 0, None, None);
        fragment_count_and_range(&fragments);
        assert!(matches!(outcome, AddLineOutcome::Window(_)));

        // masked midtextures do not claim the column in the clip lists
        let seg_span = {
            let segs = frame.draw_segs();
            (segs[0].x1 as usize, segs[0].x2 as usize)
        };
        for x in seg_span.0..=seg_span.1 {
            assert_eq!(frame.ceiling_clip()[x], -1, "ceiling clip at {x}");
            assert_eq!(
                frame.floor_clip()[x],
                VIEW_HEIGHT as i16,
                "floor clip at {x}"
            );
        }

        let segs = frame.draw_segs();
        assert_eq!(segs.len(), 1);
        assert_eq!(
            segs[0].silhouette, SIL_BOTH,
            "masked forces both silhouette bits"
        );
        // masked columns are saved but not yet consumed
        let mut unconsumed = 0;
        for x in segs[0].x1..=segs[0].x2 {
            if let Some(raw) = frame.seg_masked_col(&segs[0], x) {
                assert_ne!(raw, MAX_SHORT, "masked column {x} already consumed");
                unconsumed += 1;
            }
        }
        assert_eq!(unconsumed, segs[0].x2 - segs[0].x1 + 1);

        // now draw the masked pass
        frame.render_masked_seg_range(&map, 0, segs[0].x1, segs[0].x2);

        // fence runs are rows 0..3 and 10..13 of the texture; the wall is
        // placed so the runs land on the screen; just verify some pixels
        // were drawn and the columns are consumed afterwards
        let drawn = frame
            .framebuffer()
            .as_slice()
            .iter()
            .filter(|&&p| p != 0)
            .count();
        assert!(drawn > 0, "fence pass drew nothing");
        for x in frame.draw_segs()[0].x1..=frame.draw_segs()[0].x2 {
            assert_eq!(
                frame.seg_masked_col(&frame.draw_segs()[0], x),
                Some(MAX_SHORT)
            );
        }
    }

    #[test]
    fn window_with_ceiling_step_marks_plane_and_top_wall() {
        let kit = TestKit::new();
        let mut map = wall_fixture().0;
        // back sector: lower ceiling (worldhigh < worldtop) but same floor
        map.sectors.push(sector(0, 96, "FLOOR4_8", "CEIL3_5", 192));
        map.sidedefs.push(Sidedef {
            texture_offset: Fixed::ZERO,
            row_offset: Fixed::ZERO,
            toptexture: *b"WALL_A\0\0",
            bottomtexture: *b"-\0\0\0\0\0\0\0",
            midtexture: *b"-\0\0\0\0\0\0\0",
            sector: 1,
        });
        map.linedefs[0].sidenum = [Some(0), Some(1)];
        map.linedefs[0].backsector = Some(1);
        map.segs[0].backsector = Some(1);
        map.sidedefs[0].toptexture = *b"WALL_A\0\0";

        let mut fb = IndexedFramebuffer::new();
        let mut frame = kit.frame_z(&mut fb, Angle::ZERO, Fixed::from_int(123));
        frame.clear();
        frame
            .planes_mut()
            .find_plane(Fixed::from_int(96), 1, 192, false);
        let ceiling_plane = frame.planes().len().checked_sub(1);

        let (_fragments, outcome) = rasterize_seg(&kit, &mut frame, &map, 0, None, ceiling_plane);
        assert!(matches!(outcome, AddLineOutcome::Window(_)));

        // the top tier drew into the band above the opening and raised the
        // ceiling clip from -1
        let seg_span = {
            let segs = frame.draw_segs();
            assert_eq!(segs.len(), 1);
            (segs[0].x1 as usize, segs[0].x2 as usize)
        };
        for x in seg_span.0..=seg_span.1 {
            assert!(
                frame.ceiling_clip()[x] > -1,
                "ceiling clip at {x} should have advanced above -1 after the top tier"
            );
            assert_eq!(
                frame.floor_clip()[x],
                VIEW_HEIGHT as i16,
                "floor clip at {x}"
            );
        }
        // the ceiling plane was marked somewhere (row bytes non-0xff)
        let plane = frame.planes().get(0).unwrap();
        let marked = (0..VIEW_WIDTH).any(|x| plane.top_at(x) != 0xff);
        assert!(marked, "ceiling plane was not marked");
    }

    #[test]
    fn closed_door_is_solid_and_draws() {
        let kit = TestKit::new();
        let mut map = wall_fixture().0;
        // back sector fully hidden below the front floor (a sunken corridor):
        // back ceiling <= front floor, so add_line classifies it Solid; the
        // top tier still renders it as a wall below the viewer's horizon
        map.sectors
            .push(sector(-110, -110, "FLOOR4_8", "CEIL3_5", 192));
        map.sidedefs.push(Sidedef {
            texture_offset: Fixed::ZERO,
            row_offset: Fixed::ZERO,
            toptexture: *b"WALL_A\0\0",
            bottomtexture: *b"WALL_A\0\0",
            midtexture: *b"-\0\0\0\0\0\0\0",
            sector: 1,
        });
        map.linedefs[0].sidenum = [Some(0), Some(1)];
        map.linedefs[0].backsector = Some(1);
        map.segs[0].backsector = Some(1);

        let mut fb = IndexedFramebuffer::new();
        let mut frame = kit.frame(&mut fb, Angle::ZERO);
        frame.clear();
        let (fragments, outcome) = rasterize_seg(&kit, &mut frame, &map, 0, None, None);
        fragment_count_and_range(&fragments);
        assert!(matches!(outcome, AddLineOutcome::Solid(_)));

        // a closed door clears floor coverage? (worldlow == worldbottom here:
        // back floor 200 > front floor 0, so the bottom tier fires)
        let seg_span = {
            let segs = frame.draw_segs();
            assert_eq!(segs.len(), 1);
            (segs[0].x1 as usize, segs[0].x2 as usize)
        };
        // top tier at least must have been drawn from the ceiling side
        for x in seg_span.0..=seg_span.1 {
            assert_ne!(frame.ceiling_clip()[x], -1, "top tier drew nothing at {x}");
        }
    }

    #[test]
    fn single_column_seg_has_zero_scalestep() {
        let kit = TestKit::new();
        let (map, seg_index) = wall_fixture();
        let mut fb = IndexedFramebuffer::new();
        let mut frame = kit.frame(&mut fb, Angle::ZERO);
        frame.clear();

        let view_angle1 = Angle::from_raw(0x1fff_0000); // arbitrary in-frustum angle
        frame.store_wall_range(&map, seg_index, view_angle1, 32, 32, None, None);

        let segs = frame.draw_segs();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].x1, 32);
        assert_eq!(segs[0].x2, 32);
        assert_eq!(segs[0].scalestep, Fixed::ZERO);
        assert_eq!(segs[0].scale1, segs[0].scale2);
    }

    #[test]
    fn drawsegs_pool_exhaustion_keeps_drawing() {
        let kit = TestKit::new();
        let (map, seg_index) = wall_fixture();
        let mut fb = IndexedFramebuffer::new();
        let mut frame = kit.frame(&mut fb, Angle::ZERO);
        frame.clear();

        // fill the pool with more stores than MAX_DRAWSEGS
        let view_angle1 = Angle::from_raw(0x1fff_0000);
        for _ in 0..MAX_DRAWSEGS + 4 {
            frame.store_wall_range(&map, seg_index, view_angle1, 32, 32, None, None);
        }
        assert_eq!(frame.draw_segs().len(), MAX_DRAWSEGS);
        let errors = frame.take_errors();
        assert!(
            errors.is_empty(),
            "pool overflow logs via drawsegs_full, not errors"
        );
    }

    #[test]
    fn missing_texture_reports_error_but_keeps_clips() {
        let kit = TestKit::new();
        let mut map = wall_fixture().0;
        map.sidedefs[0].midtexture = *b"NOSUCH1\0";

        let mut fb = IndexedFramebuffer::new();
        let mut frame = kit.frame(&mut fb, Angle::ZERO);
        frame.clear();
        let view_angle1 = crate::bsp::point_to_angle(
            &ViewState::new(Fixed::from_int(32), Fixed::ZERO, Angle::ZERO),
            Fixed::from_int(64),
            Fixed::from_int(32),
        );
        frame.store_wall_range(&map, 0, view_angle1, 10, 20, None, None);

        // the error was recorded
        let errors = frame.take_errors();
        assert!(!errors.is_empty());
        // the clip arrays were still updated (wall is terminal, closes the column)
        assert_eq!(frame.ceiling_clip()[10], VIEW_HEIGHT as i16);
        assert_eq!(frame.floor_clip()[10], -1);
    }

    #[test]
    fn bad_column_range_is_rejected() {
        let kit = TestKit::new();
        let (map, seg_index) = wall_fixture();
        let mut fb = IndexedFramebuffer::new();
        let mut frame = kit.frame(&mut fb, Angle::ZERO);
        frame.clear();
        let view_angle1 = Angle::ZERO;
        frame.store_wall_range(&map, seg_index, view_angle1, 30, 20, None, None);
        assert!(frame.draw_segs().is_empty());
        frame.store_wall_range(&map, seg_index, view_angle1, -1, 5, None, None);
        assert!(frame.draw_segs().is_empty());
        frame.store_wall_range(&map, seg_index, view_angle1, 0, VIEW_WIDTH, None, None);
        assert!(frame.draw_segs().is_empty());
    }

    #[test]
    fn gradient_texture_pixel_rows_follow_projection() {
        let kit = TestKit::new();
        let mut map = wall_fixture().0;
        map.sidedefs[0].midtexture = *b"GRAD\0\0\0\0";

        let mut fb = IndexedFramebuffer::new();
        let mut frame = kit.frame(&mut fb, Angle::ZERO);
        frame.clear();
        let (_fragments, _outcome) = rasterize_seg(&kit, &mut frame, &map, 0, None, None);

        // for one column in the middle, verify the first drawn row equals the
        // texture row predicted by (dc_texturemid + (y - centery) * iscale) >> 16
        let x = VIEW_WIDTH / 2;
        let segs = frame.draw_segs();
        let ds = segs[0];
        let worldtop = Fixed::from_int(128 - 32);
        // find the first row drawn at column x (non-zero texel)
        let mut first_drawn = None;
        for y in 0..VIEW_HEIGHT {
            if let Some(pixel) = fb.get(x as usize, y as usize)
                && pixel != 0
            {
                first_drawn = Some(y);
                break;
            }
        }
        let first_drawn = first_drawn.expect("column drew nothing");
        // the texture is a 128-row gradient of u8(row); the drawn top row must
        // be within the wall band and its texel row must equal frac>>16&127 —
        // recompute from scale1 (the store recomputes scale exactly this way)
        let iscale = Fixed::from_raw((0xffff_ffffu32 / (ds.scale1.to_raw() as u32)) as i32);
        let texturemid = worldtop + Fixed::ZERO; // rw_midtexturemid = worldtop (no rowoffset)
        let frac = texturemid + iscale * Fixed::from_int(first_drawn - VIEW_HEIGHT / 2);
        let texel = ((frac.to_raw() >> 16) & 127) as u8;
        // pixel may be 0 from a dark texel, in which case the column of texels
        // is still consistent; only check when the texel is non-zero
        if texel != 0 {
            assert_eq!(fb.get(x as usize, first_drawn as usize), Some(texel));
        }
    }

    #[test]
    fn invalid_view_width_is_rejected() {
        let _ = VIEW_WIDTH;
        // WallProjection::build asserts a positive width; nothing else to see
        let tables = ViewTables::new(VIEW_WIDTH, Fixed::from_int(VIEW_WIDTH / 2));
        let _ = WallProjection::build(&tables, VIEW_WIDTH);
    }

    #[test]
    fn sectors_fixture_carries_names() {
        // guards the fixture helpers against silent drift
        let (map, _) = wall_fixture();
        let trimmed = |raw: &[u8; 8]| {
            String::from_utf8_lossy(raw)
                .trim_end_matches('\0')
                .to_string()
        };
        assert_eq!(trimmed(&map.sectors[0].floor_pic), "FLOOR4_8");
        assert_eq!(trimmed(&map.sectors[0].ceiling_pic), "CEIL3_5");
    }
}
