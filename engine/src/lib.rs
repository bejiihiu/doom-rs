//! # doom-engine
//!
//! `doom-engine` is a rewrite of the classic DOOM engine in Rust. It targets
//! behavioral compatibility with the original engine while being built for
//! modern platforms and idiomatic Rust.
//!
//! ## Architecture
//!
//! The engine is organized as small, focused modules. Modules are added as
//! the port progresses; the intended layout is:
//!
//! - `fixed` — 16.16 fixed-point arithmetic used throughout the engine.
//! - `tables` — fine angle tables (`FINE_SINE`/`FINE_COSINE`/`FINE_TANGENT`)
//!   and the BAM [`tables::Angle`] type.
//! - `math` — geometry helpers: line side tests, bounding boxes, distance
//!   and angle differences.
//! - `rng` — deterministic pseudo-random number generation ([`rng`]).
//! - `swap` — bounds-checked little-endian reads/writes on byte slices
//!   ([`swap`]).
//! - `args` — command-line argument parsing, ported from `m_argv.c`
//!   ([`args`]).
//! - `log` — global `tracing` subscriber setup ([`log`]).
//! - `errors` — engine error types.
//! - `config` — startup configuration and the ported `doom.cfg`
//!   system ([`config`]).
//! - `wad` — WAD archive reading and lump lookup.
//! - `resources` — centralized resource manager and lump lifecycle.
//! - `scratch` — managed scratch space: atomic file writes staged in a
//!   `cachegirl`-tracked temp file with crash recovery ([`scratch`]).
//! - `gfx` — graphics data formats: palettes, colormaps, patches, flats
//!   and sprites.
//! - `map` — map lump loading and validation.
//! - `bsp` — BSP tree traversal and wall clipping.
//! - `render` - the software renderer: framebuffer, columns, spans,
//!   lighting, wall/plane rasterizers, sprites, masked passes and sky.
//! - `stbar` - the status bar: widgets, layout, face state machine.
//! - `hud` — the heads-up display: the message widget and the level
//!   title, plus the ported `d_englsh.h` message strings.
//! - `menu` — the menu system: main menu, options, load/save menus,
//!   the quit/end-game prompts and the cheat-code matcher, ported from
//!   `m_menu.c`/`m_cheat.c` (includes the `d_englsh.h` prompt strings).
//! - `automap` — the automap: follow/zoom/marks, the map window,
//!   line colors, seen-line tracking, ported from `am_map.c`.
//! - `wipe` — the screen melt transition, ported from `f_wipe.c`
//!   (the color fade type is deliberately not ported; the game never
//!   used it).
//! - `finale` — the end-of-episode finale: typed-out texts, the art
//!   screens (including the E3 bunny scroll) and the DOOM II MAP30
//!   cast call, ported from `f_finale.c`.
//! - `intermission` — the between-maps stat screen: the interactive
//!   count-up, the episode world map with the "You Are Here" pointer,
//!   the deathmatch matrix and the animated backgrounds, ported from
//!   `wi_stuff.c`.
//! - `audio` — sound effects and music playback.
//! - `input` — keyboard, mouse and gamepad handling.
//! - `game` — game state, the tic loop, demo playback and save/load.
//!
//! Auxiliary developer tools live in the `tools/` directory of the
//! workspace: `wad-inspector`, `asset-inspector` and `demo-analyzer`.
//!
//! ## Status
//!
//! Early scaffolding. Only the modules documented as present below compile
//! today; the renderer and gameplay land incrementally.

/// Deterministic pseudo-random number generation, ported from `m_random.c`.
///
/// The engine owns generator instances ([`rng::DoomRng`]) instead of the
/// original's global `rndindex`, which keeps demo replays and net sync
/// deterministic.
pub mod rng;

/// Little-endian scalar reads and writes on byte slices.
///
/// Everything on this boundary is bounds-checked and returns
/// [`errors::EngineResult`]; corrupt lumps never cause a panic.
pub mod swap;

/// Command-line argument parsing, ported from `m_argv.c`.
///
/// [`args::parse`] consumes the full `argv` (including the program name,
/// like `std::env::args_os()`) and returns a typed [`args::EngineArgs`].
/// Keys are case-insensitive; unknown keys are collected and reported, not
/// fatal.
pub mod args;

/// Global `tracing` subscriber setup.
///
/// [`log::init_logging`] installs a compact fmt subscriber filtered by
/// `RUST_LOG` (default `info`). Idempotent — safe to call more than once.
pub mod log;

/// Centralized resource management.
///
/// [`resources::ResourceManager`] stacks the IWAD/PWADs into one global
/// namespace and serves raw lump bytes plus lazily-decoded typed values.
/// Decoded values are cached once per (type, lump) pair and shared; decode
/// failures are not cached.
pub mod resources;

/// WAD archive reading, stacking and lump lookup, ported from `w_wad.c`.
///
/// [`wad::WadFile`] validates an archive on load; [`wad::WadSet`] stacks
/// the IWAD/PWADs into one global namespace with vanilla's
/// last-file-overrides lookup.
pub mod wad;

