//! Vertical doors, ported from `p_doors.c`.
//!
//! Door kinds (`vldoor_e`) and the `EV_VerticalDoor`/`EV_DoDoor`/
//! `EV_DoLockedDoor` actions plus the `T_VerticalDoor` thinker step.
//! Blaze doors are the fast variants (`VDOORSPEED * 4`).
//!
//! The door thinker is [`VerticalDoor`], registered as a [`Thinker`]
//! variant; [`run_vertical_door`] is its per-tic step. Doors spawned from
//! gameplay code land in `SpecialContext::pending_thinkers` and join the
//! list at the start of the next pass.

use crate::fixed::{FRACUNIT, Fixed};
use crate::hud::HudMessage;
use crate::info::SfxId;
use crate::map::Map;
use crate::player::Card;
use crate::specials::movers::{MoveOutcome, PlaneKind, move_plane};
use crate::specials::util::{find_lowest_ceiling_surrounding, find_sector_from_line_tag};
use crate::specials::{PassCtx, SpecialContext, SpecialEvent, active_door_direction, sector_busy};
use crate::thinker::{Thinker, ThinkerId, ThinkerRegistry};

/// Vanilla `VDOORSPEED`.
pub const VDOORSPEED: Fixed = Fixed::from_raw(2 * FRACUNIT);
/// Vanilla `VDOORWAIT` (at the top, in ticks).
pub const VDOORWAIT: i32 = 150;
/// The amount the door stops short of the ceiling (`4*FRACUNIT`).
const VDOOR_CEILING_OFFSET: Fixed = Fixed::from_raw(4 * FRACUNIT);

/// Vanilla `vldoor_e`. Discriminants follow the original order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DoorKind {
    /// Regular DOOM door: waits `VDOORWAIT` at the top.
    Normal = 0,
    /// Closes, waits 30 seconds, then opens.
    Close30ThenOpen = 1,
    /// Locked-door style close (stays shut, no wait-at-top).
    Close = 2,
    /// One-way open (closes never again).
    Open = 3,
    /// Opens once, five minutes after map start (sector special 14).
    RaiseIn5Mins = 4,
    /// Regular door, four times faster.
    BlazeRaise = 5,
    /// One-way open, four times faster.
    BlazeOpen = 6,
    /// Close, four times faster.
    BlazeClose = 7,
}

/// Vanilla `vldoor_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VerticalDoor {
    /// What kind of movement this door performs.
    pub kind: DoorKind,
    /// Index of the door's sector.
    pub sector: usize,
    /// The ceiling height the door rises to.
    pub topheight: Fixed,
    /// Movement speed per tic.
    pub speed: Fixed,
    /// Vanilla `direction`: 1 = up, 0 = waiting at the top, -1 = down,
    /// 2 = the five-minute initial wait.
    pub direction: i32,
    /// Tics to wait at the top (`VDOORWAIT`).
    pub topwait: i32,
    /// Countdown to the next direction flip (top wait, the 5-minute
    /// delay, or the 30-second close-then-open pause).
    pub topcountdown: i32,
}

/// Vanilla `EV_DoDoor`: start the tagged doors moving. Returns true when
/// at least one door was started, false otherwise (vanilla's `rtn`).
pub fn ev_do_door(ctx: &mut SpecialContext, line: usize, kind: DoorKind) -> bool {
    let tag = ctx.map.linedefs[line].tag;
    ev_do_door_tag(ctx, tag, kind)
}

