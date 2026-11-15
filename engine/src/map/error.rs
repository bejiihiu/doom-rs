//! Error type for map lump decoding and validation.
//!
//! Loading a map means reading a dozen lumps produced by an external
//! BSP builder, so corrupt and hand-edited files are the norm, not the
//! exception. Every decoder returns one of these errors instead of
//! panicking, and every index that reaches the engine later is known to
//! be in range by construction.

use thiserror::Error;

/// Errors produced by the map lump decoders.
#[derive(Debug, Error)]
pub enum MapError {
    /// A lump size does not divide evenly into the record stride, or is
    /// otherwise unusable (odd BLOCKMAP length, empty REJECT, ...).
    #[error("{lump}: invalid lump size {size}: {reason}")]
    BadSize {
        /// Which lump was being read.
        lump: &'static str,
        /// Actual lump size in bytes.
        size: usize,
        /// Why the size is invalid.
        reason: &'static str,
    },

    /// A field inside the structure holds a value the format only
    /// allows within some range (a zero BLOCKMAP grid dimension, a
    /// block list missing its terminator, ...).
    #[error("{lump}: invalid value: {reason}")]
    BadValue {
        /// Which lump the field belongs to.
        lump: &'static str,
        /// What was wrong with the value.
        reason: String,
    },

    /// An index stored on disk points past the end of the array it
    /// refers to (a LINEDEFS vertex reference, a SEGS linedef, ...).
    /// `index` is `i64` so negative on-disk values can be reported
    /// as-is instead of being mangled into huge positive numbers.
    #[error("{lump}: {what} index {index} out of range ({count} entries)")]
    OutOfRange {
        /// Which lump the reference came from.
        lump: &'static str,
        /// What the index points at.
        what: &'static str,
        /// The offending index, as read from the lump.
        index: i64,
        /// Number of valid entries.
        count: usize,
    },

    /// A bounds-checked read went out of bounds. Every read is
    /// pre-validated against the lump size, so this only fires when an
    /// invariant above is violated; keep the numbers instead of an
    /// unwrap.
    #[error(
        "{lump}: truncated, need {needed} byte(s) at offset {offset}, only {available} available"
    )]
    Truncated {
        /// Which lump was being read.
        lump: &'static str,
        /// Byte offset the read started at.
        offset: usize,
        /// Bytes the read wanted.
        needed: usize,
        /// Bytes actually left in the lump from `offset`.
        available: usize,
    },

    /// A bounds-checked read failed inside the shared `swap` helpers.
    /// Should be unreachable after the up-front size validation; kept
    /// so the decoders never have to unwrap.
    #[error(transparent)]
    Engine(#[from] crate::errors::EngineError),
}

/// Convenience alias for decoder results.
pub type MapResult<T> = Result<T, MapError>;

impl From<MapError> for crate::errors::EngineError {
    fn from(err: MapError) -> Self {
        // the map decoders run behind the resource cache, which only
        // speaks EngineError; keep the message, lose the variant
        Self::Resource(err.to_string())
    }
}
