//! Line and sector specials: doors, switches, light-flash thinkers,
//! teleports, floors, plats, ceilings and the line-special dispatcher.
//! Ported from `p_doors.c`, `p_switch.c`, `p_lights.c`, `p_telept.c`,
//! `p_floor.c`, `p_plats.c`, `p_ceilng.c` and `p_spec.c`.
//!
//! ## Model
//!
//! Vanilla keeps all of these in the single THINKERS list and runs them in
//! insertion order. Here that stays the same: the special thinkers are
//! [`Thinker`] variants in the same registry the mobjs live in, and
//! [`run_thinkers`] walks the list once per tic, dispatching the special
//! variants and ignoring the mobj ones (the game loop drives those).
//!
//! Thinkers spawned by gameplay code (a door triggered by a switch) cannot
//! be added to the registry from outside a pass — the registry belongs to
//! the store. They are queued in [`SpecialContext::pending_thinkers`] and
//! appended at the tail before the next pass walks the list, mirroring
//! vanilla's `P_AddThinker` during `P_PlayerThink` running inside the
//! same tic's `P_RunThinkers`. Mutations of an existing thinker from
//! outside the pass (the manual-use door flip) go through a small command
//! queue for the same reason.
//!
//! ## What is deliberately not here
//!
//! * The animated flats/textures tables (`animdefs`/`texturetranslation`)
//!   — they feed the render layer's translation tables, which owns them.
//! * The world-tic damage plumbing for sector specials (5/7/16/4/11) is
//!   here as [`lineactions::player_in_special_sector`], but the actual
//!   damage application is the game loop's (via the events channel).
//!
//! ## Not-quite-vanilla notes
//!
//! * Sounds and damage are plain-data events
//!   ([`SpecialEvent`]) instead of calls into audio/inter code. The game
//!   loop routes them. RNG consumption order differs from vanilla in the
//!   crush path only: the blood-spray momentum is rolled here (2 calls) but
//!   the pain roll of `P_DamageMobj` happens in the damage dispatch.
//! * Sector-origin sounds carry the sector index instead of a
//!   `soundorg` pointer; the game loop picks an anchor.
//! * The `thing_height_clip` half of `P_ChangeSector` recomputes heights
//!   from the map and the registry instead of going through
//!   `MoveContext`/`check_position`, which would need `&mut MobjStore`
//!   inside the thinker pass. Missile/skull-fly damage side effects of the
//!   vanilla check are not replicated (a hovering missile overlapping a
//!   thing inside a crushing sector does not wound it), and the RNG is
//!   consumed for the blood spray only. The door-bounce decision (the part
//!   that matters for gameplay) is identical.
//! * "Linked" switch lines (switch lines with the same tag toggling
//!   together) are not a linuxdoom-1.10 feature at all — there is no tag
//!   fan-out for wall textures in the original. What vanilla does fan out
//!   is the *sector* tag on door actions (`P_FindSectorFromLineTag`), and
//!   that is kept. [`SpecialEvent::SwitchToggled`] carries the line index
//!   and tag so a compatible "linked switch" layer can be added later
//!   without changing this module.

pub(crate) mod ceilings;
pub(crate) mod doors;
pub(crate) mod floor;
mod lights;
pub(crate) mod lineactions;
pub(crate) mod movers;
pub(crate) mod plats;
mod switches;
mod teleport;
mod util;

#[cfg(test)]
mod testutil;

use crate::fixed::Fixed;
use crate::info::SfxId;
use crate::map::Map;
use crate::mobj::{MobjStore, Skill};
use crate::movement::MoveEvent;
use crate::player::Player;
use crate::rng::DoomRng;
use crate::thinker::{Thinker, ThinkerId, ThinkerRegistry};

