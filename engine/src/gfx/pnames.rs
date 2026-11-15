//! PNAMES lump decoding, ported from `R_InitTextures` in `r_data.c`.
//!
//! PNAMES is the table of patch names that wall textures reference by
//! index. It holds one `i32` count followed by that many 8-byte names:
//!
//! ```text
//! offset  size  field
//! 0       4     number of names (little-endian i32)
//! 4       n*8   patch names, 8 bytes each, NUL padded
//! ```
//!
//! The original engine compares names with a case-insensitive 8-byte
//! strncasecmp and uppercases the query. Names here are normalized the
//! same way at load time (ASCII uppercase, cut at the first NUL), so
//! lookup is a plain string compare.

use super::error::{GfxError, GfxResult};
use super::normalize_name;
use crate::swap;

/// Size of one patch name entry in the lump.
pub const MAPNAME_SIZE: usize = 8;

/// The list of patch names referenced by TEXTURE1/TEXTURE2.
///
/// Names are stored normalized (uppercase, no NUL padding), in lump order.
/// Indices line up with the `patch` field of [`crate::gfx::texture::MapPatch`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pnames(Vec<String>);

impl Pnames {
    /// Decodes a PNAMES lump.
    ///
    /// The lump must hold the count and all `count * 8` name bytes; extra
    /// trailing bytes are ignored, same as the original loader.
    pub fn parse(data: &[u8]) -> GfxResult<Self> {
        if data.len() < 4 {
            return Err(GfxError::Truncated {
                what: "PNAMES",
                offset: 0,
                needed: 4,
                available: data.len(),
            });
        }
        let num = swap::read_i32(data, 0)?;
        if num < 0 {
            return Err(GfxError::BadValue {
                what: "PNAMES",
                reason: format!("negative name count {num}"),
            });
        }
        let num = num as usize;
        let table_size = num.checked_mul(MAPNAME_SIZE).ok_or(GfxError::BadValue {
            what: "PNAMES",
            reason: "name table size overflows usize".to_string(),
        })?;
        let needed = 4usize.checked_add(table_size).ok_or(GfxError::BadValue {
            what: "PNAMES",
            reason: "name table size overflows usize".to_string(),
        })?;
        if data.len() < needed {
            return Err(GfxError::Truncated {
                what: "PNAMES",
                offset: 4,
                needed: table_size,
                available: data.len() - 4,
            });
        }

        let mut names = Vec::with_capacity(num);
        for i in 0..num {
            let offset = 4 + i * MAPNAME_SIZE;
            let raw: [u8; MAPNAME_SIZE] = data[offset..offset + MAPNAME_SIZE]
                .try_into()
                .expect("slice of exactly MAPNAME_SIZE bytes");
            names.push(normalize_name(&raw));
        }
        Ok(Self(names))
    }

    /// Number of names in the table.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The normalized name at `index`, if it exists.
    pub fn get(&self, index: usize) -> Option<&str> {
        self.0.get(index).map(String::as_str)
    }

    /// All names, in lump order.
    pub fn names(&self) -> &[String] {
        &self.0
    }

