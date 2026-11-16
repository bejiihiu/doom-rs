//! Command-line argument parsing for the engine.
//!
//! Port of `m_argv.c` plus the vanilla command-line keys used by
//! `d_main.c` (`-iwad`, `-file`, `-warp`, ...). The original scans `myargv`
//! with `M_CheckParm` (case-insensitive, starting at index 1); we do one
//! structured pass instead and hand back a typed [`crate::args::EngineArgs`].
//!
//! Behavior notes, mirroring vanilla where it makes sense:
//!
//! - Keys are matched case-insensitively, values keep their original case.
//! - `-file` consumes every following token that does not start with `-`,
//!   so PWAD paths may contain spaces as long as they are one token.
//! - `-turbo` clamps to 10..=400 like vanilla (`scale < 10 -> 10`,
//!   `scale > 400 -> 400`) and logs a warning when it clamps.
//! - `-net` takes either the vanilla form `-net <n> [host[:port] ...]`
//!   (`n` = 1-based console player number, 1-8) or a shared node list
//!   `-net <host[:port]> [...]` where the first entry is the host (key
//!   player, `consoleplayer == 0`) and every machine runs the same command
//!   line; a machine's slot is the list entry whose port equals the bound
//!   port (`-port`, default 5029). The list runs until the next
//!   dash-token, like vanilla. Hosts resolve through the system resolver;
//!   a token without a port uses the `-port` value (anywhere on the
//!   command line, like vanilla's `-port` pre-parse).
//! - `-dup N` clamps to 1..=9 like vanilla; `-extratic` sends a backup tic.
//! - `-port N` binds the local UDP port (and is the default `-net` port),
//!   `1..=65535`. There is no broadcast discovery in the original engine
//!   and we do not invent one: explicit unicast node addresses are the
//!   only way to set up a net game.
//! - Unknown `-keys` (and stray bare tokens) are collected into
//!   [`crate::args::EngineArgs::unknown`] with a warning, not an error — they are
//!   reported so typos are debuggable, but they must not crash the game.
//!
//! Unlike vanilla, a key that needs a value but gets none (or a value that
//! fails to parse) is an error instead of silently producing map 0.

use std::ffi::{OsStr, OsString};
use std::net::SocketAddr;
use std::path::PathBuf;

use tracing::warn;

use crate::errors::{EngineError, EngineResult};
use crate::net::{DOOMPORT, MAXNETNODES};

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

/// Where the `-net` argument was given as a node list.
///
/// Two forms are accepted:
///
/// - `-net <n> [host[:port] ...]` — the vanilla shape: `<n>` is the
///   1-based console player number, the remaining tokens are the *peer*
///   nodes (in any order; every node gets the same player numbering).
/// - `-net <host[:port]> [...]` — a shared node list, host (key player)
///   first. Every machine runs the same command line; its own node index
///   is derived from the bound port (see the `-net` entry in `parse`).
///
/// `host` may be an IPv4 literal, an IPv6 literal (bare `::1` or
/// bracketed `[::1]:5029`) or a hostname (resolved via the system
/// resolver, first address wins, like vanilla's `gethostbyname`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetArgs {
    /// `-net <n> [host[:port] ...]` — vanilla leading console number.
    Vanilla {
        /// 1-based console player number, `1..=MAXNETNODES`.
        console_player: usize,
        /// The peer nodes, in node order after this console.
        hosts: Vec<SocketAddr>,
    },
    /// `-net <host[:port]> [...]` — shared node list, host first.
    SharedList {
        /// The full node list; the first entry is the host (key player,
        /// `consoleplayer == 0`). Every listed node needs a distinct port.
        hosts: Vec<SocketAddr>,
    },
}

/// Parsed engine command-line arguments.
///
/// Paths are kept as raw [`OsString`]/[`PathBuf`] so non-UTF-8 file names
/// survive; everything else is a validated scalar.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// `-net ...` — network game parameters (see [`NetArgs`]).
    pub net: Option<NetArgs>,
    /// `-dup N` — tic duplication for slow links, `1..=9` (vanilla clamps).
    pub tic_dup: i32,
    /// `-extratic` — send one backup tic in every packet.
    pub extratics: bool,
    /// `-port N` — local UDP port the game binds (and the default port for
    /// `-net` hosts), `1..=65535`. Defaults to the vanilla `DOOMPORT`.
    pub net_port: u16,
    /// Keys (and stray bare tokens) we did not recognize, verbatim.
    /// Never an error; logged as a warning at parse time.
    pub unknown: Vec<String>,
}

