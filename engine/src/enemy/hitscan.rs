//! Hitscan attacks: `P_AimLineAttack` / `P_LineAttack` and the puff/blood
//! spawners, ported from p_map.c and p_mobj.c.
//!
//! The original drives both through `P_PathTraverse` with a pair of
//! callbacks and a pile of file-scope globals (`linetarget`, `shootthing`,
//! `shootz`, `la_damage`, `attackrange`, `aimslope`, `topslope`,
//! `bottomslope`). Here those globals become locals on the call and the
//! traversal goes through [`crate::movement::traverse_path`], which keeps
//! the original's blockmap walk and stable-order intercept dispatch.
//!
//! `P_ShootSpecialLine` is not ported (the specials live in the gameplay
//! layer); a shot that hits a special line emits
//! [`GameEvent::ShootSpecial`] so the specials layer decides what it does.
//!
//! The puff/blood spawners take a `PuffParts` bundle — the exact long
//! list of borrows they need — so they work both from a full
//! [`EnemyContext`] (outside any traversal) and from a raw
//! [`MoveContext`] (inside one).

use crate::fixed::{FRACBITS, FRACUNIT, Fixed};
use crate::info::{MF_NOBLOOD, MobjType, StateId};
use crate::map::Map;
use crate::map::types::ML_TWOSIDED;
use crate::math::Point;
use crate::mobj::{MobjStore, SpawnSpec, SpawnZ};
use crate::movement::MF_SHOOTABLE;
use crate::movement::{
    Intercept, InterceptPayload, MoveContext, PT_ADDLINES, PT_ADDTHINGS, line_opening,
    traverse_path,
};
use crate::rng::DoomRng;
use crate::tables::{ANGLE_TO_FINE_SHIFT, Angle, FINE_COSINE, FINE_SINE};
use crate::thinker::ThinkerId;

use super::{EnemyContext, GameEvent, MELEERANGE};

/// The borrows the puff/blood spawners need, bundled.
///
/// Construct from an [`EnemyContext`] (outside a traversal) or a
/// [`MoveContext`] plus the current skill (inside one). `leveltime` and
/// `skill` are copied, everything else is borrowed.
pub(crate) struct PuffParts<'a> {
    pub map: &'a Map,
    pub store: &'a mut MobjStore,
    pub rng: &'a mut DoomRng,
    pub skill: crate::mobj::Skill,
    pub leveltime: i32,
}

impl<'a> PuffParts<'a> {
    pub(crate) fn from_enemy(ctx: &'a mut EnemyContext<'_>) -> Self {
        PuffParts {
            map: ctx.map,
            store: ctx.store,
            rng: ctx.rng,
            skill: ctx.rules.skill,
            leveltime: ctx.leveltime,
        }
    }

    pub(crate) fn from_move(ctx: &'a mut MoveContext<'_>, skill: crate::mobj::Skill) -> Self {
        PuffParts {
            map: ctx.map,
            store: ctx.store,
            rng: ctx.rng,
            skill,
            leveltime: ctx.leveltime,
        }
    }
}

/// Vanilla `P_SpawnPuff`.
///
/// Smoke puffs for wall hits and tracer exhaust. `attackrange` selects the
/// melee variant (`S_PUFF3`); the original compared the `attackrange`
/// global against `MELEERANGE`, we pass it explicitly.
pub(crate) fn spawn_puff(
    parts: &mut PuffParts<'_>,
    x: Fixed,
    y: Fixed,
    z: Fixed,
    attackrange: Fixed,
) -> ThinkerId {
    let z = z + Fixed::from_raw((parts.rng.next() as i32 - parts.rng.next() as i32) << 10);

    let id = parts.store.spawn_mobj(
        parts.map,
        parts.rng,
        SpawnSpec {
            skill: parts.skill,
            x,
            y,
            z: SpawnZ::At(z),
            mobj_type: MobjType::MT_PUFF,
        },
    );

    if let Some(m) = parts.store.mobj_mut(id) {
        m.momz = Fixed::ONE;
        let adj = i32::from(parts.rng.next() & 3);
        m.tics -= adj;
        if m.tics < 1 {
            m.tics = 1;
        }
    }

    // don't make punches spark on the wall
    if attackrange == MELEERANGE {
        let report = parts
            .store
            .set_mobj_state(id, StateId::S_PUFF3, parts.leveltime);
        // S_PUFF3 carries no action; nothing to dispatch
        debug_assert!(report.actions.is_empty());
    }

    id
}

