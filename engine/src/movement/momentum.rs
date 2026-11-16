//! Momentum movement, ported from `p_mobj.c`: `P_XYMovement`, `P_ZMovement`
//! and `P_ExplodeMissile`.
//!
//! x/y movement is substepped the way vanilla does it (half the remaining
//! move per substep while either axis exceeds `MAXMOVE/2`), blocked moves
//! defer to [`slide_move`] for players, explode missiles, or kill the
//! momentum for everything else. Friction applies on the ground, never for
//! missiles or skullflying things, and never in the air.
//!
//! z movement applies gravity, floats towards `MF_FLOAT` targets, clips to
//! the floor/ceiling, and reports vanilla's `S_StartSound(mo, sfx_oof)` as
//! an event instead of playing it. Landing deals no damage, like in the
//! original engine.

use crate::fixed::{FRACUNIT, Fixed};
use crate::info::{
    MF_CORPSE, MF_FLOAT, MF_INFLOAT, MF_MISSILE, MF_NOCLIP, MF_NOGRAVITY, MF_SKULLFLY, SfxId,
    StateId,
};
use crate::math::aprox_distance;
use crate::player::{CF_NOMOMENTUM, VIEWHEIGHT};
use crate::thinker::ThinkerId;

use super::MoveContext;
use super::MoveEvent;
use super::MovementReport;
use super::check::try_move;
use super::slide::slide_move;
use super::{FLOATSPEED, FRICTION, GRAVITY, MAXMOVE, STOPSPEED};

/// Vanilla `MAXMOVE/2`: the substep threshold.
const HALF_MOVE: Fixed = Fixed::from_raw(MAXMOVE.to_raw() >> 1);
/// Vanilla `FRACUNIT/4`: the corpse momentum threshold.
const FRACUNIT_QUARTER: Fixed = Fixed::from_raw(FRACUNIT >> 2);
/// Vanilla `GRAVITY*2`: the initial fall impulse.
const GRAVITY_2: Fixed = Fixed::from_raw(GRAVITY.to_raw() * 2);
/// Vanilla `GRAVITY*8`: the "hard landing" momentum threshold.
const GRAVITY_8: Fixed = Fixed::from_raw(GRAVITY.to_raw() * 8);

/// Vanilla `P_ExplodeMissile`: zero the momentum, switch to the death
/// state, jitter the tics down, clear `MF_MISSILE`, and report the death
/// sound. Public because missile logic elsewhere needs it.
pub fn explode_missile(ctx: &mut MoveContext, id: ThinkerId) -> MovementReport {
    let mut report = MovementReport::new();

    let Some(mo) = ctx.store.mobj(id) else {
        return report;
    };
    let deathstate = mo.info().deathstate;
    let deathsound = mo.info().deathsound;

    // zero the movement
    if let Some(mo) = ctx.store.mobj_mut(id) {
        mo.momx = Fixed::ZERO;
        mo.momy = Fixed::ZERO;
        mo.momz = Fixed::ZERO;
    }

    // switch to the death state
    let _ = ctx.store.set_mobj_state(id, deathstate, ctx.leveltime);

    // vanilla always consumes this P_Random call, even if the state chain
    // removed the mobj; keep the RNG stream aligned
    let jitter = ctx.rng.next() & 3;

    if let Some(mo) = ctx.store.mobj_mut(id) {
        mo.tics -= i32::from(jitter);
        if mo.tics < 1 {
            mo.tics = 1;
        }
        mo.flags &= !MF_MISSILE;
    }

    if deathsound != SfxId::sfx_None {
        report.events.push(MoveEvent::Sound {
            actor: id,
            sfx: deathsound,
        });
    }

    report
}

