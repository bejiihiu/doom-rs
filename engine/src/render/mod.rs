//! The software renderer: framebuffer, low-level column/span drawers,
//! lighting tables and gamma correction.
//!
//! Everything works on raw palette indices instead of RGB; the palette is
//! applied only at output time, exactly like the original VGA pipeline.
//!
//! This module is being landed in stages; the listed modules are added
//! incrementally and every stage builds.

mod framebuffer;
mod gamma;

pub use framebuffer::{IndexedFramebuffer, SCREEN_HEIGHT, SCREEN_PIXELS, SCREEN_WIDTH};
pub use gamma::{gamma_remap, level_table, GAMMA_LEVELS, GAMMA_TABLE};
