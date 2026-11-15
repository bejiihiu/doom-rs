//! Command-line argument parsing for the engine.
//!
//! Port of `m_argv.c` plus the vanilla command-line keys used by
//! `d_main.c` (`-iwad`, `-file`, `-warp`, ...). The original scans `myargv`
//! with `M_CheckParm` (case-insensitive, starting at index 1); we do one
//! structured pass instead and hand back a typed [`EngineArgs`].
//!
//! Behavior notes, mirroring vanilla where it makes sense:
//!
//! - Keys are matched case-insensitively, values keep their original case.
//! - `-file` consumes every following token that does not start with `-`,
//!   so PWAD paths may contain spaces as long as they are one token.
//! - `-turbo` clamps to 10..=400 like vanilla (`scale < 10 -> 10`,
//!   `scale > 400 -> 400`) and logs a warning when it clamps.
//! - Unknown `-keys` (and stray bare tokens) are collected into
//!   [`EngineArgs::unknown`] with a warning, not an error — they are
//!   reported so typos are debuggable, but they must not crash the game.
//!
//! Unlike vanilla, a key that needs a value but gets none (or a value that
//! fails to parse) is an error instead of silently producing map 0.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use tracing::warn;

use crate::errors::{EngineError, EngineResult};

/// How `-warp` interprets its numbers.
///
/// Vanilla picks the interpretation from the game mode (commercial builds
/// read one number, the rest read episode + map). The parser does not know
/// the game mode, so the form is preserved and the caller maps it onto the
/// active mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Warp {
    /// `-warp <map>` — a single map number (DOOM 2 style).
    Map(u32),
    /// `-warp <episode> <map>` — episode and map (Ultimate DOOM style).
    EpisodeMap {
        /// Episode number, 1..=4.
        episode: u32,
        /// Map number inside the episode, 1..=9.
        map: u32,
    },
}

/// Deathmatch mode requested on the command line.
///
/// Mirrors the vanilla `deathmatch` variable: 0 = off, 1 = `-deathmatch`,
/// 2 = `-altdeath`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeathmatchMode {
    /// No deathmatch flags (vanilla `deathmatch == 0`).
    #[default]
    Off,
    /// `-deathmatch` (vanilla `deathmatch == 1`).
    Duel,
    /// `-altdeath` (vanilla `deathmatch == 2`). When both flags are given,
    /// vanilla checks `-altdeath` first, so alt keeps wins.
    Alt,
}

/// Parsed engine command-line arguments.
///
/// Paths are kept as raw [`OsString`]/[`PathBuf`] so non-UTF-8 file names
/// survive; everything else is a validated scalar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineArgs {
    /// `-iwad PATH` — explicit IWAD. `None` searches the standard locations.
    pub iwad: Option<PathBuf>,
    /// `-file WAD...` — PWAD files layered on top of the IWAD, in load order.
    pub pwads: Vec<PathBuf>,
    /// `-playdemo DEMO` — demo lump name to play back.
    pub play_demo: Option<String>,
    /// `-timedemo DEMO` — demo lump name to time.
    pub time_demo: Option<String>,
    /// `-warp E M` / `-warp <map>` — requested starting location.
    pub warp: Option<Warp>,
    /// `-skill 1-5` — requested skill level (1 = easiest, 5 = nightmare).
    pub skill: Option<u8>,
    /// `-nomonsters` — disable monsters.
    pub no_monsters: bool,
    /// `-turbo N` — forward/side move speed percentage, clamped to 10..=400.
    pub turbo: Option<u32>,
    /// `-fast` — fast monsters.
    pub fast: bool,
    /// `-respawn` — monsters respawn.
    pub respawn: bool,
    /// `-deathmatch` / `-altdeath`.
    pub deathmatch: DeathmatchMode,
    /// `-nosound` — disable sound output.
    pub no_sound: bool,
    /// `-nodeb` — disable debugging output.
    pub no_deb: bool,
    /// `-devparm` — running with development parameters.
    pub dev_parm: bool,
    /// `-record DEMO` — demo lump name to record.
    pub record: Option<String>,
    /// `-statcopy N` — statistics driver token. Vanilla `atoi()`d this into
    /// an integer and used it as a raw pointer; we keep the raw text and
    /// leave interpretation to whoever wires up the stats driver.
    pub stat_copy: Option<String>,
    /// `-config PATH` — explicit path to the config file.
    pub config_path: Option<PathBuf>,
    /// Keys (and stray bare tokens) we did not recognize, verbatim.
    /// Never an error; logged as a warning at parse time.
    pub unknown: Vec<String>,
}

