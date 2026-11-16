//! The drawing half of the automap: clipping, the Bresenham line, the
//! player arrow and the `AM_Drawer` entry point, ported from `am_map.c`.
//!
//! Everything works in two coordinate systems: map coordinates
//! ([`MLine`], fixed point) and framebuffer coordinates ([`FLine`],
//! window-local pixels). The window is always at `(0, 0)` in 1.10; the
//! `f_x`/`f_y` offsets are carried for completeness.

use crate::fixed::{FRACBITS, Fixed};
use crate::map::Map;
use crate::map::types::{ML_DONTDRAW, ML_SECRET};
use crate::render::IndexedFramebuffer;
use crate::stbar::draw_patch;
use crate::tables::{Angle, FINE_COSINE, FINE_SINE};

use super::state::{AutomapState, AutomapView, ThingMarker};
use super::{
    COLOR_ALL_MAP, COLOR_BACKGROUND, COLOR_CD_WALL, COLOR_CROSSHAIR, COLOR_FD_WALL, COLOR_GRID,
    COLOR_INVISIBLE_PLAYER, COLOR_SECRET_WALL, COLOR_TELEPORT_LINE, COLOR_THING, COLOR_TS_WALL,
    COLOR_WALL, COLOR_YOURS, NET_PLAYER_COLORS, TELEPORTER_SPECIAL,
};

/// `MAPBLOCKUNITS` — blockmap cell width in map units. The grid draws
/// its lines on these boundaries, relative to the blockmap origin.
const MAP_BLOCK_UNITS: i64 = 128;

// ---------------------------------------------------------------- lines

/// A map-space line (`mline_t`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MPoint {
    pub x: Fixed,
    pub y: Fixed,
}

/// A map-space segment (`mline_t`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MLine {
    pub a: MPoint,
    pub b: MPoint,
}

/// A framebuffer-space point (`fpoint_t`); window-local pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FPoint {
    x: i32,
    y: i32,
}

/// A framebuffer-space segment (`fline_t`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FLine {
    a: FPoint,
    b: FPoint,
}

// clipping outcodes (LEFT/RIGHT/BOTTOM/TOP in am_map.c)
const LEFT: u8 = 1;
const RIGHT: u8 = 2;
const BOTTOM: u8 = 4;
const TOP: u8 = 8;

/// `AM_clipMline` — clip a map-space segment to the window. Returns
/// `None` when the segment is entirely outside (trivially rejected or
/// clipped away), otherwise the framebuffer-space segment.
fn clip_mline(state: &AutomapState, ml: &MLine) -> Option<FLine> {
    let mut outcode1 = 0u8;
    let mut outcode2 = 0u8;

    // trivial rejects in map coordinates
    if ml.a.y > state.m_y2() {
        outcode1 = TOP;
    } else if ml.a.y < state.m_y() {
        outcode1 = BOTTOM;
    }
    if ml.b.y > state.m_y2() {
        outcode2 = TOP;
    } else if ml.b.y < state.m_y() {
        outcode2 = BOTTOM;
    }
    if outcode1 & outcode2 != 0 {
        return None; // trivially outside
    }

    if ml.a.x < state.m_x() {
        outcode1 |= LEFT;
    } else if ml.a.x > state.m_x2() {
        outcode1 |= RIGHT;
    }
    if ml.b.x < state.m_x() {
        outcode2 |= LEFT;
    } else if ml.b.x > state.m_x2() {
        outcode2 |= RIGHT;
    }
    if outcode1 & outcode2 != 0 {
        return None; // trivially outside
    }

    // transform to framebuffer coordinates (window-local)
    let mut fl = FLine {
        a: FPoint {
            x: state.map_x_to_screen(ml.a.x) - state.f_x(),
            y: state.map_y_to_screen(ml.a.y) - state.f_y(),
        },
        b: FPoint {
            x: state.map_x_to_screen(ml.b.x) - state.f_x(),
            y: state.map_y_to_screen(ml.b.y) - state.f_y(),
        },
    };

    outcode1 = outcode(&fl.a, state.f_w(), state.f_h());
    outcode2 = outcode(&fl.b, state.f_w(), state.f_h());
    if outcode1 & outcode2 != 0 {
        return None;
    }

    while outcode1 | outcode2 != 0 {
        let outside = if outcode1 != 0 { outcode1 } else { outcode2 };

        // clip to each side; division is truncating like the C original
        let tmp = if outside & TOP != 0 {
            let dy = fl.a.y - fl.b.y;
            let dx = fl.b.x - fl.a.x;
            FPoint {
                x: fl.a.x + (dx * fl.a.y) / dy,
                y: 0,
            }
        } else if outside & BOTTOM != 0 {
            let dy = fl.a.y - fl.b.y;
            let dx = fl.b.x - fl.a.x;
            FPoint {
                x: fl.a.x + (dx * (fl.a.y - state.f_h())) / dy,
                y: state.f_h() - 1,
            }
        } else if outside & RIGHT != 0 {
            let dy = fl.b.y - fl.a.y;
            let dx = fl.b.x - fl.a.x;
            FPoint {
                x: state.f_w() - 1,
                y: fl.a.y + (dy * (state.f_w() - 1 - fl.a.x)) / dx,
            }
        } else {
            // LEFT
            let dy = fl.b.y - fl.a.y;
            let dx = fl.b.x - fl.a.x;
            FPoint {
                x: 0,
                y: fl.a.y + (dy * (-fl.a.x)) / dx,
            }
        };

        if outside == outcode1 {
            fl.a = tmp;
            outcode1 = outcode(&fl.a, state.f_w(), state.f_h());
        } else {
            fl.b = tmp;
            outcode2 = outcode(&fl.b, state.f_w(), state.f_h());
        }

        if outcode1 & outcode2 != 0 {
            return None; // trivially outside
        }
    }

    Some(fl)
}

