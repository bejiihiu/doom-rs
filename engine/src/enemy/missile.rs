//! Missile spawning: `P_SpawnMissile` and `P_CheckMissileSpawn`, ported
//! from p_mobj.c.

use crate::fixed::{FRACUNIT, Fixed};
use crate::info::{MF_SHADOW, MOBJ_INFO, MobjType};
use crate::math::aprox_distance;
use crate::mobj::{SpawnSpec, SpawnZ};
use crate::tables::ANGLE_TO_FINE_SHIFT;
use crate::thinker::ThinkerId;

use super::{EnemyContext, GameEvent};

/// Vanilla `P_SpawnMissile`.
///
/// Creates `type` at `source`'s eye height, aims it at `dest`
/// (ground-to-ground homing is left to the tracer states), fires the
/// missile's seesound if it has one, and finally moves it forward a touch
/// so an immediately-exploding missile still lands inside its first cell.
/// Returns the spawned missile.
pub fn spawn_missile(
    ctx: &mut EnemyContext<'_>,
    source: ThinkerId,
    dest: ThinkerId,
    mobj_type: MobjType,
) -> Option<ThinkerId> {
    let Some(s) = ctx.store.mobj(source) else {
        tracing::warn!("P_SpawnMissile: stale source handle");
        return None;
    };
    let sx = s.x;
    let sy = s.y;
    let sz = s.z;
    let seesound = MOBJ_INFO[mobj_type as usize].seesound;
    let speed = Fixed::from_raw(MOBJ_INFO[mobj_type as usize].speed);

    let z = sz + Fixed::from_raw(4 * 8 * FRACUNIT);

    let missile = ctx.store.spawn_mobj(
        ctx.map,
        ctx.rng,
        SpawnSpec {
            skill: ctx.rules.skill,
            x: sx,
            y: sy,
            z: SpawnZ::At(z),
            mobj_type,
        },
    );

    if seesound != crate::info::SfxId::sfx_None {
        ctx.events.push(GameEvent::Sound {
            actor: Some(missile),
            sfx: seesound,
        });
    }

    // where it came from
    if let Some(m) = ctx.store.mobj_mut(missile) {
        m.target = Some(source);
    }

    let Some(d) = ctx.store.mobj(dest) else {
        tracing::warn!("P_SpawnMissile: stale destination handle");
        // keep the missile, it just has no aim — matches the spirit of the
        // original failing later rather than here
        return Some(missile);
    };

    // copy what we need out of the destination
    let (d_x, d_y, d_z, d_shadow) = (d.x, d.y, d.z, d.has_flag(MF_SHADOW));

    let mut an = crate::bsp::point_to_angle_2(sx, sy, d_x, d_y);

    // fuzzy player
    if d_shadow {
        let fuzz = (ctx.rng.next() as i32 - ctx.rng.next() as i32) << 20;
        an = crate::tables::Angle::from_raw(an.to_raw().wrapping_add(fuzz as u32));
    }

    let fine = (an.to_raw() >> ANGLE_TO_FINE_SHIFT) as usize;

    if let Some(m) = ctx.store.mobj_mut(missile) {
        m.angle = an;
        m.momx = speed * crate::tables::FINE_COSINE[fine];
        m.momy = speed * crate::tables::FINE_SINE[fine];
    }

    let mut dist = aprox_distance(d_x - sx, d_y - sy).to_raw();
    let missile_speed = MOBJ_INFO[mobj_type as usize].speed;
    if missile_speed == 0 {
        tracing::warn!("P_SpawnMissile: zero-speed missile type");
        return Some(missile);
    }
    dist /= missile_speed;

    if dist < 1 {
        dist = 1;
    }

    if let Some(m) = ctx.store.mobj_mut(missile) {
        m.momz = Fixed::from_raw((d_z - sz).to_raw() / dist);
    }

    check_missile_spawn(ctx, missile);

    Some(missile)
}

/// Vanilla `P_CheckMissileSpawn`.
///
/// Trims the missile's `tics` a little (it spawns already "mid-flight"),
/// pushes it forward by half a momentum step so an immediately-exploding
/// missile still tests the cell it spawned in, and explodes it right there
/// when that nudge is blocked.
pub fn check_missile_spawn(ctx: &mut EnemyContext<'_>, th: ThinkerId) {
    let adj = i32::from(ctx.rng.next() & 3);

    let (x, y, momx, momy, momz) = match ctx.store.mobj(th) {
        Some(m) => (m.x, m.y, m.momx, m.momy, m.momz),
        None => {
            tracing::warn!("P_CheckMissileSpawn: stale missile handle");
            return;
        }
    };

    if let Some(m) = ctx.store.mobj_mut(th) {
        m.tics -= adj;
        if m.tics < 1 {
            m.tics = 1;
        }
        // move a little forward so an angle can be computed if it
        // immediately explodes
        m.x += Fixed::from_raw(momx.to_raw() >> 1);
        m.y += Fixed::from_raw(momy.to_raw() >> 1);
        m.z += Fixed::from_raw(momz.to_raw() >> 1);
    }

    let nx = x + Fixed::from_raw(momx.to_raw() >> 1);
    let ny = y + Fixed::from_raw(momy.to_raw() >> 1);

    let mut mctx = ctx.move_context();
    let report = crate::movement::try_move(&mut mctx, th, nx, ny);

    ctx.events
        .extend(report.events.into_iter().map(GameEvent::Move));

    if !report.ok {
        explode_missile_dispatch(ctx, th);
    }
}

