//! The sky pass, ported from `r_plane.c`'s sky branch of `R_DrawPlanes`
//! and `r_sky.c` (`R_InitSkyMap`, `skytexturemid`).
//!
//! The sky is a texture mapped around the view like a wall: columns are
//! sampled from `SKY1/2/3` at `(viewangle + xtoviewangle[x]) >> 22`, which
//! wraps a 256-column sky texture four times across the horizon (the
//! original's `ANGLETOSKYSHIFT`; 1024 sky columns = 360°). It is drawn
//! full bright (`colormaps[0]`) so it ignores diminished lighting and the
//! invulnerability inverse map, exactly like the original.
//!
//! `R_DrawPlanes` in this engine defers sky visplanes (they arrive here
//! untouched); call [`render_sky`] after [`super::plane::render_planes`].
//! The `skytexture` selection itself (`SKY1` vs `SKY2`/`SKY3` per episode)
//! is game logic and stays with the game-state agent; this pass just
//! takes the texture index to use.
//!
//! Texture height is not special-cased: the sky texture is as tall as it
//! is (128 texels in vanilla), and the fixed `skytexturemid` of 100 units
//! positions it so its lower edge sits near where ceilings usually are.

use crate::fixed::{FRACUNIT, Fixed};

use super::column::{ColumnParams, draw_column, draw_column_low};
use super::framebuffer::{IndexedFramebuffer, SCREEN_WIDTH};
use super::planes::PlaneSet;
use super::textures::TextureSet;
use super::wall::{WallProjection, WallView};

/// Vanilla `ANGLETOSKYSHIFT`: the shift that packs the sky's full 360°
/// into 1024 column offsets.
pub const ANGLETOSKYSHIFT: u32 = 22;

/// Vanilla `skytexturemid` (`R_InitSkyMap`): 100 units.
pub const SKY_TEXTURE_MID: Fixed = Fixed::from_raw(100 * FRACUNIT);

/// What happened during one `render_sky` call.
#[derive(Default, Debug)]
pub struct SkyReport {
    /// Columns drawn.
    pub columns_drawn: u32,
    /// Non-fatal errors (missing texture, no colormaps).
    pub errors: Vec<crate::errors::EngineError>,
}