/// Vanilla `P_SpawnBlood`.
pub(crate) fn spawn_blood(
    parts: &mut PuffParts<'_>,
    x: Fixed,
    y: Fixed,
    z: Fixed,
    damage: i32,
) -> ThinkerId {
    let z = z + Fixed::from_raw((parts.rng.next() as i32 - parts.rng.next() as i32) << 10);

    let id = parts.store.spawn_mobj(
        parts.map,
        parts.rng,
        SpawnSpec {
            skill: parts.skill,
            x,
            y,
            z: SpawnZ::At(z),
            mobj_type: MobjType::MT_BLOOD,
        },
    );

    if let Some(m) = parts.store.mobj_mut(id) {
        m.momz = Fixed::from_raw(2 * FRACUNIT);
        let adj = i32::from(parts.rng.next() & 3);
        m.tics -= adj;
        if m.tics < 1 {
            m.tics = 1;
        }
    }

    let state = if (9..=12).contains(&damage) {
        StateId::S_BLOOD2
    } else if damage < 9 {
        StateId::S_BLOOD3
    } else {
        return id; // over 12: S_BLOOD1, no state switch
    };
    let report = parts.store.set_mobj_state(id, state, parts.leveltime);
    debug_assert!(report.actions.is_empty());

    id
}

/// The outcome of `P_AimLineAttack`: the aim slope and the aimed-at mobj.
/// Vanilla `linetarget`/`aimslope` globals.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AimOutcome {
    /// The slope to aim at (vanilla `aimslope`). Meaningful only when
    /// `target` is `Some`.
    pub slope: Fixed,
    /// The mobj under the crosshair (vanilla `linetarget`).
    pub target: Option<ThinkerId>,
}

/// Vanilla `P_AimLineAttack`.
///
/// Traces from `t1` along `angle` up to `distance` for the first
/// shootable thing; returns the exact aim elevation, or zero slope with no
/// target when nothing was found.
pub fn aim_line_attack(
    ctx: &mut EnemyContext<'_>,
    t1: ThinkerId,
    angle: Angle,
    distance: Fixed,
) -> AimOutcome {
    let Some(shooter) = ctx.store.mobj(t1) else {
        return AimOutcome {
            slope: Fixed::ZERO,
            target: None,
        };
    };

    let fine = angle.to_raw() >> ANGLE_TO_FINE_SHIFT;
    let dist_units = distance.to_raw() >> FRACBITS;
    let x2 =
        shooter.x + Fixed::from_raw(dist_units.wrapping_mul(FINE_COSINE[fine as usize].to_raw()));
    let y2 =
        shooter.y + Fixed::from_raw(dist_units.wrapping_mul(FINE_SINE[fine as usize].to_raw()));

    let shootz =
        shooter.z + Fixed::from_raw(shooter.height.to_raw() >> 1) + Fixed::from_raw(8 * FRACUNIT);

    // can't shoot outside view angles
    let mut aim = AimState {
        shooter: t1,
        shootz,
        attackrange: distance,
        topslope: Fixed::from_raw(100 * FRACUNIT / 160),
        bottomslope: Fixed::from_raw(-100 * FRACUNIT / 160),
        target: None,
        slope: Fixed::ZERO,
    };

    let start = Point {
        x: shooter.x,
        y: shooter.y,
    };
    let end = Point { x: x2, y: y2 };

    let mut mctx = ctx.move_context();
    let _ = traverse_path(
        &mut mctx,
        start,
        end,
        PT_ADDLINES | PT_ADDTHINGS,
        |mctx, intercept| match intercept.payload {
            InterceptPayload::Line(line) => aim_traverse_line(&mut aim, mctx, line, intercept),
            InterceptPayload::Thing(thing) => aim_traverse_thing(&mut aim, mctx, thing, intercept),
        },
    );

    AimOutcome {
        slope: aim.slope,
        target: aim.target,
    }
}

