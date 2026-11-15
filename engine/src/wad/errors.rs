//! Error type for WAD archive parsing and lookup.
//!
//! Every variant carries enough context (path, lump name, raw offsets) to
//! produce an actionable message. Bad archives are rejected at parse time;
//! lookups past the parsed directory report the requested index instead of
//! panicking.

use thiserror::Error;

/// Something went wrong while opening, validating or reading a WAD archive.
#[derive(Debug, Error)]
pub enum WadError {
    /// The file could not be read from disk.
    #[error("could not read {path}: {source}")]
    Io {
        /// Path that failed to read.
        path: String,
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// The file is smaller than a WAD header (12 bytes).
    #[error("{path}: file is {size} byte(s), smaller than a WAD header (12 bytes)")]
    TooSmall {
        /// File that was rejected.
        path: String,
        /// Actual file size in bytes.
        size: usize,
    },

    /// The magic bytes are not exactly `IWAD` or `PWAD`.
    ///
    /// Matching is strict on purpose, like the original: a homebrew file
    /// with `iwad` or `Pwad ` is not silently accepted.
    #[error("{path}: bad magic {magic}, expected IWAD or PWAD")]
    BadMagic {
        /// File that was rejected.
        path: String,
        /// The four bytes found at offset 0, escaped for readability.
        magic: String,
    },

    /// The header lists a negative number of lumps.
    #[error("{path}: negative lump count {num_lumps}")]
    NegativeLumpCount {
        /// File that was rejected.
        path: String,
        /// Raw on-disk value, before any clamping.
        num_lumps: i32,
    },

    /// The directory is empty.
    ///
    /// Vanilla tolerates a per-file zero-lump archive as long as some other
    /// file provides lumps, but a standalone empty WAD carries no information
    /// and is almost certainly a truncated download.
    #[error("{path}: WAD contains 0 lumps")]
    EmptyWad {
        /// File that was rejected.
        path: String,
    },

    /// The directory offset is negative, or the directory runs past EOF.
    ///
    /// This also covers `num_lumps * 16` overflowing a `usize`, so absurdly
    /// large lump counts are rejected instead of wrapping around.
    #[error(
        "{path}: directory (offset {dir_offset}, {num_lumps} lump(s)) does not fit in the {file_len}-byte file"
    )]
    DirectoryOutOfBounds {
        /// File that was rejected.
        path: String,
        /// Raw on-disk directory offset.
        dir_offset: i32,
        /// Raw on-disk lump count.
        num_lumps: i32,
        /// Actual file size in bytes.
        file_len: usize,
    },

    /// A lump's data range falls outside the file, or its fields are negative.
    #[error(
        "{path}: lump '{lump}' is out of bounds (position {file_pos} + size {size}, file is {file_len} bytes)"
    )]
    LumpOutOfBounds {
        /// File that was rejected.
        path: String,
        /// Name of the offending lump.
        lump: String,
        /// Raw on-disk position; may be negative.
        file_pos: i64,
        /// Raw on-disk size; may be negative.
        size: i64,
        /// Actual file size in bytes.
        file_len: usize,
    },

    /// A requested lump index does not exist in the file or set.
    #[error("lump index {index} out of bounds ({len} lump(s) loaded)")]
    LumpIndexOutOfBounds {
        /// Requested index.
        index: usize,
        /// Total number of lumps available.
        len: usize,
    },

    /// A named lookup failed.
    ///
    /// Vanilla treats this as fatal (`W_GetNumForName`); here it is a regular
    /// error so callers decide whether the game can live without the lump.
    #[error("lump '{name}' not found")]
    LumpNotFound {
        /// The looked-up name, as passed by the caller.
        name: String,
    },

    /// No WAD files were supplied at all.
    ///
    /// Mirrors vanilla's "no files found" fatal error.
    #[error("no WAD files were provided")]
    NoWadsLoaded,
}

/// Convenience alias for wad results.
pub type WadResult<T> = Result<T, WadError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_magic_message_shows_the_offset_bytes() {
        let err = WadError::BadMagic {
            path: "/tmp/fake.wad".into(),
            magic: "\"iwad\"".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("/tmp/fake.wad"), "missing path: {msg}");
        assert!(msg.contains("IWAD or PWAD"), "missing expectation: {msg}");
    }

    #[test]
    fn lump_out_of_bounds_message_names_the_lump() {
        let err = WadError::LumpOutOfBounds {
            path: "doom2.wad".into(),
            lump: "E1M1".into(),
            file_pos: 1 << 20,
            size: 1 << 20,
            file_len: 1024,
        };
        let msg = err.to_string();
        assert!(msg.contains("E1M1"), "missing lump name: {msg}");
        assert!(msg.contains("1024"), "missing file size: {msg}");
    }

    #[test]
    fn lump_not_found_echoes_the_query() {
        let err = WadError::LumpNotFound {
            name: "PLAYPAL".into(),
        };
        assert_eq!(err.to_string(), "lump 'PLAYPAL' not found");
    }
}
