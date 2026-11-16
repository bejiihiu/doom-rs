//! The `P_PathTraverse` machinery from `p_maputl.c`: trace a line from A to
//! B through the blockmap, gather every line and mobj the segment crosses,
//! then call back for each in distance order.
//!
//! The original fills a static `intercepts[MAXINTERCEPTS]` during the walk
//! and runs a selection sort on it afterwards. This port collects into a
//! growing `Vec` and uses a stable sort by `frac` — for equal fractions the
//! original's selection picks the insert-earliest entry every time, which is
//! exactly what a stable sort does. Hit order is therefore identical to the
//! original, including the ties.
//!
//! `PT_EARLYOUT` is honoured during the collection phase exactly like
//! vanilla: a solid (single-sided) line reached before `FRACUNIT` stops the
//! whole trace immediately, before any callback runs.

use std::collections::HashSet;

use crate::fixed::{FRACBITS, FRACUNIT, Fixed};
use crate::map::{Map, lines_in_box};
use crate::math::{Divline, Point, point_on_divline_side, point_on_side};
use crate::thinker::ThinkerId;

use super::MoveContext;

/// Vanilla `PT_ADDLINES`.
pub const PT_ADDLINES: u32 = 1;
/// Vanilla `PT_ADDTHINGS`.
pub const PT_ADDTHINGS: u32 = 2;
/// Vanilla `PT_EARLYOUT`.
pub const PT_EARLYOUT: u32 = 4;

/// Shift from fixed-point map units to blockmap cells (vanilla
/// `MAPBLOCKSHIFT - FRACBITS`).
const MAPBTOFRAC: u32 = 7;
/// Vanilla `MAPBLOCKSHIFT`.
const MAPBLOCKSHIFT: u32 = FRACBITS + 7;

/// What an [`Intercept`] hit. Vanilla uses a union; here the payload is a
/// plain enum and the line is a linedef index into [`Map::linedefs`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InterceptPayload {
    /// A linedef, as an index into [`Map::linedefs`].
    Line(usize),
    /// A mobj.
    Thing(ThinkerId),
}

/// One intercept along the trace (vanilla `intercept_t`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Intercept {
    /// Fractional distance along the trace, `0..=FRACUNIT` for points on the
    /// segment (vanilla also stores larger values during collection; they
    /// are filtered out at dispatch time).
    pub frac: Fixed,
    /// What was hit.
    pub payload: InterceptPayload,
}

/// Vanilla `P_MakeDivline`, inlined: a divline for a linedef.
pub(crate) fn divline_for(map: &Map, line: usize) -> Divline {
    let linedef = &map.linedefs[line];
    let v1 = &map.vertexes[linedef.v1];
    Divline {
        x: v1.x,
        y: v1.y,
        dx: linedef.dx,
        dy: linedef.dy,
    }
}

/// Vanilla `P_InterceptVector`: the fractional intercept point along `v2`
/// of `v2` against `v1` (both divlines). Returns a value in fixed point;
/// parallel lines return zero, as in the original.
pub(crate) fn intercept_vector(v2: &Divline, v1: &Divline) -> Fixed {
    // vanilla keeps 8 extra fraction bits on one side of the products
    let den =
        Fixed::from_raw(v1.dy.to_raw() >> 8) * v2.dx - Fixed::from_raw(v1.dx.to_raw() >> 8) * v2.dy;

    if den.is_zero() {
        return Fixed::ZERO;
    }

    let num = Fixed::from_raw((v1.x - v2.x).to_raw() >> 8) * v1.dy
        + Fixed::from_raw((v2.y - v1.y).to_raw() >> 8) * v1.dx;

    num / den
}

/// Vanilla `P_LineOpening`: the vertical window through a two-sided line.
/// Single-sided lines report a zero-width window (the original left its
/// globals stale; callers never read them in that case).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LineOpening {
    /// Lowest ceiling of the two sectors.
    pub top: Fixed,
    /// Highest floor of the two sectors.
    pub bottom: Fixed,
    /// `top - bottom`; the clear height through the line.
    pub range: Fixed,
    /// The lower of the two floors (vanilla `lowfloor`).
    pub low_floor: Fixed,
}

