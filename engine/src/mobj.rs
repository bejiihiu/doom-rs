//! Game objects: the vanilla `mobj_t` (`p_mobj.h`) and the half of
//! `p_mobj.c` that spawns and tracks them.
//!
//! A mobj is an actor: monsters, projectiles, items, the player's body.
//! It is also a *thinker* (see [`crate::thinker`]), and the registry owns
//! the storage. `MobjStore` is the container that ties a map, the thinker
//! list and the runtime side tables (sector/blockmap thing lists, item
//! respawn queue) together; gameplay drives it, the game loop wires it up.
//!
//! What is **not** here on purpose: the per-tic behavior (`P_MobjThinker`
//! and the movement/combat code, which need `P_XYMovement`/`P_ZMovement`)
//! and `P_KillMobj` (its interaction with the intermission stats and the
//! nightmare-respawn machinery is the combat pass). The state machine
//! itself ([`MobjStore::set_mobj_state`]) is ported, including the
//! zero-tic chain and the `S_NULL` teardown.
//!
//! ## Where vanilla kept things elsewhere
//!
//! - The linked lists (`thinglist` per sector, `blocklinks[]`) lived in
//!   globals; here they are indexed vectors inside `MobjStore`, with
//!   `Option<ThinkerId>` links on the mobj itself. No raw pointers, the
//!   indices are validated.
//! - Sounds (`S_StartSound`/`S_StopSound`) are reported as data on the
//!   spawn/despawn results instead of being emitted; the audio pass wires
//!   them up.
//! - Vanilla called state action functions inline inside
//!   `P_SetMobjState`. The `A_*` bodies do not exist yet, so the
//!   transition reports them as an ordered list of
//!   [`crate::info::StateAction`] values; the gameplay pass dispatches
//!   them. All vanilla actions that could re-enter `P_SetMobjState` on
//!   the *same* mobj do so on nested calls (e.g. via `P_DamageMobj`), so
//!   deferring whole-chain reports changes nothing observable here.
//! - Vanilla indices into `mobjinfo[]` with an unbounded `int`; here
//!   [`MobjType`] is a `#[repr(u16)]` enum and the doomednum → type lookup
//!   is bounds-checked.
//! - Vanilla `I_Error`-aborts on an unknown thing type. We log a warning
//!   and skip the thing instead, so one bad THINGS lump cannot take the
//!   process down; the skip is reported in the return value.
//! - Vanilla's item respawn ring (`itemrespawnque`) is a `VecDeque` with
//!   the same 128-entry capacity and overwrite-oldest behavior.

use std::collections::VecDeque;
use std::fmt;

use crate::args::DeathmatchMode;
use crate::bsp;
use crate::fixed::Fixed;
use crate::info::{
    FF_FULLBRIGHT, MF_AMBUSH, MF_COUNTITEM, MF_COUNTKILL, MF_DROPPED, MF_NOBLOCKMAP, MF_NOSECTOR,
    MF_NOTDMATCH, MF_SPAWNCEILING, MF_SPECIAL, MOBJ_INFO, MobjInfo, MobjType, STATES, SfxId,
    Sprite, StateAction, StateId,
};
use crate::map::Map;
use crate::map::types::{
    MTF_AMBUSH, MTF_EASY, MTF_HARD, MTF_NORMAL, MTF_NOTSINGLE, NF_INDEX, NF_SUBSECTOR, ThingSpawn,
};
use crate::player::MAXPLAYERS;
use crate::rng::DoomRng;
use crate::tables::Angle;
use crate::thinker::{Thinker, ThinkerId, ThinkerRegistry};

/// Vanilla `ONFLOORZ`: "stand on the thing's sector floor" spawn sentinel.
pub const ONFLOORZ: Fixed = Fixed::from_raw(i32::MIN);
/// Vanilla `ONCEILINGZ`: "hang from the thing's sector ceiling" spawn
/// sentinel.
pub const ONCEILINGZ: Fixed = Fixed::from_raw(i32::MAX);

const ITEM_RESPAWN_QUE_SIZE: usize = 128;
const DM_START_CAPACITY: usize = 10;
const ITEM_RESPAWN_WAIT_TICS: i32 = 30 * 35;

/// Where a mobj starts vertically. Vanilla passes `ONFLOORZ`/`ONCEILINGZ`
/// sentinels (or a plain z) through one `fixed_t`; this enum keeps the
/// three cases separate and makes misuse a compile error.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpawnZ {
    /// Snap to the floor of the containing sector (vanilla `ONFLOORZ`).
    Floor,
    /// Hang from the ceiling, leaving room for the mobj's height
    /// (vanilla `ONCEILINGZ`).
    Ceiling,
    /// A concrete z in fixed point.
    At(Fixed),
}

/// Vanilla `skill_t`, minus the raw enum repr games usually want.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Skill {
    Baby,
    Easy,
    Medium,
    Hard,
    Nightmare,
}

impl Skill {
    /// Maps the 1-5 command line value onto the vanilla skill levels
    /// (`-skill 1` = "I'm too young to die").
    pub const fn from_level(level: u8) -> Option<Self> {
        match level {
            1 => Some(Self::Baby),
            2 => Some(Self::Easy),
            3 => Some(Self::Medium),
            4 => Some(Self::Hard),
            5 => Some(Self::Nightmare),
            _ => None,
        }
    }

    /// The THINGS option bit that decides whether a thing is present on
    /// this skill (vanilla `1 << (gameskill - 1)`, with the `sk_baby` and
    /// `sk_nightmare` special cases folded together: both pick one of the
    /// two outer bits).
    const fn map_thing_bit(self) -> i16 {
        match self {
            Self::Baby | Self::Easy => MTF_EASY,
            Self::Medium => MTF_NORMAL,
            Self::Hard | Self::Nightmare => MTF_HARD,
        }
    }
}

/// What to spawn and where. Groups the spawn-time inputs so
/// [`MobjStore::spawn_mobj`] stays caller-friendly.
#[derive(Clone, Copy, Debug)]
pub struct SpawnSpec {
    /// Current skill level (nightmare suppresses `reactiontime`).
    pub skill: Skill,
    /// X in fixed point.
    pub x: Fixed,
    /// Y in fixed point.
    pub y: Fixed,
    /// Where to resolve z (vanilla `ONFLOORZ`/`ONCEILINGZ` sentinels).
    pub z: SpawnZ,
    /// Which `mobjinfo` entry to use.
    pub mobj_type: MobjType,
}

/// The rules around which vanilla spawned map things, passed by the game
/// layer (from the command line and the netcode state).
#[derive(Clone, Copy, Debug)]
pub struct SpawnRules {
    /// Current skill level.
    pub skill: Skill,
    /// True for network games (`-netgame` semantics in vanilla).
    pub netgame: bool,
    /// Deathmatch mode (`0` = off, `1` = normal, `2` = alt, from
    /// `-deathmatch`/`-altdeath`).
    pub deathmatch: DeathmatchMode,
    /// `-nomonsters`.
    pub no_monsters: bool,
}

/// Why a map thing was not spawned.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpawnSkip {
    /// The thing only exists in multiplayer and we are single player
    /// (vanilla checks `MTF_NOTSINGLE` when not `netgame`).
    SinglePlayerOnly,
    /// The thing's skill bits don't cover the current skill.
    SkillMismatch,
    /// Deathmatch hides this thing (vanilla `MF_NOTDMATCH`, e.g. keys).
    NotInDeathmatch,
    /// `-nomonsters` and a kill-flagged thing (or a skull).
    MonstersDisabled,
    /// No `mobjinfo` entry for this doomednum (vanilla `I_Error`; we warn
    /// and skip). The payload is the raw doomednum from THINGS.
    UnknownDoomedNum(i16),
}

/// The outcome of [`MobjStore::spawn_map_thing`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpawnOutcome {
    /// DoomEd type 11: a deathmatch start was collected.
    /// `slot` is its index into the deathmatch start table, or `None` if
    /// the table (10 slots) was already full - vanilla drops it silently,
    /// we report it.
    DeathmatchStart {
        /// Index into the deathmatch start list.
        slot: Option<usize>,
    },
    /// DoomEd type 1..=4: a player start was recorded.
    ///
    /// The avatar mobj is *not* spawned here: that path needs psprite
    /// setup, the status bar and the HUD. The game layer spawns players
    /// when it starts a level (and in deathmatch never does, matching
    /// vanilla).
    PlayerStart {
        /// Player index (`thing.thing_type - 1`).
        player: usize,
    },
    /// A mobj was spawned; the game layer should bump the kill/item
    /// totals from `counts_kill`/`counts_item`.
    Spawned {
        /// Handle of the new mobj.
        id: ThinkerId,
        /// `MF_COUNTKILL`: counts towards `totalkills`.
        counts_kill: bool,
        /// `MF_COUNTITEM`: counts towards `totalitems`.
        counts_item: bool,
    },
    /// The thing was filtered out before spawning.
    Skipped(SpawnSkip),
}

/// Result of [`MobjStore::remove_mobj`].
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct RemoveMobjReport {
    /// A special item (`MF_SPECIAL`, not dropped by a monster, not an
    /// invisibility sphere) was queued into the item respawn queue.
    pub respawn_queued: bool,
}

/// Result of [`MobjStore::set_mobj_state`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SetMobjStateReport {
    /// False when the state chain hit `S_NULL` and the mobj removed
    /// itself; the handle is stale from this call on.
    pub present: bool,
    /// The `A_*` actions the transition would have invoked, in order
    /// (vanilla calls them inline). The gameplay layer dispatches them;
    /// see the module docs for why deferring is safe.
    pub actions: Vec<StateAction>,
}