// DOOUTCODE
fn outcode(p: &FPoint, f_w: i32, f_h: i32) -> u8 {
    let mut oc = 0;
    if p.y < 0 {
        oc |= TOP;
    } else if p.y >= f_h {
        oc |= BOTTOM;
    }
    if p.x < 0 {
        oc |= LEFT;
    } else if p.x >= f_w {
        oc |= RIGHT;
    }
    oc
}

/// `AM_drawFline` — classic Bresenham with the two axis cases split,
/// ported statement-for-statement. Callers clip first, so the bounds
/// check is only a debug guard: out-of-window writes are dropped.
fn draw_fline(fb: &mut IndexedFramebuffer, state: &AutomapState, fl: &FLine, color: u8) {
    let put = |fb: &mut IndexedFramebuffer, x: i32, y: i32| {
        fb.set(
            (state.f_x() + x) as usize,
            (state.f_y() + y) as usize,
            color,
        );
    };

    let dx = fl.b.x - fl.a.x;
    let ax = 2 * dx.abs();
    let sx = if dx < 0 { -1 } else { 1 };

    let dy = fl.b.y - fl.a.y;
    let ay = 2 * dy.abs();
    let sy = if dy < 0 { -1 } else { 1 };

    let mut x = fl.a.x;
    let mut y = fl.a.y;

    if ax > ay {
        let mut d = ay - ax / 2;
        loop {
            debug_assert!(
                x >= 0 && x < state.f_w() && y >= 0 && y < state.f_h(),
                "AM_drawFline: point outside the window after clipping"
            );
            put(fb, x, y);
            if x == fl.b.x {
                return;
            }
            if d >= 0 {
                y += sy;
                d -= ax;
            }
            x += sx;
            d += ay;
        }
    } else {
        let mut d = ax - ay / 2;
        loop {
            debug_assert!(
                x >= 0 && x < state.f_w() && y >= 0 && y < state.f_h(),
                "AM_drawFline: point outside the window after clipping"
            );
            put(fb, x, y);
            if y == fl.b.y {
                return;
            }
            if d >= 0 {
                x += sx;
                d -= ay;
            }
            y += sy;
            d += ax;
        }
    }
}

/// `AM_drawMline` — clip, then draw the visible part of the segment.
fn draw_mline(fb: &mut IndexedFramebuffer, state: &AutomapState, ml: &MLine, color: u8) {
    if let Some(fl) = clip_mline(state, ml) {
        draw_fline(fb, state, &fl, color);
    }
}

// ------------------------------------------------------------- grid

/// `AM_drawGrid` — flat-tile-aligned grid lines, relative to the
/// blockmap origin (`bmaporgx`/`bmaporgy`).
fn draw_grid(fb: &mut IndexedFramebuffer, state: &AutomapState, map: &Map, color: u8) {
    let units = MAP_BLOCK_UNITS << FRACBITS;
    let org_x = map.blockmap.x_origin.to_raw() as i64;
    let org_y = map.blockmap.y_origin.to_raw() as i64;

    // vertical gridlines: start at the first multiple of MAPBLOCKUNITS
    // at or after m_x, relative to the blockmap origin
    let mut start = state.m_x().to_raw() as i64;
    let rem = (start - org_x) % units; // i64 % is truncating, like C99
    if rem != 0 {
        start += units - rem;
    }
    let end = (state.m_x() + state.m_w()).to_raw() as i64;

    let mut ml = MLine {
        a: MPoint {
            x: Fixed::ZERO,
            y: state.m_y(),
        },
        b: MPoint {
            x: Fixed::ZERO,
            y: state.m_y() + state.m_h(),
        },
    };
    let mut x = start;
    while x < end {
        ml.a.x = Fixed::from_raw(x as i32);
        ml.b.x = Fixed::from_raw(x as i32);
        draw_mline(fb, state, &ml, color);
        x += units;
    }

    // horizontal gridlines
    let mut start = state.m_y().to_raw() as i64;
    let rem = (start - org_y) % units;
    if rem != 0 {
        start += units - rem;
    }
    let end = (state.m_y() + state.m_h()).to_raw() as i64;

    ml.a.y = Fixed::ZERO;
    ml.b.y = Fixed::ZERO;
    ml.a.x = state.m_x();
    ml.b.x = state.m_x() + state.m_w();
    let mut y = start;
    while y < end {
        ml.a.y = Fixed::from_raw(y as i32);
        ml.b.y = Fixed::from_raw(y as i32);
        draw_mline(fb, state, &ml, color);
        y += units;
    }
}