/// Vanilla `P_LineOpening` as a pure function of the map.
pub fn line_opening(map: &Map, line: usize) -> LineOpening {
    let linedef = &map.linedefs[line];

    let (Some(front), Some(back)) = (linedef.frontsector, linedef.backsector) else {
        // single sided: no window, and the low floor is meaningless
        return LineOpening {
            top: Fixed::ZERO,
            bottom: Fixed::ZERO,
            range: Fixed::ZERO,
            low_floor: Fixed::ZERO,
        };
    };

    let front = &map.sectors[front];
    let back = &map.sectors[back];

    let top = front.ceiling_height.min(back.ceiling_height);
    let bottom = front.floor_height.max(back.floor_height);
    let low_floor = front.floor_height.min(back.floor_height);

    LineOpening {
        top,
        bottom,
        range: top - bottom,
        low_floor,
    }
}

/// One cell's worth of lines, deduplicated against `seen` (the per-trace
/// `validcount`). Returns `false` when vanilla would call the callback with
/// a false verdict (early out on a solid line).
fn add_line_intercepts(
    ctx: &MoveContext,
    line: usize,
    trace: &Divline,
    early_out: bool,
    intercepts: &mut Vec<Intercept>,
    seen: &mut HashSet<usize>,
) -> bool {
    // the original stamps lines across the whole trace so a line present in
    // several blockmap cells is only considered once
    if !seen.insert(line) {
        return true;
    }

    let map = ctx.map;
    let linedef = &map.linedefs[line];

    // avoid precision problems with two routines

    let (s1, s2) = if trace.dx > Fixed::from_int(16)
        || trace.dy > Fixed::from_int(16)
        || trace.dx < Fixed::from_int(-16)
        || trace.dy < Fixed::from_int(-16)
    {
        (
            point_on_divline_side(
                map.vertexes[linedef.v1].x,
                map.vertexes[linedef.v1].y,
                trace,
            ),
            point_on_divline_side(
                map.vertexes[linedef.v2].x,
                map.vertexes[linedef.v2].y,
                trace,
            ),
        )
    } else {
        let line_divline = divline_for(map, line);
        (
            point_on_side(trace.x, trace.y, &line_divline),
            point_on_side(trace.x + trace.dx, trace.y + trace.dy, &line_divline),
        )
    };

    if s1 == s2 {
        // line isn't crossed
        return true;
    }

    // hit the line
    let line_divline = divline_for(map, line);
    let frac = intercept_vector(trace, &line_divline);

    if frac < Fixed::ZERO {
        // behind source
        return true;
    }

    // try to early out the check
    if early_out && frac < Fixed::from_raw(FRACUNIT) && linedef.backsector.is_none() {
        return false; // stop checking
    }

    intercepts.push(Intercept {
        frac,
        payload: InterceptPayload::Line(line),
    });

    true
}

/// Vanilla `PIT_AddThingIntercepts`.
fn add_thing_intercepts(
    ctx: &MoveContext,
    thing: ThinkerId,
    trace: &Divline,
    intercepts: &mut Vec<Intercept>,
) -> bool {
    let Some(mobj) = ctx.store.mobj(thing) else {
        return true;
    };

    let tracepositive = (trace.dx.to_raw() ^ trace.dy.to_raw()) > 0;

    // check a corner to corner crossection for hit
    let (x1, y1, x2, y2) = if tracepositive {
        (
            mobj.x - mobj.radius,
            mobj.y + mobj.radius,
            mobj.x + mobj.radius,
            mobj.y - mobj.radius,
        )
    } else {
        (
            mobj.x - mobj.radius,
            mobj.y - mobj.radius,
            mobj.x + mobj.radius,
            mobj.y + mobj.radius,
        )
    };

    let s1 = point_on_divline_side(x1, y1, trace);
    let s2 = point_on_divline_side(x2, y2, trace);

    if s1 == s2 {
        // thing isn't crossed
        return true;
    }

    let dl = Divline {
        x: x1,
        y: y1,
        dx: x2 - x1,
        dy: y2 - y1,
    };

    let frac = intercept_vector(trace, &dl);

    if frac < Fixed::ZERO {
        // behind source
        return true;
    }

    intercepts.push(Intercept {
        frac,
        payload: InterceptPayload::Thing(thing),
    });

    true
}

