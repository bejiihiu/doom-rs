//! Flat wall texture composites. The original engine samples wall
//! columns straight from patch lumps and multi-patch composite blocks
//! (`R_GetColumn` walks the patch/composite chain every column fetch);
//! here the whole texture is composed once, on first use, into a flat
//! column-major pixel buffer plus per-column coverage runs, and every
//! later sample reads that buffer.
//!
//! What is preserved 1:1 from `R_GenerateComposite` / `R_DrawColumnInCache`:
//!
//! * patches are drawn back to front, later patches overwrite earlier ones;
//! * columns are clipped vertically only at the texture's top/bottom edges
//!   (negative `originy` trims texels, the tail past `height` is dropped);
//! * columns sticking out of the texture horizontally are clipped at both
//!   sides (`x1 < 0` / `x2 > width`), exactly like the original's
//!   `x` / `x2` clamping;
//! * texels stay palette indices, no colormap is applied at build time.
//!
//! What is deliberately different, documented:
//!
//! * texel rows wrap at the texture's *height* (`rem_euclid`), not at 128.
//!   The original hard-codes `& 127`, which reads into the next column's
//!   data for sub-128-tall textures (it was developed against 128-tall
//!   wall textures). For 128-tall textures the wrap is identical; for
//!   shorter ones it is the sane reading of the same wrap rule.
//! * columns wrap at the texture's *width* (`rem_euclid`) instead of the
//!   next power of two minus one (`texturewidthmask`), for the same
//!   reason; identical for power-of-two widths, i.e. all stock textures.
//! * uncovered texels are palette index 0 (black) instead of whatever the
//!   zone allocator left behind; the coverage runs make them transparent
//!   to the masked-midtexture drawer anyway.
//! * patch pixels are decoded eagerly at load, in one pass; the original
//!   decodes at first `R_GetColumn` per patch and precaches the level at
//!   map start. Eager decode means `from_resources` can fail on a corrupt
//!   patch — same failure, deterministic point in time.
//!
//! The masked-midtexture drawer consumes [`TexturePixels::runs`]: the
//! set of covered row runs per column, which is the superset of what the
//! original gets from the patch post chain (single-patch columns stay
//! bit-identical; multi-patch columns draw the union of their coverage).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::errors::{EngineError, EngineResult};
use crate::fixed::Fixed;
use crate::gfx::patch::{Patch, decode_patch};
use crate::gfx::pnames::Pnames;
use crate::gfx::texture::{Texture, parse_texture_lump};
use crate::resources::ResourceManager;

use super::column::ColumnSource;

/// One covered row run inside a texture column: rows `[top, top + len)`
/// hold texels from at least one patch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TextureRun {
    /// First covered texel row.
    pub top: i32,
    /// Number of covered texel rows.
    pub len: i32,
}

/// A flat, column-major wall texture with coverage runs.
///
/// `pixels` is laid out as `width` columns of `height` texels, index
/// `col * height + row`; `runs[col]` lists the covered runs in row
/// order. Build with [`build_texture_pixels`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TexturePixels {
    width: i32,
    height: i32,
    pixels: Vec<u8>,
    runs: Vec<Vec<TextureRun>>,
}

impl TexturePixels {
    /// Texture width in texels.
    pub fn width(&self) -> i32 {
        self.width
    }

    /// Texture height in texels.
    pub fn height(&self) -> i32 {
        self.height
    }

    /// Raw column-major pixels, `col * height + row`.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The `height` texels of column `col` (`col` wraps at the width).
    pub fn column_pixels(&self, col: i32) -> &[u8] {
        let col = col.rem_euclid(self.width) as usize;
        &self.pixels[col * self.height as usize..(col + 1) * self.height as usize]
    }

    /// One texel; `row` wraps at the texture height (see the module docs
    /// for why this is not `& 127`).
    pub fn texel(&self, col: i32, row: i32) -> u8 {
        self.pixels[col.rem_euclid(self.width) as usize * self.height as usize
            + row.rem_euclid(self.height) as usize]
    }

    /// The covered runs of column `col` (`col` wraps at the width), in row
    /// order. Fully covered columns report a single `0..height` run; this
    /// is what the masked drawer walks.
    pub fn runs(&self, col: i32) -> &[TextureRun] {
        let col = col.rem_euclid(self.width) as usize;
        &self.runs[col]
    }
}

