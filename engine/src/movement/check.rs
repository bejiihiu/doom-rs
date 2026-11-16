//! Line and mobj clipping, ported from `p_map.c` (`PIT_CheckLine`,
//! `PIT_CheckThing`, `PIT_StompThing`, `P_CheckPosition`, `P_TryMove`,
//! `P_TeleportMove`) and `P_LineOpening` from `p_maputl.c`.
//!
//! Everything the original called `tm*` lives in [`CheckState`], created
//! fresh per call; [`MoveReport`] is what the caller sees. Things are
//! checked before lines, exactly like vanilla — a mobj can pick up a
//! special on the same tic it is otherwise stopped by a wall.

use crate::fixed::Fixed;
use crate::info::{
    MF_DROPOFF, MF_FLOAT, MF_MISSILE, MF_NOCLIP, MF_PICKUP, MF_SKULLFLY, MF_SOLID, MF_SPECIAL,
    MF_TELEPORT, MobjType,
};
use crate::map::types::{ML_BLOCKING, ML_BLOCKMONSTERS};
use crate::math::{BBox, box_on_side, point_on_side};
use crate::mobj::point_in_subsector;
use crate::thinker::ThinkerId;

use super::intercepts::{divline_for, line_opening, lines_near, things_near};
use super::{MAXRADIUS, MoveContext, MoveEvent, MoveReport};

/// Vanilla `24*FRACUNIT`: the maximum step a thing may walk up, and the
/// maximum dropoff a non-`MF_DROPOFF` thing may stand over.
pub const MAX_STEP_UP: Fixed = Fixed::from_raw(24 << 16);

/// The vanilla `tm*` globals, scoped to one check.
struct CheckState {
    /// The moving thing.
    thing: ThinkerId,
    /// Its flags, copied.
    flags: u32,
    /// True when the mover is a player (vanilla `tmthing->player`).
    mover_is_player: bool,
    /// Its collision radius, copied.
    radius: Fixed,
    /// Destination X.
    x: Fixed,
    /// Destination Y.
    y: Fixed,
    /// Destination bounding box.
    bbox: BBox,
    /// Vanilla `tmfloorz`.
    floorz: Fixed,
    /// Vanilla `tmceilingz`.
    ceilingz: Fixed,
    /// Vanilla `tmdropoffz`.
    dropoffz: Fixed,
    /// Vanilla `ceilingline`, as a linedef index.
    ceiling_line: Option<usize>,
    /// Vanilla `spechit`, in contact order.
    specials: Vec<usize>,
    /// Side effects for the gameplay layer.
    events: Vec<MoveEvent>,
    /// Subsector containing the destination, if it resolved.
    subsector: Option<usize>,
}

impl CheckState {
    fn new(
        thing: ThinkerId,
        flags: u32,
        mover_is_player: bool,
        radius: Fixed,
        x: Fixed,
        y: Fixed,
    ) -> Self {
        CheckState {
            thing,
            flags,
            mover_is_player,
            radius,
            x,
            y,
            bbox: BBox {
                top: y + radius,
                bottom: y - radius,
                left: x - radius,
                right: x + radius,
            },
            floorz: Fixed::ZERO,
            ceilingz: Fixed::ZERO,
            dropoffz: Fixed::ZERO,
            ceiling_line: None,
            specials: Vec::new(),
            events: Vec::new(),
            subsector: None,
        }
    }

    fn into_report(self, ok: bool) -> MoveReport {
        MoveReport {
            ok,
            floatok: false,
            floorz: self.floorz,
            ceilingz: self.ceilingz,
            dropoffz: self.dropoffz,
            ceiling_line: self.ceiling_line,
            specials: self.specials,
            subsector: self.subsector,
            events: self.events,
        }
    }
}

/// Vanilla `PIT_CheckLine`: adjust the moving thing's floor/ceiling against
/// one line. Returns false when the line blocks.
fn check_line(ctx: &mut MoveContext, state: &mut CheckState, line: usize) -> bool {
    let map = ctx.map;
    let linedef = &map.linedefs[line];

    // bounding boxes don't overlap: nothing to do
    if state.bbox.right <= linedef.bbox.left
        || state.bbox.left >= linedef.bbox.right
        || state.bbox.top <= linedef.bbox.bottom
        || state.bbox.bottom >= linedef.bbox.top
    {
        return true;
    }

    let line_divline = divline_for(map, line);
    // vanilla treats box-on-one-side as "no crossing"
    if box_on_side(&state.bbox, &line_divline).is_some() {
        return true;
    }

    // A line has been hit: it crosses the moving thing's destination box.
    // If this should not be allowed, return false. If the line is special,
    // remember it to process if the move proves ok.

    if linedef.backsector.is_none() {
        return false; // one sided line
    }

    if state.flags & MF_MISSILE == 0 {
        if linedef.flags & ML_BLOCKING != 0 {
            return false; // explicitly blocking everything
        }
        if !state.mover_is_player && linedef.flags & ML_BLOCKMONSTERS != 0 {
            return false; // block monsters only
        }
    }

    // set openrange, opentop, openbottom
    let opening = line_opening(map, line);

    // adjust floor / ceiling heights
    if opening.top < state.ceilingz {
        state.ceilingz = opening.top;
        state.ceiling_line = Some(line);
    }

    if opening.bottom > state.floorz {
        state.floorz = opening.bottom;
    }

    if opening.low_floor < state.dropoffz {
        state.dropoffz = opening.low_floor;
    }

    // if contacted a special line, add it to the list
    if linedef.special != 0 {
        state.specials.push(line);
    }

    true
}