/// Like [`ev_do_door`] but with a bare tag; for the triggers that are
/// not tied to a linedef (the boss-death door actions).
pub fn ev_do_door_tag(ctx: &mut SpecialContext, tag: i16, kind: DoorKind) -> bool {
    let mut rtn = false;
    let mut cursor = None;

    while let Some(sector) = find_sector_from_line_tag(ctx.map, tag, cursor) {
        cursor = Some(sector);
        if sector_busy(ctx.store, &ctx.pending_thinkers, sector) {
            continue;
        }
        rtn = true;

        let blaze = matches!(
            kind,
            DoorKind::BlazeRaise | DoorKind::BlazeOpen | DoorKind::BlazeClose
        );
        let speed = if blaze {
            Fixed::from_raw(VDOORSPEED.to_raw() * 4)
        } else {
            VDOORSPEED
        };

        let (direction, topheight) = match kind {
            DoorKind::BlazeClose | DoorKind::Close => {
                (-1, lowest_surrounding_ceiling_minus_offset(ctx.map, sector))
            }
            DoorKind::Close30ThenOpen => (-1, ctx.map.sectors[sector].ceiling_height),
            // blazeRaise/blazeOpen and the regular variants all rise
            DoorKind::BlazeRaise | DoorKind::BlazeOpen | DoorKind::Normal | DoorKind::Open => {
                (1, lowest_surrounding_ceiling_minus_offset(ctx.map, sector))
            }
            DoorKind::RaiseIn5Mins => (1, lowest_surrounding_ceiling_minus_offset(ctx.map, sector)),
        };

        // vanilla: the open/raise sound only plays when the door actually
        // has somewhere to go (top height below the current ceiling)
        match kind {
            DoorKind::BlazeClose | DoorKind::Close30ThenOpen | DoorKind::Close => {
                let sfx = if blaze {
                    SfxId::sfx_bdcls
                } else {
                    SfxId::sfx_dorcls
                };
                ctx.events.push(SpecialEvent::SectorSound { sector, sfx });
            }
            DoorKind::BlazeRaise | DoorKind::BlazeOpen => {
                if topheight != ctx.map.sectors[sector].ceiling_height {
                    ctx.events.push(SpecialEvent::SectorSound {
                        sector,
                        sfx: SfxId::sfx_bdopn,
                    });
                }
            }
            DoorKind::Normal | DoorKind::Open | DoorKind::RaiseIn5Mins => {
                if topheight != ctx.map.sectors[sector].ceiling_height {
                    ctx.events.push(SpecialEvent::SectorSound {
                        sector,
                        sfx: SfxId::sfx_doropn,
                    });
                }
            }
        }

        ctx.pending_thinkers.push(Thinker::Door(VerticalDoor {
            kind,
            sector,
            topheight,
            speed,
            direction,
            topwait: VDOORWAIT,
            topcountdown: 0,
        }));
    }

    rtn
}

/// Vanilla `EV_DoLockedDoor`: the key-checked wrapper around
/// [`ev_do_door`]. Returns false (and does not open) without the key.
pub fn ev_do_locked_door(
    ctx: &mut SpecialContext,
    line: usize,
    kind: DoorKind,
    thing: ThinkerId,
) -> bool {
    let special = ctx.map.linedefs[line].special;
    let player_index = ctx.store.mobj(thing).and_then(|mobj| mobj.player);

    // vanilla returns silently for non-player users
    let Some(pi) = player_index else {
        return false;
    };

    let (has_key, message) = match special {
        99 | 133 => (
            ctx.players[pi].cards[Card::Blue.index()]
                || ctx.players[pi].cards[Card::BlueSkull.index()],
            HudMessage::PdBlueO,
        ),
        134 | 135 => (
            ctx.players[pi].cards[Card::Red.index()]
                || ctx.players[pi].cards[Card::RedSkull.index()],
            HudMessage::PdRedO,
        ),
        136 | 137 => (
            ctx.players[pi].cards[Card::Yellow.index()]
                || ctx.players[pi].cards[Card::YellowSkull.index()],
            HudMessage::PdYellowO,
        ),
        _ => (true, HudMessage::PdBlueO),
    };

    if !has_key {
        ctx.players[pi].message = Some(message.as_str().to_string());
        ctx.events.push(SpecialEvent::GlobalSound {
            sfx: SfxId::sfx_oof,
        });
        return false;
    }

    ev_do_door(ctx, line, kind)
}