/// Vanilla `P_ExplodeMissile` with the action dispatch routed through the
/// gameplay layer's dispatcher.
///
/// The movement layer's `explode_missile` sets the death state but
/// discards the state's actions (it has no dispatcher to hand them to);
/// a missile whose whole damaging payload sits on the *first* state of
/// the death chain (MT_ROCKET's `S_EXPLODE1` carries `A_Explode`) would
/// otherwise never splash. This path applies the state and runs the
/// actions, exactly like vanilla's inline call.
pub fn explode_missile_dispatch(ctx: &mut EnemyContext<'_>, th: ThinkerId) {
    let Some(mo) = ctx.store.mobj(th) else {
        return;
    };
    let deathstate = mo.info().deathstate;
    let deathsound = mo.info().deathsound;

    // zero the movement
    if let Some(m) = ctx.store.mobj_mut(th) {
        m.momx = Fixed::ZERO;
        m.momy = Fixed::ZERO;
        m.momz = Fixed::ZERO;
    }

    // switch to the death state, then run the actions it carries
    super::action::set_mobj_state_and_run_actions(ctx, th, deathstate);

    // vanilla always consumes this P_Random call, even if the state chain
    // removed the mobj; keep the RNG stream aligned
    let jitter = i32::from(ctx.rng.next() & 3);

    if let Some(m) = ctx.store.mobj_mut(th) {
        m.tics -= jitter;
        if m.tics < 1 {
            m.tics = 1;
        }
        m.flags &= !crate::info::MF_MISSILE;
    }

    if deathsound != crate::info::SfxId::sfx_None {
        ctx.events.push(GameEvent::Sound {
            actor: Some(th),
            sfx: deathsound,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::enemy::testutil::{
        TestWorld, open_map, players, spawn_player, spawn_thing, test_context,
    };
    use crate::fixed::Fixed;
    use crate::info::MobjType;
    use crate::mobj::MobjStore;
    use crate::rng::DoomRng;

    #[test]
    fn spawned_missile_flies_at_the_target() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        let missile;
        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            missile = spawn_missile(&mut ctx, monster, player, MobjType::MT_TROOPSHOT)
                .expect("missile spawned");
        }

        let (momx, momy, angle, target) = store
            .mobj(missile)
            .map(|m| (m.momx, m.momy, m.angle, m.target))
            .unwrap_or_default();
        assert!(momx > Fixed::ZERO, "fireball flies east");
        assert!(momy.to_raw().abs() < momx.to_raw());
        assert_eq!(angle, crate::tables::Angle::ZERO);
        assert_eq!(target, Some(monster));

        // the imp's fireball has a seesound and it went out
        let heard = events
            .iter()
            .any(|e| matches!(e, GameEvent::Sound { actor: Some(a), sfx: crate::info::SfxId::sfx_firsht } if *a == missile));
        assert!(heard);
    }

    #[test]
    fn missile_spawn_does_not_move_into_walls() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 240, 64);
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        // a fireball spawned inside the east wall tries to leave the world;
        // check_missile_spawn still advances it half a step and must not crash
        let missile;
        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            missile = spawn_missile(&mut ctx, monster, player, MobjType::MT_TROOPSHOT)
                .expect("missile spawned");
        }
        let tics = store.mobj(missile).map(|m| m.tics).unwrap_or(-1);
        assert!(tics >= 1);
    }

    #[test]
    fn rocket_spawned_into_a_wall_splashes_on_impact() {
        // the rocket materializes inside the east wall and nudges itself
        // into it; check_missile_spawn's try fails and the death state
        // chain must splash it right there
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 240, 64);
        let victim = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 246, 64);
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        let missile;
        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            missile = spawn_missile(&mut ctx, monster, victim, MobjType::MT_ROCKET)
                .expect("missile spawned");
        }

        // the blocked spawn detonates: death state reached and the splash
        // landed on the victim next to the wall
        assert_eq!(
            store.mobj(missile).map(|m| m.state),
            Some(crate::info::StateId::S_EXPLODE1)
        );
        let splashed = events
            .iter()
            .any(|e| matches!(e, GameEvent::Damage { target, .. } if *target == victim));
        assert!(splashed, "expected splash damage to the victim");
    }

    #[test]
    fn missile_tic_countdown_never_hits_zero() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        let missile;
        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            missile = spawn_missile(&mut ctx, monster, player, MobjType::MT_TROOPSHOT)
                .expect("missile spawned");
        }
        let tics = store.mobj(missile).map(|m| m.tics).unwrap_or(-1);
        assert!(
            tics >= 1,
            "missile must keep at least one tic after the spawn-time trim"
        );
    }
}
