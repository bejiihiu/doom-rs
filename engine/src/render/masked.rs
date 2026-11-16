//! The `R_DrawMasked` driver: back-to-front vissprite rendering with
//! drawseg silhouette clipping, plus the masked mid-texture tail.
//!
//! Ported from `r_things.c` (`R_ClearSprites`, `R_SortVisSprites`,
//! `R_DrawSprite`, `R_DrawMasked`). The per-frame state that vanilla keeps
//! in globals (`vissprites[]`, `fuzzpos`, `translationtables`) lives in
//! [`MaskedRenderer`]; create one per frame cycle, call [`MaskedRenderer::clear`]
//! at the top of the frame (while the BSP traversal runs), call
//! [`MaskedRenderer::project`] per visible thing (the `R_AddSprites`
//! equivalent), then [`MaskedRenderer::draw_masked`] after the wall and
//! flat passes.
//!
//! Weapon/player sprites (`R_DrawPlayerSprites`) are intentionally not
//! here yet: they need the PSP state machinery from the weapon agent.
//! When that lands it reuses the same projection and column drawing.

use std::sync::Arc;

use crate::errors::EngineError;
use crate::fixed::Fixed;
use crate::gfx::patch::Patch;
use crate::map::Map;
use crate::math::{Divline, point_on_divline_side};
use crate::render::lighting::MAXLIGHTSCALE;

use super::framebuffer::SCREEN_WIDTH;
use super::sprite::{
    MAX_VIS_SPRITES, ProjectedSprite, SpriteDefinitions, SpriteLumpTables, SpriteThing,
    TranslationTables, draw_vis_sprite, project_sprite, sort_vissprites,
};
use super::wall::{SIL_BOTTOM, SIL_NONE, SIL_TOP, WallFrame, WallView};

/// What happened during one `draw_masked` call. The original reports
/// nothing (it had no error path at all); we count and collect so the
/// frame loop can log and tests can assert.
#[derive(Default, Debug)]
pub struct MaskedReport {
    /// Vissprites drawn (decode failures are skipped and reported in
    /// `errors`, not counted here).
    pub sprites_drawn: u32,
    /// Masked mid-texture segs rendered by the final sweep.
    pub masked_segs_drawn: u32,
    /// Sprites dropped because the vissprite pool was exhausted
    /// (the original silently discarded these).
    pub vissprites_overflowed: u32,
    /// Non-fatal errors (patch decode failures, ...).
    pub errors: Vec<EngineError>,
}

/// The per-frame sprite/masked pass state.
///
/// Not `Clone` on purpose: it owns the (large) fuzz state and the
/// translation tables, and the frame flow is single-threaded and linear.
pub struct MaskedRenderer {
    vissprites: Vec<ProjectedSprite>,
    fuzz: super::column::FuzzState,
    translations: TranslationTables,
    overflowed: u32,
}

impl Default for MaskedRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl MaskedRenderer {
    /// Fresh state, fuzz position at 0. (The original never resets
    /// `fuzzpos`, but resetting keeps the frame output deterministic,
    /// which the engine wants.)
    pub fn new() -> Self {
        Self {
            vissprites: Vec::with_capacity(MAX_VIS_SPRITES),
            fuzz: super::column::FuzzState::new(),
            translations: TranslationTables::build(),
            overflowed: 0,
        }
    }

    /// `R_ClearSprites`: drop the vissprite list, reset the overflow
    /// counter. Call once per frame before the BSP traversal runs.
    pub fn clear(&mut self) {
        self.vissprites.clear();
        self.overflowed = 0;
    }

    /// The projected sprites collected this frame (pre-draw order).
    pub fn vissprites(&self) -> &[ProjectedSprite] {
        &self.vissprites
    }

    /// How many sprites were dropped this frame for pool exhaustion.
    pub fn overflowed(&self) -> u32 {
        self.overflowed
    }

