//! Configuration for the engine.
//!
//! Two layers live here:
//!
//! - [`Config`] — the small set of paths the engine needs in order to
//!   bootstrap itself (IWAD/PWAD/savegames). It was here from the start;
//!   its API is unchanged.
//! - [`DoomConfig`] — a port of `M_LoadDefaults`/`M_SaveDefaults` from
//!   `m_misc.c`: the classic `doom.cfg` key/value file with typed accessors
//!   and defaults, just without the original's global variables and raw
//!   pointer casts.
//!
//! File format (compatible with the original):
//!
//! - one `key value` pair per line, key and value separated by whitespace;
//! - string values are wrapped in double quotes: `chatmacro4 "Help!"`;
//! - hexadecimal integers are written `0x`-prefixed: `mouse_sensitivity 5`;
//! - lines whose first non-whitespace characters are `///` are comments and
//!   are skipped; an inline `///` comment after an unquoted value is fine
//!   too: `usegamma 0 /// hardware gamma`;
//! - unknown keys are ignored (with a warning), mirroring vanilla — a typo
//!   never aborts the game;
//! - a missing file is not an error: every value stays at its default.
//!
//! The table of known keys and their defaults is a byte-for-byte port of
//! the `defaults[]` table in `m_misc.c` (stock linux build: `NORMALUNIX`
//! keys included, plus the `LINUX`/`SNDSERV` legacy entries, which we keep
//! only so old config files round-trip).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tracing::warn;

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

/// Whether a known config key holds an integer or a string.
///
/// This is what vanilla derived from the magnitude of the default value
/// (`> -0xfff && < 0xfff` meant integer); we just say it out loud.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefaultKind {
    Int(i64),
    Str(&'static str),
}

struct DefaultEntry {
    name: &'static str,
    kind: DefaultKind,
}

