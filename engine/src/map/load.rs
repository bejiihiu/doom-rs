//! Map lump loading and cross-reference validation.
//!
//! Ported from `p_setup.c` (`P_LoadVertexes` / `P_LoadSegs` /
//! `P_LoadSubsectors` / `P_LoadSectors` / `P_LoadNodes` /
//! `P_LoadThings` / `P_LoadLineDefs` / `P_LoadSideDefs` /
//! `P_LoadBlockMap` and the `REJECT` cache call), with every pointer
//! the original dereferences turned into an index that is checked
//! before it is stored.
//!
//! Load order follows the original where it matters: vertices and
//! sectors first (everything references them), then sidedefs,
//! linedefs, segs, subsectors and nodes. Each stage validates against
//! the arrays loaded so far, so a corrupt lump fails the whole map
//! instead of producing a half-loaded one.
//!
//! The entry point is [`Map::load`] over a [`MapLumps`] bundle of raw
//! lump byte slices. Slices usually come straight from a WAD; tests
//! build them by hand.

use crate::fixed::Fixed;
use crate::math::{BBox, Point};
use crate::swap;

use super::blockmap::load_blockmap;
use super::error::{MapError, MapResult};
use super::read::{check_index, rd_i16, rd_u16, record_count};
use super::types::{
    LINEDEF_RECORD_SIZE, Linedef, ML_TWOSIDED, Map, MapLinedef, MapNode, MapSeg, MapSubsector,
    MapThing, MapVertex, NODE_RECORD_SIZE, Node, SECTOR_RECORD_SIZE, SEG_RECORD_SIZE,
    SIDEDEF_RECORD_SIZE, SUBSECTOR_RECORD_SIZE, Sector, Seg, Sidedef, SlopeType, SubSector,
    THING_RECORD_SIZE, ThingSpawn, VERTEX_RECORD_SIZE, Vertex,
};

const VERTEXES: &str = "VERTEXES";
const LINEDEFS: &str = "LINEDEFS";
const SIDEDEFS: &str = "SIDEDEFS";
const SECTORS: &str = "SECTORS";
const SEGS: &str = "SEGS";
const SSECTORS: &str = "SSECTORS";
const NODES: &str = "NODES";
const REJECT: &str = "REJECT";
const THINGS: &str = "THINGS";

/// Raw byte slices for every lump that makes up a map. The slices
/// usually borrow straight from a loaded WAD; nothing is copied.
#[derive(Debug, Clone, Copy)]
pub struct MapLumps<'a> {
    /// VERTEXES lump.
    pub vertexes: &'a [u8],
    /// LINEDEFS lump.
    pub linedefs: &'a [u8],
    /// SIDEDEFS lump.
    pub sidedefs: &'a [u8],
    /// SECTORS lump.
    pub sectors: &'a [u8],
    /// SEGS lump.
    pub segs: &'a [u8],
    /// SSECTORS lump.
    pub subsectors: &'a [u8],
    /// NODES lump.
    pub nodes: &'a [u8],
    /// BLOCKMAP lump.
    pub blockmap: &'a [u8],
    /// REJECT lump.
    pub reject: &'a [u8],
    /// THINGS lump.
    pub things: &'a [u8],
}

impl Map {
    /// Loads and validates a complete map.
    ///
    /// Fails with a [`MapError`] as soon as any lump is malformed;
    /// on error no partial `Map` is produced.
    pub fn load(lumps: &MapLumps<'_>) -> MapResult<Map> {
        let vertexes = load_vertexes(lumps.vertexes)?;
        let sectors = load_sectors(lumps.sectors)?;
        let sidedefs = load_sidedefs(lumps.sidedefs, sectors.len())?;
        let linedefs = load_linedefs(lumps.linedefs, &vertexes, &sidedefs)?;
        let segs = load_segs(lumps.segs, vertexes.len(), &linedefs, &sidedefs)?;
        let subsectors = load_subsectors(lumps.subsectors, &segs)?;
        let nodes = load_nodes(lumps.nodes, subsectors.len())?;
        let blockmap = load_blockmap(lumps.blockmap, linedefs.len())?;
        let reject = load_reject(lumps.reject, sectors.len())?;
        let things = load_things(lumps.things)?;

        Ok(Map {
            vertexes,
            linedefs,
            sidedefs,
            sectors,
            segs,
            subsectors,
            nodes,
            blockmap,
            reject,
            things,
        })
    }
}

/// `Fixed` from a raw `i16` map coordinate, like the original's
/// `SHORT(x) << FRACBITS` with sign extension.
fn fixed_from_i16(value: i16) -> Fixed {
    Fixed::from_raw(i32::from(value) << 16)
}

/// `BAM32` from a raw `i16` angle, like the original's `SHORT(a) << 16`.
fn bam_from_i16(value: i16) -> u32 {
    ((i32::from(value)) << 16) as u32
}

/// Copies an 8-byte name field verbatim. Bounds-checked like every
/// other read; callers pre-validate the lump size, so a failure here
/// means an internal invariant broke.
fn name_bytes(data: &[u8], offset: usize) -> MapResult<[u8; 8]> {
    let lo = swap::read_u32(data, offset)?;
    let hi = swap::read_u32(data, offset + 4)?;
    let mut name = [0u8; 8];
    name[..4].copy_from_slice(&lo.to_le_bytes());
    name[4..].copy_from_slice(&hi.to_le_bytes());
    Ok(name)
}

