//! `doom-rs` - the DOOM engine, rewritten in Rust, running in a real window.
//!
//! Startup order: parse our own flags, hand the remaining tokens to the
//! engine's vanilla-style argument parser, resolve the IWAD, load
//! `doom.cfg`, build the game, then open the window and run the loop.
//! Any failure before the window opens is a message on stderr and exit
//! code 1 - never a panic, never a half-initialized game.

use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use doom_engine::VERSION as ENGINE_VERSION;
use doom_engine::args;
use doom_engine::config::DoomConfig;
use doom_engine::game::{Game, GameConfig, KeyBindings};
use doom_engine::gfx::palette::parse_playpal;
use doom_engine::log::init_logging;
use doom_engine::menu::GameMode;
use doom_engine::resources::ResourceManager;
use doom_engine::wad::WadFile;

const SCREEN_WIDTH: u32 = 320;
const SCREEN_HEIGHT: u32 = 200;

mod app;
mod audio;
mod input;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
doom-rs - a faithful rewrite of the DOOM engine in Rust

Usage: doom-rs [OPTIONS] [-iwad PATH] [-file WAD...] [ENGINE OPTIONS...]

Options:
  --fullscreen      Run in borderless fullscreen mode
  --scale N         Integer scale factor for the 320x200 view (1-8, default 4)
  -h, --help        Print this help and exit
  -V, --version     Print the version and exit

Engine options (DOOM-style):
  -iwad PATH        Path to the IWAD (doom1.wad, doom.wad, doom2.wad, ...)
  -file WAD...      Layer one or more PWADs on top of the IWAD
  -warp E M         Start on episode E map M (DOOM) / map N (DOOM II)
  -skill N          Difficulty 1-5 (default: medium)
  -nomonsters       Do not spawn monsters
  -respawn          Monsters respawn after dying
  -fast             Monsters move and attack faster
  -deathmatch       Deathmatch mode
  -altdeath         Alt deathmatch rules (with -deathmatch, alt wins)
  -nosound          Disable audio output
  -playdemo DEMO    Play back a demo
  -timedemo DEMO    Time a demo playback
  -record DEMO      Record a demo
  -config PATH      Override the config file (default: ./doom.cfg)
  -turbo N          Move speed percentage (10-400)

The IWAD is resolved in this order: -iwad, the DOOM_IWAD environment
variable, then the current directory (doom2.wad, doom2f.wad,
plutonia.wad, tnt.wad, doomu.wad, doom.wad and doom1.wad).

Keys (defaults): arrows/WASD to move, Ctrl or left mouse to fire, Space
or right mouse to use, Tab for the automap, 1-9 to select weapons,
mouse to turn, Escape for the menu.";

fn print_version() {
    println!("doom-rs {VERSION} (doom-engine {ENGINE_VERSION})");
}

/// Outcome of splitting the raw arguments into our own flags and the
/// tokens the engine should see.
struct PlatformArgs {
    fullscreen: bool,
    scale: u32,
    engine_argv: Vec<OsString>,
}

/// Splits `--fullscreen`, `--scale N`/`--scale=N`, `-h`/`--help` and
/// `-V`/`--version` out of the raw argument vector. Everything else is
/// passed to the engine untouched - including the single-dash vanilla
/// flags, which the engine does not know are not ours.
fn split_platform_args(raw: &[OsString]) -> Result<Option<PlatformArgs>, String> {
    let mut engine_argv = Vec::with_capacity(raw.len());
    engine_argv.push(
        raw.first()
            .cloned()
            .unwrap_or_else(|| OsString::from("doom-rs")),
    );

    let mut fullscreen = false;
    let mut scale: u32 = 4;

    let mut it = raw.iter().skip(1);
    while let Some(token) = it.next() {
        let text = token.to_string_lossy();
        if text == "--fullscreen" {
            fullscreen = true;
        } else if text == "--scale" {
            let value = it
                .next()
                .ok_or("doom-rs: --scale expects a value between 1 and 8")?;
            scale = parse_scale(value)?;
        } else if let Some(value) = text.strip_prefix("--scale=") {
            scale = parse_scale(OsStr::new(value))?;
        } else if text == "--help" || text == "-h" {
            println!("{USAGE}");
            return Ok(None);
        } else if text == "--version" || text == "-V" {
            print_version();
            return Ok(None);
        } else {
            engine_argv.push(token.clone());
        }
    }
    Ok(Some(PlatformArgs {
        fullscreen,
        scale,
        engine_argv,
    }))
}

fn parse_scale(token: &OsStr) -> Result<u32, String> {
    let text = token
        .to_str()
        .ok_or("doom-rs: --scale expects a number between 1 and 8")?;
    let value: u32 = text
        .trim()
        .parse()
        .map_err(|_| format!("doom-rs: --scale expects a number between 1 and 8, got {text:?}"))?;
    if !(1..=8).contains(&value) {
        return Err(format!(
            "doom-rs: --scale must be between 1 and 8, got {value}"
        ));
    }
    Ok(value)
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
    init_logging();

    let raw: Vec<OsString> = env::args_os().collect();
    let platform = match split_platform_args(&raw) {
        Ok(Some(platform)) => platform,
        // --help / --version: already handled, clean exit from a --no-window run
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("doom-rs: {message}");
            eprintln!(
                "usage: doom-rs [--fullscreen] [--scale N] [ENGINE OPTIONS] (try doom-rs --help)"
            );
            return ExitCode::FAILURE;
        }
    };

    let engine_args = match args::parse(&platform.engine_argv) {
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
    println!("doom-rs {VERSION} (doom-engine {ENGINE_VERSION})");
    println!("IWAD: {} ({mode_name})", iwad.display());
    if !engine_args.pwads.is_empty() {
        let names: Vec<String> = engine_args
            .pwads
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        println!("PWADs: {}", names.join(", "));
    }
    if audio.is_none() && !no_sound {
        println!("audio: disabled (no output device)");
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