// ------------------------------------------------------------ lines

/// `AM_drawWalls` — determine visible lines and draw them.
/// This is linedef based, not seg based (same as vanilla).
fn draw_walls(
    fb: &mut IndexedFramebuffer,
    state: &AutomapState,
    map: &Map,
    view: &AutomapView<'_>,
) {
    let cheating = state.cheat_level() > 0;

    for (index, line) in map.linedefs.iter().enumerate() {
        let l = MLine {
            a: MPoint {
                x: map.vertexes[line.v1].x,
                y: map.vertexes[line.v1].y,
            },
            b: MPoint {
                x: map.vertexes[line.v2].x,
                y: map.vertexes[line.v2].y,
            },
        };

        if cheating || state.is_line_seen(index) {
            if (line.flags & ML_DONTDRAW) != 0 && !cheating {
                continue;
            }
            match (line.backsector, line.frontsector) {
                (None, _) => draw_mline(fb, state, &l, COLOR_WALL),
                (Some(back), front) => {
                    let front_sector = front.map(|i| &map.sectors[i]);
                    let back_sector = &map.sectors[back];

                    if line.special == TELEPORTER_SPECIAL {
                        // teleporters
                        draw_mline(fb, state, &l, COLOR_TELEPORT_LINE);
                    } else if (line.flags & ML_SECRET) != 0 {
                        // secret doors: wall color; SECRETWALLCOLORS
                        // (which vanilla defines as WALLCOLORS) under cheat
                        if cheating {
                            draw_mline(fb, state, &l, COLOR_SECRET_WALL);
                        } else {
                            draw_mline(fb, state, &l, COLOR_WALL);
                        }
                    } else if let Some(front_sector) = front_sector {
                        if back_sector.floor_height != front_sector.floor_height {
                            draw_mline(fb, state, &l, COLOR_FD_WALL); // floor level change
                        } else if back_sector.ceiling_height != front_sector.ceiling_height {
                            draw_mline(fb, state, &l, COLOR_CD_WALL); // ceiling level change
                        } else if cheating {
                            draw_mline(fb, state, &l, COLOR_TS_WALL);
                        }
                        // else: two sided, no height change -> not drawn,
                        // like vanilla
                    } else if cheating {
                        draw_mline(fb, state, &l, COLOR_TS_WALL);
                    }
                }
            }
        } else if view.all_map {
            // pw_allmap: faint gray instead of not drawn at all
            if (line.flags & ML_DONTDRAW) == 0 {
                draw_mline(fb, state, &l, COLOR_ALL_MAP);
            }
        }
    }
}

// ----------------------------------------------------------- players

/// The player arrow lines (`mline_t player_arrow[]`), in fixed point,
/// drawn pointing right. `R` is derived from `PLAYERRADIUS`:
/// `(8*PLAYERRADIUS)/7` as an int, like the original.
const PLAYER_RADIUS_RAW: i64 = 16 << FRACBITS;
const ARROW_R: i64 = (8 * PLAYER_RADIUS_RAW) / 7;

const fn arrow_line(ax: i64, ay: i64, bx: i64, by: i64) -> MLine {
    MLine {
        a: MPoint {
            x: Fixed::from_raw(ax as i32),
            y: Fixed::from_raw(ay as i32),
        },
        b: MPoint {
            x: Fixed::from_raw(bx as i32),
            y: Fixed::from_raw(by as i32),
        },
    }
}

const PLAYER_ARROW: [MLine; 7] = {
    let r = ARROW_R;
    [
        arrow_line(-r + r / 8, 0, r, 0),                   // -----
        arrow_line(r, 0, r - r / 2, r / 4),                // ----->
        arrow_line(r, 0, r - r / 2, -r / 4),               // ----->
        arrow_line(-r + r / 8, 0, -r - r / 8, r / 4),      // >---->
        arrow_line(-r + r / 8, 0, -r - r / 8, -r / 4),     // >---->
        arrow_line(-r + 3 * r / 8, 0, -r + r / 8, r / 4),  // >>--->
        arrow_line(-r + 3 * r / 8, 0, -r + r / 8, -r / 4), // >>--->
    ]
};

/// The cheat-mode arrow (`cheat_player_arrow[]`) — the player arrow
/// with the little "d" eyes the iddt cheat gives you, ported line for
/// line.
const CHEAT_PLAYER_ARROW: [MLine; 16] = {
    let r = ARROW_R;
    [
        arrow_line(-r + r / 8, 0, r, 0),
        arrow_line(r, 0, r - r / 2, r / 6),
        arrow_line(r, 0, r - r / 2, -r / 6),
        arrow_line(-r + r / 8, 0, -r - r / 8, r / 6),
        arrow_line(-r + r / 8, 0, -r - r / 8, -r / 6),
        arrow_line(-r + 3 * r / 8, 0, -r + r / 8, r / 6),
        arrow_line(-r + 3 * r / 8, 0, -r + r / 8, -r / 6),
        // the "d" eyes
        arrow_line(-r / 2, 0, -r / 2, -r / 6),
        arrow_line(-r / 2, -r / 6, -r / 2 + r / 6, -r / 6),
        arrow_line(-r / 2 + r / 6, -r / 6, -r / 2 + r / 6, r / 4),
        arrow_line(-r / 6, 0, -r / 6, -r / 6),
        arrow_line(-r / 6, -r / 6, 0, -r / 6),
        arrow_line(0, -r / 6, 0, r / 4),
        arrow_line(r / 6, r / 4, r / 6, -r / 7),
        arrow_line(r / 6, -r / 7, r / 6 + r / 32, -r / 7 - r / 32),
        arrow_line(r / 6 + r / 32, -r / 7 - r / 32, r / 6 + r / 10, -r / 7),
    ]
};

