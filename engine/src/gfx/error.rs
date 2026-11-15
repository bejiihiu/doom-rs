//! Error type for graphics data decoding.
//!
//! Decoding is pure: it takes the raw lump bytes and hands back typed
//! structures, or one of these errors. Nothing here touches the WAD, the
//! resource cache, or the renderer, so the errors carry enough context for a
//! human to figure out which structure rejected the bytes.

use thiserror::Error;

/// Errors produced by the graphics data decoders.
#[derive(Debug, Error)]
pub enum GfxError {
    /// The lump (or a structure inside it) is smaller than the format
    /// requires. Carries where the read started, how many bytes it wanted
    /// and how many were actually left.
    #[error(
        "{what}: truncated, need {needed} byte(s) at offset {offset}, only {available} available"
    )]
    Truncated {
        /// Which structure was being read.
        what: &'static str,
        /// Byte offset the read started at.
        offset: usize,
        /// Bytes the read wanted.
        needed: usize,
        /// Bytes actually left in the lump from `offset`.
        available: usize,
    },

    /// The lump size does not satisfy the format's rules (wrong multiple,
    /// too few entries, ...).
    #[error("{what}: invalid lump size {size}: {reason}")]
    BadSize {
        /// Which lump was being read.
        what: &'static str,
        /// Actual lump size in bytes.
        size: usize,
        /// Why the size is invalid.
        reason: &'static str,
    },

    /// A field inside the structure holds a value the format allows only
    /// within some range.
    #[error("{what}: invalid value: {reason}")]
    BadValue {
        /// Which structure the field belongs to.
        what: &'static str,
        /// What was wrong with the value.
        reason: String,
    },

    /// An offset inside the lump points outside of it. `offset` is `i64` so
    /// negative on-disk values can be reported as-is instead of being
    /// mangled into huge positive numbers.
    #[error("{what}: offset {offset} out of bounds for a {size}-byte lump")]
    OutOfBounds {
        /// Which offset was being followed.
        what: &'static str,
        /// The offending offset, as read from the lump.
        offset: i64,
        /// Size of the lump in bytes.
        size: usize,
    },

    /// Too few colormaps in the COLORMAP lump for the engine to light the
    /// scene. Vanilla expects at least 32 diminishing maps plus the
    /// invulnerability map.
    #[error("COLORMAP: only {count} colormap(s), at least {minimum} required")]
    TooFewColormaps {
        /// How many colormaps the lump actually holds.
        count: usize,
        /// Minimum the engine supports.
        minimum: usize,
    },

    /// A texture references a patch index that has no matching entry in the
    /// PNAMES table.
    #[error("texture {texture}: patch index {index} has no matching PNAMES entry")]
    MissingPatch {
        /// Name of the offending texture.
        texture: String,
        /// The out-of-range patch index, as stored on disk.
        index: i16,
    },

    /// A bounds-checked read went out of bounds. This happens when the
    /// decoders reuse the shared `swap` helpers; the engine error keeps the
    /// exact offset/needed/available numbers instead of collapsing them into
    /// a string.
    #[error(transparent)]
    Engine(#[from] crate::errors::EngineError),
}

/// Convenience alias for decoder results.
pub type GfxResult<T> = Result<T, GfxError>;

impl From<GfxError> for crate::errors::EngineError {
    fn from(err: GfxError) -> Self {
        // the graphics decoders run behind the resource cache, which only
        // speaks EngineError; keep the message, lose the variant
        Self::Resource(err.to_string())
    }
}