pub use ceilings::{CEILSPEED, Ceiling, CeilingKind, ev_ceiling_crush_stop, ev_do_ceiling};
pub use doors::{
    DoorKind, VDOORSPEED, VDOORWAIT, VerticalDoor, ev_do_door, ev_do_door_tag, ev_do_locked_door,
    ev_vertical_door, spawn_door_close_in_30, spawn_door_raise_in_5_mins,
};
pub use floor::{
    FLOORSPEED, FloorKind, FloorMover, StairKind, ev_build_stairs, ev_do_donut, ev_do_floor,
    ev_do_floor_tag,
};
pub use lights::{
    FASTDARK, FireFlicker, GLOWSPEED, Glow, LightFlash, SLOWDARK, STROBEBRIGHT, StrobeFlash,
    ev_light_turn_on, ev_start_light_strobing, ev_turn_tag_lights_off, spawn_fire_flicker,
    spawn_glowing_light, spawn_light_flash, spawn_strobe_flash,
};
pub use lineactions::{
    LineAction, SpawnSpecialsReport, SwitchState, cross_special_line, handle_line_activation,
    player_in_special_sector, shoot_special_line, spawn_specials, update_specials,
    use_special_line,
};
pub use plats::{PLATSPEED, PLATWAIT, Plat, PlatKind, PlatStatus, ev_do_plat, ev_stop_plat};
pub use switches::{
    Button, ButtonList, ButtonWhere, SwitchList, change_switch_texture, update_buttons,
};
pub use teleport::ev_teleport;

// the button accounting constants come straight from info, next to the
// switch table they belong to
pub use crate::info::{BUTTONTIME, MAXBUTTONS, MAXSWITCHES};

/// Side effects the specials layer produces for the game loop to apply.
///
/// Sounds with a sector index are the vanilla
/// `S_StartSound(&sector->soundorg, sfx)` call — there is no mobj to pass
/// to the audio layer, so the game loop picks a position for the sector.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpecialEvent {
    /// Vanilla `S_StartSound(NULL, sfx)` — full volume, no position.
    GlobalSound {
        /// The effect to play.
        sfx: SfxId,
    },
    /// Vanilla `S_StartSound(&sector->soundorg, sfx)`.
    SectorSound {
        /// Index into [`Map::sectors`].
        sector: usize,
        /// The effect to play.
        sfx: SfxId,
    },
    /// Vanilla `S_StartSound(mobj, sfx)`.
    ActorSound {
        /// The mobj to play at.
        actor: ThinkerId,
        /// The effect to play.
        sfx: SfxId,
    },
    /// Vanilla `P_DamageMobj(target, inflictor, source, damage)`.
    ///
    /// `blood` carries the two RNG momenta vanilla rolled for the spurt
    /// (`(P_Random() - P_Random()) << 12`), so the game loop can rebuild
    /// the spray with the same values.
    Damage {
        /// The mobj taking the damage.
        target: ThinkerId,
        /// What dealt it (the crusher is not a mobj, so `None`).
        inflictor: Option<ThinkerId>,
        /// Who launched it (`None` for crush damage).
        source: Option<ThinkerId>,
        /// Raw damage before armor.
        damage: i32,
        /// The two spurt momenta rolled at crush time, if any.
        blood: Option<(Fixed, Fixed)>,
    },
    /// Vanilla `P_SetMobjState(thing, S_GIBS)` plus the flag/height/radius
    /// zeroing from `PIT_ChangeSector`: a corpse crushed by a moving
    /// sector turns to giblets. The game loop applies the state change and
    /// the spawn of the spurt `MT_BLOOD`.
    GibCorpse {
        /// The corpse that got gibbed.
        id: ThinkerId,
        /// X momentum of the spurt.
        momx: Fixed,
        /// Y momentum of the spurt.
        momy: Fixed,
    },
    /// Vanilla `P_RemoveMobj` on an `MF_DROPPED` item crushed by a moving
    /// sector — the item pops out of the world.
    CrushRemove {
        /// The item to remove.
        id: ThinkerId,
    },
    /// A wall switch changed texture. `use_again` mirrors vanilla's button
    /// flag; `tag` is the line's tag (0 when the map left it blank) so a
    /// "linked switch" layer can fan the toggle out by tag.
    SwitchToggled {
        /// Index into [`Map::linedefs`].
        line: usize,
        /// Vanilla `line_t::tag`.
        tag: i16,
        /// True when the switch is a button (reverts after cooldown).
        use_again: bool,
    },
    /// A movement-layer side effect produced while applying a teleport
    /// (e.g. stomping an item at the destination); forwarded verbatim from
    /// the movement layer's report.
    Move(MoveEvent),
    /// Vanilla `G_ExitLevel()` / `G_SecretExitLevel()`: the level is over.
    /// The game loop decides what to do with the world (wipe to the
    /// intermission or the secret-exit map). `secret` selects the secret
    /// exit path (vanilla `G_SecretExitLevel`).
    ExitLevel {
        /// True for the secret exit, false for the regular one.
        secret: bool,
    },
}