struct AimState {
    shooter: ThinkerId,
    shootz: Fixed,
    attackrange: Fixed,
    topslope: Fixed,
    bottomslope: Fixed,
    target: Option<ThinkerId>,
    slope: Fixed,
}

fn aim_traverse_line(
    aim: &mut AimState,
    mctx: &mut MoveContext<'_>,
    line: usize,
    intercept: &Intercept,
) -> bool {
    let linedef = &mctx.map.linedefs[line];

    if linedef.flags & ML_TWOSIDED == 0 {
        return false; // stop
    }

    // crosses a two sided line; it restricts the possible target ranges
    let (Some(front_index), Some(back_index)) = (linedef.frontsector, linedef.backsector) else {
        return false; // stop (defensive; can't window a line without both sectors)
    };
    let front = &mctx.map.sectors[front_index];
    let back = &mctx.map.sectors[back_index];

    let opening = line_opening(mctx.map, line);

    if opening.bottom >= opening.top {
        return false; // stop
    }

    let dist = aim.attackrange * intercept.frac;

    if front.floor_height != back.floor_height {
        let slope = (opening.bottom - aim.shootz) / dist;
        if slope > aim.bottomslope {
            aim.bottomslope = slope;
        }
    }

    if front.ceiling_height != back.ceiling_height {
        let slope = (opening.top - aim.shootz) / dist;
        if slope < aim.topslope {
            aim.topslope = slope;
        }
    }

    if aim.topslope <= aim.bottomslope {
        return false; // stop
    }

    true // shot continues
}

fn aim_traverse_thing(
    aim: &mut AimState,
    mctx: &mut MoveContext<'_>,
    thing: ThinkerId,
    intercept: &Intercept,
) -> bool {
    if thing == aim.shooter {
        return true; // can't shoot self
    }

    let Some(th) = mctx.store.mobj(thing) else {
        return true;
    };

    if !th.has_flag(MF_SHOOTABLE) {
        return true; // corpse or something
    }

    // check angles to see if the thing can be aimed at
    let dist = aim.attackrange * intercept.frac;
    let thingtopslope = (th.z + th.height - aim.shootz) / dist;

    if thingtopslope < aim.bottomslope {
        return true; // shot over the thing
    }

    let thingbottomslope = (th.z - aim.shootz) / dist;

    if thingbottomslope > aim.topslope {
        return true; // shot under the thing
    }

    // this thing can be hit!
    let thingtopslope = thingtopslope.min(aim.topslope);
    let thingbottomslope = thingbottomslope.max(aim.bottomslope);

    aim.slope = Fixed::from_raw((thingtopslope + thingbottomslope).to_raw() / 2);
    aim.target = Some(thing);

    false // don't go any farther
}