impl Default for EngineArgs {
    fn default() -> Self {
        Self {
            iwad: None,
            pwads: Vec::new(),
            play_demo: None,
            time_demo: None,
            warp: None,
            skill: None,
            no_monsters: false,
            turbo: None,
            fast: false,
            respawn: false,
            deathmatch: DeathmatchMode::Off,
            no_sound: false,
            no_deb: false,
            dev_parm: false,
            record: None,
            stat_copy: None,
            config_path: None,
            net: None,
            tic_dup: 1,
            extratics: false,
            net_port: DOOMPORT,
            unknown: Vec::new(),
        }
    }
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

/// Resolve a `host` name against the system resolver, first address
/// wins — like vanilla's `gethostbyname`, just with a proper error path.
fn resolve_host(host: &str, port: u16, raw_token: &str) -> EngineResult<SocketAddr> {
    use std::net::ToSocketAddrs;
    let mut addrs = (host, port).to_socket_addrs().map_err(|err| {
        EngineError::Config(format!("-net: could not resolve {raw_token:?}: {err}"))
    })?;
    addrs
        .next()
        .ok_or_else(|| EngineError::Config(format!("-net: {raw_token:?} resolved to no addresses")))
}

/// Parse a `host[:port]` token from the `-net` list.
fn parse_net_host(token: &OsString, default_port: u16) -> EngineResult<SocketAddr> {
    let text = lossy(token);
    let raw = text.as_str();
    // bracketed IPv6: "[::1]" or "[::1]:5029"
    if let Some(rest) = raw.strip_prefix('[') {
        let Some((host, tail)) = rest.split_once(']') else {
            return Err(EngineError::Config(format!(
                "-net: unterminated bracket in host {raw:?}"
            )));
        };
        let port = match tail.strip_prefix(':') {
            Some(value) => parse_port(value, raw)?,
            None if tail.is_empty() => default_port,
            None => {
                return Err(EngineError::Config(format!(
                    "-net: unexpected trailing characters in host {raw:?}"
                )));
            }
        };
        return resolve_host(host, port, raw);
    }
    let colons = raw.matches(':').count();
    match colons {
        0 => resolve_host(raw, default_port, raw),
        1 => {
            let split = raw.rfind(':').expect("one colon");
            let (host, port) = raw.split_at(split);
            let host = host.trim();
            if host.is_empty() {
                return Err(EngineError::Config(format!(
                    "-net: missing host in {raw:?}"
                )));
            }
            resolve_host(host, parse_port(&port[1..], raw)?, raw)
        }
        // bare IPv6 literal: "::1" etc. — the port comes from -port
        _ => resolve_host(raw, default_port, raw),
    }
}

/// Parse a `port` token from the `-net` list or `-port`.
fn parse_port(text: &str, raw_token: &str) -> EngineResult<u16> {
    let port: u16 = text
        .trim()
        .parse()
        .map_err(|_| EngineError::Config(format!("-net: invalid port in {raw_token:?}")))?;
    if port == 0 {
        return Err(EngineError::Config(format!(
            "-net: port 0 is not a valid node address in {raw_token:?}"
        )));
    }
    Ok(port)
}

/// Parse the tokens following `-net` up to the next dash-token.
fn parse_net(tokens: &[OsString], default_port: u16) -> EngineResult<NetArgs> {
    let Some(first) = tokens.first() else {
        return Err(EngineError::Config(
            "-net expects a console player number or a list of node addresses".into(),
        ));
    };
    let first_text = lossy(first);
    let hosts_from = |from: usize| -> EngineResult<Vec<SocketAddr>> {
        tokens[from..]
            .iter()
            .map(|token| parse_net_host(token, default_port))
            .collect()
    };
    if let Ok(number) = first_text.trim().parse::<i64>() {
        if !(1..=MAXNETNODES as i64).contains(&number) {
            return Err(EngineError::Config(format!(
                "-net player number must be between 1 and {MAXNETNODES}, got {number}"
            )));
        }
        let hosts = hosts_from(1)?;
        if hosts.len() + 1 > MAXNETNODES {
            return Err(EngineError::Config(format!(
                "-net: too many nodes: {} (max {MAXNETNODES})",
                hosts.len() + 1
            )));
        }
        return Ok(NetArgs::Vanilla {
            console_player: number as usize,
            hosts,
        });
    }
    let hosts = hosts_from(0)?;
    if hosts.len() > MAXNETNODES {
        return Err(EngineError::Config(format!(
            "-net: too many nodes: {} (max {MAXNETNODES})",
            hosts.len()
        )));
    }
    Ok(NetArgs::SharedList { hosts })
}

/// Pre-scan `argv` for `-port` so the `-net` list can use it as the
/// default port regardless of where on the command line it appears —
/// vanilla parses `-port` before `-net` for the same reason.
fn port_default(argv: &[OsString]) -> u16 {
    let mut i = 1;
    while i < argv.len() {
        if argv[i]
            .to_str()
            .map(|s| s.eq_ignore_ascii_case("-port"))
            .unwrap_or(false)
            && let Some(value) = argv.get(i + 1)
            && let Ok(port) = value.to_string_lossy().trim().parse::<u16>()
            && port != 0
        {
            return port;
        }
        i += 1;
    }
    DOOMPORT
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
    let default_port = port_default(argv);
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
            Some("-net") => {
                // vanilla: the list runs until the next dash-token
                i += 1;
                let mut tokens = Vec::new();
                while i < argv.len() && !is_dash_token(&argv[i]) {
                    tokens.push(argv[i].clone());
                    i += 1;
                }
                args.net = Some(parse_net(&tokens, default_port)?);
            }
            Some("-dup") => {
                let value = value_after(argv, i, "-dup")?;
                let n = parse_number(value, "-dup")?;
                let clamped = n.clamp(1, 9);
                if clamped != n {
                    warn!("-dup {n} out of range, clamping to {clamped}");
                }
                args.tic_dup = clamped as i32;
                i += 2;
            }
            Some("-extratic") => {
                args.extratics = true;
                i += 1;
            }
            Some("-port") => {
                let value = value_after(argv, i, "-port")?;
                let text = value.to_str().ok_or_else(|| {
                    EngineError::Config("-port expects a number, got non-UTF-8".into())
                })?;
                let port: u32 = text.trim().parse().map_err(|_| {
                    EngineError::Config(format!("-port expects a number, got {text:?}"))
                })?;
                if !(1..=65535).contains(&port) {
                    return Err(EngineError::Config(format!(
                        "-port must be between 1 and 65535, got {port}"
                    )));
                }
                args.net_port = port as u16;
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

    #[test]
    fn net_vanilla_form_parses_addresses() {
        let args = parse(&argv(&["-net", "1", "127.0.0.1:5029", "192.168.0.2:5030"])).unwrap();
        match &args.net {
            Some(NetArgs::Vanilla {
                console_player,
                hosts,
            }) => {
                assert_eq!(*console_player, 1);
                assert_eq!(
                    hosts,
                    &vec![
                        "127.0.0.1:5029".parse::<SocketAddr>().unwrap(),
                        "192.168.0.2:5030".parse::<SocketAddr>().unwrap(),
                    ]
                );
            }
            other => panic!("expected vanilla net args, got {other:?}"),
        }
    }

    #[test]
    fn net_vanilla_form_with_no_hosts() {
        let args = parse(&argv(&["-net", "2"])).unwrap();
        match &args.net {
            Some(NetArgs::Vanilla {
                console_player,
                hosts,
            }) => {
                assert_eq!(*console_player, 2);
                assert!(hosts.is_empty());
            }
            other => panic!("expected vanilla net args, got {other:?}"),
        }
    }

    #[test]
    fn net_shared_list_form_keeps_node_order() {
        let args = parse(&argv(&["-net", "127.0.0.1:5029", "127.0.0.1:5030"])).unwrap();
        match &args.net {
            Some(NetArgs::SharedList { hosts }) => {
                assert_eq!(
                    hosts,
                    &vec![
                        "127.0.0.1:5029".parse::<SocketAddr>().unwrap(),
                        "127.0.0.1:5030".parse::<SocketAddr>().unwrap(),
                    ]
                );
            }
            other => panic!("expected a shared list, got {other:?}"),
        }
    }

    #[test]
    fn net_hosts_default_to_doomport_and_port_wins() {
        let args = parse(&argv(&["-net", "1", "127.0.0.1", "-port", "5099"])).unwrap();
        match &args.net {
            Some(NetArgs::Vanilla { hosts, .. }) => {
                // -port anywhere on the command line wins, like vanilla
                assert_eq!(hosts[0], "127.0.0.1:5099".parse::<SocketAddr>().unwrap());
            }
            other => panic!("expected vanilla net args, got {other:?}"),
        }
        assert_eq!(args.net_port, 5099);
    }

    #[test]
    fn net_default_port_is_doomport_without_port_flag() {
        let args = parse(&argv(&["-net", "1", "10.0.0.2"])).unwrap();
        match &args.net {
            Some(NetArgs::Vanilla { hosts, .. }) => {
                assert_eq!(
                    hosts[0],
                    format!("10.0.0.2:{DOOMPORT}")
                        .parse::<SocketAddr>()
                        .unwrap()
                );
            }
            other => panic!("expected vanilla net args, got {other:?}"),
        }
        assert_eq!(args.net_port, DOOMPORT);
    }

    #[test]
    fn net_list_ends_at_the_next_flag() {
        let args = parse(&argv(&["-net", "1", "127.0.0.1:5030", "-fast"])).unwrap();
        match &args.net {
            Some(NetArgs::Vanilla { hosts, .. }) => assert_eq!(hosts.len(), 1),
            other => panic!("expected vanilla net args, got {other:?}"),
        }
        assert!(args.fast);
    }

    #[test]
    fn net_rejects_out_of_range_player_number() {
        assert!(parse(&argv(&["-net", "0"])).is_err());
        assert!(parse(&argv(&["-net", "9"])).is_err());
    }

    #[test]
    fn net_rejects_too_many_nodes() {
        let hosts: Vec<String> = (0..MAXNETNODES)
            .map(|i| format!("10.0.0.{}:5029", i + 1))
            .collect();
        let mut tokens = vec!["-net", "1"];
        tokens.extend(hosts.iter().map(String::as_str));
        assert!(parse(&argv(&tokens)).is_err());
    }

    #[test]
    fn net_rejects_bad_ports_and_bad_hosts() {
        assert!(parse(&argv(&["-net", "1", "127.0.0.1:0"])).is_err());
        assert!(parse(&argv(&["-net", "1", "127.0.0.1:99999"])).is_err());
        assert!(parse(&argv(&["-net", "1", "127.0.0.1:abc"])).is_err());
        assert!(parse(&argv(&["-net", "1", "127.0.0.1:"])).is_err());
        // bogus hostname: the resolver errors out, like vanilla's I_Error
        assert!(parse(&argv(&["-net", "1", "bogus.invalid"])).is_err());
    }

    #[test]
    fn net_rejects_empty_list_and_oversized_shared_list() {
        assert!(parse(&argv(&["-net"])).is_err());
        let hosts: Vec<String> = (0..=MAXNETNODES)
            .map(|i| format!("10.0.0.{}:5029", i + 1))
            .collect();
        let with_net: Vec<&str> = std::iter::once("-net")
            .chain(hosts.iter().map(String::as_str))
            .collect();
        assert!(parse(&argv(&with_net)).is_err());
    }

    #[test]
    fn net_accepts_bracketed_ipv6() {
        let args = parse(&argv(&["-net", "1", "[::1]:5030"])).unwrap();
        match &args.net {
            Some(NetArgs::Vanilla { hosts, .. }) => {
                assert_eq!(hosts[0], "[::1]:5030".parse::<SocketAddr>().unwrap());
            }
            other => panic!("expected vanilla net args, got {other:?}"),
        }
    }

    #[test]
    fn dup_clamps_like_vanilla_and_rejects_garbage() {
        assert_eq!(parse(&argv(&["-dup", "1"])).unwrap().tic_dup, 1);
        assert_eq!(parse(&argv(&["-dup", "9"])).unwrap().tic_dup, 9);
        assert_eq!(parse(&argv(&["-dup", "0"])).unwrap().tic_dup, 1);
        assert_eq!(parse(&argv(&["-dup", "99"])).unwrap().tic_dup, 9);
        assert!(parse(&argv(&["-dup", "fast"])).is_err());
        assert!(parse(&argv(&["-dup"])).is_err());
    }

    #[test]
    fn extratic_and_port_are_parsed() {
        let args = parse(&argv(&["-extratic", "-port", "5033"])).unwrap();
        assert!(args.extratics);
        assert_eq!(args.net_port, 5033);
        assert!(parse(&argv(&["-port", "0"])).is_err());
        assert!(parse(&argv(&["-port", "65536"])).is_err());
        assert!(parse(&argv(&["-port", "abc"])).is_err());
        assert!(parse(&argv(&["-port"])).is_err());
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
