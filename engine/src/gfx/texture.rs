//! TEXTURE1/TEXTURE2 lump decoding, ported from `R_InitTextures` in
//! `r_data.c` and the on-disk layouts of `mappatch_t` / `maptexture_t`.
//!
//! Both lumps share the same format:
//!
//! ```text
//! offset  size          field
//! 0       4             number of textures, i32 (little-endian)
//! 4       count*4       texture directory, i32 offsets relative to lump start
//! ...     ...           texture definitions
//! ```
//!
//! Each texture definition (`maptexture_t`) is:
//!
//! ```text
//! offset  size  field
//! 0       8     name, NUL padded, compared case-insensitively by vanilla
//! 8       4     masked flag, i32 (non-zero = masked); the original is a
//!               4-byte `boolean` on disk
//! 12      2     width, i16, must be > 0
//! 14      2     height, i16, must be > 0
//! 16      4     column directory pointer, obsolete, always ignored
//! 20      2     number of patches, i16, must be >= 0
//! 22      n*10  patches, `mappatch_t` each (originx i16, originy i16,
//!               patch i16, stepdir i16, colormap i16)
//! ```
//!
//! The `patch` field indexes into the PNAMES table; entries out of range
//! are rejected here, like vanilla's `R_InitTextures` error on missing
//! patches. `stepdir` and `colormap` are parsed but unused by the engine.
//!
//! TEXTURE1 and TEXTURE2 are parsed independently with this module; the
//! original concatenates them at load time (TEXTURE2 only exists in
//! commercial game data), which is the caller's concern.

use super::error::{GfxError, GfxResult};
use super::normalize_name;
use super::pnames::Pnames;
use crate::swap;

/// Size of the obsolete column directory field in a texture definition.
/// Parsed for layout, never used.
const COLUMN_DIRECTORY_SIZE: usize = 4;
/// Size of the fixed part of a texture definition, up to the patch list.
pub const TEXTURE_HEADER_SIZE: usize = 22;
/// Size of one patch entry (`mappatch_t`) in a texture definition.
pub const MAPPATCH_SIZE: usize = 10;

/// One patch placed into a texture, as stored in the lump.
///
/// `patch` is the index into the PNAMES table, validated on decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapPatch {
    /// Horizontal position of the patch's left edge within the texture.
    pub origin_x: i16,
    /// Vertical position of the patch's top edge within the texture.
    pub origin_y: i16,
    /// Index into the PNAMES table. Guaranteed `0 <= patch < pnames.len()`.
    pub patch: i16,
    /// Flipping step, stored on disk but unused by the engine.
    pub step_dir: i16,
    /// Colormap translation, stored on disk but unused by the engine.
    pub color_map: i16,
}

/// One wall texture definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Texture {
    /// Texture name, normalized (uppercase, no NUL padding).
    pub name: String,
    /// Whether the texture is masked. Non-zero on disk decodes to `true`.
    pub masked: bool,
    /// Texture width in pixels. Guaranteed positive.
    pub width: i16,
    /// Texture height in pixels. Guaranteed positive.
    pub height: i16,
    /// The patches that compose the texture, in lump order.
    pub patches: Vec<MapPatch>,
}