/// Composites a texture into its flat pixel buffer.
///
/// `patches` is indexed by the patch's PNAMES table index (the indices
/// [`Texture::patches`] carries after load). Returns an error when a patch
/// is missing from the table — corrupt state that `Pnames` validation
/// normally rules out at parse time.
pub fn build_texture_pixels(
    texture: &Texture,
    patches: &[Option<Arc<Patch>>],
) -> EngineResult<TexturePixels> {
    let width = i32::from(texture.width);
    let height = i32::from(texture.height);
    if width <= 0 || height <= 0 {
        return Err(EngineError::Resource(format!(
            "texture {}: invalid {width}x{height} dimensions",
            texture.name
        )));
    }

    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| {
            EngineError::Resource(format!("texture {}: size overflows", texture.name))
        })?;

    let mut pixels = vec![0u8; pixel_count];
    let mut covered = vec![false; pixel_count];

    for patch_ref in &texture.patches {
        let patch_index = patch_ref.patch as usize;
        let patch = patches
            .get(patch_index)
            .and_then(Option::as_ref)
            .ok_or_else(|| {
                EngineError::Resource(format!(
                    "texture {}: patch index {patch_index} not resolved",
                    texture.name
                ))
            })?;
        let patch = patch.as_ref();

        // clip horizontally like the original's x / x2 clamping
        let x_start = i32::from(patch_ref.origin_x).max(0);
        let x_end = (i32::from(patch_ref.origin_x) + i32::from(patch.header.width)).min(width);
        if x_start >= x_end {
            continue;
        }

        for x in x_start..x_end {
            let patch_col = (x - i32::from(patch_ref.origin_x)) as usize;
            let column = match patch.columns.get(patch_col) {
                Some(column) => column,
                None => continue,
            };

            for post in &column.posts {
                // vertical placement, like R_DrawColumnInCache's position/count math
                let mut position = i32::from(patch_ref.origin_y) + i32::from(post.top_delta);
                let mut count = post.pixels.len() as i32;
                let mut src = 0usize;

                if position < 0 {
                    src = src.wrapping_sub(position as usize);
                    count += position;
                    position = 0;
                }
                if position + count > height {
                    count = height - position;
                }
                if count <= 0 {
                    continue;
                }

                let dest = x as usize * height as usize + position as usize;
                for (i, &texel) in post.pixels[src..src + count as usize].iter().enumerate() {
                    pixels[dest + i] = texel;
                    covered[dest + i] = true;
                }
            }
        }
    }

    // coverage booleans -> runs, one pass per column
    let mut runs = Vec::with_capacity(width as usize);
    for col in 0..width as usize {
        let mut column_runs = Vec::new();
        let base = col * height as usize;
        let mut row = 0i32;
        while row < height {
            if !covered[base + row as usize] {
                row += 1;
                continue;
            }
            let top = row;
            while row < height && covered[base + row as usize] {
                row += 1;
            }
            column_runs.push(TextureRun {
                top,
                len: row - top,
            });
        }
        runs.push(column_runs);
    }

    Ok(TexturePixels {
        width,
        height,
        pixels,
        runs,
    })
}

/// A texture column sampler for the column drawers.
///
/// Owns an `Arc` to the built composite so it can outlive the cache
/// borrow that produced it. Wraps the column at the texture width
/// (`rem_euclid`, see the module docs).
#[derive(Clone)]
pub struct TextureColumn {
    texture: Arc<TexturePixels>,
    column: i32,
}

impl ColumnSource for TextureColumn {
    #[inline]
    fn texel(&self, row: i32) -> u8 {
        self.texture.texel(self.column, row)
    }
}

impl TextureColumn {
    /// The wrapped texture column index.
    pub fn column(&self) -> i32 {
        self.column
    }
}

/// The renderer's texture table: decoded `TEXTURE1`+`TEXTURE2`,
/// PNAMES resolution, per-texture pixel heights and the flat composite
/// cache. Single threaded (`RefCell`), like the original's zone memory.
pub struct TextureSet {
    textures: Vec<Texture>,
    /// Normalized name -> texture index.
    names: HashMap<String, usize>,
    /// PNAMES index -> decoded patch, resolved and decoded at load.
    patches: Vec<Option<Arc<Patch>>>,
    /// `textureheight[]`: pixel height << FRACBITS, for texture pegging.
    texture_heights: Vec<Fixed>,
    /// `texturetranslation[]`: global animation remaps (identity for now).
    translation: Vec<usize>,
    /// Lazy flat composite cache.
    cache: RefCell<Vec<Option<Arc<TexturePixels>>>>,
}