    /// `R_ProjectSprite` + list insert (the `R_NewVisSprite` pool with
    /// `MAXVISSPRITES` capacity). `spritelights` is the sector's
    /// `scalelight` row; pick it with
    /// [`crate::render::lighting::select_row`] from the sector's
    /// `lightlevel` (plus `extralight`).
    ///
    /// Returns `true` when the thing was projected and added.
    pub fn project(
        &mut self,
        thing: &SpriteThing,
        view: &WallView<'_>,
        defs: &SpriteDefinitions,
        lumps: &SpriteLumpTables,
        spritelights: &[usize; MAXLIGHTSCALE],
    ) -> bool {
        let Some(sprite) = project_sprite(thing, view, defs, lumps, spritelights) else {
            return false;
        };
        if self.vissprites.len() >= MAX_VIS_SPRITES {
            // the original writes into a shared overflow vissprite that
            // is never drawn; we count and drop
            self.overflowed += 1;
            return false;
        }
        self.vissprites.push(sprite);
        true
    }

    /// Sort back-to-front (smallest scale first) and draw every collected
    /// vissprite, then render the drawsegs whose masked mid-texture
    /// columns were not consumed during the sprite pass (`R_DrawMasked`'s
    /// tail). Weapon sprites are not drawn here yet (see module docs).
    ///
    /// `patches` decodes a global lump index into a patch; production
    /// callers pass
    /// `|lump| resources.decode_cached::<Patch>(lump, decode_patch)`.
    pub fn draw_masked(
        &mut self,
        frame: &mut WallFrame<'_>,
        view: &WallView<'_>,
        map: &Map,
        patches: &mut dyn FnMut(usize) -> crate::errors::EngineResult<Arc<Patch>>,
    ) -> MaskedReport {
        let mut report = MaskedReport {
            vissprites_overflowed: self.overflowed,
            ..MaskedReport::default()
        };
        if view.colormaps.is_empty() {
            report.errors.push(EngineError::Resource(
                "no COLORMAP colormaps loaded; sprite pass skipped".into(),
            ));
            return report;
        }

        // the original sorts in R_DrawMasked; keep the same order here
        sort_vissprites(&mut self.vissprites);
        let sprites = std::mem::take(&mut self.vissprites);
        for sprite in &sprites {
            let patch = match patches(sprite.patch) {
                Ok(patch) => patch,
                Err(err) => {
                    report.errors.push(err);
                    continue;
                }
            };
            draw_sprite(
                frame,
                view,
                map,
                sprite,
                &patch,
                &mut self.fuzz,
                &self.translations,
            );
            report.sprites_drawn += 1;
        }
        // keep the buffer for the next frame; the list itself resets
        self.vissprites = sprites;
        self.vissprites.clear();

        // render any remaining masked mid textures, back to front
        let seg_count = frame.draw_segs().len();
        for ds_index in (0..seg_count).rev() {
            let ds = frame.draw_segs()[ds_index];
            if frame.seg_has_masked_col(&ds) {
                frame.render_masked_seg_range(map, ds_index, ds.x1, ds.x2);
                report.masked_segs_drawn += 1;
            }
        }

        report
    }
}

