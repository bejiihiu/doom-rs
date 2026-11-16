//! Monster AI: the state-action callbacks the vanilla engine hangs off
//! `states[]`, plus the two pieces of the actor brain that never were
//! actions per se — `P_CheckSight` (p_sight.c) and the noise-alert flood
//! (p_enemy.c, used for the sector `soundtarget`).
//!
//! Scope of this module, one-to-one with the original:
//!
//! | vanilla source      | where it lives                              |
//! |---------------------|---------------------------------------------|
//! | `P_CheckSight` / `P_DivlineSide` / `P_CrossSubsector` / `P_CrossBSPNode` / `P_InterceptVector2` | [`crate::enemy::sight`] |
//! | `P_NoiseAlert` / `P_RecursiveSound` | [`crate::enemy::flood`] |
//! | `P_LookForPlayers` / `P_Move` / `P_TryWalk` / `P_NewChaseDir` / `P_CheckMeleeRange` / `P_CheckMissileRange` + the `DI_*` tables | [`crate::enemy::chase`] |
//! | `P_AimLineAttack` / `P_LineAttack` / `PTR_AimTraverse` / `PTR_ShootTraverse` / `P_SpawnPuff` / `P_SpawnBlood` | [`crate::enemy::hitscan`] |
//! | `P_SpawnMissile` / `P_CheckMissileSpawn` | [`crate::enemy::missile`] |
//! | `P_RadiusAttack` / `PIT_RadiusAttack` | [`crate::enemy::radius`] |
//! | `A_*` monster callbacks (`A_Look`, `A_Chase`, ...) + `EnemyContext` + the state dispatcher | [`crate::enemy::action`] |
//!
//! The `A_*` names are kept lowercase (`a_look`, `a_chase`, ...) so they
//! stay greppable against the original source without tripping the
//! non-camel-case noise.
//!
//! ## The `P_SetMobjState` interplay
//!
//! The store reports the `A_*` actions a state chain would fire, in order,
//! but does not run them (see [`crate::mobj`] module docs). This module
//! runs them: [`crate::enemy::action::set_mobj_state_and_run_actions`] wraps
//! [`crate::mobj::MobjStore::set_mobj_state`] and dispatches every reported
//! action through [`crate::enemy::action::dispatch_action`], recursing through nested
//! state changes the way vanilla recursion does. Handlers that find their
//! actor gone (removed earlier in the same chain or by an earlier action in
//! the same dispatch round) simply bail out instead of dereferencing a
//! stale handle.
//!
//! One honest divergence: the store applies a whole zero-tic state chain
//! atomically, so an action that reads `actor->state` mid-chain observes
//! the *final* state of the chain rather than the intermediate one. No
//! monster action reads `actor->state` (A_BrainSpit reads a *fresh
//! projectile's* `state->tics`, which is stable by then), so this is
//! behavior-neutral in practice.
//!
//! ## Vanilla globals
//!
//! `p_enemy.c` and `p_sight.c` lean on file-scope globals. They become
//! explicit state here:
//!
//! | vanilla global              | where it lives now                    |
//! |-----------------------------|---------------------------------------|
//! | `braintargets` / `numbraintargets` / `braintargeton` / static `easy` | [`crate::enemy::BrainState`] |
//! | sector `soundtarget` / `validcount` / `soundtraversed` | [`crate::enemy::flood::SoundFlood`] |
//! | `xspeed[]` / `yspeed[]` / `oppposite[]` / `diags[]` | [`crate::enemy::chase`] module tables |
//! | `linetarget` / `shootthing` / `shootz` / `la_damage` / `attackrange` / `aimslope` / `topslope` / `bottomslope` | locals of [`crate::enemy::hitscan`] |
//! | `viletryx` / `viletryy` / `corpsehit` / `vileobj` | locals of `a_vile_chase` |
//! | `sightzstart` / `topslope` / `bottomslope` / `strace` / `t2x` / `t2y` / `validcount` | locals of [`crate::enemy::sight::check_sight`] |
//! | `TRACEANGLE` | [`crate::enemy::action::TRACEANGLE`] |
//! | `gameskill` / `fastparm` / `netgame` / `gamemode` / `gameepisode` / `gamemap` / `playeringame[]` | [`crate::enemy::GameRules`] and the players slice |
//!
//! `playeringame[i]` has no dedicated flag on [`crate::player::Player`]; it
//! is derived as `players.get(i).map(|p| p.mo.is_some())` — a player is in
//! the game exactly while it has an avatar mobj, which is what the game
//! loop maintains.
//!
//! `leveltime` is an `i32`, matching the rest of the crate; the original's
//! `gametic` is threaded through as the same value.
//!
//! ## Sounds
//!
//! No sound is played from here. Everything the original started with
//! `S_StartSound` comes out as a [`crate::enemy::GameEvent::Sound`] (`actor: None` is the
//! vanilla `S_StartSound(NULL, ...)` full-volume boss channel), which the
//! game layer feeds to the audio system.
//!
//! ## Known gap: deathstate actions from in-flight missile explosions
//!
//! When a missile is destroyed by the *thinker* loop, the gameplay layer
//! drives the transition through [`crate::enemy::action::set_mobj_state_and_run_actions`]
//! and everything works. The one corner that doesn't: a missile destroyed
//! inside `movement::xy_movement` (wall hit / floor clamp mid-flight)
//! goes through `movement::explode_missile`, which sets the missile's
//! death state but drops the state's actions. For `S_EXPLODE1` (MT_ROCKET)
//! that action is `A_Explode` itself, so a cyberdemon rocket that hits a
//! wall mid-flight splashes nothing in this build.
//!
//! The enemy-layer spawn path ([`crate::enemy::missile::check_missile_spawn`]) explodes
//! through [`crate::enemy::missile::explode_missile_dispatch`] instead and dispatches the
//! actions correctly; the in-flight path lives in the movement layer and
//! is out of this module's reach. The one-line fix for the movement owner
//! is to forward the `SetMobjStateReport::actions` that
//! `explode_missile` discards (e.g. as a new `MoveEvent` variant), so the
//! gameplay layer can dispatch them like any other `A_*`.
//!
//! ## RNG
//!
//! [`crate::rng::DoomRng`] is consumed call-for-call where the original
//! consumed `P_Random()`, including the index-advances-before-lookup quirk
//! (owned by `DoomRng` itself). The order of calls in every ported
//! function matches the original source, which is what keeps demo replay
//! bit-identical.