/// Vanilla `P_LineAttack`.
///
/// `slope` is the aim elevation returned by [`aim_line_attack`] (vanilla
/// passes it through the `aimslope` global). A `damage` of `0` performs a
/// test trace, like the original.
pub fn line_attack(
    ctx: &mut EnemyContext<'_>,
    t1: ThinkerId,
    angle: Angle,
    distance: Fixed,
    slope: Fixed,
    damage: i32,
) {
    let Some(shooter) = ctx.store.mobj(t1) else {
        return;
    };

    let fine = angle.to_raw() >> ANGLE_TO_FINE_SHIFT;
    let dist_units = distance.to_raw() >> FRACBITS;

    // vanilla P_LineAttack computes the end point from the UN-nudged
    // position, then P_PathTraverse nudges its own trace internally;
    // mirror that split exactly
    let x2 =
        shooter.x + Fixed::from_raw(dist_units.wrapping_mul(FINE_COSINE[fine as usize].to_raw()));
    let y2 =
        shooter.y + Fixed::from_raw(dist_units.wrapping_mul(FINE_SINE[fine as usize].to_raw()));

    // vanilla P_PathTraverse nudges a start point that sits exactly on a
    // mapblock boundary (128 units); mirror the exact same condition here
    // because the local `trace` values are used for hit positions below
    let origin = &ctx.map.blockmap;
    let mut tx = shooter.x;
    let mut ty = shooter.y;
    if (tx.to_raw() as i64 - origin.x_origin.to_raw() as i64) & 0x007f_ffff == 0 {
        tx += Fixed::from_raw(FRACUNIT);
    }
    if (ty.to_raw() as i64 - origin.y_origin.to_raw() as i64) & 0x007f_ffff == 0 {
        ty += Fixed::from_raw(FRACUNIT);
    }

    let shootz =
        shooter.z + Fixed::from_raw(shooter.height.to_raw() >> 1) + Fixed::from_raw(8 * FRACUNIT);

    let mut la = LineAttackState {
        shooter: t1,
        shootz,
        attackrange: distance,
        slope,
        damage,
        trace_x: tx,
        trace_y: ty,
        trace_dx: x2 - tx,
        trace_dy: y2 - ty,
        skill: ctx.rules.skill,
        events: Vec::new(),
    };

    let start = Point { x: tx, y: ty };
    let end = Point { x: x2, y: y2 };
    let sky_flat = ctx.sky_flat;

    let mut mctx = ctx.move_context();
    let _ = traverse_path(
        &mut mctx,
        start,
        end,
        PT_ADDLINES | PT_ADDTHINGS,
        |mctx, intercept| match intercept.payload {
            InterceptPayload::Line(line) => {
                shoot_traverse_line(&mut la, mctx, line, intercept, sky_flat)
            }
            InterceptPayload::Thing(thing) => shoot_traverse_thing(&mut la, mctx, thing, intercept),
        },
    );

    ctx.events.append(&mut la.events);
}

struct LineAttackState {
    shooter: ThinkerId,
    shootz: Fixed,
    attackrange: Fixed,
    slope: Fixed,
    damage: i32,
    trace_x: Fixed,
    trace_y: Fixed,
    trace_dx: Fixed,
    trace_dy: Fixed,
    skill: crate::mobj::Skill,
    events: Vec<GameEvent>,
}

fn shoot_traverse_line(
    la: &mut LineAttackState,
    mctx: &mut MoveContext<'_>,
    line: usize,
    intercept: &Intercept,
    sky_flat: [u8; 8],
) -> bool {
    let linedef = &mctx.map.linedefs[line];

    if linedef.special != 0 {
        la.events.push(GameEvent::ShootSpecial {
            line,
            thing: la.shooter,
        });
    }

    if linedef.flags & ML_TWOSIDED != 0 {
        // crosses a two sided line; see if the shot is stopped
        let (Some(front_index), Some(back_index)) = (linedef.frontsector, linedef.backsector)
        else {
            return hit_line(la, mctx, line, intercept, sky_flat);
        };
        let front = &mctx.map.sectors[front_index];
        let back = &mctx.map.sectors[back_index];
        let opening = line_opening(mctx.map, line);
        let dist = la.attackrange * intercept.frac;

        if front.floor_height != back.floor_height {
            let slope = (opening.bottom - la.shootz) / dist;
            if slope > la.slope {
                return hit_line(la, mctx, line, intercept, sky_flat);
            }
        }

        if front.ceiling_height != back.ceiling_height {
            let slope = (opening.top - la.shootz) / dist;
            if slope < la.slope {
                return hit_line(la, mctx, line, intercept, sky_flat);
            }
        }

        // shot continues
        return true;
    }

    hit_line(la, mctx, line, intercept, sky_flat)
}

