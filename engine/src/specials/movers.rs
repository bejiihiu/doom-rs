//! The moving-sector machinery, ported from `p_floor.c` (`T_MovePlane`)
//! and the `P_ChangeSector`/`P_ThingHeightClip` half of `p_map.c`. Doors
//! need it first; the floor/plat/ceiling thinkers will reuse the same
//! pieces.
//!
//! ## What `P_ChangeSector` does in vanilla
//!
//! For every thing near the moving sector it recomputes the thing's
//! blocking window (`P_ThingHeightClip` via `P_CheckPosition`). Failures
//! are the door-bounce condition; corpses turn to giblets; dropped items
//! are removed; shootable things take 10 damage every fourth tic when the
//! move crushes.
//!
//! The vanilla path also consumed RNG and called `P_DamageMobj` inline.
//! Here the RNG is consumed inline only for the blood-spurt momenta, and
//! the damage itself comes out as a [`SpecialEvent::Damage`] event for the
//! game loop, like everywhere else in the gameplay layer.
//!
//! The thing-vs-thing half of the window check is a plain bbox test: a
//! solid thing overlapping the moving window behaves as a blocker, exactly
//! like vanilla's `PIT_CheckThing` return value. The missile/skull-fly
//! damage branches of `P_CheckPosition` are not replicated (a hovering
//! missile inside a crushing sector does not wound what it overlaps), and
//! an `MF_PICKUP` mover touching an `MF_SPECIAL` thing emits the pickup
//! event as a side effect, exactly like vanilla.

use crate::fixed::Fixed;
use crate::info::{MF_DROPPED, MF_MISSILE, MF_NOCLIP, MF_PICKUP, MF_SOLID, MF_SPECIAL};
use crate::map::{Map, types::ML_BLOCKING, types::ML_BLOCKMONSTERS};
use crate::math::{BBox, box_on_side};
use crate::movement::MF_SHOOTABLE;
use crate::movement::intercepts::{divline_for, lines_near};
use crate::movement::line_opening;
use crate::specials::util::sector_union_box;
use crate::specials::{PassCtx, SpecialEvent};
use crate::thinker::{Thinker, ThinkerId, ThinkerRegistry};

/// Vanilla `result_e` from `p_floor.h`/`p_local.h`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MoveOutcome {
    /// The plane is still moving (or stayed put this tic).
    Ok,
    /// The plane wants to move but something doesn't fit.
    Crushed,
    /// The plane reached (or would pass) its destination.
    PastDest,
}

/// Vanilla's `floorOrCeiling` argument (0/1) for `T_MovePlane`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaneKind {
    /// Move `sector->floorheight`.
    Floor,
    /// Move `sector->ceilingheight`.
    Ceiling,
}

/// Vanilla `T_MovePlane`. Moves `sector`'s floor or ceiling by `speed`
/// toward `dest`, re-clipping everything inside, and reports how far it
/// got. `crush` toggles the crush damage (and the "stay behind the
/// destination" behavior of floor up / ceiling down moves).
///
/// The argument list mirrors vanilla's `T_MovePlane` plus the registry
/// and the pass context.
#[allow(clippy::too_many_arguments)]
pub fn move_plane(
    reg: &mut ThinkerRegistry,
    ctx: &mut PassCtx<'_>,
    sector: usize,
    speed: Fixed,
    dest: Fixed,
    crush: bool,
    plane: PlaneKind,
    direction: i32,
) -> MoveOutcome {
    match plane {
        PlaneKind::Floor => move_plane_floor(reg, ctx, sector, speed, dest, crush, direction),
        PlaneKind::Ceiling => move_plane_ceiling(reg, ctx, sector, speed, dest, crush, direction),
    }
}

fn move_plane_floor(
    reg: &mut ThinkerRegistry,
    ctx: &mut PassCtx<'_>,
    sector: usize,
    speed: Fixed,
    dest: Fixed,
    crush: bool,
    direction: i32,
) -> MoveOutcome {
    match direction {
        -1 => {
            // FLOOR DOWN
            let current = ctx.map.sectors[sector].floor_height;
            if current - speed < dest {
                let lastpos = current;
                ctx.map.sectors[sector].floor_height = dest;
                let flag = sector_change(reg, ctx, sector, crush);
                if flag {
                    ctx.map.sectors[sector].floor_height = lastpos;
                    sector_change(reg, ctx, sector, crush);
                }
                return MoveOutcome::PastDest;
            }
            let lastpos = ctx.map.sectors[sector].floor_height;
            ctx.map.sectors[sector].floor_height -= speed;
            let flag = sector_change(reg, ctx, sector, crush);
            if flag {
                ctx.map.sectors[sector].floor_height = lastpos;
                sector_change(reg, ctx, sector, crush);
                return MoveOutcome::Crushed;
            }
            MoveOutcome::Ok
        }
        1 => {
            // FLOOR UP
            let current = ctx.map.sectors[sector].floor_height;
            if current + speed > dest {
                let lastpos = current;
                ctx.map.sectors[sector].floor_height = dest;
                let flag = sector_change(reg, ctx, sector, crush);
                if flag {
                    ctx.map.sectors[sector].floor_height = lastpos;
                    sector_change(reg, ctx, sector, crush);
                }
                return MoveOutcome::PastDest;
            }
            // COULD GET CRUSHED
            let lastpos = ctx.map.sectors[sector].floor_height;
            ctx.map.sectors[sector].floor_height += speed;
            let flag = sector_change(reg, ctx, sector, crush);
            if flag {
                if crush {
                    return MoveOutcome::Crushed;
                }
                ctx.map.sectors[sector].floor_height = lastpos;
                sector_change(reg, ctx, sector, crush);
                return MoveOutcome::Crushed;
            }
            MoveOutcome::Ok
        }
        _ => MoveOutcome::Ok,
    }
}