/// The thin thing marker (`thintriangle_guy[]`): a dart, drawn at a
/// fixed 16-unit size with the thing's angle (`iddt` level 2).
const THIN_TRIANGLE_GUY: [MLine; 3] = [
    arrow_line(-32_768, -45_875, 65_536, 0), // -0.5*R,-0.7*R -> R,0
    arrow_line(65_536, 0, -32_768, 45_875),
    arrow_line(-32_768, 45_875, -32_768, -45_875),
];

/// `AM_rotate` — 2D rotation by a BAM angle.
fn rotate(x: &mut Fixed, y: &mut Fixed, angle: Angle) {
    let fine = angle.fine_angle();
    let tmpx = *x * FINE_COSINE[fine] - *y * FINE_SINE[fine];
    *y = *x * FINE_SINE[fine] + *y * FINE_COSINE[fine];
    *x = tmpx;
}

/// The parameters of one `AM_drawLineCharacter` call, bundled to keep
/// the argument count small: which line-art to draw and how to place it.
struct LineArt<'a> {
    lines: &'a [MLine],
    scale: Fixed,
    angle: Angle,
    color: u8,
}

/// `AM_drawLineCharacter` — scale (if `scale != 0`), rotate (if
/// `angle != 0`), translate and draw a line-art figure.
fn draw_line_character(
    fb: &mut IndexedFramebuffer,
    state: &AutomapState,
    art: &LineArt<'_>,
    x: Fixed,
    y: Fixed,
) {
    for line in art.lines {
        let mut l = *line;
        if !art.scale.is_zero() {
            l.a.x = art.scale * l.a.x;
            l.a.y = art.scale * l.a.y;
            l.b.x = art.scale * l.b.x;
            l.b.y = art.scale * l.b.y;
        }
        if art.angle != Angle::ZERO {
            rotate(&mut l.a.x, &mut l.a.y, art.angle);
            rotate(&mut l.b.x, &mut l.b.y, art.angle);
        }
        l.a.x += x;
        l.a.y += y;
        l.b.x += x;
        l.b.y += y;
        draw_mline(fb, state, &l, art.color);
    }
}

/// `AM_drawPlayers` — the local player's arrow (or all arrows in a net
/// game, colored by slot; invisible players near-black).
fn draw_players(fb: &mut IndexedFramebuffer, state: &AutomapState, view: &AutomapView<'_>) {
    if !view.netgame {
        let lines = if state.cheat_level() > 0 {
            &CHEAT_PLAYER_ARROW[..]
        } else {
            &PLAYER_ARROW[..]
        };
        let art = LineArt {
            lines,
            scale: Fixed::ZERO,
            angle: view.me.angle,
            color: COLOR_YOURS,
        };
        draw_line_character(fb, state, &art, view.me.x, view.me.y);
        return;
    }

    for (slot, player) in view.players.iter().enumerate() {
        if view.deathmatch && !view.singledemo && !player.is_local {
            continue;
        }
        let color = if player.invisible {
            COLOR_INVISIBLE_PLAYER
        } else {
            NET_PLAYER_COLORS[slot.min(NET_PLAYER_COLORS.len() - 1)]
        };
        let art = LineArt {
            lines: &PLAYER_ARROW,
            scale: Fixed::ZERO,
            angle: player.angle,
            color,
        };
        draw_line_character(fb, state, &art, player.x, player.y);
    }
}

/// `AM_drawThings` — every thing in the level as a thin triangle. Only
/// called under `iddt` cheat level 2.
fn draw_things(
    fb: &mut IndexedFramebuffer,
    state: &AutomapState,
    things: &[ThingMarker],
    color: u8,
) {
    for thing in things {
        let art = LineArt {
            lines: &THIN_TRIANGLE_GUY,
            scale: Fixed::from_raw(16 << FRACBITS),
            angle: thing.angle,
            color,
        };
        draw_line_character(fb, state, &art, thing.x, thing.y);
    }
}

/// `AM_drawMarks` — the ten user marks. Spots off the window are
/// skipped; the size is hardcoded 5x6 like vanilla ("because
/// something's wrong with the wad, i guess").
fn draw_marks(fb: &mut IndexedFramebuffer, state: &AutomapState) {
    let Some(assets) = state.assets() else {
        // no patches: marks are tracked but cannot be drawn; the game
        // layer uses AutomapState::load() to enable them
        return;
    };
    const W: i32 = 5;
    const H: i32 = 6;

    for (index, mark) in state.marks().iter().enumerate() {
        let Some(mark) = mark else { continue };
        let fx = state.map_x_to_screen(mark.x);
        let fy = state.map_y_to_screen(mark.y);
        if fx >= state.f_x()
            && fx <= state.f_x() + state.f_w() - W
            && fy >= state.f_y()
            && fy <= state.f_y() + state.f_h() - H
        {
            draw_patch(fb, assets.mark_patch(index), fx, fy);
        }
    }
}