/// The `defaults[]` table, in file order — saves come out in the same order
/// every time. Values are the ones from `m_misc.c`; `KEY_*` names were
/// resolved to their numeric values from `doomdef.h`.
const DEFAULTS: &[DefaultEntry] = &[
    DefaultEntry {
        name: "mouse_sensitivity",
        kind: DefaultKind::Int(5),
    },
    DefaultEntry {
        name: "sfx_volume",
        kind: DefaultKind::Int(8),
    },
    DefaultEntry {
        name: "music_volume",
        kind: DefaultKind::Int(8),
    },
    DefaultEntry {
        name: "show_messages",
        kind: DefaultKind::Int(1),
    },
    DefaultEntry {
        name: "key_right",
        kind: DefaultKind::Int(0xae),
    },
    DefaultEntry {
        name: "key_left",
        kind: DefaultKind::Int(0xac),
    },
    DefaultEntry {
        name: "key_up",
        kind: DefaultKind::Int(0xad),
    },
    DefaultEntry {
        name: "key_down",
        kind: DefaultKind::Int(0xaf),
    },
    DefaultEntry {
        name: "key_strafeleft",
        kind: DefaultKind::Int(44),
    }, // ','
    DefaultEntry {
        name: "key_straferight",
        kind: DefaultKind::Int(46),
    }, // '.'
    DefaultEntry {
        name: "key_fire",
        kind: DefaultKind::Int(0x9d),
    },
    DefaultEntry {
        name: "key_use",
        kind: DefaultKind::Int(32),
    }, // ' '
    DefaultEntry {
        name: "key_strafe",
        kind: DefaultKind::Int(0xb8),
    },
    DefaultEntry {
        name: "key_speed",
        kind: DefaultKind::Int(0xb6),
    },
    // legacy SNDSERV entries, kept so old configs round-trip
    DefaultEntry {
        name: "sndserver",
        kind: DefaultKind::Str("sndserver"),
    },
    DefaultEntry {
        name: "mb_used",
        kind: DefaultKind::Int(2),
    },
    // legacy LINUX entries, kept so old configs round-trip
    DefaultEntry {
        name: "mousedev",
        kind: DefaultKind::Str("/dev/ttyS0"),
    },
    DefaultEntry {
        name: "mousetype",
        kind: DefaultKind::Str("microsoft"),
    },
    DefaultEntry {
        name: "use_mouse",
        kind: DefaultKind::Int(1),
    },
    DefaultEntry {
        name: "mouseb_fire",
        kind: DefaultKind::Int(0),
    },
    DefaultEntry {
        name: "mouseb_strafe",
        kind: DefaultKind::Int(1),
    },
    DefaultEntry {
        name: "mouseb_forward",
        kind: DefaultKind::Int(2),
    },
    DefaultEntry {
        name: "use_joystick",
        kind: DefaultKind::Int(0),
    },
    DefaultEntry {
        name: "joyb_fire",
        kind: DefaultKind::Int(0),
    },
    DefaultEntry {
        name: "joyb_strafe",
        kind: DefaultKind::Int(1),
    },
    DefaultEntry {
        name: "joyb_use",
        kind: DefaultKind::Int(3),
    },
    DefaultEntry {
        name: "joyb_speed",
        kind: DefaultKind::Int(2),
    },
    DefaultEntry {
        name: "screenblocks",
        kind: DefaultKind::Int(9),
    },
    DefaultEntry {
        name: "detaillevel",
        kind: DefaultKind::Int(0),
    },
    DefaultEntry {
        name: "snd_channels",
        kind: DefaultKind::Int(3),
    },
    DefaultEntry {
        name: "usegamma",
        kind: DefaultKind::Int(0),
    },
    DefaultEntry {
        name: "chatmacro0",
        kind: DefaultKind::Str("No"),
    },
    DefaultEntry {
        name: "chatmacro1",
        kind: DefaultKind::Str("I'm ready to kick butt!"),
    },
    DefaultEntry {
        name: "chatmacro2",
        kind: DefaultKind::Str("I'm OK."),
    },
    DefaultEntry {
        name: "chatmacro3",
        kind: DefaultKind::Str("I'm not looking too good!"),
    },
    DefaultEntry {
        name: "chatmacro4",
        kind: DefaultKind::Str("Help!"),
    },
    DefaultEntry {
        name: "chatmacro5",
        kind: DefaultKind::Str("You suck!"),
    },
    DefaultEntry {
        name: "chatmacro6",
        kind: DefaultKind::Str("Next time, scumbag..."),
    },
    DefaultEntry {
        name: "chatmacro7",
        kind: DefaultKind::Str("Come here!"),
    },
    DefaultEntry {
        name: "chatmacro8",
        kind: DefaultKind::Str("I'll take care of it."),
    },
    DefaultEntry {
        name: "chatmacro9",
        kind: DefaultKind::Str("Yes"),
    },
];

/// Parse an integer token, accepting decimal and `0x`-prefixed hex like the
/// original's `sscanf(strparm, "%i", ...)` did.
fn parse_int(raw: &str) -> Option<i64> {
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16).ok()
    } else {
        raw.parse::<i64>().ok()
    }
}

/// A stored value is kept as text (same shape the file has); typed
/// accessors re-interpret it and fall back to defaults when they can't.
type RawValue = String;

fn raw_from_kind(kind: &DefaultKind) -> RawValue {
    match kind {
        DefaultKind::Int(n) => n.to_string(),
        DefaultKind::Str(s) => s.to_string(),
    }
}

fn normalize(kind: &DefaultKind, raw: &str) -> RawValue {
    match kind {
        // canonical decimal form so saves are stable; garbage is preserved
        // verbatim (ugly file, but the user's data stays the user's data)
        DefaultKind::Int(_) => parse_int(raw)
            .map(|n| n.to_string())
            .unwrap_or_else(|| raw.to_string()),
        DefaultKind::Str(_) => raw.to_string(),
    }
}