/// `R_DrawSprite`: clip one vissprite against the stored drawsegs (back to
/// front, keeping the innermost silhouette), then draw it.
#[allow(clippy::too_many_arguments)]
fn draw_sprite(
    frame: &mut WallFrame<'_>,
    view: &WallView<'_>,
    map: &Map,
    spr: &ProjectedSprite,
    patch: &Patch,
    fuzz: &mut super::column::FuzzState,
    translations: &TranslationTables,
) {
    let view_height = view.view_height;
    let mut clipbot = [-2i16; SCREEN_WIDTH];
    let mut cliptop = [-2i16; SCREEN_WIDTH];
    let seg_count = frame.draw_segs().len();

    // Scan drawsegs from end to start for obscuring segs.
    // The first drawseg that has a greater scale is the clip seg.
    for ds_index in (0..seg_count).rev() {
        let ds = frame.draw_segs()[ds_index];

        // determine if the drawseg obscures the sprite
        let has_masked = frame.seg_has_masked_col(&ds);
        if ds.x1 > spr.x2 || ds.x2 < spr.x1 || (ds.silhouette == SIL_NONE && !has_masked) {
            // does not cover sprite
            continue;
        }

        let r1 = ds.x1.max(spr.x1);
        let r2 = ds.x2.min(spr.x2);

        let (lowscale, scale) = if ds.scale1 > ds.scale2 {
            (ds.scale2, ds.scale1)
        } else {
            (ds.scale1, ds.scale2)
        };

        let obscures = scale < spr.scale
            || (lowscale < spr.scale && point_on_seg_side(spr.gx, spr.gy, map, ds.seg) == 0);
        if obscures {
            // masked mid texture?
            if has_masked {
                frame.render_masked_seg_range(map, ds_index, r1, r2);
            }
            // seg is behind sprite
            continue;
        }

        // clip this piece of the sprite
        let mut silhouette = ds.silhouette;

        if spr.gz >= ds.bsilheight {
            silhouette &= !SIL_BOTTOM;
        }

        if spr.gzt <= ds.tsilheight {
            silhouette &= !SIL_TOP;
        }

        if silhouette & SIL_BOTTOM != 0 {
            // bottom sil
            for x in r1..=r2 {
                let slot = &mut clipbot[x as usize];
                if *slot == -2 {
                    *slot = frame.seg_bottom_clip(&ds, x).unwrap_or(view_height as i16);
                }
            }
        }
        if silhouette & SIL_TOP != 0 {
            // top sil
            for x in r1..=r2 {
                let slot = &mut cliptop[x as usize];
                if *slot == -2 {
                    *slot = frame.seg_top_clip(&ds, x).unwrap_or(-1);
                }
            }
        }
    }

    // check for unclipped columns
    for x in spr.x1..=spr.x2 {
        if clipbot[x as usize] == -2 {
            clipbot[x as usize] = view_height as i16;
        }
        if cliptop[x as usize] == -2 {
            cliptop[x as usize] = -1;
        }
    }

    draw_vis_sprite(
        frame.framebuffer_mut(),
        fuzz,
        spr,
        patch,
        view.colormaps,
        translations,
        &clipbot,
        &cliptop,
        view.detail_shift,
        view.center_y,
        view.center_y_frac,
        view_height,
    );
}