/// Vanilla `PIT_CheckThing`.
fn check_thing(ctx: &mut MoveContext, state: &mut CheckState, other_id: ThinkerId) -> bool {
    let Some(other) = ctx.store.mobj(other_id) else {
        // stale handle in a blockmap cell; vanilla followed a valid pointer
        return true;
    };
    if other.flags & (MF_SOLID | MF_SPECIAL | super::MF_SHOOTABLE) == 0 {
        return true;
    }
    let other = (
        other.flags,
        other.x,
        other.y,
        other.z,
        other.radius,
        other.height,
        other.mobj_type,
    );
    let (oflags, ox, oy, oz, oradius, oheight, otype) = other;

    let Some(mover) = ctx.store.mobj(state.thing) else {
        return true; // the mover itself vanished; let the caller notice
    };
    let mradius = mover.radius;
    let mtgt = mover.target;
    let mtgt_type = mtgt.and_then(|t| ctx.store.mobj(t)).map(|t| t.mobj_type);
    let mover_z = mover.z;
    let mover_height = mover.height;
    let mover_damage = mover.info().damage;
    let mover_spawnstate = mover.info().spawnstate;

    let blockdist = oradius + mradius;

    // 64-bit here: fixed-point subtraction of map-extreme coordinates can
    // overflow 32 bits; the original's abs() would read it as a negative
    // number there anyway
    let dx = (ox.to_raw() as i64 - state.x.to_raw() as i64).abs();
    let dy = (oy.to_raw() as i64 - state.y.to_raw() as i64).abs();
    if dx >= blockdist.to_raw() as i64 || dy >= blockdist.to_raw() as i64 {
        // didn't hit it
        return true;
    }

    // don't clip against self
    if other_id == state.thing {
        return true;
    }

    // check for skulls slamming into things
    if state.flags & MF_SKULLFLY != 0 {
        let damage = (i32::from(ctx.rng.next() % 8) + 1) * mover_damage;

        state.events.push(MoveEvent::Damage {
            target: other_id,
            inflictor: Some(state.thing),
            source: Some(state.thing),
            damage,
        });

        if let Some(mover) = ctx.store.mobj_mut(state.thing) {
            mover.flags &= !MF_SKULLFLY;
            mover.momx = Fixed::ZERO;
            mover.momy = Fixed::ZERO;
            mover.momz = Fixed::ZERO;
        }
        let _ = ctx
            .store
            .set_mobj_state(state.thing, mover_spawnstate, ctx.leveltime);

        return false; // stop moving
    }

    // missiles can hit other things
    if state.flags & MF_MISSILE != 0 {
        // see if it went over / under
        if mover_z > oz + oheight {
            return true; // overhead
        }
        if mover_z + mover_height < oz {
            return true; // underneath
        }

        if let Some(target_id) = mtgt {
            let same_species = matches!(mtgt_type, Some(t) if t == otype);
            let bruiser_knight = matches!(
                (mtgt_type, otype),
                (Some(MobjType::MT_KNIGHT), MobjType::MT_BRUISER)
                    | (Some(MobjType::MT_BRUISER), MobjType::MT_KNIGHT)
            );
            if same_species || bruiser_knight {
                // Don't hit same species as originator. Player missiles may
                // hit the originator's species (players missile players).
                if other_id == target_id {
                    return true;
                }
                if otype != MobjType::MT_PLAYER {
                    // Explode, but do no damage.
                    return false;
                }
            }
        }

        if oflags & super::MF_SHOOTABLE == 0 {
            // didn't do any damage
            return oflags & MF_SOLID == 0; // return !(thing->flags & MF_SOLID)
        }

        // damage / explode
        let damage = (i32::from(ctx.rng.next() % 8) + 1) * mover_damage;
        state.events.push(MoveEvent::Damage {
            target: other_id,
            inflictor: Some(state.thing),
            source: mtgt,
            damage,
        });

        // don't traverse any more
        return false;
    }

    // check for special pickup
    if oflags & MF_SPECIAL != 0 {
        let solid = oflags & MF_SOLID != 0;
        if state.flags & MF_PICKUP != 0 {
            // can remove thing
            state.events.push(MoveEvent::Pickup {
                special: other_id,
                toucher: state.thing,
            });
        }
        return !solid;
    }

    oflags & MF_SOLID == 0
}

