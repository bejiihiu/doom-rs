//! Interactions: the `p_inter.c` half of the gameplay layer.
//!
//! This module owns what happens when things collide with things: damage
//! ([`damage_mobj`]), death ([`kill_mobj`]) and the pickup pass
//! ([`touch_special_thing`]). The tables that drive it (`maxammo` /
//! `clipammo` / `BONUSADD` in the original) live here too.
//!
//! | vanilla (`p_inter.c`)        | where it lives                        |
//! |------------------------------|---------------------------------------|
//! | `P_GiveAmmo` / `P_GiveWeapon` / `P_GiveBody` / `P_GiveArmor` / `P_GiveCard` / `P_GivePower` | [`items`] |
//! | `P_TouchSpecialThing`        | [`items`]                             |
//! | `P_DamageMobj`               | [`damage`]                            |
//! | `P_KillMobj`                 | [`damage`]                            |
//!
//! `d_items.c` only ships `weaponinfo[]`, the table that the weapon state
//! machines drive; it is already ported as [`crate::weapons::WEAPON_INFO`]
//! and is reused here for the weapon-pickup ammo amounts.
//!
//! ## Vanilla globals
//!
//! `p_inter.c` leans on file-scope globals (`gameskill`, `netgame`,
//! `deathmatch`, `players[]`, `consoleplayer`, `automapactive`,
//! `playeringame`, ...). They become explicit parameters, bundled into
//! [`InterContext`]. The game loop builds one and threads it through every
//! call, the same way [`crate::enemy::EnemyContext`] does for the AI layer.
//!
//! `deathmatch` travels as [`DeathmatchMode`] (`Off`/`Duel`/`Alt`, the
//! vanilla `0`/`1`/`2`); `consoleplayer` as the player index;
//! `automapactive` as a plain bool (vanilla `AM_Stop` is an event here).
//!
//! ## Side effects
//!
//! Sounds, the weapon-drop on player death and the automap stop come out
//! as plain-data [`InterEvent`]s. The A_* callbacks the deathstate chain
//! fires (`A_Scream`, `A_BossDeath`, `A_Fall`, ...) run through the enemy
//! layer's dispatcher, whose output goes into the *shared*
//! [`enemy::GameEvent`] channel ([`InterContext::game_events`]) — that is
//! the vec the game loop drains for the whole gameplay layer.
//!
//! | vanilla                    | where it goes                       |
//! |----------------------------|-------------------------------------|
//! | `S_StartSound(NULL, sfx)`  | [`InterEvent::Sound`] (NULL actor)  |
//! | `P_DropWeapon(player)`     | [`InterEvent::DropWeapon`]          |
//! | `AM_Stop()`                | [`InterEvent::StopAutomap`]         |
//! | `P_SetMobjState` actions   | [`enemy::GameEvent`] via the dispatcher |
//!
//! `P_DropWeapon` is an event because the weapon overlay machinery lives in
//! the weapons layer; `AM_Stop` is an event because the automap state lives
//! in the automap layer. The game loop routes both.
//!
//! ## Divergences from the original
//!
//! - `I_Tactile` (the player-hit rumble) is not ported; there is no haptic
//!   backend in this tree. Everything else of the damage path is 1:1.
//! - `I_Error` on an unknown sprite in `P_SpecialThing` becomes a warning
//!   and the item stays on the ground. One bad map thing cannot abort the
//!   game, mirroring how unknown things spawning is handled elsewhere in
//!   this engine.
//! - The thrust math uses a 64-bit intermediate for the
//!   `damage * (FRACUNIT>>3) * 100` product. Vanilla computes it in 32-bit
//!   `fixed_t` arithmetic, which wraps for absurd damage values (the 10000
//!   teleport stomp); mechanical 1:1 was kept everywhere else.
//! - Players keep two health fields like vanilla does: `Player::health`
//!   (clamped at 0, mirrored by the status bar) and the avatar's mobj
//!   health (goes negative; the xdeath check reads it). Do not "fix" this.
//!
//! ## RNG
//!
//! [`crate::rng::DoomRng`] is consumed call-for-call where vanilla consumed
//! `P_Random()`. The fall-forward check in the damage path can consume one
//! byte before the pain-chance roll; call order matches the original source.

use crate::args::DeathmatchMode;
use crate::enemy::{BrainState, EnemyContext, GameEvent, GameRules, SoundFlood};
use crate::info::SfxId;
use crate::map::Map;
use crate::mobj::MobjStore;
use crate::player::Player;
use crate::rng::DoomRng;

mod damage;
mod items;
#[cfg(test)]
mod testutil;

pub use damage::{damage_mobj, kill_mobj};
pub use items::{
    give_ammo, give_armor, give_body, give_card, give_power, give_weapon, touch_special_thing,
};

/// Vanilla `BONUSADD` (p_inter.c): bonuscount added per pickup.
pub const BONUS_ADD: i32 = 6;

/// Vanilla `maxammo[NUMAMMO]` (p_inter.c): clips a fresh player starts with
/// capacity for (the game loop seeds `Player::maxammo` from this).
pub const MAX_AMMO: [i32; crate::player::NUM_AMMO] = [200, 50, 300, 50];
/// Vanilla `clipammo[NUMAMMO]` (p_inter.c): units per clip of each kind.
pub const CLIP_AMMO: [i32; crate::player::NUM_AMMO] = [10, 4, 20, 1];