fn hit_line(
    la: &mut LineAttackState,
    mctx: &mut MoveContext<'_>,
    line: usize,
    intercept: &Intercept,
    sky_flat: [u8; 8],
) -> bool {
    let linedef = &mctx.map.linedefs[line];

    // position a bit closer
    let frac = intercept.frac - Fixed::from_raw(4 * FRACUNIT) / la.attackrange;
    let x = la.trace_x + la.trace_dx * frac;
    let y = la.trace_y + la.trace_dy * frac;
    let z = la.shootz + la.slope * (frac * la.attackrange);

    // don't shoot the sky!
    if let Some(front_index) = linedef.frontsector {
        let front = &mctx.map.sectors[front_index];
        if front.ceiling_pic == sky_flat {
            if z > front.ceiling_height {
                return false;
            }

            // it's a sky hack wall
            if let Some(back_index) = linedef.backsector {
                let back = &mctx.map.sectors[back_index];
                if back.ceiling_pic == sky_flat {
                    return false;
                }
            }
        }
    }

    // spawn bullet puffs
    let mut parts = PuffParts::from_move(mctx, la.skill);
    spawn_puff(&mut parts, x, y, z, la.attackrange);

    false
}

fn shoot_traverse_thing(
    la: &mut LineAttackState,
    mctx: &mut MoveContext<'_>,
    thing: ThinkerId,
    intercept: &Intercept,
) -> bool {
    if thing == la.shooter {
        return true; // can't shoot self
    }

    // grab the flags first; the borrow ends before we mutate the world
    let Some(th) = mctx.store.mobj(thing) else {
        return true;
    };

    if !th.has_flag(MF_SHOOTABLE) {
        return true; // corpse or something
    }

    // check angles to see if the thing can be aimed at
    let dist = la.attackrange * intercept.frac;
    let thingtopslope = (th.z + th.height - la.shootz) / dist;

    if thingtopslope < la.slope {
        return true; // shot over the thing
    }

    let thingbottomslope = (th.z - la.shootz) / dist;

    if thingbottomslope > la.slope {
        return true; // shot under the thing
    }

    let noblood = th.has_flag(MF_NOBLOOD);

    // hit thing; position a bit closer
    let frac = intercept.frac - Fixed::from_raw(10 * FRACUNIT) / la.attackrange;
    let x = la.trace_x + la.trace_dx * frac;
    let y = la.trace_y + la.trace_dy * frac;
    let z = la.shootz + la.slope * (frac * la.attackrange);

    // spawn bullet puffs or blood spots,
    // depending on target type.
    let mut parts = PuffParts::from_move(mctx, la.skill);
    if noblood {
        spawn_puff(&mut parts, x, y, z, la.attackrange);
    } else {
        spawn_blood(&mut parts, x, y, z, la.damage);
    }

    if la.damage != 0 {
        la.events.push(GameEvent::Damage {
            target: thing,
            inflictor: Some(la.shooter),
            source: Some(la.shooter),
            damage: la.damage,
        });
    }

    false
}
#[cfg(test)]
mod tests {
    use super::*;

    use crate::enemy::MISSILERANGE;
    use crate::enemy::testutil::{
        TestWorld, open_map, players, spawn_player, spawn_thing, split_map, test_context,
    };
    use crate::fixed::Fixed;
    use crate::info::MobjType;
    use crate::map::types::{ML_BLOCKING, ML_TWOSIDED};
    use crate::mobj::MobjStore;
    use crate::rng::DoomRng;