impl TextureSet {
    /// Loads PNAMES and TEXTURE1 (+ TEXTURE2 when present), resolves and
    /// decodes every patch lump (a missing or corrupt patch lump is an
    /// error, like `R_InitTextures` + `R_PrecacheLevel`), and builds the
    /// height/translation tables.
    pub fn from_resources(resources: &ResourceManager) -> EngineResult<Self> {
        let pnames_lump = resources.lump_data("PNAMES")?;
        let pnames = Pnames::parse(pnames_lump).map_err(EngineError::from)?;

        let texture1 = resources.lump_data("TEXTURE1")?;
        let mut textures = parse_texture_lump(texture1, &pnames).map_err(EngineError::from)?;

        if let Some(lump) = resources.find_lump("TEXTURE2") {
            let texture2 = resources.lump_data_index(lump)?;
            textures.extend(parse_texture_lump(texture2, &pnames).map_err(EngineError::from)?);
        }

        // resolve and decode only the patches the textures actually need.
        // vanilla only ever looks up patches through textures, so dangling
        // PNAMES entries that no texture references are fine to skip
        let needed: std::collections::HashSet<usize> = textures
            .iter()
            .flat_map(|t| t.patches.iter().map(|p| p.patch as usize))
            .collect();
        let mut patches: Vec<Option<Arc<Patch>>> = Vec::with_capacity(pnames.len());
        for (index, name) in pnames.names().iter().enumerate() {
            if !needed.contains(&index) {
                patches.push(None);
                continue;
            }
            let lump = resources.find_lump(name).ok_or_else(|| {
                EngineError::Resource(format!("PNAMES patch {name}: lump not found"))
            })?;
            let bytes = resources.lump_data_index(lump)?.to_vec();
            let patch = decode_patch(&bytes).map_err(EngineError::from)?;
            patches.push(Some(Arc::new(patch)));
        }

        let num_textures = textures.len();
        let texture_heights = textures
            .iter()
            .map(|t| Fixed::from_raw(i32::from(t.height) << 16))
            .collect();
        let translation = (0..num_textures).collect();

        let mut names = HashMap::with_capacity(num_textures);
        for (index, texture) in textures.iter().enumerate() {
            names.entry(texture.name.clone()).or_insert(index);
        }

        Ok(Self {
            textures,
            names,
            patches,
            texture_heights,
            translation,
            cache: RefCell::new(vec![None; num_textures]),
        })
    }

    /// Number of textures (`numtextures`).
    pub fn len(&self) -> usize {
        self.textures.len()
    }

    /// Always false: a real texture table holds at least one texture.
    pub fn is_empty(&self) -> bool {
        self.textures.is_empty()
    }

    /// The decoded texture definition at `index`.
    pub fn texture(&self, index: usize) -> Option<&Texture> {
        self.textures.get(index)
    }

    /// `textureheight[]` for `index` (`height << FRACBITS`).
    pub fn texture_height(&self, index: usize) -> Fixed {
        self.texture_heights
            .get(index)
            .copied()
            .unwrap_or(Fixed::ZERO)
    }

    /// `R_CheckTextureNumForName`: name -> index, case insensitive,
    /// `-`-prefixed names mean "no texture" (`0`, the original's
    /// stand-in). Returns `None` for missing textures (the caller logs
    /// and degrades; vanilla would `I_Error` at map load).
    pub fn lookup_name(&self, raw_name: &[u8; 8]) -> Option<usize> {
        if raw_name[0] == b'-' {
            // the no-texture marker maps to texture 0 when it exists
            return (!self.textures.is_empty()).then_some(0);
        }
        let end = raw_name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(raw_name.len());
        let name: String = String::from_utf8_lossy(&raw_name[..end]).to_ascii_uppercase();
        self.names.get(&name).copied()
    }

    /// `R_TextureNumForName` against the translation table: resolves a raw
    /// sidedef name through `texturetranslation[]` (identity until global
    /// animation lands). `None` for missing textures.
    pub fn lookup_translated(&self, raw_name: &[u8; 8]) -> Option<usize> {
        self.lookup_name(raw_name)
            .map(|index| self.translation[index])
    }

