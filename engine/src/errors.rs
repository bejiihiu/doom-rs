//! Engine error types.

use thiserror::Error;

/// Top-level error type for engine operations.
///
/// Fallible engine APIs return this type so callers have a single error to
/// handle instead of juggling per-module error enums.
#[derive(Debug, Error)]
pub enum EngineError {
    /// An underlying I/O error occurred.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A configuration value was rejected.
    #[error("invalid configuration: {0}")]
    Config(String),

    /// A resource could not be located or decoded.
    #[error("resource error: {0}")]
    Resource(String),

    /// A read from a lump buffer went out of bounds.
    ///
    /// Carries the requested `offset`, how many bytes were needed there and
    /// how many were actually available, so callers can report corrupt data
    /// precisely instead of guessing.
    #[error(
        "lump read out of bounds: offset {offset}, needed {needed} byte(s), only {available} available"
    )]
    LumpRead {
        /// Byte offset the read started at.
        offset: usize,
        /// Size of the requested read in bytes.
        needed: usize,
        /// Bytes actually left in the buffer from `offset`.
        available: usize,
    },

    /// A WAD archive failed to parse or a lump lookup failed.
    #[error("wad error: {0}")]
    Wad(#[from] crate::wad::WadError),
}

/// Convenience alias for engine results.
pub type EngineResult<T> = Result<T, EngineError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_converts_into_engine_error() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "missing file");
        let err: EngineError = io.into();
        assert!(err.to_string().contains("missing file"));
    }

    #[test]
    fn display_impls_include_the_context() {
        let err = EngineError::Config("bad value".into());
        assert_eq!(err.to_string(), "invalid configuration: bad value");

        let err = EngineError::Resource("lump PLAYPAL missing".into());
        assert_eq!(err.to_string(), "resource error: lump PLAYPAL missing");
    }

    #[test]
    fn lump_read_display_shows_the_bounds() {
        let err = EngineError::LumpRead {
            offset: 10,
            needed: 4,
            available: 2,
        };
        assert_eq!(
            err.to_string(),
            "lump read out of bounds: offset 10, needed 4 byte(s), only 2 available"
        );
    }
}
