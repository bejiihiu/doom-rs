//! Game data tables, ported from `info.c`/`info.h`. Everything here is
//! plain static data, laid out exactly like the vanilla tables so the
//! mapping back to the C source stays 1:1.
//!
//! - [`sprites`] - `spritenum_t` and `sprnames`.
//! - [`states`] - `statenum_t`, the full `states[]` table (frame/tics/action
//!   per thing state) and [`states::FF_FULLBRIGHT`]. Action bodies are not
//!   here; `StateAction` only records which vanilla `A_*` function each
//!   state carries - the gameplay modules wire those up.

mod sprites;
mod states;

pub use sprites::{SPRITE_COUNT, SPRITE_NAMES, Sprite};
pub use states::{FF_FULLBRIGHT, STATES, State, StateAction, StateId};