fn load_vertexes(data: &[u8]) -> MapResult<Vec<Vertex>> {
    let count = record_count(VERTEXES, data, VERTEX_RECORD_SIZE)?;
    let mut vertexes = Vec::with_capacity(count);
    for i in 0..count {
        let record = MapVertex {
            x: rd_i16(VERTEXES, data, i * VERTEX_RECORD_SIZE)?,
            y: rd_i16(VERTEXES, data, i * VERTEX_RECORD_SIZE + 2)?,
        };
        vertexes.push(Vertex {
            x: fixed_from_i16(record.x),
            y: fixed_from_i16(record.y),
        });
    }
    Ok(vertexes)
}

fn load_sectors(data: &[u8]) -> MapResult<Vec<Sector>> {
    let count = record_count(SECTORS, data, SECTOR_RECORD_SIZE)?;
    let mut sectors = Vec::with_capacity(count);
    for i in 0..count {
        let base = i * SECTOR_RECORD_SIZE;
        sectors.push(Sector {
            floor_height: fixed_from_i16(rd_i16(SECTORS, data, base)?),
            ceiling_height: fixed_from_i16(rd_i16(SECTORS, data, base + 2)?),
            floor_pic: name_bytes(data, base + 4)?,
            ceiling_pic: name_bytes(data, base + 12)?,
            light_level: rd_i16(SECTORS, data, base + 20)?,
            special: rd_i16(SECTORS, data, base + 22)?,
            tag: rd_i16(SECTORS, data, base + 24)?,
        });
    }
    Ok(sectors)
}

fn load_sidedefs(data: &[u8], num_sectors: usize) -> MapResult<Vec<Sidedef>> {
    let count = record_count(SIDEDEFS, data, SIDEDEF_RECORD_SIZE)?;
    let mut sidedefs = Vec::with_capacity(count);
    for i in 0..count {
        let base = i * SIDEDEF_RECORD_SIZE;
        let sector = check_index(
            SIDEDEFS,
            "sector",
            i64::from(rd_i16(SIDEDEFS, data, base + 28)?),
            num_sectors,
        )?;
        sidedefs.push(Sidedef {
            texture_offset: fixed_from_i16(rd_i16(SIDEDEFS, data, base)?),
            row_offset: fixed_from_i16(rd_i16(SIDEDEFS, data, base + 2)?),
            toptexture: name_bytes(data, base + 4)?,
            bottomtexture: name_bytes(data, base + 12)?,
            midtexture: name_bytes(data, base + 20)?,
            sector,
        });
    }
    Ok(sidedefs)
}

fn load_linedefs(
    data: &[u8],
    vertexes: &[Vertex],
    sidedefs: &[Sidedef],
) -> MapResult<Vec<Linedef>> {
    let count = record_count(LINEDEFS, data, LINEDEF_RECORD_SIZE)?;
    let num_vertexes = vertexes.len();
    let num_sides = sidedefs.len();
    let mut linedefs = Vec::with_capacity(count);
    for i in 0..count {
        let base = i * LINEDEF_RECORD_SIZE;
        let raw = MapLinedef {
            v1: rd_i16(LINEDEFS, data, base)?,
            v2: rd_i16(LINEDEFS, data, base + 2)?,
            flags: rd_i16(LINEDEFS, data, base + 4)?,
            special: rd_i16(LINEDEFS, data, base + 6)?,
            tag: rd_i16(LINEDEFS, data, base + 8)?,
            sidenum: [
                rd_i16(LINEDEFS, data, base + 10)?,
                rd_i16(LINEDEFS, data, base + 12)?,
            ],
        };

        let v1 = check_index(LINEDEFS, "vertex", i64::from(raw.v1), num_vertexes)?;
        let v2 = check_index(LINEDEFS, "vertex", i64::from(raw.v2), num_vertexes)?;
        let sidenum = [
            parse_side_ref(LINEDEFS, raw.sidenum[0], num_sides)?,
            parse_side_ref(LINEDEFS, raw.sidenum[1], num_sides)?,
        ];

        // a two sided line must have both sides, otherwise the seg
        // walker ends up indexing a missing back sidedef
        if raw.flags & ML_TWOSIDED != 0 && (sidenum[0].is_none() || sidenum[1].is_none()) {
            return Err(MapError::BadValue {
                lump: LINEDEFS,
                reason: format!("linedef {i} is two sided but has a missing sidedef"),
            });
        }

        let start = vertexes[v1];
        let end = vertexes[v2];
        let dx = end.x - start.x;
        let dy = end.y - start.y;

        let slopetype = if dx.is_zero() {
            SlopeType::Vertical
        } else if dy.is_zero() {
            SlopeType::Horizontal
        } else if dy / dx > Fixed::ZERO {
            SlopeType::Positive
        } else {
            SlopeType::Negative
        };

        let bbox = BBox::enclose([
            Point {
                x: start.x,
                y: start.y,
            },
            Point { x: end.x, y: end.y },
        ]);

        let frontsector = sidenum[0].map(|side| sidedefs[side].sector);
        let backsector = sidenum[1].map(|side| sidedefs[side].sector);

        linedefs.push(Linedef {
            v1,
            v2,
            dx,
            dy,
            flags: raw.flags,
            special: raw.special,
            tag: raw.tag,
            sidenum,
            bbox,
            slopetype,
            frontsector,
            backsector,
        });
    }
    Ok(linedefs)
}

