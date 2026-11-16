//! The shared `p_spec.c` queries: sector-from-line-tag search, adjacent
//! sector lookup and the "surrounding" min/max scans doors and lights
//! rely on.

use crate::fixed::Fixed;
use crate::map::{Map, types::ML_TWOSIDED};

/// Vanilla `getNextSector`: the sector on the other side of `line`, or
/// `None` when the line is not two-sided. `None` for one-sided lines.
pub(crate) fn get_next_sector(map: &Map, line: usize, sector: usize) -> Option<usize> {
    let linedef = &map.linedefs[line];
    if (linedef.flags & ML_TWOSIDED) == 0 {
        return None;
    }
    if linedef.frontsector == Some(sector) {
        return linedef.backsector;
    }
    linedef.frontsector
}

/// Indices of every line linedef that touches `sector` on either side.
/// Vanilla kept `sector->lines[]` from the map build; here the scan is
/// built on demand — these queries run at map load time and on rare
/// trigger calls, not per-tic.
pub(crate) fn sector_lines(map: &Map, sector: usize) -> Vec<usize> {
    (0..map.linedefs.len())
        .filter(|&index| {
            let line = &map.linedefs[index];
            line.frontsector == Some(sector) || line.backsector == Some(sector)
        })
        .collect()
}

/// Vanilla `P_FindSectorFromLineTag`. Returns the next sector whose tag
/// matches `tag`, starting the search after `start`. Callers iterate:
///
/// ```ignore
/// let mut cursor = None;
/// while let Some(sector) = find_sector_from_line_tag(map, tag, cursor) {
///     cursor = Some(sector);
///     // ...
/// }
/// ```
pub(crate) fn find_sector_from_line_tag(
    map: &Map,
    tag: i16,
    start: Option<usize>,
) -> Option<usize> {
    let from = start.map(|s| s + 1).unwrap_or(0);
    (from..map.sectors.len()).find(|&i| map.sectors[i].tag == tag)
}

/// Vanilla `P_FindLowestFloorSurrounding`.
pub(crate) fn find_lowest_floor_surrounding(map: &Map, sector: usize) -> Fixed {
    let mut floor = map.sectors[sector].floor_height;
    for index in sector_lines(map, sector) {
        if let Some(other) = get_next_sector(map, index, sector)
            && map.sectors[other].floor_height < floor
        {
            floor = map.sectors[other].floor_height;
        }
    }
    floor
}

/// Vanilla `P_FindHighestFloorSurrounding`.
pub(crate) fn find_highest_floor_surrounding(map: &Map, sector: usize) -> Fixed {
    let mut floor = Fixed::from_raw(-500 * crate::fixed::FRACUNIT);
    for index in sector_lines(map, sector) {
        if let Some(other) = get_next_sector(map, index, sector)
            && map.sectors[other].floor_height > floor
        {
            floor = map.sectors[other].floor_height;
        }
    }
    floor
}

/// Vanilla `P_FindNextHighestFloor`: the smallest floor height among the
/// surrounding sectors that is strictly higher than `currentheight`, or
/// `currentheight` when there is none.
pub(crate) fn find_next_highest_floor(map: &Map, sector: usize, currentheight: Fixed) -> Fixed {
    const MAX_ADJOINING_SECTORS: usize = 20;

    let mut heights: Vec<Fixed> = Vec::with_capacity(MAX_ADJOINING_SECTORS);
    for index in sector_lines(map, sector) {
        let Some(other) = get_next_sector(map, index, sector) else {
            continue;
        };
        let height = map.sectors[other].floor_height;
        if height > currentheight {
            heights.push(height);
        }
        // vanilla logs and stops at 20 adjoining sectors
        if heights.len() >= MAX_ADJOINING_SECTORS {
            tracing::warn!("sector with more than 20 adjoining sectors");
            break;
        }
    }
    heights.into_iter().min().unwrap_or(currentheight)
}

/// Vanilla `P_FindLowestCeilingSurrounding`. Returns `i32::MAX` when the
/// sector has no two-sided lines (vanilla did the same, `MAXINT`).
pub(crate) fn find_lowest_ceiling_surrounding(map: &Map, sector: usize) -> Fixed {
    let mut height = Fixed::from_raw(i32::MAX);
    for index in sector_lines(map, sector) {
        if let Some(other) = get_next_sector(map, index, sector)
            && map.sectors[other].ceiling_height < height
        {
            height = map.sectors[other].ceiling_height;
        }
    }
    height
}

/// Vanilla `P_FindHighestCeilingSurrounding`.
pub(crate) fn find_highest_ceiling_surrounding(map: &Map, sector: usize) -> Fixed {
    let mut height = Fixed::ZERO;
    for index in sector_lines(map, sector) {
        if let Some(other) = get_next_sector(map, index, sector)
            && map.sectors[other].ceiling_height > height
        {
            height = map.sectors[other].ceiling_height;
        }
    }
    height
}

/// Vanilla `P_FindMinSurroundingLight`: the lowest light level among the
/// surrounding sectors, or `max` when the sector stands alone.
pub(crate) fn find_min_surrounding_light(map: &Map, sector: usize, max: i32) -> i32 {
    let mut min = max;
    for index in sector_lines(map, sector) {
        if let Some(other) = get_next_sector(map, index, sector) {
            let light = i32::from(map.sectors[other].light_level);
            if light < min {
                min = light;
            }
        }
    }
    min
}

