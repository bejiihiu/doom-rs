//! Logging setup for the engine.
//!
//! One call to [`init_logging`] installs a compact `tracing` subscriber on
//! the global default. The filter comes from the `RUST_LOG` environment
//! variable and falls back to `info` when it is missing or invalid. No file
//! output — the terminal is the log.
//!
//! Initialization is idempotent: the subscriber is installed behind a
//! [`std::sync::Once`], so repeat calls (tests, embedding tools) are a
//! no-op instead of a panic or a duplicate subscriber. If somebody else
//! installed a global subscriber first, we silently keep theirs.

use std::sync::Once;

use tracing_subscriber::EnvFilter;

static INIT: Once = Once::new();

/// Install the global subscriber exactly once.
///
/// Safe to call from multiple threads and multiple times; only the first
/// call has any effect. See the module docs for the filter semantics.
pub fn init_logging() {
    init_logging_with_default("info");
}

/// Install the global subscriber exactly once, choosing the fallback
/// filter level used when `RUST_LOG` is unset.
pub fn init_logging_with_default(default_filter: &str) {
    INIT.call_once(|| {
        let filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .compact()
            .try_init();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_logging_is_idempotent() {
        // must not panic on the second (or tenth) call
        init_logging();
        init_logging();
        init_logging();
    }
}
