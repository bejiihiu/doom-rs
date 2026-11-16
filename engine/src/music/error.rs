//! Error type for the music module.
//!
//! MUS and GENMIDI lumps are untrusted input: every decoder validates its
//! bytes and reports a [`MusicError`] instead of panicking, mirroring the
//! rest of the engine's lump decoders.

use thiserror::Error;

/// Errors returned by the MUS/GENMIDI decoders and the renderer.
#[derive(Debug, Error)]
pub enum MusicError {
    /// The input is smaller than the smallest possible file of its kind.
    #[error("truncated music data: need at least {needed} byte(s), got {available}")]
    Truncated {
        /// Bytes required to hold the structure being read.
        needed: usize,
        /// Bytes actually present.
        available: usize,
    },

    /// The leading magic bytes do not match the expected format.
    #[error("bad magic: expected {expected}, got {found:02x?}")]
    BadMagic {
        /// Human-readable description of the expected magic.
        expected: &'static str,
        /// The bytes actually found at the start of the input.
        found: Vec<u8>,
    },

    /// A field value is inconsistent with the format or with other fields.
    #[error("bad value at {context}: {detail}")]
    BadValue {
        /// Where in the data the problem was found.
        context: &'static str,
        /// What exactly is wrong with it.
        detail: String,
    },

    /// The score ends without a `Finish` event.
    #[error("unterminated music score: ran out of bytes before a Finish event")]
    Unterminated,

    /// The track produced no MIDI events to render.
    #[error("music track contains no renderable events")]
    EmptyTrack,

    /// The OPL renderer rejected the event stream.
    #[error("opl render failed: {0}")]
    Render(String),
}

impl From<MusicError> for crate::errors::EngineError {
    fn from(err: MusicError) -> Self {
        crate::errors::EngineError::Resource(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_carries_context() {
        let err = MusicError::BadValue {
            context: "header",
            detail: "score length overflows the file".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("header"));
        assert!(msg.contains("score length overflows the file"));
    }

    #[test]
    fn music_error_converts_into_engine_error() {
        let err = MusicError::EmptyTrack;
        let engine_err: crate::errors::EngineError = err.into();
        assert!(engine_err.to_string().contains("no renderable events"));
    }
}
