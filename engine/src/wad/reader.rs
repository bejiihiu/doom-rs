//! Opening and validating a WAD archive.
//!
//! A WAD is a tiny container format: a 12-byte header, a bunch of lumps laid
//! out back to back, and a 16-byte-per-lump directory at the end (usually).
//! The whole file is read into memory and owned by [`WadFile`]; lump data is
//! served as slices straight into that buffer, so nothing is copied around.
//!
//! Layout reference (all integers little-endian):
//!
//! ```text
//! offset  size  field
//! 0       4     magic, exactly "IWAD" or "PWAD"
//! 4       4     number of directory entries
//! 8       4     offset of the directory
//! ...     ...   lump data, wherever the directory says it lives
//! dir     16 * num_lumps   directory entries: file_pos, size, name[8]
//! ```

use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};

use super::errors::{WadError, WadResult};

const HEADER_SIZE: usize = 12;
const DIRECTORY_ENTRY_SIZE: usize = 16;

/// Size of a lump name, in bytes, as stored on disk.
pub const LUMP_NAME_SIZE: usize = 8;

/// Which kind of WAD this is, taken from the magic bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WadKind {
    /// Registered or commercial game data (`IWAD`).
    Iwad,
    /// A patch / homebrew WAD (`PWAD`).
    Pwad,
}

impl WadKind {
    /// The magic bytes for this kind.
    pub const fn magic(self) -> &'static [u8; 4] {
        match self {
            Self::Iwad => b"IWAD",
            Self::Pwad => b"PWAD",
        }
    }
}

/// Parsed and validated WAD header.
///
/// The raw on-disk values are kept (`num_lumps` and `dir_offset` as `i32`,
/// matching the format), but once a [`WadHeader`] exists the values are
/// known-good: `num_lumps >= 1` and the whole directory fits inside the
/// file. Validation happens in [`WadFile`]'s parser, not here, so these
/// fields cannot be observed in an invalid state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WadHeader {
    /// `IWAD` or `PWAD`.
    pub id: WadKind,
    /// Number of lumps in the directory. Guaranteed `>= 1` after parsing.
    pub num_lumps: i32,
    /// Offset of the directory within the file. Guaranteed `>= 0` and large
    /// enough to hold the whole directory after parsing.
    pub dir_offset: i32,
}

/// An 8-byte lump name, normalized to ASCII uppercase and NUL-padded.
///
/// DOOM lump names are at most eight bytes. Vanilla uppercases only the
/// *query* and compares raw padded bytes, which means directory entries
/// written in lowercase silently stop matching; here the entries are
/// normalized at load time instead, so comparison is a plain byte-array
/// equality and works for any well-formed input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LumpName([u8; LUMP_NAME_SIZE]);

impl LumpName {
    /// Builds a normalized name from raw on-disk bytes (uppercase ASCII,
    /// NULs kept as padding).
    fn from_padded(bytes: [u8; LUMP_NAME_SIZE]) -> Self {
        Self(bytes.map(|b| b.to_ascii_uppercase()))
    }

    /// The raw, padded bytes as stored in the directory.
    pub fn as_bytes(&self) -> &[u8; LUMP_NAME_SIZE] {
        &self.0
    }

    /// The name as bytes up to the first NUL. Names without a NUL padding
    /// (exactly eight characters) come back in full.
    pub fn trimmed(&self) -> &[u8] {
        let end = self
            .0
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(LUMP_NAME_SIZE);
        &self.0[..end]
    }
}

impl From<&str> for LumpName {
    /// Normalizes a lookup string the same way directory entries are
    /// normalized: ASCII uppercase, truncated to eight bytes, NUL-padded.
    /// Truncation is by bytes, like vanilla's `strncpy(..., 8)`.
    fn from(value: &str) -> Self {
        let mut bytes = [0u8; LUMP_NAME_SIZE];
        for (dst, src) in bytes
            .iter_mut()
            .zip(value.as_bytes().iter().take(LUMP_NAME_SIZE))
        {
            *dst = src.to_ascii_uppercase();
        }
        Self(bytes)
    }
}

impl fmt::Display for LumpName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in self.trimmed() {
            if (0x20..0x7f).contains(&b) {
                f.write_char(b as char)?;
            } else {
                write!(f, "\\x{b:02X}")?;
            }
        }
        Ok(())
    }
}