/// Resolves a raw 8-byte texture name to its height in fixed point.
///
/// `None` for missing textures (vanilla's `-1` texture number). Only
/// the raise-to-texture floor special needs it; when the resolver is
/// absent the special is skipped with a warning instead of moving a
/// floor to a made-up height. The game loop wires its texture set in
/// here (vanilla read the render-side `textureheight[]` table
/// directly).
#[derive(Clone, Copy)]
pub struct TextureHeightResolver<'a>(pub &'a dyn Fn(&[u8; 8]) -> Option<Fixed>);

impl std::fmt::Debug for TextureHeightResolver<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TextureHeightResolver(..)")
    }
}

/// Everything the specials layer is allowed to touch.
///
/// The thinker pass destructures this into disjoint borrows, so no code in
/// here can smuggle `&MobjStore` into the registry walk — the pass runs
/// with only the registry, the map, the RNG and the event buffer. The
/// out-of-pass entry points (`EV_*` functions called from the game loop's
/// use/cross/shoot handlers) get the whole thing, including the store.
#[derive(Debug)]
pub struct SpecialContext<'a> {
    /// The map being played. Specials mutate it (line specials clear
    /// themselves, switch textures swap, sectors move, sector lights get
    /// zeroed when a thinker spawns).
    pub map: &'a mut Map,
    /// The actor store. The thinker pass never touches it; the out-of-pass
    /// entry points do.
    pub store: &'a mut MobjStore,
    /// The players array (vanilla `players`).
    pub players: &'a mut [Player],
    /// The deterministic RNG.
    pub rng: &'a mut DoomRng,
    /// Vanilla `leveltime`.
    pub leveltime: i32,
    /// Vanilla `gameskill`; fog spawns carry it.
    pub skill: Skill,
    /// Vanilla `gamemap == 30`: teleports flatten things that are not
    /// shootable (the boss level).
    pub boss_level: bool,
    /// Resolves a raw 8-byte texture name to its height in fixed point.
    /// See [`TextureHeightResolver`].
    pub texture_height: Option<TextureHeightResolver<'a>>,
    /// Thinkers queued by the gameplay layer this tic; they are appended
    /// at the start of the next pass and execute there, like vanilla's
    /// in-pass `P_AddThinker`.
    pub pending_thinkers: Vec<Thinker>,
    /// Work the out-of-pass layer queued for a live thinker (the manual
    /// door flip). Applied at the start of the next pass, before any
    /// thinker runs.
    pending_commands: Vec<SpecialCommand>,
    /// Side effects for the game loop, in production order.
    pub events: Vec<SpecialEvent>,
}

/// A mutation of an existing thinker, queued from outside the pass because
/// the registry is not reachable there. Applied at the start of the next
/// [`run_thinkers`] walk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SpecialCommand {
    /// Vanilla manual-door re-trigger: flip a sector's door direction
    /// (`ev_vertical_door`).
    FlipDoorDirection {
        /// The door sector.
        sector: usize,
        /// The direction to force (1 = up, -1 = down).
        direction: i32,
    },
}

impl<'a> SpecialContext<'a> {
    /// A context with vanilla-default rules (medium skill, not the boss
    /// level).
    pub fn new(
        map: &'a mut Map,
        store: &'a mut MobjStore,
        players: &'a mut [Player],
        rng: &'a mut DoomRng,
    ) -> Self {
        Self {
            map,
            store,
            players,
            rng,
            leveltime: 0,
            skill: Skill::Medium,
            boss_level: false,
            texture_height: None,
            pending_thinkers: Vec::new(),
            pending_commands: Vec::new(),
            events: Vec::new(),
        }
    }

