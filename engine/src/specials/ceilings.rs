//! Ceiling movers and crushers, ported from `p_ceilng.c`.
//!
//! [`Ceiling`] is the vanilla `ceiling_t`; [`run_ceiling`] is
//! `T_MoveCeiling`. [`ev_do_ceiling`] starts the tagged ceilings
//! moving, [`ev_ceiling_crush_stop`] freezes them (vanilla
//! `EV_CeilingCrushStop`).
//!
//! ## Stasis
//!
//! Ceilings in stasis do nothing every tic (vanilla nulled the thinker's
//! function pointer). The crushing ceiling actions (`crushAndRaise`,
//! `fastCrushAndRaise`, `silentCrushAndRaise`) re-activate ceilings in
//! stasis with the same tag before spawning new ones
//! (`P_ActivateInStasisCeiling`), and `EV_CeilingCrushStop` parks the
//! tagged ceilings. Both are 1:1 with vanilla; the frozen thinkers
//! resume in their old direction.
//!
//! ## Capacity
//!
//! Vanilla bounded `activeceilings[]` to 30; the thinker registry here
//! is dynamic, so there is no cap (same reasoning as the plats).

use crate::fixed::{FRACUNIT, Fixed};
use crate::info::SfxId;
use crate::specials::util::{find_highest_ceiling_surrounding, find_sector_from_line_tag};
use crate::specials::{PassCtx, SpecialContext, SpecialEvent, sector_busy};
use crate::thinker::{Thinker, ThinkerId, ThinkerRegistry};

use super::movers::{MoveOutcome, PlaneKind, move_plane};

/// Vanilla `CEILSPEED`.
pub const CEILSPEED: Fixed = Fixed::from_raw(FRACUNIT);
const EIGHT_UNITS: Fixed = Fixed::from_raw(8 * FRACUNIT);

/// Vanilla `ceiling_e`. Discriminants follow the original order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CeilingKind {
    /// Lower ceiling to floor, remove the thinker on arrival.
    LowerToFloor = 0,
    /// Raise ceiling to the highest surrounding ceiling.
    RaiseToHighest = 1,
    /// Lower and crush; halves the speed when crushing (`CEILSPEED/8`).
    LowerAndCrush = 2,
    /// Crush and raise forever.
    CrushAndRaise = 3,
    /// Fast crush and raise (`CEILSPEED*2`).
    FastCrushAndRaise = 4,
    /// Like [`CeilingKind::CrushAndRaise`] but never makes noise except
    /// for the pstop at the ends of the crush.
    SilentCrushAndRaise = 5,
}

/// Vanilla `ceiling_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ceiling {
    /// Which ceiling action this is.
    pub kind: CeilingKind,
    /// Index of the moving sector.
    pub sector: usize,
    /// Lower end of the travel.
    pub bottomheight: Fixed,
    /// Upper end of the travel.
    pub topheight: Fixed,
    /// Movement speed per tic.
    pub speed: Fixed,
    /// True for the crushing kinds: things in the way get hurt.
    pub crush: bool,
    /// Vanilla `direction`: 1 = up, 0 = in stasis, -1 = down.
    pub direction: i32,
    /// Vanilla `tag` — the *sector* tag, captured at spawn (stasis
    /// scans fan out on it, like vanilla).
    pub tag: i16,
    /// Direction to resume from stasis.
    pub olddirection: i32,
}

