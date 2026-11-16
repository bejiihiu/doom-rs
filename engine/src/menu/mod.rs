//! The DOOM menu system: the main menu, new-game/episode/skill flow,
//! the options menu, the sound volume menu, the load/save menus, the
//! read-this pages, the quit/end-game prompts and the cheat-code
//! matcher — the `m_menu.c` / `m_cheat.c` port (linuxdoom-1.10).
//!
//! What is here:
//!
//! * the **state machine** — [`Menu`] tracks the current [`MenuLevel`],
//!   the cursor (`itemOn`), the `lastOn` per level, the skull cursor
//!   animation (`M_Ticker`), wraps the cursor around at the ends and
//!   skips the spacer slots, exactly like `M_Responder`;
//! * the **message prompts** — quit ("are you sure you want to quit
//!   this great game?"), end-game ("are you sure you want to end the
//!   game?"), nightmare-skill confirm, and the various
//!   "you can't do that in a netgame" notices. A message captures the
//!   keyboard until answered; like the original, answering *any* prompt
//!   closes the menu afterwards, even "no";
//! * the **save-game string editor** — the `M_SaveSelect`/`M_DoSave`
//!   flow, with the vanilla width limit (the string may not exceed
//!   `(SAVESTRINGSIZE - 2) * 8` pixels) and `Escape` restoring the old
//!   description;
//! * **rendering** — all patches are drawn with the vanilla layout
//!   (`M_DrawOptions`, `M_DrawSound`, ...) with the HUD font for
//!   text; the skull cursor alternates every 8 tics;
//! * **cheat codes** — [`CheatTable`] ports the scrambled sequences
//!   from `st_stuff.c`/`am_map.c` and emits plain-data [`CheatEvent`]s
//!   instead of touching the player;
//! * the **`F`-key entry points** — [`Menu::open_help`],
//!   [`Menu::open_save`], [`Menu::open_load`], [`Menu::open_sound`],
//!   [`Menu::toggle_detail`], [`Menu::toggle_messages`],
//!   [`Menu::end_game`], [`Menu::quit_prompt`], [`Menu::quick_save`] and
//!   [`Menu::quick_load`] — the hooks `M_Responder` binds to F1..F10.
//!
//! Deliberately not here:
//!
//! * the F-key *bindings* themselves (KEY_F1 …) — they belong to the
//!   input layer, which will call the methods above;
//! * gamma cycling (F11) — a video-layer concern, not the menu;
//! * reading savegame descriptions from disk (`M_ReadSaveStrings`) —
//!   the game layer scans the directory and hands the menu a
//!   [`SaveSlotInfo`] table with [`Menu::set_save_names`];
//! * devparm screenshots (F1 in devparm builds);
//! * the `M_DrawEmptyCell`/`M_DrawSelCell` helpers — dead code in the
//!   original, never ported;
//! * the "read this!" image *content* — the port draws whatever patches
//!   the IWAD ships (`HELP`/`HELP1`/`HELP2`/`CREDIT`), full screen.
//!
//! Everything the menu needs from the outside comes through
//! [`MenuContext`]; everything the game needs out of it comes out as
//! [`MenuEvent`] / [`MenuSound`]. The quit-message nag picker uses the
//! `gametic` value passed to [`Menu::tick`].

mod cheat;
mod levels;
mod render;
mod state;
mod strings;

pub use cheat::{CheatContext, CheatEvent, CheatPowerUp, CheatSequence, CheatTable};
pub use levels::{ItemStatus, MenuLevel};
pub use render::{MenuAssets, render_menu};
pub use state::{
    GameMode, LINEHEIGHT, Menu, MenuContext, MenuEvent, MenuKey, MenuKeyResult, MenuMessage,
    MenuOptions, MenuSound, QUICKSAVE_NONE, QUICKSAVE_PICK, SAVE_SLOTS, SAVESTRINGSIZE, SKILL_BABY,
    SKILL_EASY, SKILL_HARD, SKILL_MEDIUM, SKILL_NIGHTMARE, SKULLXOFF, SaveSlotInfo,
};
pub use strings::{EMPTY_SLOT, quit_message};