/// `R_PointOnSegSide` for the sprite clip test: 0 = front, 1 = back.
fn point_on_seg_side(gx: Fixed, gy: Fixed, map: &Map, seg_index: usize) -> i32 {
    let seg = &map.segs[seg_index];
    let a = &map.vertexes[seg.v1];
    let b = &map.vertexes[seg.v2];
    let line = Divline {
        x: a.x,
        y: a.y,
        dx: b.x - a.x,
        dy: b.y - a.y,
    };
    point_on_divline_side(gx, gy, &line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bsp::ViewTables;
    use crate::bsp::{AddLineOutcome, ClipSegList, ViewState, add_line, point_to_angle};
    use crate::gfx::palette::Colormap;
    use crate::map::Map;
    use crate::map::types::{
        BlockMapGrid, Linedef, Sector, Seg, Sidedef, SlopeType, SubSector, ThingSpawn, Vertex,
    };
    use crate::math::{BBox, Point};
    use crate::render::WallProjection;
    use crate::render::framebuffer::IndexedFramebuffer;
    use crate::render::lighting::LightTables;
    use crate::resources::ResourceManager;
    use crate::tables::Angle;
    use crate::wad::WadFile;
    use crate::wad::testutil::build_wad;

    const VIEW_WIDTH: i32 = 64;
    const VIEW_HEIGHT: i32 = 40;

    fn identity_colormaps() -> Vec<Colormap> {
        (0..33)
            .map(|_| Colormap(core::array::from_fn(|i| i as u8)))
            .collect()
    }

    fn stripe_patch() -> Vec<u8> {
        // 128x128, texel value == column index
        let mut lump = Vec::new();
        lump.extend_from_slice(&128i16.to_le_bytes());
        lump.extend_from_slice(&128i16.to_le_bytes());
        lump.extend_from_slice(&0i16.to_le_bytes());
        lump.extend_from_slice(&0i16.to_le_bytes());
        let column_ptrs_ofs = 8usize;
        let mut ptrs = Vec::with_capacity(128 * 4);
        let mut data = Vec::new();
        for c in 0..128i32 {
            ptrs.extend_from_slice(
                &((column_ptrs_ofs + 128 * 4 + data.len()) as i32).to_le_bytes(),
            );
            data.extend_from_slice(&[0u8, 128, 0]);
            data.extend_from_slice(&[c as u8; 128]);
            data.extend_from_slice(&[0, 0xff]);
        }
        lump.extend_from_slice(&ptrs);
        lump.extend_from_slice(&data);
        lump
    }

    fn fence_patch() -> Vec<u8> {
        // 4x16 masked: runs 0..3 and 10..13, texel value == row
        let mut lump = Vec::new();
        lump.extend_from_slice(&4i16.to_le_bytes());
        lump.extend_from_slice(&16i16.to_le_bytes());
        lump.extend_from_slice(&0i16.to_le_bytes());
        lump.extend_from_slice(&0i16.to_le_bytes());
        let column_ptrs_ofs = 8usize;
        let mut ptrs = Vec::with_capacity(4 * 4);
        let mut data = Vec::new();
        for _c in 0..4i32 {
            ptrs.extend_from_slice(&((column_ptrs_ofs + 4 * 4 + data.len()) as i32).to_le_bytes());
            data.extend_from_slice(&[0, 4, 0]);
            data.extend_from_slice(&[0, 1, 2, 3]);
            data.push(0);
            data.extend_from_slice(&[10, 4, 0]);
            data.extend_from_slice(&[10, 11, 12, 13]);
            data.push(0);
            data.push(0xff);
        }
        lump.extend_from_slice(&ptrs);
        lump.extend_from_slice(&data);
        lump
    }

    fn sprite_patch(fill: u8) -> Vec<u8> {
        // 8x12, every column one post of solid `fill` texels
        let mut lump = Vec::new();
        lump.extend_from_slice(&8i16.to_le_bytes());
        lump.extend_from_slice(&12i16.to_le_bytes());
        lump.extend_from_slice(&4i16.to_le_bytes()); // left offset
        lump.extend_from_slice(&12i16.to_le_bytes()); // top offset
        let column_ptrs_ofs = 8usize;
        let mut ptrs = Vec::with_capacity(8 * 4);
        let mut data = Vec::new();
        for _c in 0..8i32 {
            ptrs.extend_from_slice(&((column_ptrs_ofs + 8 * 4 + data.len()) as i32).to_le_bytes());
            data.extend_from_slice(&[0, 12, 0]);
            data.extend_from_slice(&[fill; 12]);
            data.push(0);
            data.push(0xff);
        }
        lump.extend_from_slice(&ptrs);
        lump.extend_from_slice(&data);
        lump
    }

    fn build_test_wad() -> Vec<u8> {
        let mut pnames = Vec::new();
        pnames.extend_from_slice(&2i32.to_le_bytes());
        for name in ["STRIPE", "FENCE"] {
            let mut raw = [0u8; 8];
            raw[..name.len()].copy_from_slice(name.as_bytes());
            pnames.extend_from_slice(&raw);
        }

        let definition = |name: &str, width: i16, height: i16, patch: i16| -> Vec<u8> {
            let mut def = Vec::new();
            let mut raw = [0u8; 8];
            raw[..name.len()].copy_from_slice(name.as_bytes());
            def.extend_from_slice(&raw);
            def.extend_from_slice(&0i32.to_le_bytes()); // masked
            def.extend_from_slice(&width.to_le_bytes());
            def.extend_from_slice(&height.to_le_bytes());
            def.extend_from_slice(&0i32.to_le_bytes()); // columndirectory (obsolete)
            def.extend_from_slice(&1i16.to_le_bytes()); // one patch
            def.extend_from_slice(&0i16.to_le_bytes()); // originx
            def.extend_from_slice(&0i16.to_le_bytes()); // originy
            def.extend_from_slice(&patch.to_le_bytes());
            def.extend_from_slice(&0i16.to_le_bytes()); // stepdir
            def.extend_from_slice(&0i16.to_le_bytes()); // colormap
            def
        };

        let mut texture1 = Vec::new();
        texture1.extend_from_slice(&2i32.to_le_bytes());
        let defs = [
            definition("WALL_A", 128, 128, 0),
            definition("FENCE1", 4, 16, 1),
        ];
        let mut dir_off = 4 + 2 * 4;
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
                ("STRIPE", &stripe_patch()),
                ("FENCE", &fence_patch()),
                ("S_START", &[]),
                ("TESTA0", &sprite_patch(77)),
                ("S_END", &[]),
            ],
        )
    }

    fn manager() -> ResourceManager {
        ResourceManager::new(WadFile::from_bytes(build_test_wad()).unwrap(), Vec::new())
    }

    struct Kit {
        tables: ViewTables,
        projection: WallProjection,
        light: LightTables,
        textures: crate::render::TextureSet,
        colormaps: Vec<Colormap>,
    }

    impl Kit {
        fn new(manager: &ResourceManager) -> Self {
            let tables = ViewTables::new(VIEW_WIDTH, Fixed::from_int(VIEW_WIDTH / 2));
            let projection = WallProjection::build(&tables, VIEW_WIDTH);
            Self {
                tables,
                projection,
                light: LightTables::build(VIEW_WIDTH),
                textures: crate::render::TextureSet::from_resources(manager).unwrap(),
                colormaps: identity_colormaps(),
            }
        }

        /// the wall-fixture view: (32, 0) facing east, z from argument
        fn view_z<'a>(&'a self, view_z: Fixed) -> WallView<'a> {
            WallView {
                tables: &self.tables,
                projection: &self.projection,
                light: &self.light,
                textures: &self.textures,
                colormaps: &self.colormaps,
                view_x: Fixed::from_int(32),
                view_y: Fixed::ZERO,
                view_z,
                view_angle: Angle::ZERO,
                view_cos: crate::tables::FINE_COSINE[Angle::ZERO.fine_angle()],
                view_sin: crate::tables::FINE_SINE[Angle::ZERO.fine_angle()],
                view_width: VIEW_WIDTH,
                view_height: VIEW_HEIGHT,
                center_y: VIEW_HEIGHT / 2,
                center_x_frac: Fixed::from_int(VIEW_WIDTH / 2),
                center_y_frac: Fixed::from_int(VIEW_HEIGHT / 2),
                projection_distance: Fixed::from_int(VIEW_WIDTH / 2),
                extralight: 0,
                fixed_colormap: None,
                detail_shift: 0,
                sky_flat: None,
            }
        }
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

    /// the room fixture: a vertical one-sided wall at x = 64 facing west,
    /// viewed from (32, 0) looking east
    fn wall_fixture() -> Map {
        let vertexes = vec![
            Vertex {
                x: Fixed::from_int(64),
                y: Fixed::from_int(32),
            },
            Vertex {
                x: Fixed::from_int(64),
                y: Fixed::from_int(-32),
            },
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
        Map {
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
        }
    }

    /// rasterize seg 0 through add_line + store_wall_range, like the wall
    /// pass does for a real subsector
    fn rasterize_seg(
        kit: &Kit,
        frame: &mut WallFrame<'_>,
        map: &Map,
        seg_index: usize,
    ) -> AddLineOutcome {
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
        let rw_angle1 = point_to_angle(&view, v1.x, v1.y);

        let mut clip = ClipSegList::cleared(VIEW_WIDTH);
        add_line(
            &view,
            &kit.tables,
            &mut clip,
            Point::new(v1.x, v1.y),
            Point::new(v2.x, v2.y),
            front,
            back,
            has_mid,
            &mut |first, last| {
                frame.store_wall_range(map, seg_index, rw_angle1, first, last, None, None);
            },
        )
    }

    fn sprite_defs(manager: &ResourceManager) -> crate::render::SpriteDefinitions {
        crate::render::SpriteDefinitions::build(&[*b"TEST"], manager).unwrap()
    }

    fn sprite_lumps(manager: &ResourceManager) -> crate::render::SpriteLumpTables {
        crate::render::SpriteLumpTables::build(manager).unwrap()
    }

    fn thing(x: Fixed, y: Fixed, z: Fixed) -> SpriteThing {
        SpriteThing {
            x,
            y,
            z,
            angle: Angle::ZERO,
            sprite: 0,
            frame: 0,
            flags: 0,
        }
    }

    fn spritelights() -> [usize; MAXLIGHTSCALE] {
        [3; MAXLIGHTSCALE]
    }

    fn patches<'a>(
        manager: &'a ResourceManager,
    ) -> impl FnMut(usize) -> crate::errors::EngineResult<Arc<crate::gfx::patch::Patch>> + 'a {
        |lump| {
            manager.decode_cached::<crate::gfx::patch::Patch>(lump, |bytes| {
                crate::gfx::patch::decode_patch(bytes).map_err(crate::errors::EngineError::from)
            })
        }
    }

    #[test]
    fn project_overflow_is_counted_and_dropped() {
        let manager = manager();
        let kit = Kit::new(&manager);
        let view = kit.view_z(Fixed::from_int(32));
        let defs = sprite_defs(&manager);
        let lumps = sprite_lumps(&manager);
        let mut renderer = MaskedRenderer::new();
        renderer.clear();

        let t = thing(Fixed::from_int(40), Fixed::ZERO, Fixed::ZERO);
        for _ in 0..MAX_VIS_SPRITES {
            assert!(renderer.project(&t, &view, &defs, &lumps, &spritelights()));
        }
        // one more does not fit: dropped and counted
        assert!(!renderer.project(&t, &view, &defs, &lumps, &spritelights()));
        assert_eq!(renderer.vissprites().len(), MAX_VIS_SPRITES);
        assert_eq!(renderer.overflowed(), 1);

        renderer.clear();
        assert_eq!(renderer.vissprites().len(), 0);
        assert_eq!(renderer.overflowed(), 0);
    }

    #[test]
    fn sprite_draws_over_wall_and_behind_it_it_is_clipped() {
        let manager = manager();
        let kit = Kit::new(&manager);
        let map = wall_fixture();
        let defs = sprite_defs(&manager);
        let lumps = sprite_lumps(&manager);

        // camera looks at the wall from (32, 0); sprite in front of the wall
        // at x=48 (between camera and wall) must be visible,
        // sprite behind the wall at x=80 must be clipped entirely.
        let mut fb_without = IndexedFramebuffer::new();
        {
            let view = kit.view_z(Fixed::from_int(16));
            let mut frame = WallFrame::new(view, &mut fb_without);
            frame.clear();
            rasterize_seg(&kit, &mut frame, &map, 0);
        }
        let wall_pixels = fb_without.as_slice().to_vec();

        for (thing_x, expect_drawn) in [(48, true), (80, false)] {
            let mut fb = IndexedFramebuffer::new();
            {
                let view = kit.view_z(Fixed::from_int(16));
                let mut frame = WallFrame::new(view.clone(), &mut fb);
                frame.clear();
                rasterize_seg(&kit, &mut frame, &map, 0);

                let mut renderer = MaskedRenderer::new();
                renderer.clear();
                let t = thing(Fixed::from_int(thing_x), Fixed::ZERO, Fixed::ZERO);
                assert!(renderer.project(&t, &view, &defs, &lumps, &spritelights()));
                let mut patches = patches(&manager);
                let report = renderer.draw_masked(&mut frame, &view, &map, &mut patches);
                assert_eq!(report.sprites_drawn, 1);
                assert!(report.errors.is_empty());
            }
            if expect_drawn {
                assert_ne!(
                    fb.as_slice(),
                    wall_pixels.as_slice(),
                    "sprite in front of the wall must add pixels"
                );
            } else {
                assert_eq!(
                    fb.as_slice(),
                    wall_pixels.as_slice(),
                    "sprite behind a solid wall must not draw"
                );
            }
        }
    }

    #[test]
    fn masked_seg_sweep_draws_fence_without_sprites() {
        // two-sided line with a fence texture in the mid slot: the final
        // masked sweep must pick it up even with zero vissprites
        let manager = manager();
        let kit = Kit::new(&manager);
        let mut map = wall_fixture();
        // back sector + sidedef, two-sided line with FENCE1 in the mid slot
        map.sectors.push(sector(0, 128, "FLOOR4_8", "CEIL3_5", 192));
        map.sidedefs.push(Sidedef {
            texture_offset: Fixed::ZERO,
            row_offset: Fixed::ZERO,
            toptexture: *b"-\0\0\0\0\0\0\0",
            bottomtexture: *b"-\0\0\0\0\0\0\0",
            midtexture: *b"FENCE1\0\0",
            sector: 1,
        });
        map.linedefs[0].sidenum = [Some(0), Some(1)];
        map.linedefs[0].backsector = Some(1);
        map.segs[0].backsector = Some(1);
        map.sidedefs[0].midtexture = *b"FENCE1\0\0";

        let mut fb = IndexedFramebuffer::new();
        {
            // view_z = 121: the fence band lands inside the viewport
            let view = kit.view_z(Fixed::from_int(121));
            let mut frame = WallFrame::new(view.clone(), &mut fb);
            frame.clear();
            let outcome = rasterize_seg(&kit, &mut frame, &map, 0);
            assert!(matches!(outcome, AddLineOutcome::Window(_)));

            let mut renderer = MaskedRenderer::new();
            renderer.clear();
            let mut patches = patches(&manager);
            let report = renderer.draw_masked(&mut frame, &view, &map, &mut patches);
            assert_eq!(report.sprites_drawn, 0);
            assert_eq!(report.masked_segs_drawn, 1);
            assert!(report.errors.is_empty());
        }
        let drawn = fb.as_slice().iter().filter(|&&p| p != 0).count();
        assert!(drawn > 0, "fence pass drew nothing");
    }

    #[test]
    fn patch_decode_failure_is_reported_not_fatal() {
        // corrupt the sprite lump's patch so decode fails at draw time
        // (build only reads the 8-byte header, so defs still succeed)
        let bad_sprite = {
            let mut lump = Vec::new();
            lump.extend_from_slice(&8i16.to_le_bytes()); // width ok
            lump.extend_from_slice(&12i16.to_le_bytes()); // height ok
            lump.extend_from_slice(&4i16.to_le_bytes()); // left
            lump.extend_from_slice(&12i16.to_le_bytes()); // top
            lump.extend_from_slice(&i32::MAX.to_le_bytes()); // bogus column ptr
            lump
        };
        let wad = build_wad(
            b"IWAD",
            &[
                ("PNAMES", &0i32.to_le_bytes()),
                ("TEXTURE1", &0i32.to_le_bytes()),
                ("S_START", &[]),
                ("TESTA0", &bad_sprite),
                ("S_END", &[]),
            ],
        );
        let manager = ResourceManager::new(WadFile::from_bytes(wad).unwrap(), Vec::new());
        let kit = Kit::new(&manager);
        let view = kit.view_z(Fixed::from_int(32));
        let defs = crate::render::SpriteDefinitions::build(&[*b"TEST"], &manager).unwrap();
        let lumps = crate::render::SpriteLumpTables::build(&manager).unwrap();
        let map = wall_fixture();
        let mut fb = IndexedFramebuffer::new();
        {
            let mut frame = WallFrame::new(view.clone(), &mut fb);
            let mut renderer = MaskedRenderer::new();
            renderer.clear();
            let t = thing(Fixed::from_int(40), Fixed::ZERO, Fixed::ZERO);
            assert!(renderer.project(&t, &view, &defs, &lumps, &spritelights()));
            let mut patches = patches(&manager);
            let report = renderer.draw_masked(&mut frame, &view, &map, &mut patches);
            assert_eq!(report.sprites_drawn, 0);
            assert_eq!(report.errors.len(), 1);
        }
    }
}