/// Vanilla `EV_VerticalDoor`: a manual door use (the player "opened" the
/// line). Returns true when the use was consumed.
///
/// Only front sides of lines can be used; the caller (`P_UseSpecialLine`)
/// enforces that. Monsters may use unlocked manual doors but never locked
/// ones, and never start a door moving down (bad guys never close doors).
pub fn ev_vertical_door(ctx: &mut SpecialContext, line: usize, thing: ThinkerId) -> bool {
    let player_index = ctx.store.mobj(thing).and_then(|mobj| mobj.player);

    let special = ctx.map.linedefs[line].special;

    // lock check
    match special {
        26 | 32 | 27 | 34 | 28 | 33 => {
            let Some(pi) = player_index else {
                return false;
            };
            let (has_key, message) = match special {
                26 | 32 => (
                    ctx.players[pi].cards[Card::Blue.index()]
                        || ctx.players[pi].cards[Card::BlueSkull.index()],
                    HudMessage::PdBlueK,
                ),
                27 | 34 => (
                    ctx.players[pi].cards[Card::Yellow.index()]
                        || ctx.players[pi].cards[Card::YellowSkull.index()],
                    HudMessage::PdYellowK,
                ),
                _ => (
                    ctx.players[pi].cards[Card::Red.index()]
                        || ctx.players[pi].cards[Card::RedSkull.index()],
                    HudMessage::PdRedK,
                ),
            };
            if !has_key {
                ctx.players[pi].message = Some(message.as_str().to_string());
                ctx.events.push(SpecialEvent::GlobalSound {
                    sfx: SfxId::sfx_oof,
                });
                return false;
            }
        }
        _ => {}
    }

    // the door's sector is the back side of the line (vanilla
    // `sides[line->sidenum[side^1]].sector` with side == 0)
    let Some(back_side) = ctx.map.linedefs[line].sidenum[1] else {
        tracing::warn!("manual door on a one-sided line {line}; ignoring");
        return false;
    };
    let sector = ctx.map.sidedefs[back_side].sector;

    // if the sector has an active door thinker, use it instead of spawning
    // a second one (but only for the "raise" families)
    if let Some(direction) = active_door_direction(ctx.store, &ctx.pending_thinkers, sector) {
        match special {
            1 | 26 | 27 | 28 | 117 => {
                if direction == -1 {
                    // go back up
                    ctx.queue_flip_door_direction(sector, 1);
                } else {
                    let Some(_pi) = player_index else {
                        // bad guys never close doors
                        return false;
                    };
                    // start going down immediately
                    ctx.queue_flip_door_direction(sector, -1);
                }
                return true;
            }
            _ => {
                // vanilla falls through and spawns a second door; keep
                // that quirk (opening a perma-open door mid-animation)
            }
        }
    }

    // for proper sound
    let sfx = match special {
        117 | 118 => SfxId::sfx_bdopn,
        1 | 31 => SfxId::sfx_doropn,
        _ => SfxId::sfx_doropn,
    };
    ctx.events.push(SpecialEvent::SectorSound { sector, sfx });

    // new door thinker
    let (kind, speed) = match special {
        1 | 26 | 27 | 28 => (DoorKind::Normal, VDOORSPEED),
        31..=34 => (DoorKind::Open, VDOORSPEED),
        117 => (
            DoorKind::BlazeRaise,
            Fixed::from_raw(VDOORSPEED.to_raw() * 4),
        ),
        118 => (
            DoorKind::BlazeOpen,
            Fixed::from_raw(VDOORSPEED.to_raw() * 4),
        ),
        _ => (DoorKind::Normal, VDOORSPEED),
    };

    // one-way open doors clear their own line special (vanilla)
    if matches!(special, 31 | 32 | 33 | 34 | 118) {
        ctx.map.linedefs[line].special = 0;
    }

    // find the top of the movement range
    let topheight = lowest_surrounding_ceiling_minus_offset(ctx.map, sector);

    ctx.pending_thinkers.push(Thinker::Door(VerticalDoor {
        kind,
        sector,
        topheight,
        speed,
        direction: 1,
        topwait: VDOORWAIT,
        topcountdown: 0,
    }));

    true
}

