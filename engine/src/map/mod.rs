//! DOOM map parsing and in-memory representation.
//!
//! Ported from `p_setup.c` / `doomdata.h` / `r_defs.h`. The decoders
//! turn the raw map lumps into typed, fully validated structures:
//! every on-disk index is checked before it is stored, so downstream
//! code can trust the map instead of re-validating it.
//!
//! Layout of this module:
//!
//! | file        | contents                                                    |
//! |-------------|-------------------------------------------------------------|
//! | [`types`](crate::map::types) | raw on-disk records, engine structures, `Map` |
//! | `blockmap`  | BLOCKMAP lump loading (offset table + cell lists)          |
//! | `blockmap_query` | runtime cell lookup for a map box (`lines_in_box`)  |
//! | `load`      | per-lump loaders and the validating [`Map::load`](crate::map::Map::load) |
//! | `read`      | shared bounds-checked read helpers                          |
//! | `error`     | [`MapError`](crate::map::MapError)                          |
//!
//! The world stores references as indices instead of pointers:
//! linedefs point at vertices and sidedefs, sidedefs at sectors, segs
//! at linedefs and vertices. Coordinates are [`crate::fixed::Fixed`]
//! from the moment they are read.
//!
//! Loading is pure decoding: nothing here touches the WAD handle, the
//! resource cache or the renderer. Lumps borrow as byte slices for as
//! long as [`MapLumps`](crate::map::MapLumps) lives; every produced structure owns its
//! data.
//!
//! ```no_run
//! use doom_engine::map::{Map, MapLumps};
//!
//! # fn example(set: &doom_engine::wad::WadSet) -> Result<(), doom_engine::map::MapError> {
//! // lump slices come straight from the loaded WAD stack
//! let get = |name: &str| set.lump_data_named(name).map_err(|err| doom_engine::map::MapError::Engine(err.into()));
//! let map = Map::load(&MapLumps {
//!     vertexes: get("VERTEXES")?,
//!     linedefs: get("LINEDEFS")?,
//!     sidedefs: get("SIDEDEFS")?,
//!     sectors: get("SECTORS")?,
//!     segs: get("SEGS")?,
//!     subsectors: get("SSECTORS")?,
//!     nodes: get("NODES")?,
//!     blockmap: get("BLOCKMAP")?,
//!     reject: get("REJECT")?,
//!     things: get("THINGS")?,
//! })?;
//! # let _ = map;
//! # Ok(())
//! # }
//! ```

mod blockmap;
mod blockmap_query;
mod error;
mod load;
mod read;
#[cfg(test)]
pub(crate) mod testutil;

pub mod types;

pub use blockmap_query::{
    BlockRange, LinesInBox, UnitBox, block_index, block_range, box_overlaps_grid, lines_in_box,
    origin_units,
};
pub use error::{MapError, MapResult};
pub use load::MapLumps;
pub use types::{
    BlockMapGrid, Linedef, Map, Node, NodeChild, Sector, Seg, Sidedef, SlopeType, SubSector,
    ThingSpawn, Vertex,
};
