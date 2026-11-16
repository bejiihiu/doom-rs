//! The automap, ported from `am_map.c` (linuxdoom-1.10).
//!
//! What is (and is not) here:
//!
//! * the **map window** — a full-width, `SCREENHEIGHT - 32` tall window
//!   at the top of the framebuffer (the bottom 32 rows belong to the
//!   status bar). The automap is not an overlay in 1.10: when it is
//!   active the 3D view is not drawn at all and the window is the view.
//!   The map name title is the HUD's business (see [`crate::hud`]);
//!   this module does not draw it.
//! * the **walls** — every linedef is offered to a Cohen-Sutherland
//!   clip against the window and drawn with vanilla's Bresenham.
//!   Despite what the blockmap docs of other ports suggest, 1.10 does
//!   **not** cull lines through the blockmap: [`crate::automap::render_automap`]
//!   iterates every linedef and lets the clip reject off-screen ones,
//!   exactly like the original.
//! * line colors — 1-sided walls, teleporters (`special == 39`), secret
//!   doors (`ML_SECRET`), floor/ceiling height changes and, under the
//!   `iddt` cheat, two-sided walls. Unopened-door coloring does not
//!   exist in 1.10; ML_SECRET lines draw as plain wall color unless the
//!   map is fully revealed.
//! * the **player arrow** — a line-art arrow rotated by the player's
//!   angle, plus per-player colors in network games and a distinct
//!   "cheeseburger" arrow while cheating. Invisible players are drawn
//!   in a near-black color, like `246` in the original.
//! * **marks** — ten user marks (`m` key) drawn from the `AMMNUM0..9`
//!   patches, cycled in order.
//! * the **seen set** — vanilla tracks seen lines through the
//!   `ML_MAPPED` bit on the linedef itself. The map port keeps the map
//!   immutable, so the automap keeps its own bitset in
//!   [`crate::automap::AutomapState::mark_lines_seen`], which the renderer feeds from
//!   its `take_mapped_linedefs` output.
//! * the **`iddt` cheat** — cycles the reveal level `0 -> 1 -> 2 -> 0`:
//!   `0` draws only seen lines, `1` draws everything (secret lines keep
//!   their special color), `2` additionally draws every thing in the
//!   level as a thin triangle. The `iddt` key sequence itself lives in
//!   the menu cheat table ([`crate::menu::CheatTable`]); the game layer
//!   wires `CheatEvent::Automap` to [`crate::automap::AutomapState::cycle_iddt`].
//!
//! Layout constants, the palette indices and the window size are
//! byte-for-byte ports of the `#define`s in `am_map.c`. There is no
//! strobing light effect: vanilla's `AM_updateLightLev` call is
//! commented out and `lightlev` stays `0`, so the port uses the base
//! colors directly.
//!
//! ```no_run
//! use doom_engine::automap::{AutomapState, AutomapView, PlayerMarker};
//! # let map: &doom_engine::map::Map = unimplemented!();
//! # let me = PlayerMarker { x: doom_engine::fixed::Fixed::ZERO, y: doom_engine::fixed::Fixed::ZERO, angle: Default::default(), invisible: false, is_local: true };
//! # let view = AutomapView { me, players: &[], netgame: false, deathmatch: false, singledemo: false, all_map: false, things: &[] };
//! let mut automap = AutomapState::new();
//! automap.begin_level(map);
//! automap.start(map, me);
//! assert!(automap.is_active());
//! ```

mod draw;
mod state;
#[cfg(test)]
pub(crate) mod testutil;

pub use draw::render_automap;
pub use state::{
    AM_NUMMARKPOINTS, AutomapInput, AutomapKey, AutomapResponse, AutomapState, AutomapView,
    PlayerMarker, ThingMarker,
};

use crate::fixed::Fixed;

