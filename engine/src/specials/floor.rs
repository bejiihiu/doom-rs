//! Floor movers, ported from `p_floor.c`.
//!
//! [`FloorMover`] is the vanilla `floormove_t`; [`FLOORSPEED`] and
//! [`run_floor_mover`] are `T_MoveFloor`'s constants and thinker step.
//! [`ev_do_floor`], [`ev_build_stairs`] and [`ev_do_donut`] are the
//! `EV_*` actions the line-special dispatcher calls.
//!
//! ## Sector bookkeeping
//!
//! Vanilla keeps the `sec->specialdata` pointer per sector and refuses to
//! start a mover on a busy sector; here [`super::sector_busy`] scans the
//! thinker registry for any mover (door, plat, ceiling, floor) on the
//! sector instead. The rest of the mechanics are 1:1 with the original.
//!
//! ## raiseToTexture and `textureheight[]`
//!
//! Vanilla reads the render-side `textureheight[]` table directly. The
//! texture set lives behind the render layer here, so the resolver comes
//! in through [`crate::specials::SpecialContext::texture_height`]. When it
//! is missing (or resolves no bottom textures for the sector) the special
//! is skipped with a warning; vanilla would have started the floor
//! towards `MAXINT` and corrupted the map.
//!
//! ## lowerAndChange quirks kept and dropped
//!
//! Vanilla's `lowerAndChange` loop re-read `sec->linecount` after
//! reassigning `sec` inside the loop, which could index
//! `sectors[secnum].lines[]` past its end and pick a texture/special from
//! an unrelated sector; and `newspecial` stayed uninitialized when no
//! surrounding sector matched the destination height. Both are dropped
//! here: the scan is bounded to the activating sector's own lines, and an
//! unmatched `lowerAndChange` applies no texture/special change
//! (`new_special = 0`, the floor's own flat).

use crate::fixed::{FRACUNIT, Fixed};
use crate::info::SfxId;
use crate::map::types::ML_TWOSIDED;
use crate::specials::util::{
    find_highest_floor_surrounding, find_lowest_ceiling_surrounding, find_lowest_floor_surrounding,
    find_next_highest_floor, find_sector_from_line_tag, sector_lines,
};
use crate::specials::{PassCtx, SpecialContext, SpecialEvent, sector_busy};
use crate::thinker::{Thinker, ThinkerId, ThinkerRegistry};

use super::movers::{MoveOutcome, PlaneKind, move_plane};

/// Vanilla `FLOORSPEED`.
pub const FLOORSPEED: Fixed = Fixed::from_raw(FRACUNIT);
const EIGHT_UNITS: Fixed = Fixed::from_raw(8 * FRACUNIT);

/// Vanilla `floor_e`. Discriminants follow the original order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FloorKind {
    /// Lower floor to highest surrounding floor.
    LowerFloor = 0,
    /// Lower floor to lowest surrounding floor.
    LowerFloorToLowest = 1,
    /// Lower floor to highest surrounding floor, very fast.
    TurboLower = 2,
    /// Raise floor to lowest surrounding ceiling.
    RaiseFloor = 3,
    /// Raise floor to next highest surrounding floor.
    RaiseFloorToNearest = 4,
    /// Raise floor to shortest height texture around it.
    RaiseToTexture = 5,
    /// Lower floor to lowest surrounding floor and change floorpic.
    LowerAndChange = 6,
    /// Raise 24 units.
    RaiseFloor24 = 7,
    /// Raise 24 units and change floorpic/special to the line's front
    /// sector's.
    RaiseFloor24AndChange = 8,
    /// Raise floor to lowest surrounding ceiling and crush everything in
    /// the way.
    RaiseFloorCrush = 9,
    /// Raise to next highest floor, turbo speed.
    RaiseFloorTurbo = 10,
    /// The donut: rise the slime sector and lower the hole to the
    /// surrounding sector height.
    DonutRaise = 11,
    /// Raise 512 units.
    RaiseFloor512 = 12,
}

