//! Startup configuration for the engine.
//!
//! This module only carries the paths the engine needs in order to bootstrap
//! itself. Runtime key/value settings (video, audio, controls) are layered
//! on top by the configuration system once it lands.

use std::path::PathBuf;

use crate::errors::{EngineError, EngineResult};

/// Configuration resolved before the engine starts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Path to the IWAD to load. `None` searches the standard install
    /// locations.
    pub iwad: Option<PathBuf>,
    /// PWAD files layered on top of the IWAD, in load order.
    pub pwads: Vec<PathBuf>,
    /// Directory where save games live. `None` uses the current directory.
    pub savegame_dir: Option<PathBuf>,
}

impl Config {
    /// Creates an empty configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` when an IWAD path was provided explicitly.
    pub fn has_iwad(&self) -> bool {
        self.iwad.is_some()
    }

    /// Checks the configuration for obvious problems.
    ///
    /// Returns the first problem found, or `Ok(())` when the configuration
    /// is usable as-is.
    pub fn validate(&self) -> EngineResult<()> {
        if let Some(dir) = &self.savegame_dir
            && dir.exists()
            && !dir.is_dir()
        {
            return Err(EngineError::Config(format!(
                "savegame_dir {} is not a directory",
                dir.display()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_empty_and_valid() {
        let config = Config::new();
        assert!(!config.has_iwad());
        assert!(config.pwads.is_empty());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn explicit_iwad_is_detected() {
        let config = Config {
            iwad: Some(PathBuf::from("/games/DOOM.WAD")),
            ..Config::new()
        };
        assert!(config.has_iwad());
    }

    #[test]
    fn validate_rejects_regular_file_as_savegame_dir() {
        let file =
            std::env::temp_dir().join(format!("doom-engine-test-{}.tmp", std::process::id()));
        std::fs::write(&file, b"not a directory").unwrap();

        let config = Config {
            savegame_dir: Some(file.clone()),
            ..Config::new()
        };
        let result = config.validate();
        assert!(result.is_err());

        let _ = std::fs::remove_file(&file);
    }
}