    /// Sets the texture-height resolver (see [`SpecialContext::texture_height`]).
    pub fn with_texture_height(mut self, resolver: &'a dyn Fn(&[u8; 8]) -> Option<Fixed>) -> Self {
        self.texture_height = Some(TextureHeightResolver(resolver));
        self
    }

    fn queue_flip_door_direction(&mut self, sector: usize, direction: i32) {
        self.pending_commands
            .push(SpecialCommand::FlipDoorDirection { sector, direction });
    }
}

/// What a thinker handler gets to see. The out-of-pass entry points take
/// the full [`SpecialContext`]; in the pass the registry is only reachable
/// through the closure argument, so handlers work on this shape instead.
/// All fields are reborrowed disjointly from the context for the duration
/// of the pass — the store is not in here.
#[derive(Debug)]
pub(crate) struct PassCtx<'a> {
    /// The map being played.
    pub map: &'a mut Map,
    /// The deterministic RNG.
    pub rng: &'a mut DoomRng,
    /// Vanilla `leveltime`.
    pub leveltime: i32,
    /// Side effects for the game loop, in production order.
    pub events: &'a mut Vec<SpecialEvent>,
}

/// Runs the thinker pass: every special thinker in the list gets its
/// per-tic step, in list order. Mobjs are left alone — the game loop runs
/// those through its own dispatch.
///
/// Thinkers queued in [`SpecialContext::pending_thinkers`] are appended at
/// the tail of the registry and commands from the queue (the manual door
/// flip) are applied before any thinker runs — like vanilla's
/// `P_PlayerThink` doing its spawns/flips before `P_RunThinkers` walks the
/// list. Queued thinkers execute in the same pass, after everything that
/// was already in the list, even when the list was empty.
pub fn run_thinkers(ctx: &mut SpecialContext) {
    // Split the context into disjoint borrows: the registry walk only
    // needs these, never the store itself.
    let SpecialContext {
        map,
        store,
        rng,
        leveltime,
        pending_thinkers,
        pending_commands,
        events,
        ..
    } = ctx;
    let store: &mut MobjStore = store;
    let mut pass = PassCtx {
        map,
        rng,
        leveltime: *leveltime,
        events,
    };
    let mut pending = std::mem::take(pending_thinkers);
    let mut commands = std::mem::take(pending_commands);

    if !pending.is_empty() || !commands.is_empty() {
        let reg = store.registry_mut();
        for thinker in pending.drain(..) {
            reg.add(thinker);
        }
        apply_commands(reg, &mut commands);
    }

    store.run_thinkers(|reg, id| dispatch(reg, id, &mut pass));
}

fn apply_commands(reg: &mut ThinkerRegistry, commands: &mut Vec<SpecialCommand>) {
    for command in commands.drain(..) {
        match command {
            SpecialCommand::FlipDoorDirection { sector, direction } => {
                // find the door for this sector and force its direction
                let mut flipped = false;
                let door_ids: Vec<ThinkerId> = reg
                    .iter()
                    .filter_map(|(id, thinker)| match thinker {
                        Thinker::Door(door) if door.sector == sector => Some(id),
                        _ => None,
                    })
                    .collect();
                for id in door_ids {
                    if let Some(Thinker::Door(door)) = reg.get_mut(id) {
                        door.direction = direction;
                        flipped = true;
                    }
                }
                if !flipped {
                    tracing::warn!(
                        "FlipDoorDirection: no live door thinker for sector {sector} (door finished this tic?)"
                    );
                }
            }
        }
    }
}

/// Which special variant a thinker is; used to avoid holding the registry
/// borrow while a handler re-borrows it.
enum Dispatch {
    Door,
    FireFlicker,
    LightFlash,
    StrobeFlash,
    Glow,
    FloorMover,
    Plat,
    Ceiling,
}

