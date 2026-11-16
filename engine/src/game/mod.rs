//! The game loop: the glue that ties every subsystem together into a
//! playable engine, ported from `g_game.c`, `p_tick.c`, `p_user.c`,
//! `p_setup.c` (the level bring-up), `p_saveg.c` (save games), the demo
//! machinery in `g_game.c` and `d_main.c` (the application shell), and
//! `d_ticcmd.h` (player commands).
//!
//! This module does not reimplement any gameplay math; it routes data
//! between the subsystems and owns the global state the original kept in
//! `doomstat.h`. Layout:
//!
//! - [`game`] - [`game::Game`]: the state machine (`GS_LEVEL`,
//!   `GS_INTERMISSION`, `GS_FINALE`, `GS_DEMOSCREEN`), the ticker, the
//!   level bring-up (`P_SetupLevel`), the player think pass
//!   (`P_PlayerThink`), the level-exit flow and the platform-facing API
//!   ([`game::Game::new`], [`game::Game::tick_input`],
//!   [`game::Game::advance`], [`game::Game::should_quit`]).
//! - [`input`] - player input and the `G_BuildTiccmd` port
//!   ([`input::TicCmdInput`] in, [`player::TicCmd`] out).
//! - [`render`] - the single frame assembly: the BSP/wall pass, planes,
//!   sky, the sprite/masked pass, the weapon overlay, the status bar, the
//!   HUD, the automap and the menu, in vanilla order.
//! - [`demo`] - the `.lmp` demo format: recording, playback, the marker
//!   byte and the header ([`demo::DemoPlayback`] / [`demo::DemoRecording`]).
//! - [`savegame`] - the `p_saveg.c` port: serialization of the world
//!   state into a self-contained byte block and back, with checks on the
//!   way in ([`savegame::SaveBlock`]).
//! - [`finale_tables`] - the `CastTables` implementation over the ported
//!   info tables, for the DOOM II MAP30 cast call.
//!
//! What is deliberately absent: networking (`d_net.c`/`i_net.c` - the
//! engine is single-player and demo-deterministic), a windowing or input
//! backend (platform shells feed [`input::TicCmdInput`] and friends),
//! and any `winit` dependency - everything here is testable headless.

mod demo;
mod finale_tables;
// `game::game` mirrors how the other singleton modules are laid out
// (`game::render`, `game::input`); renaming the inner file would churn
// every path for no real gain
#[allow(clippy::module_inception)]
mod game;
mod input;
mod render;
mod savegame;
#[cfg(test)]
mod tests;

pub use demo::{DEMO_MARKER, DEMO_VERSION, DemoPlayback, DemoRecording};
pub use finale_tables::GameCastTables;
pub use game::{Game, GameAction, GameConfig, GameState, InputEvent, KeyBindings, TICRATE, keys};
pub use input::{MAX_PLMOVE, TicBuilder, TicCmdInput};
pub use savegame::{SAVE_MAGIC, SAVE_VERSION, SaveBlock, SaveGameError};