/// Vanilla `P_TraverseIntercepts` with `maxfrac == FRACUNIT` (the only
/// call site in the original). Returns true when every intercept in range
/// dispatched to `true`.
fn traverse_intercepts(
    ctx: &mut MoveContext,
    intercepts: &mut [Intercept],
    callback: &mut impl FnMut(&mut MoveContext, &Intercept) -> bool,
) -> bool {
    // stable sort: equal fractions keep their collection order,
    // matching the original's first-come-first-served selection
    intercepts.sort_by_key(|in_| in_.frac.to_raw());

    for intercept in intercepts.iter() {
        if intercept.frac > Fixed::from_raw(FRACUNIT) {
            // checked everything in range
            return true;
        }
        if !callback(ctx, intercept) {
            return false;
        }
    }

    true
}

/// Vanilla `P_PathTraverse`: trace a line from `from` to `to`, calling
/// `callback` for each crossed line/thing, nearest first, until the
/// callback says stop. Returns true when every hit in range was accepted
/// by the callback (vanilla's boolean return).
///
/// `flags` is a bitset of [`PT_ADDLINES`], [`PT_ADDTHINGS`] and
/// [`PT_EARLYOUT`]. The callback receives the shared [`MoveContext`] so it
/// can do whatever the vanilla `PIT_*` functions did inline.
pub fn traverse_path(
    ctx: &mut MoveContext,
    from: Point,
    to: Point,
    flags: u32,
    mut callback: impl FnMut(&mut MoveContext, &Intercept) -> bool,
) -> bool {
    let map = ctx.map;
    let grid = &map.blockmap;

    let early_out = flags & PT_EARLYOUT != 0;

    let mut x1 = from.x;
    let mut y1 = from.y;
    let x2 = to.x;
    let y2 = to.y;

    if ((x1.to_raw() as i64 - grid.x_origin.to_raw() as i64) & 0x007f_ffff) == 0 {
        x1 += Fixed::from_raw(FRACUNIT); // don't start exactly on a line
    }
    if ((y1.to_raw() as i64 - grid.y_origin.to_raw() as i64) & 0x007f_ffff) == 0 {
        y1 += Fixed::from_raw(FRACUNIT);
    }

    let trace = Divline {
        x: x1,
        y: y1,
        dx: x2 - x1,
        dy: y2 - y1,
    };

    // the cell walk works on origin-relative coordinates
    let rx1 = x1.to_raw() - grid.x_origin.to_raw();
    let ry1 = y1.to_raw() - grid.y_origin.to_raw();
    let rx2 = x2.to_raw() - grid.x_origin.to_raw();
    let ry2 = y2.to_raw() - grid.y_origin.to_raw();

    let xt1 = rx1 >> MAPBLOCKSHIFT;
    let yt1 = ry1 >> MAPBLOCKSHIFT;
    let xt2 = rx2 >> MAPBLOCKSHIFT;
    let yt2 = ry2 >> MAPBLOCKSHIFT;

    let mapxstep;
    let mapystep;

    let partial_x = if xt2 > xt1 {
        mapxstep = 1;
        Fixed::from_raw(FRACUNIT - ((rx1 >> MAPBTOFRAC) & (FRACUNIT - 1)))
    } else if xt2 < xt1 {
        mapxstep = -1;
        Fixed::from_raw((rx1 >> MAPBTOFRAC) & (FRACUNIT - 1))
    } else {
        mapxstep = 0;
        Fixed::from_raw(FRACUNIT)
    };
    let abs_fixed =
        |v: Fixed| Fixed::from_raw((v.to_raw() as i64).abs().min(i64::from(i32::MAX)) as i32);
    let ystep = if mapxstep == 0 {
        // unused; avoids a FixedDiv by a zero delta
        Fixed::from_raw(256 * FRACUNIT)
    } else {
        (y2 - y1) / abs_fixed(x2 - x1)
    };

    let partial_y = if yt2 > yt1 {
        mapystep = 1;
        Fixed::from_raw(FRACUNIT - ((ry1 >> MAPBTOFRAC) & (FRACUNIT - 1)))
    } else if yt2 < yt1 {
        mapystep = -1;
        Fixed::from_raw((ry1 >> MAPBTOFRAC) & (FRACUNIT - 1))
    } else {
        mapystep = 0;
        Fixed::from_raw(FRACUNIT)
    };
    let xstep = if mapystep == 0 {
        // unused; avoids a FixedDiv by a zero delta
        Fixed::from_raw(256 * FRACUNIT)
    } else {
        (x2 - x1) / abs_fixed(y2 - y1)
    };

    let mut yintercept = Fixed::from_raw(ry1 >> MAPBTOFRAC) + partial_x * ystep;
    let mut xintercept = Fixed::from_raw(rx1 >> MAPBTOFRAC) + partial_y * xstep;

    let mut intercepts: Vec<Intercept> = Vec::new();
    let mut seen_lines: HashSet<usize> = HashSet::new();

    let cols = i32::from(grid.columns);
    let rows = i32::from(grid.rows);

    let mut mapx = xt1;
    let mut mapy = yt1;

    // step through the map blocks; count prevents an infinite loop on
    // rounding errors skipping the break
    for _count in 0..64 {
        if mapx >= 0 && mapy >= 0 && mapx < cols && mapy < rows {
            let cell = usize::try_from(mapy * cols + mapx).expect("cell index in range");

            if flags & PT_ADDLINES != 0 {
                if let Some(lines) = grid.cell_lines(mapx as u16, mapy as u16) {
                    // collect first so the callback can borrow the context
                    let cell_lines: Vec<usize> = lines.iter().map(|&l| usize::from(l)).collect();
                    for line in cell_lines {
                        if !add_line_intercepts(
                            ctx,
                            line,
                            &trace,
                            early_out,
                            &mut intercepts,
                            &mut seen_lines,
                        ) {
                            return false; // early out
                        }
                    }
                }
            }

            if flags & PT_ADDTHINGS != 0 {
                let cell_things: Vec<ThinkerId> = ctx.store.blockmap_members(cell).collect();
                for thing in cell_things {
                    if !add_thing_intercepts(ctx, thing, &trace, &mut intercepts) {
                        return false; // early out (unreachable: things never early out)
                    }
                }
            }
        }

        if mapx == xt2 && mapy == yt2 {
            break;
        }

        if (yintercept.to_raw() >> FRACBITS) == mapy {
            yintercept += ystep;
            mapx += mapxstep;
        } else if (xintercept.to_raw() >> FRACBITS) == mapx {
            xintercept += xstep;
            mapy += mapystep;
        }
    }

    traverse_intercepts(ctx, &mut intercepts, &mut callback)
}

