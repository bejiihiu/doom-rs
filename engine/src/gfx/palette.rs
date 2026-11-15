//! PLAYPAL and COLORMAP decoding, ported from `v_video.c`, `r_data.c` and
//! the lighting setup in `r_main.c`.
//!
//! `PLAYPAL` is a list of 256-color palettes, each color stored as three
//! bytes (red, green, blue), 768 bytes per palette. Vanilla ships 14
//! palettes (one per gamma table), but the decoder accepts any whole number
//! of palettes of at least one, mirroring how lenient the original loader is.
//!
//! `COLORMAP` is a list of 256-byte remap tables. Entries 0..31 are the
//! diminishing brightness levels (0 = full bright), entry 32 is the
//! invulnerability (inverted) map and everything past that is extra maps the
//! original engine tolerates — commercial game data carries 34 in total.
//! The `colormap_index` lighting logic (which map to pick for a given light
//! level) is deliberately not here yet; this module only holds the data.

use super::error::{GfxError, GfxResult};

/// Size of one RGB color in the PLAYPAL lump.
const RGB_BYTES: usize = 3;

/// Number of colors in a palette (and in one COLORMAP remap).
pub const PALETTE_COLORS: usize = 256;
/// Bytes per palette in the PLAYPAL lump (256 * RGB).
pub const PALETTE_BYTES: usize = PALETTE_COLORS * RGB_BYTES;
/// Number of palettes vanilla ships in PLAYPAL.
pub const PLAYPAL_PALETTE_COUNT: usize = 14;

/// Bytes per remap table in the COLORMAP lump.
pub const COLORMAP_BYTES: usize = 256;
/// Number of diminishing-brightness maps at the start of the lump.
pub const NUM_DIMINISHING_COLORMAPS: usize = 32;
/// Index of the invulnerability (inverted) colormap. Vanilla calls this
/// `INVERSECOLORMAP`.
pub const INVULNERABILITY_COLORMAP: usize = NUM_DIMINISHING_COLORMAPS;
/// Index of the map used by the fuzzy (spectre/invisible) column drawer;
/// map 6, a bit brighter than average.
pub const FUZZ_COLORMAP: usize = 6;
/// Minimum number of maps the engine needs: the 32 diminishing levels plus
/// the invulnerability map.
pub const MIN_COLORMAPS: usize = INVULNERABILITY_COLORMAP + 1;

/// One 256-color RGB palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    /// The 256 palette colors, in palette index order.
    pub colors: [[u8; 3]; PALETTE_COLORS],
}

impl Palette {
    /// The RGB triplet for palette index `index`.
    pub fn color(&self, index: u8) -> [u8; 3] {
        self.colors[index as usize]
    }
}

/// One 256-entry colormap remap table.
///
/// Indexing it with a palette index yields the remapped palette index
/// (e.g. a brightness level or the inverted invulnerability mapping).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colormap(pub [u8; COLORMAP_BYTES]);

impl Colormap {
    /// The remapped palette index for `color`.
    pub fn lookup(&self, color: u8) -> u8 {
        self.0[color as usize]
    }
}

/// Decodes the PLAYPAL lump.
///
/// Fails unless the lump holds at least one palette and its size is a whole
/// multiple of the 768-byte palette size.
pub fn parse_playpal(data: &[u8]) -> GfxResult<Vec<Palette>> {
    if data.len() < PALETTE_BYTES {
        return Err(GfxError::BadSize {
            what: "PLAYPAL",
            size: data.len(),
            reason: "smaller than one 768-byte palette",
        });
    }
    if data.len() % PALETTE_BYTES != 0 {
        return Err(GfxError::BadSize {
            what: "PLAYPAL",
            size: data.len(),
            reason: "not a multiple of the 768-byte palette size",
        });
    }

    let palettes = data
        .chunks_exact(PALETTE_BYTES)
        .map(|chunk| {
            let mut colors = [[0u8; 3]; PALETTE_COLORS];
            for (color, bytes) in colors.iter_mut().zip(chunk.chunks_exact(RGB_BYTES)) {
                *color = [bytes[0], bytes[1], bytes[2]];
            }
            Palette { colors }
        })
        .collect();
    Ok(palettes)
}