fn move_plane_ceiling(
    reg: &mut ThinkerRegistry,
    ctx: &mut PassCtx<'_>,
    sector: usize,
    speed: Fixed,
    dest: Fixed,
    crush: bool,
    direction: i32,
) -> MoveOutcome {
    match direction {
        -1 => {
            // CEILING DOWN
            let current = ctx.map.sectors[sector].ceiling_height;
            if current - speed < dest {
                let lastpos = current;
                ctx.map.sectors[sector].ceiling_height = dest;
                let flag = sector_change(reg, ctx, sector, crush);
                if flag {
                    ctx.map.sectors[sector].ceiling_height = lastpos;
                    sector_change(reg, ctx, sector, crush);
                }
                return MoveOutcome::PastDest;
            }
            // COULD GET CRUSHED
            let lastpos = ctx.map.sectors[sector].ceiling_height;
            ctx.map.sectors[sector].ceiling_height -= speed;
            let flag = sector_change(reg, ctx, sector, crush);
            if flag {
                if crush {
                    return MoveOutcome::Crushed;
                }
                ctx.map.sectors[sector].ceiling_height = lastpos;
                sector_change(reg, ctx, sector, crush);
                return MoveOutcome::Crushed;
            }
            MoveOutcome::Ok
        }
        1 => {
            // CEILING UP
            let current = ctx.map.sectors[sector].ceiling_height;
            if current + speed > dest {
                let lastpos = current;
                ctx.map.sectors[sector].ceiling_height = dest;
                let flag = sector_change(reg, ctx, sector, crush);
                if flag {
                    ctx.map.sectors[sector].ceiling_height = lastpos;
                    sector_change(reg, ctx, sector, crush);
                }
                return MoveOutcome::PastDest;
            }
            let lastpos = ctx.map.sectors[sector].ceiling_height;
            ctx.map.sectors[sector].ceiling_height += speed;
            sector_change(reg, ctx, sector, crush);
            // vanilla's crush-return here is compiled out (#if 0 in
            // p_floor.c): a rising ceiling always succeeds
            let _ = lastpos;
            MoveOutcome::Ok
        }
        _ => MoveOutcome::Ok,
    }
}