/// Draws every visplane with `picnum == sky_picnum` as sky columns.
///
/// `planes` is the frame's visplane pool; sky planes are read (never
/// mutated) here, matching the original's handling of sky visplanes in
/// `R_DrawPlanes` (no sentinels are written for them there either).
/// `sky_texture` indexes `textures`; `sky_picnum` is the FlatSet-relative
/// sky flat index passed to `render_planes`.
pub fn render_sky(
    fb: &mut IndexedFramebuffer,
    planes: &PlaneSet,
    view: &WallView<'_>,
    projection: &WallProjection,
    textures: &TextureSet,
    sky_texture: usize,
    sky_picnum: u32,
) -> SkyReport {
    let mut report = SkyReport::default();

    let Some(sky_colormap) = view.colormaps.first() else {
        report.errors.push(crate::errors::EngineError::Resource(
            "no COLORMAP colormaps loaded; sky skipped".into(),
        ));
        return report;
    };
    if sky_texture >= textures.len() {
        report
            .errors
            .push(crate::errors::EngineError::Resource(format!(
                "sky texture index {sky_texture} is outside the texture table"
            )));
        return report;
    }

    // sky is always drawn at the sprite column scale
    let pspritescale = Fixed::from_raw(FRACUNIT * view.view_width / SCREEN_WIDTH as i32);
    let pspriteiscale = Fixed::ONE / pspritescale;
    let detail_shift = view.detail_shift.max(0) as u32;
    let dc_iscale = Fixed::from_raw(pspriteiscale.to_raw() >> detail_shift);

    for index in 0..planes.len() {
        let Some(plane) = planes.get(index) else {
            continue;
        };
        if plane.picnum != sky_picnum || plane.minx > plane.maxx {
            continue;
        }
        for x in plane.minx..=plane.maxx {
            let top = plane.top_at(x);
            // an untouched column: the original would draw garbage rows
            // here; skip instead
            if top == 0xff {
                continue;
            }
            let dc_yl = i32::from(top);
            let dc_yh = i32::from(plane.bottom_at(x));
            if dc_yl > dc_yh {
                continue;
            }

            let angle = (view.view_angle + projection.at(x)).to_raw() >> ANGLETOSKYSHIFT;
            let column = match textures.column(sky_texture, angle as i32) {
                Ok(column) => column,
                Err(err) => {
                    report.errors.push(err);
                    continue;
                }
            };
            let params = ColumnParams {
                colormap: sky_colormap,
                x,
                y_lo: dc_yl,
                y_hi: dc_yh,
                iscale: dc_iscale,
                texturemid: SKY_TEXTURE_MID,
                center_y: view.center_y,
            };
            if view.detail_shift == 1 {
                draw_column_low(fb, &column, &params);
            } else {
                draw_column(fb, &column, &params);
            }
            report.columns_drawn += 1;
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bsp::ViewTables;
    use crate::gfx::palette::Colormap;
    use crate::render::lighting::LightTables;
    use crate::render::wall::WallView;
    use crate::render::{IndexedFramebuffer, PlaneSet};
    use crate::resources::ResourceManager;
    use crate::tables::Angle;
    use crate::wad::WadFile;
    use crate::wad::testutil::build_wad;

    const VIEW_WIDTH: i32 = 320;
    const VIEW_HEIGHT: i32 = 40;

    fn stripe_patch() -> Vec<u8> {
        // 256x128, texel value == column index
        let mut lump = Vec::new();
        lump.extend_from_slice(&256i16.to_le_bytes());
        lump.extend_from_slice(&128i16.to_le_bytes());
        lump.extend_from_slice(&0i16.to_le_bytes());
        lump.extend_from_slice(&0i16.to_le_bytes());
        let column_ptrs_ofs = 8usize;
        let mut ptrs = Vec::with_capacity(256 * 4);
        let mut data = Vec::new();
        for c in 0..256i32 {
            ptrs.extend_from_slice(
                &((column_ptrs_ofs + 256 * 4 + data.len()) as i32).to_le_bytes(),
            );
            data.extend_from_slice(&[0u8, 128, 0]);
            data.extend_from_slice(&[c as u8; 128]);
            data.extend_from_slice(&[0, 0xff]);
        }
        lump.extend_from_slice(&ptrs);
        lump.extend_from_slice(&data);
        lump
    }

    fn build_sky_wad() -> Vec<u8> {
        let mut pnames = Vec::new();
        pnames.extend_from_slice(&1i32.to_le_bytes());
        let mut raw = [0u8; 8];
        raw[..8].copy_from_slice(b"SKYPATCH");
        pnames.extend_from_slice(&raw);

        let definition = {
            let mut def = Vec::new();
            def.extend_from_slice(&b"SKY1\0\0\0\0"[..]);
            def.extend_from_slice(&0i32.to_le_bytes()); // masked
            def.extend_from_slice(&256i16.to_le_bytes()); // width
            def.extend_from_slice(&128i16.to_le_bytes()); // height
            def.extend_from_slice(&0i32.to_le_bytes()); // columndirectory (obsolete)
            def.extend_from_slice(&1i16.to_le_bytes()); // one patch
            def.extend_from_slice(&0i16.to_le_bytes()); // originx
            def.extend_from_slice(&0i16.to_le_bytes()); // originy
            def.extend_from_slice(&0i16.to_le_bytes()); // patch index
            def.extend_from_slice(&0i16.to_le_bytes()); // stepdir
            def.extend_from_slice(&0i16.to_le_bytes()); // colormap
            def
        };
        let mut texture1 = Vec::new();
        texture1.extend_from_slice(&1i32.to_le_bytes());
        texture1.extend_from_slice(&8i32.to_le_bytes()); // offset of the definition (past count + directory)
        texture1.extend_from_slice(&definition);

        build_wad(
            b"IWAD",
            &[
                ("PNAMES", &pnames),
                ("TEXTURE1", &texture1),
                ("SKYPATCH", &stripe_patch()),
            ],
        )
    }

    fn manager() -> ResourceManager {
        ResourceManager::new(WadFile::from_bytes(build_sky_wad()).unwrap(), Vec::new())
    }

    fn identity_colormaps_with_invert_base() -> Vec<Colormap> {
        let mut maps: Vec<Colormap> = (0..33)
            .map(|_| Colormap(core::array::from_fn(|i| i as u8)))
            .collect();
        // fullbright map (index 0) inverts, everything else is identity
        maps[0] = Colormap(core::array::from_fn(|i| 255 - i as u8));
        maps
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
                colormaps: identity_colormaps_with_invert_base(),
            }
        }

        fn view<'a>(&'a self) -> WallView<'a> {
            WallView {
                tables: &self.tables,
                projection: &self.projection,
                light: &self.light,
                textures: &self.textures,
                colormaps: &self.colormaps,
                view_x: Fixed::ZERO,
                view_y: Fixed::ZERO,
                view_z: Fixed::from_int(16),
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
                sky_flat: Some(*b"F_SKY1\0\0"),
            }
        }
    }

    fn sky_plane(planes: &mut PlaneSet, sky_picnum: u32, mark_ranges: &[(i32, i32)]) -> usize {
        let index = planes.find_plane(Fixed::ZERO, sky_picnum, 0, true).unwrap();
        let mut index = index;
        for &(a, b) in mark_ranges {
            // like the wall code, minx/maxx come from check_plane;
            // mark() itself only writes the top/bottom arrays
            index = planes.check_plane(index, a, b).unwrap();
            for x in a..=b {
                planes.get_mut(index).unwrap().mark(x, 0, 10);
            }
        }
        index
    }

    #[test]
    fn sky_columns_use_angle_shift_and_fullbright_map() {
        let manager = manager();
        let kit = Kit::new(&manager);
        let view = kit.view();
        let textures = &kit.textures;
        let sky_texture = textures.lookup_name(b"SKY1\0\0\0\0").unwrap();
        let mut planes = PlaneSet::new();
        sky_plane(&mut planes, 7, &[(0, VIEW_WIDTH - 1)]);

        let mut fb = IndexedFramebuffer::new();
        let report = render_sky(
            &mut fb,
            &planes,
            &view,
            &kit.projection,
            textures,
            sky_texture,
            7,
        );
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.columns_drawn, VIEW_WIDTH as u32);

        for x in 0..VIEW_WIDTH {
            let angle = (view.view_angle + kit.projection.at(x)).to_raw() >> ANGLETOSKYSHIFT;
            let texel = (angle % 256) as u8;
            let expected = 255 - texel; // fullbright map inverts
            for y in 0..=10usize {
                assert_eq!(
                    fb.get(x as usize, y),
                    Some(expected),
                    "sky texel at column {x} row {y}"
                );
            }
            // rows below the marked area stay untouched
            assert_eq!(fb.get(x as usize, 11), Some(0));
        }
    }

    #[test]
    fn sky_skips_unmarked_columns_inside_the_plane() {
        let manager = manager();
        let kit = Kit::new(&manager);
        let view = kit.view();
        let sky_texture = kit.textures.lookup_name(b"SKY1\0\0\0\0").unwrap();
        let mut planes = PlaneSet::new();
        // mark columns 0 and 3 only; 1 and 2 stay 0xff (top) and are skipped
        sky_plane(&mut planes, 7, &[(0, 0), (3, 3)]);

        let mut fb = IndexedFramebuffer::new();
        let report = render_sky(
            &mut fb,
            &planes,
            &view,
            &kit.projection,
            &kit.textures,
            sky_texture,
            7,
        );
        assert_eq!(report.columns_drawn, 2);
        assert_eq!(fb.get(0, 0).unwrap(), {
            let angle = (view.view_angle + kit.projection.at(0)).to_raw() >> ANGLETOSKYSHIFT;
            255 - (angle % 256) as u8
        });
        assert_ne!(fb.get(3, 0), Some(0));
        assert_eq!(fb.get(1, 0), Some(0), "unmarked column must stay empty");
        assert_eq!(fb.get(2, 0), Some(0), "unmarked column must stay empty");
    }

    #[test]
    fn sky_reports_missing_texture_index() {
        let manager = manager();
        let kit = Kit::new(&manager);
        let view = kit.view();
        let mut planes = PlaneSet::new();
        sky_plane(&mut planes, 7, &[(0, 7)]);
        let mut fb = IndexedFramebuffer::new();
        let report = render_sky(
            &mut fb,
            &planes,
            &view,
            &kit.projection,
            &kit.textures,
            999,
            7,
        );
        assert_eq!(report.columns_drawn, 0);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(fb.as_slice().iter().filter(|&&p| p != 0).count(), 0);
    }

    #[test]
    fn sky_no_colormaps_is_an_error() {
        let manager = manager();
        let kit = Kit::new(&manager);
        let view = kit.view();
        let sky_texture = kit.textures.lookup_name(b"SKY1\0\0\0\0").unwrap();
        let mut view = view;
        view.colormaps = &[];
        let mut planes = PlaneSet::new();
        sky_plane(&mut planes, 7, &[(0, 7)]);
        let mut fb = IndexedFramebuffer::new();
        let report = render_sky(
            &mut fb,
            &planes,
            &view,
            &kit.projection,
            &kit.textures,
            sky_texture,
            7,
        );
        assert_eq!(report.columns_drawn, 0);
        assert_eq!(report.errors.len(), 1);
    }
}