/// Parses one `sidenum` entry: `-1` maps to `None`, anything else is
/// validated against the sidedef count.
fn parse_side_ref(lump: &'static str, raw: i16, num_sidedefs: usize) -> MapResult<Option<usize>> {
    if raw == -1 {
        return Ok(None);
    }
    Ok(Some(check_index(
        lump,
        "sidedef",
        i64::from(raw),
        num_sidedefs,
    )?))
}

fn load_segs(
    data: &[u8],
    num_vertexes: usize,
    linedefs: &[Linedef],
    sidedefs: &[Sidedef],
) -> MapResult<Vec<Seg>> {
    let count = record_count(SEGS, data, SEG_RECORD_SIZE)?;
    let mut segs = Vec::with_capacity(count);
    for i in 0..count {
        let base = i * SEG_RECORD_SIZE;
        let raw = MapSeg {
            v1: rd_i16(SEGS, data, base)?,
            v2: rd_i16(SEGS, data, base + 2)?,
            angle: rd_i16(SEGS, data, base + 4)?,
            linedef: rd_i16(SEGS, data, base + 6)?,
            side: rd_i16(SEGS, data, base + 8)?,
            offset: rd_i16(SEGS, data, base + 10)?,
        };

        let v1 = check_index(SEGS, "vertex", i64::from(raw.v1), num_vertexes)?;
        let v2 = check_index(SEGS, "vertex", i64::from(raw.v2), num_vertexes)?;
        let linedef_index = check_index(SEGS, "linedef", i64::from(raw.linedef), linedefs.len())?;
        if raw.side != 0 && raw.side != 1 {
            return Err(MapError::BadValue {
                lump: SEGS,
                reason: format!("seg {i}: side must be 0 or 1, got {}", raw.side),
            });
        }
        let side = raw.side as usize;

        let linedef = &linedefs[linedef_index];
        let sidedef_index = linedef.sidenum[side].ok_or_else(|| MapError::BadValue {
            lump: SEGS,
            reason: format!("seg {i}: linedef {linedef_index} has no sidedef on side {side}"),
        })?;
        let frontsector = sidedefs[sidedef_index].sector;

        let backsector = if linedef.flags & ML_TWOSIDED != 0 {
            let other = side ^ 1;
            // two sided lines were validated to have both sides
            linedef.sidenum[other].map(|side| sidedefs[side].sector)
        } else {
            None
        };

        segs.push(Seg {
            v1,
            v2,
            angle: bam_from_i16(raw.angle),
            offset: fixed_from_i16(raw.offset),
            linedef: linedef_index,
            side,
            sidedef: sidedef_index,
            frontsector,
            backsector,
        });
    }
    Ok(segs)
}

fn load_subsectors(data: &[u8], segs: &[Seg]) -> MapResult<Vec<SubSector>> {
    let count = record_count(SSECTORS, data, SUBSECTOR_RECORD_SIZE)?;
    let num_segs_total = segs.len();
    let mut subsectors = Vec::with_capacity(count);
    let mut referenced = 0u64;

    for i in 0..count {
        let base = i * SUBSECTOR_RECORD_SIZE;
        let raw = MapSubsector {
            num_segs: rd_i16(SSECTORS, data, base)?,
            first_seg: rd_i16(SSECTORS, data, base + 2)?,
        };
        if raw.num_segs < 0 {
            return Err(MapError::BadValue {
                lump: SSECTORS,
                reason: format!("subsector {i}: negative seg count {}", raw.num_segs),
            });
        }
        let num_segs = usize::from(raw.num_segs as u16);
        let first_seg_raw = i64::from(raw.first_seg);
        if first_seg_raw < 0 {
            return Err(MapError::OutOfRange {
                lump: SSECTORS,
                what: "first seg",
                index: first_seg_raw,
                count: num_segs_total,
            });
        }
        let first_seg = first_seg_raw as usize;

        let end = first_seg.checked_add(num_segs).ok_or(MapError::BadValue {
            lump: SSECTORS,
            reason: format!("subsector {i}: first seg + seg count overflows"),
        })?;
        if end > num_segs_total {
            return Err(MapError::OutOfRange {
                lump: SSECTORS,
                what: "seg range",
                index: end as i64,
                count: num_segs_total,
            });
        }

        referenced += num_segs as u64;
        let sector = if num_segs > 0 {
            Some(segs[first_seg].frontsector)
        } else {
            None
        };

        subsectors.push(SubSector {
            num_segs,
            first_seg,
            sector,
        });
    }

    // every seg belongs to exactly one subsector in a correct BSP;
    // a mismatch means the lumps came from different map builds
    if referenced != num_segs_total as u64 {
        return Err(MapError::BadValue {
            lump: SSECTORS,
            reason: format!(
                "subsectors reference {referenced} segs, but SEGS holds {num_segs_total}"
            ),
        });
    }

    Ok(subsectors)
}