enum Line {
    /// comment or blank line
    Skip,
    /// (key, unquoted value; the caller still has to cut inline comments)
    Intish(String, String),
    /// (key, string value with quotes stripped)
    Str(String, String),
    Malformed,
}

fn parse_line(line: &str) -> Line {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with("///") {
        return Line::Skip;
    }

    let split = trimmed
        .find(char::is_whitespace)
        .map(|i| (&trimmed[..i], trimmed[i..].trim_start()));
    let Some((key, rest)) = split else {
        return Line::Malformed;
    };
    if rest.is_empty() {
        return Line::Malformed;
    }

    if let Some(quoted) = rest.strip_prefix('"') {
        // string value: read until the closing quote, ignore whatever is
        // after it (vanilla did the same)
        let Some(close) = quoted.find('"') else {
            return Line::Malformed;
        };
        let inner = &quoted[..close];
        return Line::Str(key.to_string(), inner.to_string());
    }

    // unquoted: an inline `///` comment ends the value
    let value = match rest.find("///") {
        Some(i) => rest[..i].trim_end(),
        None => rest,
    };
    if value.is_empty() {
        return Line::Malformed;
    }
    Line::Intish(key.to_string(), value.to_string())
}

/// Runtime configuration ported from `M_LoadDefaults`/`M_SaveDefaults`.
///
/// Starts fully seeded with defaults (vanilla's "set everything to base
/// values" pass); [`load`](Self::load) / [`load_from_str`](Self::load_from_str)
/// then override from a file. Saves go back out in the canonical table
/// order. Unknown keys in files are dropped, like vanilla.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoomConfig {
    /// Raw values, keyed by config name. Always holds every known key.
    values: HashMap<String, RawValue>,
}

impl Default for DoomConfig {
    fn default() -> Self {
        let values = DEFAULTS
            .iter()
            .map(|e| (e.name.to_string(), raw_from_kind(&e.kind)))
            .collect();
        Self { values }
    }
}

