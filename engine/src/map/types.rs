//! Map data structures, ported from `doomdata.h` / `r_defs.h`.
//!
//! Two flavours live here:
//!
//! - Raw on-disk records (`MapVertex`, `MapLinedef`, ...), one per
//!   lump entry, in little-endian field order as stored by the BSP
//!   builders. They are plain data, no validation happens here.
//! - Engine structures (`Vertex`, `Linedef`, ...), produced by the
//!   decoders in `load`. References that are pointers in the
//!   original are plain indices here, and everything indexable is
//!   validated before it ever reaches the engine structures.
//!
//! Coordinates are `Fixed`, same as the original converts vertex
//! coordinates to fixed point at load time. Thing coordinates stay in
//! whole map units until spawn time.

use crate::fixed::Fixed;
use crate::math::BBox;

/// Size of a `mapvertex_t` record in bytes.
pub const VERTEX_RECORD_SIZE: usize = 4;
/// Size of a `maplinedef_t` record in bytes (7 shorts).
pub const LINEDEF_RECORD_SIZE: usize = 14;
/// Size of a `mapsidedef_t` record in bytes (2 shorts + 24 bytes of
/// texture names + 1 short).
pub const SIDEDEF_RECORD_SIZE: usize = 30;
/// Size of a `mapsector_t` record in bytes (2 shorts + 16 bytes of
/// flat names + 3 shorts).
pub const SECTOR_RECORD_SIZE: usize = 26;
/// Size of a `mapseg_t` record in bytes (6 shorts).
pub const SEG_RECORD_SIZE: usize = 12;
/// Size of a `mapsubsector_t` record in bytes (2 shorts).
pub const SUBSECTOR_RECORD_SIZE: usize = 4;
/// Size of a `mapnode_t` record in bytes (4 shorts + 16 bytes of
/// child bboxes + 2 ushorts).
pub const NODE_RECORD_SIZE: usize = 28;
/// Size of a `mapthing_t` record in bytes (5 shorts).
pub const THING_RECORD_SIZE: usize = 10;
/// Size of the BLOCKMAP header (origin x/y + cols/rows) in bytes.
pub const BLOCKMAP_HEADER_SIZE: usize = 8;
/// Size of one BLOCKMAP offset table entry in bytes.
pub const BLOCKMAP_OFFSET_SIZE: usize = 2;
/// Size of one BLOCKMAP cell in map units; the grid is square.
pub const BLOCKMAP_CELL_SIZE: i16 = 128;
/// On-disk BLOCKMAP offset unit: offsets are stored in i16 units, not
/// bytes, so multiply by this to get a byte offset.
pub const BLOCKMAP_OFFSET_UNIT: usize = 2;
/// First entry of every BLOCKMAP cell list. Vanilla ignores it, but
/// the builders always emit it, so it is validated and skipped.
pub const BLOCKMAP_CELL_LEAD: i16 = 0;
/// Terminator of a BLOCKMAP cell list (`-1` on disk).
pub const BLOCKMAP_CELL_END: i16 = -1;

// LineDef flags (ML_* from doomdata.h).
/// Solid line, blocks everything.
pub const ML_BLOCKING: i16 = 1;
/// Blocks monsters only.
pub const ML_BLOCKMONSTERS: i16 = 2;
/// Two sided line.
pub const ML_TWOSIDED: i16 = 4;
/// Upper texture unpegged.
pub const ML_DONTPEGTOP: i16 = 8;
/// Lower texture unpegged.
pub const ML_DONTPEGBOTTOM: i16 = 16;
/// Secret line, not shown as a normal line on the automap.
pub const ML_SECRET: i16 = 32;
/// Sound block, sound does not cross two of these.
pub const ML_SOUNDBLOCK: i16 = 64;
/// Not drawn on the automap at all.
pub const ML_DONTDRAW: i16 = 128;
/// Set when already seen on the automap.
pub const ML_MAPPED: i16 = 256;