pub mod action;
pub mod chase;
pub mod flood;
pub mod hitscan;
pub mod missile;
pub mod radius;
pub mod sight;
#[cfg(test)]
mod testutil;

use crate::fixed::{FRACUNIT, Fixed};
use crate::info::SfxId;
use crate::map::Map;
use crate::mobj::{MobjStore, Skill};
use crate::movement::{MoveContext, MoveEvent};
use crate::player::Player;
use crate::rng::DoomRng;
use crate::thinker::ThinkerId;

pub use action::{dispatch_action, set_mobj_state_and_run_actions};
pub use chase::{
    check_melee_range, check_missile_range, look_for_players, move_actor, new_chase_dir, try_walk,
};
pub use flood::{SoundFlood, noise_alert};
pub use hitscan::{AimOutcome, aim_line_attack, line_attack};
pub use missile::{check_missile_spawn, spawn_missile};
pub use radius::radius_attack;
pub use sight::check_sight;

/// Vanilla `MELEERANGE` (p_local.h).
pub const MELEERANGE: Fixed = Fixed::from_raw(64 * FRACUNIT);
/// Vanilla `USERANGE` (p_local.h).
pub const USERANGE: Fixed = Fixed::from_raw(64 * FRACUNIT);
/// Vanilla `MISSILERANGE` (p_local.h).
pub const MISSILERANGE: Fixed = Fixed::from_raw(32 * 64 * FRACUNIT);

