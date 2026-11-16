//! Line-of-sight checks, ported from `p_sight.c` (P_CheckSight and the
//! helpers that don't escape the file).
//!
//! Two layers of occlusion:
//!
//! 1. the REJECT lump — a `numsectors * numsectors` bit matrix in the map;
//!    a set bit means "no possible LOS between these sectors", decided
//!    without touching geometry.
//! 2. the BSP walk — from the viewpoint's subsector to the target's,
//!    checking every line whose front and back side sectors the trace
//!    crosses, closing in on a [top, bottom] slope window. A fully closed
//!    door (`openbottom >= opentop`) or a collapsing slope window
//!    (`topslope <= bottomslope`) blocks sight.
//!
//! `validcount` becomes a per-call `HashSet` of linedef indices (the `Map`
//! is immutable, so a stamp can't live on the linedef).

use crate::fixed::{FRACBITS, Fixed};
use crate::map::types::ML_TWOSIDED;
use crate::map::{Map, NodeChild};
use crate::math::Divline;
use crate::mobj::Mobj;
use crate::movement::intercepts::intercept_vector;
use std::collections::HashSet;

/// Vanilla `P_DivlineSide`, returning 0 (front), 1 (back) or 2 (on).
///
/// Deliberately *not* [`crate::math::point_on_divline_side`]: the sight
/// code truncates the line and delta components to integers before the
/// multiply (`>> FRACBITS`, both operands), which is a different precision
/// class with its own quirks. Keep it byte-faithful.
fn divline_side(x: Fixed, y: Fixed, node: &Divline) -> i32 {
    if node.dx.is_zero() {
        if x.to_raw() == node.x.to_raw() {
            return 2;
        }

        if x <= node.x {
            return i32::from(node.dy > Fixed::ZERO);
        }

        return i32::from(node.dy < Fixed::ZERO);
    }

    if node.dy.is_zero() {
        // vanilla compares x == node->y here (a slip in the original:
        // should be y == node->y). Kept 1:1 on purpose.
        if x.to_raw() == node.y.to_raw() {
            return 2;
        }

        if y <= node.y {
            return i32::from(node.dx < Fixed::ZERO);
        }

        return i32::from(node.dx > Fixed::ZERO);
    }

    let dx = x - node.x;
    let dy = y - node.y;

    let left = (node.dy.to_raw() >> FRACBITS).wrapping_mul(dx.to_raw() >> FRACBITS);
    let right = (dy.to_raw() >> FRACBITS).wrapping_mul(node.dx.to_raw() >> FRACBITS);

    if right < left {
        return 0; // front side
    }

    if left == right {
        return 2;
    }
    1 // back side
}

/// The pieces `P_CrossSubsector`/`P_CrossBSPNode` thread through the
/// traversal. A local value instead of vanilla's file-scope globals, so
/// nested calls can't leak `topslope` into each other.
struct SightState<'a> {
    map: &'a Map,
    /// From t1 to t2 (vanilla `strace`).
    strace: Divline,
    /// Absolute target position (vanilla `t2x`/`t2y`).
    t2x: Fixed,
    t2y: Fixed,
    /// Eye z of the looker (vanilla `sightzstart`).
    sightzstart: Fixed,
    topslope: Fixed,
    bottomslope: Fixed,
    /// Linedef indices already checked (vanilla `validcount` stamps).
    seen_lines: HashSet<usize>,
}