/// A validated directory entry: where a lump's data lives and what it's called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    /// Offset of the lump's data within the WAD file.
    pub file_pos: usize,
    /// Size of the lump in bytes. Zero-sized lumps (namespace markers) are fine.
    pub size: usize,
    /// Normalized lump name.
    pub name: LumpName,
}

/// An opened WAD archive, held fully in memory and validated.
///
/// Use [`WadFile::open`] to load from disk or [`WadFile::from_bytes`] for an
/// owned buffer (handy for tests and non-file sources). Lump data is served
/// as slices into the internal buffer; nothing is copied.
#[derive(Debug)]
pub struct WadFile {
    path: Option<PathBuf>,
    data: Vec<u8>,
    header: WadHeader,
    directory: Vec<DirectoryEntry>,
}

impl WadFile {
    /// Reads and validates a WAD from disk.
    pub fn open(path: impl AsRef<Path>) -> WadResult<Self> {
        let path_ref = path.as_ref();
        let data = std::fs::read(path_ref).map_err(|source| WadError::Io {
            path: path_ref.display().to_string(),
            source,
        })?;
        Self::parse(data, Some(path_ref.to_path_buf()))
    }

    /// Validates a WAD from an owned in-memory buffer.
    pub fn from_bytes(bytes: Vec<u8>) -> WadResult<Self> {
        Self::parse(bytes, None)
    }

    fn parse(data: Vec<u8>, path: Option<PathBuf>) -> WadResult<Self> {
        let path_str = path
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<memory buffer>".to_string());

        if data.len() < HEADER_SIZE {
            return Err(WadError::TooSmall {
                path: path_str,
                size: data.len(),
            });
        }

        // safe: len checked against HEADER_SIZE above
        let magic: [u8; 4] = data[..4].try_into().unwrap_or([0; 4]);
        let kind = match &magic {
            b"IWAD" => WadKind::Iwad,
            b"PWAD" => WadKind::Pwad,
            _ => {
                return Err(WadError::BadMagic {
                    path: path_str,
                    magic: format!("{:?}", String::from_utf8_lossy(&magic)),
                });
            }
        };

        let num_lumps_raw = i32_at(&data, 4).ok_or(WadError::TooSmall {
            path: path_str.clone(),
            size: data.len(),
        })?;
        let dir_offset_raw = i32_at(&data, 8).ok_or(WadError::TooSmall {
            path: path_str.clone(),
            size: data.len(),
        })?;

        if num_lumps_raw < 0 {
            return Err(WadError::NegativeLumpCount {
                path: path_str,
                num_lumps: num_lumps_raw,
            });
        }
        if num_lumps_raw == 0 {
            return Err(WadError::EmptyWad { path: path_str });
        }

        let dir_out_of_bounds = || WadError::DirectoryOutOfBounds {
            path: path_str.clone(),
            dir_offset: dir_offset_raw,
            num_lumps: num_lumps_raw,
            file_len: data.len(),
        };

        let num_lumps = num_lumps_raw as usize;
        if dir_offset_raw < 0 {
            return Err(dir_out_of_bounds());
        }
        let dir_offset = dir_offset_raw as usize;
        let dir_len = num_lumps
            .checked_mul(DIRECTORY_ENTRY_SIZE)
            .ok_or_else(dir_out_of_bounds)?;
        let dir_end = dir_offset
            .checked_add(dir_len)
            .ok_or_else(dir_out_of_bounds)?;
        if dir_end > data.len() {
            return Err(dir_out_of_bounds());
        }

        let mut directory = Vec::with_capacity(num_lumps);
        for i in 0..num_lumps {
            let base = dir_offset + i * DIRECTORY_ENTRY_SIZE;
            let file_pos_raw = i32_at(&data, base).ok_or_else(dir_out_of_bounds)?;
            let size_raw = i32_at(&data, base + 4).ok_or_else(dir_out_of_bounds)?;
            let name_bytes: [u8; LUMP_NAME_SIZE] = data
                .get(base + 8..base + DIRECTORY_ENTRY_SIZE)
                .and_then(|s| s.try_into().ok())
                .ok_or_else(dir_out_of_bounds)?;
            let name = LumpName::from_padded(name_bytes);

            let out_of_bounds = || WadError::LumpOutOfBounds {
                path: path_str.clone(),
                lump: name.to_string(),
                file_pos: file_pos_raw as i64,
                size: size_raw as i64,
                file_len: data.len(),
            };

            if file_pos_raw < 0 || size_raw < 0 {
                return Err(out_of_bounds());
            }
            let file_pos = file_pos_raw as usize;
            let size = size_raw as usize;
            let end = file_pos.checked_add(size).ok_or_else(out_of_bounds)?;
            if end > data.len() {
                return Err(out_of_bounds());
            }

            directory.push(DirectoryEntry {
                file_pos,
                size,
                name,
            });
        }

        Ok(Self {
            path,
            data,
            header: WadHeader {
                id: kind,
                num_lumps: num_lumps_raw,
                dir_offset: dir_offset_raw,
            },
            directory,
        })
    }