/// Vanilla `floormove_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FloorMover {
    /// Which floor action this mover performs.
    pub kind: FloorKind,
    /// True for `raiseFloorCrush`: things in the way get hurt.
    pub crush: bool,
    /// Index of the moving sector.
    pub sector: usize,
    /// Vanilla `direction`: 1 = up, -1 = down.
    pub direction: i32,
    /// Sector special to apply at the destination (vanilla `newspecial`,
    /// applied by `donutRaise`/`lowerAndChange`).
    pub new_special: i16,
    /// Flat to apply at the destination (vanilla `texture`, the flat
    /// picnum; applied by `donutRaise`/`lowerAndChange`).
    pub texture: [u8; 8],
    /// Where the floor should stop.
    pub floor_dest_height: Fixed,
    /// Movement speed per tic.
    pub speed: Fixed,
}

/// Vanilla `T_MoveFloor`: the per-tic step of a [`FloorMover`].
pub(crate) fn run_floor_mover(reg: &mut ThinkerRegistry, id: ThinkerId, ctx: &mut PassCtx<'_>) {
    let floor = match reg.get(id) {
        Some(Thinker::FloorMover(floor)) => *floor,
        _ => return,
    };

    let res = move_plane(
        reg,
        ctx,
        floor.sector,
        floor.speed,
        floor.floor_dest_height,
        floor.crush,
        PlaneKind::Floor,
        floor.direction,
    );

    if (ctx.leveltime & 7) == 0 {
        ctx.events.push(SpecialEvent::SectorSound {
            sector: floor.sector,
            sfx: SfxId::sfx_stnmov,
        });
    }

    if res == MoveOutcome::PastDest {
        if floor.direction == 1 {
            if floor.kind == FloorKind::DonutRaise {
                ctx.map.sectors[floor.sector].special = floor.new_special;
                ctx.map.sectors[floor.sector].floor_pic = floor.texture;
            }
        } else if floor.direction == -1 && floor.kind == FloorKind::LowerAndChange {
            ctx.map.sectors[floor.sector].special = floor.new_special;
            ctx.map.sectors[floor.sector].floor_pic = floor.texture;
        }
        reg.remove(id);
        ctx.events.push(SpecialEvent::SectorSound {
            sector: floor.sector,
            sfx: SfxId::sfx_pstop,
        });
        return;
    }

    if let Some(Thinker::FloorMover(slot)) = reg.get_mut(id) {
        *slot = floor;
    }
}

/// Vanilla `EV_DoFloor`: start every tagged sector's floor moving.
/// Returns true when at least one mover was started.
pub fn ev_do_floor(ctx: &mut SpecialContext, line: usize, kind: FloorKind) -> bool {
    let tag = ctx.map.linedefs[line].tag;
    ev_do_floor_impl(ctx, Some(line), tag, kind)
}

/// Like [`ev_do_floor`] but with a bare tag; for the triggers that are
/// not tied to a linedef (the boss-death floor actions).
pub fn ev_do_floor_tag(ctx: &mut SpecialContext, tag: i16, kind: FloorKind) -> bool {
    ev_do_floor_impl(ctx, None, tag, kind)
}

