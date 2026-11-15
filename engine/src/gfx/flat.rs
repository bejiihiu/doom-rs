//! Flat (floor/ceiling texture) decoding.
//!
//! A flat is a raw 64x64 square of palette indices, row by row, with no
//! header of any kind — exactly 4096 bytes. The loader finds flats by the
//! `F_START`/`F_END` namespace markers in the WAD (see the `wad` module);
//! decoding one is just checking the size and copying the bytes.

use super::error::{GfxError, GfxResult};

/// Side length of a flat in pixels.
pub const FLAT_WIDTH: usize = 64;
/// Side length of a flat in pixels.
pub const FLAT_HEIGHT: usize = 64;
/// Size of a flat lump in bytes.
pub const FLAT_SIZE: usize = FLAT_WIDTH * FLAT_HEIGHT;

/// A decoded flat: 64x64 palette indices, row major.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flat(pub [u8; FLAT_SIZE]);

impl Flat {
    /// Decodes a flat lump. The size must be exactly [`FLAT_SIZE`] bytes.
    pub fn parse(data: &[u8]) -> GfxResult<Self> {
        if data.len() != FLAT_SIZE {
            return Err(GfxError::BadSize {
                what: "flat",
                size: data.len(),
                reason: "a flat must be exactly 4096 bytes (64x64)",
            });
        }
        let mut pixels = [0u8; FLAT_SIZE];
        pixels.copy_from_slice(data);
        Ok(Self(pixels))
    }

    /// The raw pixel array, row major.
    pub fn pixels(&self) -> &[u8; FLAT_SIZE] {
        &self.0
    }

    /// The palette index at `(x, y)`. No bounds checking — coordinates are
    /// assumed to be in range (`< 64`).
    pub fn pixel(&self, x: usize, y: usize) -> u8 {
        self.0[y * FLAT_WIDTH + x]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_well_formed_flat() {
        let data: Vec<u8> = (0..FLAT_SIZE).map(|i| (i * 13 + 7) as u8).collect();
        let flat = Flat::parse(&data).unwrap();
        assert_eq!(flat.pixels()[..], data[..]);
        assert_eq!(flat.pixel(0, 0), data[0]);
        assert_eq!(flat.pixel(63, 63), data[FLAT_SIZE - 1]);
        assert_eq!(flat.pixel(3, 2), data[2 * FLAT_WIDTH + 3]);
    }

    #[test]
    fn wrong_size_is_rejected() {
        assert!(matches!(
            Flat::parse(&[0u8; FLAT_SIZE - 1]).unwrap_err(),
            GfxError::BadSize { what: "flat", .. }
        ));
        assert!(matches!(
            Flat::parse(&[0u8; FLAT_SIZE + 1]).unwrap_err(),
            GfxError::BadSize { what: "flat", .. }
        ));
        assert!(Flat::parse(&[]).is_err());
    }

    #[test]
    fn any_corruption_is_a_result_not_a_crash() {
        let data: Vec<u8> = (0..FLAT_SIZE).map(|i| (i * 29 + 3) as u8).collect();
        let mut state = 0xF1A7_CAFE_u32;
        for _ in 0..512 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let i = (state as usize) % data.len();
            let mut bad = data.clone();
            bad[i] ^= (state >> 9) as u8 | 0x01;
            // flats have no structure, so corruption never errors — it just
            // yields a different (still valid) pixel pattern
            let _ = Flat::parse(&bad);
        }
    }
}