/// Result of [`MobjStore::respawn_specials`]. All `None` outside
/// `-altdeath` and while waiting for the 30-second window.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct RespawnReport {
    /// The teleport fog spawned at the respawn spot (vanilla always
    /// spawns it when respawning an item).
    pub fog: Option<ThinkerId>,
    /// The returned item itself. `None` when the queued doomednum had no
    /// matching `mobjinfo` entry (vanilla would index out of bounds; we
    /// warn, drop the queue entry and return just the fog).
    pub item: Option<ThinkerId>,
    /// Vanilla `S_StartSound(fog, sfx_itmbk)`; the audio layer plays it at
    /// the fog's position.
    pub sfx: Option<SfxId>,
}

/// One thing in this world.
///
/// Vanilla `mobj_t` with pointers replaced by indices/handles: identity
/// comes from the [`ThinkerId`] the store hands out, sector/blockmap list
/// links are `Option<ThinkerId>`, the player link is an index into the
/// player array. Construct mobjs through
/// [`MobjStore::spawn_mobj`]/[`MobjStore::spawn_map_thing`], not directly.
pub struct Mobj {
    /// World position of the thing's feet (vanilla `x`/`y`/`z`, fixed
    /// point).
    pub x: Fixed,
    /// See [`Mobj::x`].
    pub y: Fixed,
    /// See [`Mobj::x`].
    pub z: Fixed,

    /// Orientation, BAM (vanilla `angle`).
    pub angle: Angle,
    /// Sprite to draw (vanilla `sprite`).
    pub sprite: Sprite,
    /// Frame index with vanilla `FF_FULLBRIGHT` possibly OR'ed in
    /// (vanilla `frame`, widened to `u16`). The frame number alone is
    /// [`Mobj::frame_index`].
    pub frame: u16,

    /// Next/previous thing in this sector's linked list
    /// (vanilla `snext`/`sprev`).
    pub sector_next: Option<ThinkerId>,
    /// See [`Mobj::sector_next`].
    pub sector_prev: Option<ThinkerId>,
    /// Next/previous thing in this map block's linked list
    /// (vanilla `bnext`/`bprev`).
    pub block_next: Option<ThinkerId>,
    /// See [`Mobj::block_next`].
    pub block_prev: Option<ThinkerId>,

    /// Index of the subsector the thing is in (vanilla `subsector` index
    /// replaced by `usize`).
    pub subsector: usize,
    /// Sector the thing stands in, resolved from the subsector. `None`
    /// for things in degenerate subsectors without segs; those never
    /// enter the sector lists.
    pub sector: Option<usize>,

    /// Closest floor/ceiling under/over the thing across the contacted
    /// sectors (vanilla `floorz`/`ceilingz`).
    pub floorz: Fixed,
    /// See [`Mobj::floorz`].
    pub ceilingz: Fixed,

    /// Movement collision radius (vanilla `radius`).
    pub radius: Fixed,
    /// Movement collision height (vanilla `height`).
    pub height: Fixed,

    /// Momentum per tic (vanilla `momx`/`momy`/`momz`).
    pub momx: Fixed,
    /// See [`Mobj::momx`].
    pub momy: Fixed,
    /// See [`Mobj::momx`].
    pub momz: Fixed,

    /// Blockmap/sight-check dedup stamp (vanilla `validcount`).
    pub validcount: i32,

    /// Thing type, indexed into `mobjinfo[]` (vanilla `type`).
    pub mobj_type: MobjType,

    /// Tics remaining in the current state; `-1` runs forever
    /// (vanilla `tics`).
    pub tics: i32,
    /// Current state (vanilla `state`, a pointer into `states[]`; here
    /// the [`StateId`] ordinal).
    pub state: StateId,
    /// `MF_*` bits (vanilla `flags`).
    pub flags: u32,
    /// Hit points (vanilla `health`).
    pub health: i32,

    /// Movement direction, 0-7 (vanilla `movedir`).
    pub movedir: i32,
    /// Ticks left in the current direction (vanilla `movecount`).
    pub movecount: i32,

    /// Thing being chased/attacked, or the originator for missiles
    /// (vanilla `target`).
    pub target: Option<ThinkerId>,

    /// If non-zero, don't attack yet (vanilla `reactiontime`).
    pub reactiontime: i32,

    /// If >0, the target is chased no matter what (vanilla `threshold`).
    pub threshold: i32,

    /// Player index for `MT_PLAYER` avatars, `None` otherwise
    /// (vanilla `player`).
    pub player: Option<usize>,

    /// Player number last looked for (vanilla `lastlook`).
    pub lastlook: i32,

    /// DoomEd spawn data, used by nightmare respawn and item respawn
    /// (vanilla `spawnpoint`).
    pub spawnpoint: ThingSpawn,

    /// Thing being chased for tracer missiles (vanilla `tracer`).
    pub tracer: Option<ThinkerId>,
}

impl Mobj {
    /// Reference to the thing's `mobjinfo[]` entry (vanilla `info`).
    pub fn info(&self) -> &'static MobjInfo {
        &MOBJ_INFO[self.mobj_type as usize]
    }

    /// True if any of the `flag` bits are set (vanilla `flags & MF_*`
    /// truthiness).
    pub const fn has_flag(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }

    /// The frame number with the `FF_FULLBRIGHT` bit masked out
    /// (vanilla `frame & FF_FRAMEMASK`).
    pub const fn frame_index(&self) -> u16 {
        self.frame & !FF_FULLBRIGHT
    }

    /// True when the current frame was drawn fullbright.
    pub const fn is_fullbright(&self) -> bool {
        self.frame & FF_FULLBRIGHT != 0
    }

    /// Minimal valid mobj for tests and benchmarks elsewhere in the
    /// crate. Not a game-legal object: it has no world position, no
    /// links and no sane state.
    #[cfg(test)]
    pub(crate) fn test_fixture() -> Self {
        Mobj {
            x: Fixed::default(),
            y: Fixed::default(),
            z: Fixed::default(),
            angle: Angle::ZERO,
            sprite: Sprite::SPR_TROO,
            frame: 0,
            sector_next: None,
            sector_prev: None,
            block_next: None,
            block_prev: None,
            subsector: 0,
            sector: None,
            floorz: Fixed::default(),
            ceilingz: Fixed::default(),
            radius: Fixed::default(),
            height: Fixed::default(),
            momx: Fixed::default(),
            momy: Fixed::default(),
            momz: Fixed::default(),
            validcount: 0,
            mobj_type: MobjType::MT_PLAYER,
            tics: 0,
            state: StateId::S_NULL,
            flags: 0,
            health: 0,
            movedir: 0,
            movecount: 0,
            target: None,
            reactiontime: 0,
            threshold: 0,
            player: None,
            lastlook: 0,
            spawnpoint: ThingSpawn {
                x: 0,
                y: 0,
                angle: 0,
                thing_type: 0,
                flags: 0,
            },
            tracer: None,
        }
    }
}

impl fmt::Debug for Mobj {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mobj")
            .field("mobj_type", &self.mobj_type)
            .field("state", &self.state)
            .field("x", &self.x)
            .field("y", &self.y)
            .field("z", &self.z)
            .field("flags", &format_args!("{:#x}", self.flags))
            .field("health", &self.health)
            .field("tics", &self.tics)
            .finish()
    }
}

/// The vanilla item respawn ring (`itemrespawnque`/`itemrespawntime`).
///
/// Holds the DoomEd spawn data of special items that were taken, so
/// `-altdeath` can bring them back after 30 seconds. Bounded to 128
/// entries like vanilla; when full, the oldest entry is dropped (vanilla
/// overwrites it in place).
#[derive(Debug)]
pub struct ItemRespawnQueue {
    /// Front = oldest (vanilla `iquetail`).
    entries: VecDeque<(ThingSpawn, i32)>,
}

impl ItemRespawnQueue {
    /// An empty queue (vanilla `iquehead == iquetail == 0`).
    pub const fn new() -> Self {
        ItemRespawnQueue {
            entries: VecDeque::new(),
        }
    }

    /// Number of queued items.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The oldest queued entry, without removing it.
    pub fn oldest(&self) -> Option<&(ThingSpawn, i32)> {
        self.entries.front()
    }

    /// Drops every entry (vanilla level re-init sets head/tail to 0).
    fn reset(&mut self) {
        self.entries.clear();
    }

    fn enqueue(&mut self, thing: ThingSpawn, leveltime: i32) {
        if self.entries.len() >= ITEM_RESPAWN_QUE_SIZE {
            self.entries.pop_front();
        }
        self.entries.push_back((thing, leveltime));
    }

    fn pop_oldest(&mut self) -> Option<(ThingSpawn, i32)> {
        self.entries.pop_front()
    }
}