/// Shared body; `line` is `Some` when called from a linedef (the
/// `RaiseFloor24AndChange` kind needs the line's front sector).
fn ev_do_floor_impl(
    ctx: &mut SpecialContext,
    line: Option<usize>,
    tag: i16,
    kind: FloorKind,
) -> bool {
    let mut rtn = false;
    let mut cursor = None;

    while let Some(secnum) = find_sector_from_line_tag(ctx.map, tag, cursor) {
        cursor = Some(secnum);
        if sector_busy(ctx.store, &ctx.pending_thinkers, secnum) {
            continue;
        }

        let crush = kind == FloorKind::RaiseFloorCrush;
        let mut floor = FloorMover {
            kind,
            crush,
            sector: secnum,
            direction: 0,
            new_special: 0,
            texture: ctx.map.sectors[secnum].floor_pic,
            floor_dest_height: Fixed::ZERO,
            speed: FLOORSPEED,
        };

        match kind {
            FloorKind::LowerFloor => {
                floor.direction = -1;
                floor.floor_dest_height = find_highest_floor_surrounding(ctx.map, secnum);
            }
            FloorKind::LowerFloorToLowest => {
                floor.direction = -1;
                floor.floor_dest_height = find_lowest_floor_surrounding(ctx.map, secnum);
            }
            FloorKind::TurboLower => {
                floor.direction = -1;
                floor.speed = Fixed::from_raw(FLOORSPEED.to_raw() * 4);
                floor.floor_dest_height = find_highest_floor_surrounding(ctx.map, secnum);
                if floor.floor_dest_height != ctx.map.sectors[secnum].floor_height {
                    floor.floor_dest_height += EIGHT_UNITS;
                }
            }
            FloorKind::RaiseFloorCrush | FloorKind::RaiseFloor => {
                floor.direction = 1;
                floor.floor_dest_height = find_lowest_ceiling_surrounding(ctx.map, secnum);
                let ceiling = ctx.map.sectors[secnum].ceiling_height;
                if floor.floor_dest_height > ceiling {
                    floor.floor_dest_height = ceiling;
                }
                if kind == FloorKind::RaiseFloorCrush {
                    floor.floor_dest_height -= EIGHT_UNITS;
                }
            }
            FloorKind::RaiseFloorTurbo => {
                floor.direction = 1;
                floor.speed = Fixed::from_raw(FLOORSPEED.to_raw() * 4);
                floor.floor_dest_height =
                    find_next_highest_floor(ctx.map, secnum, ctx.map.sectors[secnum].floor_height);
            }
            FloorKind::RaiseFloorToNearest => {
                floor.direction = 1;
                floor.floor_dest_height =
                    find_next_highest_floor(ctx.map, secnum, ctx.map.sectors[secnum].floor_height);
            }
            FloorKind::RaiseFloor24 => {
                floor.direction = 1;
                floor.floor_dest_height =
                    ctx.map.sectors[secnum].floor_height + Fixed::from_raw(24 * FRACUNIT);
            }
            FloorKind::RaiseFloor512 => {
                floor.direction = 1;
                floor.floor_dest_height =
                    ctx.map.sectors[secnum].floor_height + Fixed::from_raw(512 * FRACUNIT);
            }
            FloorKind::RaiseFloor24AndChange => {
                floor.direction = 1;
                floor.floor_dest_height =
                    ctx.map.sectors[secnum].floor_height + Fixed::from_raw(24 * FRACUNIT);
                match line
                    .and_then(|line| ctx.map.linedefs.get(line))
                    .and_then(|line| line.frontsector)
                {
                    Some(front) => {
                        let front_pic = ctx.map.sectors[front].floor_pic;
                        let front_special = ctx.map.sectors[front].special;
                        ctx.map.sectors[secnum].floor_pic = front_pic;
                        ctx.map.sectors[secnum].special = front_special;
                    }
                    None => {
                        tracing::warn!(
                            "raiseFloor24AndChange: triggered by a bare tag (boss death); \
                             texture/special copy skipped"
                        );
                    }
                }
            }
            FloorKind::RaiseToTexture => {
                floor.direction = 1;
                let Some(minsize) = shortest_bottom_texture(ctx, secnum) else {
                    tracing::warn!(
                        "raiseToTexture: no bottom texture with a known height borders sector {secnum}; sector skipped"
                    );
                    continue;
                };
                floor.floor_dest_height = ctx.map.sectors[secnum].floor_height + minsize;
            }
            FloorKind::LowerAndChange => {
                floor.direction = -1;
                floor.floor_dest_height = find_lowest_floor_surrounding(ctx.map, secnum);
                // the destination sector's flat/special is adopted when the
                // floor gets there (vanilla applied it in T_MoveFloor)
                floor.texture = ctx.map.sectors[secnum].floor_pic;
                floor.new_special = 0;
                let mut adopted = false;
                for index in sector_lines(ctx.map, secnum) {
                    let linedef = &ctx.map.linedefs[index];
                    if (linedef.flags & ML_TWOSIDED) == 0 {
                        continue;
                    }
                    let (Some(front_side), Some(back_side)) =
                        (linedef.sidenum[0], linedef.sidenum[1])
                    else {
                        continue;
                    };
                    let front_sector = ctx.map.sidedefs[front_side].sector;
                    let other = if front_sector == secnum {
                        ctx.map.sidedefs[back_side].sector
                    } else {
                        front_sector
                    };
                    if ctx.map.sectors[other].floor_height == floor.floor_dest_height {
                        floor.texture = ctx.map.sectors[other].floor_pic;
                        floor.new_special = ctx.map.sectors[other].special;
                        adopted = true;
                        break;
                    }
                }
                if !adopted {
                    floor.new_special = 0;
                }
            }
            FloorKind::DonutRaise => {
                // vanilla's EV_DoFloor switch has no donutRaise case;
                // donutThinkers are spawned by EV_DoDonut directly
                tracing::warn!(
                    "ev_do_floor: DonutRaise is not a valid EV_DoFloor argument; sector {secnum} skipped"
                );
                continue;
            }
        }

        rtn = true;
        ctx.pending_thinkers.push(Thinker::FloorMover(floor));
    }

    rtn
}