/// Vanilla `PIT_StompThing`: teleport destination check. Things that are
/// shootable get flattened by the move. Monsters don't stomp things except
/// on the boss level.
fn stomp_thing(
    ctx: &mut MoveContext,
    state: &mut CheckState,
    other_id: ThinkerId,
    boss_level: bool,
) -> bool {
    let Some(other) = ctx.store.mobj(other_id) else {
        return true;
    };
    if other.flags & super::MF_SHOOTABLE == 0 {
        return true;
    }
    let (ox, oy, oradius) = (other.x, other.y, other.radius);

    let blockdist = oradius + state.radius;
    let dx = (ox.to_raw() as i64 - state.x.to_raw() as i64).abs();
    let dy = (oy.to_raw() as i64 - state.y.to_raw() as i64).abs();
    if dx >= blockdist.to_raw() as i64 || dy >= blockdist.to_raw() as i64 {
        // didn't hit it
        return true;
    }

    // don't clip against self
    if other_id == state.thing {
        return true;
    }

    // monsters don't stomp things except on boss level
    if !state.mover_is_player && !boss_level {
        return false;
    }

    state.events.push(MoveEvent::Damage {
        target: other_id,
        inflictor: Some(state.thing),
        source: Some(state.thing),
        damage: 10000,
    });

    true
}

/// Vanilla `P_CheckPosition` — the bounding box of the thing at `(x, y)` is
/// checked against the blockmap for overlapping lines and mobjs. Purely
/// informative: nothing is modified except what the check itself consumes
/// (specials picked up with `MF_PICKUP`, skull fly damage bookkeeping).
///
/// The bounding box is extended by [`MAXRADIUS`] (32 units) on the thing
/// side because mobjs are grouped into mapblocks by origin and can overlap
/// into adjacent blocks.
pub fn check_position(ctx: &mut MoveContext, id: ThinkerId, x: Fixed, y: Fixed) -> MoveReport {
    let Some(mobj) = ctx.store.mobj(id) else {
        tracing::warn!("check_position: stale mobj handle");
        return MoveReport::failed();
    };
    let flags = mobj.flags;
    let radius = mobj.radius;
    let mover_is_player = mobj.player.is_some();
    let mut state = CheckState::new(id, flags, mover_is_player, radius, x, y);

    let Some(subsector) = point_in_subsector(ctx.map, x, y) else {
        tracing::warn!("check_position: no subsector at ({x}, {y})");
        return MoveReport::failed();
    };
    let sector = ctx.map.subsectors.get(subsector).and_then(|ss| ss.sector);
    let Some(sector) = sector else {
        tracing::warn!("check_position: degenerate subsector {subsector} has no sector");
        return MoveReport::failed();
    };

    // The base floor/ceiling is from the subsector that contains the point.
    // Any contacted line that steps the window closer together adjusts them.
    state.floorz = ctx.map.sectors[sector].floor_height;
    state.dropoffz = state.floorz;
    state.ceilingz = ctx.map.sectors[sector].ceiling_height;
    state.subsector = Some(subsector);

    if flags & MF_NOCLIP != 0 {
        // clipping is bypassed, but the heights are still resolved
        return state.into_report(true);
    }

    // Check things first, possibly picking things up.
    // The bounding box is extended by MAXRADIUS because mobjs are grouped
    // into mapblocks based on their origin point, and can overlap into
    // adjacent blocks by up to MAXRADIUS units.
    let thing_clip = BBox {
        top: state.bbox.top + MAXRADIUS,
        bottom: state.bbox.bottom - MAXRADIUS,
        left: state.bbox.left - MAXRADIUS,
        right: state.bbox.right + MAXRADIUS,
    };

    for other in things_near(ctx, &thing_clip) {
        if !check_thing(ctx, &mut state, other) {
            return state.into_report(false);
        }
    }

    // check lines
    for line in lines_near(ctx.map, &state.bbox) {
        if !check_line(ctx, &mut state, line) {
            return state.into_report(false);
        }
    }

    state.into_report(true)
}

/// Vanilla `P_TryMove`: attempt to move to a new position, crossing special
/// lines unless `MF_TELEPORT` is set. Returns the check outcome; `ok` is
/// vanilla's boolean return.
pub fn try_move(ctx: &mut MoveContext, id: ThinkerId, x: Fixed, y: Fixed) -> MoveReport {
    let mut report = check_position(ctx, id, x, y);
    if !report.ok {
        // solid wall or thing
        return report;
    }

    let Some(mover) = ctx.store.mobj(id) else {
        // the mover vanished during the check; roll back to a plain failure
        tracing::warn!("try_move: mover vanished mid-check");
        return MoveReport::failed();
    };
    let flags = mover.flags;
    let height = mover.height;
    let z = mover.z;

    if flags & MF_NOCLIP == 0 {
        if report.ceilingz - report.floorz < height {
            report.ok = false; // doesn't fit
            return report;
        }

        report.floatok = true;

        if flags & MF_TELEPORT == 0 && report.ceilingz - z < height {
            report.ok = false; // mobj must lower itself to fit
            return report;
        }

        if flags & MF_TELEPORT == 0 && report.floorz - z > MAX_STEP_UP {
            report.ok = false; // too big a step up
            return report;
        }

        if flags & (MF_DROPOFF | MF_FLOAT) == 0 && report.floorz - report.dropoffz > MAX_STEP_UP {
            report.ok = false; // don't stand over a dropoff
            return report;
        }
    }

    // the move is ok, so link the thing into its new position
    let oldx = mover.x;
    let oldy = mover.y;
    ctx.store.unset_thing_position(id);

    if let Some(mo) = ctx.store.mobj_mut(id) {
        mo.floorz = report.floorz;
        mo.ceilingz = report.ceilingz;
        mo.x = x;
        mo.y = y;
    }
    ctx.store.set_thing_position(ctx.map, id);

    // if any special lines were hit, report the crossings; vanilla applies
    // the effects inline (P_CrossSpecialLine) — that lives in the specials
    // layer and arrives as events
    if flags & (MF_TELEPORT | MF_NOCLIP) == 0 {
        for line in report.specials.iter().rev() {
            // see if the line was crossed
            let line_divline = divline_for(ctx.map, *line);
            let side = point_on_side(x, y, &line_divline);
            let oldside = point_on_side(oldx, oldy, &line_divline);
            if side != oldside {
                // every line in `specials` had a special at collection time
                report.events.push(MoveEvent::CrossSpecial {
                    line: *line,
                    side: oldside,
                    thing: id,
                });
            }
        }
    }

    report
}

