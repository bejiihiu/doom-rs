//! Error types for the audio subsystem.
//!
//! Mirrors the per-module error style used elsewhere in the engine (see
//! `gfx::GfxError`): structured variants for corrupt data, a passthrough
//! for engine errors, and a `From` impl into [`crate::errors::EngineError`]
//! so callers keep one error type at the boundary.

use thiserror::Error;

use crate::errors::EngineError;

/// Errors produced by the audio subsystem.
#[derive(Debug, Error)]
pub enum AudioError {
    /// No default audio output device is available on this machine.
    ///
    /// Not fatal for the engine: the game can run without audio (and the
    /// test suite always uses the null backend instead of a real device).
    #[error("no default audio output device available")]
    NoOutputDevice,

    /// An error reported by the audio backend (device unavailable, stream
    /// invalidated, backend I/O, ...).
    #[error("audio backend error: {0}")]
    Backend(#[from] cpal::Error),

    /// The device only supports a sample format we do not convert.
    ///
    /// `F32`, `I16` and `U16` outputs are handled; anything else is
    /// rejected loudly instead of silently playing garbage.
    #[error("unsupported sample format '{0}'")]
    UnsupportedSampleFormat(String),

    /// A caller asked for a channel index outside the mixer.
    #[error("channel {channel} out of range ({num_channels} channels)")]
    ChannelOutOfRange {
        /// The requested channel index.
        channel: usize,
        /// Number of channels that exist.
        num_channels: usize,
    },

    /// A volume value outside the vanilla `0..=127` range.
    ///
    /// Vanilla `I_Error`'d on this; we return an error instead of
    /// panicking across an audio boundary.
    #[error("invalid volume {0} (expected 0..=127)")]
    InvalidVolume(i32),

    /// A DMX sound lump ended before the header was complete.
    #[error("DMX sound lump truncated: needed {needed} byte(s), only {available} available")]
    DmxTruncated {
        /// Bytes the decoder tried to read.
        needed: usize,
        /// Bytes actually present in the lump.
        available: usize,
    },

    /// The DMX format tag was not the digitized-sound tag (3).
    #[error("unsupported DMX format tag {0} (expected 3)")]
    DmxFormatTag(u16),

    /// The DMX header declared a sample rate of zero, which is meaningless.
    #[error("invalid DMX sample rate 0")]
    DmxBadSampleRate,

    /// The DMX header length and the lump size disagree in a way that
    /// does not match either the padded or the unpadded layout.
    #[error(
        "DMX sound lump size mismatch: header length field {header_length}, \
         {available} byte(s) available after the 8-byte base header"
    )]
    DmxSizeMismatch {
        /// Raw 32-bit length field from the header (samples + 32 in the
        /// officially padded files).
        header_length: u32,
        /// Bytes present after the 8-byte base header.
        available: usize,
    },

    /// A linked sfx chain was self-referential or too deep.
    #[error("linked sfx chain for '{0}' loops or exceeds the depth cap")]
    LinkCycle(&'static str),

    /// Neither the sfx's own lump nor the vanilla fallback (`DSPISTOL`)
    /// could be found in the WAD set.
    #[error("sound lump for '{0}' not found (and no DSPISTOL fallback available)")]
    MissingSound(String),

    /// Passthrough for engine errors raised while reading lumps.
    #[error(transparent)]
    Engine(#[from] EngineError),
}

/// Convenience alias for audio results.
pub type AudioResult<T> = Result<T, AudioError>;

impl From<AudioError> for EngineError {
    fn from(err: AudioError) -> Self {
        match err {
            AudioError::Engine(inner) => inner,
            other => EngineError::Resource(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_error_into_engine_error_preserves_resource_context() {
        let err = AudioError::MissingSound("pistlend".into());
        let engine: EngineError = err.into();
        assert!(engine.to_string().contains("pistlend"));
    }

    #[test]
    fn engine_error_passthrough_is_not_wrapped_twice() {
        let inner = EngineError::Resource("boom".into());
        let err = AudioError::Engine(inner);
        let engine: EngineError = err.into();
        assert_eq!(engine.to_string(), "resource error: boom");
    }
}
