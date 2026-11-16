//! `doom-rs` - the DOOM engine, rewritten in Rust, running in a real window.
//!
//! Startup order: parse our own flags (via clap), hand the remaining tokens
//! to the engine's vanilla-style argument parser, resolve the IWAD, load
//! `doom.cfg`, build the game, then open the window and run the loop.
//! Any failure before the window opens is a message on stderr and exit
//! code 1 - never a panic, never a half-initialized game.

use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use doom_engine::VERSION as ENGINE_VERSION;
use doom_engine::args;
use doom_engine::config::DoomConfig;
use doom_engine::game::{Game, GameConfig, KeyBindings};
use doom_engine::gfx::palette::parse_playpal;
use doom_engine::log::init_logging_with_default;
use doom_engine::menu::GameMode;
use doom_engine::resources::ResourceManager;
use doom_engine::wad::WadFile;

const SCREEN_WIDTH: u32 = 320;
const SCREEN_HEIGHT: u32 = 200;

mod app;
mod audio;
mod input;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const LONG_ABOUT: &str = "\
doom-rs - a faithful rewrite of the DOOM engine in Rust

The engine options are the classic DOOM command line, passed through
untouched. Platform flags (--fullscreen, --scale, -h/--help, -V/--version)
must come first; everything after them goes to the engine.

Engine options:
  -iwad PATH        Path to the IWAD (doom1.wad, doom.wad, doom2.wad, ...)
  -file WAD...      Layer one or more PWADs on top of the IWAD
  -warp E M         Start on episode E map M (DOOM), or map N (DOOM II)
  -skill N          Difficulty 1-5 (default: medium)
  -nomonsters       Do not spawn monsters
  -respawn          Monsters respawn after dying
  -fast             Monsters move and attack faster
  -deathmatch       Deathmatch mode
  -altdeath         Alt deathmatch rules (with -deathmatch, alt wins)
  -nosound          Disable audio output
  -nodeb            Keep the log quiet (warning level and above only)
  -devparm          Development mode: logs \"development mode ON\"
  -playdemo DEMO    Play back a demo
  -timedemo DEMO    Time a demo playback
  -record DEMO      Record a demo
  -config PATH      Override the config file (default: ./doom.cfg)
  -turbo N          Move speed percentage (10-400)
  -statcopy N       Statistics driver token, kept raw; unused

Network play (UDP, peer-to-peer, no broadcast discovery - nodes need
explicit unicast addresses):
  -net N [HOST[:PORT] ...]
                      Vanilla form: console player N (1-8), then the peer
                      addresses. Each machine runs this with its own
                      player number, e.g.
                          doom-rs -iwad doom2.wad -net 1
                      (I'm player 1, solo net game; with no addresses the
                      key player waits for nothing and the game runs)
                          doom-rs -iwad doom2.wad -net 1 192.0.2.10
                      (I'm player 1; the other machine runs -net 2 with
                      my address instead). The default port is 5029.
  -net HOST[:PORT] ...
                      Shared node list, host first: every machine runs the
                      same command line; the first listed node is the host
                      (key player, console player 1). Each machine's slot
                      is the list entry whose port equals its -port, so
                      with multiple machines on one host you need
                      distinct -port values, e.g.
                          doom-rs -iwad doom2.wad -net 127.0.0.1:5029 127.0.0.1:5030
                          doom-rs -iwad doom2.wad -net 127.0.0.1:5029 127.0.0.1:5030 -port 5030
  -dup N            Tic duplication for slow links (1-9, clamped)
  -extratic         Send a backup tic in every packet
  -port N           Local UDP port (1-65535, default 5029); also the
                    default port for -net addresses

The IWAD is resolved in this order: -iwad, the DOOM_IWAD environment
variable, then the current directory (doom2.wad, doom2f.wad, plutonia.wad,
tnt.wad, doomu.wad, doom.wad and doom1.wad).

