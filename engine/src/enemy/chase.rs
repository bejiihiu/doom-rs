//! The chase brain: `P_Move`, `P_TryWalk`, `P_NewChaseDir`, the melee and
//! missile range checks and `P_LookForPlayers`, ported from p_enemy.c.
//!
//! `P_Move` itself is here (it is part of the AI, not of generic movement),
//! built on the collision layer ([`crate::movement::try_move`]). Special
//! lines the mover bumps are forwarded as [`GameEvent::UseSpecialLine`]
//! instead of being resolved inline — `p_spec.c` is not ported yet, so the
//! specials layer (gameplay) decides what they do. Until it lands, every
//! bumped special line counts as usable, which matches the common case
//! (unlocked doors open).

use crate::fixed::{FRACBITS, FRACUNIT, Fixed};
use crate::info::{MobjType, StateId};
use crate::math::aprox_distance;
use crate::tables::Angle;
use crate::thinker::ThinkerId;

use super::{EnemyContext, GameEvent, MELEERANGE};

/// Vanilla `dirtype_t` (p_enemy.c).
pub const DI_EAST: i32 = 0;
/// See [`DI_EAST`].
pub const DI_NORTHEAST: i32 = 1;
/// See [`DI_EAST`].
pub const DI_NORTH: i32 = 2;
/// See [`DI_EAST`].
pub const DI_NORTHWEST: i32 = 3;
/// See [`DI_EAST`].
pub const DI_WEST: i32 = 4;
/// See [`DI_EAST`].
pub const DI_SOUTHWEST: i32 = 5;
/// See [`DI_EAST`].
pub const DI_SOUTH: i32 = 6;
/// See [`DI_EAST`].
pub const DI_SOUTHEAST: i32 = 7;
/// Vanilla `DI_NODIR` — no direction is set.
pub const DI_NODIR: i32 = 8;
/// Vanilla `NUMDIRS`.
pub const NUMDIRS: i32 = 9;

/// Vanilla `opposite[]` (turn-around for every direction, `DI_NODIR` last).
pub const OPPOSITE: [i32; 9] = [
    DI_WEST,
    DI_SOUTHWEST,
    DI_SOUTH,
    DI_SOUTHEAST,
    DI_EAST,
    DI_NORTHEAST,
    DI_NORTH,
    DI_NORTHWEST,
    DI_NODIR,
];

/// Vanilla `diags[]` — the two-bit diagonal pick for the "direct route"
/// (`(deltay < 0) << 1` plus `(deltax > 0)`).
pub const DIAGS: [i32; 4] = [DI_NORTHWEST, DI_NORTHEAST, DI_SOUTHWEST, DI_SOUTHEAST];

/// Vanilla `xspeed[]` (p_enemy.c), raw fixed-point.
pub const XSPEED: [i32; 8] = [FRACUNIT, 47000, 0, -47000, -FRACUNIT, -47000, 0, 47000];
/// Vanilla `yspeed[]`, raw fixed-point.
pub const YSPEED: [i32; 8] = [0, 47000, FRACUNIT, 47000, 0, -47000, -FRACUNIT, -47000];

/// Vanilla `P_CheckMeleeRange`.
pub fn check_melee_range(ctx: &mut EnemyContext<'_>, actor: ThinkerId) -> bool {
    let Some(a) = ctx.store.mobj(actor) else {
        return false;
    };
    let Some(target_id) = a.target else {
        return false;
    };
    let Some(target) = ctx.store.mobj(target_id) else {
        return false;
    };

    let dist = aprox_distance(target.x - a.x, target.y - a.y);
    let pl_radius = target.info().radius;

    if dist >= MELEERANGE - Fixed::from_raw(20 * FRACUNIT) + pl_radius {
        return false;
    }

    super::sight::check_sight(ctx.map, a, target)
}