/// Player radius (`16 * FRACUNIT`, p_local.h). Used for the minimum zoom
/// scale, the minimum window size and the arrow geometry.
pub(crate) const PLAYER_RADIUS: Fixed = Fixed::from_raw(16 << 16);

/// The framebuffer window geometry: the window is the full screen width
/// and `SCREENHEIGHT - 32` tall, at the top of the screen. `f_x/f_y`
/// stay `0` in 1.10; the window is parameterized anyway so the port
/// does not have magic constants scattered around.
pub const FINIT_WIDTH: i32 = 320;
/// Height of the automap window (168 = 200 - 32).
pub const FINIT_HEIGHT: i32 = 200 - 32;

// Automap palette indices (the `#define` table from am_map.c). These
// index the PLAYPAL, not the colormap chain.
/// `BACKGROUND` — black.
pub const COLOR_BACKGROUND: u8 = 0;
/// `YOURCOLORS` — white, the local player's arrow.
pub const COLOR_YOURS: u8 = 209; // 256-47
/// `WALLCOLORS` — reddish, solid walls.
pub const COLOR_WALL: u8 = 176; // 256-5*16
/// `WALLRANGE` — palette steps between wall shades (unused in 1.10's draw path).
pub const WALL_RANGE: u8 = 16;
/// `TSWALLCOLORS` — gray, two-sided walls under the `iddt` cheat.
pub const COLOR_TS_WALL: u8 = 96; // 6*16
/// `TSWALLRANGE` — palette steps between two-sided wall shades.
pub const TS_WALL_RANGE: u8 = 16;
/// `FDWALLCOLORS` — brown, floor-height changes.
pub const COLOR_FD_WALL: u8 = 64; // 4*16
/// `FDWALLRANGE` — palette steps between floor-change shades.
pub const FD_WALL_RANGE: u8 = 16;
/// `CDWALLCOLORS` — yellowish, ceiling-height changes.
pub const COLOR_CD_WALL: u8 = 231; // 256-32+7
/// `CDWALLRANGE` — palette steps between ceiling-change shades.
pub const CD_WALL_RANGE: u8 = 1;
/// `THINGCOLORS` — green, things under the `iddt` cheat (level 2).
pub const COLOR_THING: u8 = 112; // 7*16
/// `THINGRANGE` — palette steps between thing shades.
pub const THING_RANGE: u8 = 16;
/// `SECRETWALLCOLORS` — secret doors. Vanilla maps this to
/// `WALLCOLORS`, so it numerically equals [`COLOR_WALL`]; it is kept
/// separate because the cheat branches (revealed vs. hidden secret)
/// name different vanilla symbols.
pub const COLOR_SECRET_WALL: u8 = COLOR_WALL;
/// `GRIDCOLORS` — `GRAYS + GRAYSRANGE/2`, the level grid lines.
pub const COLOR_GRID: u8 = 104; // 96 + 16/2
/// `XHAIRCOLORS` — the single-pixel crosshair.
pub const COLOR_CROSSHAIR: u8 = 96; // GRAYS
/// Color of a non-local, invisible player (`246`, "*close* to black").
pub const COLOR_INVISIBLE_PLAYER: u8 = 246;
/// The `teleporter` line special (`special == 39`) draws in this
/// lighter wall shade: `WALLCOLORS + WALLRANGE/2`.
pub const COLOR_TELEPORT_LINE: u8 = COLOR_WALL + WALL_RANGE / 2;
/// `GRAYS+3` — unmapped lines under the computer-map power-up.
pub const COLOR_ALL_MAP: u8 = 96 + 3;

/// The player arrow colors in a network game, indexed by player slot
/// (`their_colors[]` in am_map.c): green, gray, brown, red.
pub(crate) const NET_PLAYER_COLORS: [u8; 4] =
    [COLOR_THING, COLOR_TS_WALL, COLOR_FD_WALL, COLOR_WALL];

/// Line special that marks a teleporter line (`39`).
pub(crate) const TELEPORTER_SPECIAL: i16 = 39;