fn cross_subsector(state: &mut SightState<'_>, num: usize) -> bool {
    let subsector = &state.map.subsectors[num];
    let first = subsector.first_seg;
    let count = subsector.num_segs;
    let segs = &state.map.segs[first..first + count];

    for seg in segs {
        let line = seg.linedef;

        // already checked the other side?
        if !state.seen_lines.insert(line) {
            continue;
        }

        let linedef = &state.map.linedefs[line];
        let v1 = &state.map.vertexes[linedef.v1];
        let v2 = &state.map.vertexes[linedef.v2];

        let s1 = divline_side(v1.x, v1.y, &state.strace);
        let s2 = divline_side(v2.x, v2.y, &state.strace);

        // line isn't crossed?
        if s1 == s2 {
            continue;
        }

        let divl = Divline {
            x: v1.x,
            y: v1.y,
            dx: v2.x - v1.x,
            dy: v2.y - v1.y,
        };
        let s1 = divline_side(state.strace.x, state.strace.y, &divl);
        let s2 = divline_side(state.t2x, state.t2y, &divl);

        // line isn't crossed?
        if s1 == s2 {
            continue;
        }

        // stop because it is not two sided anyway
        if linedef.flags & ML_TWOSIDED == 0 {
            return false;
        }

        // crosses a two sided line
        let front = &state.map.sectors[seg.frontsector];
        // seg.backsector is Some for every two-sided seg in a validated
        // map; skip defensively instead of trusting it blindly
        let Some(back_index) = seg.backsector else {
            continue;
        };
        let back = &state.map.sectors[back_index];

        // no wall to block sight with?
        if front.floor_height == back.floor_height && front.ceiling_height == back.ceiling_height {
            continue;
        }

        // possible occluder
        // because of ceiling height differences
        let opentop = front.ceiling_height.min(back.ceiling_height);
        // because of ceiling height differences
        let openbottom = front.floor_height.max(back.floor_height);

        // quick test for totally closed doors
        if openbottom >= opentop {
            return false; // stop
        }

        let frac = intercept_vector(&state.strace, &divl);

        if front.floor_height != back.floor_height {
            let slope = (openbottom - state.sightzstart) / frac;
            if slope > state.bottomslope {
                state.bottomslope = slope;
            }
        }

        if front.ceiling_height != back.ceiling_height {
            let slope = (opentop - state.sightzstart) / frac;
            if slope < state.topslope {
                state.topslope = slope;
            }
        }

        if state.topslope <= state.bottomslope {
            return false; // stop
        }
    }

    // passed the subsector ok
    true
}

fn cross_node(state: &mut SightState<'_>, num: usize) -> bool {
    let node = state.map.nodes[num];
    let partition = Divline {
        x: node.x,
        y: node.y,
        dx: node.dx,
        dy: node.dy,
    };

    // decide which side the start point is on
    let mut side = divline_side(state.strace.x, state.strace.y, &partition);
    if side == 2 {
        side = 0; // an "on" should cross both sides
    }

    // cross the starting side
    if !cross_child(state, node.child(side as usize)) {
        return false;
    }

    // the partition plane is crossed here
    if side == divline_side(state.t2x, state.t2y, &partition) {
        // the line doesn't touch the other side
        return true;
    }

    // cross the ending side
    cross_child(state, node.child((side ^ 1) as usize))
}

fn cross_child(state: &mut SightState<'_>, child: NodeChild) -> bool {
    match child {
        NodeChild::SubSector(index) => cross_subsector(state, index),
        NodeChild::Node(index) => cross_node(state, index),
    }
}

