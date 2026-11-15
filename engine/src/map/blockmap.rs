//! BLOCKMAP loading, ported from `p_setup.c` (`P_LoadBlockMap`).
//!
//! The lump is a precomputed collision grid; the engine never builds
//! one. Layout, all little-endian:
//!
//! ```text
//! offset  size          field
//! 0       2             X origin of the grid, i16, map units
//! 2       2             Y origin of the grid, i16, map units
//! 4       2             columns (blocks in X), i16, must be >= 1
//! 6       2             rows (blocks in Y), i16, must be >= 1
//! 8       2 * c * r     offset table: i16 per cell, stored in i16
//!                       units from the start of the lump (unsigned)
//! ...     ...           cell lists, row-major
//! ```
//!
//! Each cell list starts with a `0x0000` word (builders always emit
//! it; vanilla silently treats it as linedef 0), is followed by i16
//! linedef indices, and terminates with `0xFFFF`. Every index is
//! validated against the number of linedefs in the map, and a cell
//! may list at most all linedefs plus its lead word, which caps a
//! corrupt list that never reaches its terminator.

use crate::fixed::Fixed;

use super::error::{MapError, MapResult};
use super::read::{check_index, rd_i16, rd_u16};
use super::types::{
    BLOCKMAP_CELL_END, BLOCKMAP_CELL_LEAD, BLOCKMAP_HEADER_SIZE, BLOCKMAP_OFFSET_SIZE,
    BLOCKMAP_OFFSET_UNIT, BlockMapGrid,
};

const LUMP: &str = "BLOCKMAP";

/// Loads and validates the BLOCKMAP lump. `num_lines` is the number
/// of linedefs in the map; every index stored in a cell list must
/// point at one of them.
pub(crate) fn load_blockmap(data: &[u8], num_lines: usize) -> MapResult<BlockMapGrid> {
    if data.len() % BLOCKMAP_OFFSET_UNIT != 0 {
        return Err(MapError::BadSize {
            lump: LUMP,
            size: data.len(),
            reason: "BLOCKMAP length in bytes must be even",
        });
    }
    if data.len() < BLOCKMAP_HEADER_SIZE {
        return Err(MapError::BadSize {
            lump: LUMP,
            size: data.len(),
            reason: "smaller than the BLOCKMAP header",
        });
    }

    let x_origin = rd_i16(LUMP, data, 0)?;
    let y_origin = rd_i16(LUMP, data, 2)?;
    let columns = rd_i16(LUMP, data, 4)?;
    let rows = rd_i16(LUMP, data, 6)?;
    if columns < 1 || rows < 1 {
        return Err(MapError::BadValue {
            lump: LUMP,
            reason: format!("grid dimensions must be >= 1, got {columns} x {rows}"),
        });
    }

    // Fits in usize on 64-bit; on 32-bit the multiply can wrap, so
    // use checked math and reject absurd grids explicitly.
    let columns = columns as u16;
    let rows = rows as u16;
    let cell_count =
        usize::from(columns)
            .checked_mul(usize::from(rows))
            .ok_or(MapError::BadValue {
                lump: LUMP,
                reason: "grid too large".to_string(),
            })?;

    let table_bytes = cell_count
        .checked_mul(BLOCKMAP_OFFSET_SIZE)
        .ok_or(MapError::BadValue {
            lump: LUMP,
            reason: "offset table size overflows".to_string(),
        })?;
    if data.len() < BLOCKMAP_HEADER_SIZE + table_bytes {
        return Err(MapError::BadSize {
            lump: LUMP,
            size: data.len(),
            reason: "offset table does not fit in the lump",
        });
    }

    // Offsets are stored in i16 units from the start of the lump, and
    // the cell lists live after the offset table.
    let lump_shorts = data.len() / BLOCKMAP_OFFSET_UNIT;
    let table_start_shorts = (BLOCKMAP_HEADER_SIZE / BLOCKMAP_OFFSET_UNIT) + cell_count;

    let mut cells = Vec::with_capacity(cell_count);
    for cell in 0..cell_count {
        let raw_offset = rd_u16(
            LUMP,
            data,
            BLOCKMAP_HEADER_SIZE + cell * BLOCKMAP_OFFSET_SIZE,
        )?;
        let start = usize::from(raw_offset);
        if start < table_start_shorts {
            return Err(MapError::BadValue {
                lump: LUMP,
                reason: format!(
                    "cell {cell} offset {start} points into the header or offset table"
                ),
            });
        }
        if start >= lump_shorts {
            return Err(MapError::OutOfRange {
                lump: LUMP,
                what: "cell offset",
                index: i64::from(raw_offset),
                count: lump_shorts,
            });
        }
        cells.push(parse_cell(data, lump_shorts, start, num_lines)?);
    }

    Ok(BlockMapGrid::new(
        Fixed::from_raw(i32::from(x_origin) << 16),
        Fixed::from_raw(i32::from(y_origin) << 16),
        columns,
        rows,
        cells,
    ))
}

/// Parses one cell list, starting at `start` (in i16 units), consuming
/// the lead word, linedef indices and the terminator.
fn parse_cell(
    data: &[u8],
    lump_shorts: usize,
    start: usize,
    num_lines: usize,
) -> MapResult<Vec<u16>> {
    let mut pos = start;

    // Lead word: builders emit 0x0000, vanilla skips nothing. Require
    // it so a corrupt offset surfaces here instead of as cell contents.
    let lead = rd_i16(LUMP, data, pos * BLOCKMAP_OFFSET_UNIT)?;
    if lead != BLOCKMAP_CELL_LEAD {
        return Err(MapError::BadValue {
            lump: LUMP,
            reason: format!("cell list at offset {start} does not start with 0x0000 (got {lead})"),
        });
    }
    pos += 1;

    // A cell can list each linedef at most once on disk, so a corrupt
    // list that keeps emitting valid indices still terminates here.
    let mut indices = Vec::new();
    loop {
        if pos >= lump_shorts {
            return Err(MapError::BadValue {
                lump: LUMP,
                reason: format!("cell list at offset {start} runs past the end of the lump"),
            });
        }
        let entry = rd_i16(LUMP, data, pos * BLOCKMAP_OFFSET_UNIT)?;
        if entry == BLOCKMAP_CELL_END {
            return Ok(indices);
        }
        if entry < 0 {
            // the only negative value a cell may hold is the terminator
            return Err(MapError::BadValue {
                lump: LUMP,
                reason: format!("cell list at offset {start} holds negative linedef index {entry}"),
            });
        }
        if indices.len() >= num_lines {
            return Err(MapError::BadValue {
                lump: LUMP,
                reason: format!("cell list at offset {start} lists more than {num_lines} linedefs"),
            });
        }
        let index = check_index(LUMP, "linedef", i64::from(entry), num_lines)?;
        indices.push(index as u16);
        pos += 1;
    }
}
