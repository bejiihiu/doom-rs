//! Elevator platforms, ported from `p_plats.c`.
//!
//! [`Plat`] is the vanilla `plat_t`; [`run_plat`] is `T_PlatRaise`.
//! [`ev_do_plat`] starts the tagged plats moving and [`ev_stop_plat`]
//! freezes them mid-move (vanilla `EV_StopPlat`).
//!
//! ## Stasis
//!
//! Plats in stasis do nothing every tic (vanilla nulled the thinker's
//! function pointer). A `perpetualRaise` action re-activates plats in
//! stasis with the same tag before spawning new ones
//! (`P_ActivateInStasis`), and `EV_StopPlat` parks the tagged plats.
//! Both are registry scans over [`crate::thinker::Thinker::Plat`].
//!
//! ## Capacity
//!
//! Vanilla bounded `activeplats[]` to 30 with an `I_Error` on overflow.
//! The thinker registry here is dynamic, so there is no fixed cap; more
//! than 30 live plats simply work. The bound existed to keep the stasis
//! scans small, which is not a constraint at this scale.

use crate::fixed::{FRACUNIT, Fixed};
use crate::info::SfxId;
use crate::specials::util::{
    find_highest_floor_surrounding, find_lowest_floor_surrounding, find_next_highest_floor,
    find_sector_from_line_tag,
};
use crate::specials::{PassCtx, SpecialContext, SpecialEvent, sector_busy};
use crate::thinker::{Thinker, ThinkerId, ThinkerRegistry};

use super::movers::{MoveOutcome, PlaneKind, move_plane};

/// Vanilla `PLATSPEED`.
pub const PLATSPEED: Fixed = Fixed::from_raw(FRACUNIT);
/// Vanilla `PLATWAIT` (in seconds; multiplied by 35 at the call sites).
pub const PLATWAIT: i32 = 3;
const WAIT_TICS: i32 = 35 * PLATWAIT;

/// Vanilla `plattype_e`. Discriminants follow the original order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlatKind {
    /// Endless up/down cycle between the lowest and highest surrounding
    /// floors.
    PerpetualRaise = 0,
    /// Go down to the lowest surrounding floor, wait, come back up, stay.
    DownWaitUpStay = 1,
    /// Raise to `base + amount` and change the flat to the line's front
    /// sector's (vanilla's `amount` comes from the line special).
    RaiseAndChange = 2,
    /// Raise to the next highest surrounding floor and change the flat.
    RaiseToNearestAndChange = 3,
    /// Like [`PlatKind::DownWaitUpStay`], eight times faster.
    BlazeDWUS = 4,
}

/// Vanilla `plat_e`. Discriminants follow the original order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlatStatus {
    /// Moving up.
    Up = 0,
    /// Moving down.
    Down = 1,
    /// Waiting at an end of the travel.
    Waiting = 2,
    /// Frozen by `EV_StopPlat`; resume via a `perpetualRaise` action.
    InStasis = 3,
}

/// Vanilla `plat_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Plat {
    /// Which platform action this is.
    pub kind: PlatKind,
    /// Index of the moving sector.
    pub sector: usize,
    /// Movement speed per tic.
    pub speed: Fixed,
    /// Lower end of the travel.
    pub low: Fixed,
    /// Upper end of the travel.
    pub high: Fixed,
    /// Ticks to wait at an end before turning around.
    pub wait: i32,
    /// Ticks left at the current end.
    pub count: i32,
    /// Where the plat is going.
    pub status: PlatStatus,
    /// Status to resume from stasis.
    pub oldstatus: PlatStatus,
    /// Vanilla `crush`; plats never set it, but the field is carried for
    /// 1:1 parity with `plat_t`.
    pub crush: bool,
    /// Vanilla `tag` — the line tag that spawned the plat (stasis scans
    /// fan out on it).
    pub tag: i16,
}