/// Side effects the AI produces that the gameplay layer must apply.
///
/// Sounds go out as [`crate::enemy::GameEvent::Sound`]; damage goes out as
/// [`GameEvent::Damage`]; anything the movement layer emits internally is
/// forwarded verbatim inside [`GameEvent::Move`]. Boss/door/floor triggers
/// and the level exit are plain variants.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GameEvent {
    /// A movement-layer side effect forwarded verbatim
    /// (`P_TryMove`/`P_XYMovement`/... emitted it).
    Move(MoveEvent),
    /// Vanilla `S_StartSound(actor, sfx)`. `actor == None` is the vanilla
    /// `S_StartSound(NULL, sfx)` full-volume boss playback.
    Sound {
        /// The mobj to play it at, if any.
        actor: Option<ThinkerId>,
        /// The effect.
        sfx: SfxId,
    },
    /// Vanilla `P_DamageMobj(target, inflictor, source, damage)`.
    Damage {
        /// The mobj taking the damage.
        target: ThinkerId,
        /// What dealt it (the missile, the bruiser, ...), if any.
        inflictor: Option<ThinkerId>,
        /// Who launched it, if any.
        source: Option<ThinkerId>,
        /// Raw damage before armor.
        damage: i32,
    },
    /// Vanilla `EV_DoFloor(&junk, lowerFloorToLowest)` from a boss death.
    /// `tag` is the line tag the original built the event for (always 666
    /// in the one call site that exists).
    FloorLowerToLowest {
        /// Vanilla `line_t::tag` from the temporary line.
        tag: i32,
    },
    /// Vanilla `EV_DoFloor(&junk, raiseToTexture)` from a boss death
    /// (`tag` 667 for the map-7 baby boss).
    FloorRaiseToTexture {
        /// Vanilla `line_t::tag` from the temporary line.
        tag: i32,
    },
    /// Vanilla `EV_DoDoor(&junk, open | blazeOpen)` from a boss/keener
    /// death. `blaze` selects `blazeOpen` (vanilla DOOM II E4-style boss
    /// door) over the regular `open` (DOOM II map 32 keeners).
    DoorOpen {
        /// Vanilla `line_t::tag` from the temporary line.
        tag: i32,
        /// True for `blazeOpen`, false for `open`.
        blaze: bool,
    },
    /// Vanilla `G_ExitLevel()` (brain death, boss death with no other
    /// door/floor trigger applicable).
    ExitLevel,
    /// Vanilla `P_UseSpecialLine(thing, line, 0)` — a monster bumped a
    /// special line and the specials layer must decide what to do with it.
    UseSpecialLine {
        /// Index into [`Map::linedefs`].
        line: usize,
        /// The mobj that bumped it.
        thing: ThinkerId,
    },
    /// Vanilla `P_ShootSpecialLine(thing, line, side)` from the hitscan
    /// trace — a damaging shot crossed a special line.
    ShootSpecial {
        /// Index into [`Map::linedefs`].
        line: usize,
        /// The mobj that fired.
        thing: ThinkerId,
    },
}

/// Vanilla game-state globals the AI branches on.
///
/// `playeringame[]` is not here — see the module docs. This is everything
/// `p_enemy.c` otherwise reads off `doomstat`/`m_misc` globals.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GameRules {
    /// Vanilla `gameskill`. `sk_nightmare` changes attack cadence and
    /// zeroes `reactiontime` at spawn time.
    pub skill: Skill,
    /// Vanilla `fastparm` (`-fast`): nightmare-like attack cadence.
    pub fastparm: bool,
    /// Vanilla `netgame`: true for network games (and deathmatch). Gates
    /// the AI's periodic re-acquire-LOS behavior while chasing.
    pub netgame: bool,
    /// Vanilla `gamemode == commercial` (DOOM II). Routes boss-death
    /// triggers.
    pub commercial: bool,
    /// Vanilla `gameepisode` (shareware/registered episodes 1..=4).
    pub episode: u8,
    /// Vanilla `gamemap` (1..=9 per episode, 1..=32 for commercial).
    pub map: u8,
}

impl GameRules {
    /// A sane single-player default (`sk_medium`, registered episode 1).
    pub const fn single_player() -> Self {
        GameRules {
            skill: Skill::Medium,
            fastparm: false,
            netgame: false,
            commercial: false,
            episode: 1,
            map: 1,
        }
    }
}

impl Default for GameRules {
    fn default() -> Self {
        Self::single_player()
    }
}

/// The vanilla `braintargets[]`/`numbraintargets`/`braintargeton` and the
/// static `easy` toggle from `A_BrainSpit`, bundled into one value the game
/// layer owns. Persists across the whole level; `a_brain_awake` refills it
/// (capped at the vanilla 32 slots; excess targets are dropped with a
/// warning instead of overrunning the original's fixed array).
#[derive(Debug)]
pub struct BrainState {
    targets: Vec<ThinkerId>,
    target_on: usize,
    /// Vanilla `static int easy` — flips on every `A_BrainSpit` call.
    easy_toggle: bool,
}

impl BrainState {
    /// An empty brain state (no targets, `easy` cleared), matching the
    /// statics' zero-initialized start.
    pub const fn new() -> Self {
        BrainState {
            targets: Vec::new(),
            target_on: 0,
            easy_toggle: false,
        }
    }