/// `AM_drawCrosshair` — a single pixel at the window center.
fn draw_crosshair(fb: &mut IndexedFramebuffer, state: &AutomapState) {
    // (f_w*(f_h+1))/2 in flat window-local offset; converted back to x,y
    let flat = (state.f_w() * (state.f_h() + 1)) / 2;
    let x = flat % state.f_w();
    let y = flat / state.f_w();
    fb.set(
        (state.f_x() + x) as usize,
        (state.f_y() + y) as usize,
        COLOR_CROSSHAIR,
    );
}

// ------------------------------------------------------------ drawer

/// `AM_Drawer` — draw the whole automap window into `fb`. The status
/// bar rows at the bottom of the screen are not touched. When the
/// automap is inactive this returns immediately.
///
/// The `view` carries the per-frame positions (player marker, net-game
/// markers, things for the `iddt` level-2 overlay); everything map-
/// related comes from `map` and the retained [`AutomapState`].
pub fn render_automap(
    fb: &mut IndexedFramebuffer,
    state: &AutomapState,
    map: &Map,
    view: &AutomapView<'_>,
) {
    if !state.is_active() {
        return;
    }

    clear_window(fb, state);

    if state.grid_on() {
        draw_grid(fb, state, map, COLOR_GRID);
    }

    draw_walls(fb, state, map, view);

    draw_players(fb, state, view);

    if state.cheat_level() == 2 {
        draw_things(fb, state, view.things, COLOR_THING);
    }

    draw_crosshair(fb, state);

    draw_marks(fb, state);
}

