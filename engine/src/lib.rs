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
//! - `render` — the software renderer: framebuffer, columns, spans and
//!   lighting.
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
/// the `zlight`/`scalelight` tables and the gamma remap tables.
///
/// Works on raw palette indices; the palette is applied at output time
/// only ([`render::IndexedFramebuffer::to_rgba_bytes`]).
pub mod render;

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
