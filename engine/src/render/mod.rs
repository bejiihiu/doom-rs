//! The software renderer: framebuffer, low-level column/span drawers,
//! lighting tables and gamma correction.
//!
//! This module is the "draw pixels" layer of the engine — the parts that
//! turn a prepared column (walls, sprites, masked mid-textures) or span
//! (floors, ceilings) into palette indices in the framebuffer. Higher
//! layers (wall/plane/sprite setup, BSP-driven clipping) live elsewhere
//! and feed these drawers.
//!
//! Everything works on raw palette indices instead of RGB; the palette is
//! applied only at output time, exactly like the original VGA pipeline.
//!
//! | module       | contents                                                 |
//! |--------------|----------------------------------------------------------|
//! | [`framebuffer`] | [`IndexedFramebuffer`], screen geometry               |
//! | [`column`]   | `R_DrawColumn` family: textured/fuzz/low-detail columns |
//! | [`span`]     | `R_DrawSpan` family: horizontal floor/ceiling spans     |
//! | [`lighting`] | `zlight`/`scalelight` tables, colormap level selection |
//! | [`gamma`]    | the five gamma remap tables                             |

mod column;
mod framebuffer;
mod gamma;
mod lighting;
mod span;

pub use column::{
    ColumnParams, ColumnSource, FUZZ_OFFSET, FUZZ_OFFSETS, FUZZ_TABLE_LEN, FuzzState,
    UnmaskedColumn, WrappedColumn, draw_column, draw_column_low, draw_fuzz_column,
};
pub use framebuffer::{IndexedFramebuffer, SCREEN_HEIGHT, SCREEN_PIXELS, SCREEN_WIDTH};
pub use gamma::{GAMMA_LEVELS, GAMMA_TABLE, gamma_remap, level_table};
pub use lighting::{
    DISTMAP, LIGHTLEVELS, LIGHTSCALESHIFT, LIGHTSEGSHIFT, LIGHTZSHIFT, LightTables, MAXLIGHTSCALE,
    MAXLIGHTZ, NUMCOLORMAPS, scalelight_fixed, select_row,
};
pub use span::{SpanParams, draw_span, draw_span_low};