/// Vanilla `P_XYMovement`.
pub fn xy_movement(ctx: &mut MoveContext, id: ThinkerId) -> MovementReport {
    let mut report = MovementReport::new();

    // copy what we need; the borrow ends before any mutation
    let (momx0, momy0, flags0, spawnstate, player_index) = {
        let Some(mo) = ctx.store.mobj(id) else {
            return report;
        };
        (mo.momx, mo.momy, mo.flags, mo.info().spawnstate, mo.player)
    };

    if momx0.is_zero() && momy0.is_zero() {
        // the skull slammed into something?
        if flags0 & MF_SKULLFLY != 0 {
            if let Some(mo) = ctx.store.mobj_mut(id) {
                mo.flags &= !MF_SKULLFLY;
                mo.momx = Fixed::ZERO;
                mo.momy = Fixed::ZERO;
                mo.momz = Fixed::ZERO;
            }
            let _ = ctx.store.set_mobj_state(id, spawnstate, ctx.leveltime);
        }
        return report;
    }

    // clamp to MAXMOVE per axis (vanilla did this before reading mo->momx/y)
    let momx = momx0.clamp(-MAXMOVE, MAXMOVE);
    let momy = momy0.clamp(-MAXMOVE, MAXMOVE);
    if let Some(mo) = ctx.store.mobj_mut(id) {
        mo.momx = momx;
        mo.momy = momy;
    }
    let no_momentum = player_index
        .and_then(|index| ctx.players.get(index))
        .is_some_and(|player| player.cheats & CF_NOMOMENTUM != 0);

    let mut xmove = momx;
    let mut ymove = momy;

    loop {
        let Some(mo) = ctx.store.mobj(id) else {
            return report;
        };

        // substep like vanilla: half the remaining move per pass while
        // either axis is above MAXMOVE/2
        let (ptryx, ptryy) = if xmove > HALF_MOVE || ymove > HALF_MOVE {
            // xmove/2 truncates, xmove>>1 floors — both as in the original
            let half_x = xmove / Fixed::from_raw(2 * FRACUNIT);
            let half_y = ymove / Fixed::from_raw(2 * FRACUNIT);
            xmove = Fixed::from_raw(xmove.to_raw() >> 1);
            ymove = Fixed::from_raw(ymove.to_raw() >> 1);
            (mo.x + half_x, mo.y + half_y)
        } else {
            let ptryx = mo.x + xmove;
            let ptryy = mo.y + ymove;
            xmove = Fixed::ZERO;
            ymove = Fixed::ZERO;
            (ptryx, ptryy)
        };

        let try_report = try_move(ctx, id, ptryx, ptryy);
        // blocked or successful, whatever the check consumed is emitted
        report.extend(try_report.events);

        if !try_report.ok {
            // blocked move
            let Some(mo) = ctx.store.mobj(id) else {
                return report;
            };
            let flags = mo.flags;
            if mo.player.is_some() {
                // try to slide along it
                let slide = slide_move(ctx, id);
                report.extend(slide.events);
            } else if flags & MF_MISSILE != 0 {
                // explode a missile; don't explode against the sky
                let against_sky = try_report
                    .ceiling_line
                    .and_then(|line| ctx.map.linedefs.get(line))
                    .and_then(|line| line.backsector)
                    .map(|back| ctx.map.sectors[back].ceiling_pic == ctx.sky_flat)
                    .unwrap_or(false);
                if against_sky {
                    // missile hit a sky ceiling: remove it quietly, like
                    // the original
                    let _ = ctx.store.remove_mobj(id, ctx.leveltime);
                    return report;
                }
                let explode = explode_missile(ctx, id);
                report.extend(explode.events);
            } else {
                // stop the momentum dead
                if let Some(mo) = ctx.store.mobj_mut(id) {
                    mo.momx = Fixed::ZERO;
                    mo.momy = Fixed::ZERO;
                }
            }
        }

        if xmove.is_zero() && ymove.is_zero() {
            break;
        }
    }

    let Some(mo) = ctx.store.mobj(id) else {
        return report;
    };

    // debug cheat for no sliding at all
    if no_momentum {
        if let Some(mo) = ctx.store.mobj_mut(id) {
            mo.momx = Fixed::ZERO;
            mo.momy = Fixed::ZERO;
        }
        return report;
    }

    // no friction for missiles or skull flying things
    if mo.flags & (MF_MISSILE | MF_SKULLFLY) != 0 {
        return report;
    }

    // no friction when airborne
    if mo.z > mo.floorz {
        return report;
    }

    // do not stop sliding a corpse halfway off a step with some momentum
    if mo.flags & MF_CORPSE != 0
        && (mo.momx > FRACUNIT_QUARTER
            || mo.momx < -FRACUNIT_QUARTER
            || mo.momy > FRACUNIT_QUARTER
            || mo.momy < -FRACUNIT_QUARTER)
    {
        let on_own_floor = ctx
            .map
            .subsectors
            .get(mo.subsector)
            .and_then(|ss| ss.sector)
            .map(|sector| mo.floorz == ctx.map.sectors[sector].floor_height);
        if on_own_floor != Some(true) {
            // not standing on the current subsector's floor: keep sliding
            return report;
        }
    }

    let player = player_index.and_then(|index| ctx.players.get(index));
    let cmd_idle = player
        .map(|p| p.cmd.forward_move == 0 && p.cmd.side_move == 0)
        .unwrap_or(true);

    if mo.momx > -STOPSPEED
        && mo.momx < STOPSPEED
        && mo.momy > -STOPSPEED
        && mo.momy < STOPSPEED
        && cmd_idle
    {
        if player.is_some() && (mo.state as usize).wrapping_sub(StateId::S_PLAY_RUN1 as usize) < 4 {
            // in a walking frame: back to the standing frame
            let _ = ctx.store.set_mobj_state(id, StateId::S_PLAY, ctx.leveltime);
        }
        if let Some(mo) = ctx.store.mobj_mut(id) {
            mo.momx = Fixed::ZERO;
            mo.momy = Fixed::ZERO;
        }
    } else if let Some(mo) = ctx.store.mobj_mut(id) {
        mo.momx *= FRICTION;
        mo.momy *= FRICTION;
    }

    report
}

