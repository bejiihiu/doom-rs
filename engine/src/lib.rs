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
//! - `intermission` - the between-maps stat screen: the interactive
//!   count-up, the episode world map with the "You Are Here" pointer,
//!   the deathmatch matrix and the animated backgrounds, ported from
//!   `wi_stuff.c`.
//! - `thinker` - the vanilla thinker registry: the THINKERS list from
//!   `p_tick.c`/`d_think.h` re-engineered as a generational arena, with
//!   vanilla's insertion-order execution and lazy removal semantics.
//! - `mobj` - game objects: the `mobj_t` from `p_mobj.h`, its spawning
//!   from `p_mobj.c` (`P_SpawnMobj`/`P_SpawnMapThing`/`P_RemoveMobj`/
//!   `P_SetMobjState`), the item respawn queue and the sector/blockmap
//!   thing lists.
//! - `player` - the per-player state: `player_t` from `d_player.h`
//!   (plus the `ticcmd_t` from `d_ticcmd.h`) and its weapon/ammo/key/
//!   power enumerations.
//! - `info` - the game data tables the gameplay layer drives: `states[]`,
//!   `mobjinfo[]`, `sprnames`, `S_sfx[]`/`S_music[]` and the switch
//!   on/off texture pairs, ported from `info.c`/`sounds.c`/`p_switch.c`.
//! - `movement` — movement and collision: `P_CheckPosition`/`P_TryMove`,
//!   the slide move, `P_PathTraverse` and the momentum ticks from
//!   `p_mobj.c`, ported from `p_map.c`/`p_maputl.c`/`p_mobj.c`.
//! - `specials` - line and sector specials: doors (`p_doors.c`), the
//!   wall-switch machinery (`p_switch.c`), light-flash thinkers
//!   (`p_lights.c`), teleports (`p_telept.c`), floors (`p_floor.c`),
//!   elevator platforms (`p_plats.c`), ceilings and crushers
//!   (`p_ceilng.c`), the `p_spec.c` line-special dispatcher
//!   ([`specials::handle_line_activation`] and friends) and the
//!   `P_SpawnSpecials`/`P_UpdateSpecials` scan. The moving-sector
//!   machinery shared by movers (the `T_MovePlane`/`P_ChangeSector`
//!   half of `p_floor.c`/`p_map.c`) lives in the module too.
//!   Side effects come out as plain-data events for the game loop.
//! - `weapons` — the player weapons: the overlay (psprite) state
//!   machines, the `A_*` weapon callbacks, the `weaponinfo[]` table and
//!   the muzzle flash, ported from `p_pspr.c` / `d_items.c` (plus
//!   `P_SpawnPlayerMissile` from `p_mobj.c`).
//! - `audio` - sound effects: the DMX lump decoder, the mixing core
//!   (ported from `i_sound.c`), the game-side sound model (ported from
//!   `s_sound.c`: channel priority eviction, distance attenuation,
//!   singularity handling) and the cpal output backend.
//! - `music` - music: the DMX MUS format parser, the GENMIDI OPL instrument
//!   bank, MUS to MIDI conversion and OPL2 FM synthesis in-process.
//! - `game` — game state, the tic loop, demos (`.lmp`), and save/load
//!   (`p_saveg.c`): the `p_tick.c` ticker, the `p_user.c` player think
//!   pass, the level bring-up from `p_setup.c`, and the whole frame
//!   assembly from `d_main.c`/`r_main.c` — BSP traversal, planes, sky,
//!   sprites, weapon overlay, status bar, HUD, automap and menu.
//!   Everything is driven from headless tests; no windowing.
//!
//! Auxiliary developer tools live in the `tools/` directory of the
//! workspace: `wad-inspector`, `asset-inspector` and `demo-analyzer`.
//!
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

/// Interactions: the `p_inter.c` port. Damage, death, pickups and the
/// item/kill tally for the intermission stats.
///
/// [`inter::damage_mobj`] applies damage (armor saves, the hell-level
/// endgame hack, thrust from the inflictor, pain/wake-up, death),
/// [`inter::kill_mobj`] handles the death transition (drop items, the
/// kill/frag counters, player death) and [`inter::touch_special_thing`]
/// is the sprite-keyed pickup switch (weapons, ammo, armor, keys, power
/// up artifacts, backpack). Side effects the game loop must apply come
/// out as plain-data [`enemy::GameEvent`]s (state chains) and
/// [`inter::InterEvent`]s (sounds, the weapon drop, the automap stop).
pub mod inter;

/// Monster AI: the `p_enemy.c`/`p_sight.c` port.
///
/// The `A_*` state callbacks monsters hang off `states[]` (`A_Look`,
/// `A_Chase`, the melee/missile attacks, the viles, the DOOM II brain, ...),
/// the line-of-sight machinery (`P_CheckSight` with the REJECT matrix and
/// the BSP slope-window walk) and the noise-alert flood
/// (`P_NoiseAlert`/`P_RecursiveSound`). Damage, sounds and special-line
/// effects come back as plain-data [`enemy::GameEvent`]s; the gameplay
/// layer dispatches them. [`enemy::set_mobj_state_and_run_actions`] wraps
/// [`mobj::MobjStore::set_mobj_state`] and runs the A_* actions a state
/// chain produces, recursing through nested state changes the way vanilla
/// recursion does.
pub mod enemy;

/// Game objects: the `mobj_t` from `p_mobj.h` and the spawn/lifecycle
/// half of `p_mobj.c`.
///
/// [`mobj::MobjStore`] owns the thinker registry and the runtime thing
/// lists (per sector / per blockmap cell) and spawns map things from
/// THINGS data; [`mobj::Mobj`] is the actor every gameplay object is.
pub mod mobj;