/// Box query for the check code: linedef indices in the cells overlapping
/// `bbox`, in vanilla traversal order, deduplicated. Duplicates would let a
/// line land in the special-lines list twice, which is not vanilla.
pub(crate) fn lines_near(map: &Map, bbox: &crate::math::BBox) -> Vec<usize> {
    let mut seen = HashSet::new();
    let unit = crate::map::UnitBox::from_fixed(bbox);
    lines_in_box(map, unit)
        .filter(|line| seen.insert(*line))
        .collect()
}

/// Box query for the check code: thinker ids of mobjs in the cells
/// overlapping `bbox`, in vanilla traversal order. Things are linked in
/// exactly one cell, so no dedup is needed.
pub(crate) fn things_near(ctx: &MoveContext, bbox: &crate::math::BBox) -> Vec<ThinkerId> {
    let grid = &ctx.map.blockmap;
    let unit = crate::map::UnitBox::from_fixed(bbox);
    let Some(range) = crate::map::block_range(grid, unit) else {
        return Vec::new();
    };

    let col_count = usize::from(grid.columns);
    let mut things = Vec::new();
    for x in range.left..=range.right {
        for y in range.bottom..=range.top {
            let cell = usize::from(y) * col_count + usize::from(x);
            things.extend(ctx.store.blockmap_members(cell));
        }
    }
    things
}