/// Vanilla `P_ZMovement`.
pub fn z_movement(ctx: &mut MoveContext, id: ThinkerId) -> MovementReport {
    let mut report = MovementReport::new();

    let Some(mo) = ctx.store.mobj(id) else {
        return report;
    };
    let player_index = mo.player;
    let is_float = mo.has_flag(MF_FLOAT);
    let has_target = mo.target.is_some();
    let is_skullfly = mo.has_flag(MF_SKULLFLY);
    let is_infloat = mo.has_flag(MF_INFLOAT);

    // check for smooth step up: the player camera follows the floor
    if let Some(index) = player_index {
        let (mo_z, mo_floorz) = (mo.z, mo.floorz);
        if mo_z < mo_floorz {
            if let Some(player) = ctx.players.get_mut(index) {
                player.viewheight -= mo_floorz - mo_z;
                player.deltaviewheight =
                    Fixed::from_raw((VIEWHEIGHT.to_raw() - player.viewheight.to_raw()) >> 3);
            }
        }
    }

    // adjust height
    let momz = mo.momz;
    if let Some(mo) = ctx.store.mobj_mut(id) {
        mo.z += momz;
    }

    // float down/up towards target if too close
    if is_float && has_target && !is_skullfly && !is_infloat {
        let target_id = ctx.store.mobj(id).and_then(|mo| mo.target);
        let target = target_id
            .and_then(|t| ctx.store.mobj(t))
            .map(|t| (t.x, t.y, t.z));
        if let Some((tx, ty, tz)) = target {
            let Some(mo) = ctx.store.mobj(id) else {
                return report;
            };
            let dist = aprox_distance(mo.x - tx, mo.y - ty);
            let delta = (tz + Fixed::from_raw(mo.height.to_raw() >> 1)) - mo.z;
            let three = Fixed::from_int(3);

            if delta < Fixed::ZERO && dist < (-delta) * three {
                if let Some(mo) = ctx.store.mobj_mut(id) {
                    mo.z -= FLOATSPEED;
                }
            } else if delta > Fixed::ZERO && dist < delta * three {
                if let Some(mo) = ctx.store.mobj_mut(id) {
                    mo.z += FLOATSPEED;
                }
            }
        }
    }

    // gather the clip-relevant snapshot; the borrow ends before the
    // mutation phase
    let (z0, floorz, flags, momz0) = {
        let Some(mo) = ctx.store.mobj(id) else {
            return report;
        };
        (mo.z, mo.floorz, mo.flags, mo.momz)
    };

    // clip movement
    if z0 <= floorz {
        let mut momz = momz0;
        if flags & MF_SKULLFLY != 0 {
            // the skull slammed into something
            momz = -momz;
            if let Some(mo) = ctx.store.mobj_mut(id) {
                mo.momz = momz;
            }
        }

        if momz < Fixed::ZERO {
            if let Some(index) = player_index {
                if momz < -GRAVITY_8 {
                    // Squat down: decrease viewheight for a moment after
                    // hitting the ground (hard), and play the oof sound.
                    if let Some(player) = ctx.players.get_mut(index) {
                        player.deltaviewheight = Fixed::from_raw(momz.to_raw() >> 3);
                    }
                    report.events.push(MoveEvent::Sound {
                        actor: id,
                        sfx: SfxId::sfx_oof,
                    });
                }
            }
            if let Some(mo) = ctx.store.mobj_mut(id) {
                mo.momz = Fixed::ZERO;
            }
        }
        if let Some(mo) = ctx.store.mobj_mut(id) {
            mo.z = mo.floorz;
        }

        if flags & MF_MISSILE != 0 && flags & MF_NOCLIP == 0 {
            let explode = explode_missile(ctx, id);
            report.extend(explode.events);
            return report;
        }
    } else if flags & MF_NOGRAVITY == 0 {
        if momz0.is_zero() {
            if let Some(mo) = ctx.store.mobj_mut(id) {
                mo.momz = -GRAVITY_2;
            }
        } else if let Some(mo) = ctx.store.mobj_mut(id) {
            mo.momz -= GRAVITY;
        }
    }

    // hit the ceiling?
    {
        let Some(mo) = ctx.store.mobj(id) else {
            return report;
        };
        if mo.z + mo.height > mo.ceilingz {
            if mo.momz > Fixed::ZERO {
                if let Some(mo) = ctx.store.mobj_mut(id) {
                    mo.momz = Fixed::ZERO;
                }
            }
            if let Some(mo) = ctx.store.mobj_mut(id) {
                mo.z = mo.ceilingz - mo.height;
            }

            if flags & MF_SKULLFLY != 0 {
                // the skull slammed into something
                if let Some(mo) = ctx.store.mobj_mut(id) {
                    mo.momz = -mo.momz;
                }
            }

            if flags & MF_MISSILE != 0 && flags & MF_NOCLIP == 0 {
                let explode = explode_missile(ctx, id);
                report.extend(explode.events);
            }
        }
    }

    report
}

