//! Runtime blockmap queries on a loaded [`Map`], ported from
//! `p_maputl.c` (`P_BlockLinesIterator`) and the caller-side bound math
//! in `p_map.c` (`xl`/`xh`/`yl`/`yh`) and `p_setup.c` (`blockbox`
//! clamping).
//!
//! The grid cells are 128 units wide and square. A coordinate in map
//! units maps to a cell with a signed shift: `(coord - origin) >> 7`,
//! which is exactly `(fixed - bmaporg) >> (FRACBITS + 7)` with the
//! fraction bits folded out ahead of time. Arithmetic shifts floor, so
//! negative coordinates land in negative cells, like the original.
//!
//! Out-of-range cells are the caller's problem in the original (the
//! per-cell iterator silently returns "everything was checked"), which
//! this port replaces with an honest clamp: [`block_range`] narrows a
//! bounding box to the cells that exist and reports `None` when the box
//! lies entirely outside the grid instead of visiting invented cells.
//! A box that merely crosses the grid edge is clamped to the edge, like
//! the sector `blockbox` setup does.
//!
//! Duplicates: a linedef listed in several cells is yielded once per
//! cell. The original suppresses repeats across cells of one logical
//! pass with the `validcount` flag on `line_t`; that flag does not exist
//! yet (it lands with the movement code), so this iterator yields the
//! raw vanilla order: column by column (low x to high x), bottom to top
//! inside each column, then the cell's on-disk list order. Callers that
//! need unique lines dedup themselves; the caller that wants raw
//! vanilla behavior gets exactly the vanilla traversal.
//!
//! Out of scope: things. The THINGS lump is spawn data and is not
//! gridded; the thing blockmap in the original lives on `mobj_t` and is
//! rebuilt at runtime, and there are no mobj's here yet.

use crate::fixed::FRACBITS;
use crate::math::BBox;

use super::types::{BlockMapGrid, Map};

/// Shift from map units to cells: cells are 2^7 = 128 units wide.
const BLOCK_SHIFT_UNITS: u32 = 7;

/// A bounding box in whole map units, not fixed point.
///
/// Mirrors the `m_bbox` quartet; edges are inclusive. An inverted box
/// (`left > right` or `bottom > top`) covers nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitBox {
    /// Minimum X, inclusive.
    pub left: i32,
    /// Minimum Y, inclusive.
    pub bottom: i32,
    /// Maximum X, inclusive.
    pub right: i32,
    /// Maximum Y, inclusive.
    pub top: i32,
}

impl UnitBox {
    /// Builds a box from raw unit coordinates. Does not reorder an
    /// inverted box; inverted boxes cover nothing.
    pub const fn new(left: i32, bottom: i32, right: i32, top: i32) -> Self {
        Self {
            left,
            bottom,
            right,
            top,
        }
    }

    /// Converts a fixed-point box to whole units, flooring every edge.
    ///
    /// `Fixed::to_int` truncates toward zero, which floors only for
    /// positive values; grid math needs a floor everywhere, so the raw
    /// bits are shifted here instead.
    pub fn from_fixed(bbox: &BBox) -> Self {
        Self {
            left: bbox.left.to_raw() >> FRACBITS,
            bottom: bbox.bottom.to_raw() >> FRACBITS,
            right: bbox.right.to_raw() >> FRACBITS,
            top: bbox.top.to_raw() >> FRACBITS,
        }
    }
}

/// Grid origin in whole units (floor of the fixed-point origin).
pub fn origin_units(grid: &BlockMapGrid) -> (i32, i32) {
    (
        grid.x_origin.to_raw() >> FRACBITS,
        grid.y_origin.to_raw() >> FRACBITS,
    )
}

/// Signed cell index for one unit-space coordinate relative to
/// `origin_units`. Negative coordinates produce negative cells, like
/// the original's arithmetic `>> MAPBLOCKSHIFT`.
///
/// The math is done in 64 bits so the full `i32` unit range can be
/// queried without overflowing the subtraction.
pub fn block_index(coord: i32, origin_units: i32) -> i32 {
    block_index_i64(i64::from(coord), i64::from(origin_units)) as i32
}

fn block_index_i64(coord: i64, origin_units: i64) -> i64 {
    (coord - origin_units) >> BLOCK_SHIFT_UNITS
}

/// A clamped, inclusive range of grid cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockRange {
    /// First column, inclusive.
    pub left: u16,
    /// Last column, inclusive.
    pub right: u16,
    /// First row, inclusive.
    pub bottom: u16,
    /// Last row, inclusive.
    pub top: u16,
}