/// Vanilla `P_TeleportMove`: teleport the thing without momentum or height
/// checks, stomping anything shootable at the destination. `boss_level` is
/// vanilla's `gamemap == 30` (monsters may stomp only there).
///
/// Note this does *not* apply the `P_TryMove` fit checks — teleporting into
/// a closet is legal, just the stomping matters. The caller is responsible
/// for fog/sound effects at both ends.
pub fn teleport_move(
    ctx: &mut MoveContext,
    id: ThinkerId,
    x: Fixed,
    y: Fixed,
    boss_level: bool,
) -> MoveReport {
    let Some(mobj) = ctx.store.mobj(id) else {
        tracing::warn!("teleport_move: stale mobj handle");
        return MoveReport::failed();
    };
    let flags = mobj.flags;
    let radius = mobj.radius;
    let mover_is_player = mobj.player.is_some();
    let mut state = CheckState::new(id, flags, mover_is_player, radius, x, y);

    let Some(subsector) = point_in_subsector(ctx.map, x, y) else {
        tracing::warn!("teleport_move: no subsector at ({x}, {y})");
        return MoveReport::failed();
    };
    let sector = ctx.map.subsectors.get(subsector).and_then(|ss| ss.sector);
    let Some(sector) = sector else {
        tracing::warn!("teleport_move: degenerate subsector {subsector} has no sector");
        return MoveReport::failed();
    };

    // The base floor/ceiling is from the subsector that contains the point.
    state.floorz = ctx.map.sectors[sector].floor_height;
    state.dropoffz = state.floorz;
    state.ceilingz = ctx.map.sectors[sector].ceiling_height;
    state.subsector = Some(subsector);

    // stomp on any things contacted
    let thing_clip = BBox {
        top: state.bbox.top + MAXRADIUS,
        bottom: state.bbox.bottom - MAXRADIUS,
        left: state.bbox.left - MAXRADIUS,
        right: state.bbox.right + MAXRADIUS,
    };

    for other in things_near(ctx, &thing_clip) {
        if !stomp_thing(ctx, &mut state, other, boss_level) {
            return state.into_report(false);
        }
    }

    // the move is ok, so link the thing into its new position
    ctx.store.unset_thing_position(id);

    if let Some(mo) = ctx.store.mobj_mut(id) {
        mo.floorz = state.floorz;
        mo.ceilingz = state.ceilingz;
        mo.x = x;
        mo.y = y;
    }
    ctx.store.set_thing_position(ctx.map, id);

    state.into_report(true)
}

#[cfg(test)]
mod tests {
    //! Tests for the move clipping code. Coordinates are map units unless
    //! stated otherwise.
    use super::*;

    use crate::fixed::Fixed;
    use crate::info::{MF_DROPOFF, MF_NOCLIP, MF_SKULLFLY, MF_TELEPORT, MobjType};
    use crate::map::types::{ML_BLOCKING, ML_BLOCKMONSTERS, ML_TWOSIDED};
    use crate::mobj::MobjStore;
    use crate::rng::DoomRng;

    use super::super::MoveContext;
    use super::super::testutil::{closed_map, fixture, spawn_at, split_map, test_player};

    fn fixed_units(units: i32) -> Fixed {
        Fixed::from_int(units)
    }