// Thing option bits (MTF_* from doomdef.h).
/// Thing is present at skill 1 and 2.
pub const MTF_EASY: i16 = 1;
/// Thing is present at skill 3 (hurt me plenty).
pub const MTF_NORMAL: i16 = 2;
/// Thing is present at skill 4 and 5.
pub const MTF_HARD: i16 = 4;
/// Deaf monster; does not react to sound.
pub const MTF_AMBUSH: i16 = 8;
/// Thing only appears in multiplayer games.
pub const MTF_NOTSINGLE: i16 = 16;

/// High bit of a node child word: the child is a subsector, not a
/// node. Mirrors `NF_SUBSECTOR`.
pub const NF_SUBSECTOR: u16 = 0x8000;
/// Mask for the actual index inside a node child word.
pub const NF_INDEX: u16 = !NF_SUBSECTOR;

/// Raw on-disk `mapvertex_t`: vertex coordinates in whole map units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapVertex {
    /// X in map units.
    pub x: i16,
    /// Y in map units.
    pub y: i16,
}

/// Raw on-disk `maplinedef_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapLinedef {
    /// Index of the start vertex.
    pub v1: i16,
    /// Index of the end vertex.
    pub v2: i16,
    /// `ML_*` flags.
    pub flags: i16,
    /// Line special (action on use/walk-over). 0 means none.
    pub special: i16,
    /// Tag tying this line to sectors with the same tag.
    pub tag: i16,
    /// Sidedef indices; `-1` means the side does not exist.
    pub sidenum: [i16; 2],
}

/// Raw on-disk `mapsidedef_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapSidedef {
    /// Texture X offset in map units.
    pub texture_offset: i16,
    /// Texture Y offset in map units.
    pub row_offset: i16,
    /// Upper texture name.
    pub toptexture: [u8; 8],
    /// Lower texture name.
    pub bottomtexture: [u8; 8],
    /// Middle texture name.
    pub midtexture: [u8; 8],
    /// Index of the sector this sidedef faces.
    pub sector: i16,
}

/// Raw on-disk `mapsector_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapSector {
    /// Floor height in map units.
    pub floor_height: i16,
    /// Ceiling height in map units.
    pub ceiling_height: i16,
    /// Floor flat name.
    pub floor_pic: [u8; 8],
    /// Ceiling flat name.
    pub ceiling_pic: [u8; 8],
    /// Light level (0-255, used as a palette shift).
    pub light_level: i16,
    /// Sector special (damaging floors, secret counts, ...).
    pub special: i16,
    /// Tag tying this sector to line tags.
    pub tag: i16,
}

/// Raw on-disk `mapseg_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapSeg {
    /// Index of the start vertex.
    pub v1: i16,
    /// Index of the end vertex.
    pub v2: i16,
    /// Angle in BAM-hi format (`i16 << 16` is the full BAM).
    pub angle: i16,
    /// Index of the linedef this seg was split from.
    pub linedef: i16,
    /// Which side of the linedef this seg is on (`0` front, `1` back).
    pub side: i16,
    /// Offset along the linedef, in map units, fixed at load time.
    pub offset: i16,
}

/// Raw on-disk `mapsubsector_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapSubsector {
    /// Number of segs in this subsector.
    pub num_segs: i16,
    /// Index of the first seg; segs are stored sequentially.
    pub first_seg: i16,
}

/// Raw on-disk `mapnode_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapNode {
    /// Partition line origin, in map units.
    pub x: i16,
    /// Partition line origin, in map units.
    pub y: i16,
    /// Partition line delta, in map units.
    pub dx: i16,
    /// Partition line delta, in map units.
    pub dy: i16,
    /// Bounding boxes per child, in map units. `bbox[0]` is child 0.
    pub bbox: [[i16; 4]; 2],
    /// Child words; high bit (`NF_SUBSECTOR`) selects a subsector.
    pub children: [u16; 2],
}

/// Raw on-disk `mapthing_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapThing {
    /// X position in map units.
    pub x: i16,
    /// Y position in map units.
    pub y: i16,
    /// Facing angle in degrees (the port quantizes to 45° steps).
    pub angle: i16,
    /// DoomEd thing type number.
    pub thing_type: i16,
    /// `MTF_*` option bits. Vanilla calls this field `options`.
    pub flags: i16,
}