    /// Path the WAD was loaded from, or `None` for in-memory buffers.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The validated header.
    pub fn header(&self) -> &WadHeader {
        &self.header
    }

    /// Convenience for `header().id`.
    pub fn kind(&self) -> WadKind {
        self.header.id
    }

    /// Number of lumps in the directory.
    pub fn lump_count(&self) -> usize {
        self.directory.len()
    }

    /// The validated directory, in file order.
    pub fn entries(&self) -> &[DirectoryEntry] {
        &self.directory
    }

    /// Raw bytes of the lump at `index`.
    ///
    /// The range was validated at parse time; it is checked again here so a
    /// slice can never go out of bounds, corrupt data or not.
    pub fn lump(&self, index: usize) -> WadResult<&[u8]> {
        let entry = self
            .directory
            .get(index)
            .ok_or(WadError::LumpIndexOutOfBounds {
                index,
                len: self.directory.len(),
            })?;
        self.data
            .get(entry.file_pos..entry.file_pos + entry.size)
            .ok_or(WadError::LumpOutOfBounds {
                path: self
                    .path
                    .as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<memory buffer>".to_string()),
                lump: entry.name.to_string(),
                file_pos: entry.file_pos as i64,
                size: entry.size as i64,
                file_len: self.data.len(),
            })
    }

    /// Finds the lump with `name`, searching from the end of the directory.
    ///
    /// This mirrors vanilla `W_CheckNumForName`, which scans backwards so
    /// patch WADs override the IWAD. Returns the index into [`entries`](Self::entries).
    pub fn find_lump(&self, name: &str) -> Option<usize> {
        let name = LumpName::from(name);
        self.directory.iter().rposition(|e| e.name == name)
    }
}

