//! Sliding along walls, ported from `p_map.c` (`P_SlideMove`,
//! `PTR_SlideTraverse`, `P_HitSlideLine`).
//!
//! The idea: when a move is blocked, trace the three leading corners of the
//! thing's bounding box along the movement vector, find the closest blocking
//! line, advance up to it (minus a small fudge so the thing never ends up
//! exactly touching the wall), then clip the remaining movement to the wall
//! direction and try again. Up to three iterations, then a stair-step
//! fallback (axis-aligned partial move) gives up.
//!
//! The original kept `bestslidefrac`/`bestslideline` (and a never-read
//! `secondslide*` pair) in file scope; here they live in `SlideState`,
//! created fresh per call. The vestigial second-best tracking is dropped
//! (nothing in linuxdoom-1.10 ever reads it).

use crate::fixed::{FRACUNIT, Fixed};
use crate::map::types::{ML_TWOSIDED, SlopeType};
use crate::math::{Point, angle_delta, aprox_distance, point_on_side};
use crate::tables::{ANGLE_TO_FINE_SHIFT, Angle, FINE_COSINE, FINE_SINE};
use crate::thinker::ThinkerId;

use super::MoveContext;
use super::MovementReport;
use super::check::try_move;
use super::intercepts::{
    Intercept, InterceptPayload, PT_ADDLINES, divline_for, line_opening, traverse_path,
};

/// The slide bookkeeping (vanilla `bestslidefrac`/`bestslideline`/
/// `slidemo`/`tmxmove`/`tmymove`).
struct SlideState {
    /// Vanilla `bestslidefrac`: fraction of the movement up to the blocking line.
    best_frac: Fixed,
    /// Vanilla `bestslideline`: blocking linedef index.
    best_line: Option<usize>,
    /// The moving thing.
    slidemo: ThinkerId,
    /// Clipped movement for this call.
    xmove: Fixed,
    /// Clipped movement for this call.
    ymove: Fixed,
}

/// Vanilla `PTR_SlideTraverse`: keeps the closest blocking line. Returns
/// false (stop) when the line blocks, true (continue) otherwise.
fn slide_traverse(ctx: &mut MoveContext, intercept: &Intercept, state: &mut SlideState) -> bool {
    let InterceptPayload::Line(line) = intercept.payload else {
        // a thing payload is impossible here — the trace only adds lines
        return true;
    };

    let map = ctx.map;
    let linedef = &map.linedefs[line];

    if linedef.flags & ML_TWOSIDED == 0 {
        let line_divline = divline_for(map, line);
        let Some(mover) = ctx.store.mobj(state.slidemo) else {
            return true;
        };
        if point_on_side(mover.x, mover.y, &line_divline) != 0 {
            // don't hit the back side
            return true;
        }
        // the line blocks
        return slide_line_blocks(intercept.frac, line, state);
    }

    // set openrange, opentop, openbottom
    let opening = line_opening(map, line);

    let Some(mover) = ctx.store.mobj(state.slidemo) else {
        return true;
    };
    if opening.range < mover.height {
        return slide_line_blocks(intercept.frac, line, state); // doesn't fit
    }
    if opening.top - mover.z < mover.height {
        return slide_line_blocks(intercept.frac, line, state); // mobj is too high
    }
    if opening.bottom - mover.z > Fixed::from_raw(24 << 16) {
        return slide_line_blocks(intercept.frac, line, state); // too big a step up
    }

    // this line doesn't block movement
    true
}

/// Shared tail of `PTR_SlideTraverse`'s blocking paths.
fn slide_line_blocks(frac: Fixed, line: usize, state: &mut SlideState) -> bool {
    if frac < state.best_frac {
        state.best_frac = frac;
        state.best_line = Some(line);
    }
    false // stop
}