/// Vanilla `P_CheckSight`.
///
/// Returns `true` when a straight line between the eyes of `from` and any
/// part of `to` is unobstructed. Uses the REJECT matrix first, then the
/// BSP walk with the sloped window test.
pub fn check_sight(map: &Map, from: &Mobj, to: &Mobj) -> bool {
    let (Some(s1), Some(s2)) = (from.sector, to.sector) else {
        // vanilla would read the sector through the subsector; a degenerate
        // subsector means we can't resolve REJECT bits, so refuse sight
        tracing::warn!("P_CheckSight: degenerate subsector without a sector");
        return false;
    };

    // First check for trivial rejection.
    let numsectors = map.sectors.len();
    let pnum = s1 * numsectors + s2;
    let bytenum = pnum >> 3;
    let bitnum = 1u8 << (pnum & 7);

    // Check in REJECT table. A short/absent reject lump can't be indexed;
    // treat the pair as visible rather than panic (the loader normally
    // guarantees the size).
    if let Some(byte) = map.reject.get(bytenum) {
        if byte & bitnum != 0 {
            return false;
        }
    }

    // An unobstructed LOS is possible.
    // Now look from eyes of t1 to any part of t2.
    let sightzstart = from.z + from.height - Fixed::from_raw(from.height.to_raw() >> 2);
    let mut state = SightState {
        map,
        strace: Divline {
            x: from.x,
            y: from.y,
            dx: to.x - from.x,
            dy: to.y - from.y,
        },
        t2x: to.x,
        t2y: to.y,
        sightzstart,
        topslope: (to.z + to.height) - sightzstart,
        bottomslope: to.z - sightzstart,
        seen_lines: HashSet::new(),
    };

    // the head node is the last node output
    if map.nodes.is_empty() {
        // vanilla's `numnodes - 1 == -1` case: walk subsector 0
        return cross_subsector(&mut state, 0);
    }
    cross_node(&mut state, map.nodes.len() - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::enemy::testutil::{open_map, players, spawn_player, spawn_thing, split_map};
    use crate::info::MobjType;
    use crate::map::types::{ML_BLOCKING, ML_TWOSIDED};
    use crate::mobj::MobjStore;
    use crate::rng::DoomRng;

    #[test]
    fn sight_is_open_in_a_plain_room() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);

        let from = store.mobj(monster).expect("monster spawned");
        let to = store.mobj(player).expect("player spawned");
        assert!(check_sight(&map, from, to));
    }

    #[test]
    fn reject_blocks_sight_south_to_north() {
        let map = split_map(0, 128, ML_TWOSIDED, 0x02); // bit 1: 0*2+1
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 192);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);

        let from = store.mobj(monster).expect("monster");
        let to = store.mobj(player).expect("player");
        assert!(!check_sight(&map, from, to));

        // ...but the opposite direction is open
        let from = store.mobj(player).expect("player");
        let to = store.mobj(monster).expect("monster");
        assert!(check_sight(&map, from, to));
    }

    #[test]
    fn reject_blocks_sight_north_to_south() {
        let map = split_map(0, 128, ML_TWOSIDED, 0x04); // bit 2: 1*2+0
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 192);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);

        let from = store.mobj(player).expect("player");
        let to = store.mobj(monster).expect("monster");
        assert!(!check_sight(&map, from, to));
    }

    #[test]
    fn one_sided_divider_blocks_sight() {
        let map = split_map(0, 128, ML_BLOCKING, 0x00); // solid wall, no opening
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 192);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);

        let from = store.mobj(monster).expect("monster");
        let to = store.mobj(player).expect("player");
        assert!(!check_sight(&map, from, to));
    }

    #[test]
    fn closed_door_divider_blocks_sight() {
        // north sector floor 0 ceiling 0: the two-sided divider is fully
        // closed (openbottom >= opentop)
        let map = split_map(0, 0, ML_TWOSIDED, 0x00);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 192);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);

        let from = store.mobj(monster).expect("monster");
        let to = store.mobj(player).expect("player");
        assert!(!check_sight(&map, from, to));
    }

    #[test]
    fn step_up_twenty_lets_sight_through() {
        let map = split_map(20, 128, ML_TWOSIDED, 0x00);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 192);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);

        let from = store.mobj(monster).expect("monster");
        let to = store.mobj(player).expect("player");
        assert!(check_sight(&map, from, to));
    }

    #[test]
    fn hundred_unit_ledge_blocks_sight() {
        let map = split_map(100, 128, ML_TWOSIDED, 0x00);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 192);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);

        let from = store.mobj(monster).expect("monster");
        let to = store.mobj(player).expect("player");
        assert!(!check_sight(&map, from, to));
    }
}