#[cfg(test)]
mod tests {
    //! Tests for the momentum ticks. Coordinates are map units.
    use super::*;

    use crate::fixed::Fixed;
    use crate::info::MobjType;
    use crate::mobj::{MobjStore, Skill, SpawnSpec, SpawnZ};
    use crate::player::{CF_NOMOMENTUM, TicCmd};
    use crate::rng::DoomRng;
    use crate::thinker::ThinkerId;

    use super::super::testutil::{fixture, sky_map, spawn_at, test_player};

    fn u(units: i32) -> Fixed {
        Fixed::from_int(units)
    }

    fn spawn_at_z(
        store: &mut MobjStore,
        map: &crate::map::Map,
        rng: &mut DoomRng,
        mobj_type: MobjType,
        x: i32,
        y: i32,
        z: Fixed,
    ) -> ThinkerId {
        store.spawn_mobj(
            map,
            rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: u(x),
                y: u(y),
                z: SpawnZ::At(z),
                mobj_type,
            },
        )
    }

    #[test]
    fn x_momentum_is_clamped_to_maxmove() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 64, 64);
        if let Some(mo) = store.mobj_mut(id) {
            mo.momx = u(100); // way over the 30 unit cap
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = xy_movement(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();
        // clamped to MAXMOVE and moved exactly 30 units
        assert_eq!(mo.x, u(94));
        assert!(
            mo.momx < crate::movement::MAXMOVE,
            "friction applies on the ground"
        );
        assert!(mo.momx > u(27));
        let _ = &report;
    }

    #[test]
    fn x_blocked_monster_loses_all_momentum() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 236, 64);
        if let Some(mo) = store.mobj_mut(id) {
            mo.momx = u(40); // substeps into the east wall
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = xy_movement(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();
        assert_eq!(mo.momx, Fixed::ZERO);
        assert_eq!(mo.momy, Fixed::ZERO);
        assert_eq!(mo.x, u(236), "the blocked step does not move the mobj");
        let _ = &report;
    }

    #[test]
    fn missile_blocked_by_wall_explodes() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_TROOPSHOT, 236, 64);
        if let Some(mo) = store.mobj_mut(id) {
            mo.momx = u(40);
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = xy_movement(&mut ctx, id);

        let mo = ctx.store.mobj(id).unwrap();
        assert_eq!(mo.flags & MF_MISSILE, 0, "the missile stops being one");
        assert_eq!(
            mo.state,
            crate::info::MOBJ_INFO[MobjType::MT_TROOPSHOT as usize].deathstate
        );
        assert!(
            report
                .events
                .iter()
                .any(|event| matches!(event, MoveEvent::Sound { actor, sfx: crate::info::SfxId::sfx_firxpl } if *actor == id)),
            "the explosion sound is reported"
        );
        assert_eq!(mo.x, u(236));
    }

    #[test]
    fn missile_against_sky_ceiling_is_consumed_silently() {
        let map = sky_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_TROOPSHOT, 128, 120);
        if let Some(mo) = store.mobj_mut(id) {
            mo.momy = u(16); // substep lands the missile exactly into the window
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = xy_movement(&mut ctx, id);
        assert!(
            ctx.store.mobj(id).is_none(),
            "the missile is removed without exploding"
        );
        assert!(
            !report
                .events
                .iter()
                .any(|event| matches!(event, MoveEvent::Sound { .. })),
            "no sound for a sky hit"
        );
    }

    #[test]
    fn ground_friction_slows_a_walking_mobj() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_PLAYER, 64, 64);
        if let Some(mo) = store.mobj_mut(id) {
            mo.player = Some(0);
            mo.momx = u(4);
        }
        let players = &mut [test_player()];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = xy_movement(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();
        assert_eq!(mo.x, u(68));
        assert_eq!(mo.momx, u(4) * crate::movement::FRICTION);
        let _ = &report;
    }

    #[test]
    fn slow_ground_momentum_with_idle_buttons_stops_dead() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_PLAYER, 64, 64);
        if let Some(mo) = store.mobj_mut(id) {
            mo.player = Some(0);
            // below STOPSPEED (0x1000 = 1/16 unit)
            mo.momx = Fixed::from_raw(2000);
            mo.momy = Fixed::from_raw(-2000);
        }
        // idle tic buttons
        let mut player = test_player();
        player.mo = Some(id);
        player.cmd = TicCmd::default();
        let players = &mut [player];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = xy_movement(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();
        assert_eq!(mo.momx, Fixed::ZERO);
        assert_eq!(mo.momy, Fixed::ZERO);
        let _ = &report;
    }

    #[test]
    fn airborne_momentum_is_not_damped() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let id = spawn_at_z(
            &mut store,
            &map,
            &mut rng,
            MobjType::MT_POSSESSED,
            64,
            64,
            u(32),
        );
        if let Some(mo) = store.mobj_mut(id) {
            mo.momx = u(4);
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = xy_movement(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();
        // not on the floor: no friction
        assert_eq!(mo.momx, u(4));
        let _ = &report;
    }

    #[test]
    fn missiles_feel_no_friction() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_TROOPSHOT, 64, 64);
        if let Some(mo) = store.mobj_mut(id) {
            mo.momx = u(4);
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = xy_movement(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();
        // missiles are exempt from ground friction, on the floor or not
        assert_eq!(mo.momx, u(4));
        let _ = &report;
    }

    #[test]
    fn corpse_off_its_floor_keeps_momentum() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_POSSESSED, 64, 64);
        if let Some(mo) = store.mobj_mut(id) {
            // corpse half off a step: MF_CORPSE, standing above the floor
            mo.flags |= crate::info::MF_CORPSE;
            mo.z = u(20);
            mo.floorz = u(20); // not the sector's floor (0)
            mo.momx = u(5);
            mo.momy = u(0);
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = xy_movement(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();
        assert_eq!(mo.momx, u(5), "no friction halfway off a step");
        let _ = &report;
    }

    #[test]
    fn z_falls_with_gravity_in_two_tics() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let id = spawn_at_z(
            &mut store,
            &map,
            &mut rng,
            MobjType::MT_POSSESSED,
            64,
            64,
            u(64),
        );
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);

        // first tick off rest: z does not move yet (z += momz happens first),
        // gravity arms the first fall impulse for the next tick
        let _ = z_movement(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();
        assert_eq!(mo.momz, -GRAVITY_2);
        assert_eq!(mo.z, u(64));

        // second tick: z drops by the armed impulse, gravity adds GRAVITY
        let _ = z_movement(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();
        assert_eq!(
            mo.momz,
            Fixed::from_raw(-GRAVITY_2.to_raw() - GRAVITY.to_raw())
        );
        assert_eq!(mo.z, u(62));

        // third tick: z drops by the full 3 units of accumulated momentum
        let _ = z_movement(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();
        assert_eq!(mo.z, u(59));
    }

    #[test]
    fn z_landing_on_the_floor_clamps() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let id = spawn_at_z(
            &mut store,
            &map,
            &mut rng,
            MobjType::MT_POSSESSED,
            64,
            64,
            u(2),
        );
        if let Some(mo) = store.mobj_mut(id) {
            mo.momz = u(-8);
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = z_movement(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();
        assert_eq!(mo.z, Fixed::ZERO);
        assert_eq!(mo.momz, Fixed::ZERO);
        let _ = &report;
    }

    #[test]
    fn z_bumps_into_the_ceiling() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        // 73 + 56 (height) = 129 > 128 ceiling
        let id = spawn_at_z(
            &mut store,
            &map,
            &mut rng,
            MobjType::MT_POSSESSED,
            64,
            64,
            u(73),
        );
        if let Some(mo) = store.mobj_mut(id) {
            mo.momz = u(5);
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = z_movement(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();
        assert_eq!(mo.z, u(72), "z = ceiling - height");
        assert_eq!(mo.momz, Fixed::ZERO, "the upward momentum is cancelled");
        let _ = &report;
    }

    #[test]
    fn z_hard_landing_reports_oof() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let id = spawn_at_z(
            &mut store,
            &map,
            &mut rng,
            MobjType::MT_PLAYER,
            64,
            64,
            u(2),
        );
        if let Some(mo) = store.mobj_mut(id) {
            mo.player = Some(0);
            mo.momz = u(-12); // well past the -8 threshold
        }
        let mut player = test_player();
        player.mo = Some(id);
        let players = &mut [player];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = z_movement(&mut ctx, id);

        assert!(
            report
                .events
                .iter()
                .any(|event| matches!(event, MoveEvent::Sound { actor, sfx: crate::info::SfxId::sfx_oof } if *actor == id)),
            "the hard landing reports the oof sound"
        );
        let player = &ctx.players[0];
        // momz >> 3 with the pre-clamp momentum: -12 << 16 >> 3
        assert_eq!(player.deltaviewheight.to_raw(), (-12 << 16) >> 3);
        let mo = ctx.store.mobj(id).unwrap();
        assert_eq!(mo.z, Fixed::ZERO);
    }

    #[test]
    fn explode_missile_goes_to_deathstate_and_reports_sound() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_TROOPSHOT, 64, 64);
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = explode_missile(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();
        assert_eq!(mo.flags & MF_MISSILE, 0);
        assert_eq!(
            mo.state,
            crate::info::MOBJ_INFO[MobjType::MT_TROOPSHOT as usize].deathstate
        );
        assert!(mo.tics >= 1, "the death state has at least one tic");
        assert!(
            report
                .events
                .iter()
                .any(|event| matches!(event, MoveEvent::Sound { .. })),
            "the explosion sound is reported"
        );
    }

    #[test]
    fn skullfly_with_no_momentum_returns_to_spawnstate() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_SKULL, 64, 64);
        if let Some(mo) = store.mobj_mut(id) {
            mo.flags |= MF_SKULLFLY;
            mo.momx = Fixed::ZERO;
            mo.momy = Fixed::ZERO;
        }
        let players = &mut [];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = xy_movement(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();
        assert_eq!(mo.flags & MF_SKULLFLY, 0);
        assert_eq!(
            mo.state,
            crate::info::MOBJ_INFO[MobjType::MT_SKULL as usize].spawnstate
        );
        let _ = &report;
    }

    #[test]
    fn xy_player_blocked_by_wall_slides_up_it() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_PLAYER, 24, 64);
        if let Some(mo) = store.mobj_mut(id) {
            mo.player = Some(0);
            mo.momx = u(-10);
            mo.momy = u(8);
        }
        let players = &mut [test_player()];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = xy_movement(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();
        assert_eq!(mo.momx, Fixed::ZERO, "the wall kills the x momentum");
        assert!(mo.momy > Fixed::ZERO, "and the y momentum survives");
        assert!(mo.y > u(64), "it slid up the wall");
        let _ = &report;
    }

    #[test]
    fn momentum_cheat_zeroes_the_move() {
        let (map, mut store) = fixture();
        let mut rng = DoomRng::new();
        let id = spawn_at(&mut store, &map, &mut rng, MobjType::MT_PLAYER, 64, 64);
        if let Some(mo) = store.mobj_mut(id) {
            mo.player = Some(0);
            mo.momx = u(6);
            mo.momy = u(2);
        }
        let mut player = test_player();
        player.cheats = CF_NOMOMENTUM;
        player.mo = Some(id);
        let players = &mut [player];
        let mut ctx = MoveContext::new(&map, &mut store, players, &mut rng);
        let report = xy_movement(&mut ctx, id);
        let mo = ctx.store.mobj(id).unwrap();
        // vanilla: the mobj still moves this tick, but ends with zeroed mom
        assert_eq!(mo.momx, Fixed::ZERO);
        assert_eq!(mo.momy, Fixed::ZERO);
        let _ = &report;
    }
}