impl Default for ItemRespawnQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Cached snapshot of the blockmap grid geometry (vanilla reads the
/// globals `bmaporgx`/`bmaporgy`/`bmapwidth`/`bmapheight`).
#[derive(Clone, Copy, Debug)]
struct BlockMapLayout {
    x_origin: Fixed,
    y_origin: Fixed,
    columns: u16,
    rows: u16,
}

impl BlockMapLayout {
    fn from_map(map: &Map) -> Self {
        BlockMapLayout {
            x_origin: map.blockmap.x_origin,
            y_origin: map.blockmap.y_origin,
            columns: map.blockmap.columns,
            rows: map.blockmap.rows,
        }
    }

    fn cell_index(&self, x: Fixed, y: Fixed) -> Option<usize> {
        // Vanilla: (fixed - bmaporg) >> MAPBLOCKSHIFT with MAPBLOCKSHIFT = FRACBITS+7.
        let bx = (x.to_raw() as i64 - self.x_origin.to_raw() as i64) >> 23;
        let by = (y.to_raw() as i64 - self.y_origin.to_raw() as i64) >> 23;
        if bx < 0 || by < 0 || bx >= i64::from(self.columns) || by >= i64::from(self.rows) {
            return None;
        }
        Some(by as usize * usize::from(self.columns) + bx as usize)
    }
}

/// The container for all mobjs in a level: the thinker registry plus the
/// runtime side tables vanilla kept in globals (`thinglist` per sector,
/// `blocklinks[]`, the item respawn queue, player/deathmatch starts).
///
/// The game loop owns one store per level and calls
/// [`MobjStore::reset_for_level`] on map load. Spawning works without it
/// (side tables are sized lazily), but resetting is the only way to
/// guarantee no stale handles across a level switch.
#[derive(Debug)]
pub struct MobjStore {
    registry: ThinkerRegistry,
    /// Per-sector thing list heads (vanilla `sector_t.thinglist`).
    sector_heads: Vec<Option<ThinkerId>>,
    /// Per-block thing list heads (vanilla `blocklinks[]`).
    blockmap_links: Vec<Option<ThinkerId>>,
    blockmap_layout: Option<BlockMapLayout>,
    respawn_queue: ItemRespawnQueue,
    player_starts: [Option<ThingSpawn>; MAXPLAYERS],
    deathmatch_starts: Vec<ThingSpawn>,
}

impl MobjStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Re-initializes the store for a fresh level and sizes the side
    /// tables from the map (vanilla level setup: `P_InitThinkers`, the
    /// `blocklinks`/`thinglist` purge and the item respawn queue reset).
    ///
    /// All handles issued before the call are invalidated.
    pub fn reset_for_level(&mut self, map: &Map) {
        self.registry.clear();
        self.sector_heads.clear();
        self.blockmap_links.clear();
        self.sector_heads.resize(map.sectors.len(), None);
        self.blockmap_links.resize(
            usize::from(map.blockmap.columns) * usize::from(map.blockmap.rows),
            None,
        );
        self.blockmap_layout = Some(BlockMapLayout::from_map(map));
        self.respawn_queue.reset();
        self.player_starts = [None; MAXPLAYERS];
        self.deathmatch_starts.clear();
    }

    /// Number of thinkers in the list (including zombies, like vanilla's
    /// list). Mostly for tests and diagnostics.
    pub fn len(&self) -> usize {
        self.registry.len()
    }

    /// True when no thinkers are in the list.
    pub fn is_empty(&self) -> bool {
        self.registry.is_empty()
    }

    /// Iterates the thinkers in execution order.
    pub fn thinkers(&self) -> impl Iterator<Item = (ThinkerId, &Thinker)> + '_ {
        self.registry.iter()
    }

    /// Borrows a mobj by handle.
    pub fn mobj(&self, id: ThinkerId) -> Option<&Mobj> {
        match self.registry.get(id) {
            Some(Thinker::Mobj(mobj)) => Some(mobj),
            None => None,
        }
    }

    /// Mutably borrows a mobj by handle.
    pub fn mobj_mut(&mut self, id: ThinkerId) -> Option<&mut Mobj> {
        match self.registry.get_mut(id) {
            Some(Thinker::Mobj(mobj)) => Some(mobj),
            None => None,
        }
    }

    /// Handle of the first mobj in a sector's thing list, if any.
    pub fn sector_head(&self, sector: usize) -> Option<ThinkerId> {
        self.sector_heads.get(sector).copied().flatten()
    }

    /// Walks a sector's thing list (vanilla `sector->thinglist`), newest
    /// first. Used by the renderer and by tests.
    pub fn sector_members(&self, sector: usize) -> impl Iterator<Item = ThinkerId> + '_ {
        let mut next = self.sector_head(sector);
        std::iter::from_fn(move || {
            let id = next?;
            next = self.mobj(id).and_then(|m| m.sector_next);
            Some(id)
        })
    }

    /// Handle of the first mobj in a blockmap cell's thing list, if any.
    pub fn blockmap_head(&self, cell: usize) -> Option<ThinkerId> {
        self.blockmap_links.get(cell).copied().flatten()
    }

    /// Walks a blockmap cell's thing list (vanilla `blocklinks[y][x]`),
    /// newest first.
    pub fn blockmap_members(&self, cell: usize) -> impl Iterator<Item = ThinkerId> + '_ {
        let mut next = self.blockmap_head(cell);
        std::iter::from_fn(move || {
            let id = next?;
            next = self.mobj(id).and_then(|m| m.block_next);
            Some(id)
        })
    }

    /// The recorded player start for a player slot, if the map had one.
    pub fn player_start(&self, player: usize) -> Option<&ThingSpawn> {
        self.player_starts.get(player).and_then(Option::as_ref)
    }

    /// The recorded deathmatch starts (up to 10, map order).
    pub fn deathmatch_starts(&self) -> &[ThingSpawn] {
        &self.deathmatch_starts
    }

    /// The item respawn queue (for diagnostics and the altdeath pass).
    pub fn item_respawn_queue(&self) -> &ItemRespawnQueue {
        &self.respawn_queue
    }

    /// P_SpawnMobj: spawns a thing at `(x, y, z)`.
    ///
    /// `spec.z` is resolved against the containing sector. The thing is
    /// linked into its sector's and blockmap cell's thing lists, added to
    /// the thinker list (appended, like vanilla `P_AddThinker`) and
    /// returned as a handle. `spawnpoint` is zeroed; callers that spawn
    /// from the map fill it in (vanilla copies `*mthing`).
    pub fn spawn_mobj(&mut self, map: &Map, rng: &mut DoomRng, spec: SpawnSpec) -> ThinkerId {
        self.ensure_side_tables(map);

        let SpawnSpec {
            skill,
            x,
            y,
            z,
            mobj_type,
        } = spec;
        let info = &MOBJ_INFO[mobj_type as usize];
        let spawn_state = STATES[info.spawnstate as usize];

        // Vanilla P_SetThingPosition resolves the subsector before the
        // thing exists; do the same to find the floor/ceiling.
        let subsector = point_in_subsector(map, x, y);
        let (floorz, ceilingz, sector) = resolve_sector(map, subsector, "P_SpawnMobj");

        let z = match z {
            SpawnZ::Floor => floorz,
            SpawnZ::Ceiling => ceilingz - info.height,
            SpawnZ::At(z) => z,
        };

        let subsector_index = subsector.unwrap_or(0);
        let mobj = Mobj {
            x,
            y,
            z,
            angle: Angle::ZERO,
            sprite: spawn_state.sprite,
            frame: spawn_state.frame,
            sector_next: None,
            sector_prev: None,
            block_next: None,
            block_prev: None,
            subsector: subsector_index,
            sector,
            floorz,
            ceilingz,
            radius: info.radius,
            height: info.height,
            momx: Fixed::default(),
            momy: Fixed::default(),
            momz: Fixed::default(),
            validcount: 0,
            mobj_type,
            tics: i32::from(spawn_state.tics),
            state: info.spawnstate,
            flags: info.flags,
            health: info.spawnhealth,
            movedir: 0,
            movecount: 0,
            target: None,
            reactiontime: if skill == Skill::Nightmare {
                // vanilla: reactiontime is not set on nightmare
                0
            } else {
                info.reactiontime
            },
            threshold: 0,
            player: None,
            lastlook: i32::from(rng.next()) % MAXPLAYERS as i32,
            spawnpoint: ThingSpawn {
                x: 0,
                y: 0,
                angle: 0,
                thing_type: 0,
                flags: 0,
            },
            tracer: None,
        };

        let id = self.registry.add(Thinker::Mobj(mobj));
        self.set_thing_position(map, id);
        id
    }

    /// P_RemoveMobj: queues the item for altdeath respawn (if a dropped,
    /// non-sphere special), unlinks the thing from its sector and blockmap
    /// lists and flags the thinker for removal.
    ///
    /// The thinker itself is collected at its turn on the next
    /// [`MobjStore::run_thinkers`], like vanilla. `S_StopSound` has no
    /// counterpart yet; the audio pass owns that.
    pub fn remove_mobj(&mut self, id: ThinkerId, leveltime: i32) -> RemoveMobjReport {
        let respawn_queued = match self.registry.get(id) {
            Some(Thinker::Mobj(mobj)) => {
                let queued = mobj.has_flag(MF_SPECIAL)
                    && !mobj.has_flag(MF_DROPPED)
                    && mobj.mobj_type != MobjType::MT_INV
                    && mobj.mobj_type != MobjType::MT_INS;
                if queued {
                    self.respawn_queue.enqueue(mobj.spawnpoint, leveltime);
                }
                queued
            }
            None => {
                tracing::warn!("P_RemoveMobj: stale mobj handle {id:?} ignored");
                false
            }
        };

        self.unset_thing_position(id);
        self.registry.remove(id);
        RemoveMobjReport { respawn_queued }
    }

    /// P_SetMobjState: transitions a mobj to `state` and reports what
    /// happened.
    ///
    /// Ported faithfully, including the zero-tic chain: states are applied
    /// in a loop until one has a non-zero `tics` (or `S_NULL` removes the
    /// mobj). `S_NULL` flags the thinker for removal and yields
    /// `present: false`; the handle is stale afterwards.
    ///
    /// The state action functions (`A_*`) are reported in the returned
    /// vector, in the order vanilla would have called them, but are **not
    /// executed** here; see the module docs. A mobj that disappears during
    /// the chain (an action removed it) is reported by the removal itself,
    /// not by this function.
    pub fn set_mobj_state(
        &mut self,
        id: ThinkerId,
        state: StateId,
        leveltime: i32,
    ) -> SetMobjStateReport {
        let mut actions = Vec::new();
        let mut state = state;
        loop {
            if state == StateId::S_NULL {
                if let Some(Thinker::Mobj(mobj)) = self.registry.get_mut(id) {
                    mobj.state = StateId::S_NULL;
                }
                self.remove_mobj(id, leveltime);
                return SetMobjStateReport {
                    present: false,
                    actions,
                };
            }

            let st = STATES[state as usize];
            let sprite = st.sprite;
            let frame = st.frame;
            let tics = i32::from(st.tics);
            let action = st.action;
            let next = st.next;

            match self.registry.get_mut(id) {
                Some(Thinker::Mobj(mobj)) => {
                    mobj.state = state;
                    mobj.tics = tics;
                    mobj.sprite = sprite;
                    mobj.frame = frame;
                }
                // Vanilla would follow a dangling pointer into freed
                // memory; we report the mobj as gone instead.
                None => {
                    return SetMobjStateReport {
                        present: false,
                        actions,
                    };
                }
            }

            if action != StateAction::NoAction {
                actions.push(action);
            }

            state = next;
            if tics != 0 {
                break;
            }
        }
        SetMobjStateReport {
            present: true,
            actions,
        }
    }

    /// P_SpawnMapThing: spawns a thing from THINGS, applying the vanilla
    /// filters (deathmatch starts, player starts, single player things,
    /// skill bits, deathmatch MF_NOTDMATCH hiding, `-nomonsters`).
    ///
    /// Total kill/item counting is left to the caller: the outcome says
    /// whether this thing counts (`MF_COUNTKILL`/`MF_COUNTITEM`).
    pub fn spawn_map_thing(
        &mut self,
        map: &Map,
        rng: &mut DoomRng,
        rules: SpawnRules,
        thing: &ThingSpawn,
    ) -> SpawnOutcome {
        // count deathmatch starts
        if thing.thing_type == 11 {
            let slot = if self.deathmatch_starts.len() < DM_START_CAPACITY {
                self.deathmatch_starts.push(*thing);
                Some(self.deathmatch_starts.len() - 1)
            } else {
                None
            };
            return SpawnOutcome::DeathmatchStart { slot };
        }

        // players, keep spots for respawning in network games
        if (1..=4).contains(&thing.thing_type) {
            let player = (thing.thing_type - 1) as usize;
            self.player_starts[player] = Some(*thing);
            // vanilla spawns the avatar here in single player; that needs
            // psprite/status-bar setup, which is the game layer's job.
            return SpawnOutcome::PlayerStart { player };
        }

        // single player only things
        if !rules.netgame && (thing.flags & MTF_NOTSINGLE) != 0 {
            return SpawnOutcome::Skipped(SpawnSkip::SinglePlayerOnly);
        }

        // check for appropriate skill level
        let bit = rules.skill.map_thing_bit();
        if (thing.flags & bit) == 0 {
            return SpawnOutcome::Skipped(SpawnSkip::SkillMismatch);
        }

        // find which type to spawn
        let Some(mobj_type) = mobj_type_for_doomednum(i32::from(thing.thing_type)) else {
            tracing::warn!(
                "P_SpawnMapThing: unknown type {} at ({}, {})",
                thing.thing_type,
                thing.x,
                thing.y
            );
            return SpawnOutcome::Skipped(SpawnSkip::UnknownDoomedNum(thing.thing_type));
        };

        // don't spawn key cards and players in deathmatch
        if rules.deathmatch != DeathmatchMode::Off
            && MOBJ_INFO[mobj_type as usize].has_flag(MF_NOTDMATCH)
        {
            return SpawnOutcome::Skipped(SpawnSkip::NotInDeathmatch);
        }

        // don't spawn any monsters if -nomonsters
        if rules.no_monsters
            && (mobj_type == MobjType::MT_SKULL
                || MOBJ_INFO[mobj_type as usize].has_flag(MF_COUNTKILL))
        {
            return SpawnOutcome::Skipped(SpawnSkip::MonstersDisabled);
        }

        let x = Fixed::from_int(i32::from(thing.x));
        let y = Fixed::from_int(i32::from(thing.y));
        let z = if MOBJ_INFO[mobj_type as usize].has_flag(MF_SPAWNCEILING) {
            SpawnZ::Ceiling
        } else {
            SpawnZ::Floor
        };

        let id = self.spawn_mobj(
            map,
            rng,
            SpawnSpec {
                skill: rules.skill,
                x,
                y,
                z,
                mobj_type,
            },
        );

        let mut counts_kill = false;
        let mut counts_item = false;
        if let Some(mobj) = self.mobj_mut(id) {
            mobj.spawnpoint = *thing;
            if mobj.tics > 0 {
                // vanilla: 1 + P_Random() % tics; rng.next() is the same 0..255 byte
                let tics = mobj.tics;
                mobj.tics = 1 + (i32::from(rng.next()) % tics);
            }
            mobj.angle = Angle::from_raw(
                Angle::ANG45
                    .to_raw()
                    .wrapping_mul((i32::from(thing.angle) / 45) as u32),
            );
            if (thing.flags & MTF_AMBUSH) != 0 {
                mobj.flags |= MF_AMBUSH;
            }
            counts_kill = mobj.has_flag(MF_COUNTKILL);
            counts_item = mobj.has_flag(MF_COUNTITEM);
        }

        SpawnOutcome::Spawned {
            id,
            counts_kill,
            counts_item,
        }
    }

    /// P_RespawnSpecials: brings back one queued special item in
    /// `-altdeath` games, 30 seconds after it was taken.
    ///
    /// Vanilla parity notes: the fog + its sound are emitted even if the
    /// queued doomednum no longer resolves to a mobj type (we drop that
    /// item from the queue with a warning instead of reading out of
    /// bounds like vanilla). Only one item respawns per call.
    pub fn respawn_specials(
        &mut self,
        map: &Map,
        rng: &mut DoomRng,
        skill: Skill,
        deathmatch: DeathmatchMode,
        leveltime: i32,
    ) -> RespawnReport {
        if deathmatch != DeathmatchMode::Alt {
            return RespawnReport::default();
        }

        let Some(&(mthing, taken_at)) = self.respawn_queue.oldest() else {
            return RespawnReport::default();
        };

        if leveltime - taken_at < ITEM_RESPAWN_WAIT_TICS {
            return RespawnReport::default();
        }

        let x = Fixed::from_int(i32::from(mthing.x));
        let y = Fixed::from_int(i32::from(mthing.y));
        let subsector = point_in_subsector(map, x, y);
        let (floorz, _, _) = resolve_sector(map, subsector, "P_RespawnSpecials");

        // spawn a teleport fog at the new spot
        let fog = self.spawn_mobj(
            map,
            rng,
            SpawnSpec {
                skill,
                x,
                y,
                z: SpawnZ::At(floorz),
                mobj_type: MobjType::MT_IFOG,
            },
        );
        let sfx = Some(SfxId::sfx_itmbk);

        // find which type to spawn
        let Some(mobj_type) = mobj_type_for_doomednum(i32::from(mthing.thing_type)) else {
            tracing::warn!(
                "P_RespawnSpecials: queued thing type {} is not spawnable; dropping it",
                mthing.thing_type
            );
            self.respawn_queue.pop_oldest();
            return RespawnReport {
                fog: Some(fog),
                item: None,
                sfx,
            };
        };

        // spawn it
        let z = if MOBJ_INFO[mobj_type as usize].has_flag(MF_SPAWNCEILING) {
            SpawnZ::Ceiling
        } else {
            SpawnZ::Floor
        };
        let id = self.spawn_mobj(
            map,
            rng,
            SpawnSpec {
                skill,
                x,
                y,
                z,
                mobj_type,
            },
        );
        if let Some(mobj) = self.mobj_mut(id) {
            mobj.spawnpoint = mthing;
            mobj.angle = Angle::from_raw(
                Angle::ANG45
                    .to_raw()
                    .wrapping_mul((i32::from(mthing.angle) / 45) as u32),
            );
        }

        self.respawn_queue.pop_oldest();
        RespawnReport {
            fog: Some(fog),
            item: Some(id),
            sfx,
        }
    }

    /// Executes every thinker in insertion order, forwarding to the
    /// registry (see [`ThinkerRegistry::run`]).
    pub fn run_thinkers(&mut self, f: impl FnMut(&mut ThinkerRegistry, ThinkerId)) {
        self.registry.run(f);
    }

    fn ensure_side_tables(&mut self, map: &Map) {
        let sectors = map.sectors.len();
        if self.sector_heads.len() != sectors {
            self.sector_heads.clear();
            self.sector_heads.resize(sectors, None);
        }
        let cells = usize::from(map.blockmap.columns) * usize::from(map.blockmap.rows);
        if self.blockmap_links.len() != cells {
            self.blockmap_links.clear();
            self.blockmap_links.resize(cells, None);
        }
        if self.blockmap_layout.is_none() {
            self.blockmap_layout = Some(BlockMapLayout::from_map(map));
        }
    }

    /// Vanilla `P_SetThingPosition` minus the caller-discipline
    /// (unlinking first); unlinks defensively before linking.
    /// `pub(crate)` so the movement code can re-link things after a move.
    pub(crate) fn set_thing_position(&mut self, map: &Map, id: ThinkerId) {
        self.unset_thing_position(id);

        let Some(Thinker::Mobj(mobj)) = self.registry.get(id) else {
            return;
        };
        let x = mobj.x;
        let y = mobj.y;
        let no_sector = mobj.has_flag(MF_NOSECTOR);
        let no_blockmap = mobj.has_flag(MF_NOBLOCKMAP);

        let subsector = point_in_subsector(map, x, y);
        let sector = subsector
            .and_then(|i| map.subsectors.get(i))
            .and_then(|ss| ss.sector);
        if let Some(mobj) = self.mobj_mut(id) {
            mobj.subsector = subsector.unwrap_or(0);
            mobj.sector = sector;
        }

        if !no_sector {
            if let Some(sec) = sector {
                let old_head = self.sector_heads[sec];
                if let Some(mobj) = self.mobj_mut(id) {
                    mobj.sector_next = old_head;
                    mobj.sector_prev = None;
                }
                if let Some(head) = old_head {
                    if let Some(h) = self.mobj_mut(head) {
                        h.sector_prev = Some(id);
                    }
                }
                self.sector_heads[sec] = Some(id);
            }
        }

        if !no_blockmap {
            if let Some(layout) = self.blockmap_layout {
                if let Some(cell) = layout.cell_index(x, y) {
                    let old_head = self.blockmap_links[cell];
                    if let Some(mobj) = self.mobj_mut(id) {
                        mobj.block_next = old_head;
                        mobj.block_prev = None;
                    }
                    if let Some(head) = old_head {
                        if let Some(h) = self.mobj_mut(head) {
                            h.block_prev = Some(id);
                        }
                    }
                    self.blockmap_links[cell] = Some(id);
                }
                // off-map thing: vanilla clears the links in P_SetThingPosition
            }
        }
    }

    /// Vanilla `P_UnsetThingPosition`. `pub(crate)` for the movement
    /// code, same reason as [`Self::set_thing_position`].
    pub(crate) fn unset_thing_position(&mut self, id: ThinkerId) {
        let Some(Thinker::Mobj(mobj)) = self.registry.get(id) else {
            return;
        };
        let no_sector = mobj.has_flag(MF_NOSECTOR);
        let no_blockmap = mobj.has_flag(MF_NOBLOCKMAP);
        let sector = mobj.sector;
        let sector_next = mobj.sector_next;
        let sector_prev = mobj.sector_prev;
        let block_next = mobj.block_next;
        let block_prev = mobj.block_prev;
        let position = (mobj.x, mobj.y);

        if !no_sector {
            if let Some(sec) = sector {
                if let Some(next) = sector_next {
                    if let Some(m) = self.mobj_mut(next) {
                        m.sector_prev = sector_prev;
                    }
                }
                match sector_prev {
                    Some(prev) => {
                        if let Some(m) = self.mobj_mut(prev) {
                            m.sector_next = sector_next;
                        }
                    }
                    None => {
                        if sec < self.sector_heads.len() && self.sector_heads[sec] == Some(id) {
                            self.sector_heads[sec] = sector_next;
                        }
                    }
                }
            }
            if let Some(m) = self.mobj_mut(id) {
                m.sector_next = None;
                m.sector_prev = None;
            }
        }

        if !no_blockmap {
            if let Some(layout) = self.blockmap_layout {
                if let Some(cell) = layout.cell_index(position.0, position.1) {
                    if let Some(next) = block_next {
                        if let Some(m) = self.mobj_mut(next) {
                            m.block_prev = block_prev;
                        }
                    }
                    match block_prev {
                        Some(prev) => {
                            if let Some(m) = self.mobj_mut(prev) {
                                m.block_next = block_next;
                            }
                        }
                        None => {
                            if self.blockmap_links[cell] == Some(id) {
                                self.blockmap_links[cell] = block_next;
                            }
                        }
                    }
                }
            }
            if let Some(m) = self.mobj_mut(id) {
                m.block_next = None;
                m.block_prev = None;
            }
        }
    }
}