/// Decodes a TEXTURE1/TEXTURE2 lump, validating every directory offset,
/// field and patch index against `pnames`. Corrupt data yields `Err`.
pub fn parse_texture_lump(data: &[u8], pnames: &Pnames) -> GfxResult<Vec<Texture>> {
    if data.len() < 4 {
        return Err(GfxError::Truncated {
            what: "TEXTURE directory",
            offset: 0,
            needed: 4,
            available: data.len(),
        });
    }
    let num = swap::read_i32(data, 0)?;
    if num < 0 {
        return Err(GfxError::BadValue {
            what: "TEXTURE directory",
            reason: format!("negative texture count {num}"),
        });
    }
    let num = num as usize;
    let table_size = num.checked_mul(4).ok_or(GfxError::BadValue {
        what: "TEXTURE directory",
        reason: "directory size overflows usize".to_string(),
    })?;
    let table_end = 4usize.checked_add(table_size).ok_or(GfxError::BadValue {
        what: "TEXTURE directory",
        reason: "directory size overflows usize".to_string(),
    })?;
    if data.len() < table_end {
        return Err(GfxError::Truncated {
            what: "TEXTURE directory",
            offset: 4,
            needed: table_size,
            available: data.len() - 4,
        });
    }

    let mut textures = Vec::with_capacity(num);
    for i in 0..num {
        let offset = swap::read_i32(data, 4 + i * 4)?;
        if offset < 0 || offset as usize >= data.len() {
            return Err(GfxError::OutOfBounds {
                what: "texture definition",
                offset: offset as i64,
                size: data.len(),
            });
        }
        let start = offset as usize;
        if data.len() - start < TEXTURE_HEADER_SIZE {
            return Err(GfxError::Truncated {
                what: "texture definition",
                offset: start,
                needed: TEXTURE_HEADER_SIZE,
                available: data.len() - start,
            });
        }

        let name_raw: [u8; 8] = data[start..start + 8].try_into().expect("8 name bytes");
        let name = normalize_name(&name_raw);
        let masked = swap::read_i32(data, start + 8)? != 0;
        let width = swap::read_i16(data, start + 12)?;
        let height = swap::read_i16(data, start + 14)?;
        // the 4 bytes at start + 16 are the obsolete column directory; skip them
        let patch_count = swap::read_i16(data, start + 16 + COLUMN_DIRECTORY_SIZE)?;
        if width <= 0 {
            return Err(GfxError::BadValue {
                what: "texture definition",
                reason: format!("texture {name}: width must be positive, got {width}"),
            });
        }
        if height <= 0 {
            return Err(GfxError::BadValue {
                what: "texture definition",
                reason: format!("texture {name}: height must be positive, got {height}"),
            });
        }
        if patch_count < 0 {
            return Err(GfxError::BadValue {
                what: "texture definition",
                reason: format!("texture {name}: negative patch count {patch_count}"),
            });
        }

        let patches_start = start + TEXTURE_HEADER_SIZE;
        let patches_size =
            (patch_count as usize)
                .checked_mul(MAPPATCH_SIZE)
                .ok_or(GfxError::BadValue {
                    what: "texture definition",
                    reason: format!("texture {name}: patch list size overflows usize"),
                })?;
        let patches_end = patches_start
            .checked_add(patches_size)
            .ok_or(GfxError::BadValue {
                what: "texture definition",
                reason: format!("texture {name}: patch list size overflows usize"),
            })?;
        if data.len() < patches_end {
            return Err(GfxError::Truncated {
                what: "texture patch list",
                offset: patches_start,
                needed: patches_size,
                available: data.len() - patches_start,
            });
        }

        let mut patches = Vec::with_capacity(patch_count as usize);
        for j in 0..patch_count as usize {
            let p = patches_start + j * MAPPATCH_SIZE;
            let origin_x = swap::read_i16(data, p)?;
            let origin_y = swap::read_i16(data, p + 2)?;
            let patch = swap::read_i16(data, p + 4)?;
            let step_dir = swap::read_i16(data, p + 6)?;
            let color_map = swap::read_i16(data, p + 8)?;
            if patch < 0 || patch as usize >= pnames.len() {
                return Err(GfxError::MissingPatch {
                    texture: name,
                    index: patch,
                });
            }
            patches.push(MapPatch {
                origin_x,
                origin_y,
                patch,
                step_dir,
                color_map,
            });
        }

        textures.push(Texture {
            name,
            masked,
            width,
            height,
            patches,
        });
    }
    Ok(textures)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_pnames(names: &[&str]) -> Pnames {
        let mut data = Vec::new();
        data.extend_from_slice(&(names.len() as i32).to_le_bytes());
        for name in names {
            let mut raw = [0u8; 8];
            for (dst, src) in raw.iter_mut().zip(name.as_bytes().iter()) {
                *dst = *src;
            }
            data.extend_from_slice(&raw);
        }
        Pnames::parse(&data).unwrap()
    }

    /// One patch definition as raw bytes.
    fn patch_bytes(origin_x: i16, origin_y: i16, patch: i16) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(MAPPATCH_SIZE);
        bytes.extend_from_slice(&origin_x.to_le_bytes());
        bytes.extend_from_slice(&origin_y.to_le_bytes());
        bytes.extend_from_slice(&patch.to_le_bytes());
        bytes.extend_from_slice(&0i16.to_le_bytes()); // stepdir, unused
        bytes.extend_from_slice(&0i16.to_le_bytes()); // colormap, unused
        bytes
    }

    /// One texture definition (`maptexture_t`) as raw bytes.
    fn texture_def_bytes(
        name: &str,
        masked: i32,
        width: i16,
        height: i16,
        patches: &[(i16, i16, i16)],
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut raw = [0u8; 8];
        for (dst, src) in raw.iter_mut().zip(name.as_bytes().iter()) {
            *dst = *src;
        }
        bytes.extend_from_slice(&raw);
        bytes.extend_from_slice(&masked.to_le_bytes());
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&0i32.to_le_bytes()); // obsolete column directory
        bytes.extend_from_slice(&(patches.len() as i16).to_le_bytes());
        for &(x, y, patch) in patches {
            bytes.extend_from_slice(&patch_bytes(x, y, patch));
        }
        bytes
    }

    /// A complete TEXTURE lump with the given definitions.
    fn build_texture_lump(defs: &[Vec<u8>]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&(defs.len() as i32).to_le_bytes());
        let table_start = data.len();
        data.resize(table_start + defs.len() * 4, 0);
        for (i, def) in defs.iter().enumerate() {
            let offset = data.len() as i32;
            data[table_start + i * 4..table_start + (i + 1) * 4]
                .copy_from_slice(&offset.to_le_bytes());
            data.extend_from_slice(def);
        }
        data
    }

    #[test]
    fn decodes_a_well_formed_lump() {
        let pnames = build_pnames(&["TROOA1", "SKY1", "WOOD1"]);
        let defs = vec![
            texture_def_bytes("WALL01", 0, 128, 64, &[(0, 0, 0), (32, 8, 1)]),
            texture_def_bytes("sky1", 1, 256, 128, &[(0, 0, 1)]),
        ];
        let data = build_texture_lump(&defs);
        let textures = parse_texture_lump(&data, &pnames).unwrap();

        assert_eq!(textures.len(), 2);
        assert_eq!(textures[0].name, "WALL01");
        assert_eq!(textures[0].width, 128);
        assert_eq!(textures[0].height, 64);
        assert!(!textures[0].masked);
        assert_eq!(textures[0].patches.len(), 2);
        assert_eq!(textures[0].patches[0].origin_x, 0);
        assert_eq!(textures[0].patches[1].origin_y, 8);
        assert_eq!(textures[0].patches[1].patch, 1);

        assert_eq!(textures[1].name, "SKY1");
        assert!(textures[1].masked);
        assert_eq!(textures[1].patches[0].patch, 1);
    }

    #[test]
    fn texture_with_no_patches_is_legal() {
        let pnames = build_pnames(&["TROOA1"]);
        let defs = vec![texture_def_bytes("EMPTY", 0, 64, 64, &[])];
        let textures = parse_texture_lump(&build_texture_lump(&defs), &pnames).unwrap();
        assert_eq!(textures[0].patches.len(), 0);
    }

    #[test]
    fn patch_index_past_pnames_is_rejected() {
        let pnames = build_pnames(&["ONLYONE"]);
        let defs = vec![texture_def_bytes("BAD", 0, 64, 64, &[(0, 0, 1)])];
        let err = parse_texture_lump(&build_texture_lump(&defs), &pnames).unwrap_err();
        assert!(matches!(err, GfxError::MissingPatch { index: 1, .. }));
    }

    #[test]
    fn negative_patch_index_is_rejected() {
        let pnames = build_pnames(&["ONLYONE"]);
        let defs = vec![texture_def_bytes("BAD", 0, 64, 64, &[(0, 0, -1)])];
        assert!(matches!(
            parse_texture_lump(&build_texture_lump(&defs), &pnames).unwrap_err(),
            GfxError::MissingPatch { index: -1, .. }
        ));
    }

    #[test]
    fn missing_pnames_table_rejects_everything() {
        let pnames = build_pnames(&[]);
        let defs = vec![texture_def_bytes("BAD", 0, 64, 64, &[(0, 0, 0)])];
        assert!(matches!(
            parse_texture_lump(&build_texture_lump(&defs), &pnames).unwrap_err(),
            GfxError::MissingPatch { .. }
        ));
    }

    #[test]
    fn directory_offset_out_of_bounds_is_rejected() {
        let pnames = build_pnames(&["TROOA1"]);
        let defs = vec![texture_def_bytes("A", 0, 64, 64, &[])];
        let mut data = build_texture_lump(&defs);
        data[4..8].copy_from_slice(&10_000_000i32.to_le_bytes());
        assert!(matches!(
            parse_texture_lump(&data, &pnames).unwrap_err(),
            GfxError::OutOfBounds { .. }
        ));

        // negative offset, too
        let mut data = build_texture_lump(&defs);
        data[4..8].copy_from_slice(&(-1i32).to_le_bytes());
        assert!(matches!(
            parse_texture_lump(&data, &pnames).unwrap_err(),
            GfxError::OutOfBounds { offset: -1, .. }
        ));
    }

    #[test]
    fn zero_width_or_height_is_rejected() {
        let pnames = build_pnames(&["TROOA1"]);
        for (width, height) in [(0, 64), (64, 0), (-1, 64), (64, -1)] {
            let defs = vec![texture_def_bytes("A", 0, width, height, &[])];
            assert!(parse_texture_lump(&build_texture_lump(&defs), &pnames).is_err());
        }
    }

    #[test]
    fn negative_texture_count_is_rejected() {
        let pnames = build_pnames(&["TROOA1"]);
        let data = (-1i32).to_le_bytes();
        assert!(matches!(
            parse_texture_lump(&data, &pnames).unwrap_err(),
            GfxError::BadValue { .. }
        ));
    }

    #[test]
    fn absurd_texture_count_does_not_overflow() {
        let pnames = build_pnames(&["TROOA1"]);
        let data = i32::MAX.to_le_bytes();
        assert!(parse_texture_lump(&data, &pnames).is_err());
    }

    #[test]
    fn directory_larger_than_lump_is_rejected() {
        let pnames = build_pnames(&["TROOA1"]);
        let mut data = 3i32.to_le_bytes().to_vec();
        data.extend_from_slice(&[0u8; 4]); // only one offset for three textures
        assert!(matches!(
            parse_texture_lump(&data, &pnames).unwrap_err(),
            GfxError::Truncated { .. }
        ));
    }

    #[test]
    fn texture_definition_running_past_the_end_is_rejected() {
        let pnames = build_pnames(&["TROOA1"]);
        let defs = vec![texture_def_bytes("A", 0, 64, 64, &[(0, 0, 0)])];
        let data = build_texture_lump(&defs);
        // cut off the last patch entry
        let truncated = &data[..data.len() - 2];
        assert!(matches!(
            parse_texture_lump(truncated, &pnames).unwrap_err(),
            GfxError::Truncated { .. }
        ));
    }

    #[test]
    fn patch_count_negative_is_rejected() {
        let pnames = build_pnames(&["TROOA1"]);
        let mut def = texture_def_bytes("A", 0, 64, 64, &[]);
        // overwrite patchcount with -1
        def[20..22].copy_from_slice(&(-1i16).to_le_bytes());
        let data = build_texture_lump(&[def]);
        assert!(parse_texture_lump(&data, &pnames).is_err());
    }

    #[test]
    fn names_are_uppercased() {
        let pnames = build_pnames(&["TROOA1"]);
        let defs = vec![texture_def_bytes("lowercase", 0, 64, 64, &[])];
        let textures = parse_texture_lump(&build_texture_lump(&defs), &pnames).unwrap();
        assert_eq!(textures[0].name, "LOWERCAS"); // truncated to 8 bytes
    }

    #[test]
    fn any_corruption_is_a_result_not_a_crash() {
        let pnames = build_pnames(&["TROOA1", "SKY1", "WOOD1"]);
        let defs = vec![
            texture_def_bytes("WALL01", 0, 128, 64, &[(0, 0, 0), (32, 8, 1)]),
            texture_def_bytes("WALL02", 1, 64, 128, &[(0, 0, 2)]),
        ];
        let data = build_texture_lump(&defs);
        let mut state = 0xFEED_FACE_u32;
        for i in 0..data.len() {
            for _ in 0..2 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let mut bad = data.clone();
                bad[i] ^= (state >> 9) as u8 | 0x01;
                let _ = parse_texture_lump(&bad, &pnames);
            }
        }
    }
}