/// The shortest `bottomtexture` height among the sector's two-sided
/// lines, both sides, resolved through
/// [`crate::specials::SpecialContext::texture_height`]. `None` when no
/// resolvable bottom texture exists.
fn shortest_bottom_texture(ctx: &SpecialContext, sector: usize) -> Option<Fixed> {
    let resolver = &ctx.texture_height?.0;
    let mut minsize: Option<Fixed> = None;
    for index in sector_lines(ctx.map, sector) {
        let linedef = &ctx.map.linedefs[index];
        if (linedef.flags & ML_TWOSIDED) == 0 {
            continue;
        }
        for sidenum in [linedef.sidenum[0], linedef.sidenum[1]] {
            let Some(side) = sidenum else { continue };
            let bottom = ctx.map.sidedefs[side].bottomtexture;
            let Some(height) = resolver(&bottom) else {
                continue;
            };
            minsize = Some(match minsize {
                Some(current) => current.min(height),
                None => height,
            });
        }
    }
    minsize
}

/// Vanilla `stair_e`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StairKind {
    /// Slowly build by 8 units per step (`FLOORSPEED/4`).
    Build8,
    /// Quickly build by 16 units per step (`FLOORSPEED*4`).
    Turbo16,
}

/// Vanilla `EV_BuildStairs`: build a staircase starting from every
/// tagged sector, walking through two-sided lines into sectors with the
/// same floor flat, 8 (or 16) units higher every step. Returns true when
/// at least one mover was started.
pub fn ev_build_stairs(ctx: &mut SpecialContext, line: usize, kind: StairKind) -> bool {
    let tag = ctx.map.linedefs[line].tag;
    let mut rtn = false;
    let mut cursor = None;

    let (speed, stair_size) = match kind {
        StairKind::Build8 => (Fixed::from_raw(FRACUNIT / 4), Fixed::from_raw(8 * FRACUNIT)),
        StairKind::Turbo16 => (
            Fixed::from_raw(FLOORSPEED.to_raw() * 4),
            Fixed::from_raw(16 * FRACUNIT),
        ),
    };

    while let Some(secnum) = find_sector_from_line_tag(ctx.map, tag, cursor) {
        cursor = Some(secnum);
        if sector_busy(ctx.store, &ctx.pending_thinkers, secnum) {
            continue;
        }

        rtn = true;
        let mut secnum = secnum;
        let texture = ctx.map.sectors[secnum].floor_pic;
        let mut height = ctx.map.sectors[secnum].floor_height + stair_size;

        ctx.pending_thinkers.push(Thinker::FloorMover(FloorMover {
            // T_MoveFloor only special-cases donutRaise/lowerAndChange;
            // the stair destination is explicit, so any raise kind works
            kind: FloorKind::RaiseFloor,
            crush: false,
            sector: secnum,
            direction: 1,
            new_special: 0,
            texture,
            floor_dest_height: height,
            speed,
        }));

        // Find the next sector to raise: two-sided lines where the current
        // sector is the front side, into a sector with the same flat.
        // Vanilla's `height += stairsize` ran before the busy check, so the
        // height accumulated even across busy sectors; kept 1:1.
        loop {
            let mut stepped = false;
            for index in sector_lines(ctx.map, secnum) {
                let linedef = &ctx.map.linedefs[index];
                if (linedef.flags & ML_TWOSIDED) == 0 {
                    continue;
                }
                if linedef.frontsector != Some(secnum) {
                    continue;
                }
                let Some(back_sector) = linedef.backsector else {
                    continue;
                };
                if ctx.map.sectors[back_sector].floor_pic != texture {
                    continue;
                }

                height += stair_size;

                if sector_busy(ctx.store, &ctx.pending_thinkers, back_sector) {
                    continue;
                }

                ctx.pending_thinkers.push(Thinker::FloorMover(FloorMover {
                    // see above: the kind is irrelevant for the stair dest
                    kind: FloorKind::RaiseFloor,
                    crush: false,
                    sector: back_sector,
                    direction: 1,
                    new_special: 0,
                    texture,
                    floor_dest_height: height,
                    speed,
                }));

                secnum = back_sector;
                stepped = true;
                break;
            }
            if !stepped {
                break;
            }
        }
    }

    rtn
}