impl Default for MobjStore {
    fn default() -> Self {
        MobjStore {
            registry: ThinkerRegistry::new(),
            sector_heads: Vec::new(),
            blockmap_links: Vec::new(),
            blockmap_layout: None,
            respawn_queue: ItemRespawnQueue::new(),
            player_starts: [None; MAXPLAYERS],
            deathmatch_starts: Vec::new(),
        }
    }
}

/// Vanilla `R_PointInSubsector`: walks the BSP tree from the root and
/// returns the leaf's subsector index.
///
/// Returns `None` only when the map has no subsectors at all, or when a
/// corrupt child word points outside the tables (`Map::load` validates
/// those, so this is unreachable for maps that loaded cleanly).
pub fn point_in_subsector(map: &Map, x: Fixed, y: Fixed) -> Option<usize> {
    if map.subsectors.is_empty() {
        return None;
    }
    if map.nodes.is_empty() {
        return Some(0);
    }
    let mut nodenum = map.nodes.len() - 1;
    loop {
        let node = &map.nodes[nodenum];
        let side = bsp::point_on_node_side(x, y, node) as usize;
        let child = node.children[side & 1];
        let index = usize::from(child & NF_INDEX);
        if child & NF_SUBSECTOR != 0 {
            return (index < map.subsectors.len()).then_some(index);
        }
        if index >= map.nodes.len() {
            return None;
        }
        nodenum = index;
    }
}