impl DoomConfig {
    fn entry(key: &str) -> Option<&'static DefaultEntry> {
        DEFAULTS.iter().find(|e| e.name == key)
    }

    /// Load defaults from a file, overriding only what the file mentions.
    ///
    /// A missing file is not an error (vanilla behaved the same); an
    /// unreadable file is.
    pub fn load(path: &Path) -> EngineResult<Self> {
        match std::fs::read_to_string(path) {
            Ok(content) => Ok(Self::load_from_str(&content)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(EngineError::Io(e)),
        }
    }

    /// Load defaults from a string, overriding only what the string
    /// mentions. Malformed lines and unknown keys are logged and skipped.
    pub fn load_from_str(content: &str) -> Self {
        let mut config = Self::default();
        for line in content.lines() {
            let (key, raw) = match parse_line(line) {
                Line::Skip => continue,
                Line::Malformed => {
                    warn!("config: skipping malformed line: {line:?}");
                    continue;
                }
                Line::Intish(k, v) | Line::Str(k, v) => (k, v),
            };
            match Self::entry(&key) {
                Some(entry) => {
                    config
                        .values
                        .insert(key.clone(), normalize(&entry.kind, &raw));
                }
                None => warn!("config: ignoring unknown key {key:?}"),
            }
        }
        config
    }

    /// Serialize to the canonical `doom.cfg` format: one `key value` pair
    /// per line, strings quoted, integers bare, table order.
    pub fn serialize(&self) -> String {
        let mut out = String::with_capacity(512);
        for entry in DEFAULTS {
            // every known key is always present — seeded at construction
            let raw = &self.values[entry.name];
            match &entry.kind {
                DefaultKind::Int(_) => {
                    out.push_str(&format!("{}\t\t{}\n", entry.name, raw));
                }
                DefaultKind::Str(_) => {
                    out.push_str(&format!("{}\t\t\"{}\"\n", entry.name, raw));
                }
            }
        }
        out
    }

    /// Write the config to a file in the canonical format.
    ///
    /// The payload is staged in a managed scratch file next to `path`
    /// and renamed into place, so a crash mid-write cannot leave a
    /// half-written config behind.
    pub fn save(&self, path: &Path) -> EngineResult<()> {
        crate::scratch::atomic_write(path, self.serialize().as_bytes())
    }

    /// Integer accessor with a fallback to the key's default.
    ///
    /// Unknown keys, missing values and values that do not fit an `i32`
    /// all yield the default (or 0 for keys that do not exist at all).
    pub fn get_i32(&self, key: &str) -> i32 {
        let value = self.values.get(key);
        let default = Self::entry(key).map(|e| match e.kind {
            DefaultKind::Int(n) => n,
            // a string default read as an integer is nonsense; fall back to 0
            DefaultKind::Str(_) => 0,
        });
        value
            .and_then(|raw| parse_int(raw))
            .and_then(|n| i32::try_from(n).ok())
            .or(default.map(|n| n.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32))
            .unwrap_or(0)
    }

    /// Boolean accessor (`!= 0`), same fallback semantics as [`get_i32`](Self::get_i32).
    pub fn get_bool(&self, key: &str) -> bool {
        self.get_i32(key) != 0
    }

    /// String accessor. Unknown keys yield an empty string (with a warning).
    pub fn get_string(&self, key: &str) -> String {
        match self.values.get(key) {
            Some(raw) => raw.clone(),
            None => {
                warn!("config: unknown key {key:?}");
                String::new()
            }
        }
    }

    /// Set an integer key. Fails when the key is unknown or holds a string.
    pub fn set_i32(&mut self, key: &str, value: i32) -> EngineResult<()> {
        match Self::entry(key) {
            Some(DefaultEntry {
                kind: DefaultKind::Int(_),
                ..
            }) => {
                self.values.insert(key.to_string(), value.to_string());
                Ok(())
            }
            Some(_) => Err(EngineError::Config(format!(
                "config key {key:?} holds a string, cannot set an integer"
            ))),
            None => Err(EngineError::Config(format!("unknown config key {key:?}"))),
        }
    }

    /// Set a boolean key (stored as 0/1). Fails when the key is unknown or
    /// holds a string.
    pub fn set_bool(&mut self, key: &str, value: bool) -> EngineResult<()> {
        self.set_i32(key, i32::from(value))
    }

    /// Set a string key. Fails when the key is unknown or holds an integer,
    /// or when the value contains a quote or a newline (would corrupt the
    /// file format).
    pub fn set_string(&mut self, key: &str, value: &str) -> EngineResult<()> {
        match Self::entry(key) {
            Some(DefaultEntry {
                kind: DefaultKind::Str(_),
                ..
            }) => {}
            Some(_) => {
                return Err(EngineError::Config(format!(
                    "config key {key:?} holds an integer, cannot set a string"
                )));
            }
            None => {
                return Err(EngineError::Config(format!("unknown config key {key:?}")));
            }
        }
        if value.contains(['"', '\n', '\r']) {
            return Err(EngineError::Config(format!(
                "config string for {key:?} contains a quote or newline"
            )));
        }
        self.values.insert(key.to_string(), value.to_string());
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

    #[test]
    fn defaults_match_the_original_table() {
        let cfg = DoomConfig::default();
        assert_eq!(cfg.get_i32("mouse_sensitivity"), 5);
        assert_eq!(cfg.get_i32("sfx_volume"), 8);
        assert_eq!(cfg.get_i32("music_volume"), 8);
        assert_eq!(cfg.get_i32("show_messages"), 1);
        assert_eq!(cfg.get_i32("key_right"), 0xae);
        assert_eq!(cfg.get_i32("key_left"), 0xac);
        assert_eq!(cfg.get_i32("key_up"), 0xad);
        assert_eq!(cfg.get_i32("key_down"), 0xaf);
        assert_eq!(cfg.get_i32("key_strafeleft"), 44);
        assert_eq!(cfg.get_i32("key_straferight"), 46);
        assert_eq!(cfg.get_i32("key_fire"), 157);
        assert_eq!(cfg.get_i32("key_use"), 32);
        assert_eq!(cfg.get_i32("key_strafe"), 184);
        assert_eq!(cfg.get_i32("key_speed"), 182);
        assert_eq!(cfg.get_i32("use_mouse"), 1);
        assert_eq!(cfg.get_i32("mouseb_fire"), 0);
        assert_eq!(cfg.get_i32("mouseb_strafe"), 1);
        assert_eq!(cfg.get_i32("mouseb_forward"), 2);
        assert_eq!(cfg.get_i32("use_joystick"), 0);
        assert_eq!(cfg.get_i32("joyb_fire"), 0);
        assert_eq!(cfg.get_i32("joyb_strafe"), 1);
        assert_eq!(cfg.get_i32("joyb_use"), 3);
        assert_eq!(cfg.get_i32("joyb_speed"), 2);
        assert_eq!(cfg.get_i32("screenblocks"), 9);
        assert_eq!(cfg.get_i32("detaillevel"), 0);
        assert_eq!(cfg.get_i32("snd_channels"), 3);
        assert_eq!(cfg.get_i32("usegamma"), 0);
        assert_eq!(cfg.get_i32("mb_used"), 2);
    }

    #[test]
    fn string_defaults_match_the_original_table() {
        let cfg = DoomConfig::default();
        assert_eq!(cfg.get_string("sndserver"), "sndserver");
        assert_eq!(cfg.get_string("mousedev"), "/dev/ttyS0");
        assert_eq!(cfg.get_string("mousetype"), "microsoft");
        assert_eq!(cfg.get_string("chatmacro0"), "No");
        assert_eq!(cfg.get_string("chatmacro1"), "I'm ready to kick butt!");
        assert_eq!(cfg.get_string("chatmacro2"), "I'm OK.");
        assert_eq!(cfg.get_string("chatmacro3"), "I'm not looking too good!");
        assert_eq!(cfg.get_string("chatmacro4"), "Help!");
        assert_eq!(cfg.get_string("chatmacro5"), "You suck!");
        assert_eq!(cfg.get_string("chatmacro6"), "Next time, scumbag...");
        assert_eq!(cfg.get_string("chatmacro7"), "Come here!");
        assert_eq!(cfg.get_string("chatmacro8"), "I'll take care of it.");
        assert_eq!(cfg.get_string("chatmacro9"), "Yes");
    }

    #[test]
    fn bool_accessor_reads_nonzero_as_true() {
        let mut cfg = DoomConfig::default();
        assert!(cfg.get_bool("show_messages"));
        cfg.set_bool("show_messages", false).unwrap();
        assert!(!cfg.get_bool("show_messages"));
        cfg.set_bool("show_messages", true).unwrap();
        assert!(cfg.get_bool("show_messages"));
    }

    #[test]
    fn load_overrides_only_what_the_file_mentions() {
        let cfg = DoomConfig::load_from_str("sfx_volume 3\n");
        assert_eq!(cfg.get_i32("sfx_volume"), 3);
        assert_eq!(cfg.get_i32("music_volume"), 8); // untouched default
    }

    #[test]
    fn load_handles_hex_and_string_values() {
        let cfg = DoomConfig::load_from_str("key_fire 0x101\nchatmacro4 \"Send help\"\n");
        assert_eq!(cfg.get_i32("key_fire"), 0x101);
        assert_eq!(cfg.get_string("chatmacro4"), "Send help");
    }

    #[test]
    fn load_skips_comments_and_malformed_lines() {
        let cfg = DoomConfig::load_from_str(
            "/// header comment\n\
             \n\
                sfx_volume 4 /// inline comment\n\
                garbage line with no value\n\
                \"unterminated string\n\
                music_volume 2\n",
        );
        assert_eq!(cfg.get_i32("sfx_volume"), 4);
        assert_eq!(cfg.get_i32("music_volume"), 2);
    }

    #[test]
    fn load_ignores_unknown_keys() {
        let cfg = DoomConfig::load_from_str("totally_made_up_key 123\nsfx_volume 6\n");
        assert_eq!(cfg.get_i32("sfx_volume"), 6);
    }

    #[test]
    fn load_tolerates_garbage_int_values() {
        let cfg = DoomConfig::load_from_str("sfx_volume loud\n");
        assert_eq!(cfg.get_i32("sfx_volume"), 8); // falls back to default
    }

    #[test]
    fn missing_file_yields_defaults() {
        let path = std::env::temp_dir().join(format!(
            "doom-engine-no-such-config-{}.cfg",
            std::process::id()
        ));
        let cfg = DoomConfig::load(&path).unwrap();
        assert_eq!(cfg, DoomConfig::default());
    }

    #[test]
    fn serialize_uses_tabs_and_quotes_strings() {
        let text = DoomConfig::default().serialize();
        assert!(text.contains("mouse_sensitivity\t\t5\n"));
        assert!(text.contains("chatmacro1\t\t\"I'm ready to kick butt!\"\n"));
        assert!(text.contains("usegamma\t\t0\n"));
    }

    #[test]
    fn tmp_file_round_trip_preserves_values() {
        let path =
            std::env::temp_dir().join(format!("doom-engine-roundtrip-{}.cfg", std::process::id()));

        let mut cfg = DoomConfig::default();
        cfg.set_i32("mouse_sensitivity", 9).unwrap();
        cfg.set_bool("show_messages", false).unwrap();
        cfg.set_string("chatmacro5", "whatever you say").unwrap();
        cfg.save(&path).unwrap();

        let loaded = DoomConfig::load(&path).unwrap();
        assert_eq!(loaded.get_i32("mouse_sensitivity"), 9);
        assert!(!loaded.get_bool("show_messages"));
        assert_eq!(loaded.get_string("chatmacro5"), "whatever you say");

        // serialize -> parse -> serialize must be byte-identical
        let once = loaded.serialize();
        let twice = DoomConfig::load_from_str(&once).serialize();
        assert_eq!(once, twice);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hex_values_round_trip_through_save() {
        let path = std::env::temp_dir().join(format!("doom-engine-hex-{}.cfg", std::process::id()));

        std::fs::write(&path, b"key_fire 0x9d\n").unwrap();
        let loaded = DoomConfig::load(&path).unwrap();
        assert_eq!(loaded.get_i32("key_fire"), 0x9d);
        loaded.save(&path).unwrap();
        let reloaded = DoomConfig::load(&path).unwrap();
        assert_eq!(reloaded.get_i32("key_fire"), 0x9d);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn setters_reject_wrong_kinds_and_unknown_keys() {
        let mut cfg = DoomConfig::default();
        assert!(cfg.set_i32("chatmacro0", 1).is_err());
        assert!(cfg.set_string("sfx_volume", "loud").is_err());
        assert!(cfg.set_i32("no_such_key", 1).is_err());
        assert!(cfg.set_string("no_such_key", "x").is_err());
        assert!(cfg.set_string("chatmacro0", "has \"quote\"").is_err());
        assert!(cfg.set_string("chatmacro0", "has\nnewline").is_err());
    }

    #[test]
    fn unknown_string_accessor_key_yields_empty() {
        let cfg = DoomConfig::default();
        assert_eq!(cfg.get_string("no_such_key"), "");
        assert_eq!(cfg.get_i32("no_such_key"), 0);
        assert!(!cfg.get_bool("no_such_key"));
    }

    #[test]
    fn out_of_i32_range_falls_back_to_default() {
        let cfg = DoomConfig::load_from_str("sfx_volume 99999999999999\n");
        assert_eq!(cfg.get_i32("sfx_volume"), 8);
    }
}