    /// Looks up the index of `name`, case-insensitively.
    ///
    /// The query is normalized the same way the entries were: ASCII
    /// uppercase, cut at the first NUL, truncated to 8 bytes.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        let end = name
            .as_bytes()
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(name.len());
        let key = name.as_bytes()[..end]
            .iter()
            .take(MAPNAME_SIZE)
            .map(u8::to_ascii_uppercase)
            .collect::<Vec<u8>>();
        self.0
            .iter()
            .position(|entry| entry.as_bytes() == key.as_slice())
    }

    /// Whether `name` has an entry in the table, case-insensitively.
    pub fn contains(&self, name: &str) -> bool {
        self.index_of(name).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_pnames(names: &[&str]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&(names.len() as i32).to_le_bytes());
        for name in names {
            let mut raw = [0u8; MAPNAME_SIZE];
            for (dst, src) in raw.iter_mut().zip(name.as_bytes().iter()) {
                *dst = *src;
            }
            data.extend_from_slice(&raw);
        }
        data
    }

    #[test]
    fn parses_a_normal_table() {
        let data = build_pnames(&["TROOA1", "SKY1", "WOOD1"]);
        let pnames = Pnames::parse(&data).unwrap();
        assert_eq!(pnames.len(), 3);
        assert_eq!(pnames.get(0), Some("TROOA1"));
        assert_eq!(pnames.get(1), Some("SKY1"));
        assert_eq!(pnames.get(2), Some("WOOD1"));
        assert_eq!(pnames.get(3), None);
    }

    #[test]
    fn names_are_uppercased_and_trimmed() {
        let data = build_pnames(&["trooa1", "a\0padded"]);
        let pnames = Pnames::parse(&data).unwrap();
        assert_eq!(pnames.names(), &["TROOA1".to_string(), "A".to_string()]);
    }

    #[test]
    fn names_of_exactly_eight_bytes_keep_all_of_them() {
        let data = build_pnames(&["EIGHTBYTES".get(..8).unwrap()]);
        let pnames = Pnames::parse(&data).unwrap();
        assert_eq!(pnames.get(0), Some("EIGHTBYT"));
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let pnames = Pnames::parse(&build_pnames(&["TROOA1", "SKY1"])).unwrap();
        assert_eq!(pnames.index_of("trooa1"), Some(0));
        assert_eq!(pnames.index_of("TROOA1"), Some(0));
        assert_eq!(pnames.index_of("TrooA1"), Some(0));
        assert_eq!(pnames.index_of("sky1"), Some(1));
        assert_eq!(pnames.index_of("NOPE"), None);
        assert!(pnames.contains("SKY1"));
        assert!(!pnames.contains("NOPE"));
    }

    #[test]
    fn lookup_ignores_padding_and_truncates_to_eight_bytes() {
        let pnames = Pnames::parse(&build_pnames(&["SKY1"])).unwrap();
        // NUL-padded query matches the trimmed entry
        assert_eq!(pnames.index_of("SKY1\0\0\0\0"), Some(0));
        // overlong query is truncated to 8 bytes, like vanilla strncpy
        assert_eq!(pnames.index_of("SKY1EXTRA"), None);
    }

    #[test]
    fn empty_table_is_fine() {
        let pnames = Pnames::parse(&0i32.to_le_bytes()).unwrap();
        assert!(pnames.is_empty());
        assert_eq!(pnames.len(), 0);
    }

    #[test]
    fn negative_count_is_rejected() {
        let data = (-1i32).to_le_bytes();
        assert!(matches!(
            Pnames::parse(&data).unwrap_err(),
            GfxError::BadValue { .. }
        ));
    }

    #[test]
    fn absurd_count_does_not_overflow() {
        // i32::MAX names would need 16 GiB of table; the checked math must
        // turn it into a clean error instead of a wrap-around allocation
        let data = i32::MAX.to_le_bytes();
        assert!(Pnames::parse(&data).is_err());
    }

    #[test]
    fn count_larger_than_lump_is_rejected() {
        let mut data = build_pnames(&["A"]);
        // claim two names, ship only one
        data[..4].copy_from_slice(&2i32.to_le_bytes());
        assert!(matches!(
            Pnames::parse(&data).unwrap_err(),
            GfxError::Truncated { .. }
        ));
    }

    #[test]
    fn truncated_header_is_rejected() {
        assert!(matches!(
            Pnames::parse(&[0x01, 0x02]).unwrap_err(),
            GfxError::Truncated { .. }
        ));
        assert!(matches!(
            Pnames::parse(&[]).unwrap_err(),
            GfxError::Truncated { .. }
        ));
    }

    #[test]
    fn trailing_garbage_is_tolerated() {
        let mut data = build_pnames(&["A"]);
        data.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let pnames = Pnames::parse(&data).unwrap();
        assert_eq!(pnames.names(), &["A".to_string()]);
    }

    #[test]
    fn any_corruption_is_a_result_not_a_crash() {
        let data = build_pnames(&["TROOA1", "SKY1", "FLOOR4_8", "WOOD1"]);
        let mut state = 0x1BAD_B002u32;
        for i in 0..data.len() {
            for _ in 0..2 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let mut bad = data.clone();
                bad[i] ^= (state >> 9) as u8 | 0x01;
                let _ = Pnames::parse(&bad);
            }
        }
    }
}