/// Vanilla `P_SpawnDoorCloseIn30`: a sector that will close 30 seconds
/// after map start. The sector special is cleared as a side effect.
pub fn spawn_door_close_in_30(ctx: &mut SpecialContext, sector: usize) {
    ctx.map.sectors[sector].special = 0;
    ctx.pending_thinkers.push(Thinker::Door(VerticalDoor {
        kind: DoorKind::Normal,
        sector,
        // unused on the close path; vanilla leaves it uninitialized
        topheight: Fixed::ZERO,
        speed: VDOORSPEED,
        direction: 0,
        topwait: VDOORWAIT,
        topcountdown: 30 * 35,
    }));
}

/// Vanilla `P_SpawnDoorRaiseIn5Mins`: a sector that opens five minutes
/// after map start. The sector special is cleared as a side effect.
pub fn spawn_door_raise_in_5_mins(ctx: &mut SpecialContext, sector: usize) {
    ctx.map.sectors[sector].special = 0;
    ctx.pending_thinkers.push(Thinker::Door(VerticalDoor {
        kind: DoorKind::RaiseIn5Mins,
        sector,
        topheight: lowest_surrounding_ceiling_minus_offset(ctx.map, sector),
        speed: VDOORSPEED,
        direction: 2,
        topwait: VDOORWAIT,
        topcountdown: 5 * 60 * 35,
    }));
}