/// Vanilla `EV_DoDonut`: the donut elevator. The tagged sector lowers to
/// the ring sector's destination; the ring sector (the one across the
/// tagged sector's first line) rises to the inner sector's floor height
/// and adopts its flat. Returns true when at least one mover was started.
pub fn ev_do_donut(ctx: &mut SpecialContext, line: usize) -> bool {
    let tag = ctx.map.linedefs[line].tag;
    let mut rtn = false;
    let mut cursor = None;

    while let Some(secnum) = find_sector_from_line_tag(ctx.map, tag, cursor) {
        cursor = Some(secnum);
        if sector_busy(ctx.store, &ctx.pending_thinkers, secnum) {
            continue;
        }
        rtn = true;

        // the ring is the sector behind the tagged sector's first line
        let first_line = match sector_lines(ctx.map, secnum).first() {
            Some(&index) => index,
            None => {
                tracing::warn!("donut: sector {secnum} has no lines; skipped");
                continue;
            }
        };
        let ring = match super::util::get_next_sector(ctx.map, first_line, secnum) {
            Some(ring) => ring,
            None => {
                tracing::warn!("donut: first line of sector {secnum} is one-sided; skipped");
                continue;
            }
        };

        // find the inner sector across one of the ring's two-sided lines,
        // not back into the tagged sector
        let half_speed = Fixed::from_raw(FRACUNIT / 2);
        for index in sector_lines(ctx.map, ring) {
            let linedef = &ctx.map.linedefs[index];
            // vanilla's `!flags & ML_TWOSIDED` tested flags == 0, not
            // one-sidedness; skip anything without a back sector instead
            let Some(inner) = linedef.backsector else {
                continue;
            };
            if inner == secnum {
                continue;
            }
            let inner_floor = ctx.map.sectors[inner].floor_height;
            let inner_pic = ctx.map.sectors[inner].floor_pic;

            // rising slime
            ctx.pending_thinkers.push(Thinker::FloorMover(FloorMover {
                kind: FloorKind::DonutRaise,
                crush: false,
                sector: ring,
                direction: 1,
                new_special: 0,
                texture: inner_pic,
                floor_dest_height: inner_floor,
                speed: half_speed,
            }));
            // lowering donut-hole
            ctx.pending_thinkers.push(Thinker::FloorMover(FloorMover {
                // the kind is irrelevant: lowerAndChange is the only
                // down-kind T_MoveFloor special-cases
                kind: FloorKind::LowerFloor,
                crush: false,
                sector: secnum,
                direction: -1,
                new_special: 0,
                texture: ctx.map.sectors[secnum].floor_pic,
                floor_dest_height: inner_floor,
                speed: half_speed,
            }));
            break;
        }
    }

    rtn
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::FRACUNIT;
    use crate::info::MobjType;
    use crate::map::Map;
    use crate::mobj::MobjStore;
    use crate::rng::DoomRng;
    use crate::specials::testutil::{
        donut_map, spawn_player, spawn_thing, split_map, split_map_custom, test_player,
    };
    use crate::specials::{SpecialContext, run_thinkers};
    use crate::thinker::Thinker;

    fn fixed(units: i32) -> Fixed {
        Fixed::from_raw(units * FRACUNIT)
    }

    struct World {
        map: Map,
        store: MobjStore,
        players: Vec<crate::player::Player>,
        rng: DoomRng,
    }

    fn world(north_floor: i16, north_ceil: i16, north_tag: i16) -> World {
        World {
            map: split_map(north_floor, north_ceil, north_tag),
            store: MobjStore::new(),
            players: vec![test_player()],
            rng: DoomRng::with_seed(0),
        }
    }

    fn ctx<'a>(w: &'a mut World) -> SpecialContext<'a> {
        SpecialContext::new(&mut w.map, &mut w.store, &mut w.players, &mut w.rng)
    }

    fn tick(ctx: &mut SpecialContext<'_>, n: usize) {
        for _ in 0..n {
            run_thinkers(ctx);
        }
    }

    fn floor_thinkers(store: &MobjStore) -> Vec<FloorMover> {
        store
            .thinkers()
            .filter_map(|(_, t)| match t {
                Thinker::FloorMover(f) => Some(*f),
                _ => None,
            })
            .collect()
    }

    fn tagged_line_2(w: &mut World, tag: i16) {
        w.map.linedefs[2].tag = tag;
    }

    #[test]
    fn raise_floor_to_lowest_ceiling_clamps_to_own_ceiling() {
        let mut w = world(20, 96, 7);
        // the lowest surrounding ceiling is south's 128; the sector's own
        // ceiling (96) is the clamp
        tagged_line_2(&mut w, 7);
        let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 32, 32);
        let _ = avatar;
        let mut ctx = ctx(&mut w);

        assert!(ev_do_floor(&mut ctx, 2, FloorKind::RaiseFloor));
        // sanity: queued before the pass runs
        assert_eq!(ctx.pending_thinkers.len(), 1);

        tick(&mut ctx, 80);
        assert_eq!(ctx.map.sectors[1].floor_height, fixed(96));
        assert!(
            floor_thinkers(ctx.store).is_empty(),
            "floor thinker must finish and leave"
        );
        assert!(ctx.events.iter().any(|e| matches!(
            e,
            SpecialEvent::SectorSound {
                sector: 1,
                sfx: SfxId::sfx_pstop
            }
        )));
    }

    #[test]
    fn raise_floor_to_nearest_uses_next_highest_surrounding_floor() {
        let mut w = world(0, 128, 7);
        // south floor is higher (72): north finds it as the next highest
        w.map.sectors[0].floor_height = fixed(72);
        tagged_line_2(&mut w, 7);
        let mut ctx = ctx(&mut w);

        assert!(ev_do_floor(&mut ctx, 2, FloorKind::RaiseFloorToNearest));
        tick(&mut ctx, 80);
        assert_eq!(ctx.map.sectors[1].floor_height, fixed(72));
        assert!(floor_thinkers(ctx.store).is_empty());
    }

    #[test]
    fn lower_floor_to_highest_surrounding_stops_at_highest_neighbor() {
        let mut w = world(64, 128, 7);
        // north floor 64; south is 0: the highest surrounding floor is 0
        tagged_line_2(&mut w, 7);
        let mut ctx = ctx(&mut w);

        assert!(ev_do_floor(&mut ctx, 2, FloorKind::LowerFloor));
        tick(&mut ctx, 80);
        assert_eq!(ctx.map.sectors[1].floor_height, fixed(0));
    }

    #[test]
    fn lower_and_change_adopts_destination_flat_and_special() {
        let mut map = split_map_custom("SOUTHPIC", 5, "NORTHPIC", 9);
        map.sectors[1].tag = 7;
        map.linedefs[2].tag = 7;
        let mut w = World {
            map,
            store: MobjStore::new(),
            players: vec![test_player()],
            rng: DoomRng::with_seed(0),
        };
        let mut ctx = ctx(&mut w);

        assert!(ev_do_floor(&mut ctx, 2, FloorKind::LowerAndChange));
        tick(&mut ctx, 40);
        assert_eq!(ctx.map.sectors[1].floor_height, fixed(0));
        assert_eq!(ctx.map.sectors[1].floor_pic, name8("SOUTHPIC"));
        assert_eq!(ctx.map.sectors[1].special, 5);
    }

    fn name8(name: &str) -> [u8; 8] {
        let mut out = [0u8; 8];
        for (dst, src) in out.iter_mut().zip(name.as_bytes().iter().take(8)) {
            *dst = src.to_ascii_uppercase();
        }
        out
    }

    #[test]
    fn raise_to_texture_uses_the_resolver_and_stops_at_the_shortest_bottom_texture() {
        let mut map = split_map(0, 128, 7);
        map.sectors[1].tag = 7;
        map.linedefs[2].tag = 7;
        // the divider carries the bottom textures the special measures
        map.sidedefs[4].bottomtexture = name8("WALL1");
        map.sidedefs[5].bottomtexture = name8("WALL1");
        let texture_height = |name: &[u8; 8]| -> Option<Fixed> {
            if name == &name8("WALL1") {
                Some(fixed(24))
            } else {
                None
            }
        };
        let mut w = World {
            map,
            store: MobjStore::new(),
            players: vec![test_player()],
            rng: DoomRng::with_seed(0),
        };
        let mut ctx = SpecialContext::new(&mut w.map, &mut w.store, &mut w.players, &mut w.rng)
            .with_texture_height(&texture_height);

        assert!(ev_do_floor(&mut ctx, 2, FloorKind::RaiseToTexture));
        tick(&mut ctx, 30);
        assert_eq!(ctx.map.sectors[1].floor_height, fixed(24));
    }

    #[test]
    fn raise_to_texture_without_a_resolver_skips_the_sector() {
        let mut w = world(0, 128, 7);
        tagged_line_2(&mut w, 7);
        let mut ctx = ctx(&mut w);

        assert!(!ev_do_floor(&mut ctx, 2, FloorKind::RaiseToTexture));
        assert!(ctx.pending_thinkers.is_empty());
    }

    #[test]
    fn stairs_build_two_steps_from_the_tagged_sector() {
        let mut w = world(0, 128, 99);
        // only the south sector starts a staircase
        w.map.linedefs[2].tag = 0; // sector 0 has tag 0
        let mut ctx = ctx(&mut w);

        assert!(ev_build_stairs(&mut ctx, 2, StairKind::Turbo16));
        // two movers: south (16) and north (32)
        tick(&mut ctx, 10);
        assert_eq!(ctx.map.sectors[0].floor_height, fixed(16));
        assert_eq!(ctx.map.sectors[1].floor_height, fixed(32));
        assert!(floor_thinkers(ctx.store).is_empty(), "stairs must finish");
    }

    #[test]
    fn donut_completes_and_the_middle_adopts_the_deet_flat() {
        let mut map = donut_map();
        map.linedefs[0].tag = 99;
        let mut w = World {
            map,
            store: MobjStore::new(),
            players: vec![test_player()],
            rng: DoomRng::with_seed(0),
        };
        let mut ctx = ctx(&mut w);

        assert!(ev_do_donut(&mut ctx, 0));
        assert_eq!(ctx.pending_thinkers.len(), 2);

        tick(&mut ctx, 200);
        assert_eq!(
            ctx.map.sectors[0].floor_height,
            fixed(32),
            "ring lowered to the island height"
        );
        assert_eq!(
            ctx.map.sectors[1].floor_height,
            fixed(32),
            "slime rose to the island height"
        );
        assert_eq!(ctx.map.sectors[1].floor_pic, name8("HOLE1"));
        assert_eq!(ctx.map.sectors[1].special, 0);
        assert!(floor_thinkers(ctx.store).is_empty(), "donut must finish");
    }

    #[test]
    fn floor_crusher_hurts_a_thing_in_the_way() {
        let mut w = world(0, 128, 7);
        // monster in the tagged sector; the floor rise crushes it
        let monster = spawn_thing(
            &mut w.store,
            &w.map,
            &mut w.rng,
            MobjType::MT_POSSESSED,
            128,
            192,
        );
        tagged_line_2(&mut w, 7);
        let mut ctx = ctx(&mut w);

        assert!(ev_do_floor(&mut ctx, 2, FloorKind::RaiseFloorCrush));
        tick(&mut ctx, 100);
        let crushed = ctx.events.iter().any(
            |e| matches!(e, SpecialEvent::Damage { target, damage: 10, .. } if *target == monster),
        );
        assert!(crushed, "crusher must hurt the thing in the way");
    }

    #[test]
    fn busy_sector_is_skipped_by_floor_actions() {
        let mut w = world(0, 128, 7);
        tagged_line_2(&mut w, 7);
        let mut ctx = ctx(&mut w);

        assert!(ev_do_floor(&mut ctx, 2, FloorKind::RaiseFloor));
        // second action on the same sector is skipped while busy
        assert!(!ev_do_floor(&mut ctx, 2, FloorKind::LowerFloor));
        // and so is a door on a sector with an active floor mover
        assert!(!crate::specials::ev_do_door(
            &mut ctx,
            2,
            crate::specials::DoorKind::Normal
        ));

        tick(&mut ctx, 150);
        assert_eq!(ctx.map.sectors[1].floor_height, fixed(128));
    }

    #[test]
    fn no_move_needed_finishes_immediately() {
        let mut w = world(0, 128, 7);
        // south floor is the highest around; lowerFloor has nowhere to go
        w.map.sectors[1].floor_height = fixed(0);
        w.map.sectors[0].floor_height = fixed(0);
        tagged_line_2(&mut w, 7);
        let mut ctx = ctx(&mut w);

        assert!(ev_do_floor(&mut ctx, 2, FloorKind::LowerFloor));
        tick(&mut ctx, 2);
        assert!(floor_thinkers(ctx.store).is_empty());
    }
}