/// Vanilla `P_CheckMissileRange`.
pub fn check_missile_range(ctx: &mut EnemyContext<'_>, actor: ThinkerId) -> bool {
    let Some(a) = ctx.store.mobj(actor) else {
        return false;
    };
    let Some(target_id) = a.target else {
        return false;
    };
    let Some(target) = ctx.store.mobj(target_id) else {
        return false;
    };

    if !super::sight::check_sight(ctx.map, a, target) {
        return false;
    }

    if a.has_flag(crate::info::MF_JUSTHIT) {
        // the target just hit the enemy, so fight back!
        if let Some(m) = ctx.store.mobj_mut(actor) {
            m.flags &= !crate::info::MF_JUSTHIT;
        }
        return true;
    }

    if a.reactiontime != 0 {
        return false; // do not attack yet
    }

    // OPTIMIZE: get this from a global checksight (sic)
    let mut dist = aprox_distance(a.x - target.x, a.y - target.y).to_raw();
    dist -= 64 * FRACUNIT;

    if a.info().meleestate == StateId::S_NULL {
        dist -= 128 * FRACUNIT; // no melee attack, so fire more
    }

    dist >>= FRACBITS; // vanilla `dist >>= 16`

    if a.mobj_type == MobjType::MT_VILE && dist > 14 * 64 {
        return false; // too far away
    }

    if a.mobj_type == MobjType::MT_UNDEAD {
        if dist < 196 {
            return false; // close for fist attack
        }
        dist >>= 1;
    }

    if a.mobj_type == MobjType::MT_CYBORG
        || a.mobj_type == MobjType::MT_SPIDER
        || a.mobj_type == MobjType::MT_SKULL
    {
        dist >>= 1;
    }

    if dist > 200 {
        dist = 200;
    }

    if a.mobj_type == MobjType::MT_CYBORG && dist > 160 {
        dist = 160;
    }

    if i32::from(ctx.rng.next()) < dist {
        return false;
    }

    true
}

/// Vanilla `P_Move`. Moves the actor one speed-step in its current
/// `movedir`; `false` when blocked.
///
/// Floating actors adjust height instead of falling back; actors with
/// `MF_FLOAT` that can't fit are nudged toward floor/ceiling instead of
/// walking. Special lines the bump produces a [`GameEvent::UseSpecialLine`]
/// each and count as usable (see module docs).
pub fn move_actor(ctx: &mut EnemyContext<'_>, actor: ThinkerId) -> bool {
    let Some(a) = ctx.store.mobj(actor) else {
        return false;
    };

    if a.movedir == DI_NODIR {
        return false;
    }

    if a.movedir < 0 || a.movedir >= 8 {
        // vanilla I_Error'd out; a corrupt movedir shouldn't panic the
        // whole tic loop
        tracing::error!("P_Move: weird actor->movedir! ({})", a.movedir);
        return false;
    }

    let movedir = a.movedir as usize;
    let speed = a.info().speed;
    let tryx = a.x + Fixed::from_raw(speed.wrapping_mul(XSPEED[movedir]));
    let tryy = a.y + Fixed::from_raw(speed.wrapping_mul(YSPEED[movedir]));
    let actor_flags = a.flags;
    let actor_z = a.z;

    let mut mctx = ctx.move_context();
    let report = crate::movement::try_move(&mut mctx, actor, tryx, tryy);

    // forward movement events (missile contact damage, pickups, line crossings)
    ctx.events
        .extend(report.events.into_iter().map(GameEvent::Move));

    if !report.ok {
        // open any specials
        if actor_flags & crate::info::MF_FLOAT != 0 && report.floatok {
            // must adjust height
            let step = crate::movement::FLOATSPEED;
            if actor_z < report.floorz {
                if let Some(m) = ctx.store.mobj_mut(actor) {
                    m.z += step;
                }
            } else if let Some(m) = ctx.store.mobj_mut(actor) {
                m.z -= step;
            }
            if let Some(m) = ctx.store.mobj_mut(actor) {
                m.flags |= crate::info::MF_INFLOAT;
            }
            return true;
        }

        if report.specials.is_empty() {
            return false;
        }

        if let Some(m) = ctx.store.mobj_mut(actor) {
            m.movedir = DI_NODIR;
        }
        let mut good = false;
        // vanilla walks spechit in reverse with `while (numspechit--)`
        for line in report.specials.iter().rev() {
            ctx.events.push(GameEvent::UseSpecialLine {
                line: *line,
                thing: actor,
            });
            good = true; // see module docs: specials count as usable until p_spec lands
        }
        return good;
    }

    if let Some(m) = ctx.store.mobj_mut(actor) {
        m.flags &= !crate::info::MF_INFLOAT;
    }

    if actor_flags & crate::info::MF_FLOAT == 0 {
        if let Some(m) = ctx.store.mobj_mut(actor) {
            m.z = report.floorz;
        }
    }
    true
}