/// Vanilla `INVULNTICS` (doomdef.h): 30 seconds.
pub const INVUL_TICS: i32 = 30 * 35;
/// Vanilla `INVISTICS` (doomdef.h): 60 seconds.
pub const INVIS_TICS: i32 = 60 * 35;
/// Vanilla `IRONTICS` (doomdef.h): 60 seconds.
pub const IRON_TICS: i32 = 60 * 35;
/// Vanilla `INFRATICS` (doomdef.h): 120 seconds.
pub const INFRA_TICS: i32 = 120 * 35;

/// Vanilla `BASETHRESHOLD` (p_local.h): ticks a monster chases its attacker
/// regardless of line of sight.
pub const BASE_THRESHOLD: i32 = 100;

/// Side effects the inter layer produces that the gameplay layer must apply.
///
/// Sounds go out NULL-actor (vanilla `S_StartSound(NULL, sfx)`); the weapon
/// drop and the automap stop are forwarded to the layers that own those
/// machines. The A_* output of the deathstate chain rides the shared
/// [`enemy::GameEvent`] channel instead — see [`InterContext::game_events`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InterEvent {
    /// Vanilla `S_StartSound(NULL, sfx)`: pickup sounds at full volume.
    Sound {
        /// The effect to play.
        sfx: SfxId,
    },
    /// Vanilla `P_DropWeapon(player)`: the dead player's weapon overlay
    /// machinery. The weapons layer performs the actual psprite transition.
    DropWeapon {
        /// Player index whose weapon should drop.
        player: usize,
    },
    /// Vanilla `AM_Stop()`: the dying player is the console player and the
    /// automap was up. The automap layer closes the map window.
    StopAutomap,
}

/// Everything the inter layer is allowed to touch.
///
/// Mirrors [`enemy::EnemyContext`] field for field, plus the three vanilla
/// globals the AI layer does not need: `deathmatch` (weapon placement rules),
/// `consoleplayer` (pickup sounds, automap death check) and `automapactive`.
/// One of these is built per dispatch by the gameplay layer.
#[derive(Debug)]
pub struct InterContext<'a> {
    /// The map being played (sector `special == 11` hell-damage hack).
    pub map: &'a Map,
    /// The actor store; owns thinkers and the runtime thing lists.
    pub store: &'a mut MobjStore,
    /// The players array (vanilla `players`). Entries without an avatar
    /// mobj count as not-in-game (same derivation as the AI layer).
    pub players: &'a mut Vec<Player>,
    /// The deterministic RNG. Consumed call-for-call where vanilla
    /// consumed `P_Random()`.
    pub rng: &'a mut DoomRng,
    /// Vanilla `leveltime` (the tic counter for this level).
    pub leveltime: i32,
    /// The ceiling flat name (kept for the nested enemy context).
    pub sky_flat: [u8; 8],
    /// The sector `soundtarget` flood state (vanilla `P_NoiseAlert`).
    /// Only needed to build the short-lived enemy context the deathstate
    /// dispatcher requires; never read directly here.
    pub flood: &'a mut SoundFlood,
    /// The brain-target ring, same story as [`Self::flood`].
    pub brain: &'a mut BrainState,
    /// Vanilla game-state globals (skill, netgame, commercial, ...).
    pub rules: GameRules,
    /// Effects from the A_* actions the dispatched state changes fire —
    /// shared with the enemy/weapons layers. Anything the game layer
    /// produces for the shared channel lands here.
    pub game_events: &'a mut Vec<GameEvent>,
    /// Inter-specific effects (sounds, the weapon drop, automap stop).
    pub events: &'a mut Vec<InterEvent>,
    /// Vanilla `deathmatch` (`0` = off, `1` = `-deathmatch`, `2` = `-altdeath`).
    pub deathmatch: DeathmatchMode,
    /// Vanilla `consoleplayer`: the index of the local player.
    pub consoleplayer: usize,
    /// Vanilla `automapactive`: is the map window currently up.
    pub automap_active: bool,
}

impl<'a> InterContext<'a> {
    /// A context reporting into the caller's event buffers.
    #[allow(clippy::too_many_arguments)] // the event buffers are the API
    pub fn new(
        map: &'a Map,
        store: &'a mut MobjStore,
        players: &'a mut Vec<Player>,
        rng: &'a mut DoomRng,
        flood: &'a mut SoundFlood,
        brain: &'a mut BrainState,
        game_events: &'a mut Vec<GameEvent>,
        events: &'a mut Vec<InterEvent>,
    ) -> Self {
        InterContext {
            map,
            store,
            players,
            rng,
            leveltime: 0,
            sky_flat: *b"F_SKY1\0\0",
            flood,
            brain,
            rules: GameRules::default(),
            game_events,
            events,
            deathmatch: DeathmatchMode::Off,
            consoleplayer: 0,
            automap_active: false,
        }
    }

    /// A short-lived enemy context for the state-transition dispatcher.
    /// Borrows the world for as long as it lives; the caller flushes
    /// [`GameEvent`]s from the shared buffer before building the next one.
    pub(crate) fn enemy_context(&mut self) -> EnemyContext<'_> {
        EnemyContext {
            map: self.map,
            store: &mut *self.store,
            players: &mut *self.players,
            rng: &mut *self.rng,
            leveltime: self.leveltime,
            sky_flat: self.sky_flat,
            events: &mut *self.game_events,
            rules: self.rules,
            flood: &mut *self.flood,
            brain: &mut *self.brain,
        }
    }
}