/// Vanilla `P_ChangeSector`. Re-clips every thing near `sector` after the
/// sector moved; returns vanilla's `nofit` — true when at least one
/// shootable thing does not fit anymore.
pub(crate) fn sector_change(
    reg: &mut ThinkerRegistry,
    ctx: &mut PassCtx<'_>,
    sector: usize,
    crush: bool,
) -> bool {
    let mut nofit = false;
    let crushchange = crush;

    let Some((left, bottom, right, top)) = sector_union_box(ctx.map, sector) else {
        // no lines: nothing to clip against
        return false;
    };

    // gather the mobjs whose bbox overlaps the sector's expanded line box
    let candidates: Vec<ThinkerId> = reg
        .iter()
        .filter_map(|(id, thinker)| match thinker {
            Thinker::Mobj(mobj) => Some((id, mobj)),
            _ => None,
        })
        .filter(|(_, mobj)| {
            let radius = mobj.radius;
            mobj.x + radius >= left
                && mobj.x - radius <= right
                && mobj.y + radius >= bottom
                && mobj.y - radius <= top
        })
        .map(|(id, _)| id)
        .collect();

    for id in candidates {
        let map: &Map = ctx.map;
        if thing_height_clip(reg, map, id, ctx.events) {
            continue;
        }

        // it doesn't fit: corpses gib, dropped items pop out
        let Some(Thinker::Mobj(mobj)) = reg.get(id) else {
            continue;
        };
        let (flags, health) = (mobj.flags, mobj.health);

        if health <= 0 {
            // vanilla: P_SetMobjState(thing, S_GIBS) + clear solid +
            // height=0 + radius=0
            let momx =
                Fixed::from_raw((i32::from(ctx.rng.next()) - i32::from(ctx.rng.next())) << 12);
            let momy =
                Fixed::from_raw((i32::from(ctx.rng.next()) - i32::from(ctx.rng.next())) << 12);
            ctx.events.push(SpecialEvent::GibCorpse { id, momx, momy });
            if let Some(Thinker::Mobj(mobj)) = reg.get_mut(id) {
                mobj.flags &= !MF_SOLID;
                mobj.height = Fixed::ZERO;
                mobj.radius = Fixed::ZERO;
            }
            continue;
        }

        if flags & MF_DROPPED != 0 {
            // vanilla: P_RemoveMobj
            ctx.events.push(SpecialEvent::CrushRemove { id });
            continue;
        }

        if flags & MF_SHOOTABLE == 0 {
            // gibs and such; leave them alone
            continue;
        }

        nofit = true;

        if crushchange && (ctx.leveltime & 3) == 0 {
            // vanilla: P_DamageMobj(thing, NULL, NULL, 10) + blood spurt
            let momx =
                Fixed::from_raw((i32::from(ctx.rng.next()) - i32::from(ctx.rng.next())) << 12);
            let momy =
                Fixed::from_raw((i32::from(ctx.rng.next()) - i32::from(ctx.rng.next())) << 12);
            ctx.events.push(SpecialEvent::Damage {
                target: id,
                inflictor: None,
                source: None,
                damage: 10,
                blood: Some((momx, momy)),
            });
        }
    }

    nofit
}