/// Vanilla `P_TryWalk`. Returns `true` and resets `movecount` on a
/// successful move, `false` otherwise (blocked, or the direction table has
/// nothing better to offer — the caller picks a new direction).
pub fn try_walk(ctx: &mut EnemyContext<'_>, actor: ThinkerId) -> bool {
    if !move_actor(ctx, actor) {
        return false;
    }

    let movecount = i32::from(ctx.rng.next() & 15);
    if let Some(m) = ctx.store.mobj_mut(actor) {
        m.movecount = movecount;
    }
    true
}

/// Vanilla `P_NewChaseDir`. Picks the next `movedir` for the actor towards
/// its target, in vanilla's priority order:
///
/// 1. the direct diagonal (if both axes are off by more than 10 units)
/// 2. `d[1]`, then `d[2]` (the axis with more influence first, possibly
///    swapped by the 1-in-256 `P_Random() > 200` roll / the `abs` test)
/// 3. the old direction if that still works
/// 4. a random sweep east→southeast or southeast→east (`P_Random() & 1`),
///    skipping the turnaround direction
/// 5. the turnaround direction
/// 6. `DI_NODIR` when nothing works
pub fn new_chase_dir(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let Some(target_id) = a.target else {
        // vanilla I_Error'd; the call sites only call this with a live target
        tracing::warn!("P_NewChaseDir: called with no target");
        return;
    };
    let Some(target) = ctx.store.mobj(target_id) else {
        tracing::warn!("P_NewChaseDir: stale target handle");
        return;
    };

    let olddir = a.movedir;
    let turnaround = OPPOSITE[olddir.clamp(0, 8) as usize];

    let deltax = target.x - a.x;
    let deltay = target.y - a.y;

    let d1 = if deltax.to_raw() > 10 * FRACUNIT {
        DI_EAST
    } else if deltax.to_raw() < -10 * FRACUNIT {
        DI_WEST
    } else {
        DI_NODIR
    };

    let d2 = if deltay.to_raw() < -10 * FRACUNIT {
        DI_SOUTH
    } else if deltay.to_raw() > 10 * FRACUNIT {
        DI_NORTH
    } else {
        DI_NODIR
    };

    // try direct route
    if d1 != DI_NODIR && d2 != DI_NODIR {
        let diag =
            DIAGS[(((deltay.to_raw() < 0) as usize) << 1) + ((deltax.to_raw() > 0) as usize)];
        if let Some(m) = ctx.store.mobj_mut(actor) {
            m.movedir = diag;
        }
        if diag != turnaround && try_walk(ctx, actor) {
            return;
        }
    }

    // try other directions
    let mut d1 = d1;
    let mut d2 = d2;
    if ctx.rng.next() > 200 || deltay.to_raw().unsigned_abs() > deltax.to_raw().unsigned_abs() {
        std::mem::swap(&mut d1, &mut d2);
    }

    if d1 == turnaround {
        d1 = DI_NODIR;
    }
    if d2 == turnaround {
        d2 = DI_NODIR;
    }

    if d1 != DI_NODIR {
        if let Some(m) = ctx.store.mobj_mut(actor) {
            m.movedir = d1;
        }
        if try_walk(ctx, actor) {
            // either moved forward or attacked
            return;
        }
    }

    if d2 != DI_NODIR {
        if let Some(m) = ctx.store.mobj_mut(actor) {
            m.movedir = d2;
        }
        if try_walk(ctx, actor) {
            return;
        }
    }

    // there is no direct path to the player,
    // so pick another direction.
    if olddir != DI_NODIR {
        if let Some(m) = ctx.store.mobj_mut(actor) {
            m.movedir = olddir;
        }
        if try_walk(ctx, actor) {
            return;
        }
    }

    // randomly determine direction of search
    if ctx.rng.next() & 1 != 0 {
        for tdir in DI_EAST..=DI_SOUTHEAST {
            if tdir != turnaround {
                if let Some(m) = ctx.store.mobj_mut(actor) {
                    m.movedir = tdir;
                }
                if try_walk(ctx, actor) {
                    return;
                }
            }
        }
    } else {
        for tdir in (DI_EAST..=DI_SOUTHEAST).rev() {
            if tdir != turnaround {
                if let Some(m) = ctx.store.mobj_mut(actor) {
                    m.movedir = tdir;
                }
                if try_walk(ctx, actor) {
                    return;
                }
            }
        }
    }

    if turnaround != DI_NODIR {
        if let Some(m) = ctx.store.mobj_mut(actor) {
            m.movedir = turnaround;
        }
        if try_walk(ctx, actor) {
            return;
        }
    }

    if let Some(m) = ctx.store.mobj_mut(actor) {
        m.movedir = DI_NODIR; // can not move
    }
}