    /// The flat composite for `index`, building it on first use.
    /// Errors are not cached: a later call retries from the decoded patches.
    pub fn pixels(&self, index: usize) -> EngineResult<Arc<TexturePixels>> {
        if index >= self.textures.len() {
            return Err(EngineError::Resource(format!(
                "texture index {index} out of range ({})",
                self.textures.len()
            )));
        }
        if let Some(cached) = self.cache.borrow().get(index).and_then(Option::as_ref) {
            return Ok(Arc::clone(cached));
        }

        let built = Arc::new(build_texture_pixels(&self.textures[index], &self.patches)?);
        self.cache.borrow_mut()[index] = Some(Arc::clone(&built));
        Ok(built)
    }

    /// A column sampler for `index` at `column`. The column index wraps at
    /// the texture width; the texture is built on first use.
    pub fn column(&self, index: usize, column: i32) -> EngineResult<TextureColumn> {
        let pixels = self.pixels(index)?;
        let column = column.rem_euclid(pixels.width());
        Ok(TextureColumn {
            texture: pixels,
            column,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::testutil as map_util;
    use crate::wad::WadFile;
    use crate::wad::testutil::build_wad;

    fn patch_with_posts(width: i16, height: i16, posts: Vec<Vec<Post>>) -> Patch {
        let columns = posts
            .into_iter()
            .map(|posts| PatchColumn { posts })
            .collect::<Vec<_>>();
        Patch {
            header: PatchHeader {
                width,
                height,
                left_offset: 0,
                top_offset: 0,
                column_ptrs: Vec::new(), // unused by the composite
            },
            columns,
        }
    }

    use crate::gfx::patch::{PatchColumn, PatchHeader, Post};

    fn post(top: u8, len: usize, value: u8) -> Post {
        Post {
            top_delta: top,
            pixels: vec![value; len],
        }
    }

    #[test]
    fn composite_single_patch_fully_covered() {
        let texture = Texture {
            name: "WALL1".into(),
            masked: false,
            width: 4,
            height: 128,
            patches: vec![crate::gfx::texture::MapPatch {
                origin_x: 0,
                origin_y: 0,
                patch: 0,
                step_dir: 0,
                color_map: 0,
            }],
        };
        let patch = Arc::new(patch_with_posts(
            4,
            128,
            (0..4).map(|_| vec![post(0, 128, 7)]).collect(),
        ));
        let pixels = build_texture_pixels(&texture, &[Some(patch)]).unwrap();

        assert_eq!(pixels.width(), 4);
        assert_eq!(pixels.height(), 128);
        assert_eq!(pixels.pixels().len(), 4 * 128);
        assert!(pixels.pixels().iter().all(|&b| b == 7));
        for col in 0..4 {
            assert_eq!(pixels.runs(col), &[TextureRun { top: 0, len: 128 }]);
        }
    }

    #[test]
    fn composite_clips_horizontally() {
        let texture = Texture {
            name: "WALL1".into(),
            masked: false,
            width: 8,
            height: 16,
            patches: vec![crate::gfx::texture::MapPatch {
                origin_x: -2,
                origin_y: 0,
                patch: 0,
                step_dir: 0,
                color_map: 0,
            }],
        };
        let patch = Arc::new(patch_with_posts(
            4,
            16,
            (0..4).map(|_| vec![post(0, 16, 9)]).collect(),
        ));
        let pixels = build_texture_pixels(&texture, &[Some(patch)]).unwrap();

        // patch columns 2..3 land at texture columns 0..1
        assert_eq!(pixels.texel(0, 0), 9);
        assert_eq!(pixels.texel(1, 0), 9);
        assert_eq!(pixels.texel(2, 0), 0, "column 2 untouched");
        assert_eq!(pixels.runs(0), [TextureRun { top: 0, len: 16 }]);
        assert_eq!(pixels.runs(2), &[]);
    }

    #[test]
    fn composite_clips_vertically() {
        let texture = Texture {
            name: "WALL2".into(),
            masked: false,
            width: 2,
            height: 16,
            patches: vec![crate::gfx::texture::MapPatch {
                origin_x: 0,
                origin_y: -4,
                patch: 0,
                step_dir: 0,
                color_map: 0,
            }],
        };
        // post top at row 2, length 8 -> rows -2..5 in texture space (clipped to 0..5)
        let patch = Arc::new(patch_with_posts(
            2,
            16,
            (0..2).map(|_| vec![post(2, 8, 5)]).collect(),
        ));
        let pixels = build_texture_pixels(&texture, &[Some(patch)]).unwrap();

        // position = -4 + 2 = -2; count = 8; head clipped to 0; rows 0..5 covered
        assert_eq!(pixels.runs(0), [TextureRun { top: 0, len: 6 }]);
        assert_eq!(pixels.texel(0, 5), 5);
        assert_eq!(pixels.texel(0, 6), 0);
    }

    #[test]
    fn composite_bottom_edge_clips() {
        let texture = Texture {
            name: "WALL3".into(),
            masked: false,
            width: 1,
            height: 16,
            patches: vec![crate::gfx::texture::MapPatch {
                origin_x: 0,
                origin_y: 12,
                patch: 0,
                step_dir: 0,
                color_map: 0,
            }],
        };
        let patch = Arc::new(patch_with_posts(1, 16, vec![vec![post(0, 128, 3)]]));
        let pixels = build_texture_pixels(&texture, &[Some(patch)]).unwrap();
        assert_eq!(pixels.runs(0), [TextureRun { top: 12, len: 4 }]);
    }

    #[test]
    fn composite_back_to_front_layering() {
        let texture = Texture {
            name: "WALL4".into(),
            masked: false,
            width: 2,
            height: 8,
            patches: vec![
                crate::gfx::texture::MapPatch {
                    origin_x: 0,
                    origin_y: 0,
                    patch: 0,
                    step_dir: 0,
                    color_map: 0,
                },
                crate::gfx::texture::MapPatch {
                    origin_x: 0,
                    origin_y: 0,
                    patch: 1,
                    step_dir: 0,
                    color_map: 0,
                },
            ],
        };
        let back = Arc::new(patch_with_posts(
            2,
            8,
            (0..2).map(|_| vec![post(0, 8, 1)]).collect(),
        ));
        let front = Arc::new(patch_with_posts(
            2,
            8,
            (0..2).map(|_| vec![post(0, 8, 2)]).collect(),
        ));
        let pixels = build_texture_pixels(&texture, &[Some(back), Some(front)]).unwrap();
        assert_eq!(pixels.texel(0, 0), 2, "later patch wins");
    }

    #[test]
    fn composite_runs_reflect_post_gaps() {
        let texture = Texture {
            name: "FENCE1".into(),
            masked: true,
            width: 1,
            height: 16,
            patches: vec![crate::gfx::texture::MapPatch {
                origin_x: 0,
                origin_y: 0,
                patch: 0,
                step_dir: 0,
                color_map: 0,
            }],
        };
        // two runs: 0..3 and 8..15, gap between
        let patch = Arc::new(patch_with_posts(
            1,
            16,
            vec![vec![post(0, 4, 1), post(8, 8, 2)]],
        ));
        let pixels = build_texture_pixels(&texture, &[Some(patch)]).unwrap();
        assert_eq!(
            pixels.runs(0),
            [TextureRun { top: 0, len: 4 }, TextureRun { top: 8, len: 8 }]
        );
        assert_eq!(pixels.texel(0, 4), 0, "gap is uncovered");
    }

    fn build_test_wad() -> Vec<u8> {
        let mut pnames = Vec::new();
        pnames.extend_from_slice(&2i32.to_le_bytes());
        for name in ["PATCHA", "PATCHB"] {
            let mut raw = [0u8; 8];
            raw[..name.len()].copy_from_slice(name.as_bytes());
            pnames.extend_from_slice(&raw);
        }

        let patch_lump = |value: u8| -> Vec<u8> {
            // 1 wide, 4 high patch, single post of 4 pixels
            let mut lump = Vec::new();
            lump.extend_from_slice(&1i16.to_le_bytes()); // width
            lump.extend_from_slice(&4i16.to_le_bytes()); // height
            lump.extend_from_slice(&0i16.to_le_bytes()); // left offset
            lump.extend_from_slice(&0i16.to_le_bytes()); // top offset
            lump.extend_from_slice(&12i32.to_le_bytes()); // column 0 offset
            lump.extend_from_slice(&0u8.to_le_bytes()); // topdelta 0
            lump.extend_from_slice(&4u8.to_le_bytes()); // length 4
            lump.push(0); // pad
            lump.extend_from_slice(&[value; 4]);
            lump.push(0); // pad
            lump.push(0xff); // terminator
            lump
        };

        let definition = |name: &str, patch: i16| -> Vec<u8> {
            let mut def = Vec::new();
            let mut raw = [0u8; 8];
            raw[..name.len()].copy_from_slice(name.as_bytes());
            def.extend_from_slice(&raw);
            def.extend_from_slice(&0i32.to_le_bytes()); // masked
            def.extend_from_slice(&1i16.to_le_bytes()); // width
            def.extend_from_slice(&4i16.to_le_bytes()); // height
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
        texture1.extend_from_slice(&2i32.to_le_bytes());
        let def_a = definition("WALL_A", 0);
        let def_b = definition("WALL_B", 1);
        let offset_a = 4 + 2 * 4;
        texture1.extend_from_slice(&(offset_a as i32).to_le_bytes());
        texture1.extend_from_slice(&((offset_a + def_a.len()) as i32).to_le_bytes());
        texture1.extend_from_slice(&def_a);
        texture1.extend_from_slice(&def_b);

        let _ = map_util::vertexes; // keep the import used if fixtures drift

        build_wad(
            b"IWAD",
            &[
                ("PNAMES", &pnames),
                ("TEXTURE1", &texture1),
                ("PATCHA", &patch_lump(10)),
                ("PATCHB", &patch_lump(20)),
            ],
        )
    }

    fn test_manager() -> ResourceManager {
        ResourceManager::new(WadFile::from_bytes(build_test_wad()).unwrap(), Vec::new())
    }

    #[test]
    fn texture_set_loads_and_composites() {
        let set = TextureSet::from_resources(&test_manager()).unwrap();
        assert_eq!(set.len(), 2);
        assert_eq!(set.texture_height(0), Fixed::from_int(4));
        assert_eq!(set.lookup_name(b"WALL_A\0\0"), Some(0));
        assert_eq!(set.lookup_name(b"WALL_B\0\0"), Some(1));
        assert_eq!(set.lookup_name(b"wall_a\0\0"), Some(0), "case insensitive");
        assert_eq!(set.lookup_name(b"-NONE\0\0\0"), Some(0), "dash marker -> 0");
        assert_eq!(set.lookup_name(b"MISSING\0"), None);

        let pixels = set.pixels(0).unwrap();
        assert_eq!(pixels.texel(0, 0), 10);
        let pixels_again = set.pixels(0).unwrap();
        assert!(
            Arc::ptr_eq(&pixels, &pixels_again),
            "cache returns the same Arc"
        );

        let pixels_b = set.pixels(1).unwrap();
        assert_eq!(pixels_b.texel(0, 0), 20);
    }

    #[test]
    fn texture_set_missing_patch_lump_fails_at_load() {
        let mut pnames = Vec::new();
        pnames.extend_from_slice(&1i32.to_le_bytes());
        pnames.extend_from_slice(b"NOSUCH\0\0");

        let mut texture1 = Vec::new();
        texture1.extend_from_slice(&1i32.to_le_bytes());
        texture1.extend_from_slice(&8i32.to_le_bytes());
        let mut def = Vec::new();
        def.extend_from_slice(b"EMPTY\0\0\0");
        def.extend_from_slice(&0i32.to_le_bytes()); // masked
        def.extend_from_slice(&1i16.to_le_bytes()); // width
        def.extend_from_slice(&4i16.to_le_bytes()); // height
        def.extend_from_slice(&0i32.to_le_bytes()); // obsolete column directory
        def.extend_from_slice(&1i16.to_le_bytes()); // patch count
        def.extend_from_slice(&0i16.to_le_bytes()); // originx
        def.extend_from_slice(&0i16.to_le_bytes()); // originy
        def.extend_from_slice(&0i16.to_le_bytes()); // patch index (missing lump)
        def.extend_from_slice(&0i16.to_le_bytes()); // stepdir
        def.extend_from_slice(&0i16.to_le_bytes()); // colormap
        texture1.extend_from_slice(&def);

        let wad = build_wad(b"IWAD", &[("PNAMES", &pnames), ("TEXTURE1", &texture1)]);
        let manager = ResourceManager::new(WadFile::from_bytes(wad).unwrap(), Vec::new());
        let err = TextureSet::from_resources(&manager)
            .err()
            .expect("must fail");
        assert!(matches!(err, EngineError::Resource(_)));
    }

    #[test]
    fn out_of_range_index_is_an_error() {
        let set = TextureSet::from_resources(&test_manager()).unwrap();
        assert!(set.pixels(99).is_err());
    }
}