/// Vanilla `T_MoveCeiling`: the per-tic step of a [`Ceiling`].
pub(crate) fn run_ceiling(reg: &mut ThinkerRegistry, id: ThinkerId, ctx: &mut PassCtx<'_>) {
    let mut ceiling = match reg.get(id) {
        Some(Thinker::Ceiling(ceiling)) => *ceiling,
        _ => return,
    };

    match ceiling.direction {
        0 => {
            // IN STASIS
        }
        1 => {
            // UP
            let res = move_plane(
                reg,
                ctx,
                ceiling.sector,
                ceiling.speed,
                ceiling.topheight,
                false,
                PlaneKind::Ceiling,
                ceiling.direction,
            );

            if (ctx.leveltime & 7) == 0 && ceiling.kind != CeilingKind::SilentCrushAndRaise {
                ctx.events.push(SpecialEvent::SectorSound {
                    sector: ceiling.sector,
                    sfx: SfxId::sfx_stnmov,
                });
            }

            if res == MoveOutcome::PastDest {
                match ceiling.kind {
                    CeilingKind::RaiseToHighest => {
                        reg.remove(id);
                        return;
                    }
                    CeilingKind::SilentCrushAndRaise => {
                        ctx.events.push(SpecialEvent::SectorSound {
                            sector: ceiling.sector,
                            sfx: SfxId::sfx_pstop,
                        });
                        ceiling.direction = -1;
                    }
                    CeilingKind::FastCrushAndRaise | CeilingKind::CrushAndRaise => {
                        ceiling.direction = -1;
                    }
                    CeilingKind::LowerToFloor | CeilingKind::LowerAndCrush => {}
                }
            }
        }
        -1 => {
            // DOWN
            let res = move_plane(
                reg,
                ctx,
                ceiling.sector,
                ceiling.speed,
                ceiling.bottomheight,
                ceiling.crush,
                PlaneKind::Ceiling,
                ceiling.direction,
            );

            if (ctx.leveltime & 7) == 0 && ceiling.kind != CeilingKind::SilentCrushAndRaise {
                ctx.events.push(SpecialEvent::SectorSound {
                    sector: ceiling.sector,
                    sfx: SfxId::sfx_stnmov,
                });
            }

            if res == MoveOutcome::PastDest {
                match ceiling.kind {
                    CeilingKind::SilentCrushAndRaise => {
                        ctx.events.push(SpecialEvent::SectorSound {
                            sector: ceiling.sector,
                            sfx: SfxId::sfx_pstop,
                        });
                        ceiling.speed = CEILSPEED;
                        ceiling.direction = 1;
                    }
                    CeilingKind::CrushAndRaise => {
                        ceiling.speed = CEILSPEED;
                        ceiling.direction = 1;
                    }
                    CeilingKind::FastCrushAndRaise => {
                        ceiling.direction = 1;
                    }
                    CeilingKind::LowerAndCrush | CeilingKind::LowerToFloor => {
                        reg.remove(id);
                        return;
                    }
                    CeilingKind::RaiseToHighest => {}
                }
            } else if res == MoveOutcome::Crushed {
                match ceiling.kind {
                    CeilingKind::SilentCrushAndRaise
                    | CeilingKind::CrushAndRaise
                    | CeilingKind::LowerAndCrush => {
                        ceiling.speed = Fixed::from_raw(CEILSPEED.to_raw() / 8);
                    }
                    CeilingKind::FastCrushAndRaise
                    | CeilingKind::LowerToFloor
                    | CeilingKind::RaiseToHighest => {}
                }
            }
        }
        _ => {}
    }

    if let Some(Thinker::Ceiling(slot)) = reg.get_mut(id) {
        *slot = ceiling;
    }
}

/// Vanilla `P_ActivateInStasisCeiling`: resume the ceilings in stasis
/// with the line's tag, back to whatever direction they had when
/// frozen.
fn activate_in_stasis_ceilings(
    store: &mut crate::mobj::MobjStore,
    pending: &mut [Thinker],
    tag: i16,
) {
    super::for_each_thinker_mut(store, pending, |thinker| {
        if let Thinker::Ceiling(ceiling) = thinker {
            if ceiling.tag == tag && ceiling.direction == 0 {
                ceiling.direction = ceiling.olddirection;
            }
        }
    });
}