/// Vanilla `R_PointInSubsector`'s caller-side sector fetch: finds the
/// subsector's sector and reports (floorz, ceilingz, sector index).
/// Degenerate subsectors without segs get a zero-height world and a
/// warning instead of a crash.
fn resolve_sector(
    map: &Map,
    subsector: Option<usize>,
    caller: &str,
) -> (Fixed, Fixed, Option<usize>) {
    match subsector
        .and_then(|i| map.subsectors.get(i))
        .and_then(|ss| ss.sector)
        .and_then(|s| map.sectors.get(s).map(|sec| (s, sec)))
    {
        Some((index, sector)) => (sector.floor_height, sector.ceiling_height, Some(index)),
        None => {
            tracing::warn!("{caller}: thing landed in a subsector with no sector; using z = 0");
            (Fixed::default(), Fixed::default(), None)
        }
    }
}

/// `mobjinfo[]` types in table order, so [`mobj_type_for_doomednum`]
/// stays free of unchecked index→enum casts. Tests assert that
/// `ALL_MOBJ_TYPES[i] as usize == i`, so the table can never drift from
/// the enum definition.
const ALL_MOBJ_TYPES: [MobjType; MobjType::COUNT] = [
    MobjType::MT_PLAYER,
    MobjType::MT_POSSESSED,
    MobjType::MT_SHOTGUY,
    MobjType::MT_VILE,
    MobjType::MT_FIRE,
    MobjType::MT_UNDEAD,
    MobjType::MT_TRACER,
    MobjType::MT_SMOKE,
    MobjType::MT_FATSO,
    MobjType::MT_FATSHOT,
    MobjType::MT_CHAINGUY,
    MobjType::MT_TROOP,
    MobjType::MT_SERGEANT,
    MobjType::MT_SHADOWS,
    MobjType::MT_HEAD,
    MobjType::MT_BRUISER,
    MobjType::MT_BRUISERSHOT,
    MobjType::MT_KNIGHT,
    MobjType::MT_SKULL,
    MobjType::MT_SPIDER,
    MobjType::MT_BABY,
    MobjType::MT_CYBORG,
    MobjType::MT_PAIN,
    MobjType::MT_WOLFSS,
    MobjType::MT_KEEN,
    MobjType::MT_BOSSBRAIN,
    MobjType::MT_BOSSSPIT,
    MobjType::MT_BOSSTARGET,
    MobjType::MT_SPAWNSHOT,
    MobjType::MT_SPAWNFIRE,
    MobjType::MT_BARREL,
    MobjType::MT_TROOPSHOT,
    MobjType::MT_HEADSHOT,
    MobjType::MT_ROCKET,
    MobjType::MT_PLASMA,
    MobjType::MT_BFG,
    MobjType::MT_ARACHPLAZ,
    MobjType::MT_PUFF,
    MobjType::MT_BLOOD,
    MobjType::MT_TFOG,
    MobjType::MT_IFOG,
    MobjType::MT_TELEPORTMAN,
    MobjType::MT_EXTRABFG,
    MobjType::MT_MISC0,
    MobjType::MT_MISC1,
    MobjType::MT_MISC2,
    MobjType::MT_MISC3,
    MobjType::MT_MISC4,
    MobjType::MT_MISC5,
    MobjType::MT_MISC6,
    MobjType::MT_MISC7,
    MobjType::MT_MISC8,
    MobjType::MT_MISC9,
    MobjType::MT_MISC10,
    MobjType::MT_MISC11,
    MobjType::MT_MISC12,
    MobjType::MT_INV,
    MobjType::MT_MISC13,
    MobjType::MT_INS,
    MobjType::MT_MISC14,
    MobjType::MT_MISC15,
    MobjType::MT_MISC16,
    MobjType::MT_MEGA,
    MobjType::MT_CLIP,
    MobjType::MT_MISC17,
    MobjType::MT_MISC18,
    MobjType::MT_MISC19,
    MobjType::MT_MISC20,
    MobjType::MT_MISC21,
    MobjType::MT_MISC22,
    MobjType::MT_MISC23,
    MobjType::MT_MISC24,
    MobjType::MT_MISC25,
    MobjType::MT_CHAINGUN,
    MobjType::MT_MISC26,
    MobjType::MT_MISC27,
    MobjType::MT_MISC28,
    MobjType::MT_SHOTGUN,
    MobjType::MT_SUPERSHOTGUN,
    MobjType::MT_MISC29,
    MobjType::MT_MISC30,
    MobjType::MT_MISC31,
    MobjType::MT_MISC32,
    MobjType::MT_MISC33,
    MobjType::MT_MISC34,
    MobjType::MT_MISC35,
    MobjType::MT_MISC36,
    MobjType::MT_MISC37,
    MobjType::MT_MISC38,
    MobjType::MT_MISC39,
    MobjType::MT_MISC40,
    MobjType::MT_MISC41,
    MobjType::MT_MISC42,
    MobjType::MT_MISC43,
    MobjType::MT_MISC44,
    MobjType::MT_MISC45,
    MobjType::MT_MISC46,
    MobjType::MT_MISC47,
    MobjType::MT_MISC48,
    MobjType::MT_MISC49,
    MobjType::MT_MISC50,
    MobjType::MT_MISC51,
    MobjType::MT_MISC52,
    MobjType::MT_MISC53,
    MobjType::MT_MISC54,
    MobjType::MT_MISC55,
    MobjType::MT_MISC56,
    MobjType::MT_MISC57,
    MobjType::MT_MISC58,
    MobjType::MT_MISC59,
    MobjType::MT_MISC60,
    MobjType::MT_MISC61,
    MobjType::MT_MISC62,
    MobjType::MT_MISC63,
    MobjType::MT_MISC64,
    MobjType::MT_MISC65,
    MobjType::MT_MISC66,
    MobjType::MT_MISC67,
    MobjType::MT_MISC68,
    MobjType::MT_MISC69,
    MobjType::MT_MISC70,
    MobjType::MT_MISC71,
    MobjType::MT_MISC72,
    MobjType::MT_MISC73,
    MobjType::MT_MISC74,
    MobjType::MT_MISC75,
    MobjType::MT_MISC76,
    MobjType::MT_MISC77,
    MobjType::MT_MISC78,
    MobjType::MT_MISC79,
    MobjType::MT_MISC80,
    MobjType::MT_MISC81,
    MobjType::MT_MISC82,
    MobjType::MT_MISC83,
    MobjType::MT_MISC84,
    MobjType::MT_MISC85,
    MobjType::MT_MISC86,
];