    fn possessed_in_open_room(
        x: i32,
        y: i32,
    ) -> (
        crate::map::Map,
        MobjStore,
        DoomRng,
        crate::thinker::ThinkerId,
    ) {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, x, y);
        (map, store, rng, id)
    }

    #[test]
    fn position_in_free_space_is_ok() {
        let (map, mut store, mut rng, id) = possessed_in_open_room(64, 64);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = check_position(&mut ctx, id, fixed_units(100), fixed_units(64));

        assert!(report.ok);
        assert_eq!(report.floorz, fixed_units(0));
        assert_eq!(report.ceilingz, fixed_units(128));
        assert_eq!(report.dropoffz, fixed_units(0));
        assert!(report.specials.is_empty());
        assert!(report.events.is_empty());
    }

    #[test]
    fn position_overlapping_one_sided_wall_is_blocked() {
        // x = 8 with radius 20 reaches into the wall at x = 0
        let (map, mut store, mut rng, id) = possessed_in_open_room(64, 64);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = check_position(&mut ctx, id, fixed_units(8), fixed_units(64));
        assert!(!report.ok);

        // still fine just outside the wall (x = 20 touches, but the box test is >=)
        let report = check_position(&mut ctx, id, fixed_units(30), fixed_units(64));
        assert!(report.ok);
    }

    #[test]
    fn position_adjusts_window_against_divider() {
        // north sector floor 16: overlapping the divider lifts tmfloorz
        let map = split_map(16, 128, ML_TWOSIDED, 0);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 128, 64);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = check_position(&mut ctx, id, fixed_units(128), fixed_units(120));
        assert!(report.ok);
        assert_eq!(report.floorz, fixed_units(16));
    }

    #[test]
    fn ceiling_below_window_is_reported_but_not_blocked_here() {
        // P_CheckPosition only clips heights; the fit test lives in try_move
        let map = closed_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 128, 64);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = check_position(&mut ctx, id, fixed_units(128), fixed_units(120));
        assert!(report.ok);
        assert_eq!(report.ceilingz, fixed_units(0)); // window collapsed by the divider
    }

    #[test]
    fn try_move_into_wall_fails_and_keeps_position() {
        let (map, mut store, mut rng, id) = possessed_in_open_room(64, 64);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        // straddling the wall plane stops the move
        let report = try_move(&mut ctx, id, fixed_units(-1), fixed_units(64));
        assert!(!report.ok);
        let mo = ctx.store.mobj(id).unwrap();
        assert_eq!(mo.x, fixed_units(64));
        assert_eq!(mo.y, fixed_units(64));
    }

    #[test]
    fn try_move_exactly_touching_the_wall_is_fine() {
        // vanilla's reject is strict: box edge exactly on the line is not
        // "overlapping" — a direct destination check cannot distinguish a
        // touch from clearance (swept collision does not exist here)
        let (map, mut store, mut rng, id) = possessed_in_open_room(64, 64);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        // box reaches exactly x = 0, the wall plane
        let report = try_move(&mut ctx, id, fixed_units(-20), fixed_units(64));
        assert!(report.ok);
    }

    #[test]
    fn try_move_through_open_divider_updates_position() {
        let (map, mut store, mut rng, id) = possessed_in_open_room(128, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = try_move(&mut ctx, id, fixed_units(128), fixed_units(140));
        assert!(report.ok);
        assert!(report.floatok);
        assert_eq!(report.floorz, fixed_units(0));
        let mo = ctx.store.mobj(id).unwrap();
        assert_eq!(mo.y, fixed_units(140));
    }

    #[test]
    fn step_up_of_exactly_24_units_is_walkable() {
        let map = split_map(24, 128, ML_TWOSIDED, 0);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 128, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = try_move(&mut ctx, id, fixed_units(128), fixed_units(140));
        assert!(report.ok, "24 units must be exactly walkable");
    }

    #[test]
    fn step_up_of_25_units_is_blocked() {
        let map = split_map(25, 128, ML_TWOSIDED, 0);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 128, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = try_move(&mut ctx, id, fixed_units(128), fixed_units(140));
        assert!(!report.ok, "25 units is one too many");
    }

    #[test]
    fn dropoff_of_24_units_is_allowed() {
        let map = split_map(-24, 128, ML_TWOSIDED, 0);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 128, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = try_move(&mut ctx, id, fixed_units(128), fixed_units(140));
        assert!(report.ok, "24 units of dropoff is exactly fine");
    }

    #[test]
    fn dropoff_of_25_units_blocks_without_dropoff_flag() {
        let map = split_map(-25, 128, ML_TWOSIDED, 0);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 128, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = try_move(&mut ctx, id, fixed_units(128), fixed_units(140));
        assert!(!report.ok, "25 units of dropoff is one too many");

        // with MF_DROPOFF the same move is legal
        if let Some(mo) = ctx.store.mobj_mut(id) {
            mo.flags |= MF_DROPOFF;
        }
        let report = try_move(&mut ctx, id, fixed_units(128), fixed_units(140));
        assert!(report.ok);
    }

    #[test]
    fn try_move_into_closed_cap_fails_on_height() {
        let map = closed_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 128, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = try_move(&mut ctx, id, fixed_units(128), fixed_units(140));
        assert!(!report.ok, "window 0 is smaller than any mobj");
        assert!(!report.floatok);
    }

    #[test]
    fn closing_ceiling_window_smaller_than_mobj_blocks() {
        // window 48 > 0 but smaller than the 56 unit player
        let map = split_map(0, 48, ML_TWOSIDED, 0);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_PLAYER, 128, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = try_move(&mut ctx, id, fixed_units(128), fixed_units(140));
        assert!(!report.ok);
    }

    #[test]
    fn closing_ceiling_window_of_exactly_mobj_height_is_fine() {
        // window 56 == player height exactly
        let map = split_map(0, 56, ML_TWOSIDED, 0);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_PLAYER, 128, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = try_move(&mut ctx, id, fixed_units(128), fixed_units(140));
        assert!(report.ok);
    }

    #[test]
    fn monster_blocks_monster() {
        let (map, mut store, mut rng, mover) = possessed_in_open_room(64, 64);
        let _target = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 100, 64);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = try_move(&mut ctx, mover, fixed_units(96), fixed_units(64));
        assert!(!report.ok);
    }

    #[test]
    fn moving_thing_does_not_block_itself() {
        let (map, mut store, mut rng, id) = possessed_in_open_room(64, 64);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        // sliding almost onto the same spot is fine
        let report = try_move(&mut ctx, id, fixed_units(64), fixed_units(66));
        assert!(report.ok);
    }

    #[test]
    fn thing_in_unextended_box_is_still_found_via_maxradius_extension() {
        // mover destination box alone does not reach the target, but the
        // MAXRADIUS-extended thing search does
        let (map, mut store, mut rng, mover) = possessed_in_open_room(100, 64);
        let _target = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 139, 64);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = try_move(&mut ctx, mover, fixed_units(100), fixed_units(64));
        assert!(!report.ok, "extended search must find the neighbour");
    }

    #[test]
    fn missile_passes_over_monster() {
        let (map, mut store, mut rng, _m) = possessed_in_open_room(128, 64);
        let target = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 128, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        // missile above the 56 unit monster (floor 0)
        let id = ctx.store.spawn_mobj(
            ctx.map,
            ctx.rng,
            crate::mobj::SpawnSpec {
                skill: crate::mobj::Skill::Medium,
                x: fixed_units(128),
                y: fixed_units(100),
                z: crate::mobj::SpawnZ::At(fixed_units(100)),
                mobj_type: MobjType::MT_TROOPSHOT,
            },
        );
        let report = check_position(&mut ctx, id, fixed_units(128), fixed_units(100));
        assert!(report.ok, "missile over the monster does not hit it");
        let _ = target;
    }

    #[test]
    fn missile_hits_monster_and_reports_damage() {
        let (map, mut store, mut rng, _m) = possessed_in_open_room(128, 64);
        let target = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 128, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        // missile at head height of the monster (monster z=0, height 56)
        let id = ctx.store.spawn_mobj(
            ctx.map,
            ctx.rng,
            crate::mobj::SpawnSpec {
                skill: crate::mobj::Skill::Medium,
                x: fixed_units(128),
                y: fixed_units(100),
                z: crate::mobj::SpawnZ::At(fixed_units(20)),
                mobj_type: MobjType::MT_TROOPSHOT,
            },
        );
        let report = check_position(&mut ctx, id, fixed_units(128), fixed_units(100));
        assert!(!report.ok);
        let damage_events: Vec<_> = report
            .events
            .iter()
            .filter_map(|event| match event {
                MoveEvent::Damage { target, damage, .. } => Some((*target, *damage)),
                _ => None,
            })
            .collect();
        assert_eq!(damage_events.len(), 1);
        assert_eq!(damage_events[0].0, target);
        // ((P_Random() % 8) + 1) * damage(3)
        assert!((3..=24).contains(&damage_events[0].1));
    }

    #[test]
    fn missile_flies_over_own_shooter() {
        let (map, mut store, mut rng, _m) = possessed_in_open_room(128, 64);
        let bruiser = spawn_at(&mut store, &map, &mut rng, MobjType::MT_BRUISER, 128, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let id = ctx.store.spawn_mobj(
            ctx.map,
            ctx.rng,
            crate::mobj::SpawnSpec {
                skill: crate::mobj::Skill::Medium,
                x: fixed_units(128),
                y: fixed_units(100),
                z: crate::mobj::SpawnZ::At(fixed_units(0)),
                mobj_type: MobjType::MT_BRUISERSHOT,
            },
        );
        if let Some(mo) = ctx.store.mobj_mut(id) {
            mo.target = Some(bruiser);
        }
        // the missile passes over the thing it was fired from
        let report = check_position(&mut ctx, id, fixed_units(128), fixed_units(100));
        assert!(report.ok);
    }

    #[test]
    fn bruiser_missile_explodes_on_knight_without_damage() {
        let (map, mut store, mut rng, _m) = possessed_in_open_room(128, 64);
        let shooter = spawn_at(&mut store, &map, &mut rng, MobjType::MT_BRUISER, 100, 100);
        let knight = spawn_at(&mut store, &map, &mut rng, MobjType::MT_KNIGHT, 112, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let id = ctx.store.spawn_mobj(
            ctx.map,
            ctx.rng,
            crate::mobj::SpawnSpec {
                skill: crate::mobj::Skill::Medium,
                x: fixed_units(112),
                y: fixed_units(100),
                z: crate::mobj::SpawnZ::At(fixed_units(0)),
                mobj_type: MobjType::MT_BRUISERSHOT,
            },
        );
        if let Some(mo) = ctx.store.mobj_mut(id) {
            mo.target = Some(shooter);
        }
        let report = check_position(&mut ctx, id, fixed_units(112), fixed_units(100));
        assert!(!report.ok, "the missile stops on the knight");
        // but the sibling species takes no damage
        assert!(
            report.events.iter().all(
                |event| !matches!(event, MoveEvent::Damage { target, .. } if *target == knight)
            ),
            "bruiser missiles never damage knights"
        );
    }

    #[test]
    fn skullfly_slams_thing_and_clears_flag() {
        let (map, mut store, mut rng, _m) = possessed_in_open_room(128, 64);
        let target = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 110, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let skull = ctx.store.spawn_mobj(
            ctx.map,
            ctx.rng,
            crate::mobj::SpawnSpec {
                skill: crate::mobj::Skill::Medium,
                x: fixed_units(110),
                y: fixed_units(100),
                z: crate::mobj::SpawnZ::At(fixed_units(0)),
                mobj_type: MobjType::MT_SKULL,
            },
        );
        if let Some(mo) = ctx.store.mobj_mut(skull) {
            mo.flags |= MF_SKULLFLY;
            mo.momx = fixed_units(6);
            mo.momy = fixed_units(0);
        }
        let report = check_position(&mut ctx, skull, fixed_units(110), fixed_units(100));
        assert!(!report.ok);
        let damage = report
            .events
            .iter()
            .find_map(|event| match event {
                MoveEvent::Damage { target, damage, .. } => Some((*target, *damage)),
                _ => None,
            })
            .expect("the slam must deal damage");
        assert_eq!(damage.0, target);
        // ((P_Random() % 8) + 1) * damage(3)
        assert!((3..=24).contains(&damage.1));
        let mo = ctx.store.mobj(skull).expect("skull still present");
        assert_eq!(mo.flags & MF_SKULLFLY, 0);
        assert_eq!(mo.momx, Fixed::ZERO);
        assert_eq!(mo.momy, Fixed::ZERO);
        assert_eq!(
            mo.state,
            crate::info::MOBJ_INFO[MobjType::MT_SKULL as usize].spawnstate
        );
    }

    #[test]
    fn player_missile_hits_other_player() {
        let (map, mut store, mut rng, _m) = possessed_in_open_room(128, 64);
        let other = spawn_at(&mut store, &map, &mut rng, MobjType::MT_PLAYER, 104, 100);
        let shooter = spawn_at(&mut store, &map, &mut rng, MobjType::MT_PLAYER, 88, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let id = ctx.store.spawn_mobj(
            ctx.map,
            ctx.rng,
            crate::mobj::SpawnSpec {
                skill: crate::mobj::Skill::Medium,
                x: fixed_units(104),
                y: fixed_units(100),
                z: crate::mobj::SpawnZ::At(fixed_units(0)),
                mobj_type: MobjType::MT_TROOPSHOT,
            },
        );
        if let Some(mo) = ctx.store.mobj_mut(id) {
            mo.target = Some(shooter);
        }
        let report = check_position(&mut ctx, id, fixed_units(104), fixed_units(100));
        assert!(!report.ok);
        assert!(
            report
                .events
                .iter()
                .any(|event| matches!(event, MoveEvent::Damage { target, .. } if *target == other)),
            "players may missile other players"
        );
    }

    #[test]
    fn special_item_is_picked_up_with_pickup_flag() {
        let (map, mut store, mut rng, _m) = possessed_in_open_room(128, 64);
        let clip = spawn_at(&mut store, &map, &mut rng, MobjType::MT_CLIP, 72, 100);
        let player = spawn_at(&mut store, &map, &mut rng, MobjType::MT_PLAYER, 64, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = try_move(&mut ctx, player, fixed_units(72), fixed_units(100));
        assert!(report.ok, "the item does not block the player");
        assert!(
            report
                .events
                .iter()
                .any(|event| matches!(event, MoveEvent::Pickup { special, toucher } if *special == clip && *toucher == player)),
            "the touch must surface as a Pickup event"
        );
        // the deferred event has not consumed the item yet
        assert!(ctx.store.mobj(clip).is_some());
    }

    #[test]
    fn monster_does_not_pick_up_items() {
        let (map, mut store, mut rng, _m) = possessed_in_open_room(128, 64);
        let _clip = spawn_at(&mut store, &map, &mut rng, MobjType::MT_CLIP, 72, 100);
        let monster = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 64, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = try_move(&mut ctx, monster, fixed_units(72), fixed_units(100));
        assert!(report.ok);
        assert!(
            report.events.is_empty(),
            "monsters have no MF_PICKUP, no pickup event"
        );
    }

    #[test]
    fn crossing_special_line_is_reported_once_backwards_order() {
        // divider special 42: crossing south->north fires the crossing event
        let map = split_map(0, 128, ML_TWOSIDED, 42);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 128, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = try_move(&mut ctx, id, fixed_units(128), fixed_units(140));
        assert!(report.ok);
        let crossings: Vec<_> = report
            .events
            .iter()
            .filter_map(|event| match event {
                MoveEvent::CrossSpecial { line, side, .. } => Some((*line, *side)),
                _ => None,
            })
            .collect();
        assert_eq!(crossings, vec![(4, 0)], "the line is crossed from side 0");
    }

    #[test]
    fn divider_lands_in_specials_exactly_once() {
        // the divider is listed in three blockmap cells; the special must
        // still be recorded once, like vanilla's validcount dedup
        let map = split_map(0, 128, ML_TWOSIDED, 42);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 128, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = check_position(&mut ctx, id, fixed_units(128), fixed_units(140));
        assert!(report.ok);
        assert_eq!(report.specials, vec![4]);
    }

    #[test]
    fn teleport_flag_suppresses_special_crossing() {
        let map = split_map(0, 128, ML_TWOSIDED, 42);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 128, 100);
        if let Some(mo) = store.mobj_mut(id) {
            mo.flags |= MF_TELEPORT;
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = try_move(&mut ctx, id, fixed_units(128), fixed_units(140));
        assert!(report.ok);
        assert!(report.events.is_empty(), "teleports cross no specials");
    }

    #[test]
    fn monster_blocked_by_monster_block_line() {
        let map = split_map(0, 128, ML_TWOSIDED | ML_BLOCKMONSTERS, 0);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 128, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = try_move(&mut ctx, id, fixed_units(128), fixed_units(140));
        assert!(!report.ok, "ML_BLOCKMONSTERS stops monsters");
    }

    #[test]
    fn player_passes_monster_block_line() {
        let map = split_map(0, 128, ML_TWOSIDED | ML_BLOCKMONSTERS, 0);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_PLAYER, 128, 100);
        // the mover must act like a player
        if let Some(mo) = store.mobj_mut(id) {
            mo.player = Some(0);
        }
        let mut players = [test_player()];
        let mut ctx = MoveContext::new(&map, &mut store, &mut players, &mut rng);
        let report = try_move(&mut ctx, id, fixed_units(128), fixed_units(140));
        assert!(report.ok, "players ignore ML_BLOCKMONSTERS");
    }

    #[test]
    fn blocking_line_stops_players_too() {
        let map = split_map(0, 128, ML_TWOSIDED | ML_BLOCKING, 0);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_PLAYER, 128, 100);
        if let Some(mo) = store.mobj_mut(id) {
            mo.player = Some(0);
        }
        let mut players = [test_player()];
        let mut ctx = MoveContext::new(&map, &mut store, &mut players, &mut rng);
        let report = try_move(&mut ctx, id, fixed_units(128), fixed_units(140));
        assert!(!report.ok, "ML_BLOCKING stops everyone");
    }

    #[test]
    fn noclip_walks_through_walls() {
        let (map, mut store, mut rng, id) = possessed_in_open_room(64, 64);
        if let Some(mo) = store.mobj_mut(id) {
            mo.flags |= MF_NOCLIP;
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = try_move(&mut ctx, id, fixed_units(-20), fixed_units(64));
        assert!(report.ok, "NOCLIP ignores the wall");
        let mo = ctx.store.mobj(id).unwrap();
        assert_eq!(mo.x, fixed_units(-20));
    }

    #[test]
    fn corpse_still_blocks() {
        let (map, mut store, mut rng, mover) = possessed_in_open_room(64, 64);
        let corpse = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 96, 64);
        // death clears MF_SHOOTABLE but keeps MF_SOLID
        if let Some(mo) = store.mobj_mut(corpse) {
            mo.flags &= !crate::movement::MF_SHOOTABLE;
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = try_move(&mut ctx, mover, fixed_units(90), fixed_units(64));
        assert!(!report.ok, "corpses are still solid");
    }

    #[test]
    fn teleport_move_stomps_monster() {
        let (map, mut store, mut rng, _m) = possessed_in_open_room(64, 64);
        let victim = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 112, 100);
        let mover = spawn_at(&mut store, &map, &mut rng, MobjType::MT_PLAYER, 64, 64);
        // the avatar is a player, not just an MT_PLAYER-typed thing
        if let Some(mo) = store.mobj_mut(mover) {
            mo.player = Some(0);
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = teleport_move(&mut ctx, mover, fixed_units(112), fixed_units(100), false);
        assert!(report.ok);
        assert_eq!(
            ctx.store.mobj(mover).unwrap().x,
            fixed_units(112),
            "the teleport lands exactly on the victim's cell"
        );
        assert!(
            report
                .events
                .iter()
                .any(|event| matches!(event, MoveEvent::Damage { target, damage: 10000, .. } if *target == victim)),
            "teleporting onto a monster stomps it"
        );
    }

    #[test]
    fn teleport_move_monster_does_not_stomp_off_boss() {
        let (map, mut store, mut rng, mover) = possessed_in_open_room(64, 64);
        let victim = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 112, 100);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = teleport_move(&mut ctx, mover, fixed_units(112), fixed_units(100), false);
        assert!(!report.ok, "monsters may not stomp off the boss level");
        assert_eq!(ctx.store.mobj(mover).unwrap().x, fixed_units(64));
        let report = teleport_move(&mut ctx, mover, fixed_units(112), fixed_units(100), true);
        assert!(report.ok, "on the boss level monsters may stomp");
        let _ = victim;
    }
}