/// Vanilla `T_PlatRaise`: the per-tic step of a [`Plat`].
pub(crate) fn run_plat(reg: &mut ThinkerRegistry, id: ThinkerId, ctx: &mut PassCtx<'_>) {
    let mut plat = match reg.get(id) {
        Some(Thinker::Plat(plat)) => *plat,
        _ => return,
    };

    match plat.status {
        PlatStatus::Up => {
            let res = move_plane(
                reg,
                ctx,
                plat.sector,
                plat.speed,
                plat.high,
                plat.crush,
                PlaneKind::Floor,
                1,
            );

            if (plat.kind == PlatKind::RaiseAndChange
                || plat.kind == PlatKind::RaiseToNearestAndChange)
                && (ctx.leveltime & 7) == 0
            {
                ctx.events.push(SpecialEvent::SectorSound {
                    sector: plat.sector,
                    sfx: SfxId::sfx_stnmov,
                });
            }

            if res == MoveOutcome::Crushed && !plat.crush {
                plat.count = plat.wait;
                plat.status = PlatStatus::Down;
                ctx.events.push(SpecialEvent::SectorSound {
                    sector: plat.sector,
                    sfx: SfxId::sfx_pstart,
                });
            } else if res == MoveOutcome::PastDest {
                plat.count = plat.wait;
                plat.status = PlatStatus::Waiting;
                ctx.events.push(SpecialEvent::SectorSound {
                    sector: plat.sector,
                    sfx: SfxId::sfx_pstop,
                });

                match plat.kind {
                    PlatKind::BlazeDWUS
                    | PlatKind::DownWaitUpStay
                    | PlatKind::RaiseAndChange
                    | PlatKind::RaiseToNearestAndChange => {
                        reg.remove(id);
                        return;
                    }
                    PlatKind::PerpetualRaise => {}
                }
            }
        }
        PlatStatus::Down => {
            let res = move_plane(
                reg,
                ctx,
                plat.sector,
                plat.speed,
                plat.low,
                false,
                PlaneKind::Floor,
                -1,
            );

            if res == MoveOutcome::PastDest {
                plat.count = plat.wait;
                plat.status = PlatStatus::Waiting;
                ctx.events.push(SpecialEvent::SectorSound {
                    sector: plat.sector,
                    sfx: SfxId::sfx_pstop,
                });
            }
        }
        PlatStatus::Waiting => {
            plat.count -= 1;
            if plat.count == 0 {
                plat.status = if ctx.map.sectors[plat.sector].floor_height == plat.low {
                    PlatStatus::Up
                } else {
                    PlatStatus::Down
                };
                ctx.events.push(SpecialEvent::SectorSound {
                    sector: plat.sector,
                    sfx: SfxId::sfx_pstart,
                });
            }
        }
        PlatStatus::InStasis => {}
    }

    if let Some(Thinker::Plat(slot)) = reg.get_mut(id) {
        *slot = plat;
    }
}

/// Vanilla `P_ActivateInStasis`: resume the plats in stasis with the
/// given tag, back to whatever status they had when frozen.
fn activate_in_stasis(store: &mut crate::mobj::MobjStore, pending: &mut [Thinker], tag: i16) {
    super::for_each_thinker_mut(store, pending, |thinker| {
        if let Thinker::Plat(plat) = thinker {
            if plat.tag == tag && plat.status == PlatStatus::InStasis {
                plat.status = plat.oldstatus;
            }
        }
    });
}

