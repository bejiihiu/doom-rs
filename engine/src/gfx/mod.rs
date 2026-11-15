//! Decoding of DOOM's graphics data lumps.
//!
//! This module contains pure decoders only: it turns raw lump bytes into
//! typed structures and validates them on the way in. Loading the lumps from
//! a WAD, caching them and handing them to the renderer is the resource
//! manager's job, not this module's.
//!
//! Formats covered here:
//!
//! | lump                | structures                                   |
//! |---------------------|----------------------------------------------|
//! | `PLAYPAL`           | [`palette::Palette`](crate::gfx::palette::Palette) |
//! | `COLORMAP`          | [`palette::Colormap`](crate::gfx::palette::Colormap) |
//! | patches (sprites,   | [`patch::Patch`](crate::gfx::patch::Patch)  |
//! | weapon frames, ...) |                                              |
//! | `TEXTURE1`/`TEXTURE2` | [`texture::Texture`](crate::gfx::texture::Texture), [`texture::MapPatch`](crate::gfx::texture::MapPatch) |
//! | `PNAMES`            | [`pnames::Pnames`](crate::gfx::pnames::Pnames) |
//! | flats (`F_START`..`F_END`) | [`flat::Flat`](crate::gfx::flat::Flat)     |
//!
//! Sprites are patches with a special naming convention; see [`sprite`](crate::gfx::sprite).

mod error;

pub mod flat;
pub mod palette;
pub mod patch;
pub mod pnames;
pub mod sprite;
pub mod texture;

pub use error::{GfxError, GfxResult};

/// Normalizes an on-disk 8-byte name to the `String` form used everywhere
/// in the engine: ASCII uppercase, cut at the first NUL, invalid UTF-8
/// replaced lossily (DOOM lump names are effectively ASCII, but the bytes
/// on disk are not guaranteed to be).
pub(crate) fn normalize_name(raw: &[u8; 8]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).to_ascii_uppercase()
}
