//! Movement and collision, ported from `p_map.c`, `p_maputl.c` and the
//! movement half of `p_mobj.c` (`P_XYMovement`/`P_ZMovement`).
//!
//! The world model is the engine's: a [`map::Map`] for
//! geometry and blockmap, an [`MobjStore`](crate::mobj::MobjStore) for the
//! actors. Everything the original kept in file-scope globals becomes an
//! explicit [`crate::movement::MoveContext`] parameter, so re-entrant calls cannot leak
//! state into each other:
//!
//! | vanilla global      | where it lives now                     |
//! |---------------------|----------------------------------------|
//! | `tmthing`/`tmflags`/`tmx`/`tmy`/`tmbbox` | locals of the check, carried in [`crate::movement::MoveReport`] |
//! | `tmfloorz`/`tmceilingz`/`tmdropoffz`    | [`crate::movement::MoveReport`] fields                |
//! | `floatok`           | [`crate::movement::MoveReport::floatok`]                |
//! | `ceilingline`       | [`crate::movement::MoveReport::ceiling_line`] (linedef index) |
//! | `spechit`/`numspechit` | [`crate::movement::MoveReport::specials`] (Vec, no 8-entry cap) |
//! | `validcount`        | a per-call `HashSet<usize>` of seen linedefs |
//! | `intercepts[]`      | a growing `Vec<Intercept>`; no `MAXINTERCEPTS` UB |
//! | `trace`/`earlyout`/`ptflags` | parameters of [`crate::movement::traverse_path`]       |
//!
//! Two deliberate calls, same spirit as elsewhere in the engine:
//!
//! - `P_DamageMobj` / `P_TouchSpecialThing` live in `p_inter.c` and are not
//!   ported yet. The collision code never calls out into game logic directly;
//!   instead it pushes [`crate::movement::MoveEvent`]s onto the report and the gameplay layer
//!   (damage, pickups, line specials, sounds) dispatches them. The RNG is
//!   still consumed exactly where vanilla consumed it, so demo replay stays
//!   bit-identical once the dispatcher lands.
//! - The `validcount` line dedup is a fresh `HashSet` per check instead of a
//!   stamp on the linedef (the `Map` is immutable). Same effect, no hidden
//!   mutation of map data.
//!
//! Vanilla `P_CheckPosition` and friends clip against *every* mobj, other
//! players included; the only exclusions are self (`thing == tmthing`),
//! things without `MF_SOLID|MF_SPECIAL|MF_SHOOTABLE` and the
//! over/under pair for missiles. That is kept as-is — do not add a
//! "players don't collide with players" carve-out.
//!
//! One note on `MF_SPECIAL` things: vanilla lets an `MF_SPECIAL` mobj be
//! *overlapped* when the mover has `MF_PICKUP` (it consumes the thing via
//! `P_TouchSpecialThing`). There is no "items don't collide with items" rule
//! in the original — item-vs-item collisions never happen because items are
//! never moved; the only consumers are `MF_PICKUP` movers. The code does
//! exactly what vanilla does, no invented exclusions.
//!
//! `MF_SHOOTABLE` is not re-exported by `info::mod` yet; its value comes
//! straight from `info::mobjinfo` and is duplicated here until the
//! re-export lands.

pub mod check;
pub mod intercepts;
pub mod momentum;
pub mod slide;
#[cfg(test)]
mod testutil;

use crate::fixed::FRACUNIT;
use crate::fixed::Fixed;
use crate::info::SfxId;
use crate::map::Map;
use crate::mobj::MobjStore;
use crate::player::Player;
use crate::rng::DoomRng;
use crate::thinker::ThinkerId;

pub use check::{MAX_STEP_UP, check_position, teleport_move, try_move};
pub use intercepts::{
    Intercept, InterceptPayload, LineOpening, PT_ADDLINES, PT_ADDTHINGS, PT_EARLYOUT, line_opening,
    traverse_path,
};
pub use momentum::{explode_missile, xy_movement, z_movement};
pub use slide::slide_move;

/// `MF_SHOOTABLE`, duplicated from `info::mobjinfo` (0x0000_0004). Drop
/// once `info::mod` re-exports it.
pub const MF_SHOOTABLE: u32 = 0x0000_0004;

/// Vanilla `MAXRADIUS`: mobjs are grouped into mapblocks by their origin,
/// and can overlap into adjacent blocks by up to this much.
pub const MAXRADIUS: Fixed = Fixed::from_raw(32 * FRACUNIT);
/// Vanilla `GRAVITY`.
pub const GRAVITY: Fixed = Fixed::from_raw(FRACUNIT);
/// Vanilla `FLOATSPEED` (p_local.h).
pub const FLOATSPEED: Fixed = Fixed::from_raw(4 * FRACUNIT);
/// Vanilla `MAXMOVE`: momentum is clamped to this per axis.
pub const MAXMOVE: Fixed = Fixed::from_raw(30 * FRACUNIT);
/// Vanilla `STOPSPEED`: below this momentum and with no walk buttons held,
/// things stop dead instead of sliding to a halt forever.
pub const STOPSPEED: Fixed = Fixed::from_raw(0x1000);
/// Vanilla `FRICTION`: ground momentum multiplied by this per tic.
pub const FRICTION: Fixed = Fixed::from_raw(0xe800);