/// Vanilla `P_ThingHeightClip`. Recomputes the thing's floor/ceiling
/// window after the sector moved, re-pins its z, and reports whether it
/// still fits. Side effect: `MF_PICKUP` movers may pick up touched items,
/// as a side effect of the window check — kept for vanilla parity.
fn thing_height_clip(
    reg: &mut ThinkerRegistry,
    map: &Map,
    id: ThinkerId,
    events: &mut Vec<SpecialEvent>,
) -> bool {
    let Some(Thinker::Mobj(mobj)) = reg.get(id) else {
        return true;
    };
    let (flags, radius, x, y, z) = (mobj.flags, mobj.radius, mobj.x, mobj.y, mobj.z);
    let mover_is_player = mobj.player.is_some();
    let onfloor = z == mobj.floorz;

    // base window from the thing's sector
    let sector_index = mobj.sector.or_else(|| {
        crate::mobj::point_in_subsector(map, x, y)
            .and_then(|ss| map.subsectors.get(ss))
            .and_then(|ss| ss.sector)
    });
    let Some(sector_index) = sector_index else {
        tracing::warn!("thing_height_clip: degenerate subsector, skipping clip");
        return true;
    };

    let mut floorz = map.sectors[sector_index].floor_height;
    let mut ceilingz = map.sectors[sector_index].ceiling_height;

    if flags & MF_NOCLIP == 0 {
        // lines: shrink the window against contacted two-sided lines;
        // blocking lines make the thing not fit
        let bbox = BBox {
            top: y + radius,
            bottom: y - radius,
            left: x - radius,
            right: x + radius,
        };
        for line in lines_near(map, &bbox) {
            let linedef = &map.linedefs[line];
            if bbox.right <= linedef.bbox.left
                || bbox.left >= linedef.bbox.right
                || bbox.top <= linedef.bbox.bottom
                || bbox.bottom >= linedef.bbox.top
            {
                continue;
            }
            let divline = divline_for(map, line);
            if box_on_side(&bbox, &divline).is_some() {
                continue;
            }
            if linedef.backsector.is_none() {
                // one-sided line: the thing can't fit through it
                return false;
            }
            if flags & MF_MISSILE == 0 {
                if (linedef.flags & ML_BLOCKING) != 0 {
                    return false;
                }
                if !mover_is_player && (linedef.flags & ML_BLOCKMONSTERS) != 0 {
                    return false;
                }
            }
            let opening = line_opening(map, line);
            if opening.top < ceilingz {
                ceilingz = opening.top;
            }
            if opening.bottom > floorz {
                floorz = opening.bottom;
            }
        }

        // things: a solid thing overlapping the window blocks, like
        // vanilla's PIT_CheckThing return value. No RNG is consumed here;
        // the missile/skull-fly damage branches of the vanilla check are
        // not replicated (see the module docs).
        let overlap: Vec<ThinkerId> = reg
            .iter()
            .filter_map(|(other_id, other)| match other {
                Thinker::Mobj(other) => Some((other_id, other)),
                _ => None,
            })
            .filter(|(other_id, _)| *other_id != id)
            .filter(|(_, other)| other.flags & (MF_SOLID | MF_SPECIAL | MF_SHOOTABLE) != 0)
            .filter(|(_, other)| {
                let blockdist = other.radius + radius;
                let dx = (other.x.to_raw() as i64 - x.to_raw() as i64).unsigned_abs();
                let dy = (other.y.to_raw() as i64 - y.to_raw() as i64).unsigned_abs();
                dx < blockdist.to_raw() as u64 && dy < blockdist.to_raw() as u64
            })
            .map(|(other_id, _)| other_id)
            .collect();

        for other_id in overlap {
            let Some(Thinker::Mobj(other)) = reg.get(other_id) else {
                continue;
            };
            let other_flags = other.flags;
            let other_solid = other_flags & MF_SOLID != 0;

            if other_flags & MF_SPECIAL != 0 {
                // vanilla: an MF_PICKUP mover consumes the thing as a side
                // effect of the check
                if flags & MF_PICKUP != 0 {
                    events.push(SpecialEvent::Move(crate::movement::MoveEvent::Pickup {
                        special: other_id,
                        toucher: id,
                    }));
                }
                if other_solid {
                    return false;
                }
                continue;
            }

            if other_solid {
                return false;
            }
        }
    }

    // write the new window back, re-pin z
    if let Some(Thinker::Mobj(mobj)) = reg.get_mut(id) {
        mobj.floorz = floorz;
        mobj.ceilingz = ceilingz;
        if onfloor {
            mobj.z = floorz;
        } else if mobj.z + mobj.height > ceilingz {
            mobj.z = ceilingz - mobj.height;
        }
        return ceilingz - floorz >= mobj.height;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::FRACUNIT;
    use crate::map::Map;
    use crate::rng::DoomRng;
    use crate::specials::testutil::split_map;

    fn fixed(units: i32) -> Fixed {
        Fixed::from_raw(units * FRACUNIT)
    }

    fn world() -> (Map, DoomRng) {
        let map = split_map(0, 128, 0);
        let rng = DoomRng::with_seed(0);
        (map, rng)
    }

    use crate::specials::testutil::test_pass as pass_for;

    #[test]
    fn move_plane_ceiling_down_moves_and_stops_at_dest() {
        let (mut map, mut rng) = world();
        // south room ceiling 128; lower it toward y=20
        let mut events = Vec::new();
        let mut ctx = pass_for(&mut map, &mut rng, &mut events);
        let mut reg = ThinkerRegistry::new();
        let res = move_plane(
            &mut reg,
            &mut ctx,
            0,
            fixed(2),
            fixed(20),
            false,
            PlaneKind::Ceiling,
            -1,
        );
        assert_eq!(res, MoveOutcome::Ok);
        assert_eq!(ctx.map.sectors[0].ceiling_height, fixed(126));
    }

    #[test]
    fn move_plane_ceiling_up_reaches_past_dest() {
        let (mut map, mut rng) = world();
        map.sectors[0].ceiling_height = fixed(250);
        let mut events = Vec::new();
        let mut ctx = pass_for(&mut map, &mut rng, &mut events);
        let mut reg = ThinkerRegistry::new();
        // far above 256*FRACUNIT? dest is 256; current 250 + 2 speed <= 256 -> Ok
        let res = move_plane(
            &mut reg,
            &mut ctx,
            0,
            fixed(2),
            fixed(256),
            false,
            PlaneKind::Ceiling,
            1,
        );
        assert_eq!(res, MoveOutcome::Ok);
        let res = move_plane(
            &mut reg,
            &mut ctx,
            0,
            fixed(10),
            fixed(256),
            false,
            PlaneKind::Ceiling,
            1,
        );
        assert_eq!(res, MoveOutcome::PastDest);
        // past dest clamps the ceiling to the destination exactly
        assert_eq!(ctx.map.sectors[0].ceiling_height, fixed(256));
    }

    #[test]
    fn crush_fixture_without_mobj_reaches_dest() {
        let (mut map, mut rng) = world();
        // no mobjs in the pass registry means nothing blocks the plane,
        // so it walks all the way down to dest over several tics
        map.sectors[0].ceiling_height = fixed(4);
        let mut events = Vec::new();
        let mut ctx = pass_for(&mut map, &mut rng, &mut events);
        let mut reg = ThinkerRegistry::new();
        let mut outcome = MoveOutcome::Ok;
        for _ in 0..10 {
            if outcome != MoveOutcome::Ok {
                break;
            }
            outcome = move_plane(
                &mut reg,
                &mut ctx,
                0,
                fixed(2),
                fixed(0),
                false,
                PlaneKind::Ceiling,
                -1,
            );
        }
        assert_eq!(outcome, MoveOutcome::PastDest);
        assert_eq!(ctx.map.sectors[0].ceiling_height, fixed(0));
    }
}
