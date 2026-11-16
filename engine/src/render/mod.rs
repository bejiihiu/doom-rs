//! The software renderer: framebuffer, low-level column/span drawers,
//! lighting tables, the wall/plane rasterizers and gamma correction.
//!
//! This module is the "draw pixels" layer of the engine — the parts that
//! turn a prepared column (walls, sprites, masked mid-textures) or span
//! (floors, ceilings) into palette indices in the framebuffer. Higher
//! layers (BSP-driven wall/plane setup) live elsewhere and feed these
//! drawers.
//!
//! Everything works on raw palette indices instead of RGB; the palette is
//! applied only at output time, exactly like the original VGA pipeline.
//!
//! | module       | contents                                                 |
//! |--------------|----------------------------------------------------------|
//! | `framebuffer` | [`crate::render::IndexedFramebuffer`], screen geometry               |
//! | [`column`]   | `R_DrawColumn` family: textured/fuzz/low-detail columns |
//! | `span`     | `R_DrawSpan` family: horizontal floor/ceiling spans     |
//! | `lighting` | `zlight`/`scalelight` tables, colormap level selection |
//! | `gamma`    | the five gamma remap tables                             |
//! | `planes`   | visplane bookkeeping (`R_FindPlane`/`R_CheckPlane`)     |
//! | `plane`    | flat span renderer (`R_DrawPlanes`/`R_MapPlane`)      |
//! | `textures` | flat composited wall textures + lazy composite cache    |
//! | `wall`     | `R_StoreWallRange`/`R_RenderSegLoop`/masked segs         |
//! | `sprite`   | `R_InitSprites`/`R_ProjectSprite`/`R_DrawVisSprite`     |
//! | `masked`   | `R_DrawMasked`/`R_DrawSprite`: vissprite sweep          |
//! | `sky`      | the sky column pass (`R_DrawPlanes`' sky branch)        |

mod column;
mod framebuffer;
mod gamma;
mod lighting;
mod masked;
mod plane;
mod planes;
mod sky;
mod span;
mod sprite;
mod textures;
mod wall;

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
pub use masked::{MaskedRenderer, MaskedReport};
pub use plane::{FlatSet, PlaneProjection, PlaneRenderReport, render_planes};
pub use planes::{MAX_VISPLANES, PlaneSet, Visplane};
pub use sky::{ANGLETOSKYSHIFT, SKY_TEXTURE_MID, SkyReport, render_sky};
pub use span::{SpanParams, draw_span, draw_span_low};
pub use sprite::{
    BASEYCENTER, FF_FRAMEMASK, FF_FULLBRIGHT, MAX_VIS_SPRITES, MF_SHADOW, MF_TRANSLATION,
    MF_TRANSSHIFT, ProjectedSprite, Sprite, SpriteDefinitions, SpriteError, SpriteFrame,
    SpriteLumpTables, SpriteThing, TranslationTables, decode_sprite_patch, draw_psprite,
    project_sprite, sort_vissprites,
};
pub use textures::{TextureColumn, TexturePixels, TextureRun, TextureSet, build_texture_pixels};
pub use wall::{
    DrawSeg, MAX_DRAWSEGS, MAX_OPENINGS, SIL_BOTH, SIL_BOTTOM, SIL_NONE, SIL_TOP, WallFrame,
    WallProjection, WallView,
};