/// Managed scratch space for atomic writes and crash-safe temp files.
///
/// [`scratch::atomic_write`] stages a payload in a `cachegirl`-tracked
/// temp file next to the target and renames it into place; crash debris
/// is reclaimed by the startup recovery pass. All durable file writes
/// in the engine go through here.
pub mod scratch;

/// Graphics data formats, ported from `r_data.c` / `v_video.c`.
///
/// Pure decoders for the graphics lumps: palettes, colormaps, patches,
/// textures, patch-name tables, flats and sprites. Every decoder validates
/// its input and returns [`gfx::GfxError`] on corrupt data instead of
/// panicking. Loading the lumps and caching the decoded values is the
/// resource manager's job; see [`resources`].
pub mod gfx;

/// The software renderer's core: the indexed framebuffer, the
/// low-level column/span drawers (`R_DrawColumn`/`R_DrawSpan` families),
/// the `zlight`/`scalelight` tables, the gamma remap tables, the wall
/// rasterizer (`R_StoreWallRange` and friends) and the visplane
/// bookkeeping the wall rasterizer marks. The sprite pass
/// (`R_ProjectSprite`/`R_DrawMasked`) and the sky column pass
/// (`R_DrawPlanes`' sky branch) are here too.
///
/// Works on raw palette indices; the palette is applied at output time
/// only ([`render::IndexedFramebuffer::to_rgba_bytes`]).
pub mod render;

/// The status bar: the `st_stuff.c`/`st_lib.c` port.
///
/// [`stbar::StatusBar`] decodes the bar's patches through the resource
/// manager once at startup; [`stbar::render_status_bar`] draws the bar
/// into the bottom 32 rows of the framebuffer every frame, redrawing
/// only what changed. The face (mugshot) state machine lives in
/// [`stbar::FaceMachine`] and is driven once per tic by the game layer.
pub mod stbar;

/// The heads-up display: the `hu_stuff.c`/`hu_lib.c` port.
///
/// [`hud::Hud`] decodes the HUD font (`STCFN033..STCFN095`) once at
/// startup; [`hud::render_hud`] ticks the message timeout and draws the
/// message widget and (while the automap is up) the level title. The
/// `d_englsh.h` player messages are ported as [`hud::HudMessage`].
pub mod hud;

/// The menu system: the `m_menu.c`/`m_cheat.c` port.
///
/// [`menu::Menu`] is the state machine (level, cursor, skull animation,
/// message prompts, the save-game string editor); it renders through
/// [`menu::MenuAssets`] into the framebuffer and reports player intent
/// as plain-data [`menu::MenuEvent`] / [`menu::MenuSound`] values.
/// Cheat codes go through [`menu::CheatTable`] and come out as
/// [`menu::CheatEvent`]s.
pub mod menu;

/// The automap: the `am_map.c` port.
///
/// [`automap::AutomapState`] is the retained state (window geometry,
/// zoom, follow mode, marks, seen-line tracking); [`automap::render_automap`]
/// draws the map window into the framebuffer. Input events come in as
/// [`automap::AutomapInput`] and player messages come back as
/// [`automap::AutomapResponse`]; the level-name title stays with the
/// HUD ([`hud::HudState`]).
pub mod automap;

/// The screen melt transition, ported from `f_wipe.c`.
///
/// [`wipe::ScreenWipe`] snapshots the current and the freshly drawn
/// screen and drips the new frame in column by column (2-pixel columns,
/// accelerating melt, per-column random delays). Only the melt is
/// ported — the 8-bit color fade was never used by the game.
pub mod wipe;

/// The end-of-episode finale, ported from `f_finale.c`.
///
/// [`finale::Finale`] is the state machine (typed-out text → art
/// screen, and the DOOM II MAP30 cast call). The info tables are
/// injected through [`finale::CastTables`]; music, sounds, wipes and
/// the world-done decision come out as [`finale::FinaleEvent`]s.
pub mod finale;

/// The between-maps intermission, ported from `wi_stuff.c`.
///
/// [`intermission::Intermission`] owns the stat count-up state machines
/// (single player / net game / deathmatch), the episode world map and
/// the animated background. Level names and graphics are decoded
/// through the resource manager at start; everything the game layer
/// must do comes out as [`intermission::IntermissionEvent`]s.
pub mod intermission;

/// BSP tree traversal and wall clipping, ported from `r_bsp.c`/`r_main.c`.
///
/// Pure math and state machines over [`map::Node`] and column spans; the
/// renderer feeds its output through `emit` callbacks. See [`bsp`] module
/// docs.
pub mod bsp;

/// Map parsing and in-memory representation, ported from `p_setup.c`.
///
/// Turns the raw map lumps into typed, fully validated structures;
/// every on-disk index is checked before it is stored, so downstream
/// code can trust the map instead of re-validating it. Corrupt maps
/// come back as [`map::MapError`], never as a panic. The `map` module
/// also carries the runtime blockmap queries
/// ([`map::lines_in_box`]) ported from `p_maputl.c`; they walk the grid
/// for a map-unit box and yield linedef indices in vanilla traversal
/// order, duplicates included.
pub mod map;

pub mod config;
pub mod errors;
pub mod fixed;
pub mod math;
pub mod tables;

/// Version of the engine crate. Re-exported for binaries and tools so they
/// can report which engine they were built against.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