/// Vanilla `P_LookForPlayers`. Scans the (up to) four player slots,
/// starting from the slot after the one the actor last looked at, in
/// order. `allaround` ignores the frontal-180-degrees restriction.
///
/// Returns `true` and sets `actor->target` when a live, visible,
/// in-arc player is found.
pub fn look_for_players(ctx: &mut EnemyContext<'_>, actor: ThinkerId, allaround: bool) -> bool {
    let Some((a_x, a_y, a_angle, a_lastlook)) = ctx
        .store
        .mobj(actor)
        .map(|a| (a.x, a.y, a.angle, a.lastlook))
    else {
        return false;
    };
    let mut lastlook = a_lastlook.clamp(0, 3);
    let stop = (lastlook - 1) & 3;
    let mut count = 0;

    loop {
        let index = lastlook as usize;
        let next = (lastlook + 1) & 3;

        if !ctx.player_in_game(index) {
            // vanilla `continue` hits the loop-bottom increment
            if let Some(m) = ctx.store.mobj_mut(actor) {
                m.lastlook = next;
            }
            lastlook = next;
            continue;
        }

        if count == 2 || index as i32 == stop {
            // done looking; vanilla returns with lastlook == current slot
            if let Some(m) = ctx.store.mobj_mut(actor) {
                m.lastlook = index as i32;
            }
            return false;
        }
        count += 1;

        let player = &ctx.players[index];

        if player.health <= 0 {
            // dead; continue -> bottom increment
            if let Some(m) = ctx.store.mobj_mut(actor) {
                m.lastlook = next;
            }
            lastlook = next;
            continue;
        }

        let Some(player_mo) = player.mo else {
            continue; // shouldn't happen (playeringame derived), but keep it safe
        };

        let Some(pm) = ctx.store.mobj(player_mo) else {
            if let Some(m) = ctx.store.mobj_mut(actor) {
                m.lastlook = next;
            }
            lastlook = next;
            continue;
        };

        let actor_ref = ctx.store.mobj(actor);
        let Some(a) = actor_ref else {
            // actor vanished mid-scan; nothing sensible left to target
            return false;
        };

        if !super::sight::check_sight(ctx.map, a, pm) {
            // out of sight; continue -> bottom increment
            if let Some(m) = ctx.store.mobj_mut(actor) {
                m.lastlook = next;
            }
            lastlook = next;
            continue;
        }

        if !allaround {
            let an = crate::bsp::point_to_angle_2(a_x, a_y, pm.x, pm.y) - a_angle;

            if an.to_raw() > Angle::ANG90.to_raw() && an.to_raw() < Angle::ANG270.to_raw() {
                let dist = aprox_distance(pm.x - a.x, pm.y - a.y);
                // if real close, react anyway
                if dist > MELEERANGE {
                    // behind back; continue -> bottom increment
                    if let Some(m) = ctx.store.mobj_mut(actor) {
                        m.lastlook = next;
                    }
                    lastlook = next;
                    continue;
                }
            }
        }

        if let Some(m) = ctx.store.mobj_mut(actor) {
            m.target = Some(player_mo);
            // vanilla returns with lastlook == the slot it found
            m.lastlook = index as i32;
        }
        return true;
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    use crate::enemy::testutil::{
        TestWorld, open_map, players, spawn_player, spawn_thing, split_map, test_context,
    };
    use crate::info::MobjType;
    use crate::map::types::ML_TWOSIDED;
    use crate::mobj::MobjStore;
    use crate::rng::DoomRng;

    fn set_mobj(store: &mut MobjStore, id: ThinkerId, f: impl FnOnce(&mut crate::mobj::Mobj)) {
        if let Some(m) = store.mobj_mut(id) {
            f(m);
        }
    }

    fn setup() -> (
        crate::map::Map,
        MobjStore,
        crate::thinker::ThinkerId,
        crate::thinker::ThinkerId,
        Vec<crate::player::Player>,
        DoomRng,
    ) {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);
        set_mobj(&mut store, monster, |m| m.lastlook = 0); // deterministic scan order
        (map, store, player, monster, players, rng)
    }

    #[test]
    fn look_for_players_sets_target_on_sight() {
        let (map, mut store, _player, monster, mut players, mut rng) = setup();
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
        assert!(look_for_players(&mut ctx, monster, false));

        let target = store.mobj(monster).and_then(|m| m.target);
        assert_eq!(target, players[0].mo);
    }

    #[test]
    fn look_for_players_ignores_dead_player() {
        let (map, mut store, _player, monster, mut players, mut rng) = setup();
        players[0].health = 0;
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
        assert!(!look_for_players(&mut ctx, monster, false));
        assert_eq!(store.mobj(monster).and_then(|m| m.target), None);
    }

    #[test]
    fn look_for_players_rejects_behind_the_back() {
        let (map, mut store, _player, monster, mut players, mut rng) = setup();
        // monster faces east by spawn; centre it in the map
        set_mobj(&mut store, monster, |m| {
            m.x = Fixed::from_int(128);
        });
        if let Some(p) = players[0].mo {
            set_mobj(&mut store, p, |m| {
                m.x = Fixed::from_int(0); // due west, 128 units away
                m.y = Fixed::from_int(64);
            });
        }

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
        // behind the back and not close enough to force a reaction -> not targeted
        assert!(!look_for_players(&mut ctx, monster, false));

        // ...but with allaround the monster does see the player
        assert!(look_for_players(&mut ctx, monster, true));
    }

    #[test]
    fn look_for_players_reacts_to_close_backstabbers() {
        let (map, mut store, _player, monster, mut players, mut rng) = setup();
        if let Some(p) = players[0].mo {
            set_mobj(&mut store, p, |m| {
                m.x = Fixed::from_int(48); // due west, inside melee range
                m.y = Fixed::from_int(64);
            });
        }

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
        // behind the back but inside melee range, so the monster reacts anyway
        assert!(look_for_players(&mut ctx, monster, false));
    }

    #[test]
    fn check_melee_range_true_when_close_and_visible() {
        let (map, mut store, player, monster, mut players, mut rng) = setup();
        // shove the player right next to the monster
        if let Some(p) = players[0].mo {
            set_mobj(&mut store, p, |m| {
                m.x = Fixed::from_int(96);
                m.y = Fixed::from_int(64);
            });
        }

        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);
        set_mobj(&mut store, monster, |m| m.target = Some(player));

        let mut ctx = test_context(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut events,
        );
        assert!(check_melee_range(&mut ctx, monster));
    }

    #[test]
    fn check_melee_range_false_when_far() {
        let (map, mut store, player, monster, mut players, mut rng) = setup();
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);
        set_mobj(&mut store, monster, |m| m.target = Some(player));

        let mut ctx = test_context(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut events,
        );
        assert!(!check_melee_range(&mut ctx, monster));
    }

    #[test]
    fn check_melee_range_false_without_target() {
        let (map, mut store, _player, monster, mut players, mut rng) = setup();
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
        assert!(!check_melee_range(&mut ctx, monster));
    }

    #[test]
    fn try_walk_succeeds_on_open_ground() {
        let (map, mut store, _player, monster, mut players, mut rng) = setup();
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        set_mobj(&mut store, monster, |m| m.movedir = DI_EAST);
        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            assert!(try_walk(&mut ctx, monster));
        }

        let x = store.mobj(monster).map(|m| m.x).unwrap_or_default();
        assert!(x > Fixed::from_int(64));
        // movecount was reset like vanilla P_TryWalk does
        assert!(store.mobj(monster).map(|m| m.movecount).unwrap_or(-1) >= 0);
    }

    #[test]
    fn try_walk_fails_into_a_wall() {
        let (map, mut store, _player, monster, mut players, mut rng) = setup();
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        set_mobj(&mut store, monster, |m| {
            m.movedir = DI_WEST;
            m.x = Fixed::from_int(8);
        });
        let mut ctx = test_context(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut events,
        );
        assert!(!try_walk(&mut ctx, monster));
    }

    #[test]
    fn new_chase_dir_heads_towards_target() {
        let (map, mut store, player, monster, mut players, mut rng) = setup();
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        set_mobj(&mut store, monster, |m| {
            m.target = Some(player);
            m.movedir = DI_NORTHWEST; // force a re-pick
        });
        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            new_chase_dir(&mut ctx, monster);
        }

        assert_eq!(store.mobj(monster).map(|m| m.movedir), Some(DI_EAST));
    }

    #[test]
    fn check_missile_range_false_without_los() {
        let map = split_map(0, 0, ML_TWOSIDED, 0x00); // closed divider, target in the north room
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 192);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        set_mobj(&mut store, monster, |m| m.target = Some(player));
        let mut ctx = test_context(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut events,
        );
        assert!(!check_missile_range(&mut ctx, monster));
    }

    #[test]
    fn check_missile_range_true_when_just_hit() {
        let (map, mut store, player, monster, mut players, mut rng) = setup();
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        set_mobj(&mut store, monster, |m| {
            m.target = Some(player);
            m.flags |= crate::info::MF_JUSTHIT;
        });
        let mut ctx = test_context(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut events,
        );
        assert!(check_missile_range(&mut ctx, monster));
        // the flag is consumed by the fire decision
        assert_eq!(
            store
                .mobj(monster)
                .map(|m| m.flags & crate::info::MF_JUSTHIT),
            Some(0)
        );
    }

    #[test]
    fn check_missile_range_false_when_still_waking_up() {
        let (map, mut store, player, monster, mut players, mut rng) = setup();
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        set_mobj(&mut store, monster, |m| {
            m.target = Some(player);
            m.reactiontime = 8;
        });
        let mut ctx = test_context(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut events,
        );
        assert!(!check_missile_range(&mut ctx, monster));
    }

    #[test]
    fn check_missile_range_false_for_distant_undead() {
        let map = split_map(0, 128, ML_TWOSIDED, 0x00);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 224, 64);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_UNDEAD, 32, 64);
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        set_mobj(&mut store, monster, |m| m.target = Some(player));
        let mut ctx = test_context(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut events,
        );
        // ~192 units away, inside the fist-range refusal
        assert!(!check_missile_range(&mut ctx, monster));
    }

    #[test]
    fn move_actor_blocked_by_closed_divider() {
        let map = split_map(0, 0, ML_TWOSIDED, 0x00); // fully closed divider
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_PAIN, 64, 120);
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        set_mobj(&mut store, monster, |m| m.movedir = DI_NORTH);
        let mut ctx = test_context(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut events,
        );
        // a pain elemental can't fit through a zero-height gap either
        assert!(!move_actor(&mut ctx, monster));
        assert_eq!(
            store.mobj(monster).map(|m| m.z),
            Some(crate::fixed::Fixed::ZERO)
        );
        assert_eq!(
            store
                .mobj(monster)
                .map(|m| m.flags & crate::info::MF_INFLOAT),
            Some(0)
        );
    }
}