fn load_nodes(data: &[u8], num_subsectors: usize) -> MapResult<Vec<Node>> {
    let count = record_count(NODES, data, NODE_RECORD_SIZE)?;
    let mut nodes = Vec::with_capacity(count);
    for i in 0..count {
        let base = i * NODE_RECORD_SIZE;
        let raw = MapNode {
            x: rd_i16(NODES, data, base)?,
            y: rd_i16(NODES, data, base + 2)?,
            dx: rd_i16(NODES, data, base + 4)?,
            dy: rd_i16(NODES, data, base + 6)?,
            bbox: [
                [
                    rd_i16(NODES, data, base + 8)?,
                    rd_i16(NODES, data, base + 10)?,
                    rd_i16(NODES, data, base + 12)?,
                    rd_i16(NODES, data, base + 14)?,
                ],
                [
                    rd_i16(NODES, data, base + 16)?,
                    rd_i16(NODES, data, base + 18)?,
                    rd_i16(NODES, data, base + 20)?,
                    rd_i16(NODES, data, base + 22)?,
                ],
            ],
            children: [
                rd_u16(NODES, data, base + 24)?,
                rd_u16(NODES, data, base + 26)?,
            ],
        };

        for &raw_child in raw.children.iter() {
            let index = usize::from(raw_child & super::types::NF_INDEX);
            if raw_child & super::types::NF_SUBSECTOR != 0 {
                if index >= num_subsectors {
                    return Err(MapError::OutOfRange {
                        lump: NODES,
                        what: "subsector child",
                        index: i64::from(raw_child),
                        count: num_subsectors,
                    });
                }
            } else if index >= count {
                return Err(MapError::OutOfRange {
                    lump: NODES,
                    what: "node child",
                    index: i64::from(raw_child),
                    count,
                });
            }
        }

        nodes.push(Node {
            x: fixed_from_i16(raw.x),
            y: fixed_from_i16(raw.y),
            dx: fixed_from_i16(raw.dx),
            dy: fixed_from_i16(raw.dy),
            bbox: [
                raw.bbox[0].map(fixed_from_i16),
                raw.bbox[1].map(fixed_from_i16),
            ],
            children: raw.children,
        });
    }
    Ok(nodes)
}

fn load_reject(data: &[u8], num_sectors: usize) -> MapResult<Vec<u8>> {
    // the visibility matrix is one bit per sector pair, rounded up to bytes
    let needed = ((num_sectors as u64) * (num_sectors as u64)).div_ceil(8);
    if (data.len() as u64) < needed {
        return Err(MapError::BadSize {
            lump: REJECT,
            size: data.len(),
            reason: "smaller than one bit per sector pair requires",
        });
    }
    Ok(data.to_vec())
}