#[cfg(test)]
mod tests {
    //! Tests for the path tracing code. Coordinates are map units.
    use super::*;

    use crate::fixed::{FRACUNIT, Fixed};
    use crate::info::MobjType;

    use crate::rng::DoomRng;

    use super::super::MoveContext;
    use super::super::testutil::{fixture, spawn_at};

    fn u(units: i32) -> Fixed {
        Fixed::from_int(units)
    }

    fn point(x: i32, y: i32) -> Point {
        Point { x: u(x), y: u(y) }
    }

    /// Collect all intercepts in order (callback returns true).
    fn collect_intercepts(
        ctx: &mut MoveContext,
        from: Point,
        to: Point,
        flags: u32,
    ) -> Vec<Intercept> {
        let mut hits = Vec::new();
        traverse_path(ctx, from, to, flags, |_ctx, intercept| {
            hits.push(*intercept);
            true
        });
        hits
    }

    #[test]
    fn intercept_vector_perpendicular_is_half() {
        // trace from (0,0) to (256,0), line at (128,0)-(128,256): halfway
        let trace = Divline {
            x: u(0),
            y: u(0),
            dx: u(256),
            dy: u(0),
        };
        let line = Divline {
            x: u(128),
            y: u(0),
            dx: u(0),
            dy: u(256),
        };
        let frac = intercept_vector(&trace, &line);
        assert_eq!(frac, Fixed::from_raw(FRACUNIT / 2));
    }

    #[test]
    fn intercept_vector_parallel_is_zero() {
        let trace = Divline {
            x: u(0),
            y: u(0),
            dx: u(256),
            dy: u(0),
        };
        let line = Divline {
            x: u(0),
            y: u(64),
            dx: u(128),
            dy: u(0),
        };
        let frac = intercept_vector(&trace, &line);
        assert_eq!(frac, Fixed::ZERO);
    }