/// A side effect the move layer cannot perform itself. The gameplay layer
/// drains these (in order) after the action that produced them and applies
/// them. RNG consumption in the producers matches vanilla call for call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MoveEvent {
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
    /// Vanilla `P_TouchSpecialThing(special, toucher)`.
    Pickup {
        /// The special (pickup) being touched.
        special: ThinkerId,
        /// The mobj that walked over it.
        toucher: ThinkerId,
    },
    /// Vanilla `P_CrossSpecialLine(line, oldside, thing)`: a special line
    /// was crossed. `side` is the side the mobj was on *before* the move.
    CrossSpecial {
        /// Index into [`Map::linedefs`].
        line: usize,
        /// Side before the move, vanilla's `oldside`.
        side: i32,
        /// The mobj that crossed.
        thing: ThinkerId,
    },
    /// A sound vanilla would play from this code path.
    Sound {
        /// The mobj to play it at.
        actor: ThinkerId,
        /// The effect.
        sfx: SfxId,
    },
}

/// Outcome of a [`check_position`] / [`try_move`] / [`teleport_move`] call.
///
/// Vanilla communicated everything through the `tm*` globals; here it all
/// comes back in one value. `ok` is vanilla's boolean return.
#[derive(Clone, Debug)]
pub struct MoveReport {
    /// False when the position is blocked (solid line or thing), vanilla's
    /// `return false`.
    pub ok: bool,
    /// Vanilla `floatok`: the move would fit between `floorz` and
    /// `ceilingz`. Only meaningful after [`try_move`].
    pub floatok: bool,
    /// Closest floor below the contact, vanilla `tmfloorz`.
    pub floorz: Fixed,
    /// Closest ceiling above the contact, vanilla `tmceilingz`.
    pub ceilingz: Fixed,
    /// Lowest contacted floor, vanilla `tmdropoffz`. Monsters refuse to walk
    /// over a dropoff taller than 24 units.
    pub dropoffz: Fixed,
    /// Vanilla `ceilingline`: the line whose ceiling clipped the move the
    /// most, as a linedef index. Needed by the "don't explode missiles
    /// against a sky" hack.
    pub ceiling_line: Option<usize>,
    /// Vanilla `spechit`: special lines contacted, in contact order.
    /// [`try_move`] consumes these in reverse order, like vanilla's
    /// `while (numspechit--)`.
    pub specials: Vec<usize>,
    /// Subsector containing the checked point, if it (and its sector) could
    /// be resolved. Informational; the `floorz`/`ceilingz` fields already
    /// carry the useful part.
    pub subsector: Option<usize>,
    /// Side effects for the gameplay layer, in production order.
    pub events: Vec<MoveEvent>,
}

impl MoveReport {
    /// A report for "the call could not even start" (vanished mobj, dead
    /// world). Blocks movement, carries no information beyond that.
    pub(crate) fn failed() -> Self {
        MoveReport {
            ok: false,
            floatok: false,
            floorz: Fixed::ZERO,
            ceilingz: Fixed::ZERO,
            dropoffz: Fixed::ZERO,
            ceiling_line: None,
            specials: Vec::new(),
            subsector: None,
            events: Vec::new(),
        }
    }
}

/// Outcome of a momentum tick ([`xy_movement`] / [`z_movement`] /
/// [`slide_move`] / [`explode_missile`]). The world changes themselves are
/// already applied to the store; this carries what the gameplay layer must
/// do about them.
#[derive(Clone, Debug, Default)]
pub struct MovementReport {
    /// Side effects for the gameplay layer, in production order.
    pub events: Vec<MoveEvent>,
    /// State-chain actions (`A_Explode` and friends) produced by state
    /// transitions taken inside the movement code. The gameplay layer
    /// dispatches them through the usual `A_*` dispatcher.
    pub actions: Vec<crate::info::StateAction>,
}

impl MovementReport {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn extend(&mut self, events: Vec<MoveEvent>) {
        self.events.extend(events);
    }

    pub(crate) fn extend_actions(&mut self, actions: Vec<crate::info::StateAction>) {
        self.actions.extend(actions);
    }
}

/// The moving thing plus everything the movement code is allowed to touch.
///
/// Vanilla read globals (`players`, the blockmap grid globals, `skyflatnum`,
/// the RNG); here every one of those is a field the caller fills in. Build
/// one per world pass and reuse it across mobjs; it is cheap to construct.
#[derive(Debug)]
pub struct MoveContext<'a> {
    /// The map being played.
    pub map: &'a Map,
    /// The actor store; owns thinkers and the runtime thing lists.
    pub store: &'a mut MobjStore,
    /// The players array (vanilla `players`, `MAXPLAYERS` entries). Entries
    /// are looked up through `mobj.player`; out-of-range indices just skip
    /// player-specific effects instead of reading garbage.
    pub players: &'a mut [Player],
    /// The deterministic RNG. Consumed call-for-call where vanilla consumed
    /// `P_Random()`.
    pub rng: &'a mut DoomRng,
    /// Vanilla `leveltime`, needed to drive `P_SetMobjState`.
    pub leveltime: i32,
    /// The ceiling flat name the "missiles don't explode against the sky"
    /// hack compares against. Vanilla compared a flat *index* to
    /// `skyflatnum`; sectors here store raw name bytes, so the comparison
    /// happens on bytes. Defaults to `F_SKY1`.
    pub sky_flat: [u8; 8],
}

impl<'a> MoveContext<'a> {
    /// A context with the vanilla sky flat name.
    pub fn new(
        map: &'a Map,
        store: &'a mut MobjStore,
        players: &'a mut [Player],
        rng: &'a mut DoomRng,
    ) -> Self {
        MoveContext {
            map,
            store,
            players,
            rng,
            leveltime: 0,
            sky_flat: *b"F_SKY1\0\0",
        }
    }
}
