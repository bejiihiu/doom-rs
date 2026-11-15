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
//! - `errors` — engine error types.
//! - `config` — startup configuration handling.
//! - `wad` — WAD archive reading and lump lookup.
//! - `resources` — centralized resource manager and lump lifecycle.
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

pub mod config;
pub mod errors;
pub mod fixed;
pub mod math;
pub mod tables;

/// Version of the engine crate. Re-exported for binaries and tools so they
/// can report which engine they were built against.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