/// Reads a little-endian `i32` at `offset`, or `None` if out of bounds.
/// Total on purpose: no panics, no partially-read values.
fn i32_at(data: &[u8], offset: usize) -> Option<i32> {
    let end = offset.checked_add(4)?;
    let bytes = data.get(offset..end)?;
    Some(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wad::testutil::{build_wad, poke_i32};

    fn one_lump_wad(magic: &[u8; 4], name: &str, payload: &[u8]) -> Vec<u8> {
        build_wad(magic, &[(name, payload)])
    }

    fn dir_offset_of(blob: &[u8]) -> usize {
        i32_at(blob, 8).unwrap() as usize
    }

    #[test]
    fn parses_a_valid_iwad() {
        let blob = build_wad(
            b"IWAD",
            &[("PLAYPAL", b"\x01\x02\x03"), ("COLORMAP", b"\xaa")],
        );
        let wad = WadFile::from_bytes(blob).unwrap();
        assert_eq!(wad.kind(), WadKind::Iwad);
        assert_eq!(wad.header().num_lumps, 2);
        assert_eq!(wad.lump_count(), 2);
        assert_eq!(wad.entries()[0].name, LumpName::from("PLAYPAL"));
        assert_eq!(wad.entries()[0].file_pos, 12);
        assert_eq!(wad.entries()[0].size, 3);
        assert_eq!(wad.entries()[1].name, LumpName::from("COLORMAP"));
        assert_eq!(wad.lump(0).unwrap(), b"\x01\x02\x03");
        assert_eq!(wad.lump(1).unwrap(), b"\xaa");
    }

    #[test]
    fn parses_a_valid_pwad() {
        let blob = one_lump_wad(b"PWAD", "MYLEVEL", b"level data");
        let wad = WadFile::from_bytes(blob).unwrap();
        assert_eq!(wad.kind(), WadKind::Pwad);
        assert_eq!(wad.header().id, WadKind::Pwad);
    }

    #[test]
    fn wad_kind_magic_roundtrip() {
        assert_eq!(WadKind::Iwad.magic(), b"IWAD");
        assert_eq!(WadKind::Pwad.magic(), b"PWAD");
    }

    #[test]
    fn magic_must_be_strict_uppercase() {
        let blob = one_lump_wad(b"iwad", "A", b"x");
        let err = WadFile::from_bytes(blob).unwrap_err();
        assert!(matches!(err, WadError::BadMagic { .. }));

        let blob = one_lump_wad(b"pwad", "A", b"x");
        assert!(matches!(
            WadFile::from_bytes(blob).unwrap_err(),
            WadError::BadMagic { .. }
        ));
    }

    #[test]
    fn bad_magic_message_mentions_the_file() {
        let blob = one_lump_wad(b"XWAD", "A", b"x");
        let err = WadFile::from_bytes(blob).unwrap_err();
        assert!(err.to_string().contains("<memory buffer>"));
    }

    #[test]
    fn file_smaller_than_a_header_is_rejected() {
        let err = WadFile::from_bytes(b"IWAD".to_vec()).unwrap_err();
        assert!(matches!(err, WadError::TooSmall { size: 4, .. }));

        let err = WadFile::from_bytes(Vec::new()).unwrap_err();
        assert!(matches!(err, WadError::TooSmall { size: 0, .. }));
    }

    #[test]
    fn negative_lump_count_is_rejected() {
        let mut blob = one_lump_wad(b"IWAD", "A", b"x");
        poke_i32(&mut blob, 4, -1);
        let err = WadFile::from_bytes(blob).unwrap_err();
        assert!(matches!(
            err,
            WadError::NegativeLumpCount { num_lumps: -1, .. }
        ));
    }

    #[test]
    fn empty_directory_is_rejected() {
        let mut blob = one_lump_wad(b"IWAD", "A", b"x");
        poke_i32(&mut blob, 4, 0);
        let err = WadFile::from_bytes(blob).unwrap_err();
        assert!(matches!(err, WadError::EmptyWad { .. }));
    }

    #[test]
    fn negative_directory_offset_is_rejected() {
        let mut blob = one_lump_wad(b"IWAD", "A", b"x");
        poke_i32(&mut blob, 8, -16);
        let err = WadFile::from_bytes(blob).unwrap_err();
        assert!(matches!(err, WadError::DirectoryOutOfBounds { .. }));
    }

    #[test]
    fn directory_offset_past_eof_is_rejected() {
        let mut blob = one_lump_wad(b"IWAD", "A", b"x");
        poke_i32(&mut blob, 8, i32::MAX);
        let err = WadFile::from_bytes(blob).unwrap_err();
        assert!(matches!(err, WadError::DirectoryOutOfBounds { .. }));
    }

    #[test]
    fn absurd_lump_count_does_not_overflow() {
        let mut blob = one_lump_wad(b"IWAD", "A", b"x");
        let blob_len = blob.len();
        // i32::MAX lumps * 16 bytes is way past EOF; the check must use
        // checked math and fail, not wrap around.
        poke_i32(&mut blob, 4, i32::MAX);
        poke_i32(&mut blob, 8, 12);
        let err = WadFile::from_bytes(blob).unwrap_err();
        match err {
            WadError::DirectoryOutOfBounds { file_len, .. } => assert_eq!(file_len, blob_len),
            other => panic!("expected DirectoryOutOfBounds, got {other:?}"),
        }
    }

    #[test]
    fn lump_range_past_eof_is_rejected_and_names_the_lump() {
        let mut blob = build_wad(b"IWAD", &[("BROKEN", b"payload")]);
        let dir = dir_offset_of(&blob);
        // position 1 + huge size, or anything past the file: rejected either way
        poke_i32(&mut blob, dir, 0x0100_0000);
        poke_i32(&mut blob, dir + 4, 0x0100_0000);
        let err = WadFile::from_bytes(blob).unwrap_err();
        match err {
            WadError::LumpOutOfBounds { lump, .. } => assert_eq!(lump, "BROKEN"),
            other => panic!("expected LumpOutOfBounds, got {other:?}"),
        }
    }

    #[test]
    fn lump_with_negative_position_or_size_is_rejected() {
        for (pos, size) in [(-1, 1), (0, -1)] {
            let mut blob = build_wad(b"IWAD", &[("BROKEN", b"payload")]);
            let dir = dir_offset_of(&blob);
            poke_i32(&mut blob, dir, pos);
            poke_i32(&mut blob, dir + 4, size);
            assert!(matches!(
                WadFile::from_bytes(blob).unwrap_err(),
                WadError::LumpOutOfBounds { .. }
            ));
        }
    }

    #[test]
    fn lump_ending_exactly_at_eof_is_accepted() {
        let payload = b"exactfit";
        let blob = one_lump_wad(b"IWAD", "EXACT", payload);
        let wad = WadFile::from_bytes(blob).unwrap();
        assert_eq!(wad.lump(0).unwrap(), payload);
    }

    #[test]
    fn zero_sized_lumps_are_accepted() {
        let blob = build_wad(
            b"IWAD",
            &[("S_START", b""), ("SPRT1", b"sprite"), ("S_END", b"")],
        );
        let wad = WadFile::from_bytes(blob).unwrap();
        assert_eq!(wad.entries()[0].size, 0);
        assert_eq!(wad.lump(0).unwrap(), b"");
    }

    #[test]
    fn name_of_exactly_eight_chars_has_no_nul_padding() {
        let blob = one_lump_wad(b"IWAD", "12345678", b"x");
        let wad = WadFile::from_bytes(blob).unwrap();
        let entry = &wad.entries()[0];
        assert_eq!(entry.name.as_bytes(), b"12345678");
        assert_eq!(entry.name.trimmed(), b"12345678");
    }

    #[test]
    fn short_names_are_nul_padded_on_disk() {
        let blob = one_lump_wad(b"IWAD", "AB", b"x");
        let dir = dir_offset_of(&blob);
        assert_eq!(&blob[dir + 8..dir + 16], b"AB\0\0\0\0\0\0");
        let wad = WadFile::from_bytes(blob).unwrap();
        assert_eq!(wad.entries()[0].name.as_bytes(), b"AB\0\0\0\0\0\0");
        assert_eq!(wad.entries()[0].name.trimmed(), b"AB");
    }

    #[test]
    fn lookup_is_case_insensitive_on_both_sides() {
        // lowercase entries in the directory, mixed-case query from the caller
        let blob = one_lump_wad(b"IWAD", "playpal", b"palette");
        let wad = WadFile::from_bytes(blob).unwrap();
        assert_eq!(wad.entries()[0].name, LumpName::from("PLAYPAL"));
        assert_eq!(wad.find_lump("playpal"), Some(0));
        assert_eq!(wad.find_lump("PLAYPAL"), Some(0));
        assert_eq!(wad.find_lump("PlayPal"), Some(0));
    }

    #[test]
    fn lookup_in_a_single_file_searches_from_the_end() {
        let blob = build_wad(b"IWAD", &[("FLAT1", b"first"), ("FLAT1", b"second")]);
        let wad = WadFile::from_bytes(blob).unwrap();
        assert_eq!(wad.find_lump("FLAT1"), Some(1));
        assert_eq!(
            wad.lump(wad.find_lump("FLAT1").unwrap()).unwrap(),
            b"second"
        );
    }

    #[test]
    fn name_longer_than_eight_bytes_is_truncated() {
        // matches vanilla's strncpy(..., 8) behavior on the query side
        assert_eq!(LumpName::from("123456789"), LumpName::from("12345678"));
        assert_eq!(LumpName::from("").as_bytes(), &[0; 8]);
    }

    #[test]
    fn lump_index_out_of_bounds_is_an_error_not_a_panic() {
        let blob = one_lump_wad(b"IWAD", "A", b"x");
        let wad = WadFile::from_bytes(blob).unwrap();
        assert!(matches!(
            wad.lump(5).unwrap_err(),
            WadError::LumpIndexOutOfBounds { index: 5, len: 1 }
        ));
    }

    #[test]
    fn find_lump_returns_none_for_missing_names() {
        let blob = one_lump_wad(b"IWAD", "A", b"x");
        let wad = WadFile::from_bytes(blob).unwrap();
        assert_eq!(wad.find_lump("NOPE"), None);
    }

    #[test]
    fn open_reads_and_validates_from_disk() {
        let blob = build_wad(b"PWAD", &[("TEST", b"hello")]);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "doom_engine_wad_test_{}_{}.wad",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, &blob).unwrap();

        let wad = WadFile::open(&path).unwrap();
        assert_eq!(wad.kind(), WadKind::Pwad);
        assert_eq!(wad.path(), Some(path.as_path()));
        assert_eq!(wad.lump(0).unwrap(), b"hello");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_reports_the_path_on_io_failure() {
        let path = std::env::temp_dir().join("doom_engine_wad_test_definitely_missing.wad");
        let err = WadFile::open(&path).unwrap_err();
        match err {
            WadError::Io { path: p, .. } => assert!(p.contains("definitely_missing")),
            other => panic!("expected Io, got {other:?}"),
        }
    }
}