impl BlockRange {
    /// Number of columns the range covers.
    pub fn columns(&self) -> usize {
        usize::from(self.right - self.left) + 1
    }

    /// Number of rows the range covers.
    pub fn rows(&self) -> usize {
        usize::from(self.top - self.bottom) + 1
    }
}

/// Clamps a unit-space box to the cells of `grid`.
///
/// Returns `None` when the box covers no cell at all (inverted, or
/// entirely outside the grid). A box that straddles an edge is clamped
/// to the cells that exist, like the sector `blockbox` computation in
/// `p_setup.c`; the cells it yields are exactly the ones whose unit
/// range intersects the box.
pub fn block_range(grid: &BlockMapGrid, bbox: UnitBox) -> Option<BlockRange> {
    if bbox.left > bbox.right || bbox.bottom > bbox.top {
        return None;
    }

    let (origin_x, origin_y) = origin_units(grid);
    let left = block_index_i64(i64::from(bbox.left), i64::from(origin_x));
    let right = block_index_i64(i64::from(bbox.right), i64::from(origin_x));
    let bottom = block_index_i64(i64::from(bbox.bottom), i64::from(origin_y));
    let top = block_index_i64(i64::from(bbox.top), i64::from(origin_y));

    let columns = i64::from(grid.columns);
    let rows = i64::from(grid.rows);

    // Entirely past an edge: the original skips these cells, and so do
    // we. Clamping without this check would visit cells the box never
    // touches.
    if right < 0 || left >= columns || top < 0 || bottom >= rows {
        return None;
    }

    Some(BlockRange {
        left: left.clamp(0, columns - 1) as u16,
        right: right.clamp(0, columns - 1) as u16,
        bottom: bottom.clamp(0, rows - 1) as u16,
        top: top.clamp(0, rows - 1) as u16,
    })
}

/// True when `bbox` intersects at least one cell of `grid`.
///
/// Handy as a cheap early-out before walking the cells (movement and
/// sound code want to know whether the map grid can affect the query
/// at all).
pub fn box_overlaps_grid(grid: &BlockMapGrid, bbox: UnitBox) -> bool {
    block_range(grid, bbox).is_some()
}

/// Iterator over the linedef indices in the cells overlapping `bbox`.
///
/// See the module docs for ordering and duplicate semantics. Yields
/// nothing when the box is inverted or lies entirely outside the grid.
#[derive(Debug, Clone)]
pub struct LinesInBox<'m> {
    grid: &'m BlockMapGrid,
    range: Option<BlockRange>,
    x: u16,
    y: u16,
    cell: &'m [u16],
}

impl Iterator for LinesInBox<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        loop {
            if let Some((&line, rest)) = self.cell.split_first() {
                self.cell = rest;
                return Some(usize::from(line));
            }

            let range = self.range?;
            // vanilla walks columns outer, rows inner: for (bx) for (by)
            if self.y < range.top {
                self.y += 1;
            } else if self.x < range.right {
                self.x += 1;
                self.y = range.bottom;
            } else {
                self.range = None;
                return None;
            }
            self.cell = self.grid.cell_lines(self.x, self.y).unwrap_or(&[]);
        }
    }
}

impl core::iter::FusedIterator for LinesInBox<'_> {}