/// Vertex in fixed point, same representation as the original's
/// `vertex_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vertex {
    /// X in fixed point.
    pub x: Fixed,
    /// Y in fixed point.
    pub y: Fixed,
}

/// Slope type of a linedef, used by the move clipping code.
/// Mirrors `slopetype_t`; the discriminants match `ST_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlopeType {
    /// `dx == 0`.
    Vertical = 1,
    /// `dy == 0`.
    Horizontal = 0,
    /// `dy / dx > 0`.
    Positive = 2,
    /// `dy / dx < 0`.
    Negative = 3,
}

/// Linedef with validated references, mirrors `line_t`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Linedef {
    /// Index of the start vertex.
    pub v1: usize,
    /// Index of the end vertex.
    pub v2: usize,
    /// `v2 - v1`, precomputed for side tests.
    pub dx: Fixed,
    /// `v2 - v1`, precomputed for side tests.
    pub dy: Fixed,
    /// `ML_*` flags, raw.
    pub flags: i16,
    /// Line special.
    pub special: i16,
    /// Line tag.
    pub tag: i16,
    /// Sidedef indices; `None` marks a missing side.
    pub sidenum: [Option<usize>; 2],
    /// Bounding box of the line.
    pub bbox: BBox,
    /// Slope classification for move clipping.
    pub slopetype: SlopeType,
    /// Sector on the front side, via the front sidedef.
    pub frontsector: Option<usize>,
    /// Sector on the back side, if any.
    pub backsector: Option<usize>,
}

/// Sidedef with validated sector reference, mirrors `side_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sidedef {
    /// Texture X offset in fixed point.
    pub texture_offset: Fixed,
    /// Texture Y offset in fixed point.
    pub row_offset: Fixed,
    /// Upper texture name, raw bytes as stored on disk.
    pub toptexture: [u8; 8],
    /// Lower texture name, raw bytes as stored on disk.
    pub bottomtexture: [u8; 8],
    /// Middle texture name, raw bytes as stored on disk.
    pub midtexture: [u8; 8],
    /// Index of the sector this sidedef faces.
    pub sector: usize,
}

/// Sector, mirrors `sector_t` with the runtime-only bits left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sector {
    /// Floor height in fixed point.
    pub floor_height: Fixed,
    /// Ceiling height in fixed point.
    pub ceiling_height: Fixed,
    /// Floor flat name, raw bytes as stored on disk.
    pub floor_pic: [u8; 8],
    /// Ceiling flat name, raw bytes as stored on disk.
    pub ceiling_pic: [u8; 8],
    /// Light level (0-255, used as a palette shift).
    pub light_level: i16,
    /// Sector special.
    pub special: i16,
    /// Sector tag.
    pub tag: i16,
}

/// LineSeg with validated references, mirrors `seg_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seg {
    /// Index of the start vertex.
    pub v1: usize,
    /// Index of the end vertex.
    pub v2: usize,
    /// Angle in BAM32 (`i16 angle << 16`).
    pub angle: u32,
    /// Offset along the source linedef in fixed point.
    pub offset: Fixed,
    /// Index of the linedef this seg was split from.
    pub linedef: usize,
    /// Which side of the linedef this seg sits on (`0` or `1`).
    pub side: usize,
    /// Index of the sidedef this seg is on.
    pub sidedef: usize,
    /// Sector this seg faces, via its sidedef.
    pub frontsector: usize,
    /// Sector on the other side of the linedef, if the linedef is
    /// two sided.
    pub backsector: Option<usize>,
}

/// SubSector: a convex BSP leaf listing its segs, mirrors
/// `subsector_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubSector {
    /// Number of segs in the leaf.
    pub num_segs: usize,
    /// Index of the first seg.
    pub first_seg: usize,
    /// Sector of the leaf, resolved through its first seg.
    /// `None` for degenerate leaves with no segs.
    pub sector: Option<usize>,
}