    #[test]
    fn aim_finds_the_player_in_the_open() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        let mut ctx = test_context(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut events,
        );
        let aim = aim_line_attack(&mut ctx, monster, Angle::ZERO, MISSILERANGE);
        assert_eq!(aim.target, players[0].mo);
        assert!(aim.slope.to_raw().abs() < Fixed::from_int(1).to_raw());
    }

    #[test]
    fn aim_is_blocked_by_a_one_sided_wall() {
        let map = split_map(0, 128, ML_BLOCKING, 0x00);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 192);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        let mut ctx = test_context(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut events,
        );
        // fire at the divider: a one-sided line stops the trace before the player
        let an = Angle::from_raw(0x2000_0000); // 45 degrees, north-east
        let aim = aim_line_attack(&mut ctx, monster, an, MISSILERANGE);
        assert_eq!(aim.target, None);
    }

    #[test]
    fn aim_goes_through_an_open_window() {
        let map = split_map(0, 128, ML_TWOSIDED, 0x00);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 192);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        let mut ctx = test_context(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut events,
        );
        let an = Angle::from_raw(0x2000_0000); // 45 degrees, north-east
        let aim = aim_line_attack(&mut ctx, monster, an, MISSILERANGE);
        assert_eq!(aim.target, players[0].mo);
    }

    #[test]
    fn line_attack_damages_the_player() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        let slope;
        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            let aim = aim_line_attack(&mut ctx, monster, Angle::ZERO, MISSILERANGE);
            slope = aim.slope;
        }

        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            line_attack(&mut ctx, monster, Angle::ZERO, MISSILERANGE, slope, 9);
        }

        let player_hit = events
            .iter()
            .any(|e| matches!(e, GameEvent::Damage { target, damage, .. } if Some(*target) == players[0].mo && *damage == 9));
        assert!(player_hit, "expected a 9-damage event for the player");
    }

    #[test]
    fn line_attack_hits_the_wall_and_spawns_puffs() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut no_players = Vec::new();
        // no player in the east, just a wall
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 240, 64);
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        let mut ctx = test_context(
            &map,
            &mut store,
            &mut no_players,
            &mut rng,
            &mut world,
            &mut events,
        );
        line_attack(&mut ctx, monster, Angle::ZERO, MISSILERANGE, Fixed::ZERO, 5);

        let puffs = store
            .thinkers()
            .filter(|(_id, t)| match t {
                crate::thinker::Thinker::Mobj(m) => m.mobj_type == MobjType::MT_PUFF,
                _ => false,
            })
            .count();
        assert_eq!(puffs, 1);
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, GameEvent::Damage { .. }))
        );
    }

    #[test]
    fn blood_puffs_spawn_for_shootable_things() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let victim = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 192, 64);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            line_attack(&mut ctx, monster, Angle::ZERO, MISSILERANGE, Fixed::ZERO, 9);
        }

        let blood = store
            .thinkers()
            .filter(|(_id, t)| match t {
                crate::thinker::Thinker::Mobj(m) => m.mobj_type == MobjType::MT_BLOOD,
                _ => false,
            })
            .count();
        assert_eq!(blood, 1);
        let victim_hit = events
            .iter()
            .any(|e| matches!(e, GameEvent::Damage { target, .. } if *target == victim));
        assert!(victim_hit);
    }

    #[test]
    fn puff_uses_melee_state_for_melee_range() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut no_players = Vec::new();
        let _monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        let melee_puff;
        let normal_puff;
        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut no_players,
                &mut rng,
                &mut world,
                &mut events,
            );
            {
                let mut parts = PuffParts::from_enemy(&mut ctx);
                melee_puff = spawn_puff(
                    &mut parts,
                    Fixed::from_int(64),
                    Fixed::from_int(64),
                    Fixed::ZERO,
                    MELEERANGE,
                );
            }
            {
                let mut parts = PuffParts::from_enemy(&mut ctx);
                normal_puff = spawn_puff(
                    &mut parts,
                    Fixed::from_int(64),
                    Fixed::from_int(64),
                    Fixed::ZERO,
                    MISSILERANGE,
                );
            }
        }

        assert_eq!(
            store.mobj(melee_puff).map(|m| m.state),
            Some(StateId::S_PUFF3)
        );
        assert_eq!(
            store.mobj(normal_puff).map(|m| m.state),
            Some(StateId::S_PUFF1)
        );
    }
}
