//! Synthetic map lump builders for tests. Test-only, on purpose: the
//! fixtures stay close to the tests that use them.
#![cfg(test)]

use super::load::MapLumps;
use super::types::{ML_BLOCKING, NF_SUBSECTOR};

/// Builds a VERTEXES lump from `(x, y)` map units.
pub(crate) fn vertexes(points: &[(i16, i16)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(points.len() * 4);
    for &(x, y) in points {
        out.extend_from_slice(&x.to_le_bytes());
        out.extend_from_slice(&y.to_le_bytes());
    }
    out
}

/// Builds a LINEDEFS lump. Arguments per line: `v1, v2, flags, special,
/// tag, sidedef_front, sidedef_back`; `-1` marks a missing sidedef.
pub(crate) fn linedefs(lines: &[(i16, i16, i16, i16, i16, i16, i16)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(lines.len() * 14);
    for &(v1, v2, flags, special, tag, side0, side1) in lines {
        out.extend_from_slice(&v1.to_le_bytes());
        out.extend_from_slice(&v2.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&special.to_le_bytes());
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&side0.to_le_bytes());
        out.extend_from_slice(&side1.to_le_bytes());
    }
    out
}

/// Builds a SIDEDEFS lump. Arguments per sidedef: `texture_offset,
/// row_offset, top, bottom, mid, sector`.
pub(crate) fn sidedefs(sides: &[(i16, i16, &str, &str, &str, i16)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(sides.len() * 30);
    for &(texoff, rowoff, top, bottom, mid, sector) in sides {
        out.extend_from_slice(&texoff.to_le_bytes());
        out.extend_from_slice(&rowoff.to_le_bytes());
        out.extend_from_slice(&name8(top));
        out.extend_from_slice(&name8(bottom));
        out.extend_from_slice(&name8(mid));
        out.extend_from_slice(&sector.to_le_bytes());
    }
    out
}

/// Builds a SECTORS lump. Arguments per sector: `floor_height,
/// ceiling_height, floor_pic, ceiling_pic, light_level, special, tag`.
pub(crate) fn sectors(sectors: &[(i16, i16, &str, &str, i16, i16, i16)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(sectors.len() * 26);
    for &(floor, ceil, floor_pic, ceil_pic, light, special, tag) in sectors {
        out.extend_from_slice(&floor.to_le_bytes());
        out.extend_from_slice(&ceil.to_le_bytes());
        out.extend_from_slice(&name8(floor_pic));
        out.extend_from_slice(&name8(ceil_pic));
        out.extend_from_slice(&light.to_le_bytes());
        out.extend_from_slice(&special.to_le_bytes());
        out.extend_from_slice(&tag.to_le_bytes());
    }
    out
}

/// Builds a SEGS lump. Arguments per seg: `v1, v2, angle, linedef,
/// side, offset`.
pub(crate) fn segs(segs: &[(i16, i16, i16, i16, i16, i16)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(segs.len() * 12);
    for &(v1, v2, angle, linedef, side, offset) in segs {
        out.extend_from_slice(&v1.to_le_bytes());
        out.extend_from_slice(&v2.to_le_bytes());
        out.extend_from_slice(&angle.to_le_bytes());
        out.extend_from_slice(&linedef.to_le_bytes());
        out.extend_from_slice(&side.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
    }
    out
}

/// Builds a SSECTORS lump. Arguments per subsector: `num_segs,
/// first_seg`.
pub(crate) fn subsectors(subs: &[(i16, i16)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(subs.len() * 4);
    for &(num_segs, first_seg) in subs {
        out.extend_from_slice(&num_segs.to_le_bytes());
        out.extend_from_slice(&first_seg.to_le_bytes());
    }
    out
}

/// One node for the NODES builder.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NodeSpec {
    /// Partition line origin.
    pub x: i16,
    /// Partition line origin.
    pub y: i16,
    /// Partition line delta.
    pub dx: i16,
    /// Partition line delta.
    pub dy: i16,
    /// Per-child bboxes, child-major.
    pub bbox: [[i16; 4]; 2],
    /// Child words; set the high bit (`NF_SUBSECTOR`) for leaves.
    pub children: [u16; 2],
}

/// Builds a NODES lump from node specs.
pub(crate) fn nodes(nodes: &[NodeSpec]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nodes.len() * 28);
    for node in nodes {
        out.extend_from_slice(&node.x.to_le_bytes());
        out.extend_from_slice(&node.y.to_le_bytes());
        out.extend_from_slice(&node.dx.to_le_bytes());
        out.extend_from_slice(&node.dy.to_le_bytes());
        for bbox in &node.bbox {
            for &value in bbox {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        for &child in &node.children {
            out.extend_from_slice(&child.to_le_bytes());
        }
    }
    out
}

/// Builds a BLOCKMAP lump. `offsets` holds the per-cell raw offsets
/// (in i16 units from the start of the lump); `cells` holds the cell
/// lists without the lead word and terminator, which are added here.
/// Passing mismatched `offsets`/`cells` builds a corrupt lump on
/// purpose.
pub(crate) fn blockmap(
    origin: (i16, i16),
    columns: i16,
    rows: i16,
    offsets: &[u16],
    cells: &[&[i16]],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&origin.0.to_le_bytes());
    out.extend_from_slice(&origin.1.to_le_bytes());
    out.extend_from_slice(&columns.to_le_bytes());
    out.extend_from_slice(&rows.to_le_bytes());
    for &offset in offsets {
        out.extend_from_slice(&offset.to_le_bytes());
    }
    for cell in cells {
        out.extend_from_slice(&0i16.to_le_bytes()); // lead word
        for &line in *cell {
            out.extend_from_slice(&line.to_le_bytes());
        }
        out.extend_from_slice(&(-1i16).to_le_bytes()); // terminator
    }
    out
}

/// Builds a well-formed BLOCKMAP for a rectangular grid with one cell
/// per provided list of linedef indices. Offsets are computed.
pub(crate) fn blockmap_grid(
    origin: (i16, i16),
    columns: i16,
    rows: i16,
    cells: &[&[i16]],
) -> Vec<u8> {
    let cell_count = usize::from(columns.max(0) as u16) * usize::from(rows.max(0) as u16);
    assert_eq!(cell_count, cells.len(), "every cell needs a list");

    let table_start = 4 + cell_count; // i16 units
    let mut offsets = Vec::with_capacity(cell_count);
    let mut pos = table_start;
    for cell in cells {
        offsets.push(pos as u16);
        pos += 2 + cell.len(); // lead + indices + terminator
    }
    blockmap(origin, columns, rows, &offsets, cells)
}

/// Builds a REJECT lump verbatim.
pub(crate) fn reject(bytes: &[u8]) -> Vec<u8> {
    bytes.to_vec()
}

/// Builds a THINGS lump. Arguments per thing: `x, y, angle, type,
/// flags`.
pub(crate) fn things(things: &[(i16, i16, i16, i16, i16)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(things.len() * 10);
    for &(x, y, angle, thing_type, flags) in things {
        out.extend_from_slice(&x.to_le_bytes());
        out.extend_from_slice(&y.to_le_bytes());
        out.extend_from_slice(&angle.to_le_bytes());
        out.extend_from_slice(&thing_type.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
    }
    out
}

/// NUL-pads/truncates a name like the on-disk format does.
fn name8(name: &str) -> [u8; 8] {
    let mut out = [0u8; 8];
    for (dst, src) in out.iter_mut().zip(name.as_bytes().iter().take(8)) {
        *dst = *src;
    }
    out
}

/// A complete, internally consistent map fixture: a 128x128 room split
/// into two subsectors by a horizontal partition, all lines one-sided.
#[derive(Debug, Clone)]
pub(crate) struct MapFixture {
    /// VERTEXES lump.
    pub vertexes: Vec<u8>,
    /// LINEDEFS lump.
    pub linedefs: Vec<u8>,
    /// SIDEDEFS lump.
    pub sidedefs: Vec<u8>,
    /// SECTORS lump.
    pub sectors: Vec<u8>,
    /// SEGS lump.
    pub segs: Vec<u8>,
    /// SSECTORS lump.
    pub subsectors: Vec<u8>,
    /// NODES lump.
    pub nodes: Vec<u8>,
    /// BLOCKMAP lump.
    pub blockmap: Vec<u8>,
    /// REJECT lump.
    pub reject: Vec<u8>,
    /// THINGS lump.
    pub things: Vec<u8>,
}

impl MapFixture {
    /// Borrows the fixture as a `MapLumps` bundle for `Map::load`.
    pub(crate) fn lumps(&self) -> MapLumps<'_> {
        MapLumps {
            vertexes: &self.vertexes,
            linedefs: &self.linedefs,
            sidedefs: &self.sidedefs,
            sectors: &self.sectors,
            segs: &self.segs,
            subsectors: &self.subsectors,
            nodes: &self.nodes,
            blockmap: &self.blockmap,
            reject: &self.reject,
            things: &self.things,
        }
    }
}

/// The canonical valid map. Used as the baseline every corruption
/// test mutates.
pub(crate) fn valid_map() -> MapFixture {
    MapFixture {
        vertexes: vertexes(&[(0, 0), (128, 0), (128, 128), (0, 128)]),
        linedefs: linedefs(&[
            (0, 1, ML_BLOCKING, 0, 0, 0, -1),
            (1, 2, ML_BLOCKING, 0, 0, 1, -1),
            (2, 3, ML_BLOCKING, 0, 0, 2, -1),
            (3, 0, ML_BLOCKING, 0, 0, 3, -1),
        ]),
        sidedefs: sidedefs(&[
            (0, 0, "-", "-", "WALL1", 0),
            (0, 0, "-", "-", "WALL1", 0),
            (0, 0, "-", "-", "WALL1", 0),
            (0, 0, "-", "-", "WALL1", 0),
        ]),
        sectors: sectors(&[(0, 128, "FLOOR4_8", "CEIL3_5", 176, 0, 0)]),
        segs: segs(&[
            (0, 1, 0, 0, 0, 0),
            (1, 2, 0, 1, 0, 0),
            (2, 3, 0, 2, 0, 0),
            (3, 0, 0, 3, 0, 0),
        ]),
        subsectors: subsectors(&[(2, 0), (2, 2)]),
        nodes: nodes(&[NodeSpec {
            x: 0,
            y: 64,
            dx: 128,
            dy: 0,
            bbox: [[0, 64, 128, 0], [64, 128, 128, 0]],
            children: [NF_SUBSECTOR, NF_SUBSECTOR | 1],
        }]),
        blockmap: blockmap_grid((-8, -8), 1, 1, &[&[0, 1, 2, 3]]),
        reject: reject(&[0x00]),
        things: things(&[(32, 32, 90, 1, 7)]),
    }
}
