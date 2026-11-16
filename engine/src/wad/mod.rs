//! WAD archive handling, ported from `w_wad.c`.
//!
//! IWADs and PWADs parsed by [`crate::wad::WadFile`] are stacked by [`crate::wad::WadSet`] into one
//! global lump namespace. Names are normalized (ASCII uppercase, NUL-padded)
//! at load, duplicates are allowed, and lookups scan from the end of the
//! directory so later files override earlier ones — the vanilla rule.
//!
//! Lump data is served as slices into the owning buffers; nothing is copied
//! out. All offsets and sizes are validated at parse time with checked
//! arithmetic, so a malformed archive is rejected with a descriptive
//! [`crate::wad::WadError`] instead of a panic or out-of-bounds read.
//!
//! ```no_run
//! use doom_engine::wad::{WadFile, WadSet};
//!
//! # fn example() -> Result<(), doom_engine::wad::WadError> {
//! let iwad = WadFile::open("DOOM.WAD")?;
//! let mut set = WadSet::new();
//! set.add_wad(iwad);
//! let playpal = set.lump_data_named("PLAYPAL")?;
//! # let _ = playpal;
//! # Ok(())
//! # }
//! ```

mod directory;
mod errors;
mod reader;

pub use directory::{LumpEntry, WadSet};
pub use errors::{WadError, WadResult};
pub use reader::{DirectoryEntry, LUMP_NAME_SIZE, LumpName, WadFile, WadHeader, WadKind};

#[cfg(test)]
pub(crate) mod testutil;