/// One decoded node child. `NF_SUBSECTOR` in the raw word selects a
/// leaf, otherwise the value indexes another node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeChild {
    /// Leaf: index into [`Map::subsectors`].
    SubSector(usize),
    /// Branch: index into [`Map::nodes`].
    Node(usize),
}

/// BSP node, mirrors `node_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    /// Partition line origin in fixed point.
    pub x: Fixed,
    /// Partition line origin in fixed point.
    pub y: Fixed,
    /// Partition line delta in fixed point.
    pub dx: Fixed,
    /// Partition line delta in fixed point.
    pub dy: Fixed,
    /// Bounding boxes for child 0 and child 1, in fixed point.
    /// `bbox[child]` maps to `[top, bottom, right, left]` of `m_bbox`
    /// order; kept child-major like the on-disk format.
    pub bbox: [[Fixed; 4]; 2],
    /// Raw child words; see [`Node::child`].
    pub children: [u16; 2],
}

impl Node {
    /// Decodes a raw child word into a [`NodeChild`]. The indices are
    /// guaranteed to be in range for every map that passed [`Map::load`]
    /// validation.
    pub fn child(&self, index: usize) -> NodeChild {
        let raw = self.children[index];
        let index = usize::from(raw & NF_INDEX);
        if raw & NF_SUBSECTOR != 0 {
            NodeChild::SubSector(index)
        } else {
            NodeChild::Node(index)
        }
    }
}

/// A decoded thing from the THINGS lump. Coordinates stay in whole
/// map units; spawn code converts them to fixed point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThingSpawn {
    /// X position in map units.
    pub x: i16,
    /// Y position in map units.
    pub y: i16,
    /// Facing angle in degrees.
    pub angle: i16,
    /// DoomEd thing type number.
    pub thing_type: i16,
    /// `MTF_*` option bits.
    pub flags: i16,
}

/// BLOCKMAP grid loaded from a WAD lump.
///
/// Offsets in the lump are stored in i16 units from the start of the
/// lump; every cell list starts with a zero, lists linedef indices,
/// and terminates with `0xFFFF`. The grid is row-major: cell `y *
/// columns + x` is column `x`, row `y`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockMapGrid {
    /// X coordinate of the grid origin (bottom-left corner), fixed
    /// point.
    pub x_origin: Fixed,
    /// Y coordinate of the grid origin (bottom-left corner), fixed
    /// point.
    pub y_origin: Fixed,
    /// Number of columns (blocks in X direction).
    pub columns: u16,
    /// Number of rows (blocks in Y direction).
    pub rows: u16,
    /// Per-cell linedef index lists; `cells.len() == columns * rows`.
    cells: Vec<Vec<u16>>,
}

impl BlockMapGrid {
    /// Builds the grid from validated parts. The loader has already
    /// checked everything, so construction itself is infallible.
    pub(crate) fn new(
        x_origin: Fixed,
        y_origin: Fixed,
        columns: u16,
        rows: u16,
        cells: Vec<Vec<u16>>,
    ) -> Self {
        Self {
            x_origin,
            y_origin,
            columns,
            rows,
            cells,
        }
    }

    /// All cells, row-major.
    pub fn cells(&self) -> &[Vec<u16>] {
        &self.cells
    }

    /// Linedef indices in the cell at column `col`, row `row`.
    /// `None` when the coordinate lies outside the grid.
    pub fn cell_lines(&self, col: u16, row: u16) -> Option<&[u16]> {
        if col >= self.columns || row >= self.rows {
            return None;
        }
        Some(&self.cells[usize::from(row) * usize::from(self.columns) + usize::from(col)])
    }

    /// Total number of cells, `columns * rows`.
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }
}