/// Looks up the mobj type for a DoomEd doomednum, first match wins
/// (vanilla loops the table in order). `None` when nothing spawnable
/// matches. Types with doomednum `-1` (player, missiles, effects) are
/// deliberately excluded: vanilla's `==` scan would match the player
/// type for a `-1` THINGS entry, which is worse than skipping the thing
/// with a warning.
pub fn mobj_type_for_doomednum(doomednum: i32) -> Option<MobjType> {
    ALL_MOBJ_TYPES
        .iter()
        .find(|&&t| {
            let d = MOBJ_INFO[t as usize].doomednum;
            d != -1 && d == doomednum
        })
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::info::STATES;
    use crate::map::types::{BlockMapGrid, Map, Node, Sector, SubSector};

    const TEST_FLOOR: i16 = 0;
    const TEST_CEIL: i16 = 128;

    fn block_grid() -> BlockMapGrid {
        // 8x8 cells, origin (-512, -512), every cell empty; spans -512..512
        BlockMapGrid::new(
            Fixed::from_int(-512),
            Fixed::from_int(-512),
            8,
            8,
            vec![Vec::new(); 64],
        )
    }

    fn sector(floor: i16, ceil: i16) -> Sector {
        Sector {
            floor_height: Fixed::from_int(i32::from(floor)),
            ceiling_height: Fixed::from_int(i32::from(ceil)),
            floor_pic: [0; 8],
            ceiling_pic: [0; 8],
            light_level: 192,
            special: 0,
            tag: 0,
        }
    }

    fn test_map() -> Map {
        Map {
            vertexes: Vec::new(),
            linedefs: Vec::new(),
            sidedefs: Vec::new(),
            sectors: vec![sector(TEST_FLOOR, TEST_CEIL)],
            segs: Vec::new(),
            subsectors: vec![SubSector {
                num_segs: 0,
                first_seg: 0,
                sector: Some(0),
            }],
            nodes: Vec::new(),
            blockmap: block_grid(),
            reject: Vec::new(),
            things: Vec::new(),
        }
    }

    fn test_map_two_sectors() -> Map {
        // vertical partition through the origin; front (+x) subsector 0 in
        // sector 0 (floor 0), back (-x) subsector 1 in sector 1 (floor 64)
        let mut map = test_map();
        map.sectors = vec![sector(0, 128), sector(64, 256)];
        map.subsectors = vec![
            SubSector {
                num_segs: 0,
                first_seg: 0,
                sector: Some(0),
            },
            SubSector {
                num_segs: 0,
                first_seg: 0,
                sector: Some(1),
            },
        ];
        map.nodes = vec![Node {
            x: Fixed::default(),
            y: Fixed::default(),
            dx: Fixed::default(),
            dy: Fixed::from_int(1),
            bbox: [[Fixed::default(); 4]; 2],
            children: [NF_SUBSECTOR, NF_SUBSECTOR | 1],
        }];
        map
    }

    fn thing(thing_type: i16, x: i16, y: i16, angle: i16, flags: i16) -> ThingSpawn {
        ThingSpawn {
            x,
            y,
            angle,
            thing_type,
            flags,
        }
    }

    fn all_skill_flags() -> i16 {
        MTF_EASY | MTF_NORMAL | MTF_HARD
    }

    fn rules() -> SpawnRules {
        SpawnRules {
            skill: Skill::Medium,
            netgame: false,
            deathmatch: DeathmatchMode::Off,
            no_monsters: false,
        }
    }

    #[test]
    fn spawn_possessed_at_floor_basics() {
        let map = test_map();
        let mut store = MobjStore::new();
        let mut rng = DoomRng::new();

        let id = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(10),
                y: Fixed::from_int(10),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_POSSESSED,
            },
        );

        let info = &MOBJ_INFO[MobjType::MT_POSSESSED as usize];
        let spawn_state = &STATES[info.spawnstate as usize];
        let mobj = store.mobj(id).expect("fresh mobj");
        assert_eq!(mobj.mobj_type, MobjType::MT_POSSESSED);
        assert_eq!(mobj.state, info.spawnstate);
        assert_eq!(mobj.tics, i32::from(spawn_state.tics));
        assert_eq!(mobj.sprite, spawn_state.sprite);
        assert_eq!(mobj.frame, spawn_state.frame);
        assert_eq!(mobj.health, info.spawnhealth);
        assert_eq!(mobj.flags, info.flags);
        assert_eq!(mobj.radius, info.radius);
        assert_eq!(mobj.height, info.height);
        assert_eq!(mobj.floorz, Fixed::from_int(0));
        assert_eq!(mobj.ceilingz, Fixed::from_int(128));
        assert_eq!(mobj.z, Fixed::from_int(0));
        assert_eq!(mobj.sector, Some(0));
        assert_eq!(mobj.subsector, 0);
        assert!((0..MAXPLAYERS as i32).contains(&mobj.lastlook));
        assert!(store.thinkers().any(|(tid, _)| tid == id));
    }

    #[test]
    fn spawn_ceiling_hangs_from_ceiling() {
        let map = test_map();
        let mut store = MobjStore::new();
        let mut rng = DoomRng::new();
        let id = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(5),
                y: Fixed::from_int(5),
                z: SpawnZ::Ceiling,
                mobj_type: MobjType::MT_POSSESSED,
            },
        );
        let info = &MOBJ_INFO[MobjType::MT_POSSESSED as usize];
        let mobj = store.mobj(id).unwrap();
        assert_eq!(mobj.z, Fixed::from_int(128) - info.height);
    }

    #[test]
    fn spawn_at_concrete_z() {
        let map = test_map();
        let mut store = MobjStore::new();
        let mut rng = DoomRng::new();
        let id = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Easy,
                x: Fixed::from_int(0),
                y: Fixed::from_int(0),
                z: SpawnZ::At(Fixed::from_int(7)),
                mobj_type: MobjType::MT_PUFF,
            },
        );
        assert_eq!(store.mobj(id).unwrap().z, Fixed::from_int(7));
    }

    #[test]
    fn spawn_picks_the_sector_of_the_containing_subsector() {
        let map = test_map_two_sectors();
        let mut store = MobjStore::new();
        let mut rng = DoomRng::new();

        let right = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(1),
                y: Fixed::from_int(0),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_POSSESSED,
            },
        );
        let left = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(-1),
                y: Fixed::from_int(0),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_POSSESSED,
            },
        );

        let right = store.mobj(right).unwrap();
        assert_eq!(right.sector, Some(0));
        assert_eq!(right.floorz, Fixed::from_int(0));
        assert_eq!(right.z, Fixed::from_int(0));

        let left = store.mobj(left).unwrap();
        assert_eq!(left.sector, Some(1));
        assert_eq!(left.floorz, Fixed::from_int(64));
        assert_eq!(left.ceilingz, Fixed::from_int(256));
        assert_eq!(left.z, Fixed::from_int(64));
    }

    #[test]
    fn point_in_subsector_walks_the_nodes() {
        let map = test_map_two_sectors();
        assert_eq!(
            point_in_subsector(&map, Fixed::from_int(1), Fixed::from_int(0)),
            Some(0)
        );
        assert_eq!(
            point_in_subsector(&map, Fixed::from_int(-1), Fixed::from_int(0)),
            Some(1)
        );
        assert_eq!(
            point_in_subsector(&map, Fixed::from_int(0), Fixed::from_int(999)),
            Some(1)
        );
    }

    #[test]
    fn point_in_subsector_without_nodes_and_without_subsectors() {
        let mut map = test_map();
        assert_eq!(
            point_in_subsector(&map, Fixed::from_int(123), Fixed::from_int(-45)),
            Some(0),
            "nodeless map falls back to the first subsector"
        );
        map.subsectors.clear();
        assert_eq!(
            point_in_subsector(&map, Fixed::default(), Fixed::default()),
            None
        );
    }

    #[test]
    fn nightmare_clears_reaction_time() {
        let map = test_map();
        let mut rng = DoomRng::new();

        let mut store = MobjStore::new();
        let easy = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Easy,
                x: Fixed::from_int(0),
                y: Fixed::from_int(0),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_POSSESSED,
            },
        );
        let hard = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Nightmare,
                x: Fixed::from_int(100),
                y: Fixed::from_int(100),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_POSSESSED,
            },
        );

        let info = &MOBJ_INFO[MobjType::MT_POSSESSED as usize];
        assert_eq!(store.mobj(easy).unwrap().reactiontime, info.reactiontime);
        assert_eq!(store.mobj(hard).unwrap().reactiontime, 0);
    }

    #[test]
    fn all_mobj_types_table_is_dense_and_matches_the_enum() {
        for (i, &ty) in ALL_MOBJ_TYPES.iter().enumerate() {
            assert_eq!(ty as usize, i);
        }
    }

    #[test]
    fn every_spawnable_doomednum_resolves_and_matches() {
        // spawn one of each spawnable type; the type must round-trip
        let map = test_map();
        let mut store = MobjStore::new();
        let mut rng = DoomRng::new();

        for (i, info) in MOBJ_INFO.iter().enumerate() {
            if info.doomednum == -1 {
                continue;
            }
            let id = store.spawn_mobj(
                &map,
                &mut rng,
                SpawnSpec {
                    skill: Skill::Medium,
                    x: Fixed::from_int(i as i32 % 64),
                    y: Fixed::from_int(i as i32 / 64),
                    z: SpawnZ::Floor,
                    mobj_type: ALL_MOBJ_TYPES[i],
                },
            );
            assert_eq!(store.mobj(id).unwrap().mobj_type as usize, i);
        }
    }

    #[test]
    fn mobj_type_for_doomednum_spot_checks() {
        assert_eq!(mobj_type_for_doomednum(3004), Some(MobjType::MT_POSSESSED));
        assert_eq!(mobj_type_for_doomednum(3001), Some(MobjType::MT_TROOP));
        assert_eq!(mobj_type_for_doomednum(3002), Some(MobjType::MT_SERGEANT));
        assert_eq!(mobj_type_for_doomednum(2001), Some(MobjType::MT_SHOTGUN));
        assert_eq!(mobj_type_for_doomednum(2007), Some(MobjType::MT_CLIP));
        assert_eq!(mobj_type_for_doomednum(2022), Some(MobjType::MT_INV));
        assert_eq!(mobj_type_for_doomednum(2024), Some(MobjType::MT_INS));
        assert_eq!(mobj_type_for_doomednum(83), Some(MobjType::MT_MEGA));
        assert_eq!(mobj_type_for_doomednum(-1), None);
        assert_eq!(mobj_type_for_doomednum(9999), None);
    }

    #[test]
    fn mobj_type_for_doomednum_is_unique_per_type() {
        let mut seen = std::collections::HashMap::new();
        for (i, info) in MOBJ_INFO.iter().enumerate() {
            if info.doomednum == -1 {
                continue;
            }
            seen.insert(info.doomednum, i);
        }
        assert_eq!(
            seen.len(),
            MOBJ_INFO.iter().filter(|i| i.doomednum != -1).count()
        );
    }

    #[test]
    fn set_mobj_state_applies_states_and_reports_actions_in_order() {
        let map = test_map();
        let mut store = MobjStore::new();
        let mut rng = DoomRng::new();
        let id = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(0),
                y: Fixed::from_int(0),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_PAIN,
            },
        );

        let report = store.set_mobj_state(id, StateId::S_PAIN_ATK4, 100);
        // S_PAIN_ATK4 has tics == 0, so the machine settles on S_PAIN_RUN1
        assert!(report.present);
        assert_eq!(
            report.actions,
            vec![StateAction::A_PainAttack, StateAction::A_Chase]
        );
        let mobj = store.mobj(id).unwrap();
        assert_eq!(mobj.state, StateId::S_PAIN_RUN1);
        assert_eq!(
            mobj.tics,
            i32::from(STATES[StateId::S_PAIN_RUN1 as usize].tics)
        );
        assert_eq!(mobj.sprite, STATES[StateId::S_PAIN_RUN1 as usize].sprite);
        assert_eq!(mobj.frame, STATES[StateId::S_PAIN_RUN1 as usize].frame);
    }

    #[test]
    fn set_mobj_state_preserves_fullbright_frame_bit() {
        let map = test_map();
        let mut store = MobjStore::new();
        let mut rng = DoomRng::new();
        let id = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(0),
                y: Fixed::from_int(0),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_VILE,
            },
        );
        // S_VILE_ATK1 has tics == 0 and settles on S_VILE_ATK2
        let report = store.set_mobj_state(id, StateId::S_VILE_ATK1, 100);
        assert_eq!(
            report.actions,
            vec![StateAction::A_VileStart, StateAction::A_FaceTarget]
        );
        let mobj = store.mobj(id).unwrap();
        assert_eq!(mobj.state, StateId::S_VILE_ATK2);
        // S_VILE_ATK2 carries FF_FULLBRIGHT in its frame word (32774 = 0x8006)
        assert_eq!(mobj.frame, STATES[StateId::S_VILE_ATK2 as usize].frame);
        assert!(mobj.is_fullbright());
        assert_eq!(mobj.frame_index(), 6);
    }

    #[test]
    fn set_mobj_state_null_removes_and_queues_special_item() {
        let map = test_map();
        let mut store = MobjStore::new();
        let mut rng = DoomRng::new();
        let id = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(0),
                y: Fixed::from_int(0),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_CLIP, // MF_SPECIAL
            },
        );
        assert_eq!(store.item_respawn_queue().len(), 0);
        let report = store.set_mobj_state(id, StateId::S_NULL, 500);
        assert!(!report.present);
        assert_eq!(store.item_respawn_queue().len(), 1);
        let (spawnpoint, time) = store.item_respawn_queue().oldest().copied().unwrap();
        assert_eq!(time, 500);
        assert_eq!(spawnpoint.thing_type, 0); // zeroed at spawn
        assert!(store.mobj(id).is_none());
    }

    #[test]
    fn stale_handle_after_s_null_is_inert() {
        let map = test_map();
        let mut store = MobjStore::new();
        let mut rng = DoomRng::new();
        let id = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(0),
                y: Fixed::from_int(0),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_PUFF,
            },
        );
        store.set_mobj_state(id, StateId::S_NULL, 100);
        // acting on a stale handle is a no-op, not a panic
        let report = store.set_mobj_state(id, StateId::S_PAIN_RUN1, 100);
        assert!(!report.present);
        assert!(report.actions.is_empty());
        assert!(store.mobj(id).is_none());
    }

    #[test]
    fn remove_mobj_unlinks_from_sector_and_blockmap_lists() {
        let map = test_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();

        let a = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(10),
                y: Fixed::from_int(10),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_POSSESSED,
            },
        );
        let b = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(20),
                y: Fixed::from_int(20),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_POSSESSED,
            },
        );

        // newest first, like vanilla's prepend-at-head linking
        assert_eq!(store.sector_members(0).collect::<Vec<_>>(), vec![b, a]);
        // (10 - (-512)) >> 7 == 4, cell = 4 * 8 + 4 == 36
        assert_eq!(store.blockmap_members(36).collect::<Vec<_>>(), vec![b, a]);

        store.remove_mobj(b, 100);
        assert_eq!(store.sector_members(0).collect::<Vec<_>>(), vec![a]);
        assert_eq!(store.blockmap_members(36).collect::<Vec<_>>(), vec![a]);

        store.remove_mobj(a, 100);
        assert!(store.sector_members(0).next().is_none());
        assert_eq!(store.blockmap_members(36).next(), None);
        // zombies linger until the next thinker pass, like vanilla
        assert_eq!(store.len(), 2);
        store.run_thinkers(|_, _| {});
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn remove_mobj_queues_and_skips() {
        let map = test_map();
        let mut store = MobjStore::new();
        let mut rng = DoomRng::new();

        // special item: queued
        let clip = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(0),
                y: Fixed::from_int(0),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_CLIP,
            },
        );
        assert!(store.remove_mobj(clip, 42).respawn_queued);

        // invulnerability sphere: MF_SPECIAL but exempt (MT_INV)
        let inv = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(10),
                y: Fixed::from_int(10),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_INV,
            },
        );
        assert!(!store.remove_mobj(inv, 42).respawn_queued);

        // invisibility sphere: exempt (MT_INS)
        let ins = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(20),
                y: Fixed::from_int(20),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_INS,
            },
        );
        assert!(!store.remove_mobj(ins, 42).respawn_queued);

        // dropped special item: exempt (MF_DROPPED)
        let dropped = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(30),
                y: Fixed::from_int(30),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_CLIP,
            },
        );
        store.mobj_mut(dropped).unwrap().flags |= MF_DROPPED;
        assert!(!store.remove_mobj(dropped, 42).respawn_queued);

        assert_eq!(store.item_respawn_queue().len(), 1);
        let (spawnpoint, time) = store.item_respawn_queue().oldest().copied().unwrap();
        assert_eq!(time, 42);
        assert_eq!(spawnpoint.thing_type, 0);
    }

    #[test]
    fn spawn_map_thing_spawns_every_spawnable_type() {
        let map = test_map();
        let mut store = MobjStore::new();
        let mut rng = DoomRng::new();
        let rules = rules();

        let mut spawned_count = 0;
        for (i, info) in MOBJ_INFO.iter().enumerate() {
            if info.doomednum == -1 {
                continue;
            }
            let outcome = store.spawn_map_thing(
                &map,
                &mut rng,
                rules,
                &thing(
                    info.doomednum as i16,
                    (i % 32) as i16,
                    (i / 32) as i16,
                    0,
                    all_skill_flags(),
                ),
            );
            match outcome {
                SpawnOutcome::Spawned {
                    id,
                    counts_kill,
                    counts_item,
                } => {
                    assert_eq!(store.mobj(id).unwrap().mobj_type as usize, i);
                    assert_eq!(counts_kill, info.has_flag(MF_COUNTKILL));
                    assert_eq!(counts_item, info.has_flag(MF_COUNTITEM));
                    spawned_count += 1;
                }
                other => panic!(
                    "doomednum {} unexpectedly filtered: {other:?}",
                    info.doomednum
                ),
            }
        }
        assert!(spawned_count > 100, "most doomednums are spawnable");
    }

    #[test]
    fn spawn_map_thing_records_spawnpoint_angle_ambush_and_randomizes_tics() {
        let map = test_map();
        let mut store = MobjStore::new();
        let mut rng = DoomRng::new();
        let original_tics =
            i32::from(STATES[MOBJ_INFO[MobjType::MT_POSSESSED as usize].spawnstate as usize].tics);
        let outcome = store.spawn_map_thing(
            &map,
            &mut rng,
            rules(),
            &thing(3004, 100, 200, 90, all_skill_flags() | MTF_AMBUSH),
        );
        let SpawnOutcome::Spawned { id, .. } = outcome else {
            panic!("expected spawn");
        };
        let mobj = store.mobj(id).unwrap();
        assert_eq!(mobj.spawnpoint.thing_type, 3004);
        assert_eq!(mobj.spawnpoint.x, 100);
        assert_eq!(mobj.spawnpoint.y, 200);
        assert_eq!(mobj.angle, Angle::ANG90);
        assert_ne!(mobj.flags & MF_AMBUSH, 0);
        assert!(mobj.tics >= 1 && mobj.tics <= original_tics);
    }

    #[test]
    fn spawn_map_thing_filters() {
        let map = test_map();
        let mut store = MobjStore::new();
        let mut rng = DoomRng::new();

        // wrong skill bit
        let outcome =
            store.spawn_map_thing(&map, &mut rng, rules(), &thing(3004, 0, 0, 0, MTF_EASY));
        assert_eq!(outcome, SpawnOutcome::Skipped(SpawnSkip::SkillMismatch));

        // MTF_NOTSINGLE in single player
        let outcome = store.spawn_map_thing(
            &map,
            &mut rng,
            rules(),
            &thing(3004, 0, 0, 0, all_skill_flags() | MTF_NOTSINGLE),
        );
        assert_eq!(outcome, SpawnOutcome::Skipped(SpawnSkip::SinglePlayerOnly));

        // unknown doomednum: warn + skip, no crash
        let outcome = store.spawn_map_thing(
            &map,
            &mut rng,
            rules(),
            &thing(9999, 0, 0, 0, all_skill_flags()),
        );
        assert_eq!(
            outcome,
            SpawnOutcome::Skipped(SpawnSkip::UnknownDoomedNum(9999))
        );

        // deathmatch hides MF_NOTDMATCH things (5 = blue key card)
        let dm = SpawnRules {
            deathmatch: DeathmatchMode::Duel,
            ..rules()
        };
        let outcome =
            store.spawn_map_thing(&map, &mut rng, dm, &thing(5, 0, 0, 0, all_skill_flags()));
        assert_eq!(outcome, SpawnOutcome::Skipped(SpawnSkip::NotInDeathmatch));

        // -nomonsters hides monsters and skulls but not items
        let nm = SpawnRules {
            no_monsters: true,
            ..rules()
        };
        let outcome =
            store.spawn_map_thing(&map, &mut rng, nm, &thing(3004, 0, 0, 0, all_skill_flags()));
        assert_eq!(outcome, SpawnOutcome::Skipped(SpawnSkip::MonstersDisabled));
        let outcome =
            store.spawn_map_thing(&map, &mut rng, nm, &thing(2007, 0, 0, 0, all_skill_flags()));
        assert!(matches!(outcome, SpawnOutcome::Spawned { .. }));
    }

    #[test]
    fn spawn_map_thing_deathmatch_starts_and_player_starts() {
        let map = test_map();
        let mut store = MobjStore::new();
        let mut rng = DoomRng::new();

        // 12 deathmatch starts, only 10 slots
        for i in 0..12 {
            let outcome = store.spawn_map_thing(
                &map,
                &mut rng,
                rules(),
                &thing(11, i, 0, 0, all_skill_flags()),
            );
            match outcome {
                SpawnOutcome::DeathmatchStart { slot } => {
                    if i < 10 {
                        assert_eq!(slot, Some(i as usize));
                    } else {
                        assert_eq!(slot, None);
                    }
                }
                other => panic!("expected deathmatch start, got {other:?}"),
            }
        }
        assert_eq!(store.deathmatch_starts().len(), 10);

        // player starts are recorded but never spawn mobjs
        for i in 1..=4 {
            let outcome = store.spawn_map_thing(
                &map,
                &mut rng,
                rules(),
                &thing(i, 50 + i, 50, 90, all_skill_flags()),
            );
            assert_eq!(
                outcome,
                SpawnOutcome::PlayerStart {
                    player: (i - 1) as usize
                }
            );
            assert_eq!(store.player_start((i - 1) as usize).unwrap().x, 50 + i);
        }
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn respawn_specials_only_in_altdeath_after_30_seconds() {
        let map = test_map();
        let mut store = MobjStore::new();
        let mut rng = DoomRng::new();

        // spawn an ammo clip through the map-thing path, like the game does;
        // its spawnpoint carries the doomednum the respawn logic looks up
        let outcome = store.spawn_map_thing(
            &map,
            &mut rng,
            rules(),
            &thing(2007, 3, 4, 45, all_skill_flags()),
        );
        let SpawnOutcome::Spawned { id, .. } = outcome else {
            panic!("clip spawns");
        };
        store.remove_mobj(id, 100);
        assert_eq!(store.item_respawn_queue().len(), 1);

        // vanilla waits 30 * 35 = 1050 tics after the take
        let report = store.respawn_specials(
            &map,
            &mut rng,
            Skill::Medium,
            DeathmatchMode::Off,
            100 + 1050,
        );
        assert_eq!(report, RespawnReport::default());
        let report = store.respawn_specials(
            &map,
            &mut rng,
            Skill::Medium,
            DeathmatchMode::Duel,
            100 + 1050,
        );
        assert_eq!(report, RespawnReport::default());
        let report = store.respawn_specials(
            &map,
            &mut rng,
            Skill::Medium,
            DeathmatchMode::Alt,
            100 + 1049,
        );
        assert_eq!(report, RespawnReport::default());
        assert_eq!(
            store.item_respawn_queue().len(),
            1,
            "too early: still queued"
        );

        let report = store.respawn_specials(
            &map,
            &mut rng,
            Skill::Medium,
            DeathmatchMode::Alt,
            100 + 1050,
        );
        let fog = report.fog.expect("fog spawned");
        let item = report.item.expect("item respawned");
        assert_eq!(report.sfx, Some(SfxId::sfx_itmbk));
        assert_eq!(store.mobj(fog).unwrap().mobj_type, MobjType::MT_IFOG);
        assert_eq!(store.mobj(item).unwrap().mobj_type, MobjType::MT_CLIP);
        assert_eq!(store.mobj(item).unwrap().spawnpoint.thing_type, 2007);
        assert_eq!(store.mobj(item).unwrap().angle, Angle::ANG45);
        assert_eq!(store.item_respawn_queue().len(), 0);
    }

    #[test]
    fn respawn_drops_unspawnable_doomednum_with_fog_only() {
        let map = test_map();
        let mut store = MobjStore::new();
        let mut rng = DoomRng::new();

        // queue an entry by hand: a doomednum with no mobjinfo entry
        store.respawn_queue.enqueue(thing(9999, 3, 4, 0, 0), 1000);
        let report = store.respawn_specials(
            &map,
            &mut rng,
            Skill::Medium,
            DeathmatchMode::Alt,
            1000 + 1050,
        );
        assert!(report.fog.is_some());
        assert_eq!(report.item, None);
        assert_eq!(report.sfx, Some(SfxId::sfx_itmbk));
        assert_eq!(store.item_respawn_queue().len(), 0);
    }

    #[test]
    fn reset_for_level_invalidates_stale_handles_and_lists() {
        let map = test_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(0),
                y: Fixed::from_int(0),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_POSSESSED,
            },
        );
        assert_eq!(store.sector_members(0).collect::<Vec<_>>(), vec![id]);

        store.reset_for_level(&map);
        assert!(store.mobj(id).is_none());
        assert!(store.sector_members(0).next().is_none());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn run_thinkers_via_store_flags_and_collects() {
        let map = test_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let a = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(0),
                y: Fixed::from_int(0),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_PUFF,
            },
        );
        let b = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(10),
                y: Fixed::from_int(10),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_PUFF,
            },
        );
        let mut ran = Vec::new();
        store.run_thinkers(|reg, id| {
            ran.push(id);
            if id == a {
                // flag b (it hasn't run yet this pass): b is skipped
                reg.remove(b);
            }
        });
        assert_eq!(ran, vec![a]);
        assert_eq!(store.len(), 1);
        store.remove_mobj(a, 100);
        let mut ran = Vec::new();
        store.run_thinkers(|_, id| ran.push(id));
        assert!(ran.is_empty());
        assert_eq!(store.len(), 0);
    }
    #[test]
    fn blood_lists_in_sector_but_not_blockmap() {
        // MT_BLOOD is MF_NOBLOCKMAP (doesn't block) but not MF_NOSECTOR
        let map = test_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(10),
                y: Fixed::from_int(10),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_BLOOD,
            },
        );
        assert_eq!(store.mobj(id).unwrap().flags & MF_NOBLOCKMAP, MF_NOBLOCKMAP);
        assert_eq!(store.sector_members(0).collect::<Vec<_>>(), vec![id]);
        // cell 36 holds non-blocklisting things? no: block list must be empty
        assert!(store.blockmap_members(36).next().is_none());
    }

    #[test]
    fn teleportman_is_not_listed_anywhere() {
        // MT_TELEPORTMAN is MF_NOBLOCKMAP | MF_NOSECTOR
        let map = test_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let _id = store.spawn_mobj(
            &map,
            &mut rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(10),
                y: Fixed::from_int(10),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_TELEPORTMAN,
            },
        );
        assert!(store.sector_members(0).next().is_none());
        assert!(store.blockmap_members(36).next().is_none());
    }
}