/// Vanilla `T_VerticalDoor`: the door thinker's per-tic step.
pub(crate) fn run_vertical_door(reg: &mut ThinkerRegistry, id: ThinkerId, ctx: &mut PassCtx<'_>) {
    let mut door = match reg.get(id) {
        Some(Thinker::Door(door)) => *door,
        _ => return,
    };

    match door.direction {
        0 => {
            // WAITING
            door.topcountdown -= 1;
            if door.topcountdown == 0 {
                match door.kind {
                    DoorKind::BlazeRaise => {
                        door.direction = -1; // time to go back down
                        ctx.events.push(SpecialEvent::SectorSound {
                            sector: door.sector,
                            sfx: SfxId::sfx_bdcls,
                        });
                    }
                    DoorKind::Normal => {
                        door.direction = -1; // time to go back down
                        ctx.events.push(SpecialEvent::SectorSound {
                            sector: door.sector,
                            sfx: SfxId::sfx_dorcls,
                        });
                    }
                    DoorKind::Close30ThenOpen => {
                        door.direction = 1;
                        ctx.events.push(SpecialEvent::SectorSound {
                            sector: door.sector,
                            sfx: SfxId::sfx_doropn,
                        });
                    }
                    _ => {}
                }
            }
        }
        2 => {
            // INITIAL WAIT (five minutes)
            door.topcountdown -= 1;
            if door.topcountdown == 0 && door.kind == DoorKind::RaiseIn5Mins {
                door.direction = 1;
                door.kind = DoorKind::Normal;
                ctx.events.push(SpecialEvent::SectorSound {
                    sector: door.sector,
                    sfx: SfxId::sfx_doropn,
                });
            }
        }
        -1 => {
            // DOWN
            let floor = ctx.map.sectors[door.sector].floor_height;
            let res = move_plane(
                reg,
                ctx,
                door.sector,
                door.speed,
                floor,
                false,
                PlaneKind::Ceiling,
                -1,
            );
            if res == MoveOutcome::PastDest {
                match door.kind {
                    DoorKind::BlazeRaise | DoorKind::BlazeClose => {
                        reg.remove(id);
                        ctx.events.push(SpecialEvent::SectorSound {
                            sector: door.sector,
                            sfx: SfxId::sfx_bdcls,
                        });
                    }
                    DoorKind::Normal | DoorKind::Close => {
                        reg.remove(id);
                    }
                    DoorKind::Close30ThenOpen => {
                        door.direction = 0;
                        door.topcountdown = 35 * 30;
                    }
                    _ => {}
                }
            } else if res == MoveOutcome::Crushed {
                match door.kind {
                    DoorKind::BlazeClose | DoorKind::Close => {
                        // DO NOT GO BACK UP!
                    }
                    _ => {
                        door.direction = 1;
                        ctx.events.push(SpecialEvent::SectorSound {
                            sector: door.sector,
                            sfx: SfxId::sfx_doropn,
                        });
                    }
                }
            }
        }
        1 => {
            // UP
            let res = move_plane(
                reg,
                ctx,
                door.sector,
                door.speed,
                door.topheight,
                false,
                PlaneKind::Ceiling,
                1,
            );
            if res == MoveOutcome::PastDest {
                match door.kind {
                    DoorKind::BlazeRaise | DoorKind::Normal => {
                        door.direction = 0; // wait at top
                        door.topcountdown = door.topwait;
                    }
                    DoorKind::Close30ThenOpen | DoorKind::BlazeOpen | DoorKind::Open => {
                        reg.remove(id);
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    if let Some(Thinker::Door(slot)) = reg.get_mut(id) {
        *slot = door;
    }
}

fn lowest_surrounding_ceiling_minus_offset(map: &Map, sector: usize) -> Fixed {
    find_lowest_ceiling_surrounding(map, sector) - VDOOR_CEILING_OFFSET
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::FRACUNIT;
    use crate::info::MobjType;
    use crate::mobj::MobjStore;
    use crate::rng::DoomRng;
    use crate::specials::testutil::{spawn_player, spawn_thing, split_map, test_player};
    use crate::specials::{SpecialContext, SpecialEvent, run_thinkers};
    use crate::thinker::Thinker;

    fn fixed(units: i32) -> Fixed {
        Fixed::from_raw(units * FRACUNIT)
    }

    struct DoorWorld {
        map: Map,
        store: MobjStore,
        players: Vec<crate::player::Player>,
        rng: DoomRng,
    }

    fn world(north_floor: i16, north_ceil: i16, north_tag: i16) -> DoorWorld {
        DoorWorld {
            map: split_map(north_floor, north_ceil, north_tag),
            store: MobjStore::new(),
            players: vec![test_player()],
            rng: DoomRng::with_seed(0),
        }
    }

    fn ctx<'a>(w: &'a mut DoorWorld) -> SpecialContext<'a> {
        SpecialContext::new(&mut w.map, &mut w.store, &mut w.players, &mut w.rng)
    }

    fn door_thinkers(store: &MobjStore) -> Vec<VerticalDoor> {
        store
            .thinkers()
            .filter_map(|(_, t)| match t {
                Thinker::Door(d) => Some(*d),
                _ => None,
            })
            .collect()
    }

    fn tick(ctx: &mut SpecialContext<'_>, n: usize) {
        for _ in 0..n {
            run_thinkers(ctx);
        }
    }

    #[test]
    fn door_full_cycle_raise_wait_lower() {
        let mut w = world(0, 64, 7);
        // the north wall line gets the door tag so the action finds sector 1
        w.map.linedefs[2].tag = 7;
        // an avatar far away so the registry is never empty (it always is
        // in gameplay) — the door must not touch it
        spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 32, 32);
        let mut ctx = ctx(&mut w);

        // spawn a normal door
        assert!(ev_do_door(&mut ctx, 2, DoorKind::Normal));
        assert_eq!(ctx.pending_thinkers.len(), 1);
        // opening sound played at spawn (topheight 124 != current ceiling 64)
        assert!(
            ctx.events.iter().any(|e| matches!(
                e,
                SpecialEvent::SectorSound {
                    sector: 1,
                    sfx: SfxId::sfx_doropn
                }
            )),
            "expected the open sound at spawn"
        );

        // the door thinks: this pass drains it and moves 64 -> 66
        run_thinkers(&mut ctx);
        assert_eq!(door_thinkers(ctx.store).len(), 1);

        // raise to the top; direction flips to 0 (wait) at the ceiling
        for _ in 0..40 {
            run_thinkers(&mut ctx);
            if door_thinkers(ctx.store)[0].direction == 0 {
                break;
            }
        }
        assert_eq!(ctx.map.sectors[1].ceiling_height, fixed(124));
        assert_eq!(door_thinkers(ctx.store)[0].direction, 0);
        assert_eq!(door_thinkers(ctx.store)[0].topcountdown, VDOORWAIT);

        // wait out VDOORWAIT ticks -> direction flips to -1 with the close sound
        for _ in 0..VDOORWAIT as usize {
            run_thinkers(&mut ctx);
        }
        assert_eq!(door_thinkers(ctx.store)[0].direction, -1);
        assert!(
            ctx.events.iter().any(|e| matches!(
                e,
                SpecialEvent::SectorSound {
                    sector: 1,
                    sfx: SfxId::sfx_dorcls
                }
            )),
            "expected the close sound when the wait expires"
        );

        // lower until past dest; the regular door removes itself there
        for _ in 0..80 {
            run_thinkers(&mut ctx);
            if door_thinkers(ctx.store).is_empty() {
                break;
            }
        }
        assert_eq!(ctx.map.sectors[1].ceiling_height, fixed(0));
        assert_eq!(door_thinkers(ctx.store).len(), 0);
    }

    #[test]
    fn locked_door_requires_key_card() {
        let mut w = world(0, 64, 7);
        w.map.linedefs[2].tag = 99;
        w.map.linedefs[2].special = 99; // BlzOpenDoor BLUE
        w.map.sectors[1].tag = 99; // find_sector_from_line_tag scans sectors
        let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 128, 32);
        let mut ctx = ctx(&mut w);

        // no blue card: refused
        assert!(!ev_do_locked_door(&mut ctx, 2, DoorKind::BlazeOpen, avatar));
        assert!(ctx.pending_thinkers.is_empty(), "no thinker must be queued");
        assert_eq!(
            ctx.players[0].message.as_deref(),
            Some(crate::hud::HudMessage::PdBlueO.as_str())
        );
        assert!(ctx.events.iter().any(|e| matches!(
            e,
            SpecialEvent::GlobalSound {
                sfx: SfxId::sfx_oof
            }
        )));

        // with the card: opens
        ctx.players[0].cards[Card::Blue.index()] = true;
        assert!(ev_do_locked_door(&mut ctx, 2, DoorKind::BlazeOpen, avatar));
        assert_eq!(ctx.pending_thinkers.len(), 1);
    }

    #[test]
    fn locked_door_refuses_monsters() {
        let mut w = world(0, 64, 7);
        w.map.linedefs[2].tag = 99;
        w.map.linedefs[2].special = 99;
        let monster = spawn_thing(
            &mut w.store,
            &w.map,
            &mut w.rng,
            MobjType::MT_POSSESSED,
            128,
            32,
        );
        let mut ctx = ctx(&mut w);

        assert!(!ev_do_locked_door(
            &mut ctx,
            2,
            DoorKind::BlazeOpen,
            monster
        ));
        assert!(ctx.pending_thinkers.is_empty());
    }

    #[test]
    fn manual_door_raise_flips_direction_on_retrigger() {
        let mut w = world(0, 64, 7);
        // divider line: front side sector 0, back side sector 1
        w.map.linedefs[4].special = 1;
        let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 128, 32);
        let mut ctx = ctx(&mut w);

        assert!(ev_vertical_door(&mut ctx, 4, avatar));
        assert_eq!(
            ctx.map.linedefs[4].special, 1,
            "raise doors keep their special"
        );
        assert_eq!(ctx.pending_thinkers.len(), 1);

        // while it is raising, use the line again -> should queue a flip down
        assert!(ev_vertical_door(&mut ctx, 4, avatar));

        // the pass applies the flip before the door thinks
        run_thinkers(&mut ctx);
        assert_eq!(door_thinkers(ctx.store)[0].direction, -1);

        // retrigger again: door going down flips back up
        assert!(ev_vertical_door(&mut ctx, 4, avatar));
        run_thinkers(&mut ctx);
        assert_eq!(door_thinkers(ctx.store)[0].direction, 1);
    }

    #[test]
    fn one_way_open_door_clears_its_special() {
        let mut w = world(0, 64, 7);
        w.map.linedefs[4].special = 31; // manual open door
        let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 128, 32);
        let mut ctx = ctx(&mut w);

        assert!(ev_vertical_door(&mut ctx, 4, avatar));
        assert_eq!(ctx.map.linedefs[4].special, 0);
        let Thinker::Door(door) = ctx.pending_thinkers[0] else {
            panic!("expected a door thinker");
        };
        assert_eq!(
            door,
            VerticalDoor {
                kind: DoorKind::Open,
                sector: 1,
                topheight: fixed(124),
                speed: VDOORSPEED,
                direction: 1,
                topwait: VDOORWAIT,
                topcountdown: 0,
            }
        );
    }

    #[test]
    fn blaze_door_moves_four_times_faster() {
        let mut w = world(0, 64, 7);
        w.map.linedefs[4].special = 117; // blazing door raise
        let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 128, 32);
        let mut ctx = ctx(&mut w);

        assert!(ev_vertical_door(&mut ctx, 4, avatar));
        let Thinker::Door(door) = ctx.pending_thinkers[0] else {
            panic!("expected a door thinker");
        };
        assert_eq!(door.speed, Fixed::from_raw(VDOORSPEED.to_raw() * 4));
    }

    #[test]
    fn door_bounces_off_a_monster_it_would_crush() {
        let mut w = world(0, 64, 7);
        let monster = spawn_thing(
            &mut w.store,
            &w.map,
            &mut w.rng,
            MobjType::MT_POSSESSED,
            128,
            192,
        );
        let _ = monster;
        w.map.linedefs[2].tag = 7;
        let mut ctx = ctx(&mut w);

        // close30ThenOpen bounces off a crushing obstacle; a plain Close
        // door would never go back up (vanilla's "DO NOT GO BACK UP!")
        assert!(ev_do_door(&mut ctx, 2, DoorKind::Close30ThenOpen));
        // the door starts at ceiling 64 and closes by 2 each tic
        let mut bounced = false;
        for _ in 0..64 {
            run_thinkers(&mut ctx);
            if door_thinkers(ctx.store)[0].direction == 1 {
                bounced = true;
                break;
            }
        }
        assert!(
            bounced,
            "door should bounce off the monster, not crush to the floor"
        );
        assert!(
            ctx.events.iter().any(|e| matches!(
                e,
                SpecialEvent::SectorSound {
                    sector: 1,
                    sfx: SfxId::sfx_doropn
                }
            )),
            "expected the reopen sound"
        );
        // ceiling must not have reached the floor
        assert!(ctx.map.sectors[1].ceiling_height > fixed(0));
    }

    #[test]
    fn plain_close_door_does_not_bounce_off_a_living_thing() {
        let mut w = world(0, 64, 7);
        let monster = spawn_thing(
            &mut w.store,
            &w.map,
            &mut w.rng,
            MobjType::MT_POSSESSED,
            128,
            192,
        );
        w.map.linedefs[2].tag = 7;
        let mut ctx = ctx(&mut w);

        assert!(ev_do_door(&mut ctx, 2, DoorKind::Close));
        // doors never crush-damage in vanilla (crush is false for doors):
        // a Close door stops at the height where the thing last fit
        // instead of reversing or damaging it
        for _ in 0..64 {
            run_thinkers(&mut ctx);
        }
        // possessed is 56 units tall, floor 0: the door holds at 56
        assert_eq!(ctx.map.sectors[1].ceiling_height, fixed(56));
        assert!(
            !ctx.events.iter().any(|e| matches!(
                e,
                SpecialEvent::SectorSound {
                    sector: 1,
                    sfx: SfxId::sfx_doropn
                }
            )),
            "close door must not reopen"
        );
        assert!(
            !ctx.events
                .iter()
                .any(|e| matches!(e, SpecialEvent::Damage { .. })),
            "doors never crush-damage"
        );
        assert_eq!(
            door_thinkers(ctx.store)[0].direction,
            -1,
            "close door must not flip"
        );
        let hp = ctx.store.mobj(monster).map(|m| m.health);
        assert_eq!(hp, Some(20), "the monster is untouched");
    }

    #[test]
    fn door_gibs_a_corpse_instead_of_bouncing() {
        let mut w = world(0, 64, 7);
        let monster = spawn_thing(
            &mut w.store,
            &w.map,
            &mut w.rng,
            MobjType::MT_POSSESSED,
            128,
            192,
        );
        if let Some(m) = w.store.mobj_mut(monster) {
            m.health = 0;
        }
        w.map.linedefs[2].tag = 7;
        let mut ctx = ctx(&mut w);

        assert!(ev_do_door(&mut ctx, 2, DoorKind::Close));
        for _ in 0..64 {
            run_thinkers(&mut ctx);
        }
        assert_eq!(
            ctx.map.sectors[1].ceiling_height,
            fixed(0),
            "door closes right through the corpse"
        );
        let gibbed = ctx
            .events
            .iter()
            .any(|e| matches!(e, SpecialEvent::GibCorpse { id, .. } if *id == monster));
        assert!(gibbed, "expected the corpse to gib under the crushing door");
        // close door removes itself silently at the floor
        assert_eq!(door_thinkers(ctx.store).len(), 0);
    }

    #[test]
    fn close_30_then_open_cycle() {
        let mut w = world(0, 64, 7);
        w.map.linedefs[2].tag = 7;
        let mut ctx = ctx(&mut w);

        assert!(ev_do_door(&mut ctx, 2, DoorKind::Close30ThenOpen));
        // 64 -> 0 takes 32 movement tics, plus one tic to notice
        // pastdest and flip to the 30-second (1050 tic) wait
        run_thinkers(&mut ctx); // drain pending + first tic of closing
        tick(&mut ctx, 32);
        assert_eq!(ctx.map.sectors[1].ceiling_height, fixed(0));
        assert_eq!(door_thinkers(ctx.store)[0].direction, 0);
        assert_eq!(door_thinkers(ctx.store)[0].topcountdown, 35 * 30);

        // after the wait elapses it opens again
        tick(&mut ctx, 35 * 30);
        assert_eq!(door_thinkers(ctx.store)[0].direction, 1);

        // rises to the top and removes itself there (one-way)
        tick(&mut ctx, 64);
        assert_eq!(door_thinkers(ctx.store).len(), 0);
    }

    #[test]
    fn raise_in_5_mins_fires_after_countdown() {
        let mut w = world(0, 64, 7);
        let mut ctx = ctx(&mut w);
        ctx.pending_thinkers.push(Thinker::Door(VerticalDoor {
            kind: DoorKind::RaiseIn5Mins,
            sector: 1,
            topheight: fixed(124),
            speed: VDOORSPEED,
            direction: 2,
            topwait: VDOORWAIT,
            topcountdown: 5,
        }));

        tick(&mut ctx, 4);
        assert_eq!(door_thinkers(ctx.store)[0].direction, 2);
        tick(&mut ctx, 1);
        assert_eq!(door_thinkers(ctx.store)[0].direction, 1);
        assert_eq!(door_thinkers(ctx.store)[0].kind, DoorKind::Normal);
        assert!(
            ctx.events.iter().any(|e| matches!(
                e,
                SpecialEvent::SectorSound {
                    sector: 1,
                    sfx: SfxId::sfx_doropn
                }
            )),
            "expected the open sound when the delay expires"
        );
    }

    #[test]
    fn spawning_a_second_door_on_a_busy_sector_is_skipped() {
        let mut w = world(0, 64, 7);
        w.map.linedefs[2].tag = 7;
        let mut ctx = ctx(&mut w);

        assert!(ev_do_door(&mut ctx, 2, DoorKind::Normal));
        // same sector again: skipped while the first door is queued/active
        assert!(!ev_do_door(&mut ctx, 2, DoorKind::Normal));
        assert_eq!(ctx.pending_thinkers.len(), 1);

        run_thinkers(&mut ctx);
        // and after the door is live in the registry
        assert!(!ev_do_door(&mut ctx, 2, DoorKind::Normal));
        assert_eq!(door_thinkers(ctx.store).len(), 1);
    }
}