fn load_things(data: &[u8]) -> MapResult<Vec<ThingSpawn>> {
    let count = record_count(THINGS, data, THING_RECORD_SIZE)?;
    let mut things = Vec::with_capacity(count);
    for i in 0..count {
        let base = i * THING_RECORD_SIZE;
        let raw = MapThing {
            x: rd_i16(THINGS, data, base)?,
            y: rd_i16(THINGS, data, base + 2)?,
            angle: rd_i16(THINGS, data, base + 4)?,
            thing_type: rd_i16(THINGS, data, base + 6)?,
            flags: rd_i16(THINGS, data, base + 8)?,
        };
        things.push(ThingSpawn {
            x: raw.x,
            y: raw.y,
            angle: raw.angle,
            thing_type: raw.thing_type,
            flags: raw.flags,
        });
    }
    Ok(things)
}
#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::super::types::{ML_BLOCKING, ML_TWOSIDED, NF_SUBSECTOR, NodeChild, SlopeType};
    use super::*;
    use crate::fixed::Fixed;

    #[test]
    fn valid_map_loads() {
        let fixture = valid_map();
        let map = Map::load(&fixture.lumps()).expect("valid map must load");

        assert_eq!(map.vertexes.len(), 4);
        assert_eq!(map.vertexes[0].x, Fixed::ZERO);
        assert_eq!(map.vertexes[0].y, Fixed::ZERO);
        assert_eq!(map.vertexes[1].x, Fixed::from_int(128));
        assert_eq!(map.vertexes[2].y, Fixed::from_int(128));

        assert_eq!(map.sectors.len(), 1);
        assert_eq!(map.sectors[0].ceiling_height, Fixed::from_int(128));
        assert_eq!(&map.sectors[0].floor_pic, b"FLOOR4_8");
        assert_eq!(&map.sectors[0].ceiling_pic, b"CEIL3_5\0");
        assert_eq!(map.sectors[0].light_level, 176);

        assert_eq!(map.sidedefs.len(), 4);
        assert!(map.sidedefs.iter().all(|side| side.sector == 0));

        assert_eq!(map.linedefs.len(), 4);
        let first = &map.linedefs[0];
        assert_eq!(first.v1, 0);
        assert_eq!(first.v2, 1);
        assert_eq!(first.sidenum, [Some(0), None]);
        assert_eq!(first.slopetype, SlopeType::Horizontal);
        assert_eq!(first.bbox.left, Fixed::ZERO);
        assert_eq!(first.bbox.right, Fixed::from_int(128));
        assert_eq!(first.bbox.top, Fixed::ZERO);
        assert_eq!(first.bbox.bottom, Fixed::ZERO);
        assert_eq!(first.frontsector, Some(0));
        assert_eq!(first.backsector, None);

        let north = &map.linedefs[1];
        assert_eq!(north.slopetype, SlopeType::Vertical);
        assert_eq!(north.bbox.left, Fixed::from_int(128));
        assert_eq!(north.bbox.right, Fixed::from_int(128));
        assert_eq!(north.bbox.top, Fixed::from_int(128));

        assert_eq!(map.segs.len(), 4);
        assert_eq!(map.segs[0].linedef, 0);
        assert_eq!(map.segs[0].sidedef, 0);
        assert_eq!(map.segs[0].frontsector, 0);
        assert_eq!(map.segs[0].backsector, None);

        assert_eq!(map.subsectors.len(), 2);
        assert_eq!(map.subsectors[0].num_segs, 2);
        assert_eq!(map.subsectors[0].first_seg, 0);
        assert_eq!(map.subsectors[0].sector, Some(0));
        assert_eq!(map.subsectors[1].first_seg, 2);

        assert_eq!(map.nodes.len(), 1);
        assert_eq!(map.nodes[0].child(0), NodeChild::SubSector(0));
        assert_eq!(map.nodes[0].child(1), NodeChild::SubSector(1));
        assert_eq!(map.nodes[0].x, Fixed::ZERO);
        assert_eq!(map.nodes[0].y, Fixed::from_int(64));
        assert_eq!(map.nodes[0].dx, Fixed::from_int(128));
        assert_eq!(map.nodes[0].dy, Fixed::ZERO);
        assert_eq!(map.nodes[0].bbox[0][0], Fixed::ZERO);
        assert_eq!(map.nodes[0].bbox[1][0], Fixed::from_int(64));

        assert_eq!(map.blockmap.cell_count(), 1);
        assert_eq!(map.blockmap.columns, 1);
        assert_eq!(map.blockmap.rows, 1);
        assert_eq!(map.blockmap.x_origin, Fixed::from_int(-8));
        assert_eq!(map.blockmap.cell_lines(0, 0), Some(&[0, 1, 2, 3][..]));
        assert_eq!(map.blockmap.cell_lines(1, 0), None);

        assert_eq!(map.reject, vec![0x00]);

        assert_eq!(map.things.len(), 1);
        assert_eq!(map.things[0].thing_type, 1);
        assert_eq!(map.things[0].flags, 7);
        assert_eq!(map.things[0].angle, 90);
        assert_eq!(map.things[0].x, 32);
        assert_eq!(map.things[0].y, 32);
    }

    #[test]
    fn fixed_point_conversion_keeps_sign() {
        let mut fixture = valid_map();
        fixture.vertexes = vertexes(&[(-8, 100), (128, 0), (128, 128), (0, 128)]);
        let map = Map::load(&fixture.lumps()).unwrap();
        assert_eq!(map.vertexes[0].x, Fixed::from_int(-8));
        assert_eq!(map.vertexes[0].y, Fixed::from_int(100));
    }

    #[test]
    fn sidedef_offsets_are_fixed_point() {
        let mut fixture = valid_map();
        fixture.sidedefs = sidedefs(&[
            (64, 16, "-", "-", "WALL1", 0),
            (64, 16, "-", "-", "WALL1", 0),
            (64, 16, "-", "-", "WALL1", 0),
            (64, 16, "-", "-", "WALL1", 0),
        ]);
        let map = Map::load(&fixture.lumps()).unwrap();
        assert_eq!(map.sidedefs[0].texture_offset, Fixed::from_int(64));
        assert_eq!(map.sidedefs[0].row_offset, Fixed::from_int(16));
    }

    #[test]
    fn positive_and_negative_slopes() {
        let mut fixture = valid_map();
        fixture.vertexes = vertexes(&[(0, 0), (128, 128), (128, 0), (0, 128)]);
        fixture.linedefs = linedefs(&[
            (0, 1, ML_BLOCKING, 0, 0, 0, -1), // up-right
            (2, 3, ML_BLOCKING, 0, 0, 1, -1), // up-left
            (3, 0, ML_BLOCKING, 0, 0, 2, -1),
            (1, 2, ML_BLOCKING, 0, 0, 3, -1),
        ]);
        fixture.segs = segs(&[
            (0, 1, 0, 0, 0, 0),
            (2, 3, 0, 1, 0, 0),
            (3, 0, 0, 2, 0, 0),
            (1, 2, 0, 3, 0, 0),
        ]);
        let map = Map::load(&fixture.lumps()).unwrap();
        assert_eq!(map.linedefs[0].slopetype, SlopeType::Positive);
        assert_eq!(map.linedefs[1].slopetype, SlopeType::Negative);
        assert_eq!(map.linedefs[2].slopetype, SlopeType::Vertical);
        assert_eq!(map.linedefs[3].slopetype, SlopeType::Vertical);
    }

    #[test]
    fn two_sided_linedef_resolves_backsector() {
        let mut fixture = valid_map();
        fixture.sectors = sectors(&[
            (0, 128, "FLOOR4_8", "CEIL3_5", 176, 0, 0),
            (0, 128, "FLOOR4_8", "CEIL3_5", 176, 0, 0),
        ]);
        fixture.sidedefs = sidedefs(&[
            (0, 0, "-", "-", "WALL1", 0),
            (0, 0, "-", "-", "WALL1", 1),
            (0, 0, "-", "-", "WALL1", 0),
            (0, 0, "-", "-", "WALL1", 0),
        ]);
        fixture.linedefs = linedefs(&[
            (0, 1, ML_BLOCKING | ML_TWOSIDED, 0, 0, 0, 1),
            (1, 2, ML_BLOCKING, 0, 0, 2, -1),
            (2, 3, ML_BLOCKING, 0, 0, 3, -1),
            (3, 0, ML_BLOCKING, 0, 0, 1, -1),
        ]);
        fixture.segs = segs(&[
            (0, 1, 0, 0, 0, 0),
            (1, 2, 0, 1, 0, 0),
            (2, 3, 0, 2, 0, 0),
            (3, 0, 0, 3, 0, 0),
        ]);
        fixture.reject = reject(&[0x00, 0x00]); // 2x2 bits -> 1 byte minimum, keep 2
        let map = Map::load(&fixture.lumps()).unwrap();
        assert_eq!(map.linedefs[0].backsector, Some(1));
        assert_eq!(map.segs[0].backsector, Some(1));
        assert_eq!(map.segs[1].backsector, None);
    }

    #[test]
    fn empty_map_loads() {
        let fixture = MapFixture {
            vertexes: Vec::new(),
            linedefs: Vec::new(),
            sidedefs: Vec::new(),
            sectors: Vec::new(),
            segs: Vec::new(),
            subsectors: Vec::new(),
            nodes: Vec::new(),
            blockmap: blockmap_grid((0, 0), 1, 1, &[&[]]),
            reject: Vec::new(),
            things: Vec::new(),
        };
        let map = Map::load(&fixture.lumps()).unwrap();
        assert!(map.vertexes.is_empty());
        assert_eq!(map.blockmap.cell_lines(0, 0), Some(&[][..]));
    }

    #[test]
    fn vertexes_ragged_lump_is_rejected() {
        let mut fixture = valid_map();
        fixture.vertexes.truncate(6);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        assert!(matches!(err, MapError::BadSize { size: 6, .. }));
    }

    #[test]
    fn linedef_vertex_out_of_range() {
        let mut fixture = valid_map();
        fixture.linedefs = linedefs(&[(9, 1, ML_BLOCKING, 0, 0, 0, -1)]);
        fixture.segs = segs(&[(9, 1, 0, 0, 0, 0)]);
        fixture.subsectors = subsectors(&[(1, 0)]);
        fixture.nodes = nodes(&[NodeSpec {
            x: 0,
            y: 64,
            dx: 128,
            dy: 0,
            bbox: [[0, 64, 128, 0], [64, 128, 128, 0]],
            children: [NF_SUBSECTOR, NF_SUBSECTOR],
        }]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        match err {
            MapError::OutOfRange {
                what, index, count, ..
            } => {
                assert_eq!(what, "vertex");
                assert_eq!(index, 9);
                assert_eq!(count, 4);
            }
            other => panic!("expected OutOfRange, got {other}"),
        }
    }

    #[test]
    fn linedef_sidedef_out_of_range() {
        let mut fixture = valid_map();
        fixture.linedefs = linedefs(&[(0, 1, ML_BLOCKING, 0, 0, 7, -1)]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        match err {
            MapError::OutOfRange {
                what, index, count, ..
            } => {
                assert_eq!(what, "sidedef");
                assert_eq!(index, 7);
                assert_eq!(count, 4);
            }
            other => panic!("expected OutOfRange, got {other}"),
        }
    }

    #[test]
    fn two_sided_linedef_missing_back_sidedef_is_rejected() {
        let mut fixture = valid_map();
        fixture.linedefs = linedefs(&[(0, 1, ML_BLOCKING | ML_TWOSIDED, 0, 0, 0, -1)]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        assert!(matches!(err, MapError::BadValue { .. }));
    }

    #[test]
    fn sidedef_sector_out_of_range() {
        let mut fixture = valid_map();
        fixture.sidedefs = sidedefs(&[
            (0, 0, "-", "-", "WALL1", 5),
            (0, 0, "-", "-", "WALL1", 0),
            (0, 0, "-", "-", "WALL1", 0),
            (0, 0, "-", "-", "WALL1", 0),
        ]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        match err {
            MapError::OutOfRange {
                what, index, count, ..
            } => {
                assert_eq!(what, "sector");
                assert_eq!(index, 5);
                assert_eq!(count, 1);
            }
            other => panic!("expected OutOfRange, got {other}"),
        }
    }

    #[test]
    fn seg_vertex_out_of_range() {
        let mut fixture = valid_map();
        fixture.segs = segs(&[(0, 9, 0, 0, 0, 0)]);
        fixture.subsectors = subsectors(&[(1, 0)]);
        fixture.nodes = nodes(&[NodeSpec {
            x: 0,
            y: 64,
            dx: 128,
            dy: 0,
            bbox: [[0, 64, 128, 0], [64, 128, 128, 0]],
            children: [NF_SUBSECTOR, NF_SUBSECTOR],
        }]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        match err {
            MapError::OutOfRange { what, .. } => assert_eq!(what, "vertex"),
            other => panic!("expected OutOfRange, got {other}"),
        }
    }

    #[test]
    fn seg_linedef_out_of_range() {
        let mut fixture = valid_map();
        fixture.segs = segs(&[(0, 1, 0, 99, 0, 0)]);
        fixture.subsectors = subsectors(&[(1, 0)]);
        fixture.nodes = nodes(&[NodeSpec {
            x: 0,
            y: 64,
            dx: 128,
            dy: 0,
            bbox: [[0, 64, 128, 0], [64, 128, 128, 0]],
            children: [NF_SUBSECTOR, NF_SUBSECTOR],
        }]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        match err {
            MapError::OutOfRange {
                what, index, count, ..
            } => {
                assert_eq!(what, "linedef");
                assert_eq!(index, 99);
                assert_eq!(count, 4);
            }
            other => panic!("expected OutOfRange, got {other}"),
        }
    }

    #[test]
    fn seg_side_must_be_zero_or_one() {
        let mut fixture = valid_map();
        fixture.segs = segs(&[(0, 1, 0, 0, 2, 0)]);
        fixture.subsectors = subsectors(&[(1, 0)]);
        fixture.nodes = nodes(&[NodeSpec {
            x: 0,
            y: 64,
            dx: 128,
            dy: 0,
            bbox: [[0, 64, 128, 0], [64, 128, 128, 0]],
            children: [NF_SUBSECTOR, NF_SUBSECTOR],
        }]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        assert!(matches!(err, MapError::BadValue { .. }));
    }

    #[test]
    fn seg_on_missing_side_is_rejected() {
        // linedef 0 is one-sided (sidenum[1] == -1); a seg pointing at
        // side 1 would index a missing sidedef
        let mut fixture = valid_map();
        fixture.segs = segs(&[(0, 1, 0, 0, 1, 0)]);
        fixture.subsectors = subsectors(&[(1, 0)]);
        fixture.nodes = nodes(&[NodeSpec {
            x: 0,
            y: 64,
            dx: 128,
            dy: 0,
            bbox: [[0, 64, 128, 0], [64, 128, 128, 0]],
            children: [NF_SUBSECTOR, NF_SUBSECTOR],
        }]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        assert!(matches!(err, MapError::BadValue { .. }));
    }

    #[test]
    fn subsector_range_beyond_segs() {
        let mut fixture = valid_map();
        fixture.subsectors = subsectors(&[(2, 3), (2, 0)]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        match err {
            MapError::OutOfRange { what, .. } => assert_eq!(what, "seg range"),
            other => panic!("expected OutOfRange, got {other}"),
        }
    }

    #[test]
    fn subsector_negative_seg_count() {
        let mut fixture = valid_map();
        fixture.subsectors = subsectors(&[(-1, 0), (2, 2)]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        assert!(matches!(err, MapError::BadValue { .. }));
    }

    #[test]
    fn subsector_seg_sum_mismatch() {
        let mut fixture = valid_map();
        fixture.subsectors = subsectors(&[(1, 0), (1, 2)]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        match err {
            MapError::BadValue { reason, .. } => assert!(reason.contains("2 segs")),
            other => panic!("expected BadValue, got {other}"),
        }
    }

    #[test]
    fn node_subsector_child_out_of_range() {
        let mut fixture = valid_map();
        fixture.nodes = nodes(&[NodeSpec {
            x: 0,
            y: 64,
            dx: 128,
            dy: 0,
            bbox: [[0, 64, 128, 0], [64, 128, 128, 0]],
            children: [NF_SUBSECTOR | 9, NF_SUBSECTOR | 1],
        }]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        match err {
            MapError::OutOfRange {
                what, index, count, ..
            } => {
                assert_eq!(what, "subsector child");
                assert_eq!(index, i64::from(NF_SUBSECTOR | 9));
                assert_eq!(count, 2);
            }
            other => panic!("expected OutOfRange, got {other}"),
        }
    }

    #[test]
    fn node_node_child_out_of_range() {
        let mut fixture = valid_map();
        fixture.nodes = nodes(&[NodeSpec {
            x: 0,
            y: 64,
            dx: 128,
            dy: 0,
            bbox: [[0, 64, 128, 0], [64, 128, 128, 0]],
            children: [3, NF_SUBSECTOR],
        }]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        match err {
            MapError::OutOfRange {
                what, index, count, ..
            } => {
                assert_eq!(what, "node child");
                assert_eq!(index, 3);
                assert_eq!(count, 1);
            }
            other => panic!("expected OutOfRange, got {other}"),
        }
    }

    #[test]
    fn reject_too_small_is_rejected() {
        let mut fixture = valid_map();
        fixture.reject = reject(&[]); // 1 sector needs at least 1 byte
        let err = Map::load(&fixture.lumps()).unwrap_err();
        assert!(matches!(err, MapError::BadSize { .. }));
    }

    #[test]
    fn reject_scales_with_sector_count() {
        let mut fixture = valid_map();
        // 9 sectors -> 81 bits -> 11 bytes minimum
        fixture.sectors = sectors(&vec![(0, 128, "FLOOR4_8", "CEIL3_5", 176, 0, 0); 9]);
        fixture.reject = reject(&[0x00; 10]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        assert!(matches!(err, MapError::BadSize { .. }));

        fixture.reject = reject(&[0x00; 11]);
        // 9 sectors with one sidedef referencing sector 3 (valid)
        fixture.sidedefs = sidedefs(&[
            (0, 0, "-", "-", "WALL1", 3),
            (0, 0, "-", "-", "WALL1", 0),
            (0, 0, "-", "-", "WALL1", 0),
            (0, 0, "-", "-", "WALL1", 0),
        ]);
        assert!(Map::load(&fixture.lumps()).is_ok());
    }

    #[test]
    fn blockmap_offset_into_table_is_rejected() {
        let mut fixture = valid_map();
        fixture.blockmap = blockmap((-8, -8), 1, 1, &[2], &[&[0, 1, 2, 3]]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        assert!(matches!(err, MapError::BadValue { .. }));
    }

    #[test]
    fn blockmap_offset_past_the_lump_is_rejected() {
        let mut fixture = valid_map();
        fixture.blockmap = blockmap((-8, -8), 1, 1, &[999], &[&[0, 1, 2, 3]]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        match err {
            MapError::OutOfRange {
                what, index, count, ..
            } => {
                assert_eq!(what, "cell offset");
                assert_eq!(index, 999);
                assert_eq!(count, 11); // 22 bytes = 11 i16 words
            }
            other => panic!("expected OutOfRange, got {other}"),
        }
    }

    #[test]
    fn blockmap_linedef_index_out_of_range() {
        let mut fixture = valid_map();
        fixture.blockmap = blockmap_grid((-8, -8), 1, 1, &[&[0, 1, 2, 7]]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        match err {
            MapError::OutOfRange {
                what, index, count, ..
            } => {
                assert_eq!(what, "linedef");
                assert_eq!(index, 7);
                assert_eq!(count, 4);
            }
            other => panic!("expected OutOfRange, got {other}"),
        }
    }

    #[test]
    fn blockmap_unterminated_cell_is_rejected() {
        let mut fixture = valid_map();
        let mut data = Vec::new();
        data.extend_from_slice(&(-8i16).to_le_bytes());
        data.extend_from_slice(&(-8i16).to_le_bytes());
        data.extend_from_slice(&1i16.to_le_bytes()); // columns
        data.extend_from_slice(&1i16.to_le_bytes()); // rows
        data.extend_from_slice(&5u16.to_le_bytes()); // offset of the cell, in i16 words
        // cell: lead + 0, 1, 2, 3 and NO terminator; lump ends here
        for word in [0i16, 0, 1, 2, 3] {
            data.extend_from_slice(&word.to_le_bytes());
        }
        fixture.blockmap = data;
        let err = Map::load(&fixture.lumps()).unwrap_err();
        match err {
            MapError::BadValue { reason, .. } => assert!(reason.contains("runs past the end")),
            other => panic!("expected BadValue, got {other}"),
        }
    }

    #[test]
    fn blockmap_cell_without_lead_is_rejected() {
        let mut fixture = valid_map();
        let mut data = Vec::new();
        data.extend_from_slice(&(-8i16).to_le_bytes());
        data.extend_from_slice(&(-8i16).to_le_bytes());
        data.extend_from_slice(&1i16.to_le_bytes());
        data.extend_from_slice(&1i16.to_le_bytes());
        data.extend_from_slice(&5u16.to_le_bytes());
        // cell starts with 5 instead of the 0x0000 lead word
        for word in [5i16, 0, 1, 2, 3, -1] {
            data.extend_from_slice(&word.to_le_bytes());
        }
        fixture.blockmap = data;
        let err = Map::load(&fixture.lumps()).unwrap_err();
        assert!(matches!(err, MapError::BadValue { .. }));
    }

    #[test]
    fn blockmap_negative_linedef_is_rejected() {
        let mut fixture = valid_map();
        let mut data = Vec::new();
        data.extend_from_slice(&(-8i16).to_le_bytes());
        data.extend_from_slice(&(-8i16).to_le_bytes());
        data.extend_from_slice(&1i16.to_le_bytes());
        data.extend_from_slice(&1i16.to_le_bytes());
        data.extend_from_slice(&5u16.to_le_bytes());
        // lead, then a negative word that is NOT the 0xFFFF terminator
        for word in [0i16, 0, -2, -1] {
            data.extend_from_slice(&word.to_le_bytes());
        }
        fixture.blockmap = data;
        let err = Map::load(&fixture.lumps()).unwrap_err();
        assert!(matches!(err, MapError::BadValue { .. }));
    }

    #[test]
    fn blockmap_zero_dimension_is_rejected() {
        let mut fixture = valid_map();
        fixture.blockmap = blockmap((-8, -8), 0, 1, &[], &[]);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        assert!(matches!(err, MapError::BadValue { .. }));
    }

    #[test]
    fn blockmap_odd_size_is_rejected() {
        let mut fixture = valid_map();
        fixture.blockmap.truncate(21);
        let err = Map::load(&fixture.lumps()).unwrap_err();
        assert!(matches!(err, MapError::BadSize { .. }));
    }

    #[test]
    fn blockmap_grid_is_row_major_and_supports_empty_cells() {
        let mut fixture = valid_map();
        fixture.blockmap = blockmap_grid((0, 0), 2, 2, &[&[], &[0], &[1, 2], &[]]);
        let map = Map::load(&fixture.lumps()).unwrap();
        assert_eq!(map.blockmap.cell_lines(0, 0), Some(&[][..]));
        assert_eq!(map.blockmap.cell_lines(1, 0), Some(&[0][..]));
        assert_eq!(map.blockmap.cell_lines(0, 1), Some(&[1, 2][..]));
        assert_eq!(map.blockmap.cell_lines(1, 1), Some(&[][..]));
        assert_eq!(map.blockmap.cell_lines(2, 0), None);
        assert_eq!(map.blockmap.cell_lines(0, 2), None);
    }

    #[test]
    fn things_ragged_lump_is_rejected() {
        let mut fixture = valid_map();
        fixture.things = vec![0u8; 11]; // 11 % 10 != 0
        let err = Map::load(&fixture.lumps()).unwrap_err();
        assert!(matches!(err, MapError::BadSize { size: 11, .. }));
    }

    #[test]
    fn things_decode_fields() {
        let mut fixture = valid_map();
        fixture.things = things(&[(-100, 200, 45, 3004, 7)]);
        let map = Map::load(&fixture.lumps()).unwrap();
        assert_eq!(
            map.things[0],
            ThingSpawn {
                x: -100,
                y: 200,
                angle: 45,
                thing_type: 3004,
                flags: 7,
            }
        );
    }

    #[test]
    fn node_bbox_and_values_are_fixed_point() {
        let map = {
            let fixture = valid_map();
            Map::load(&fixture.lumps()).unwrap()
        };
        assert_eq!(
            map.nodes[0].bbox[0],
            [
                Fixed::ZERO,
                Fixed::from_int(64),
                Fixed::from_int(128),
                Fixed::ZERO
            ]
        );
        assert_eq!(
            map.nodes[0].bbox[1],
            [
                Fixed::from_int(64),
                Fixed::from_int(128),
                Fixed::from_int(128),
                Fixed::ZERO
            ]
        );
    }
}