    #[test]
    fn trace_crosses_the_divider_and_reports_it() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let hits = collect_intercepts(&mut ctx, point(128, 100), point(128, 200), PT_ADDLINES);
        let lines: Vec<_> = hits
            .iter()
            .filter_map(|in_| match in_.payload {
                InterceptPayload::Line(line) => Some(line),
                _ => None,
            })
            .collect();
        assert_eq!(lines, vec![4], "the divider is line 4");
        // frac ≈ (128-100)/(200-100) = 0.28
        let frac = hits[0].frac.to_raw();
        let expected = Fixed::from_raw(28 * FRACUNIT / 100).to_raw();
        assert!(
            (frac - expected).abs() < FRACUNIT / 100,
            "frac {frac} ≈ {expected}"
        );
    }

    #[test]
    fn trace_finds_things_nearest_first() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let near = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 128, 64);
        let far = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 200, 64);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let hits = collect_intercepts(&mut ctx, point(32, 64), point(240, 64), PT_ADDTHINGS);
        let things: Vec<_> = hits
            .iter()
            .filter_map(|in_| match in_.payload {
                InterceptPayload::Thing(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(things, vec![near, far]);
    }

    #[test]
    fn trace_hits_lines_and_things_in_frac_order() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        // a monster in the path, plus the walls at the far ends
        let near = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 96, 64);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let hits = collect_intercepts(
            &mut ctx,
            point(32, 64),
            point(240, 64),
            PT_ADDLINES | PT_ADDTHINGS,
        );
        // the monster is at frac ≈ 0.3; it must come before any wall
        let first = hits.first().expect("at least the monster");
        assert_eq!(first.payload, InterceptPayload::Thing(near));
    }

    #[test]
    fn trace_ignores_intercepts_beyond_the_segment() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        // the north wall at y = 256 is past the end of this trace
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let hits = collect_intercepts(&mut ctx, point(64, 64), point(240, 64), PT_ADDLINES);
        assert!(hits.is_empty(), "nothing crosses the segment");
    }

    #[test]
    fn trace_behind_the_source_is_ignored() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let behind = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 10, 64);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let hits = collect_intercepts(&mut ctx, point(32, 64), point(200, 64), PT_ADDTHINGS);
        assert!(
            hits.iter()
                .all(|in_| matches!(in_.payload, InterceptPayload::Thing(id) if id != behind)),
            "a thing behind the trace is not an intercept"
        );
    }

    #[test]
    fn callback_returning_false_stops_dispatch() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let _first = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 96, 64);
        let _second = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 180, 64);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let mut seen = 0;
        let ok = traverse_path(
            &mut ctx,
            point(32, 64),
            point(240, 64),
            PT_ADDTHINGS,
            |_ctx, _intercept| {
                seen += 1;
                false
            },
        );
        assert!(!ok);
        assert_eq!(seen, 1, "the second thing is never dispatched");
    }

    #[test]
    fn line_listed_in_multiple_cells_is_added_once() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        // the trace crosses the divider and walks cells (0,1) and (1,1) on
        // the way — the divider is listed in both; it must be collected once
        let hits = collect_intercepts(&mut ctx, point(10, 120), point(240, 140), PT_ADDLINES);
        let divider_hits = hits
            .iter()
            .filter(|in_| matches!(in_.payload, InterceptPayload::Line(4)))
            .count();
        assert_eq!(divider_hits, 1, "the divider is deduplicated per trace");
    }

    #[test]
    fn earlyout_solid_line_stops_the_trace_before_dispatch() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        // the west wall (line 3) is solid; heading west the trace hits it
        // before anything else — and nothing is ever dispatched
        let mut seen = 0;
        let ok = traverse_path(
            &mut ctx,
            point(64, 64),
            point(-100, 64),
            PT_ADDLINES | PT_ADDTHINGS | PT_EARLYOUT,
            |_ctx, _intercept| {
                seen += 1;
                true
            },
        );
        assert!(!ok, "earlyout stops the trace");
        assert_eq!(seen, 0, "no intercept is dispatched past a solid line");
    }

    #[test]
    fn trace_ending_inside_a_cell_works() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let near = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 96, 64);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        // a very short trace, entirely inside one cell
        let hits = collect_intercepts(&mut ctx, point(64, 64), point(110, 64), PT_ADDTHINGS);
        let things: Vec<_> = hits
            .iter()
            .filter_map(|in_| match in_.payload {
                InterceptPayload::Thing(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(things, vec![near]);
    }

    #[test]
    fn line_opening_single_sided_is_zero_range() {
        let (map, _store) = fixture();
        let opening = line_opening(&map, 0); // L0, single sided
        assert_eq!(opening.range, Fixed::ZERO);
    }

    #[test]
    fn line_opening_mixed_heights_picks_the_narrow_window() {
        let map = crate::movement::testutil::split_map(16, 48, crate::map::types::ML_TWOSIDED, 0);
        let opening = line_opening(&map, 4); // the divider
        // top = min(128, 48) = 48, bottom = max(0, 16) = 16
        assert_eq!(opening.top, u(48));
        assert_eq!(opening.bottom, u(16));
        assert_eq!(opening.range, u(32));
        // the lower floor is the south sector's
        assert_eq!(opening.low_floor, u(0));
    }
}
