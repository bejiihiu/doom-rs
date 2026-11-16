//! Synthetic map fixture for the automap tests. Test-only, on purpose:
//! the fixture stays close to the tests that use it.
#![cfg(test)]

use crate::fixed::Fixed;
use crate::map::types::{
    BlockMapGrid, Linedef, ML_BLOCKING, ML_DONTDRAW, ML_SECRET, ML_TWOSIDED, Map, Sector,
    SlopeType, Vertex,
};
use crate::math::BBox;

use super::TELEPORTER_SPECIAL;

fn v(x: i16, y: i16) -> Vertex {
    Vertex {
        x: Fixed::from_int(i32::from(x)),
        y: Fixed::from_int(i32::from(y)),
    }
}

fn sector(floor: i16, ceil: i16) -> Sector {
    Sector {
        floor_height: Fixed::from_int(i32::from(floor)),
        ceiling_height: Fixed::from_int(i32::from(ceil)),
        floor_pic: *b"FLOOR4_8",
        ceiling_pic: *b"CEIL3_5\0",
        light_level: 176,
        special: 0,
        tag: 0,
    }
}

fn linedef(
    vertexes: &[Vertex],
    v1: usize,
    v2: usize,
    flags: i16,
    special: i16,
    front_sector: usize,
    back_sector: Option<usize>,
) -> Linedef {
    let p1 = vertexes[v1];
    let p2 = vertexes[v2];
    let dx = p2.x - p1.x;
    let dy = p2.y - p1.y;

    let mut bbox = BBox::cleared();
    bbox.add_to_box(p1.x, p1.y);
    bbox.add_to_box(p2.x, p2.y);

    let slopetype = if dx.to_raw() == 0 {
        SlopeType::Vertical
    } else if dy.to_raw() == 0 {
        SlopeType::Horizontal
    } else if dy / dx > Fixed::ZERO {
        SlopeType::Positive
    } else {
        SlopeType::Negative
    };

    Linedef {
        v1,
        v2,
        dx,
        dy,
        flags,
        special,
        tag: 0,
        sidenum: [Some(0), back_sector.map(|_| 1)],
        bbox,
        slopetype,
        frontsector: Some(front_sector),
        backsector: back_sector,
    }
}

/// A small two-room fixture covering every line color branch the
/// renderer has:
///
/// * `0..=5` — solid one-sided walls (`5` is along the shared border).
/// * `4` — one-sided with `ML_DONTDRAW` (never drawn normally, drawn
///   under full-map reveal).
/// * `6` — two-sided, floor height change (`0` vs `32`) — brown.
/// * `7` — two-sided teleporter (`special == 39`) — light wall shade.
/// * `8` — two-sided secret door — plain wall color.
/// * `9` — two-sided, ceiling height change (`128` vs `96`) — yellow.
/// * `10` — two-sided, identical heights — drawn only under `iddt`.
/// * `11` — one-sided wall *inside* the default view.
///
/// Bounds: `(0, 0)..(256, 128)`.
pub(crate) fn fixture_map() -> Map {
    let vertexes = vec![
        v(0, 0),     // 0
        v(128, 0),   // 1
        v(128, 128), // 2
        v(0, 128),   // 3
        v(256, 0),   // 4
        v(256, 128), // 5
        v(32, 32),   // 6
        v(96, 32),   // 7
        v(32, 96),   // 8
        v(96, 96),   // 9
        v(16, 56),   // 10
        v(48, 56),   // 11
        v(32, 48),   // 12
        v(96, 48),   // 13
        v(48, 80),   // 14
        v(80, 80),   // 15
    ];

    let sectors = vec![
        sector(0, 128),  // 0 — big left room
        sector(32, 128), // 1 — right room
        sector(0, 96),   // 2 — middle band, lower ceiling
    ];

    let blockmap = BlockMapGrid::new(
        Fixed::from_int(-8),
        Fixed::from_int(-8),
        8,
        4,
        vec![Vec::new(); 32],
    );

    let linedefs = vec![
        linedef(&vertexes, 0, 1, ML_BLOCKING, 0, 0, None), // 0
        linedef(&vertexes, 1, 4, ML_BLOCKING, 0, 0, None), // 1
        linedef(&vertexes, 4, 5, ML_BLOCKING, 0, 1, None), // 2
        linedef(&vertexes, 5, 2, ML_BLOCKING, 0, 1, None), // 3
        linedef(&vertexes, 12, 13, ML_BLOCKING | ML_DONTDRAW, 0, 0, None), // 4
        linedef(&vertexes, 3, 0, ML_BLOCKING, 0, 0, None), // 5
        linedef(&vertexes, 1, 2, ML_TWOSIDED, 0, 0, Some(1)), // 6 — floor change
        linedef(&vertexes, 6, 7, ML_TWOSIDED, TELEPORTER_SPECIAL, 0, Some(2)), // 7 — teleporter
        linedef(&vertexes, 7, 6, ML_TWOSIDED | ML_SECRET, 0, 2, Some(0)), // 8 — secret
        linedef(&vertexes, 8, 9, ML_TWOSIDED, 0, 0, Some(2)), // 9 — ceiling change
        linedef(&vertexes, 10, 11, ML_TWOSIDED, 0, 0, Some(0)), // 10 — nothing to see
        linedef(&vertexes, 14, 15, ML_BLOCKING, 0, 0, None), // 11 — interior wall
    ];

    Map {
        vertexes,
        linedefs,
        sidedefs: Vec::new(),
        sectors,
        segs: Vec::new(),
        subsectors: Vec::new(),
        nodes: Vec::new(),
        blockmap,
        reject: vec![0],
        things: Vec::new(),
    }
}
