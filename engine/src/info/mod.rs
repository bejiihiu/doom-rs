//! Game data tables, ported from `info.c`/`info.h`, `sounds.c`/`sounds.h`
//! and `p_switch.c`. Everything here is plain static data, laid out exactly
//! like the vanilla tables so the mapping back to the C source stays 1:1.
//!
//! - [`sprites`] - `spritenum_t` and `sprnames`.
//! - [`states`] - `statenum_t`, the full `states[]` table (frame/tics/action
//!   per thing state) and [`states::FF_FULLBRIGHT`]. Action bodies are not
//!   here; `StateAction` only records which vanilla `A_*` function each
//!   state carries - the gameplay modules wire those up.
//! - [`sounds`] - `sfxenum_t`/`musicenum_t`, the `S_sfx[]`/`S_music[]`
//!   tables and the mixing constants from `s_sound.c`.
//! - [`mobjinfo`] - `mobjtype_t`, the full `mobjinfo[]` table (spawn health,
//!   states, sounds, radii, `MF_*` flags, ...).
//! - [`switches`] - the `alphSwitchList[]` on/off texture pairs
//!   (`P_InitSwitchList` resolves them to texture indices at startup).
//!
//! Runtime scratch that vanilla kept inside the table structs (`lumpnum`,
//! `usefulness`, `data`, `handle`, button list state) is deliberately not
//! ported here - it lives with the sound/mobility subsystems that own it.

mod mobjinfo;
mod sounds;
mod sprites;
mod states;
mod switches;

pub use mobjinfo::{
    MF_AMBUSH, MF_CORPSE, MF_COUNTITEM, MF_COUNTKILL, MF_DROPOFF, MF_DROPPED, MF_FLOAT, MF_INFLOAT,
    MF_JUSTATTACKED, MF_JUSTHIT, MF_MISSILE, MF_NOBLOCKMAP, MF_NOBLOOD, MF_NOCLIP, MF_NOGRAVITY,
    MF_NOSECTOR, MF_NOTDMATCH, MF_PICKUP, MF_SHADOW, MF_SKULLFLY, MF_SLIDE, MF_SOLID,
    MF_SPAWNCEILING, MF_SPECIAL, MF_TELEPORT, MF_TRANSLATION, MF_TRANSSHIFT, MOBJ_INFO, MobjInfo,
    MobjType,
};
pub use sounds::{
    MUSIC, MUSIC_COUNT, MusicId, MusicInfo, NORM_PITCH, NORM_PRIORITY, S_ATTENUATOR,
    S_CLIPPING_DIST, S_CLOSE_DIST, S_MAX_VOLUME, S_PITCH_PERTURB, SFX, SFX_COUNT, SfxId, SfxInfo,
};
pub use sprites::{SPRITE_COUNT, SPRITE_NAMES, Sprite};
pub use states::{FF_FULLBRIGHT, STATES, State, StateAction, StateId};
pub use switches::{BUTTONTIME, MAXBUTTONS, MAXSWITCHES, SWITCH_LIST, SwitchListEntry};