/// Vanilla `EV_DoPlat`: start every tagged sector's plat. `amount` is
/// only used by [`PlatKind::RaiseAndChange`] (the line special passes 24
/// or 32). Returns true when at least one plat was started.
pub fn ev_do_plat(ctx: &mut SpecialContext, line: usize, kind: PlatKind, amount: i32) -> bool {
    let tag = ctx.map.linedefs[line].tag;

    // restart frozen plats with the same tag first (the perpetual raise
    // trigger doubles as a wake-up call)
    if kind == PlatKind::PerpetualRaise {
        activate_in_stasis(ctx.store, &mut ctx.pending_thinkers, tag);
    }

    let mut rtn = false;
    let mut cursor = None;

    while let Some(secnum) = find_sector_from_line_tag(ctx.map, tag, cursor) {
        cursor = Some(secnum);
        if sector_busy(ctx.store, &ctx.pending_thinkers, secnum) {
            continue;
        }
        rtn = true;

        let mut plat = Plat {
            kind,
            sector: secnum,
            speed: PLATSPEED,
            low: Fixed::ZERO,
            high: Fixed::ZERO,
            wait: 0,
            count: 0,
            status: PlatStatus::Up,
            oldstatus: PlatStatus::Up,
            crush: false,
            tag,
        };

        // the front side's sector flat is what `raiseAndChange` copies in
        let front_pic = ctx.map.linedefs[line].sidenum[0]
            .map(|side| ctx.map.sectors[ctx.map.sidedefs[side].sector].floor_pic);

        match kind {
            PlatKind::RaiseToNearestAndChange => {
                plat.speed = Fixed::from_raw(FRACUNIT / 2);
                match front_pic {
                    Some(pic) => ctx.map.sectors[secnum].floor_pic = pic,
                    None => {
                        tracing::warn!(
                            "raiseToNearestAndChange: line {line} has no front side; flat copy skipped"
                        );
                    }
                }
                plat.high =
                    find_next_highest_floor(ctx.map, secnum, ctx.map.sectors[secnum].floor_height);
                plat.wait = 0;
                plat.status = PlatStatus::Up;
                // no more damage, if applicable
                ctx.map.sectors[secnum].special = 0;

                ctx.events.push(SpecialEvent::SectorSound {
                    sector: secnum,
                    sfx: SfxId::sfx_stnmov,
                });
            }
            PlatKind::RaiseAndChange => {
                plat.speed = Fixed::from_raw(FRACUNIT / 2);
                match front_pic {
                    Some(pic) => ctx.map.sectors[secnum].floor_pic = pic,
                    None => {
                        tracing::warn!(
                            "raiseAndChange: line {line} has no front side; flat copy skipped"
                        );
                    }
                }
                plat.high =
                    ctx.map.sectors[secnum].floor_height + Fixed::from_raw(amount * FRACUNIT);
                plat.wait = 0;
                plat.status = PlatStatus::Up;

                ctx.events.push(SpecialEvent::SectorSound {
                    sector: secnum,
                    sfx: SfxId::sfx_stnmov,
                });
            }
            PlatKind::DownWaitUpStay => {
                plat.speed = Fixed::from_raw(PLATSPEED.to_raw() * 4);
                plat.low = find_lowest_floor_surrounding(ctx.map, secnum);
                if plat.low > ctx.map.sectors[secnum].floor_height {
                    plat.low = ctx.map.sectors[secnum].floor_height;
                }
                plat.high = ctx.map.sectors[secnum].floor_height;
                plat.wait = WAIT_TICS;
                plat.count = plat.wait;
                plat.status = PlatStatus::Down;

                ctx.events.push(SpecialEvent::SectorSound {
                    sector: secnum,
                    sfx: SfxId::sfx_pstart,
                });
            }
            PlatKind::BlazeDWUS => {
                plat.speed = Fixed::from_raw(PLATSPEED.to_raw() * 8);
                plat.low = find_lowest_floor_surrounding(ctx.map, secnum);
                if plat.low > ctx.map.sectors[secnum].floor_height {
                    plat.low = ctx.map.sectors[secnum].floor_height;
                }
                plat.high = ctx.map.sectors[secnum].floor_height;
                plat.wait = WAIT_TICS;
                plat.count = plat.wait;
                plat.status = PlatStatus::Down;

                ctx.events.push(SpecialEvent::SectorSound {
                    sector: secnum,
                    sfx: SfxId::sfx_pstart,
                });
            }
            PlatKind::PerpetualRaise => {
                plat.speed = PLATSPEED;
                plat.low = find_lowest_floor_surrounding(ctx.map, secnum);
                if plat.low > ctx.map.sectors[secnum].floor_height {
                    plat.low = ctx.map.sectors[secnum].floor_height;
                }
                plat.high = find_highest_floor_surrounding(ctx.map, secnum);
                if plat.high < ctx.map.sectors[secnum].floor_height {
                    plat.high = ctx.map.sectors[secnum].floor_height;
                }
                plat.wait = WAIT_TICS;
                // vanilla rolls P_Random()&1 for the starting direction
                plat.status = if ctx.rng.next() & 1 != 0 {
                    PlatStatus::Down
                } else {
                    PlatStatus::Up
                };
                plat.oldstatus = PlatStatus::Waiting;

                ctx.events.push(SpecialEvent::SectorSound {
                    sector: secnum,
                    sfx: SfxId::sfx_pstart,
                });
            }
        }

        ctx.pending_thinkers.push(Thinker::Plat(plat));
    }

    rtn
}