/// The thinker registry: the vanilla THINKERS list from `p_tick.c`.
///
/// Thinkers are stored in a generational arena and run in insertion
/// order with vanilla's lazy removal semantics; new thinker kinds are
/// added as variants of [`thinker::Thinker`].
pub mod thinker;

/// The per-player state: `player_t` from `d_player.h`.
///
/// The avatar itself is a regular [`mobj::Mobj`] of type `MT_PLAYER`;
/// [`player::Player`] carries view state, inventory, cheats and the
/// overlay psprites.
pub mod player;

/// Game data tables, ported from `info.c`/`info.h`, `sounds.c`/`sounds.h`
/// and `p_switch.c`.
///
/// The `states[]` table, `mobjinfo[]` table, `sprnames[]`, the `S_sfx[]` /
/// `S_music[]` tables and the switch on/off texture pairs the gameplay
/// layer drives live here. Everything is plain static data keyed by
/// `#[repr(u16)]` enums mirroring the vanilla enum values.
pub mod info;

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

/// Player weapons: the overlay (psprite) state machines and the `A_*`
/// weapon callbacks, ported from `p_pspr.c` (plus the `weaponinfo[]`
/// table from `d_items.c`).
///
/// [`weapons::setup_psprites`]/[`weapons::move_psprites`] are the
/// per-level/per-tic drivers the game loop calls;
/// [`weapons::set_psprite`] applies a state change (the vanilla
/// `P_SetPsprite`) and dispatches the `A_*` actions the chain produces,
/// and [`weapons::dispatch_weapon_action`] is the target of that
/// dispatch (the gameplay layer chains its own dispatcher from the
/// `false` case). Sounds and damage come out as plain-data
/// [`enemy::GameEvent`]s like everywhere else in the gameplay layer.
/// [`weapons::WEAPON_INFO`] mirrors `weaponinfo[]` (`d_items.c`) and
/// [`weapons::draw_player_psprites`] is the view half — `R_DrawPSprite`
/// for the overlay sprites.
pub mod weapons;

/// Line and sector specials: doors (the `p_doors.c` port), the wall
/// switch machinery (the `p_switch.c` port), light-flash thinkers
/// (`p_lights.c`), teleports (`p_telept.c`), floors (the `p_floor.c`
/// port: `EV_DoFloor`, stairs, donuts), elevator platforms (the
/// `p_plats.c` port), ceiling crushers (the `p_ceilng.c` port) and the
/// `p_spec.c` line-special interpreter
/// ([`specials::handle_line_activation`] plus the `use`/`cross`/`shoot`
/// tables, the `P_SpawnSpecials` map-load scan, the `P_UpdateSpecials`
/// button/timer/scroll tick and `P_PlayerInSpecialSector`). The
/// moving-sector machinery shared by movers (the `T_MovePlane`/
/// `P_ChangeSector` half of `p_floor.c`/`p_map.c`) lives in the module
/// too.
///
/// Thinkers are [`thinker::Thinker`] variants in the shared registry,
/// dispatched by [`specials::run_thinkers`]; the `EV_*` entry points are
/// called by the line-special interpreter. Side effects come out as
/// plain-data [`specials::SpecialEvent`]s; exits are
/// [`specials::SpecialEvent::ExitLevel`].
pub mod specials;

/// Movement and collision: `p_map.c`, `p_maputl.c` and the movement half of
/// `p_mobj.c`.
///
/// The `tm*` globals become [`movement::MoveContext`]; [`movement::try_move`]
/// is the position commit point, [`movement::slide_move`] handles wall
/// sliding, [`movement::traverse_path`] is the shared line/thing trace, and
/// [`movement::xy_movement`] / [`movement::z_movement`] are the momentum
/// ticks. Side effects the gameplay layer owns (damage, pickups, line
/// specials, sounds) come back as plain-data [`movement::MoveEvent`]s.
pub mod movement;

/// Audio: sound effects. The DMX lump decoder, the mixing core
/// (`i_sound.c` port), the game-side sound model (`s_sound.c` port:
/// channel priority eviction, distance attenuation, singularity handling)
/// and the output backends (cpal for real playback, an in-memory one for
/// tests). Music lives in its own module instead; see [`music`].
///
/// [`audio::SoundEngine`] owns the logical channels and the mixer; the
/// backend reaches the same mixer through [`audio::SharedMixer`].
pub mod audio;

pub mod config;
pub mod errors;
pub mod fixed;
pub mod math;
pub mod tables;

/// Music: the DMX MUS format and OPL2 FM synthesis, in-process.
///
/// [`music::MusicEngine`] parses MUS lumps, synthesizes them through an
/// OPL2 emulator with the `GENMIDI` instrument bank, and serves mono
/// samples through [`music::MusicEngine::pull_samples`]. The platform
/// layer drives it on its own clock; playback speed is the authentic
/// DMX time division (140 ticks per second).
pub mod music;

/// The game loop, ported from `g_game.c`/`p_tick.c`/`p_user.c`/`p_setup.c`
/// and the `d_main.c` application shell: game state (`GS_LEVEL` and
/// friends), the ticker, the level bring-up, demos, save games and the
/// one-pass frame renderer.
///
/// [`game::Game`] owns every subsystem and is the entry point for the
/// platform shell: [`game::Game::new`] builds it from a WAD and the
/// parsed command line, [`game::Game::tick_input`] runs one tic and one
/// frame, [`game::Game::should_quit`] reports the player's quit request,
/// and [`game::InputEvent`] carries OS-level input into the engine.
pub mod game;

/// Version of the engine crate. Re-exported for binaries and tools so they
/// can report which engine they were built against.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