/// A fully loaded, fully validated map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Map {
    /// Map vertices in fixed point.
    pub vertexes: Vec<Vertex>,
    /// Linedefs with validated vertex/sidedef/sector references.
    pub linedefs: Vec<Linedef>,
    /// Sidedefs, indexed by linedef `sidenum` entries.
    pub sidedefs: Vec<Sidedef>,
    /// Sectors, indexed by sidedef / seg references.
    pub sectors: Vec<Sector>,
    /// Segs, indexed by subsector `first_seg` entries.
    pub segs: Vec<Seg>,
    /// Subsectors, indexed by node children.
    pub subsectors: Vec<SubSector>,
    /// BSP nodes; the tree root is the last node.
    pub nodes: Vec<Node>,
    /// Precomputed collision grid from the BLOCKMAP lump.
    pub blockmap: BlockMapGrid,
    /// Sector-sector visibility matrix, raw bytes.
    pub reject: Vec<u8>,
    /// Decoded THINGS; coordinates in map units.
    pub things: Vec<ThingSpawn>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_sizes_match_the_on_disk_formats() {
        assert_eq!(VERTEX_RECORD_SIZE, 4);
        assert_eq!(LINEDEF_RECORD_SIZE, 14);
        assert_eq!(SIDEDEF_RECORD_SIZE, 30);
        assert_eq!(SECTOR_RECORD_SIZE, 26);
        assert_eq!(SEG_RECORD_SIZE, 12);
        assert_eq!(SUBSECTOR_RECORD_SIZE, 4);
        assert_eq!(NODE_RECORD_SIZE, 28);
        assert_eq!(THING_RECORD_SIZE, 10);
        assert_eq!(BLOCKMAP_HEADER_SIZE, 8);
        assert_eq!(BLOCKMAP_OFFSET_SIZE, 2);
    }

    #[test]
    fn raw_record_sizes_are_consistent_with_the_structs() {
        // the strides the decoders use must match what the writers used
        assert_eq!(std::mem::size_of::<MapVertex>(), VERTEX_RECORD_SIZE);
        assert_eq!(std::mem::size_of::<MapLinedef>(), LINEDEF_RECORD_SIZE);
        assert_eq!(std::mem::size_of::<MapSidedef>(), SIDEDEF_RECORD_SIZE);
        assert_eq!(std::mem::size_of::<MapSector>(), SECTOR_RECORD_SIZE);
        assert_eq!(std::mem::size_of::<MapSeg>(), SEG_RECORD_SIZE);
        assert_eq!(std::mem::size_of::<MapSubsector>(), SUBSECTOR_RECORD_SIZE);
        assert_eq!(std::mem::size_of::<MapNode>(), NODE_RECORD_SIZE);
        assert_eq!(std::mem::size_of::<MapThing>(), THING_RECORD_SIZE);
    }

    #[test]
    fn node_child_decodes_leaf_and_branch() {
        let node = Node {
            x: Fixed::ZERO,
            y: Fixed::ZERO,
            dx: Fixed::ZERO,
            dy: Fixed::ZERO,
            bbox: [[Fixed::ZERO; 4]; 2],
            children: [NF_SUBSECTOR | 7, 3],
        };
        assert_eq!(node.child(0), NodeChild::SubSector(7));
        assert_eq!(node.child(1), NodeChild::Node(3));
    }

    #[test]
    fn line_flags_keep_their_vanilla_bit_values() {
        assert_eq!(ML_BLOCKING, 1);
        assert_eq!(ML_BLOCKMONSTERS, 2);
        assert_eq!(ML_TWOSIDED, 4);
        assert_eq!(ML_DONTPEGTOP, 8);
        assert_eq!(ML_DONTPEGBOTTOM, 16);
        assert_eq!(ML_SECRET, 32);
        assert_eq!(ML_SOUNDBLOCK, 64);
        assert_eq!(ML_DONTDRAW, 128);
        assert_eq!(ML_MAPPED, 256);
    }

    #[test]
    fn thing_flags_keep_their_vanilla_bit_values() {
        assert_eq!(MTF_EASY, 1);
        assert_eq!(MTF_NORMAL, 2);
        assert_eq!(MTF_HARD, 4);
        assert_eq!(MTF_AMBUSH, 8);
        assert_eq!(MTF_NOTSINGLE, 16);
    }
}
