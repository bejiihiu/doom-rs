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
}