/// Vanilla `EV_StopPlat`: freeze every plat with the line's tag that is
/// not already in stasis. Returns true when at least one plat was
/// frozen.
pub fn ev_stop_plat(ctx: &mut SpecialContext, line: usize) -> bool {
    let tag = ctx.map.linedefs[line].tag;
    let mut any = false;

    super::for_each_thinker_mut(ctx.store, &mut ctx.pending_thinkers, |thinker| {
        if let Thinker::Plat(plat) = thinker {
            if plat.tag == tag && plat.status != PlatStatus::InStasis {
                plat.oldstatus = plat.status;
                plat.status = PlatStatus::InStasis;
                any = true;
            }
        }
    });

    any
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::FRACUNIT;
    use crate::map::Map;
    use crate::mobj::MobjStore;
    use crate::rng::DoomRng;
    use crate::specials::testutil::{split_map, split_map_custom, test_player};
    use crate::specials::{SpecialContext, run_thinkers};
    use crate::thinker::Thinker;

    fn fixed(units: i32) -> Fixed {
        Fixed::from_raw(units * FRACUNIT)
    }

    struct World {
        map: Map,
        store: MobjStore,
        players: Vec<crate::player::Player>,
        rng: DoomRng,
    }

    fn world(north_floor: i16, north_ceil: i16, north_tag: i16) -> World {
        World {
            map: split_map(north_floor, north_ceil, north_tag),
            store: MobjStore::new(),
            players: vec![test_player()],
            rng: DoomRng::with_seed(0),
        }
    }

    fn ctx<'a>(w: &'a mut World) -> SpecialContext<'a> {
        SpecialContext::new(&mut w.map, &mut w.store, &mut w.players, &mut w.rng)
    }

    fn tick(ctx: &mut SpecialContext<'_>, n: usize) {
        for _ in 0..n {
            run_thinkers(ctx);
        }
    }

    fn plats(store: &MobjStore) -> Vec<Plat> {
        store
            .thinkers()
            .filter_map(|(_, t)| match t {
                Thinker::Plat(p) => Some(*p),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn down_wait_up_stay_full_cycle() {
        let mut w = world(32, 128, 7);
        w.map.linedefs[4].tag = 7; // the divider triggers the plat
        let mut ctx = ctx(&mut w);

        assert!(ev_do_plat(&mut ctx, 4, PlatKind::DownWaitUpStay, 0));
        // speed 4: 32 units down in 8 tics
        tick(&mut ctx, 10);
        assert_eq!(ctx.map.sectors[1].floor_height, fixed(0));
        assert_eq!(plats(ctx.store)[0].status, PlatStatus::Waiting);

        // wait out PLATWAIT*35 tics, then it rises again and removes itself
        tick(&mut ctx, 35 * PLATWAIT as usize + 30);
        assert!(
            ctx.map.sectors[1].floor_height > fixed(0),
            "plat must have risen past 0 after the wait expired"
        );
        assert!(
            plats(ctx.store).is_empty(),
            "downWaitUpStay removes itself at the top"
        );
        assert_eq!(ctx.map.sectors[1].floor_height, fixed(32));
        assert!(ctx.events.iter().any(|e| matches!(
            e,
            SpecialEvent::SectorSound {
                sector: 1,
                sfx: SfxId::sfx_pstop
            }
        )));
    }

    #[test]
    fn raise_and_change_copies_the_front_flat_and_adds_amount() {
        let mut map = split_map_custom("SOUTHPIC", 0, "FLOOR4_8", 0);
        map.sectors[1].tag = 7;
        map.linedefs[4].tag = 7;
        let mut w = World {
            map,
            store: MobjStore::new(),
            players: vec![test_player()],
            rng: DoomRng::with_seed(0),
        };
        let mut ctx = ctx(&mut w);

        assert!(ev_do_plat(&mut ctx, 4, PlatKind::RaiseAndChange, 24));
        tick(&mut ctx, 120);
        assert_eq!(ctx.map.sectors[1].floor_height, fixed(32 + 24));
        assert_eq!(ctx.map.sectors[1].floor_pic, name8("SOUTHPIC"));
        // one-way: the thinker removes itself at the top
        assert!(plats(ctx.store).is_empty());
    }

    fn name8(name: &str) -> [u8; 8] {
        let mut out = [0u8; 8];
        for (dst, src) in out.iter_mut().zip(name.as_bytes().iter().take(8)) {
            *dst = src.to_ascii_uppercase();
        }
        out
    }

    #[test]
    fn perpetual_plat_stops_and_resumes_with_the_same_tag() {
        let mut w = world(0, 128, 7);
        // south floor is higher: the plat travels 0..64 and starts moving up
        w.map.sectors[0].floor_height = fixed(64);
        w.map.linedefs[4].tag = 7;
        let mut ctx = ctx(&mut w);

        assert!(ev_do_plat(&mut ctx, 4, PlatKind::PerpetualRaise, 0));
        tick(&mut ctx, 1); // drain pending thinkers into the registry
        assert_eq!(plats(ctx.store).len(), 1);

        // let it move a little, then freeze it
        tick(&mut ctx, 3);
        assert!(ev_stop_plat(&mut ctx, 4));
        assert_eq!(plats(ctx.store)[0].status, PlatStatus::InStasis);
        let frozen_floor = ctx.map.sectors[1].floor_height;
        tick(&mut ctx, 30);
        assert_eq!(
            ctx.map.sectors[1].floor_height, frozen_floor,
            "a plat in stasis must not move"
        );

        // the perpetual raise action doubles as a wake-up call; the
        // resumed plat counts as busy, so no *new* plat was started
        // (vanilla returns 0 here too)
        assert!(!ev_do_plat(&mut ctx, 4, PlatKind::PerpetualRaise, 0));
        let status = plats(ctx.store)[0].status;
        assert!(
            status != PlatStatus::InStasis,
            "stasis platforms must resume on re-trigger"
        );
        tick(&mut ctx, 3);
        assert_ne!(
            ctx.map.sectors[1].floor_height, frozen_floor,
            "a resumed plat must move again"
        );
    }

    #[test]
    fn busy_sector_skips_the_plat() {
        let mut w = world(32, 128, 7);
        w.map.linedefs[4].tag = 7;
        w.map.linedefs[2].tag = 7;
        let mut ctx = ctx(&mut w);

        // an active door makes the sector busy for the plat too
        assert!(crate::specials::ev_do_door(
            &mut ctx,
            2,
            crate::specials::DoorKind::Normal
        ));
        assert!(!ev_do_plat(&mut ctx, 4, PlatKind::DownWaitUpStay, 0));
    }

    #[test]
    fn blaze_dwus_is_eight_times_plat_speed() {
        let mut w = world(32, 128, 7);
        w.map.linedefs[4].tag = 7;
        let mut ctx = ctx(&mut w);

        assert!(ev_do_plat(&mut ctx, 4, PlatKind::BlazeDWUS, 0));
        tick(&mut ctx, 1);
        assert_eq!(ctx.map.sectors[1].floor_height, fixed(32 - 8));
    }
}