impl EngineArgs {
    /// Wrap the requested deathmatch mode as the vanilla 0/1/2 integer.
    pub fn deathmatch_level(&self) -> u8 {
        match self.deathmatch {
            DeathmatchMode::Off => 0,
            DeathmatchMode::Duel => 1,
            DeathmatchMode::Alt => 2,
        }
    }
}

/// Returns true when the token starts with a `-` byte (encoding-agnostic).
fn is_dash_token(token: &OsStr) -> bool {
    token.as_encoded_bytes().first() == Some(&b'-')
}

/// Fetch the value token following `i`, rejecting end-of-args and flags.
fn value_after<'a>(argv: &'a [OsString], i: usize, key: &str) -> EngineResult<&'a OsStr> {
    match argv.get(i + 1) {
        Some(token) if !is_dash_token(token) => Ok(token),
        _ => Err(EngineError::Config(format!("missing value for {key}"))),
    }
}

/// Lossily render a token that is known to be a plain string value.
fn lossy(token: &OsStr) -> String {
    token.to_string_lossy().into_owned()
}

/// Parse a decimal u32 token, spelled out for good error messages.
fn parse_number(token: &OsStr, key: &str) -> EngineResult<u32> {
    let text = token
        .to_str()
        .ok_or_else(|| EngineError::Config(format!("{key} expects a number, got non-UTF-8")))?;
    text.trim()
        .parse::<u32>()
        .map_err(|_| EngineError::Config(format!("{key} expects a number, got {text:?}")))
}

fn parse_warp(argv: &[OsString], i: usize) -> EngineResult<(Warp, usize)> {
    let first = value_after(argv, i, "-warp")?;
    let a = parse_number(first, "-warp")?;

    // a second number (episode + map form) is only taken when it is not a
    // flag; anything else bare must still parse or it is an error
    let mut end = i + 2;
    let mut warp = Warp::Map(a);
    if let Some(token) = argv.get(i + 2)
        && !is_dash_token(token)
    {
        let b = parse_number(token, "-warp")?;
        warp = Warp::EpisodeMap { episode: a, map: b };
        end = i + 3;
    }

    match warp {
        Warp::Map(map) => {
            if !(1..=99).contains(&map) {
                return Err(EngineError::Config(format!(
                    "-warp map number must be between 1 and 99, got {map}"
                )));
            }
        }
        Warp::EpisodeMap { episode, map } => {
            if !(1..=4).contains(&episode) {
                return Err(EngineError::Config(format!(
                    "-warp episode must be between 1 and 4, got {episode}"
                )));
            }
            if !(1..=9).contains(&map) {
                return Err(EngineError::Config(format!(
                    "-warp map must be between 1 and 9, got {map}"
                )));
            }
        }
    }
    Ok((warp, end))
}