Keys (defaults): arrows/WASD to move, Ctrl or left mouse to fire, Space
or right mouse to use, Tab for the automap, 1-9 to select weapons,
mouse to turn, Escape for the menu.";

/// The platform shell's own flags. Everything that is not one of these is
/// handed to the engine's DOOM-style argument parser untouched.
#[derive(Parser, Debug)]
#[command(
    name = "doom-rs",
    version = VERSION,
    about = "a faithful rewrite of the DOOM engine in Rust",
    long_about = LONG_ABOUT
)]
struct Cli {
    /// Run in borderless fullscreen mode
    #[arg(long)]
    fullscreen: bool,
    /// Integer scale factor for the 320x200 view
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u32).range(1..=8))]
    scale: u32,
    /// Everything else is passed to the engine unchanged
    /// (DOOM-style: -iwad PATH, -warp E M, ...)
    #[arg(value_name = "ENGINE ARGS", trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    engine_args: Vec<OsString>,
}

/// Standard IWAD search order after `-iwad` and `DOOM_IWAD`, mirroring
/// the original's filename-based version detection.
const IWAD_CANDIDATES: &[&str] = &[
    "doom2.wad",
    "doom2f.wad",
    "plutonia.wad",
    "tnt.wad",
    "doomu.wad",
    "doom.wad",
    "doom1.wad",
    "DOOM2.WAD",
    "DOOM2F.WAD",
    "PLUTONIA.WAD",
    "TNT.WAD",
    "DOOMU.WAD",
    "DOOM.WAD",
    "DOOM1.WAD",
];

/// Resolves the IWAD path: explicit `-iwad`, then `DOOM_IWAD`, then the
/// current directory. Returns a printable path plus an error the caller
/// can turn into an exit code.
fn resolve_iwad(args: &args::EngineArgs) -> Result<PathBuf, String> {
    if let Some(path) = &args.iwad {
        if path.is_file() {
            return Ok(path.clone());
        }
        return Err(format!(
            "the IWAD passed with -iwad does not exist or is not a file: {}",
            path.display()
        ));
    }
    if let Some(value) = env::var_os("DOOM_IWAD") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "the IWAD set in DOOM_IWAD does not exist or is not a file: {}",
            path.display()
        ));
    }
    for candidate in IWAD_CANDIDATES {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(format!(
        "no IWAD found. Looked in: -iwad (not given), the DOOM_IWAD environment variable (not set), and the current directory for: {}",
        IWAD_CANDIDATES.join(", ")
    ))
}

/// Identifies the game mode from the IWAD file name, the way the
/// original did; falls back to probing map lumps for unknown names.
fn detect_game_mode(iwad: &Path) -> GameMode {
    let stem = iwad
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    match stem.as_str() {
        "doom2" | "doom2f" | "plutonia" | "tnt" => return GameMode::Commercial,
        "doomu" => return GameMode::Retail,
        "doom" => return GameMode::Registered,
        "doom1" => return GameMode::Shareware,
        _ => {}
    }
    // unknown file name: sniff the directory contents
    match WadFile::open(iwad) {
        Ok(wad) => {
            if wad.find_lump("MAP01").is_some() {
                GameMode::Commercial
            } else if wad.find_lump("E4M1").is_some() {
                GameMode::Retail
            } else if wad.find_lump("E3M1").is_some() {
                GameMode::Registered
            } else {
                GameMode::Shareware
            }
        }
        Err(err) => {
            tracing::warn!(
                "could not probe the IWAD for its game mode ({err}); assuming shareware"
            );
            GameMode::Shareware
        }
    }
}

