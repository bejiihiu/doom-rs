//! Game data tables, ported from `info.c`, `info.h`, `sounds.c`/`sounds.h`
//! and `s_sound.c`. Everything here is plain static data, laid out exactly
//! like the vanilla tables so the mapping back to the C source stays 1:1.
//!
//! - [`sprites`] - `spritenum_t` and `sprnames`.
//! - [`states`] - `statenum_t`, the full `states[]` table (frame/tics/action
//!   per thing state) and [`states::FF_FULLBRIGHT`]. Action bodies are not
//!   here; `StateAction` only records which vanilla `A_*` function each
//!   state carries - the gameplay modules wire those up.
//! - [`sounds`] - `sfxenum_t`/`musicenum_t`, the `S_sfx[]`/`S_music[]`
//!   tables and the mixing constants from `s_sound.c`.

mod sounds;
mod sprites;
mod states;

pub use sounds::{
    MUSIC, MUSIC_COUNT, MusicId, MusicInfo, NORM_PITCH, NORM_PRIORITY, S_ATTENUATOR,
    S_CLIPPING_DIST, S_CLOSE_DIST, S_MAX_VOLUME, S_PITCH_PERTURB, SFX, SFX_COUNT, SfxId, SfxInfo,
};
pub use sprites::{SPRITE_COUNT, SPRITE_NAMES, Sprite};
pub use states::{FF_FULLBRIGHT, STATES, State, StateAction, StateId};