fn dispatch(reg: &mut ThinkerRegistry, id: ThinkerId, ctx: &mut PassCtx<'_>) {
    let kind = match reg.get(id) {
        Some(Thinker::Door(_)) => Dispatch::Door,
        Some(Thinker::FireFlicker(_)) => Dispatch::FireFlicker,
        Some(Thinker::LightFlash(_)) => Dispatch::LightFlash,
        Some(Thinker::StrobeFlash(_)) => Dispatch::StrobeFlash,
        Some(Thinker::Glow(_)) => Dispatch::Glow,
        Some(Thinker::FloorMover(_)) => Dispatch::FloorMover,
        Some(Thinker::Plat(_)) => Dispatch::Plat,
        Some(Thinker::Ceiling(_)) => Dispatch::Ceiling,
        // mobjs and anything not ours
        _ => return,
    };
    match kind {
        Dispatch::Door => doors::run_vertical_door(reg, id, ctx),
        Dispatch::FireFlicker => lights::run_fire_flicker(reg, id, ctx),
        Dispatch::LightFlash => lights::run_light_flash(reg, id, ctx),
        Dispatch::StrobeFlash => lights::run_strobe_flash(reg, id, ctx),
        Dispatch::Glow => lights::run_glow(reg, id, ctx),
        Dispatch::FloorMover => floor::run_floor_mover(reg, id, ctx),
        Dispatch::Plat => plats::run_plat(reg, id, ctx),
        Dispatch::Ceiling => ceilings::run_ceiling(reg, id, ctx),
    }
}

/// Re-exports used by the busy-thinker scans (active doors, pending
/// thinkers). Kept crate-local so the game loop can't depend on them.
pub(crate) fn active_door_direction(
    store: &MobjStore,
    pending: &[Thinker],
    sector: usize,
) -> Option<i32> {
    for thinker in pending {
        if let Thinker::Door(door) = thinker
            && door.sector == sector
        {
            return Some(door.direction);
        }
    }
    for (_, thinker) in store.thinkers() {
        if let Thinker::Door(door) = thinker
            && door.sector == sector
        {
            return Some(door.direction);
        }
    }
    None
}

/// Visits every thinker (queued this tic first, then the live registry)
/// with a mutable borrow. Used by the stasis scans of `EV_StopPlat` /
/// `EV_CeilingCrushStop` and the re-activation in `P_ActivateInStasis`.
pub(crate) fn for_each_thinker_mut(
    store: &mut crate::mobj::MobjStore,
    pending: &mut [Thinker],
    mut f: impl FnMut(&mut Thinker),
) {
    for thinker in pending.iter_mut() {
        f(thinker);
    }
    let ids: Vec<ThinkerId> = store.thinkers().map(|(id, _)| id).collect();
    let reg = store.registry_mut();
    for id in ids {
        if let Some(thinker) = reg.get_mut(id) {
            f(thinker);
        }
    }
}

/// The sector a mover thinker is operating on, if any.
///
/// Vanilla's `sector->specialdata` is a single pointer shared by doors,
/// plats, ceilings and floors — a sector with any of those running is
/// busy for all of them. This is the rust-side equivalent of reading
/// that pointer.
pub(crate) fn mover_sector(thinker: &Thinker) -> Option<usize> {
    match thinker {
        Thinker::Door(door) => Some(door.sector),
        Thinker::FloorMover(floor) => Some(floor.sector),
        Thinker::Plat(plat) => Some(plat.sector),
        Thinker::Ceiling(ceiling) => Some(ceiling.sector),
        _ => None,
    }
}

/// True when any mover thinker (door, plat, ceiling or floor) is already
/// operating on `sector`, live or queued this tic. Vanilla tracks this
/// through `sector->specialdata`; a registry scan keeps the registry the
/// single source of truth.
pub(crate) fn sector_busy(store: &MobjStore, pending: &[Thinker], sector: usize) -> bool {
    pending.iter().any(|t| mover_sector(t) == Some(sector))
        || store
            .thinkers()
            .any(|(_, thinker)| mover_sector(thinker) == Some(sector))
}