fn main() -> ExitCode {
    let raw: Vec<OsString> = env::args_os().collect();

    // -nodeb keeps the log quiet, like the original's -nodeb switch; the
    // default filter is picked before anything logs, so a bad WAD still
    // gets an error message, just not the banner noise
    let no_deb = raw.iter().skip(1).any(|token| {
        token
            .to_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("-nodeb"))
    });
    init_logging_with_default(if no_deb { "warn" } else { "info" });

    let cli = Cli::parse();
    let mut engine_argv: Vec<OsString> = vec![OsString::from("doom-rs")];
    engine_argv.extend(cli.engine_args.iter().cloned());
    let platform = cli;

    let engine_args = match args::parse(&engine_argv) {
        Ok(engine_args) => engine_args,
        Err(err) => {
            eprintln!("doom-rs: error: {err}");
            eprintln!("usage: doom-rs [ENGINE OPTIONS] (try doom-rs --help)");
            return ExitCode::FAILURE;
        }
    };
    if !engine_args.unknown.is_empty() {
        tracing::warn!(
            "ignoring unknown arguments: {}",
            engine_args.unknown.join(" ")
        );
    }
    if engine_args.dev_parm {
        tracing::info!("development mode ON");
    }

    let iwad = match resolve_iwad(&engine_args) {
        Ok(iwad) => iwad,
        Err(message) => {
            eprintln!("doom-rs: error: {message}");
            eprintln!(
                "hint: pass -iwad PATH or set DOOM_IWAD to your IWAD (doom1.wad, doom.wad or doom2.wad)"
            );
            return ExitCode::FAILURE;
        }
    };
    for pwad in &engine_args.pwads {
        if !pwad.is_file() {
            eprintln!(
                "doom-rs: error: a -file PWAD does not exist: {}",
                pwad.display()
            );
            return ExitCode::FAILURE;
        }
    }

    let config_path = engine_args
        .config_path
        .clone()
        .unwrap_or_else(|| PathBuf::from("doom.cfg"));
    let config = match DoomConfig::load(&config_path) {
        Ok(config) => config,
        Err(err) => {
            eprintln!(
                "doom-rs: error: could not read {}: {err}",
                config_path.display()
            );
            return ExitCode::FAILURE;
        }
    };

    let game_mode = detect_game_mode(&iwad);
    let mut wad_paths = vec![iwad.clone()];
    wad_paths.extend(engine_args.pwads.iter().cloned());
    let resources = match ResourceManager::from_paths(wad_paths) {
        Ok(resources) => resources,
        Err(err) => {
            eprintln!("doom-rs: error: could not load the WAD file(s): {err}");
            return ExitCode::FAILURE;
        }
    };

    // the palette is needed by the platform, not the engine; grab the raw
    // lump now, before the resource manager moves into the game
    let playpal = match resources.lump_data("PLAYPAL").map(|data| data.to_vec()) {
        Ok(playpal) => playpal,
        Err(err) => {
            eprintln!(
                "doom-rs: error: could not read PLAYPAL (the IWAD looks truncated or corrupt): {err}"
            );
            return ExitCode::FAILURE;
        }
    };
    let palettes = match parse_playpal(&playpal) {
        Ok(palettes) => palettes,
        Err(err) => {
            eprintln!("doom-rs: error: could not parse PLAYPAL: {err}");
            return ExitCode::FAILURE;
        }
    };
    let palette = palettes
        .into_iter()
        .next()
        .expect("parse_playpal guarantees >= 1 palette");
    // vanilla bakes the gamma level into the palette at load time; we apply
    // the same remap at present time instead
    let gamma_level = config
        .get_i32("usegamma")
        .clamp(0, doom_engine::render::GAMMA_LEVELS as i32 - 1) as usize;
    let gamma_lut = *doom_engine::render::level_table(gamma_level);

    // the engine wants integers; doom.cfg stores them in the same shape
    let keys = KeyBindings {
        key_right: config.get_i32("key_right") as u8,
        key_left: config.get_i32("key_left") as u8,
        key_up: config.get_i32("key_up") as u8,
        key_down: config.get_i32("key_down") as u8,
        key_strafeleft: config.get_i32("key_strafeleft") as u8,
        key_straferight: config.get_i32("key_straferight") as u8,
        key_fire: config.get_i32("key_fire") as u8,
        key_use: config.get_i32("key_use") as u8,
        key_strafe: config.get_i32("key_strafe") as u8,
        key_speed: config.get_i32("key_speed") as u8,
    };

    let save_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let sfx_volume = config.get_i32("sfx_volume").clamp(0, 15);
    let music_volume = config.get_i32("music_volume").clamp(0, 15);
    let no_sound = engine_args.no_sound;
    let game_config = GameConfig {
        game_mode,
        save_dir,
        sfx_volume: if no_sound { 0 } else { sfx_volume },
        music_volume: if no_sound { 0 } else { music_volume },
        detail_level: config.get_i32("detaillevel"),
        screen_blocks: config.get_i32("screenblocks"),
        show_messages: config.get_bool("show_messages"),
        mouse_sensitivity: config.get_i32("mouse_sensitivity"),
        keys,
    };

    let mut game = match Game::new(game_config, resources, &engine_args) {
        Ok(game) => game,
        Err(err) => {
            eprintln!("doom-rs: error: could not start the game: {err}");
            eprintln!(
                "hint: the IWAD may be corrupt, or a required lump (COLORMAP, TEXTURE1, ...) is missing"
            );
            return ExitCode::FAILURE;
        }
    };

    let audio = if no_sound {
        tracing::info!("audio disabled by -nosound");
        None
    } else {
        match audio::start(&mut game) {
            Ok(output) => {
                tracing::info!(
                    "audio: {} Hz, {} channel(s), {:?}",
                    output.device_rate,
                    output.channels,
                    output.sample_format
                );
                Some(output)
            }
            Err(message) => {
                // never let a missing device kill the game
                tracing::warn!("audio output unavailable ({message}); continuing without sound");
                None
            }
        }
    };

    let gilrs = match gilrs::Gilrs::new() {
        Ok(gilrs) => Some(gilrs),
        Err(err) => {
            tracing::warn!("gamepads unavailable ({err}); continuing with keyboard and mouse");
            None
        }
    };

    let mode_name = match game_mode {
        GameMode::Shareware => "shareware",
        GameMode::Registered => "registered",
        GameMode::Commercial => "commercial",
        GameMode::Retail => "ultimate",
    };
    // startup summary on the log - stdout stays clean for --help/--version
    tracing::info!("doom-rs {VERSION} (doom-engine {ENGINE_VERSION})");
    tracing::info!("IWAD: {} ({mode_name})", iwad.display());
    if !engine_args.pwads.is_empty() {
        let names: Vec<String> = engine_args
            .pwads
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        tracing::info!("PWADs: {}", names.join(", "));
    }
    if audio.is_none() && !no_sound {
        tracing::info!("audio: disabled (no output device)");
    }

    let event_loop = match winit::event_loop::EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(err) => {
            eprintln!("doom-rs: error: could not create the event loop: {err}");
            return ExitCode::FAILURE;
        }
    };
    let context = match softbuffer::Context::new(event_loop.owned_display_handle()) {
        Ok(context) => context,
        Err(err) => {
            eprintln!("doom-rs: error: could not create the drawing context: {err}");
            return ExitCode::FAILURE;
        }
    };

    let app = app::App::new(app::AppInit {
        game,
        palette,
        gamma_lut,
        fullscreen: platform.fullscreen,
        window_width: SCREEN_WIDTH * platform.scale,
        window_height: SCREEN_HEIGHT * platform.scale,
        bindings: keys,
        audio,
        gilrs,
        context,
    });
    let fatal_error = match app::run(app, event_loop) {
        Ok(fatal) => fatal,
        Err(message) => {
            eprintln!("doom-rs: error: {message}");
            return ExitCode::FAILURE;
        }
    };
    if fatal_error {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