/// Vanilla `P_HitSlideLine`: adjust the x/y move so the next move slides
/// along the wall.
fn hit_slide_line(ctx: &MoveContext, line: usize, state: &mut SlideState) {
    let Some(mover) = ctx.store.mobj(state.slidemo) else {
        state.xmove = Fixed::ZERO;
        state.ymove = Fixed::ZERO;
        return;
    };
    let map = ctx.map;
    let linedef = &map.linedefs[line];

    if linedef.slopetype == SlopeType::Horizontal {
        state.ymove = Fixed::ZERO;
        return;
    }

    if linedef.slopetype == SlopeType::Vertical {
        state.xmove = Fixed::ZERO;
        return;
    }

    let line_divline = divline_for(map, line);
    let side = point_on_side(mover.x, mover.y, &line_divline);

    let mut lineangle =
        crate::bsp::point_to_angle_2(Fixed::ZERO, Fixed::ZERO, linedef.dx, linedef.dy);

    if side == 1 {
        lineangle += Angle::ANG180;
    }

    let moveangle =
        crate::bsp::point_to_angle_2(Fixed::ZERO, Fixed::ZERO, state.xmove, state.ymove);
    let mut deltaangle = angle_delta(moveangle, lineangle);

    if deltaangle.to_raw() > Angle::ANG180.to_raw() {
        deltaangle += Angle::ANG180;
    }

    // fine angle indices: a BAM shifted down 19 bits lands in 0..=8191
    let line_fine = (lineangle.to_raw() >> ANGLE_TO_FINE_SHIFT) as usize;
    let delta_fine = (deltaangle.to_raw() >> ANGLE_TO_FINE_SHIFT) as usize;

    let movelen = aprox_distance(state.xmove, state.ymove);
    let newlen = movelen * FINE_COSINE[delta_fine];

    state.xmove = newlen * FINE_COSINE[line_fine];
    state.ymove = newlen * FINE_SINE[line_fine];
}

/// Vanilla `P_SlideMove`. The momentum is bad, so try to slide along a
/// wall: find the first line hit, move flush to it, and slide along it.
pub fn slide_move(ctx: &mut MoveContext, id: ThinkerId) -> MovementReport {
    let mut report = MovementReport::new();

    let mut state = SlideState {
        best_frac: Fixed::from_raw(FRACUNIT + 1),
        best_line: None,
        slidemo: id,
        xmove: Fixed::ZERO,
        ymove: Fixed::ZERO,
    };

    let mut hitcount = 0;

    loop {
        hitcount += 1;
        if hitcount == 3 {
            // don't loop forever
            return stairstep(ctx, id, &mut report);
        }

        let Some(mo) = ctx.store.mobj(id) else {
            return report;
        };

        // trace along the three leading corners
        let (leadx, trailx) = if mo.momx > Fixed::ZERO {
            (mo.x + mo.radius, mo.x - mo.radius)
        } else {
            (mo.x - mo.radius, mo.x + mo.radius)
        };
        let (leady, traily) = if mo.momy > Fixed::ZERO {
            (mo.y + mo.radius, mo.y - mo.radius)
        } else {
            (mo.y - mo.radius, mo.y + mo.radius)
        };
        let momx = mo.momx;
        let momy = mo.momy;

        // reset per attempt (vanilla cleared only the frac; a stale line was
        // unreachable when frac stayed at FRACUNIT+1)
        state.best_frac = Fixed::from_raw(FRACUNIT + 1);
        state.best_line = None;

        let corner_lead = Point { x: leadx, y: leady };
        let corner_trail_y = Point {
            x: trailx,
            y: leady,
        };
        let corner_trail_x = Point {
            x: leadx,
            y: traily,
        };
        let shift = Point { x: momx, y: momy };

        traverse_path(
            ctx,
            corner_lead,
            Point {
                x: corner_lead.x + shift.x,
                y: corner_lead.y + shift.y,
            },
            PT_ADDLINES,
            |ctx, intercept| slide_traverse(ctx, intercept, &mut state),
        );
        traverse_path(
            ctx,
            corner_trail_y,
            Point {
                x: corner_trail_y.x + shift.x,
                y: corner_trail_y.y + shift.y,
            },
            PT_ADDLINES,
            |ctx, intercept| slide_traverse(ctx, intercept, &mut state),
        );
        traverse_path(
            ctx,
            corner_trail_x,
            Point {
                x: corner_trail_x.x + shift.x,
                y: corner_trail_x.y + shift.y,
            },
            PT_ADDLINES,
            |ctx, intercept| slide_traverse(ctx, intercept, &mut state),
        );

        // move up to the wall
        if state.best_frac == Fixed::from_raw(FRACUNIT + 1) {
            // the move must have hit the middle, so stairstep
            return stairstep(ctx, id, &mut report);
        }

        // fudge a bit to make sure it doesn't hit
        state.best_frac -= Fixed::from_raw(0x800);
        if state.best_frac > Fixed::ZERO {
            let newx = momx * state.best_frac;
            let newy = momy * state.best_frac;

            let Some(mo) = ctx.store.mobj(id) else {
                return report;
            };
            let try_report = try_move(ctx, id, mo.x + newx, mo.y + newy);
            if !try_report.ok {
                return stairstep(ctx, id, &mut report);
            }
            report.events.extend(try_report.events);
        }

        // Now continue along the wall.
        // First calculate remainder.
        state.best_frac = Fixed::from_raw(FRACUNIT) - (state.best_frac + Fixed::from_raw(0x800));

        if state.best_frac > Fixed::from_raw(FRACUNIT) {
            state.best_frac = Fixed::from_raw(FRACUNIT);
        }

        if state.best_frac <= Fixed::ZERO {
            return report;
        }

        state.xmove = momx * state.best_frac;
        state.ymove = momy * state.best_frac;

        let Some(best_line) = state.best_line else {
            // frac was below FRACUNIT+1, so a line was recorded
            return report;
        };
        hit_slide_line(ctx, best_line, &mut state);

        if let Some(mo) = ctx.store.mobj_mut(id) {
            mo.momx = state.xmove;
            mo.momy = state.ymove;
        }

        let Some(mo) = ctx.store.mobj(id) else {
            return report;
        };
        let try_report = try_move(ctx, id, mo.x + state.xmove, mo.y + state.ymove);
        report.events.extend(try_report.events);
        if !try_report.ok {
            continue; // retry
        }
        return report;
    }
}