/// The bounding box of every line linedef touching `sector`, expanded by
/// vanilla `MAXRADIUS`. This is the Rust stand-in for the
/// `sector->blockbox` vanilla precomputed in `p_setup.c`; `None` when the
/// sector has no lines (nothing to clip against).
pub(crate) fn sector_union_box(map: &Map, sector: usize) -> Option<(Fixed, Fixed, Fixed, Fixed)> {
    const MAXRADIUS: i32 = 32 * crate::fixed::FRACUNIT;

    let mut left = None;
    let mut bottom = None;
    let mut right = None;
    let mut top = None;
    for index in sector_lines(map, sector) {
        let bbox = &map.linedefs[index].bbox;
        let (l, b, r, t) = (bbox.left, bbox.bottom, bbox.right, bbox.top);
        left = min_opt(left, l);
        bottom = min_opt(bottom, b);
        right = max_opt(right, r);
        top = max_opt(top, t);
    }
    match (left, bottom, right, top) {
        (Some(l), Some(b), Some(r), Some(t)) => Some((
            Fixed::from_raw(l.to_raw() - MAXRADIUS),
            Fixed::from_raw(b.to_raw() - MAXRADIUS),
            Fixed::from_raw(r.to_raw() + MAXRADIUS),
            Fixed::from_raw(t.to_raw() + MAXRADIUS),
        )),
        _ => None,
    }
}

/// The pairwise min/max accumulation helpers for the union box.
fn min_opt(a: Option<Fixed>, b: Fixed) -> Option<Fixed> {
    Some(a.map_or(b, |v| v.min(b)))
}

fn max_opt(a: Option<Fixed>, b: Fixed) -> Option<Fixed> {
    Some(a.map_or(b, |v| v.max(b)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::specials::testutil::split_map;

    #[test]
    fn find_sector_from_line_tag_walks_all_matches() {
        let map = split_map(0, 128, 42);
        // south sector has tag 0, north has tag 42
        assert_eq!(find_sector_from_line_tag(&map, 42, None), Some(1));
        assert_eq!(find_sector_from_line_tag(&map, 42, Some(1)), None);
        assert_eq!(find_sector_from_line_tag(&map, 7, None), None);
        // tag 0 matches the south sector only
        assert_eq!(find_sector_from_line_tag(&map, 0, None), Some(0));
        assert_eq!(find_sector_from_line_tag(&map, 0, Some(0)), None);
    }

    #[test]
    fn get_next_sector_handles_sidedness() {
        let map = split_map(0, 128, 0);
        // divider line (index 4) is two-sided
        assert_eq!(get_next_sector(&map, 4, 0), Some(1));
        assert_eq!(get_next_sector(&map, 4, 1), Some(0));
        // south wall is one-sided
        assert_eq!(get_next_sector(&map, 0, 0), None);
    }

    #[test]
    fn surrounding_scans_pick_the_extremes() {
        let map = split_map(20, 96, 0);
        // south sector (0): floor 0, ceiling 128; north: floor 20, ceiling 96
        assert_eq!(find_lowest_floor_surrounding(&map, 0), Fixed::ZERO);
        assert_eq!(
            find_lowest_floor_surrounding(&map, 1),
            Fixed::ZERO,
            "north sector sees the lower south floor"
        );
        assert_eq!(
            find_lowest_ceiling_surrounding(&map, 1),
            Fixed::from_int(128),
            "north sector sees the higher south ceiling"
        );
        assert_eq!(
            find_min_surrounding_light(&map, 0, 200),
            176,
            "both sectors are lit at 176"
        );
    }

    #[test]
    fn highest_and_next_highest_floor() {
        let map = split_map(72, 128, 0);
        assert_eq!(find_highest_floor_surrounding(&map, 0), Fixed::from_int(72));
        assert_eq!(
            find_next_highest_floor(&map, 0, Fixed::ZERO),
            Fixed::from_int(72),
            "picks the smallest floor above the current one"
        );
        assert_eq!(
            find_next_highest_floor(&map, 0, Fixed::from_int(72)),
            Fixed::from_int(72),
            "no higher floor: falls back to the given height"
        );
    }

    #[test]
    fn highest_ceiling_surrounding() {
        let map = split_map(0, 96, 0);
        assert_eq!(
            find_highest_ceiling_surrounding(&map, 1),
            Fixed::from_int(128)
        );
        assert_eq!(
            find_highest_ceiling_surrounding(&map, 0),
            Fixed::from_int(96),
            "the south room's only neighbor is the low north sector"
        );
    }

    #[test]
    fn sector_union_box_covers_lines_expanded_by_max_radius() {
        let map = split_map(0, 128, 0);
        let (left, bottom, right, top) = sector_union_box(&map, 0).expect("sector has lines");
        // south sector lines: south (0,0)-(256,0), east (256,256)-(256,0),
        // west (0,0)-(0,256), divider (0,128)-(256,128)
        assert_eq!(left, Fixed::from_raw(-32 * crate::fixed::FRACUNIT));
        assert_eq!(bottom, Fixed::from_raw(-32 * crate::fixed::FRACUNIT));
        assert_eq!(right, Fixed::from_raw(288 * crate::fixed::FRACUNIT));
        assert_eq!(top, Fixed::from_raw(288 * crate::fixed::FRACUNIT));
    }
}