    /// The number of `MT_BOSSTARGET`s found by the last `A_BrainAwake`.
    pub fn targets_len(&self) -> usize {
        self.targets.len()
    }

    /// Which target index is up next (vanilla `braintargeton`).
    pub fn target_on(&self) -> usize {
        self.target_on
    }

    /// Replace the target list, keeping `braintargeton` in range (the
    /// original kept the stale value, which could read past the new list).
    pub(crate) fn set_targets(&mut self, targets: Vec<ThinkerId>) {
        self.targets = targets;
        if self.target_on >= self.targets.len() {
            self.target_on = 0;
        }
    }

    /// The target the next `A_BrainSpit` would shoot, and the state after
    /// advancing the ring (vanilla `braintargeton = (braintargeton+1) %
    /// numbraintargets`). Returns `None` when no targets are known
    /// (vanilla divided by zero; we skip the shot).
    pub(crate) fn next_target(&mut self) -> Option<ThinkerId> {
        if self.targets.is_empty() {
            return None;
        }
        let target = self.targets[self.target_on];
        self.target_on = (self.target_on + 1) % self.targets.len();
        Some(target)
    }

    /// Flip and return the `easy` toggle (vanilla `easy ^= 1`).
    pub(crate) fn flip_easy(&mut self) -> bool {
        self.easy_toggle = !self.easy_toggle;
        self.easy_toggle
    }
}

impl Default for BrainState {
    fn default() -> Self {
        Self::new()
    }
}

/// Everything the AI actions are allowed to touch.
///
/// One of these is built per dispatch (or per action) by the gameplay
/// layer. Fields not present in the original's globals (`flood`, `brain`,
/// `rules`) hold exactly those globals; everything else is borrowed, not
/// owned, so re-entrant calls can't leak state.
#[derive(Debug)]
pub struct EnemyContext<'a> {
    /// The map being played.
    pub map: &'a Map,
    /// The actor store; owns thinkers and the runtime thing lists.
    pub store: &'a mut MobjStore,
    /// The players array (vanilla `players`). Entries without an avatar
    /// mobj count as not-in-game (see the module docs).
    pub players: &'a mut Vec<Player>,
    /// The deterministic RNG. Consumed call-for-call where vanilla
    /// consumed `P_Random()`.
    pub rng: &'a mut DoomRng,
    /// Vanilla `leveltime` (the tic counter for this level).
    pub leveltime: i32,
    /// The ceiling flat name the "missiles don't explode against the sky"
    /// hack compares against. Defaults to `F_SKY1`.
    pub sky_flat: [u8; 8],
    /// Side effects for the gameplay layer, in production order.
    pub events: &'a mut Vec<GameEvent>,
    /// Vanilla game-state globals (skill, fastparm, ...).
    pub rules: GameRules,
    /// The sector `soundtarget` flood state (vanilla `P_NoiseAlert`).
    pub flood: &'a mut SoundFlood,
    /// The brain-target ring (vanilla `braintargets` and friends).
    pub brain: &'a mut BrainState,
}

impl<'a> EnemyContext<'a> {
    /// A context that reports into the caller's event vector.
    pub fn new(
        map: &'a Map,
        store: &'a mut MobjStore,
        players: &'a mut Vec<Player>,
        rng: &'a mut DoomRng,
        flood: &'a mut SoundFlood,
        brain: &'a mut BrainState,
        events: &'a mut Vec<GameEvent>,
    ) -> Self {
        EnemyContext {
            map,
            store,
            players,
            rng,
            leveltime: 0,
            sky_flat: *b"F_SKY1\0\0",
            events,
            rules: GameRules::default(),
            flood,
            brain,
        }
    }

    /// A short-lived movement context for the collide/clip calls. Shares
    /// the world borrow; the caller's context is unusable while the
    /// movement borrow is alive.
    pub(crate) fn move_context(&mut self) -> MoveContext<'_> {
        MoveContext {
            map: self.map,
            store: self.store,
            players: self.players.as_mut_slice(),
            rng: self.rng,
            leveltime: self.leveltime,
            sky_flat: self.sky_flat,
        }
    }

    /// An index into `players` of an in-game player, or `None`.
    /// Vanilla's `playeringame[i]`, derived from the avatar mobj.
    pub(crate) fn player_in_game(&self, index: usize) -> bool {
        self.players.get(index).is_some_and(|p| p.mo.is_some())
    }
}