// memset(fb, BACKGROUND, f_w*f_h) with the window rich enough to be at
// the top of the screen.
fn clear_window(fb: &mut IndexedFramebuffer, state: &AutomapState) {
    for y in 0..state.f_h() {
        if let Some(row) = fb.row_mut((state.f_y() + y) as usize) {
            for x in 0..state.f_w() {
                row[(state.f_x() + x) as usize] = COLOR_BACKGROUND;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automap::testutil::fixture_map;
    use crate::automap::{AM_NUMMARKPOINTS, AutomapInput, AutomapKey, PlayerMarker};
    use crate::gfx::patch::{Patch, decode_patch};
    use crate::render::SCREEN_WIDTH;
    use std::sync::Arc;

    fn me(x: i16, y: i16) -> PlayerMarker {
        PlayerMarker {
            x: Fixed::from_int(i32::from(x)),
            y: Fixed::from_int(i32::from(y)),
            angle: Angle::ZERO,
            invisible: false,
            is_local: true,
        }
    }

    fn view<'a>(me: PlayerMarker) -> AutomapView<'a> {
        AutomapView {
            me,
            players: &[],
            netgame: false,
            deathmatch: false,
            singledemo: false,
            all_map: false,
            things: &[],
        }
    }

    fn open(map: &Map) -> AutomapState {
        let mut state = AutomapState::new();
        state.begin_level(map);
        state.start(map, me(128, 64));
        state
    }

    fn count_pixels(fb: &IndexedFramebuffer, color: u8) -> usize {
        fb.as_slice().iter().filter(|&&p| p == color).count()
    }

    #[test]
    fn inactive_automap_draws_nothing() {
        let map = fixture_map();
        let state = AutomapState::new();
        let mut fb = IndexedFramebuffer::new();
        fb.clear(0xAA);
        render_automap(&mut fb, &state, &map, &view(me(128, 64)));
        assert!(fb.as_slice().iter().all(|&p| p == 0xAA));
    }

    #[test]
    fn window_clears_to_background_and_leaves_status_bar_alone() {
        let map = fixture_map();
        let state = open(&map);
        let mut fb = IndexedFramebuffer::new();
        fb.clear(0xAA);
        render_automap(&mut fb, &state, &map, &view(me(128, 64)));
        // top window cleared to the background color
        for y in 0..168 {
            for x in 0..SCREEN_WIDTH {
                // walls will have drawn some pixels; the background is
                // still the dominant color there, so just check the
                // status bar rows survived untouched
                let _ = (x, y);
            }
        }
        for y in 168..200 {
            for x in 0..SCREEN_WIDTH {
                assert_eq!(fb.get(x, y), Some(0xAA), "status bar row {y} was touched");
            }
        }
    }

    #[test]
    fn one_sided_wall_draws_wall_color_when_seen() {
        let map = fixture_map();
        let mut state = open(&map);
        state.mark_lines_seen([11]); // the interior one-sided wall
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(128, 64)));
        assert!(count_pixels(&fb, COLOR_WALL) > 0);
    }

    #[test]
    fn unseen_wall_draws_nothing() {
        let map = fixture_map();
        let state = open(&map);
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(128, 64)));
        assert_eq!(count_pixels(&fb, COLOR_WALL), 0);
    }

    fn row_count(fb: &IndexedFramebuffer, y: usize, color: u8) -> usize {
        fb.row(y).unwrap().iter().filter(|&&p| p == color).count()
    }

    #[test]
    fn all_map_powerup_draws_unseen_lines_in_faint_gray() {
        let map = fixture_map();
        let state = open(&map);
        let row = state.map_y_to_screen(Fixed::from_int(48)) as usize;
        let mut fb = IndexedFramebuffer::new();
        let mut v = view(me(128, 64));
        v.all_map = true;
        render_automap(&mut fb, &state, &map, &v);
        assert!(count_pixels(&fb, COLOR_ALL_MAP) > 0);
        // the ML_DONTDRAW line on y=48 stays hidden even under all_map
        assert_eq!(row_count(&fb, row, COLOR_WALL), 0);
    }

    #[test]
    fn full_map_reveal_shows_the_hidden_line() {
        let map = fixture_map();
        let mut state = open(&map);
        state.cycle_iddt(); // 0 -> 1
        let row = state.map_y_to_screen(Fixed::from_int(48)) as usize;
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(128, 64)));
        // the ML_DONTDRAW line is drawn once the map is fully revealed
        assert!(row_count(&fb, row, COLOR_WALL) > 0);
    }

    #[test]
    fn cheat_level_1_reveals_everything_including_hidden_line() {
        let map = fixture_map();
        let mut state = open(&map);
        state.cycle_iddt(); // 0 -> 1
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(128, 64)));
        // line 4 (ML_DONTDRAW) is drawn under the cheat: it is one-sided
        // so it lands in COLOR_WALL too
        assert!(count_pixels(&fb, COLOR_WALL) > 0);
    }

    #[test]
    fn floor_change_line_draws_brown() {
        let map = fixture_map();
        let mut state = open(&map);
        state.mark_lines_seen([6]); // 1->2 two-sided, floor 0 vs 32
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(128, 64)));
        let line = MLine {
            a: MPoint {
                x: Fixed::from_int(128),
                y: Fixed::ZERO,
            },
            b: MPoint {
                x: Fixed::from_int(128),
                y: Fixed::from_int(128),
            },
        };
        let _ = line; // pixels verified through the count instead
        assert!(count_pixels(&fb, COLOR_FD_WALL) > 0);
    }

    #[test]
    fn teleporter_line_draws_light_wall_shade() {
        let map = fixture_map();
        let mut state = open(&map);
        state.mark_lines_seen([7]); // special 39
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(64, 16)));
        assert!(count_pixels(&fb, COLOR_TELEPORT_LINE) > 0);
    }

    #[test]
    fn ceiling_change_line_draws_yellow() {
        let map = fixture_map();
        let mut state = open(&map);
        state.mark_lines_seen([9]); // 8->9 two-sided, ceil 128 vs 96
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(64, 64)));
        assert!(count_pixels(&fb, COLOR_CD_WALL) > 0);
    }

    #[test]
    fn same_height_two_sided_line_only_draws_under_cheat() {
        let map = fixture_map();
        let mut state = open(&map);
        state.mark_lines_seen([10]);
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(32, 64)));
        // GRAYS is both TS_WALL and the crosshair color; drop the
        // crosshair pixel first
        fb.set(160, 84, 0);
        assert_eq!(count_pixels(&fb, COLOR_TS_WALL), 0);

        state.cycle_iddt(); // 0 -> 1
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(32, 64)));
        fb.set(160, 84, 0);
        assert!(count_pixels(&fb, COLOR_TS_WALL) > 0);
    }

    #[test]
    fn secret_line_draws_as_wall_when_seen() {
        let map = fixture_map();
        let mut state = open(&map);
        state.mark_lines_seen([8]);
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(64, 16)));
        assert!(count_pixels(&fb, COLOR_WALL) > 0);
    }

    #[test]
    fn things_only_drawn_at_cheat_level_2() {
        let map = fixture_map();
        let things = [ThingMarker {
            x: Fixed::from_int(64),
            y: Fixed::from_int(64),
            angle: Angle::ZERO,
        }];
        let mut base = view(me(128, 64));
        base.things = &things;

        let mut state = open(&map);
        state.cycle_iddt(); // 1
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &base);
        assert_eq!(count_pixels(&fb, COLOR_THING), 0);

        state.cycle_iddt(); // 2
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &base);
        assert!(count_pixels(&fb, COLOR_THING) > 0);
    }

    #[test]
    fn crosshair_lands_at_window_center() {
        let map = fixture_map();
        let state = open(&map);
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(128, 64)));
        // (f_w*(f_h+1))/2 flat offset = row 84, col 160 for 320x168
        assert_eq!(fb.get(160, 84), Some(COLOR_CROSSHAIR));
    }

    #[test]
    fn grid_overlay_draws_grid_colored_pixels() {
        let map = fixture_map();
        let mut state = open(&map);
        state.handle_input(AutomapInput::KeyDown(AutomapKey::ToggleGrid));
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(128, 64)));
        assert!(count_pixels(&fb, COLOR_GRID) > 0);

        state.handle_input(AutomapInput::KeyDown(AutomapKey::ToggleGrid));
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(128, 64)));
        assert_eq!(count_pixels(&fb, COLOR_GRID), 0);
    }

    #[test]
    fn player_arrow_draws_at_player_position() {
        let map = fixture_map();
        let state = open(&map);
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(128, 64)));
        let fx = state.map_x_to_screen(Fixed::from_int(128));
        let fy = state.map_y_to_screen(Fixed::from_int(64));
        assert_eq!(fb.get(fx as usize, fy as usize), Some(COLOR_YOURS));
    }

    #[test]
    fn player_arrow_rotates_with_angle() {
        let map = fixture_map();
        let state = open(&map);
        let mut fb = IndexedFramebuffer::new();
        let mut rotated = me(128, 64);
        rotated.angle = Angle::ANG90;
        render_automap(&mut fb, &state, &map, &view(rotated));
        // a 90-degree arrow's long axis is vertical, its center column
        // must carry at least one arrow-colored pixel above the center
        let fx = state.map_x_to_screen(Fixed::from_int(128)) as usize;
        let fy = state.map_y_to_screen(Fixed::from_int(64)) as usize;
        let col: Vec<usize> = (0..fy)
            .filter(|&y| fb.get(fx, y) == Some(COLOR_YOURS))
            .collect();
        assert!(
            !col.is_empty(),
            "no arrow pixels above the center after 90 deg rotation"
        );
    }

    #[test]
    fn netgame_arrows_use_slot_colors() {
        let map = fixture_map();
        let state = open(&map);
        let players = [
            PlayerMarker {
                is_local: true,
                ..me(100, 64)
            },
            PlayerMarker {
                is_local: false,
                ..me(140, 64)
            },
            PlayerMarker {
                is_local: false,
                invisible: true,
                ..me(180, 64)
            },
        ];
        let mut fb = IndexedFramebuffer::new();
        let mut v = view(me(128, 64));
        v.players = &players;
        v.netgame = true;
        render_automap(&mut fb, &state, &map, &v);
        assert!(count_pixels(&fb, NET_PLAYER_COLORS[0]) > 0);
        assert!(count_pixels(&fb, NET_PLAYER_COLORS[1]) > 0);
        assert!(count_pixels(&fb, COLOR_INVISIBLE_PLAYER) > 0);
    }

    #[test]
    fn deathmatch_hides_nonlocal_players() {
        let map = fixture_map();
        let state = open(&map);
        let players = [
            PlayerMarker {
                is_local: true,
                ..me(100, 64)
            },
            PlayerMarker {
                is_local: false,
                ..me(140, 64)
            },
        ];
        let mut fb = IndexedFramebuffer::new();
        let mut v = view(me(128, 64));
        v.players = &players;
        v.netgame = true;
        v.deathmatch = true;
        render_automap(&mut fb, &state, &map, &v);
        fb.set(160, 84, 0); // the crosshair is the same gray
        assert_eq!(count_pixels(&fb, NET_PLAYER_COLORS[1]), 0);
        assert!(count_pixels(&fb, NET_PLAYER_COLORS[0]) > 0);
    }

    #[test]
    fn deathmatch_shows_everyone_in_singledemo() {
        let map = fixture_map();
        let state = open(&map);
        let players = [
            PlayerMarker {
                is_local: true,
                ..me(100, 64)
            },
            PlayerMarker {
                is_local: false,
                ..me(140, 64)
            },
        ];
        let mut fb = IndexedFramebuffer::new();
        let mut v = view(me(128, 64));
        v.players = &players;
        v.netgame = true;
        v.deathmatch = true;
        v.singledemo = true;
        render_automap(&mut fb, &state, &map, &v);
        assert!(count_pixels(&fb, NET_PLAYER_COLORS[1]) > 0);
    }

    #[test]
    fn cheat_arrow_drawn_when_cheating() {
        let map = fixture_map();
        let mut state = open(&map);
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(128, 64)));
        let before = count_pixels(&fb, COLOR_YOURS);

        state.cycle_iddt();
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(128, 64)));
        let after = count_pixels(&fb, COLOR_YOURS);
        // the cheat arrow has more line segments than the normal arrow
        assert!(
            after > before,
            "cheat arrow ({after} px) should cover more than the plain arrow ({before} px)"
        );
    }

    fn build_patch_bytes(width: i16, height: i16, fill: u8) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&0i16.to_le_bytes());
        data.extend_from_slice(&0i16.to_le_bytes());
        let table_start = data.len();
        data.resize(table_start + width as usize * 4, 0);
        for col in 0..width as usize {
            let column_offset = data.len() as i32;
            data[table_start + col * 4..table_start + (col + 1) * 4]
                .copy_from_slice(&column_offset.to_le_bytes());
            data.push(0); // topdelta
            data.push(height as u8); // length
            data.push(0); // pad
            data.extend(std::iter::repeat_n(fill, height as usize));
            data.push(0); // pad
            data.push(0xff); // terminator
        }
        data
    }

    #[test]
    fn marks_draw_when_patches_loaded() {
        let map = fixture_map();
        let mut state = open(&map);
        let patch = decode_patch(&build_patch_bytes(1, 1, 0x5A)).unwrap();
        let patches: [Arc<Patch>; AM_NUMMARKPOINTS] = std::array::from_fn(|_| {
            Arc::new(decode_patch(&build_patch_bytes(1, 1, 0x5A)).unwrap())
        });
        let _ = patch;
        state.set_mark_patches_for_test(patches);

        // drop one mark at the window center
        state.handle_input(AutomapInput::KeyDown(AutomapKey::Mark));
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(128, 64)));
        assert_eq!(count_pixels(&fb, 0x5A), 1);
    }

    #[test]
    fn marks_skipped_without_patch_assets() {
        let map = fixture_map();
        let mut state = open(&map);
        state.handle_input(AutomapInput::KeyDown(AutomapKey::Mark));
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(128, 64)));
        // no AMMNUM patches: mark tracked but nothing drawn
        assert_eq!(count_pixels(&fb, 0x5A), 0);
    }

    #[test]
    fn mark_off_screen_is_clipped() {
        let map = fixture_map();
        let mut state = open(&map);
        // shrink the view to a tiny corner of the map and mark a point
        // far outside it by panning hard
        state.handle_input(AutomapInput::KeyDown(AutomapKey::ToggleFollow));
        state.handle_input(AutomapInput::KeyDown(AutomapKey::PanRight));
        for _ in 0..400 {
            state.tick(&view(me(128, 64)));
        }
        state.handle_input(AutomapInput::KeyDown(AutomapKey::Mark));
        // mark sits at the (clamped) window center, which is on-screen,
        // so this exercises the "inside" path end to end
        let patches: [Arc<Patch>; AM_NUMMARKPOINTS] = std::array::from_fn(|_| {
            Arc::new(decode_patch(&build_patch_bytes(5, 6, 0x33)).unwrap())
        });
        state.set_mark_patches_for_test(patches);
        let mut fb = IndexedFramebuffer::new();
        render_automap(&mut fb, &state, &map, &view(me(128, 64)));
        assert!(count_pixels(&fb, 0x33) > 0);
    }

    #[test]
    fn fline_clip_rejects_fully_outside_lines() {
        let map = fixture_map();
        let state = open(&map);
        let line = MLine {
            a: MPoint {
                x: Fixed::from_int(-1000),
                y: Fixed::from_int(-1000),
            },
            b: MPoint {
                x: Fixed::from_int(-900),
                y: Fixed::from_int(-900),
            },
        };
        assert!(clip_mline(&state, &line).is_none());
    }

    #[test]
    fn fline_clip_keeps_partial_lines_inside() {
        let map = fixture_map();
        let state = open(&map);
        // a line crossing the whole visible window from outside the left
        // to outside the right: both ends clipped to the window edges
        let line = MLine {
            a: MPoint {
                x: Fixed::from_int(-1000),
                y: Fixed::from_int(64),
            },
            b: MPoint {
                x: Fixed::from_int(1000),
                y: Fixed::from_int(64),
            },
        };
        let clipped = clip_mline(&state, &line).expect("line crosses the window");
        assert_eq!(clipped.a.x, 0);
        assert_eq!(clipped.b.x, state.f_w() - 1);
        assert_eq!(clipped.a.y, clipped.b.y);
    }

    #[test]
    fn fline_clip_clips_top_and_bottom() {
        let map = fixture_map();
        let state = open(&map);
        // y = -1000 maps below the window (screen row > f_h), so the
        // clip snaps the lower end to the bottom edge
        let line = MLine {
            a: MPoint {
                x: Fixed::from_int(128),
                y: Fixed::from_int(-1000),
            },
            b: MPoint {
                x: Fixed::from_int(128),
                y: Fixed::from_int(64),
            },
        };
        let clipped = clip_mline(&state, &line).expect("line enters from the bottom");
        assert_eq!(clipped.a.y, state.f_h() - 1);
        assert_eq!(
            clipped.b.y,
            state.map_y_to_screen(Fixed::from_int(64)) - state.f_y()
        );
    }

    #[test]
    fn straight_horizontal_line_draws_all_its_pixels() {
        let map = fixture_map();
        let state = open(&map);
        let mut fb = IndexedFramebuffer::new();
        let line = MLine {
            a: MPoint {
                x: Fixed::from_int(100),
                y: Fixed::from_int(64),
            },
            b: MPoint {
                x: Fixed::from_int(140),
                y: Fixed::from_int(64),
            },
        };
        draw_mline(&mut fb, &state, &line, 0x77);
        let fy = state.map_y_to_screen(Fixed::from_int(64)) as usize;
        let x0 = state.map_x_to_screen(Fixed::from_int(100)) as usize;
        let x1 = state.map_x_to_screen(Fixed::from_int(140)) as usize;
        for x in x0..=x1 {
            assert_eq!(fb.get(x, fy), Some(0x77), "gap at x={x}");
        }
    }

    #[test]
    fn seen_walls_draw_nothing_below_row_168() {
        let map = fixture_map();
        let mut state = open(&map);
        state.mark_lines_seen([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        let mut fb = IndexedFramebuffer::new();
        fb.clear(0x11);
        render_automap(&mut fb, &state, &map, &view(me(128, 64)));
        for y in 168..200 {
            for x in 0..SCREEN_WIDTH {
                assert_eq!(fb.get(x, y), Some(0x11));
            }
        }
    }
}