/// Decodes the COLORMAP lump.
///
/// Fails unless the size is a whole multiple of 256 bytes and the lump
/// holds at least [`MIN_COLORMAPS`] remap tables. Extra tables past the
/// invulnerability map are kept as-is.
pub fn parse_colormap(data: &[u8]) -> GfxResult<Vec<Colormap>> {
    if data.len() % COLORMAP_BYTES != 0 {
        return Err(GfxError::BadSize {
            what: "COLORMAP",
            size: data.len(),
            reason: "not a multiple of the 256-byte colormap size",
        });
    }
    let count = data.len() / COLORMAP_BYTES;
    if count < MIN_COLORMAPS {
        return Err(GfxError::TooFewColormaps {
            count,
            minimum: MIN_COLORMAPS,
        });
    }

    let maps = data
        .chunks_exact(COLORMAP_BYTES)
        .map(|chunk| {
            let mut table = [0u8; COLORMAP_BYTES];
            table.copy_from_slice(chunk);
            Colormap(table)
        })
        .collect();
    Ok(maps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_playpal(num_palettes: usize) -> Vec<u8> {
        let mut data = vec![0u8; num_palettes * PALETTE_BYTES];
        for (i, byte) in data.iter_mut().enumerate() {
            // deterministic, patterned content so we can tell colors apart
            *byte = (i * 7 + 3) as u8;
        }
        data
    }

    #[test]
    fn playpal_roundtrip_14_palettes() {
        let data = synthetic_playpal(PLAYPAL_PALETTE_COUNT);
        let palettes = parse_playpal(&data).unwrap();
        assert_eq!(palettes.len(), PLAYPAL_PALETTE_COUNT);
        for (p, palette) in palettes.iter().enumerate() {
            for (c, color) in palette.colors.iter().enumerate() {
                let base = p * PALETTE_BYTES + c * RGB_BYTES;
                assert_eq!(color, &[data[base], data[base + 1], data[base + 2]]);
            }
        }
    }

    #[test]
    fn playpal_accepts_non_vanilla_palette_counts() {
        // one palette and 34 palettes both decode fine; vanilla is just the
        // 14-palette shipment, not a hard constraint of the format
        assert_eq!(parse_playpal(&synthetic_playpal(1)).unwrap().len(), 1);
        assert_eq!(parse_playpal(&synthetic_playpal(34)).unwrap().len(), 34);
    }

    #[test]
    fn playpal_rejects_too_small_and_unaligned_sizes() {
        let err = parse_playpal(&[0u8; PALETTE_BYTES - 1]).unwrap_err();
        assert!(matches!(
            err,
            GfxError::BadSize {
                what: "PLAYPAL",
                ..
            }
        ));

        let err = parse_playpal(&[0u8; PALETTE_BYTES + 1]).unwrap_err();
        assert!(matches!(
            err,
            GfxError::BadSize {
                what: "PLAYPAL",
                ..
            }
        ));

        // one byte over two whole palettes is still bad
        let err = parse_playpal(&[0u8; 2 * PALETTE_BYTES + 1]).unwrap_err();
        assert!(matches!(err, GfxError::BadSize { .. }));
    }

    #[test]
    fn playpal_empty_input_is_an_error_not_a_panic() {
        assert!(parse_playpal(&[]).is_err());
    }

    #[test]
    fn palette_color_lookup() {
        let palette = Palette {
            colors: [[9, 8, 7]; PALETTE_COLORS],
        };
        assert_eq!(palette.color(200), [9, 8, 7]);
        assert_eq!(palette.color(0), [9, 8, 7]);
    }

    fn synthetic_colormap(num_maps: usize) -> Vec<u8> {
        let mut data = vec![0u8; num_maps * COLORMAP_BYTES];
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = (i * 31 + 11) as u8;
        }
        data
    }

    #[test]
    fn colormap_roundtrip_minimum_33_maps() {
        let data = synthetic_colormap(MIN_COLORMAPS);
        let maps = parse_colormap(&data).unwrap();
        assert_eq!(maps.len(), MIN_COLORMAPS);
        for (m, map) in maps.iter().enumerate() {
            for (i, &value) in map.0.iter().enumerate() {
                assert_eq!(value, data[m * COLORMAP_BYTES + i]);
            }
        }
    }

    #[test]
    fn colormap_roundtrip_34_maps() {
        let data = synthetic_colormap(34);
        let maps = parse_colormap(&data).unwrap();
        assert_eq!(maps.len(), 34);
    }

    #[test]
    fn colormap_rejects_unaligned_sizes() {
        let err = parse_colormap(&[0u8; COLORMAP_BYTES + 1]).unwrap_err();
        assert!(matches!(
            err,
            GfxError::BadSize {
                what: "COLORMAP",
                ..
            }
        ));
    }

    #[test]
    fn colormap_rejects_too_few_maps() {
        let err = parse_colormap(&synthetic_colormap(NUM_DIMINISHING_COLORMAPS)).unwrap_err();
        assert!(matches!(
            err,
            GfxError::TooFewColormaps {
                count: 32,
                minimum: 33
            }
        ));

        // empty lump is also just "too few", not a panic
        assert!(matches!(
            parse_colormap(&[]).unwrap_err(),
            GfxError::TooFewColormaps { count: 0, .. }
        ));
    }

    #[test]
    fn colormap_lookup() {
        let mut table = [0u8; COLORMAP_BYTES];
        for (i, slot) in table.iter_mut().enumerate() {
            *slot = (255 - i) as u8;
        }
        let map = Colormap(table);
        assert_eq!(map.lookup(0), 255);
        assert_eq!(map.lookup(255), 0);
    }

    /// Deterministic byte mutator: whatever garbage we throw at the decoders,
    /// they must return `Err`, never panic.
    #[test]
    fn playpal_any_corruption_is_a_result_not_a_crash() {
        let data = synthetic_playpal(3);
        let mut state = 0xDEAD_BEEFu32;
        for i in 0..data.len() {
            for _ in 0..2 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let mut bad = data.clone();
                bad[i] ^= (state >> 9) as u8 | 0x01;
                let _ = parse_playpal(&bad);
            }
        }
    }

    #[test]
    fn colormap_any_corruption_is_a_result_not_a_crash() {
        let data = synthetic_colormap(33);
        let mut state = 0x0BADC0DEu32;
        for i in 0..data.len() {
            for _ in 0..2 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let mut bad = data.clone();
                bad[i] ^= (state >> 9) as u8 | 0x01;
                let _ = parse_colormap(&bad);
            }
        }
    }
}