/// Vanilla `EV_DoCeiling`: start every tagged sector's ceiling moving.
/// Returns true when at least one mover was started.
pub fn ev_do_ceiling(ctx: &mut SpecialContext, line: usize, kind: CeilingKind) -> bool {
    let tag = ctx.map.linedefs[line].tag;

    // reactivate frozen ceilings of the crushing kinds first
    match kind {
        CeilingKind::FastCrushAndRaise
        | CeilingKind::SilentCrushAndRaise
        | CeilingKind::CrushAndRaise => {
            activate_in_stasis_ceilings(ctx.store, &mut ctx.pending_thinkers, tag);
        }
        CeilingKind::LowerToFloor | CeilingKind::RaiseToHighest | CeilingKind::LowerAndCrush => {}
    }

    let mut rtn = false;
    let mut cursor = None;

    while let Some(secnum) = find_sector_from_line_tag(ctx.map, tag, cursor) {
        cursor = Some(secnum);
        if sector_busy(ctx.store, &ctx.pending_thinkers, secnum) {
            continue;
        }
        rtn = true;

        let mut ceiling = Ceiling {
            kind,
            sector: secnum,
            bottomheight: Fixed::ZERO,
            topheight: Fixed::ZERO,
            speed: CEILSPEED,
            crush: false,
            direction: -1,
            tag: ctx.map.sectors[secnum].tag,
            olddirection: -1,
        };

        match kind {
            CeilingKind::FastCrushAndRaise => {
                ceiling.crush = true;
                ceiling.topheight = ctx.map.sectors[secnum].ceiling_height;
                ceiling.bottomheight = ctx.map.sectors[secnum].floor_height + EIGHT_UNITS;
                ceiling.direction = -1;
                ceiling.speed = Fixed::from_raw(CEILSPEED.to_raw() * 2);
            }
            CeilingKind::SilentCrushAndRaise | CeilingKind::CrushAndRaise => {
                ceiling.crush = true;
                ceiling.topheight = ctx.map.sectors[secnum].ceiling_height;
                ceiling.bottomheight = ctx.map.sectors[secnum].floor_height + EIGHT_UNITS;
                ceiling.direction = -1;
                ceiling.speed = CEILSPEED;
            }
            CeilingKind::LowerAndCrush => {
                ceiling.bottomheight = ctx.map.sectors[secnum].floor_height + EIGHT_UNITS;
                ceiling.direction = -1;
                ceiling.speed = CEILSPEED;
            }
            CeilingKind::LowerToFloor => {
                ceiling.bottomheight = ctx.map.sectors[secnum].floor_height;
                ceiling.direction = -1;
                ceiling.speed = CEILSPEED;
            }
            CeilingKind::RaiseToHighest => {
                ceiling.topheight = find_highest_ceiling_surrounding(ctx.map, secnum);
                ceiling.direction = 1;
                ceiling.speed = CEILSPEED;
            }
        }

        ctx.pending_thinkers.push(Thinker::Ceiling(ceiling));
    }

    rtn
}