/// Parse a full `argv` (including the program name at index 0), mirroring
/// `std::env::args_os()`. The program name is skipped.
///
/// # Errors
///
/// Returns the first hard error encountered: a recognized key was given a
/// value that is missing or invalid. Unknown keys never error; they are
/// collected and reported.
pub fn parse(argv: &[OsString]) -> EngineResult<EngineArgs> {
    let mut args = EngineArgs::default();
    let mut i = 1; // argv[0] is the program name, like vanilla myargv

    while i < argv.len() {
        let token = argv[i].as_os_str();
        let key = token.to_str().map(|s| s.to_ascii_lowercase());

        match key.as_deref() {
            Some("-iwad") => {
                args.iwad = Some(PathBuf::from(value_after(argv, i, "-iwad")?));
                i += 2;
            }
            Some("-file") => {
                // vanilla: consume everything until a dash-prefixed token
                i += 1;
                while i < argv.len() && !is_dash_token(&argv[i]) {
                    args.pwads.push(PathBuf::from(&argv[i]));
                    i += 1;
                }
            }
            Some("-playdemo") => {
                args.play_demo = Some(lossy(value_after(argv, i, "-playdemo")?));
                i += 2;
            }
            Some("-timedemo") => {
                args.time_demo = Some(lossy(value_after(argv, i, "-timedemo")?));
                i += 2;
            }
            Some("-record") => {
                args.record = Some(lossy(value_after(argv, i, "-record")?));
                i += 2;
            }
            Some("-warp") => {
                let (warp, end) = parse_warp(argv, i)?;
                args.warp = Some(warp);
                i = end;
            }
            Some("-skill") => {
                let value = value_after(argv, i, "-skill")?;
                let n = parse_number(value, "-skill")?;
                if !(1..=5).contains(&n) {
                    return Err(EngineError::Config(format!(
                        "-skill must be between 1 and 5, got {n}"
                    )));
                }
                args.skill = Some(n as u8);
                i += 2;
            }
            Some("-turbo") => {
                let value = value_after(argv, i, "-turbo")?;
                let n = parse_number(value, "-turbo")?;
                let clamped = n.clamp(10, 400);
                if clamped != n {
                    warn!("-turbo {n} out of range, clamping to {clamped}");
                }
                args.turbo = Some(clamped);
                i += 2;
            }
            Some("-statcopy") => {
                args.stat_copy = Some(lossy(value_after(argv, i, "-statcopy")?));
                i += 2;
            }
            Some("-config") => {
                args.config_path = Some(PathBuf::from(value_after(argv, i, "-config")?));
                i += 2;
            }
            Some("-nomonsters") => {
                args.no_monsters = true;
                i += 1;
            }
            Some("-fast") => {
                args.fast = true;
                i += 1;
            }
            Some("-respawn") => {
                args.respawn = true;
                i += 1;
            }
            Some("-deathmatch") => {
                // altdeath wins if both are given, same as vanilla
                if args.deathmatch == DeathmatchMode::Off {
                    args.deathmatch = DeathmatchMode::Duel;
                }
                i += 1;
            }
            Some("-altdeath") => {
                args.deathmatch = DeathmatchMode::Alt;
                i += 1;
            }
            Some("-nosound") => {
                args.no_sound = true;
                i += 1;
            }
            Some("-nodeb") => {
                args.no_deb = true;
                i += 1;
            }
            Some("-devparm") => {
                args.dev_parm = true;
                i += 1;
            }
            _ => {
                // unknown flag or stray bare token — keep it for the user
                args.unknown.push(lossy(token));
                i += 1;
            }
        }
    }

    if !args.unknown.is_empty() {
        warn!("unrecognized arguments: {}", args.unknown.join(" "));
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// build an argv the way `std::env::args_os()` would see it
    fn argv(args: &[&str]) -> Vec<OsString> {
        let mut v = vec![OsString::from("doom-rs")];
        v.extend(args.iter().map(OsString::from));
        v
    }

    #[test]
    fn empty_argv_yields_defaults() {
        let args = parse(&[]).unwrap();
        assert_eq!(args, EngineArgs::default());
    }

    #[test]
    fn program_name_is_skipped() {
        let args = parse(&argv(&["-fast"])).unwrap();
        assert!(args.fast);
        assert!(!args.dev_parm);
    }

    #[test]
    fn keys_are_case_insensitive() {
        let args = parse(&argv(&["-IWAD", "DOOM.WAD", "-FAST"])).unwrap();
        assert_eq!(args.iwad.as_deref(), Some(std::path::Path::new("DOOM.WAD")));
        assert!(args.fast);
    }

    #[test]
    fn iwad_is_parsed() {
        let args = parse(&argv(&["-iwad", "/games/doom2.wad"])).unwrap();
        assert_eq!(
            args.iwad.as_deref(),
            Some(std::path::Path::new("/games/doom2.wad"))
        );
    }

    #[test]
    fn iwad_missing_value_is_an_error() {
        assert!(parse(&argv(&["-iwad"])).is_err());
        assert!(parse(&argv(&["-iwad", "-fast"])).is_err());
    }

    #[test]
    fn file_consumes_wads_until_next_flag() {
        let args = parse(&argv(&["-file", "a.wad", "b.wad", "-fast"])).unwrap();
        assert_eq!(
            args.pwads,
            vec![PathBuf::from("a.wad"), PathBuf::from("b.wad")]
        );
        assert!(args.fast);
    }

    #[test]
    fn file_at_end_with_no_wads_is_fine() {
        let args = parse(&argv(&["-fast", "-file"])).unwrap();
        assert!(args.pwads.is_empty());
        assert!(args.fast);
    }

    #[test]
    fn demo_and_record_names_are_kept() {
        let args = parse(&argv(&[
            "-playdemo",
            "demo1",
            "-timedemo",
            "demo2",
            "-record",
            "demo3",
        ]))
        .unwrap();
        assert_eq!(args.play_demo.as_deref(), Some("demo1"));
        assert_eq!(args.time_demo.as_deref(), Some("demo2"));
        assert_eq!(args.record.as_deref(), Some("demo3"));
    }

    #[test]
    fn warp_with_single_number() {
        let args = parse(&argv(&["-warp", "5"])).unwrap();
        assert_eq!(args.warp, Some(Warp::Map(5)));
    }

    #[test]
    fn warp_with_episode_and_map() {
        let args = parse(&argv(&["-warp", "2", "3"])).unwrap();
        assert_eq!(args.warp, Some(Warp::EpisodeMap { episode: 2, map: 3 }));
    }

    #[test]
    fn warp_rejects_bad_numbers() {
        assert!(parse(&argv(&["-warp", "abc"])).is_err());
        assert!(parse(&argv(&["-warp", "0"])).is_err());
        assert!(parse(&argv(&["-warp", "1", "x"])).is_err());
    }

    #[test]
    fn warp_rejects_out_of_range() {
        assert!(parse(&argv(&["-warp", "5", "10"])).is_err()); // map > 9
        assert!(parse(&argv(&["-warp", "9", "1"])).is_err()); // episode > 4
    }

    #[test]
    fn warp_missing_value_is_an_error() {
        assert!(parse(&argv(&["-warp"])).is_err());
        assert!(parse(&argv(&["-warp", "-skill", "3"])).is_err());
    }

    #[test]
    fn warp_tolerates_trailing_flag_after_episode_and_map() {
        let args = parse(&argv(&["-warp", "1", "2", "-skill", "4"])).unwrap();
        assert_eq!(args.warp, Some(Warp::EpisodeMap { episode: 1, map: 2 }));
        assert_eq!(args.skill, Some(4));
    }

    #[test]
    fn skill_accepts_one_through_five() {
        for n in 1..=5u32 {
            let args = parse(&argv(&["-skill", &n.to_string()])).unwrap();
            assert_eq!(args.skill, Some(n as u8));
        }
    }

    #[test]
    fn skill_out_of_range_is_an_error() {
        assert!(parse(&argv(&["-skill", "0"])).is_err());
        assert!(parse(&argv(&["-skill", "6"])).is_err());
    }

    #[test]
    fn skill_non_numeric_is_an_error() {
        assert!(parse(&argv(&["-skill", "hard"])).is_err());
        assert!(parse(&argv(&["-skill"])).is_err());
    }

    #[test]
    fn turbo_is_clamped_like_vanilla() {
        let args = parse(&argv(&["-turbo", "100"])).unwrap();
        assert_eq!(args.turbo, Some(100));

        let args = parse(&argv(&["-turbo", "5"])).unwrap();
        assert_eq!(args.turbo, Some(10));

        let args = parse(&argv(&["-turbo", "9000"])).unwrap();
        assert_eq!(args.turbo, Some(400));
    }

    #[test]
    fn turbo_non_numeric_is_an_error() {
        assert!(parse(&argv(&["-turbo", "fast"])).is_err());
    }

    #[test]
    fn all_boolean_flags_are_parsed() {
        let args = parse(&argv(&[
            "-nomonsters",
            "-fast",
            "-respawn",
            "-deathmatch",
            "-nosound",
            "-nodeb",
            "-devparm",
        ]))
        .unwrap();
        assert!(args.no_monsters);
        assert!(args.fast);
        assert!(args.respawn);
        assert_eq!(args.deathmatch, DeathmatchMode::Duel);
        assert_eq!(args.deathmatch_level(), 1);
        assert!(args.no_sound);
        assert!(args.no_deb);
        assert!(args.dev_parm);
    }

    #[test]
    fn altdeath_maps_to_two() {
        let args = parse(&argv(&["-altdeath"])).unwrap();
        assert_eq!(args.deathmatch, DeathmatchMode::Alt);
        assert_eq!(args.deathmatch_level(), 2);
    }

    #[test]
    fn altdeath_wins_when_both_flags_are_given() {
        // vanilla checks -altdeath first
        let args = parse(&argv(&["-deathmatch", "-altdeath"])).unwrap();
        assert_eq!(args.deathmatch, DeathmatchMode::Alt);

        let args = parse(&argv(&["-altdeath", "-deathmatch"])).unwrap();
        assert_eq!(args.deathmatch, DeathmatchMode::Alt);
    }

    #[test]
    fn statcopy_keeps_the_raw_token() {
        let args = parse(&argv(&["-statcopy", "0x4000"])).unwrap();
        assert_eq!(args.stat_copy.as_deref(), Some("0x4000"));
    }

    #[test]
    fn config_path_is_parsed() {
        let args = parse(&argv(&["-config", "custom.cfg"])).unwrap();
        assert_eq!(
            args.config_path.as_deref(),
            Some(std::path::Path::new("custom.cfg"))
        );
    }

    #[test]
    fn unknown_keys_and_stray_tokens_are_collected_not_errors() {
        let args = parse(&argv(&["-fast", "-turbo", "150", "-frobnicate", "hello"])).unwrap();
        assert_eq!(
            args.unknown,
            vec!["-frobnicate".to_string(), "hello".to_string()]
        );
        assert!(args.fast);
        assert_eq!(args.turbo, Some(150));
    }

    #[test]
    fn unknown_flag_after_file_terminates_the_wad_list() {
        // a flag always terminates -file's consumption, known or not
        let args = parse(&argv(&["-file", "a.wad", "-frobnicate"])).unwrap();
        assert_eq!(args.pwads, vec![PathBuf::from("a.wad")]);
        assert_eq!(args.unknown, vec!["-frobnicate".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_path_for_iwad_works() {
        use std::os::unix::ffi::OsStringExt;
        let raw = OsString::from_vec(vec![0x80, 0x81, b'.', b'w', b'a', b'd']);
        let args = parse(&[
            OsString::from("doom-rs"),
            OsString::from("-iwad"),
            raw.clone(),
        ])
        .unwrap();
        assert_eq!(
            args.iwad.as_deref(),
            Some(std::path::Path::new(raw.as_os_str()))
        );
    }
}