/// Lines in the cells overlapping `bbox`, in vanilla traversal order.
///
/// Yields map [`Map::linedefs`] indices; a line listed in several
/// cells is yielded once per cell. The pass-level dedup the original
/// does with `validcount` is deliberately left to the caller — see the
/// module docs.
pub fn lines_in_box(map: &Map, bbox: UnitBox) -> LinesInBox<'_> {
    let grid = &map.blockmap;
    match block_range(grid, bbox) {
        Some(range) => LinesInBox {
            grid,
            range: Some(range),
            x: range.left,
            y: range.bottom,
            cell: grid.cell_lines(range.left, range.bottom).unwrap_or(&[]),
        },
        None => LinesInBox {
            grid,
            range: None,
            x: 0,
            y: 0,
            cell: &[],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::fixed::Fixed;
    use crate::map::testutil::{blockmap_grid, linedefs, sectors, sidedefs, vertexes};
    use crate::map::types::ML_BLOCKING;
    use crate::map::{Map, MapLumps};

    /// A map whose blockmap is the given grid. `cells` entries are
    /// linedef-index lists per cell, row-major; referenced indices must
    /// point at linedefs that exist, so the fixture builds exactly as
    /// many dummy lines as the highest index needs.
    fn map_with(origin: (i16, i16), columns: i16, rows: i16, cells: &[&[i16]]) -> Map {
        let num_lines = cells
            .iter()
            .flat_map(|cell| cell.iter())
            .map(|&line| usize::try_from(line).map(|i| i + 1).unwrap_or(0))
            .max()
            .unwrap_or(0);

        let vertexes = vertexes(&[(0, 0), (1000, 1000)]);
        let linedefs = linedefs(&vec![(0, 1, ML_BLOCKING, 0, 0, 0, -1); num_lines]);
        let sidedefs = sidedefs(&vec![(0, 0, "-", "-", "WALL1", 0); num_lines]);
        let sectors = sectors(&[(0, 128, "FLOOR4_8", "CEIL3_5", 176, 0, 0)]);
        let blockmap = blockmap_grid(origin, columns, rows, cells);
        let lumps = MapLumps {
            vertexes: &vertexes,
            linedefs: &linedefs,
            sidedefs: &sidedefs,
            sectors: &sectors,
            segs: &[],
            subsectors: &[],
            nodes: &[],
            blockmap: &blockmap,
            reject: &[0x00],
            things: &[],
        };
        Map::load(&lumps).expect("synthetic map should load")
    }

    /// The standard 2x2 grid over (0..255, 0..255): cell (0,0) has lines
    /// 0,1; (1,0) has 1,2; (0,1) has 2; (1,1) has 0,2.
    fn two_by_two() -> Map {
        map_with((0, 0), 2, 2, &[&[0, 1], &[1, 2], &[2], &[0, 2]])
    }

    #[test]
    fn block_index_matches_signed_shift() {
        assert_eq!(block_index(0, 0), 0);
        assert_eq!(block_index(127, 0), 0);
        assert_eq!(block_index(128, 0), 1);
        assert_eq!(block_index(255, 0), 1);
        assert_eq!(block_index(256, 0), 2);
        assert_eq!(block_index(-1, 0), -1);
        assert_eq!(block_index(-100, 0), -1);
        assert_eq!(block_index(-128, 0), -1);
        assert_eq!(block_index(-129, 0), -2);
    }

    #[test]
    fn block_index_respects_origin() {
        // origin -8 shifts the grid so unit -8..119 lands in cell 0
        assert_eq!(block_index(-8, -8), 0);
        assert_eq!(block_index(0, -8), 0);
        assert_eq!(block_index(119, -8), 0);
        assert_eq!(block_index(120, -8), 1);
        assert_eq!(block_index(-9, -8), -1);
    }

    #[test]
    fn block_index_handles_full_i32_range_without_overflow() {
        assert_eq!(block_index(i32::MIN, 0), i32::MIN >> BLOCK_SHIFT_UNITS);
        assert_eq!(block_index(i32::MAX, 0), i32::MAX >> BLOCK_SHIFT_UNITS);
        // subtraction overflow is the trap this guards against
        assert_eq!(
            block_index(i32::MIN, i32::MAX),
            ((i64::from(i32::MIN) - i64::from(i32::MAX)) >> 7) as i32
        );
        assert_eq!(
            block_index(i32::MAX, i32::MIN),
            ((i64::from(i32::MAX) - i64::from(i32::MIN)) >> 7) as i32
        );
    }

    #[test]
    fn origin_units_floors_the_fixed_origin() {
        let map = map_with((-8, -8), 1, 1, &[&[]]);
        assert_eq!(origin_units(&map.blockmap), (-8, -8));
        let map = map_with((0, 0), 1, 1, &[&[]]);
        assert_eq!(origin_units(&map.blockmap), (0, 0));
    }

    #[test]
    fn block_range_single_cell() {
        let map = two_by_two();
        let range = block_range(&map.blockmap, UnitBox::new(10, 10, 20, 20)).unwrap();
        assert_eq!(
            range,
            BlockRange {
                left: 0,
                right: 0,
                bottom: 0,
                top: 0
            }
        );
        assert_eq!(range.columns(), 1);
        assert_eq!(range.rows(), 1);
    }

    #[test]
    fn block_range_spans_and_clamps() {
        let map = two_by_two();
        // whole grid, edges exactly on cell boundaries
        let range = block_range(&map.blockmap, UnitBox::new(0, 0, 255, 255)).unwrap();
        assert_eq!(
            range,
            BlockRange {
                left: 0,
                right: 1,
                bottom: 0,
                top: 1
            }
        );
        // box crossing the far edge clamps onto the edge cells
        let range = block_range(&map.blockmap, UnitBox::new(130, 130, 10_000, 10_000)).unwrap();
        assert_eq!(
            range,
            BlockRange {
                left: 1,
                right: 1,
                bottom: 1,
                top: 1
            }
        );
        // negative edges clamp onto cell 0
        let range = block_range(&map.blockmap, UnitBox::new(-10_000, -10_000, 255, 255)).unwrap();
        assert_eq!(
            range,
            BlockRange {
                left: 0,
                right: 1,
                bottom: 0,
                top: 1
            }
        );
    }

    #[test]
    fn block_range_cell_boundary_points() {
        let map = two_by_two();
        let at = |p: i32| block_range(&map.blockmap, UnitBox::new(p, p, p, p));
        assert_eq!(
            at(0).unwrap(),
            BlockRange {
                left: 0,
                right: 0,
                bottom: 0,
                top: 0
            }
        );
        assert_eq!(
            at(127).unwrap(),
            BlockRange {
                left: 0,
                right: 0,
                bottom: 0,
                top: 0
            }
        );
        assert_eq!(
            at(128).unwrap(),
            BlockRange {
                left: 1,
                right: 1,
                bottom: 1,
                top: 1
            }
        );
        assert_eq!(
            at(255).unwrap(),
            BlockRange {
                left: 1,
                right: 1,
                bottom: 1,
                top: 1
            }
        );
        // first unit past the grid on the far side: no cell at all
        assert_eq!(at(256), None);
    }

    #[test]
    fn block_range_entirely_outside_is_none() {
        let map = two_by_two();
        assert_eq!(
            block_range(&map.blockmap, UnitBox::new(-500, 0, -1, 100)),
            None
        );
        assert_eq!(
            block_range(&map.blockmap, UnitBox::new(256, 0, 400, 100)),
            None
        );
        assert_eq!(
            block_range(&map.blockmap, UnitBox::new(0, -500, 100, -1)),
            None
        );
        assert_eq!(
            block_range(&map.blockmap, UnitBox::new(0, 256, 100, 400)),
            None
        );
    }

    #[test]
    fn block_range_straddling_edge_clamps() {
        let map = two_by_two();
        // left edge negative, right edge inside: lowest cell, not None
        let range = block_range(&map.blockmap, UnitBox::new(-10, -10, 130, 130)).unwrap();
        assert_eq!(
            range,
            BlockRange {
                left: 0,
                right: 1,
                bottom: 0,
                top: 1
            }
        );
    }

    #[test]
    fn block_range_inverted_box_is_none() {
        let map = two_by_two();
        assert_eq!(
            block_range(&map.blockmap, UnitBox::new(10, 10, 5, 20)),
            None
        );
        assert_eq!(block_range(&map.blockmap, UnitBox::new(0, 20, 10, 5)), None);
    }

    #[test]
    fn block_range_extremes_do_not_overflow() {
        let map = two_by_two();
        let range = block_range(
            &map.blockmap,
            UnitBox::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX),
        )
        .unwrap();
        assert_eq!(
            range,
            BlockRange {
                left: 0,
                right: 1,
                bottom: 0,
                top: 1
            }
        );
    }

    #[test]
    fn block_range_negative_origin_grid() {
        // grid shifted to cover -8..119 in x and y
        let map = map_with((-8, -8), 1, 1, &[&[0]]);
        let range = block_range(&map.blockmap, UnitBox::new(-8, -8, 119, 119)).unwrap();
        assert_eq!(
            range,
            BlockRange {
                left: 0,
                right: 0,
                bottom: 0,
                top: 0
            }
        );
        // -9 lies one unit before the grid; clamping pulls in cell 0
        let range = block_range(&map.blockmap, UnitBox::new(-9, -8, 0, 0)).unwrap();
        assert_eq!(
            range,
            BlockRange {
                left: 0,
                right: 0,
                bottom: 0,
                top: 0
            }
        );
        // entirely before the grid
        assert_eq!(
            block_range(&map.blockmap, UnitBox::new(-100, -8, -9, 0)),
            None
        );
    }

    #[test]
    fn lines_in_box_single_cell() {
        let map = two_by_two();
        let lines: Vec<_> = lines_in_box(&map, UnitBox::new(10, 10, 20, 20)).collect();
        assert_eq!(lines, [0, 1]);
        let lines: Vec<_> = lines_in_box(&map, UnitBox::new(200, 200, 200, 200)).collect();
        assert_eq!(lines, [0, 2]);
    }

    #[test]
    fn lines_in_box_yields_duplicates_across_cells() {
        let map = two_by_two();
        // y = 0 strip touches both cells on the bottom row
        let lines: Vec<_> = lines_in_box(&map, UnitBox::new(0, 0, 200, 0)).collect();
        assert_eq!(lines, [0, 1, 1, 2]);
    }

    #[test]
    fn lines_in_box_vanilla_traversal_order_x_outer_y_inner() {
        let map = two_by_two();
        let lines: Vec<_> = lines_in_box(&map, UnitBox::new(0, 0, 255, 255)).collect();
        // cell (0,0) -> cell (0,1) -> cell (1,0) -> cell (1,1)
        assert_eq!(lines, [0, 1, 2, 1, 2, 0, 2]);
    }

    #[test]
    fn lines_in_box_preserves_disk_order_within_a_cell() {
        // three entries, all <= num_lines-1, so the loader accepts them
        let map = map_with((0, 0), 1, 1, &[&[2, 0, 1]]);
        let lines: Vec<_> = lines_in_box(&map, UnitBox::new(0, 0, 127, 127)).collect();
        assert_eq!(lines, [2, 0, 1]);
    }

    #[test]
    fn lines_in_box_empty_cell_yields_nothing() {
        let map = map_with((0, 0), 2, 1, &[&[], &[0]]);
        let lines: Vec<_> = lines_in_box(&map, UnitBox::new(0, 0, 127, 127)).collect();
        assert_eq!(lines, []);
        let lines: Vec<_> = lines_in_box(&map, UnitBox::new(128, 0, 255, 127)).collect();
        assert_eq!(lines, [0]);
    }

    #[test]
    fn lines_in_box_outside_yields_nothing() {
        let map = two_by_two();
        assert_eq!(
            lines_in_box(&map, UnitBox::new(-500, -500, -1, 400)).count(),
            0
        );
        assert_eq!(
            lines_in_box(&map, UnitBox::new(256, 0, 1024, 255)).count(),
            0
        );
        assert_eq!(
            lines_in_box(&map, UnitBox::new(0, 256, 255, 1024)).count(),
            0
        );
    }

    #[test]
    fn lines_in_box_inverted_box_yields_nothing() {
        let map = two_by_two();
        assert_eq!(lines_in_box(&map, UnitBox::new(10, 10, 0, 20)).count(), 0);
    }

    #[test]
    fn lines_in_box_exhaustion_is_stable() {
        let map = two_by_two();
        let mut iter = lines_in_box(&map, UnitBox::new(0, 0, 0, 0));
        assert_eq!(iter.next(), Some(0));
        assert_eq!(iter.next(), Some(1));
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn box_overlaps_grid_matches_block_range() {
        let map = two_by_two();
        assert!(box_overlaps_grid(
            &map.blockmap,
            UnitBox::new(10, 10, 20, 20)
        ));
        assert!(box_overlaps_grid(
            &map.blockmap,
            UnitBox::new(-100, -100, 100, 100)
        ));
        assert!(!box_overlaps_grid(
            &map.blockmap,
            UnitBox::new(-500, -500, -1, -1)
        ));
        assert!(!box_overlaps_grid(
            &map.blockmap,
            UnitBox::new(256, 256, 512, 512)
        ));
        assert!(!box_overlaps_grid(
            &map.blockmap,
            UnitBox::new(20, 20, 10, 10)
        ));
    }

    #[test]
    fn unit_box_from_fixed_floors_edges() {
        // -0.5, 5.75, 127.999 floor to -1, 5, 127 — not toward zero like to_int
        let bbox = BBox {
            left: Fixed::from_raw(-32768),
            bottom: Fixed::from_raw(-1),
            right: Fixed::from_raw(5 * 65536 + 49152),
            top: Fixed::from_raw(127 * 65536 + 65535),
        };
        let bbox = UnitBox::from_fixed(&bbox);
        assert_eq!(bbox, UnitBox::new(-1, -1, 5, 127));
    }

    #[test]
    fn unit_box_from_fixed_whole_values() {
        let bbox = BBox {
            left: Fixed::from_int(-100),
            bottom: Fixed::from_int(0),
            right: Fixed::from_int(200),
            top: Fixed::from_int(300),
        };
        assert_eq!(UnitBox::from_fixed(&bbox), UnitBox::new(-100, 0, 200, 300));
    }
}
