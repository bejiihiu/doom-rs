//! BSP traversal and wall clipping — the mathematical core of the renderer.
//!
//! Ported from the original engine's `r_bsp.c` / `r_main.c` hierarchy. This
//! module decides *if* and *which columns* of a wall are visible; it never
//! draws anything. The renderer plugs its output into the `emit` callbacks
//! and consumes [`crate::bsp::traverse_bsp`] for draw order and [`crate::bsp::check_bbox`] for
//! whole-subtree culling.
//!
//! Layout:
//!
//! - `clip` — the `solidsegs` clip list ([`crate::bsp::ClipSegList`]) with the solid
//!   and pass-through wall segment clipper.
//! - `view` — view projection: [`crate::bsp::ViewState`], [`crate::bsp::ViewTables`]
//!   (`view_angle_to_x`, `clip_angle`) and `R_PointToAngle`.
//! - `walls` — line/seg clipping ([`crate::bsp::add_line`], [`crate::bsp::check_bbox`]).
//! - `node` — point-vs-partition side test and the traversal order.
//!
//! Points/angles follow the engine's conventions: fixed-point world
//! coordinates, BAM32 angles ([`crate::tables::Angle`]), wrap-around
//! subtraction for spans. Comments citing `as in original:` mark spots
//! where a non-obvious decision exists only to match the reference.
//!
//! Point-vs-divline classification lives in [`crate::math`] (with the same
//! sign-bit fast path `R_PointOnSide` uses); [`crate::bsp::point_on_node_side`] adapts
//! it to BSP nodes instead of duplicating the arithmetic here.

mod clip;
mod node;
mod tantoangle;
mod view;
mod walls;

pub use clip::{ClipRange, ClipSegList, VANILLA_MAX_CLIP_SEGS};
pub use node::{point_on_node_side, traverse_bsp};
pub use view::{FIELD_OF_VIEW, ViewState, ViewTables, point_to_angle, point_to_angle_2};
pub use walls::{AddLineOutcome, add_line, check_bbox};