/// Vanilla `stairstep`: give up sliding and just try the two axis-aligned
/// components separately.
fn stairstep(ctx: &mut MoveContext, id: ThinkerId, report: &mut MovementReport) -> MovementReport {
    let Some(mo) = ctx.store.mobj(id) else {
        return std::mem::take(report);
    };
    let try_vertical = try_move(ctx, id, mo.x, mo.y + mo.momy);
    report.events.extend(try_vertical.events);
    if !try_vertical.ok {
        let Some(mo) = ctx.store.mobj(id) else {
            return std::mem::take(report);
        };
        let try_horizontal = try_move(ctx, id, mo.x + mo.momx, mo.y);
        report.events.extend(try_horizontal.events);
    }
    std::mem::take(report)
}

#[cfg(test)]
mod tests {
    //! Tests for the wall-sliding code. Coordinates are map units.
    use super::*;

    use crate::fixed::Fixed;
    use crate::info::MobjType;
    use crate::mobj::MobjStore;
    use crate::rng::DoomRng;

    use super::super::testutil::{closed_map, fixture, spawn_at};

    fn u(units: i32) -> Fixed {
        Fixed::from_int(units)
    }

    #[test]
    fn diagonal_into_vertical_wall_slides_up_it() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 24, 64);
        if let Some(mo) = store.mobj_mut(id) {
            mo.momx = u(-8);
            mo.momy = u(8);
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = slide_move(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();

        assert_eq!(mo.momx, Fixed::ZERO, "the wall kills the x move");
        assert!(
            mo.momy > Fixed::ZERO,
            "the y move survives clipped to the remainder"
        );
        // the mobj ends up flush to the wall (radius 20, wall plane x = 0)
        assert!(mo.x <= u(24) && mo.x >= u(19), "x = {}", mo.x);
        assert!(mo.y > u(64), "it slid up the wall");
        let _ = &report;
    }

    #[test]
    fn diagonal_into_horizontal_wall_slides_along_it() {
        // the north sector has a closed ceiling, so the divider blocks
        let map = closed_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 64, 100);
        if let Some(mo) = store.mobj_mut(id) {
            mo.momx = u(6);
            mo.momy = u(28);
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = slide_move(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();

        assert_eq!(mo.momy, Fixed::ZERO, "a horizontal wall kills the y move");
        assert!(mo.momx > Fixed::ZERO, "the x move survives");
        // it stops flush to the divider and slides east, knocking y to the
        // last legal position (top of the box exactly on the divider)
        assert!(mo.y > u(100) && mo.y <= u(108), "y = {}", mo.y);
        let _ = &report;
    }

    #[test]
    fn blocked_only_by_a_thing_stair_steps_the_y_move() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let _wall = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 170, 112);
        let mover = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 122, 100);
        if let Some(mo) = store.mobj_mut(mover) {
            mo.momx = u(10);
            mo.momy = u(10);
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        // no line blocks this move, so the slide search finds nothing and
        // the stairstep fallback kicks in: y moves, x does not
        let report = slide_move(&mut ctx, mover);
        let mo = ctx.store.mobj(mover).unwrap();
        assert_eq!(mo.x, u(122), "x does not move on the y-first stairstep");
        assert_eq!(mo.y, u(110), "y steps forward");
        let _ = &report;
    }

    #[test]
    fn blocked_by_a_thing_with_no_axis_free_stays_put() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let _wall = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 150, 112);
        let mover = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 112, 100);
        if let Some(mo) = store.mobj_mut(mover) {
            mo.momx = u(10);
            mo.momy = u(10);
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = slide_move(&mut ctx, mover);
        let mo = ctx.store.mobj(mover).unwrap();
        assert_eq!(mo.x, u(112));
        assert_eq!(mo.y, u(100));
        let _ = &report;
    }
}