/// Vanilla `EV_CeilingCrushStop`: freeze every ceiling with the line's
/// tag that is not already in stasis. Returns true when at least one
/// ceiling was frozen.
pub fn ev_ceiling_crush_stop(ctx: &mut SpecialContext, line: usize) -> bool {
    let tag = ctx.map.linedefs[line].tag;
    let mut rtn = false;

    super::for_each_thinker_mut(ctx.store, &mut ctx.pending_thinkers, |thinker| {
        if let Thinker::Ceiling(ceiling) = thinker {
            if ceiling.tag == tag && ceiling.direction != 0 {
                ceiling.olddirection = ceiling.direction;
                ceiling.direction = 0;
                rtn = true;
            }
        }
    });

    rtn
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::FRACUNIT;
    use crate::info::MobjType;
    use crate::map::Map;
    use crate::mobj::MobjStore;
    use crate::rng::DoomRng;
    use crate::specials::testutil::{spawn_thing, split_map, test_player};
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

    fn ceilings(store: &MobjStore) -> Vec<Ceiling> {
        store
            .thinkers()
            .filter_map(|(_, t)| match t {
                Thinker::Ceiling(c) => Some(*c),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn crusher_descends_crushes_and_comes_back_up() {
        let mut w = world(0, 96, 7);
        let monster = spawn_thing(
            &mut w.store,
            &w.map,
            &mut w.rng,
            MobjType::MT_POSSESSED,
            128,
            192,
        );
        w.map.linedefs[4].tag = 7;
        let mut ctx = ctx(&mut w);

        assert!(ev_do_ceiling(&mut ctx, 4, CeilingKind::CrushAndRaise));
        let mut reached_bottom = false;
        let mut raised_again = false;
        // the crusher halves its speed while it digs into the thing,
        // so the last stretch is slow — give it room
        for _ in 0..1500 {
            run_thinkers(&mut ctx);
            let ceiling_now = ceilings(ctx.store);
            if ceiling_now.is_empty() {
                break;
            }
            let direction = ceiling_now[0].direction;
            if ctx.map.sectors[1].ceiling_height <= fixed(8) {
                reached_bottom = true;
            }
            if reached_bottom && direction == 1 {
                raised_again = true;
                break;
            }
        }
        assert!(reached_bottom, "crusher must reach floor + 8");
        assert!(
            raised_again,
            "crusher must rise again after reaching the bottom"
        );
        let crushed = ctx.events.iter().any(
            |e| matches!(e, SpecialEvent::Damage { target, damage: 10, .. } if *target == monster),
        );
        assert!(crushed, "crusher must hurt the thing in the way");
    }

    #[test]
    fn lower_and_crush_halves_speed_while_crushing() {
        let mut w = world(0, 96, 7);
        spawn_thing(
            &mut w.store,
            &w.map,
            &mut w.rng,
            MobjType::MT_POSSESSED,
            128,
            192,
        );
        w.map.linedefs[4].tag = 7;
        let mut ctx = ctx(&mut w);

        assert!(ev_do_ceiling(&mut ctx, 4, CeilingKind::LowerAndCrush));
        // LowerAndCrush has crush=false: the ceiling stops at the monster
        // and crawls at CEILSPEED/8
        for _ in 0..100 {
            run_thinkers(&mut ctx);
        }
        let ceiling = ceilings(ctx.store);
        assert_eq!(ceiling.len(), 1);
        assert_eq!(ceiling[0].speed, Fixed::from_raw(CEILSPEED.to_raw() / 8));
    }

    #[test]
    fn crush_stop_parks_the_ceiling_and_a_new_crush_wakes_it() {
        let mut w = world(0, 96, 7);
        w.map.linedefs[4].tag = 7;
        let mut ctx = ctx(&mut w);

        assert!(ev_do_ceiling(&mut ctx, 4, CeilingKind::CrushAndRaise));
        tick(&mut ctx, 3);
        assert!(ev_ceiling_crush_stop(&mut ctx, 4));
        assert_eq!(ceilings(ctx.store)[0].direction, 0);
        let parked = ctx.map.sectors[1].ceiling_height;
        tick(&mut ctx, 20);
        assert_eq!(
            ctx.map.sectors[1].ceiling_height, parked,
            "a parked ceiling must not move"
        );

        // a fresh crush action re-activates ceilings in stasis with the
        // tag; the resumed ceiling counts as busy, so no *new* ceiling
        // was started (vanilla returns 0 here too)
        assert!(!ev_do_ceiling(&mut ctx, 4, CeilingKind::CrushAndRaise));
        let direction = ceilings(ctx.store)[0].direction;
        assert_eq!(direction, -1, "stasis ceiling resumes in its old direction");
    }

    #[test]
    fn raise_to_highest_reaches_the_highest_neighbor_then_removes_itself() {
        let mut w = world(0, 64, 7);
        // south ceiling is 128; north (tagged) is 64 -> rise to 128
        w.map.linedefs[4].tag = 7;
        let mut ctx = ctx(&mut w);

        assert!(ev_do_ceiling(&mut ctx, 4, CeilingKind::RaiseToHighest));
        tick(&mut ctx, 80);
        assert_eq!(ctx.map.sectors[1].ceiling_height, fixed(128));
        assert!(ceilings(ctx.store).is_empty());
    }

    #[test]
    fn lower_to_floor_descends_to_the_floor_and_removes_itself() {
        let mut w = world(0, 96, 7);
        w.map.linedefs[4].tag = 7;
        let mut ctx = ctx(&mut w);

        assert!(ev_do_ceiling(&mut ctx, 4, CeilingKind::LowerToFloor));
        // no crush: the thinker walks to 0 and removes itself
        tick(&mut ctx, 120);
        assert_eq!(ctx.map.sectors[1].ceiling_height, fixed(0));
        assert!(ceilings(ctx.store).is_empty());
    }
}
