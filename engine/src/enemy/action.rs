//! The `A_*` monster callbacks (from `p_enemy.c`) and the dispatcher that
//! runs the actions a state chain reports.
//!
//! Every handler reads its actor fresh off the store — actions collected
//! from an earlier state chain may run on a mobj that another action
//! already removed, and stale handles simply bail out.
//!
//! RNG consumption order matches the original source, call for call.

use crate::fixed::{FRACUNIT, Fixed};
use crate::info::{
    MF_AMBUSH, MF_CORPSE, MF_JUSTATTACKED, MF_SHADOW, MF_SKULLFLY, MF_SOLID, MOBJ_INFO, MobjType,
    STATES, SfxId, StateAction, StateId,
};
use crate::math::aprox_distance;
use crate::mobj::{Skill, SpawnSpec, SpawnZ};
use crate::movement::MF_SHOOTABLE;
use crate::tables::{ANGLE_TO_FINE_SHIFT, Angle, FINE_COSINE, FINE_SINE};
use crate::thinker::{Thinker, ThinkerId};

use super::chase::{DI_NODIR, check_melee_range, check_missile_range, move_actor, new_chase_dir};
use super::missile::spawn_missile;
use super::radius::radius_attack;
use super::{EnemyContext, GameEvent, MISSILERANGE};

/// Vanilla `TRACEANGLE` (p_enemy.c).
pub const TRACEANGLE: u32 = 0x0C00_0000;
/// Vanilla `SKULLSPEED` (p_enemy.c).
pub const SKULLSPEED: Fixed = Fixed::from_raw(20 * FRACUNIT);
/// Vanilla `FATSPREAD` (p_enemy.c, `ANG90/8`).
pub const FATSPREAD: u32 = 0x0800_0000;

/// Vanilla `P_SetMobjState` followed by the actions it produces, inline.
///
/// Wraps [`crate::mobj::MobjStore::set_mobj_state`]: the store applies the
/// zero-tic state chain atomically and reports the actions it would have
/// called; this runs them in order, recursing through nested state changes
/// (e.g. `A_Look` switching to the seesound state) the way vanilla's
/// recursion does.
pub fn set_mobj_state_and_run_actions(
    ctx: &mut EnemyContext<'_>,
    actor: ThinkerId,
    state: StateId,
) {
    let report = ctx.store.set_mobj_state(actor, state, ctx.leveltime);
    for action in report.actions {
        dispatch_action(ctx, actor, action);
    }
}

/// Dispatches one `A_*` action.
///
/// Returns `true` when the action was handled by the monster/world layer,
/// `false` for psprite (weapon) actions and light actions the psprite and
/// light-special layers own. Unknown/weapon actions are not an error:
/// the gameplay layer chains its own dispatcher from the `false` case.
pub fn dispatch_action(ctx: &mut EnemyContext<'_>, actor: ThinkerId, action: StateAction) -> bool {
    match action {
        StateAction::A_Look => a_look(ctx, actor),
        StateAction::A_Chase => a_chase(ctx, actor),
        StateAction::A_FaceTarget => a_face_target(ctx, actor),
        StateAction::A_PosAttack => a_pos_attack(ctx, actor),
        StateAction::A_SPosAttack => a_spos_attack(ctx, actor),
        StateAction::A_CPosAttack => a_cpos_attack(ctx, actor),
        StateAction::A_CPosRefire => a_cpos_refire(ctx, actor),
        StateAction::A_SpidRefire => a_spid_refire(ctx, actor),
        StateAction::A_BspiAttack => a_bspi_attack(ctx, actor),
        StateAction::A_TroopAttack => a_troop_attack(ctx, actor),
        StateAction::A_SargAttack => a_sarg_attack(ctx, actor),
        StateAction::A_HeadAttack => a_head_attack(ctx, actor),
        StateAction::A_BruisAttack => a_bruis_attack(ctx, actor),
        StateAction::A_CyberAttack => a_cyber_attack(ctx, actor),
        StateAction::A_FatRaise => a_fat_raise(ctx, actor),
        StateAction::A_FatAttack1 => a_fat_attack_1(ctx, actor),
        StateAction::A_FatAttack2 => a_fat_attack_2(ctx, actor),
        StateAction::A_FatAttack3 => a_fat_attack_3(ctx, actor),
        StateAction::A_SkelWhoosh => a_skel_whoosh(ctx, actor),
        StateAction::A_SkelFist => a_skel_fist(ctx, actor),
        StateAction::A_SkelMissile => a_skel_missile(ctx, actor),
        StateAction::A_Tracer => a_tracer(ctx, actor),
        StateAction::A_SkullAttack => a_skull_attack(ctx, actor),
        StateAction::A_PainAttack => a_pain_attack(ctx, actor),
        StateAction::A_PainDie => a_pain_die(ctx, actor),
        StateAction::A_Metal => a_metal(ctx, actor),
        StateAction::A_Hoof => a_hoof(ctx, actor),
        StateAction::A_BabyMetal => a_baby_metal(ctx, actor),
        StateAction::A_VileChase => a_vile_chase(ctx, actor),
        StateAction::A_VileStart => a_vile_start(ctx, actor),
        StateAction::A_VileTarget => a_vile_target(ctx, actor),
        StateAction::A_VileAttack => a_vile_attack(ctx, actor),
        StateAction::A_StartFire => a_start_fire(ctx, actor),
        StateAction::A_Fire => a_fire(ctx, actor),
        StateAction::A_FireCrackle => a_fire_crackle(ctx, actor),
        StateAction::A_Explode => a_explode(ctx, actor),
        StateAction::A_BFGSpray => a_bfg_spray(ctx, actor),
        StateAction::A_Pain => a_pain(ctx, actor),
        StateAction::A_Scream => a_scream(ctx, actor),
        StateAction::A_XScream => a_x_scream(ctx, actor),
        StateAction::A_PlayerScream => a_player_scream(ctx, actor),
        StateAction::A_Fall => a_fall(ctx, actor),
        StateAction::A_BossDeath => a_boss_death(ctx, actor),
        StateAction::A_KeenDie => a_keen_die(ctx, actor),
        StateAction::A_BrainAwake => a_brain_awake(ctx, actor),
        StateAction::A_BrainPain => a_brain_pain(ctx, actor),
        StateAction::A_BrainScream => a_brain_scream(ctx, actor),
        StateAction::A_BrainExplode => a_brain_explode(ctx, actor),
        StateAction::A_BrainDie => a_brain_die(ctx, actor),
        StateAction::A_BrainSpit => a_brain_spit(ctx, actor),
        StateAction::A_SpawnSound => a_spawn_sound(ctx, actor),
        StateAction::A_SpawnFly => a_spawn_fly(ctx, actor),
        _ => return false,
    }
    true
}

// ---------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------

/// Vanilla `A_FaceTarget`.
///
/// Turns the actor towards its target; clears `MF_AMBUSH` (a monster that
/// faced something is no longer dormant). Mirrors the vanilla shadow-fuzz
/// angle when the target is fuzzy.
pub fn a_face_target(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let Some(target) = a.target else {
        return;
    };
    let Some(t) = ctx.store.mobj(target) else {
        return;
    };

    let mut angle = crate::bsp::point_to_angle_2(a.x, a.y, t.x, t.y);

    if t.has_flag(MF_SHADOW) {
        let fuzz = ((ctx.rng.next() as i32 - ctx.rng.next() as i32) << 21) as u32;
        angle = Angle::from_raw(angle.to_raw().wrapping_add(fuzz));
    }

    if let Some(m) = ctx.store.mobj_mut(actor) {
        m.flags &= !MF_AMBUSH;
        m.angle = angle;
    }
}

fn see_you(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let seesound = a.info().seesound;
    if seesound != SfxId::sfx_None {
        let sound = match seesound {
            SfxId::sfx_posit1 | SfxId::sfx_posit2 | SfxId::sfx_posit3 => {
                [SfxId::sfx_posit1, SfxId::sfx_posit2, SfxId::sfx_posit3]
                    [ctx.rng.random_below(3) as usize]
            }
            SfxId::sfx_bgsit1 | SfxId::sfx_bgsit2 => {
                [SfxId::sfx_bgsit1, SfxId::sfx_bgsit2][ctx.rng.random_below(2) as usize]
            }
            other => other,
        };

        let boss = a.mobj_type == MobjType::MT_SPIDER || a.mobj_type == MobjType::MT_CYBORG;
        // full volume for the bosses
        ctx.events.push(GameEvent::Sound {
            actor: if boss { None } else { Some(actor) },
            sfx: sound,
        });
    }

    let seestate = a.info().seestate;
    set_mobj_state_and_run_actions(ctx, actor, seestate);
}

/// Vanilla `A_Look`. Stay in state until a player is sighted.
pub fn a_look(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    // read what we need, then drop the borrow before mutating
    let Some((sector, ambush)) = ctx
        .store
        .mobj(actor)
        .map(|a| (a.sector, a.has_flag(MF_AMBUSH)))
    else {
        return;
    };

    // any shot will wake up
    if let Some(m) = ctx.store.mobj_mut(actor) {
        m.threshold = 0;
    }

    // playeringame[i], derived (see module docs of crate::enemy)
    let sound_target = sector
        .and_then(|sector| ctx.flood.sound_target(sector))
        .and_then(|targ| {
            ctx.store.mobj(targ).and_then(|t| {
                if t.has_flag(MF_SHOOTABLE) {
                    Some(targ)
                } else {
                    None
                }
            })
        });

    if let Some(targ) = sound_target {
        let proceed = if ambush {
            match (ctx.store.mobj(actor), ctx.store.mobj(targ)) {
                (Some(a), Some(t)) => super::sight::check_sight(ctx.map, a, t),
                _ => false,
            }
        } else {
            true
        };

        if proceed {
            if let Some(m) = ctx.store.mobj_mut(actor) {
                m.target = Some(targ);
            }
            see_you(ctx, actor);
            return;
        }
        // ambush with no LOS: fall through and try the normal scan
    }

    if !super::chase::look_for_players(ctx, actor, false) {
        return;
    }

    see_you(ctx, actor);
}

/// Vanilla `A_Chase`. Actor has a melee attack, so it tries to close as
/// fast as possible.
pub fn a_chase(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    // read what we need, then mutate freely
    let Some((reactiontime, threshold, movedir, angle_raw)) = ctx
        .store
        .mobj(actor)
        .map(|a| (a.reactiontime, a.threshold, a.movedir, a.angle.to_raw()))
    else {
        return;
    };

    if reactiontime > 0 {
        if let Some(m) = ctx.store.mobj_mut(actor) {
            m.reactiontime -= 1;
        }
    }

    if threshold != 0 {
        let target_ok = ctx
            .store
            .mobj(actor)
            .and_then(|a| a.target)
            .and_then(|t| ctx.store.mobj(t))
            .is_some_and(|t| t.health > 0);
        if let Some(m) = ctx.store.mobj_mut(actor) {
            if target_ok {
                m.threshold -= 1;
            } else {
                m.threshold = 0;
            }
        }
    }

    // turn towards movement direction if not there yet
    if movedir < 8 {
        let masked = angle_raw & 0xE000_0000;
        let delta = masked.wrapping_sub((movedir as u32) << 29) as i32;
        let new_angle = if delta > 0 {
            Angle::from_raw(masked.wrapping_sub(0x2000_0000))
        } else if delta < 0 {
            Angle::from_raw(masked.wrapping_add(0x2000_0000))
        } else {
            Angle::from_raw(masked)
        };
        if let Some(m) = ctx.store.mobj_mut(actor) {
            m.angle = new_angle;
        }
    }

    // re-read for the validity check
    let Some((
        target,
        threshold,
        movecount,
        meleestate,
        missilestate,
        attacksound,
        activesound,
        justattacked,
    )) = ctx.store.mobj(actor).map(|a| {
        (
            a.target,
            a.threshold,
            a.movecount,
            a.info().meleestate,
            a.info().missilestate,
            a.info().attacksound,
            a.info().activesound,
            a.has_flag(MF_JUSTATTACKED),
        )
    })
    else {
        return;
    };

    let target_valid = target
        .and_then(|t| ctx.store.mobj(t))
        .is_some_and(|t| t.has_flag(MF_SHOOTABLE));

    if !target_valid {
        // look for a new target
        if super::chase::look_for_players(ctx, actor, true) {
            return; // got a new target
        }

        let spawnstate = ctx.store.mobj(actor).map(|a| a.info().spawnstate);
        if let Some(spawnstate) = spawnstate {
            set_mobj_state_and_run_actions(ctx, actor, spawnstate);
        }
        return;
    }

    // do not attack twice in a row
    if justattacked {
        if let Some(m) = ctx.store.mobj_mut(actor) {
            m.flags &= !MF_JUSTATTACKED;
        }
        if ctx.rules.skill != crate::mobj::Skill::Nightmare && !ctx.rules.fastparm {
            new_chase_dir(ctx, actor);
        }
        return;
    }

    // check for melee attack
    if meleestate != StateId::S_NULL && check_melee_range(ctx, actor) {
        if attacksound != SfxId::sfx_None {
            ctx.events.push(GameEvent::Sound {
                actor: Some(actor),
                sfx: attacksound,
            });
        }
        set_mobj_state_and_run_actions(ctx, actor, meleestate);
        return;
    }

    // check for missile attack
    if missilestate != StateId::S_NULL {
        let may_fire = ctx.rules.skill == crate::mobj::Skill::Nightmare
            || ctx.rules.fastparm
            || movecount == 0;
        if may_fire && check_missile_range(ctx, actor) {
            set_mobj_state_and_run_actions(ctx, actor, missilestate);
            if let Some(m) = ctx.store.mobj_mut(actor) {
                m.flags |= MF_JUSTATTACKED;
            }
            return;
        }
    }

    // possibly choose another target
    if ctx.rules.netgame && threshold == 0 {
        let target_gone_or_unseen = match target.and_then(|t| ctx.store.mobj(t)) {
            Some(t) => {
                let in_sight = ctx
                    .store
                    .mobj(actor)
                    .map(|a| super::sight::check_sight(ctx.map, a, t))
                    .unwrap_or(false);
                !in_sight
            }
            None => true,
        };
        if target_gone_or_unseen && super::chase::look_for_players(ctx, actor, true) {
            return; // got a new target
        }
    }

    // chase towards player
    let movecount = movecount - 1;
    if let Some(m) = ctx.store.mobj_mut(actor) {
        m.movecount = movecount;
    }

    if movecount < 0 || !move_actor(ctx, actor) {
        new_chase_dir(ctx, actor);
    }

    // make active sound
    if activesound != SfxId::sfx_None && ctx.rng.next() < 3 {
        ctx.events.push(GameEvent::Sound {
            actor: Some(actor),
            sfx: activesound,
        });
    }
}

// ---------------------------------------------------------------------
// monster attacks
// ---------------------------------------------------------------------

/// Vanilla `A_PosAttack`.
pub fn a_pos_attack(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    if a.target.is_none() {
        return;
    }

    a_face_target(ctx, actor);
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let angle = a.angle;
    let aim = super::hitscan::aim_line_attack(ctx, actor, angle, MISSILERANGE);

    ctx.events.push(GameEvent::Sound {
        actor: Some(actor),
        sfx: SfxId::sfx_pistol,
    });

    let angle =
        angle + Angle::from_raw(((ctx.rng.next() as i32 - ctx.rng.next() as i32) << 20) as u32);
    let damage = ((ctx.rng.random_below(5)) + 1) as i32 * 3;
    super::hitscan::line_attack(ctx, actor, angle, MISSILERANGE, aim.slope, damage);
}

/// Vanilla `A_SPosAttack`.
pub fn a_spos_attack(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    if a.target.is_none() {
        return;
    }

    ctx.events.push(GameEvent::Sound {
        actor: Some(actor),
        sfx: SfxId::sfx_shotgn,
    });

    a_face_target(ctx, actor);
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let bangle = a.angle;
    let aim = super::hitscan::aim_line_attack(ctx, actor, bangle, MISSILERANGE);

    for _ in 0..3 {
        let angle = bangle
            + Angle::from_raw(((ctx.rng.next() as i32 - ctx.rng.next() as i32) << 20) as u32);
        let damage = (ctx.rng.random_below(5) + 1) as i32 * 3;
        super::hitscan::line_attack(ctx, actor, angle, MISSILERANGE, aim.slope, damage);
    }
}

/// Vanilla `A_CPosAttack`.
pub fn a_cpos_attack(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    if a.target.is_none() {
        return;
    }

    ctx.events.push(GameEvent::Sound {
        actor: Some(actor),
        sfx: SfxId::sfx_shotgn,
    });

    a_face_target(ctx, actor);
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let bangle = a.angle;
    let aim = super::hitscan::aim_line_attack(ctx, actor, bangle, MISSILERANGE);

    let angle =
        bangle + Angle::from_raw(((ctx.rng.next() as i32 - ctx.rng.next() as i32) << 20) as u32);
    let damage = (ctx.rng.random_below(5) + 1) as i32 * 3;
    super::hitscan::line_attack(ctx, actor, angle, MISSILERANGE, aim.slope, damage);
}

/// Vanilla `A_CPosRefire`. Keep firing unless the target got out of
/// sight.
pub fn a_cpos_refire(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    // keep firing unless target got out of sight
    a_face_target(ctx, actor);

    if ctx.rng.next() < 40 {
        return;
    }

    let seestate = {
        let Some(a) = ctx.store.mobj(actor) else {
            return;
        };
        a.info().seestate
    };

    let target = ctx.store.mobj(actor).and_then(|a| a.target);
    let keep = match target {
        Some(t) => match ctx.store.mobj(t) {
            Some(m) => {
                m.health > 0
                    && match (ctx.store.mobj(actor), ctx.store.mobj(t)) {
                        (Some(a), Some(pm)) => super::sight::check_sight(ctx.map, a, pm),
                        _ => false,
                    }
            }
            None => false,
        },
        None => false,
    };

    if !keep {
        set_mobj_state_and_run_actions(ctx, actor, seestate);
    }
}

/// Vanilla `A_SpidRefire`. Keep firing unless the target got out of
/// sight.
pub fn a_spid_refire(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    // keep firing unless target got out of sight
    a_face_target(ctx, actor);

    if ctx.rng.next() < 10 {
        return;
    }

    let seestate = {
        let Some(a) = ctx.store.mobj(actor) else {
            return;
        };
        a.info().seestate
    };

    let target = ctx.store.mobj(actor).and_then(|a| a.target);
    let keep = match target {
        Some(t) => match ctx.store.mobj(t) {
            Some(m) => {
                m.health > 0
                    && match (ctx.store.mobj(actor), ctx.store.mobj(t)) {
                        (Some(a), Some(pm)) => super::sight::check_sight(ctx.map, a, pm),
                        _ => false,
                    }
            }
            None => false,
        },
        None => false,
    };

    if !keep {
        set_mobj_state_and_run_actions(ctx, actor, seestate);
    }
}

/// Vanilla `A_BspiAttack`.
pub fn a_bspi_attack(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let Some(target) = a.target else {
        return;
    };

    a_face_target(ctx, actor);

    // launch a missile
    spawn_missile(ctx, actor, target, MobjType::MT_ARACHPLAZ);
}

/// Vanilla `A_TroopAttack`.
pub fn a_troop_attack(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let Some(target) = a.target else {
        return;
    };

    a_face_target(ctx, actor);

    if check_melee_range(ctx, actor) {
        ctx.events.push(GameEvent::Sound {
            actor: Some(actor),
            sfx: SfxId::sfx_claw,
        });
        let damage = (ctx.rng.random_below(8) + 1) as i32 * 3;
        ctx.events.push(GameEvent::Damage {
            target,
            inflictor: Some(actor),
            source: Some(actor),
            damage,
        });
        return;
    }

    // launch a missile
    spawn_missile(ctx, actor, target, MobjType::MT_TROOPSHOT);
}

/// Vanilla `A_SargAttack`.
pub fn a_sarg_attack(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let Some(target) = a.target else {
        return;
    };

    a_face_target(ctx, actor);

    if check_melee_range(ctx, actor) {
        let damage = (ctx.rng.random_below(10) + 1) as i32 * 4;
        ctx.events.push(GameEvent::Damage {
            target,
            inflictor: Some(actor),
            source: Some(actor),
            damage,
        });
    }
}

/// Vanilla `A_HeadAttack`.
pub fn a_head_attack(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let Some(target) = a.target else {
        return;
    };

    a_face_target(ctx, actor);

    if check_melee_range(ctx, actor) {
        let damage = (ctx.rng.random_below(6) + 1) as i32 * 10;
        ctx.events.push(GameEvent::Damage {
            target,
            inflictor: Some(actor),
            source: Some(actor),
            damage,
        });
        return;
    }

    // launch a missile
    spawn_missile(ctx, actor, target, MobjType::MT_HEADSHOT);
}

/// Vanilla `A_BruisAttack`.
pub fn a_bruis_attack(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let Some(target) = a.target else {
        return;
    };

    if check_melee_range(ctx, actor) {
        ctx.events.push(GameEvent::Sound {
            actor: Some(actor),
            sfx: SfxId::sfx_claw,
        });
        let damage = (ctx.rng.random_below(8) + 1) as i32 * 10;
        ctx.events.push(GameEvent::Damage {
            target,
            inflictor: Some(actor),
            source: Some(actor),
            damage,
        });
        return;
    }

    // launch a missile
    spawn_missile(ctx, actor, target, MobjType::MT_BRUISERSHOT);
}

/// Vanilla `A_CyberAttack`.
pub fn a_cyber_attack(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let Some(target) = a.target else {
        return;
    };

    a_face_target(ctx, actor);
    spawn_missile(ctx, actor, target, MobjType::MT_ROCKET);
}

/// Vanilla `A_FatRaise`.
pub fn a_fat_raise(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    a_face_target(ctx, actor);
    if ctx.store.mobj(actor).is_some() {
        ctx.events.push(GameEvent::Sound {
            actor: Some(actor),
            sfx: SfxId::sfx_manatk,
        });
    }
}

/// Vanilla `A_FatAttack1`.
pub fn a_fat_attack_1(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    a_face_target(ctx, actor);
    let Some(target) = ctx.store.mobj(actor).and_then(|a| a.target) else {
        return;
    };

    // change direction to one side
    if let Some(m) = ctx.store.mobj_mut(actor) {
        m.angle = Angle::from_raw(m.angle.to_raw().wrapping_add(FATSPREAD));
    }
    spawn_missile(ctx, actor, target, MobjType::MT_FATSHOT);

    let Some(mo) = spawn_missile(ctx, actor, target, MobjType::MT_FATSHOT) else {
        return;
    };
    let speed = Fixed::from_raw(MOBJ_INFO[MobjType::MT_FATSHOT as usize].speed);
    if let Some(m) = ctx.store.mobj_mut(mo) {
        m.angle = Angle::from_raw(m.angle.to_raw().wrapping_add(FATSPREAD));
        let an = (m.angle.to_raw() >> ANGLE_TO_FINE_SHIFT) as usize;
        m.momx = speed * FINE_COSINE[an];
        m.momy = speed * FINE_SINE[an];
    }
}

/// Vanilla `A_FatAttack2`.
pub fn a_fat_attack_2(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    a_face_target(ctx, actor);
    let Some(target) = ctx.store.mobj(actor).and_then(|a| a.target) else {
        return;
    };

    // now here choose the opposite deviation
    if let Some(m) = ctx.store.mobj_mut(actor) {
        m.angle = Angle::from_raw(m.angle.to_raw().wrapping_sub(FATSPREAD));
    }
    spawn_missile(ctx, actor, target, MobjType::MT_FATSHOT);

    let Some(mo) = spawn_missile(ctx, actor, target, MobjType::MT_FATSHOT) else {
        return;
    };
    let speed = Fixed::from_raw(MOBJ_INFO[MobjType::MT_FATSHOT as usize].speed);
    if let Some(m) = ctx.store.mobj_mut(mo) {
        m.angle = Angle::from_raw(m.angle.to_raw().wrapping_sub(FATSPREAD.wrapping_mul(2)));
        let an = (m.angle.to_raw() >> ANGLE_TO_FINE_SHIFT) as usize;
        m.momx = speed * FINE_COSINE[an];
        m.momy = speed * FINE_SINE[an];
    }
}

/// Vanilla `A_FatAttack3`.
pub fn a_fat_attack_3(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    a_face_target(ctx, actor);
    let Some(target) = ctx.store.mobj(actor).and_then(|a| a.target) else {
        return;
    };

    let speed = Fixed::from_raw(MOBJ_INFO[MobjType::MT_FATSHOT as usize].speed);

    let Some(mo) = spawn_missile(ctx, actor, target, MobjType::MT_FATSHOT) else {
        return;
    };
    if let Some(m) = ctx.store.mobj_mut(mo) {
        m.angle = Angle::from_raw(m.angle.to_raw().wrapping_sub(FATSPREAD / 2));
        let an = (m.angle.to_raw() >> ANGLE_TO_FINE_SHIFT) as usize;
        m.momx = speed * FINE_COSINE[an];
        m.momy = speed * FINE_SINE[an];
    }

    let Some(mo) = spawn_missile(ctx, actor, target, MobjType::MT_FATSHOT) else {
        return;
    };
    if let Some(m) = ctx.store.mobj_mut(mo) {
        m.angle = Angle::from_raw(m.angle.to_raw().wrapping_add(FATSPREAD / 2));
        let an = (m.angle.to_raw() >> ANGLE_TO_FINE_SHIFT) as usize;
        m.momx = speed * FINE_COSINE[an];
        m.momy = speed * FINE_SINE[an];
    }
}

/// Vanilla `A_SkelWhoosh`.
pub fn a_skel_whoosh(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    if a.target.is_none() {
        return;
    }
    a_face_target(ctx, actor);
    ctx.events.push(GameEvent::Sound {
        actor: Some(actor),
        sfx: SfxId::sfx_skeswg,
    });
}

/// Vanilla `A_SkelFist`.
pub fn a_skel_fist(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let Some(target) = a.target else {
        return;
    };

    a_face_target(ctx, actor);

    if check_melee_range(ctx, actor) {
        let damage = (ctx.rng.random_below(10) + 1) as i32 * 6;
        ctx.events.push(GameEvent::Sound {
            actor: Some(actor),
            sfx: SfxId::sfx_skepch,
        });
        ctx.events.push(GameEvent::Damage {
            target,
            inflictor: Some(actor),
            source: Some(actor),
            damage,
        });
    }
}

/// Vanilla `A_SkelMissile`.
pub fn a_skel_missile(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let Some(target) = a.target else {
        return;
    };

    a_face_target(ctx, actor);

    // so the missile spawns higher
    if let Some(m) = ctx.store.mobj_mut(actor) {
        m.z += Fixed::from_raw(16 * FRACUNIT);
    }

    let mo = spawn_missile(ctx, actor, target, MobjType::MT_TRACER);

    // back to normal
    if let Some(m) = ctx.store.mobj_mut(actor) {
        m.z -= Fixed::from_raw(16 * FRACUNIT);
    }

    let Some(mo) = mo else {
        return;
    };
    let (momx, momy) = match ctx.store.mobj(mo) {
        Some(m) => (m.momx, m.momy),
        None => return,
    };
    if let Some(m) = ctx.store.mobj_mut(mo) {
        m.x += momx;
        m.y += momy;
        m.tracer = Some(target);
    }
}

/// Vanilla `A_Tracer`.
pub fn a_tracer(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    if ctx.leveltime & 3 != 0 {
        return;
    }

    // spawn a puff of smoke behind the rocket
    let (x, y, z, momx, momy, angle) = match ctx.store.mobj(actor) {
        Some(a) => (a.x, a.y, a.z, a.momx, a.momy, a.angle),
        None => return,
    };

    {
        let mut parts = super::hitscan::PuffParts::from_enemy(ctx);
        super::hitscan::spawn_puff(&mut parts, x, y, z, MISSILERANGE);
    }

    let smoke = ctx.store.spawn_mobj(
        ctx.map,
        ctx.rng,
        SpawnSpec {
            skill: ctx.rules.skill,
            x: x - momx,
            y: y - momy,
            z: SpawnZ::At(z),
            mobj_type: MobjType::MT_SMOKE,
        },
    );

    {
        let adj = i32::from(ctx.rng.next() & 3);
        if let Some(m) = ctx.store.mobj_mut(smoke) {
            m.momz = Fixed::ONE;
            m.tics -= adj;
            if m.tics < 1 {
                m.tics = 1;
            }
        }
    }

    // adjust direction
    let Some(dest) = ctx.store.mobj(actor).and_then(|a| a.tracer) else {
        return;
    };

    // copy what we need out of the destination before borrowing again
    let (d_x, d_y, d_z, d_health) = match ctx.store.mobj(dest) {
        Some(d) => (d.x, d.y, d.z, d.health),
        None => return,
    };

    if d_health <= 0 {
        return;
    }

    let exact = crate::bsp::point_to_angle_2(x, y, d_x, d_y);

    let mut angle = angle;
    if exact != angle {
        let diff = exact.to_raw().wrapping_sub(angle.to_raw());
        if diff > 0x8000_0000 {
            let new_angle = Angle::from_raw(angle.to_raw().wrapping_sub(TRACEANGLE));
            let new_diff = exact.to_raw().wrapping_sub(new_angle.to_raw());
            angle = new_angle;
            if new_diff < 0x8000_0000 {
                angle = exact;
            }
        } else {
            let new_angle = Angle::from_raw(angle.to_raw().wrapping_add(TRACEANGLE));
            let new_diff = exact.to_raw().wrapping_sub(new_angle.to_raw());
            angle = new_angle;
            if new_diff > 0x8000_0000 {
                angle = exact;
            }
        }
    }

    let fine = (angle.to_raw() >> ANGLE_TO_FINE_SHIFT) as usize;
    let speed = Fixed::from_raw(MOBJ_INFO[MobjType::MT_TRACER as usize].speed);

    if let Some(m) = ctx.store.mobj_mut(actor) {
        m.angle = angle;
        m.momx = speed * FINE_COSINE[fine];
        m.momy = speed * FINE_SINE[fine];
    }

    // change slope
    let dist = aprox_distance(d_x - x, d_y - y).to_raw();
    let tracer_speed = MOBJ_INFO[MobjType::MT_TRACER as usize].speed;
    let dist = if tracer_speed == 0 {
        1
    } else {
        let dist = dist / tracer_speed;
        if dist < 1 { 1 } else { dist }
    };

    let slope = (d_z + Fixed::from_raw(40 * FRACUNIT) - z).to_raw() / dist;

    let momz = ctx.store.mobj(actor).map(|a| a.momz);
    if let Some(momz) = momz {
        let adj = Fixed::from_raw(FRACUNIT / 8);
        let new_momz = if slope < momz.to_raw() {
            momz - adj
        } else {
            momz + adj
        };
        if let Some(m) = ctx.store.mobj_mut(actor) {
            m.momz = new_momz;
        }
    }
}

/// Vanilla `A_SkullAttack`. Fly at the player like a missile.
pub fn a_skull_attack(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let Some(dest) = a.target else {
        return;
    };

    if let Some(m) = ctx.store.mobj_mut(actor) {
        m.flags |= MF_SKULLFLY;
    }

    let attacksound = ctx
        .store
        .mobj(actor)
        .map(|m| m.info().attacksound)
        .unwrap_or(SfxId::sfx_None);
    if attacksound != SfxId::sfx_None {
        ctx.events.push(GameEvent::Sound {
            actor: Some(actor),
            sfx: attacksound,
        });
    }

    a_face_target(ctx, actor);

    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let an = (a.angle.to_raw() >> ANGLE_TO_FINE_SHIFT) as usize;
    let ax = a.x;
    let ay = a.y;
    let az = a.z;

    if let Some(m) = ctx.store.mobj_mut(actor) {
        m.momx = SKULLSPEED * FINE_COSINE[an];
        m.momy = SKULLSPEED * FINE_SINE[an];
    }

    let Some(d) = ctx.store.mobj(dest) else {
        return;
    };
    let dist = aprox_distance(d.x - ax, d.y - ay).to_raw();
    let mut dist = dist / SKULLSPEED.to_raw();

    if dist < 1 {
        dist = 1;
    }

    let momz = (d.z + Fixed::from_raw(d.height.to_raw() >> 1) - az).to_raw() / dist;
    if let Some(m) = ctx.store.mobj_mut(actor) {
        m.momz = Fixed::from_raw(momz);
    }
}

/// Vanilla `A_PainShootSkull`. Spawn a lost soul and launch it at the
/// target.
fn pain_shoot_skull(ctx: &mut EnemyContext<'_>, actor: ThinkerId, angle: Angle) {
    // count total number of skulls currently on the level
    let mut count = 0;
    for (_, thinker) in ctx.store.thinkers() {
        // doors and light thinkers are not skulls
        let Thinker::Mobj(m) = thinker else {
            continue;
        };
        if m.mobj_type == MobjType::MT_SKULL {
            count += 1;
        }
    }

    // if there are already 20 skulls on the level, don't spit another one
    if count > 20 {
        return;
    }

    // okay, there's room for another one
    let an = (angle.to_raw() >> ANGLE_TO_FINE_SHIFT) as usize;

    let prestep = Fixed::from_raw(
        4 * FRACUNIT
            + 3 * (ctx
                .store
                .mobj(actor)
                .map(|a| a.info().radius.to_raw())
                .unwrap_or(0)
                + MOBJ_INFO[MobjType::MT_SKULL as usize].radius.to_raw())
                / 2,
    );

    let (x, y, z) = match ctx.store.mobj(actor) {
        Some(a) => (
            a.x + prestep * FINE_COSINE[an],
            a.y + prestep * FINE_SINE[an],
            a.z + Fixed::from_raw(8 * FRACUNIT),
        ),
        None => return,
    };

    let newmobj = ctx.store.spawn_mobj(
        ctx.map,
        ctx.rng,
        SpawnSpec {
            skill: ctx.rules.skill,
            x,
            y,
            z: SpawnZ::At(z),
            mobj_type: MobjType::MT_SKULL,
        },
    );

    // check for movements
    let mut mctx = ctx.move_context();
    let report = crate::movement::try_move(&mut mctx, newmobj, x, y);
    ctx.events
        .extend(report.events.into_iter().map(GameEvent::Move));

    if !report.ok {
        // kill it immediately
        ctx.events.push(GameEvent::Damage {
            target: newmobj,
            inflictor: Some(actor),
            source: Some(actor),
            damage: 10000,
        });
        return;
    }

    let target = ctx.store.mobj(actor).and_then(|a| a.target);
    if let Some(m) = ctx.store.mobj_mut(newmobj) {
        m.target = target;
    }
    a_skull_attack(ctx, newmobj);
}

/// Vanilla `A_PainAttack`. Spawn a lost soul and launch it at the target.
pub fn a_pain_attack(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    if a.target.is_none() {
        return;
    }

    a_face_target(ctx, actor);

    let angle = match ctx.store.mobj(actor) {
        Some(a) => a.angle,
        None => return,
    };
    pain_shoot_skull(ctx, actor, angle);
}

/// Vanilla `A_PainDie`.
pub fn a_pain_die(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    a_fall(ctx, actor);

    let angle = match ctx.store.mobj(actor) {
        Some(a) => a.angle,
        None => return,
    };
    pain_shoot_skull(
        ctx,
        actor,
        Angle::from_raw(angle.to_raw().wrapping_add(Angle::ANG90.to_raw())),
    );
    pain_shoot_skull(
        ctx,
        actor,
        Angle::from_raw(angle.to_raw().wrapping_add(Angle::ANG180.to_raw())),
    );
    pain_shoot_skull(
        ctx,
        actor,
        Angle::from_raw(angle.to_raw().wrapping_add(Angle::ANG270.to_raw())),
    );
}

/// Vanilla `A_Metal`.
pub fn a_metal(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    ctx.events.push(GameEvent::Sound {
        actor: Some(actor),
        sfx: SfxId::sfx_metal,
    });
    a_chase(ctx, actor);
}

/// Vanilla `A_Hoof`.
pub fn a_hoof(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    ctx.events.push(GameEvent::Sound {
        actor: Some(actor),
        sfx: SfxId::sfx_hoof,
    });
    a_chase(ctx, actor);
}

/// Vanilla `A_BabyMetal`.
pub fn a_baby_metal(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    ctx.events.push(GameEvent::Sound {
        actor: Some(actor),
        sfx: SfxId::sfx_bspwlk,
    });
    a_chase(ctx, actor);
}

// ---------------------------------------------------------------------
// vile attack
// ---------------------------------------------------------------------

/// Vanilla `PIT_VileCheck`. Returns `true` when the corpse was found and
/// prepared for the raise. The original's return convention is inverted
/// (`false` = stop = got one); this wrapper keeps the semantics.
fn vile_check(
    ctx: &mut EnemyContext<'_>,
    thing: ThinkerId,
    viletryx: Fixed,
    viletryy: Fixed,
) -> bool {
    // copy what we need before mutating the store
    let Some(t) = ctx.store.mobj(thing) else {
        return false; // stale, skip
    };

    if !t.has_flag(MF_CORPSE) {
        return false; // not a monster
    }

    if t.tics != -1 {
        return false; // not lying still yet
    }

    if MOBJ_INFO[t.mobj_type as usize].raisestate == StateId::S_NULL {
        return false; // monster doesn't have a raise state
    }

    let maxdist = t.radius + MOBJ_INFO[MobjType::MT_VILE as usize].radius;

    if (t.x - viletryx).abs() > maxdist || (t.y - viletryy).abs() > maxdist {
        return false; // not actually touching
    }
    let (t_x, t_y, t_height) = (t.x, t.y, t.height);

    // fits in the spot?
    if let Some(m) = ctx.store.mobj_mut(thing) {
        m.momx = Fixed::ZERO;
        m.momy = Fixed::ZERO;
        m.height = Fixed::from_raw(m.height.to_raw() << 2);
    }

    let check = {
        let mut mctx = ctx.move_context();
        let report = crate::movement::check_position(&mut mctx, thing, t_x, t_y);
        ctx.events
            .extend(report.events.into_iter().map(GameEvent::Move));
        report.ok
    };

    if let Some(m) = ctx.store.mobj_mut(thing) {
        m.height = t_height;
    }

    check // got one, so stop checking
}

/// Vanilla `A_VileChase`. Check for resurrecting a body, or return to
/// the normal attack.
pub fn a_vile_chase(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };

    if a.movedir != DI_NODIR {
        let movedir = a.movedir.clamp(0, 7) as usize;
        let speed = Fixed::from_raw(a.info().speed.wrapping_mul(super::chase::XSPEED[movedir]));
        let viletryx = a.x + speed;
        let viletryy =
            a.y + Fixed::from_raw(a.info().speed.wrapping_mul(super::chase::YSPEED[movedir]));

        // find corpses to raise, in blockmap cells touched by the try box
        let radius_box = crate::fixed::Fixed::from_raw(crate::fixed::FRACUNIT * 32 * 2);
        let bbox = crate::math::BBox {
            top: viletryy + radius_box,
            bottom: viletryy - radius_box,
            right: viletryx + radius_box,
            left: viletryx - radius_box,
        };

        if let Some(range) =
            crate::map::block_range(&ctx.map.blockmap, crate::map::UnitBox::from_fixed(&bbox))
        {
            let columns = usize::from(ctx.map.blockmap.columns);
            let mut seen: std::collections::HashSet<ThinkerId> = std::collections::HashSet::new();

            'outer: for x in range.left..=range.right {
                for y in range.bottom..=range.top {
                    let cell = usize::from(y) * columns + usize::from(x);
                    let things: Vec<ThinkerId> = ctx.store.blockmap_members(cell).collect();
                    for thing in things {
                        if !seen.insert(thing) {
                            continue;
                        }
                        if vile_check(ctx, thing, viletryx, viletryy) {
                            // got one!
                            let temp = ctx.store.mobj(actor).and_then(|a| a.target);
                            if let Some(m) = ctx.store.mobj_mut(actor) {
                                m.target = Some(thing);
                            }
                            a_face_target(ctx, actor);
                            if let Some(m) = ctx.store.mobj_mut(actor) {
                                m.target = temp;
                            }

                            set_mobj_state_and_run_actions(ctx, actor, StateId::S_VILE_HEAL1);
                            ctx.events.push(GameEvent::Sound {
                                actor: Some(thing),
                                sfx: SfxId::sfx_slop,
                            });

                            let raisestate = ctx
                                .store
                                .mobj(thing)
                                .map(|m| MOBJ_INFO[m.mobj_type as usize].raisestate);
                            if let Some(raisestate) = raisestate {
                                set_mobj_state_and_run_actions(ctx, thing, raisestate);
                            }

                            if let Some(m) = ctx.store.mobj_mut(thing) {
                                m.height = Fixed::from_raw(m.height.to_raw() << 2);
                                let info = MOBJ_INFO[m.mobj_type as usize];
                                m.flags = info.flags;
                                m.health = info.spawnhealth;
                                m.target = None;
                            }

                            break 'outer;
                        }
                    }
                }
            }
        }
    }

    // return to normal attack
    a_chase(ctx, actor);
}

/// Vanilla `A_VileStart`.
pub fn a_vile_start(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    ctx.events.push(GameEvent::Sound {
        actor: Some(actor),
        sfx: SfxId::sfx_vilatk,
    });
}

/// Vanilla `A_Fire`. Keep the fire in front of the vile unless it lost
/// sight.
pub fn a_fire(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let Some(dest) = a.tracer else {
        return;
    };

    // don't move it if the vile lost sight
    let Some(vile_id) = a.target else {
        return;
    };
    let Some(d) = ctx.store.mobj(dest) else {
        return;
    };
    let Some(v) = ctx.store.mobj(vile_id) else {
        return;
    };
    if !super::sight::check_sight(ctx.map, v, d) {
        return;
    }

    let an = (d.angle.to_raw() >> ANGLE_TO_FINE_SHIFT) as usize;
    let offset = Fixed::from_raw(24 * FRACUNIT);

    let new_x = d.x + offset * FINE_COSINE[an];
    let new_y = d.y + offset * FINE_SINE[an];
    let z = d.z;

    ctx.store.unset_thing_position(actor);
    if let Some(m) = ctx.store.mobj_mut(actor) {
        m.x = new_x;
        m.y = new_y;
        m.z = z;
    }
    ctx.store.set_thing_position(ctx.map, actor);
}

/// Vanilla `A_StartFire`.
pub fn a_start_fire(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    ctx.events.push(GameEvent::Sound {
        actor: Some(actor),
        sfx: SfxId::sfx_flamst,
    });
    a_fire(ctx, actor);
}

/// Vanilla `A_FireCrackle`.
pub fn a_fire_crackle(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    ctx.events.push(GameEvent::Sound {
        actor: Some(actor),
        sfx: SfxId::sfx_flame,
    });
    a_fire(ctx, actor);
}

// ---------------------------------------------------------------------
// explosions and the BFG spray
// ---------------------------------------------------------------------

/// Vanilla `A_Explode`.
pub fn a_explode(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let target = ctx.store.mobj(actor).and_then(|a| a.target);
    radius_attack(ctx, actor, target, 128);
}

/// Vanilla `A_BFGSpray`.
///
/// Spawns a `MT_EXTRABFG` on every monster in a 40-ray fan around the
/// firing player's attack angle and deals the BFG's signature damage to
/// it. Vanilla walks the rays left to right; each ray that hits a
/// shootable target fires once.
pub fn a_bfg_spray(ctx: &mut EnemyContext<'_>, mo: ThinkerId) {
    let Some(mo_m) = ctx.store.mobj(mo) else {
        return;
    };
    let Some(shooter) = mo_m.target else {
        return; // vanilla would deref NULL
    };
    let base_angle = mo_m.angle;

    for i in 0..40u32 {
        let an = Angle::from_raw(
            base_angle
                .to_raw()
                .wrapping_sub(Angle::ANG90.to_raw() / 2)
                .wrapping_add((Angle::ANG90.to_raw() / 40).wrapping_mul(i)),
        );

        // mo->target is the originator (player) of the missile
        let aim =
            super::hitscan::aim_line_attack(ctx, shooter, an, Fixed::from_raw(16 * 64 * FRACUNIT));

        let Some(linetarget) = aim.target else {
            continue;
        };

        let Some(t) = ctx.store.mobj(linetarget) else {
            continue;
        };

        ctx.store.spawn_mobj(
            ctx.map,
            ctx.rng,
            SpawnSpec {
                skill: ctx.rules.skill,
                x: t.x,
                y: t.y,
                z: SpawnZ::At(t.z + Fixed::from_raw(t.height.to_raw() >> 2)),
                mobj_type: MobjType::MT_EXTRABFG,
            },
        );

        let mut damage = 0i32;
        for _ in 0..15 {
            damage += i32::from(ctx.rng.next() & 7) + 1;
        }

        ctx.events.push(GameEvent::Damage {
            target: linetarget,
            inflictor: Some(shooter),
            source: Some(shooter),
            damage,
        });
    }
}

// ---------------------------------------------------------------------
// pain / screams / death
// ---------------------------------------------------------------------

/// Vanilla `A_Pain`.
pub fn a_pain(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let painsound = ctx.store.mobj(actor).map(|a| a.info().painsound);
    if let Some(sfx) = painsound {
        if sfx != SfxId::sfx_None {
            ctx.events.push(GameEvent::Sound {
                actor: Some(actor),
                sfx,
            });
        }
    }
}

/// Vanilla `A_Scream`.
pub fn a_scream(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    // vanilla reads actor->info->deathsound into a switch
    let (deathsound, mobj_type) = match ctx.store.mobj(actor) {
        Some(a) => (a.info().deathsound, a.mobj_type),
        None => return,
    };

    if deathsound == SfxId::sfx_None {
        return;
    }

    let sound = match deathsound {
        SfxId::sfx_podth1 | SfxId::sfx_podth2 | SfxId::sfx_podth3 => {
            [SfxId::sfx_podth1, SfxId::sfx_podth2, SfxId::sfx_podth3]
                [ctx.rng.random_below(3) as usize]
        }
        SfxId::sfx_bgdth1 | SfxId::sfx_bgdth2 => {
            [SfxId::sfx_bgdth1, SfxId::sfx_bgdth2][ctx.rng.random_below(2) as usize]
        }
        other => other,
    };

    // check for bosses
    let boss = mobj_type == MobjType::MT_SPIDER || mobj_type == MobjType::MT_CYBORG;
    ctx.events.push(GameEvent::Sound {
        actor: if boss { None } else { Some(actor) },
        sfx: sound,
    });
}

/// Vanilla `A_XScream`.
pub fn a_x_scream(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    ctx.events.push(GameEvent::Sound {
        actor: Some(actor),
        sfx: SfxId::sfx_slop,
    });
}

/// Vanilla `A_PlayerScream`.
pub fn a_player_scream(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    // default death sound
    let mut sound = SfxId::sfx_pldeth;

    let health = ctx.store.mobj(actor).map(|m| m.health).unwrap_or(0);
    if ctx.rules.commercial && health < -50 {
        // IF THE PLAYER DIES less than -50% WITHOUT GIBBING
        sound = SfxId::sfx_pdiehi;
    }

    ctx.events.push(GameEvent::Sound {
        actor: Some(actor),
        sfx: sound,
    });
}

/// Vanilla `A_Fall`.
pub fn a_fall(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    // actor is on ground, it can be walked over
    if let Some(m) = ctx.store.mobj_mut(actor) {
        m.flags &= !MF_SOLID;
    }
}

// ---------------------------------------------------------------------
// boss death triggers
// ---------------------------------------------------------------------

/// Vanilla `A_BossDeath`. Possibly trigger special effects if on first
/// boss level.
pub fn a_boss_death(ctx: &mut EnemyContext<'_>, mo: ThinkerId) {
    let Some(mo_m) = ctx.store.mobj(mo) else {
        return;
    };
    let mobj_type = mo_m.mobj_type;

    if ctx.rules.commercial {
        if ctx.rules.map != 7 {
            return;
        }

        if mobj_type != MobjType::MT_FATSO && mobj_type != MobjType::MT_BABY {
            return;
        }
    } else {
        match ctx.rules.episode {
            1 => {
                if ctx.rules.map != 8 {
                    return;
                }

                if mobj_type != MobjType::MT_BRUISER {
                    return;
                }
            }

            2 => {
                if ctx.rules.map != 8 {
                    return;
                }

                if mobj_type != MobjType::MT_CYBORG {
                    return;
                }
            }

            3 => {
                if ctx.rules.map != 8 {
                    return;
                }

                if mobj_type != MobjType::MT_SPIDER {
                    return;
                }
            }

            4 => match ctx.rules.map {
                6 => {
                    if mobj_type != MobjType::MT_CYBORG {
                        return;
                    }
                }
                8 => {
                    if mobj_type != MobjType::MT_SPIDER {
                        return;
                    }
                }
                _ => return,
            },

            _ => {
                if ctx.rules.map != 8 {
                    return;
                }
            }
        }
    }

    // make sure there is a player alive for victory
    let any_alive = (0..crate::player::MAXPLAYERS)
        .any(|i| ctx.player_in_game(i) && ctx.players.get(i).is_some_and(|p| p.health > 0));

    if !any_alive {
        return; // no one left alive, so do not end game
    }

    // scan the remaining thinkers to see if all bosses are dead
    for (id, thinker) in ctx.store.thinkers() {
        let Thinker::Mobj(mo2) = thinker else {
            continue;
        };
        if id == mo {
            continue;
        }
        if mo2.mobj_type == mobj_type && mo2.health > 0 {
            return; // other boss not dead
        }
    }

    // victory!
    if ctx.rules.commercial {
        if ctx.rules.map == 7 {
            if mobj_type == MobjType::MT_FATSO {
                ctx.events.push(GameEvent::FloorLowerToLowest { tag: 666 });
                return;
            }

            if mobj_type == MobjType::MT_BABY {
                ctx.events.push(GameEvent::FloorRaiseToTexture { tag: 667 });
                return;
            }
        }
        // commercial map 7 handles both doors above; anything else falls
        // through to the level exit
    } else {
        match ctx.rules.episode {
            1 => {
                ctx.events.push(GameEvent::FloorLowerToLowest { tag: 666 });
                return;
            }

            4 => match ctx.rules.map {
                6 => {
                    ctx.events.push(GameEvent::DoorOpen {
                        tag: 666,
                        blaze: true,
                    });
                    return;
                }

                8 => {
                    ctx.events.push(GameEvent::FloorLowerToLowest { tag: 666 });
                    return;
                }
                _ => {}
            },
            _ => {}
        }
    }

    ctx.events.push(GameEvent::ExitLevel);
}

/// Vanilla `A_KeenDie`. DOOM II special, map 32. Uses special tag 666.
pub fn a_keen_die(ctx: &mut EnemyContext<'_>, mo: ThinkerId) {
    a_fall(ctx, mo);

    let Some(mo_m) = ctx.store.mobj(mo) else {
        return;
    };
    let mobj_type = mo_m.mobj_type;

    // scan the remaining thinkers
    // to see if all Keens are dead
    for (id, thinker) in ctx.store.thinkers() {
        let Thinker::Mobj(mo2) = thinker else {
            continue;
        };
        if id == mo {
            continue;
        }
        if mo2.mobj_type == mobj_type && mo2.health > 0 {
            // other Keen not dead
            return;
        }
    }

    ctx.events.push(GameEvent::DoorOpen {
        tag: 666,
        blaze: false,
    });
}

// ---------------------------------------------------------------------
// the brain (DOOM II MAP30)
// ---------------------------------------------------------------------

/// Vanilla `A_BrainAwake`. Finds all the target spots.
pub fn a_brain_awake(ctx: &mut EnemyContext<'_>, mo: ThinkerId) {
    // find all the target spots
    let mut targets = Vec::new();
    for (id, thinker) in ctx.store.thinkers() {
        let Thinker::Mobj(m) = thinker else {
            continue;
        };
        if m.mobj_type == MobjType::MT_BOSSTARGET {
            targets.push(id);
        }
    }

    // vanilla kept 32 slots in a fixed array; drop the rest with a warning
    if targets.len() > 32 {
        tracing::warn!(
            "A_BrainAwake: more than 32 brain targets ({}), dropping the excess",
            targets.len()
        );
        targets.truncate(32);
    }

    ctx.brain.set_targets(targets);

    let _ = mo;
    ctx.events.push(GameEvent::Sound {
        actor: None,
        sfx: SfxId::sfx_bossit,
    });
}

/// Vanilla `A_BrainPain`.
pub fn a_brain_pain(ctx: &mut EnemyContext<'_>, mo: ThinkerId) {
    let _ = mo;
    ctx.events.push(GameEvent::Sound {
        actor: None,
        sfx: SfxId::sfx_bospn,
    });
}

/// Vanilla `A_BrainScream`.
pub fn a_brain_scream(ctx: &mut EnemyContext<'_>, mo: ThinkerId) {
    let Some(mo_m) = ctx.store.mobj(mo) else {
        return;
    };
    let mo_x = mo_m.x;
    let mo_y = mo_m.y;

    let mut x = mo_x.to_raw() - 196 * FRACUNIT;
    let end = mo_x.to_raw() + 320 * FRACUNIT;

    while x < end {
        let y = mo_y - Fixed::from_raw(320 * FRACUNIT);
        let z = Fixed::from_int(128) + Fixed::from_raw(i32::from(ctx.rng.next()) * 2 * FRACUNIT);

        let th = ctx.store.spawn_mobj(
            ctx.map,
            ctx.rng,
            SpawnSpec {
                skill: ctx.rules.skill,
                x: Fixed::from_raw(x),
                y,
                z: SpawnZ::At(z),
                mobj_type: MobjType::MT_ROCKET,
            },
        );

        let momz = Fixed::from_raw(i32::from(ctx.rng.next()) * 512);
        let report = ctx
            .store
            .set_mobj_state(th, StateId::S_BRAINEXPLODE1, ctx.leveltime);
        debug_assert!(report.actions.is_empty());

        if let Some(m) = ctx.store.mobj_mut(th) {
            m.momz = momz;
            m.tics -= i32::from(ctx.rng.next() & 7);
            if m.tics < 1 {
                m.tics = 1;
            }
        }

        x += 8 * FRACUNIT;
    }

    ctx.events.push(GameEvent::Sound {
        actor: None,
        sfx: SfxId::sfx_bosdth,
    });
}

/// Vanilla `A_BrainExplode`.
pub fn a_brain_explode(ctx: &mut EnemyContext<'_>, mo: ThinkerId) {
    let Some(mo_m) = ctx.store.mobj(mo) else {
        return;
    };
    let mo_x = mo_m.x;
    let mo_y = mo_m.y;

    let x = mo_x
        .to_raw()
        .wrapping_add((ctx.rng.next() as i32 - ctx.rng.next() as i32).wrapping_mul(2048));
    let y = mo_y;
    let z = Fixed::from_int(128) + Fixed::from_raw(i32::from(ctx.rng.next()) * 2 * FRACUNIT);

    let th = ctx.store.spawn_mobj(
        ctx.map,
        ctx.rng,
        SpawnSpec {
            skill: ctx.rules.skill,
            x: Fixed::from_raw(x),
            y,
            z: SpawnZ::At(z),
            mobj_type: MobjType::MT_ROCKET,
        },
    );

    let momz = Fixed::from_raw(i32::from(ctx.rng.next()) * 512);
    let report = ctx
        .store
        .set_mobj_state(th, StateId::S_BRAINEXPLODE1, ctx.leveltime);
    debug_assert!(report.actions.is_empty());

    if let Some(m) = ctx.store.mobj_mut(th) {
        m.momz = momz;
        m.tics -= i32::from(ctx.rng.next() & 7);
        if m.tics < 1 {
            m.tics = 1;
        }
    }
}

/// Vanilla `A_BrainDie`.
pub fn a_brain_die(ctx: &mut EnemyContext<'_>, mo: ThinkerId) {
    let _ = mo;
    ctx.events.push(GameEvent::ExitLevel);
}

/// Vanilla `A_BrainSpit`.
pub fn a_brain_spit(ctx: &mut EnemyContext<'_>, mo: ThinkerId) {
    // static int easy = 0; easy ^= 1;
    let easy = matches!(ctx.rules.skill, Skill::Baby | Skill::Easy);
    let easy_flag = ctx.brain.flip_easy();
    if easy && !easy_flag {
        return;
    }

    // shoot a cube at the current target
    let Some(targ) = ctx.brain.next_target() else {
        tracing::warn!("A_BrainSpit: no boss targets known (A_BrainAwake not run?)");
        return;
    };

    let Some(mo_m) = ctx.store.mobj(mo) else {
        return;
    };
    let mo_y = mo_m.y;

    // spawn brain missile
    let newmobj = spawn_missile(ctx, mo, targ, MobjType::MT_SPAWNSHOT);

    if let Some(newmobj) = newmobj {
        if let Some(m) = ctx.store.mobj_mut(newmobj) {
            m.target = Some(targ);
        }

        let (targ_y, mo_y_val) = match ctx.store.mobj(targ) {
            Some(t) => (t.y, mo_y),
            None => return,
        };

        let (momy, tics) = match ctx.store.mobj(newmobj) {
            Some(m) => (m.momy.to_raw(), i32::from(STATES[m.state as usize].tics)),
            None => return,
        };

        if momy != 0 && tics >= 1 {
            let rt = (targ_y - mo_y_val).to_raw() / momy / tics;
            if let Some(m) = ctx.store.mobj_mut(newmobj) {
                m.reactiontime = rt;
            }
        } else {
            tracing::warn!(
                "A_BrainSpit: missile momy or state tics is zero, reactiontime left alone"
            );
        }
    }

    ctx.events.push(GameEvent::Sound {
        actor: None,
        sfx: SfxId::sfx_bospit,
    });
}

/// Vanilla `A_SpawnSound`. Travelling cube sound.
pub fn a_spawn_sound(ctx: &mut EnemyContext<'_>, mo: ThinkerId) {
    ctx.events.push(GameEvent::Sound {
        actor: Some(mo),
        sfx: SfxId::sfx_boscub,
    });
    a_spawn_fly(ctx, mo);
}

/// Vanilla `A_SpawnFly`.
pub fn a_spawn_fly(ctx: &mut EnemyContext<'_>, mo: ThinkerId) {
    // still flying?
    let reactiontime = match ctx.store.mobj_mut(mo) {
        Some(m) => {
            m.reactiontime -= 1;
            m.reactiontime
        }
        None => return,
    };

    if reactiontime != 0 {
        return; // still flying
    }

    // spawn teleport fog where the cube is heading
    let Some(targ) = ctx.store.mobj(mo).and_then(|m| m.target) else {
        return;
    };

    let Some(t) = ctx.store.mobj(targ) else {
        return;
    };
    // copy the spot; the cube is spawned relative to it
    let (t_x, t_y, t_z) = (t.x, t.y, t.z);

    let fog = ctx.store.spawn_mobj(
        ctx.map,
        ctx.rng,
        SpawnSpec {
            skill: ctx.rules.skill,
            x: t_x,
            y: t_y,
            z: SpawnZ::At(t_z),
            mobj_type: MobjType::MT_SPAWNFIRE,
        },
    );
    ctx.events.push(GameEvent::Sound {
        actor: Some(fog),
        sfx: SfxId::sfx_telept,
    });

    // randomly select a monster to spawn
    let r = ctx.rng.next();

    // probability distribution (kind of :), decreasing likelihood.
    let monster = if r < 50 {
        MobjType::MT_TROOP
    } else if r < 90 {
        MobjType::MT_SERGEANT
    } else if r < 120 {
        MobjType::MT_SHADOWS
    } else if r < 130 {
        MobjType::MT_PAIN
    } else if r < 160 {
        MobjType::MT_HEAD
    } else if r < 162 {
        MobjType::MT_VILE
    } else if r < 172 {
        MobjType::MT_UNDEAD
    } else if r < 192 {
        MobjType::MT_BABY
    } else if r < 222 {
        MobjType::MT_FATSO
    } else if r < 246 {
        MobjType::MT_KNIGHT
    } else {
        MobjType::MT_BRUISER
    };

    let newmobj = ctx.store.spawn_mobj(
        ctx.map,
        ctx.rng,
        SpawnSpec {
            skill: ctx.rules.skill,
            x: t_x,
            y: t_y,
            z: SpawnZ::At(t_z),
            mobj_type: monster,
        },
    );

    // wake it up
    if super::chase::look_for_players(ctx, newmobj, true) {
        let seestate = ctx.store.mobj(newmobj).map(|m| m.info().seestate);
        if let Some(seestate) = seestate {
            set_mobj_state_and_run_actions(ctx, newmobj, seestate);
        }
    }

    // telefrag anything in this spot
    let boss_level = ctx.rules.map == 30;
    let mut mctx = ctx.move_context();
    let report = crate::movement::teleport_move(&mut mctx, newmobj, t_x, t_y, boss_level);
    ctx.events
        .extend(report.events.into_iter().map(GameEvent::Move));

    // remove self (i.e., the cube)
    ctx.store.remove_mobj(mo, ctx.leveltime);
}

/// Vanilla `A_VileTarget`. Spawn the hellfire.
pub fn a_vile_target(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some((target, d_x, d_z)) = ctx
        .store
        .mobj(actor)
        .and_then(|a| a.target)
        .and_then(|t| ctx.store.mobj(t).map(|m| (t, m.x, m.z)))
    else {
        return;
    };

    a_face_target(ctx, actor);

    // NOTE: vanilla uses target->x twice (a slip in the original), leaving
    // the fire's y equal to its target's x; kept 1:1
    let fog = ctx.store.spawn_mobj(
        ctx.map,
        ctx.rng,
        SpawnSpec {
            skill: ctx.rules.skill,
            x: d_x,
            y: d_x,
            z: SpawnZ::At(d_z),
            mobj_type: MobjType::MT_FIRE,
        },
    );

    if let Some(m) = ctx.store.mobj_mut(actor) {
        m.tracer = Some(fog);
    }
    if let Some(f) = ctx.store.mobj_mut(fog) {
        f.target = Some(actor);
        f.tracer = Some(target);
    }

    a_fire(ctx, fog);
}

/// Vanilla `A_VileAttack`.
pub fn a_vile_attack(ctx: &mut EnemyContext<'_>, actor: ThinkerId) {
    let Some(a) = ctx.store.mobj(actor) else {
        return;
    };
    let Some(target) = a.target else {
        return;
    };

    a_face_target(ctx, actor);

    // copy what we need; the fire position math uses the attacker's
    // angle and the target's coords
    let Some((a_angle, d_x, d_y, mass)) = ctx.store.mobj(actor).and_then(|a| {
        ctx.store
            .mobj(target)
            .map(|d| (a.angle, d.x, d.y, d.info().mass))
    }) else {
        return;
    };

    {
        let (Some(a), Some(d)) = (ctx.store.mobj(actor), ctx.store.mobj(target)) else {
            return;
        };
        if !super::sight::check_sight(ctx.map, a, d) {
            return;
        }
    }

    ctx.events.push(GameEvent::Sound {
        actor: Some(actor),
        sfx: SfxId::sfx_barexp,
    });
    ctx.events.push(GameEvent::Damage {
        target,
        inflictor: Some(actor),
        source: Some(actor),
        damage: 20,
    });

    let momz = Fixed::from_raw((1000 * FRACUNIT) / mass);
    if let Some(m) = ctx.store.mobj_mut(target) {
        m.momz = momz;
    }

    let an = (a_angle.to_raw() >> ANGLE_TO_FINE_SHIFT) as usize;

    let Some(fire) = ctx.store.mobj(actor).and_then(|m| m.tracer) else {
        return;
    };

    // move the fire between the vile and the player
    let offset = Fixed::from_raw(24 * FRACUNIT);
    let new_x = d_x - offset * FINE_COSINE[an];
    let new_y = d_y - offset * FINE_SINE[an];

    if let Some(f) = ctx.store.mobj_mut(fire) {
        f.x = new_x;
        f.y = new_y;
    }

    radius_attack(ctx, fire, Some(actor), 70);
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::enemy::testutil::{
        TestWorld, open_map, players, spawn_player, spawn_thing, split_map, test_context,
    };
    use crate::info::MF_AMBUSH;
    use crate::map::types::ML_TWOSIDED;
    use crate::mobj::MobjStore;

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
        crate::rng::DoomRng,
    ) {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = crate::rng::DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);
        set_mobj(&mut store, monster, |m| m.lastlook = 0); // deterministic scan order
        (map, store, player, monster, players, rng)
    }

    #[test]
    fn dispatch_handles_monster_actions() {
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
        assert!(dispatch_action(&mut ctx, monster, StateAction::A_Fall));
        // weapon actions belong to the psprite layer
        assert!(!dispatch_action(&mut ctx, monster, StateAction::A_Punch));
        assert!(!dispatch_action(
            &mut ctx,
            monster,
            StateAction::A_WeaponReady
        ));
    }

    #[test]
    fn a_look_wakes_and_sees_the_player() {
        let (map, mut store, player, monster, mut players, mut rng) = setup();
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        set_mobj(&mut store, monster, |m| {
            m.threshold = 99;
            m.lastlook = 0;
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
            a_look(&mut ctx, monster);
        }

        // the seesound fired for the imp
        let heard = events.iter().any(|e| matches!(e, GameEvent::Sound { actor: Some(a), sfx } if *a == monster && matches!(sfx, SfxId::sfx_bgsit1 | SfxId::sfx_bgsit2)));
        assert!(heard, "expected an imp seesound");

        // it switched into its seesound state
        let state = store.mobj(monster).map(|m| m.state);
        assert_eq!(state, Some(crate::info::StateId::S_TROO_RUN1));

        // ...and it is aiming at the player now
        assert_eq!(store.mobj(monster).and_then(|m| m.target), Some(player));
        // threshold got reset by the wake-up
        assert_eq!(store.mobj(monster).map(|m| m.threshold), Some(0));
    }

    #[test]
    fn a_look_ambush_wakes_on_noise_when_it_can_see() {
        let (map, mut store, player, monster, mut players, mut rng) = setup();
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        set_mobj(&mut store, monster, |m| {
            m.flags |= MF_AMBUSH;
            m.lastlook = 0;
        });

        // the player fires; the noise floods the sector and, with a clear
        // line of sight, the ambush monster wakes too
        if let Some(p) = players[0].mo {
            let _ = crate::enemy::noise_alert(&map, &store, &mut world.flood, p, p);
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
            a_look(&mut ctx, monster);
        }

        let heard = events.iter().any(|e| matches!(e, GameEvent::Sound { sfx, .. } if matches!(sfx, SfxId::sfx_bgsit1 | SfxId::sfx_bgsit2)));
        assert!(heard, "ambush monster with a clear shot must wake on noise");
        assert_eq!(store.mobj(monster).and_then(|m| m.target), Some(player));
    }

    #[test]
    fn a_look_ambush_stays_dormant_when_blind() {
        // closed divider: the noise flood doesn't cross, so the ambush
        // monster hears nothing and stays asleep
        let map = split_map(0, 0, ML_TWOSIDED, 0x00);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = crate::rng::DoomRng::new();
        let mut players = players();
        let _player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 192);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);

        set_mobj(&mut store, monster, |m| {
            m.flags |= MF_AMBUSH;
            m.lastlook = 0;
        });

        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);
        if let Some(p) = players[0].mo {
            let _ = crate::enemy::noise_alert(&map, &store, &mut world.flood, p, p);
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
            a_look(&mut ctx, monster);
        }

        let heard = events.iter().any(|e| matches!(e, GameEvent::Sound { sfx, .. } if matches!(sfx, SfxId::sfx_bgsit1 | SfxId::sfx_bgsit2)));
        assert!(!heard, "ambush monster beyond the door must not wake");
        assert_eq!(
            store.mobj(monster).map(|m| m.state),
            Some(crate::info::StateId::S_TROO_STND)
        );
    }

    #[test]
    fn a_look_sound_wakes_non_ambush_monster_even_off_axis() {
        let (map, mut store, player, monster, mut players, mut rng) = setup();
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        // facing away from the player; a wall is not required, the sound alone wakes it
        set_mobj(&mut store, monster, |m| {
            m.angle = Angle::ANG180;
            m.lastlook = 0;
        });

        {
            let ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            crate::enemy::noise_alert(ctx.map, ctx.store, ctx.flood, player, player);
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
            a_look(&mut ctx, monster);
        }

        let heard = events.iter().any(|e| matches!(e, GameEvent::Sound { sfx, .. } if matches!(sfx, SfxId::sfx_bgsit1 | SfxId::sfx_bgsit2)));
        assert!(heard, "non-ambush monster wakes on sound alone");
        assert_eq!(store.mobj(monster).and_then(|m| m.target), Some(player));
    }

    #[test]
    fn a_chase_walks_towards_the_player() {
        let (map, mut store, player, monster, mut players, mut rng) = setup();
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        set_mobj(&mut store, monster, |m| {
            m.target = Some(player);
            m.movecount = 100;
            m.movedir = super::super::chase::DI_EAST;
            m.lastlook = 0;
        });

        let x_before = store.mobj(monster).map(|m| m.x);
        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            a_chase(&mut ctx, monster);
        }
        let x_after = store.mobj(monster).map(|m| m.x);
        assert!(x_after > x_before);
    }

    #[test]
    fn a_chase_fires_missile_on_ready() {
        let (map, mut store, player, monster, mut players, mut rng) = setup();
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        // adjacent, awake, first attack slot free
        set_mobj(&mut store, players[0].mo.unwrap(), |m| {
            m.x = Fixed::from_int(96);
            m.y = Fixed::from_int(64);
        });
        set_mobj(&mut store, monster, |m| {
            m.target = Some(player);
            m.reactiontime = 0;
            m.movecount = 0;
            m.threshold = 99; // threshold must be decremented, not blocking
            m.lastlook = 0;
        });

        // missile attacks against a distant target still work; here the
        // monster is adjacent, which forces a *hitscan* attack... use a imp
        // whose missile range check passes at any distance
        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            a_chase(&mut ctx, monster);
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
            a_chase(&mut ctx, monster);
        }

        // the melee branch entered the imp's attack sequence; the claw
        // itself lands two states later, driven by the thinker
        let state = store.mobj(monster).map(|m| m.state);
        assert_eq!(state, Some(crate::info::StateId::S_TROO_ATK1));
        // ...and it turned to face the player
        let angle = store.mobj(monster).map(|m| m.angle);
        assert_eq!(angle, Some(Angle::ZERO));
        assert_eq!(
            store
                .mobj(monster)
                .map(|m| m.flags & crate::info::MF_JUSTATTACKED),
            Some(0)
        );
    }

    #[test]
    fn a_chase_does_not_fire_during_reaction_time() {
        let (map, mut store, player, monster, mut players, mut rng) = setup();
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        set_mobj(&mut store, players[0].mo.unwrap(), |m| {
            m.x = Fixed::from_int(96);
            m.y = Fixed::from_int(64);
        });
        set_mobj(&mut store, monster, |m| {
            m.target = Some(player);
            m.reactiontime = 8;
            m.movecount = 0;
            m.lastlook = 0;
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
            a_chase(&mut ctx, monster);
        }

        assert!(
            events
                .iter()
                .all(|e| !matches!(e, GameEvent::Damage { .. }))
        );
        assert_eq!(store.mobj(monster).map(|m| m.reactiontime), Some(7));
    }

    #[test]
    fn troop_melee_uses_the_claw() {
        let (map, mut store, player, monster, mut players, mut rng) = setup();
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        set_mobj(&mut store, players[0].mo.unwrap(), |m| {
            m.x = Fixed::from_int(96);
            m.y = Fixed::from_int(64);
        });
        set_mobj(&mut store, monster, |m| m.target = Some(player));

        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            a_troop_attack(&mut ctx, monster);
        }

        let heard_claw = events.iter().any(|e| {
            matches!(
                e,
                GameEvent::Sound {
                    sfx: SfxId::sfx_claw,
                    ..
                }
            )
        });
        assert!(heard_claw);

        let damaged = events.iter().any(|e| matches!(e, GameEvent::Damage { target, damage, .. } if Some(*target) == players[0].mo && *damage > 0));
        assert!(damaged);
    }

    #[test]
    fn troop_ranged_attack_throws_a_fireball() {
        let (map, mut store, player, monster, mut players, mut rng) = setup();
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        set_mobj(&mut store, monster, |m| m.target = Some(player));

        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            a_troop_attack(&mut ctx, monster);
        }

        let heard = events.iter().any(|e| {
            matches!(
                e,
                GameEvent::Sound {
                    sfx: SfxId::sfx_firsht,
                    ..
                }
            )
        });
        assert!(heard, "expected the fireball seesound");

        let fireball = store.thinkers().any(|(_id, t)| match t {
            Thinker::Mobj(m) => m.mobj_type == MobjType::MT_TROOPSHOT,
            _ => false,
        });
        assert!(fireball, "expected a MT_TROOPSHOT to exist");
    }

    #[test]
    fn fireball_flies_towards_the_player() {
        let (map, mut store, player, monster, mut players, mut rng) = setup();
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        set_mobj(&mut store, monster, |m| m.target = Some(player));

        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            a_troop_attack(&mut ctx, monster);
        }

        let fireball = store
            .thinkers()
            .filter_map(|(id, t)| match t {
                Thinker::Mobj(m) if m.mobj_type == MobjType::MT_TROOPSHOT => Some(id),
                _ => None,
            })
            .next();
        let Some(fireball) = fireball else {
            panic!("no fireball spawned");
        };
        let momx = store.mobj(fireball).map(|m| m.momx);
        assert!(
            momx.map(|v| v > Fixed::ZERO).unwrap_or(false),
            "fireball should fly east"
        );
    }

    #[test]
    fn a_fall_clears_solid_flag() {
        let (map, mut store, _player, monster, mut players, mut rng) = setup();
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        set_mobj(&mut store, monster, |m| m.flags |= crate::info::MF_SOLID);
        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            a_fall(&mut ctx, monster);
        }
        assert_eq!(
            store.mobj(monster).map(|m| m.flags & crate::info::MF_SOLID),
            Some(0)
        );
    }

    #[test]
    fn a_face_target_clears_ambush_and_faces_player() {
        let (map, mut store, player, monster, mut players, mut rng) = setup();
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        set_mobj(&mut store, monster, |m| {
            m.flags |= MF_AMBUSH;
            m.angle = Angle::ANG180;
            m.target = Some(player);
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
            a_face_target(&mut ctx, monster);
        }

        let (angle, flags) = store
            .mobj(monster)
            .map(|m| (m.angle, m.flags))
            .unwrap_or((Angle::ZERO, 0));
        assert_eq!(flags & MF_AMBUSH, 0);
        // player is due east; the monster must now face east
        assert_eq!(angle, Angle::ZERO);
    }

    #[test]
    fn a_scream_boss_gets_full_volume() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = crate::rng::DoomRng::new();
        let mut players = players();
        spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let boss = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_CYBORG, 64, 64);
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
            a_scream(&mut ctx, boss);
        }

        let full_volume = events.iter().any(|e| {
            matches!(
                e,
                GameEvent::Sound {
                    actor: None,
                    sfx: SfxId::sfx_cybdth
                }
            )
        });
        assert!(full_volume, "boss death sound should be full volume");
    }

    #[test]
    fn a_pain_die_spawns_three_skulls_and_falls() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = crate::rng::DoomRng::new();
        let mut players = players();
        spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let pain = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_PAIN, 64, 64);
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
            a_pain_die(&mut ctx, pain);
        }

        let skulls = store
            .thinkers()
            .filter(|(_id, t)| match t {
                Thinker::Mobj(m) => m.mobj_type == MobjType::MT_SKULL,
                _ => false,
            })
            .count();
        assert_eq!(skulls, 3);
        assert_eq!(
            store.mobj(pain).map(|m| m.flags & crate::info::MF_SOLID),
            Some(0)
        );
    }

    #[test]
    fn a_boss_death_level_exit_for_episode_one() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = crate::rng::DoomRng::new();
        let mut players = players();
        spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        // the bruiser on episode 1, map 8
        let boss = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_BRUISER, 64, 64);
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
            ctx.rules.episode = 1;
            ctx.rules.map = 8;
            ctx.rules.commercial = false;
            a_boss_death(&mut ctx, boss);
        }

        let tagged = events
            .iter()
            .any(|e| matches!(e, GameEvent::FloorLowerToLowest { tag: 666 }));
        assert!(tagged, "episode 1 boss should lower the floor on tag 666");
    }

    #[test]
    fn a_boss_death_no_trigger_on_wrong_map() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = crate::rng::DoomRng::new();
        let mut players = players();
        spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let boss = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_BRUISER, 64, 64);
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
            ctx.rules.episode = 1;
            ctx.rules.map = 1; // not the boss map
            ctx.rules.commercial = false;
            a_boss_death(&mut ctx, boss);
        }

        assert!(events.is_empty());
    }

    #[test]
    fn a_vile_chase_raises_a_corpse() {
        let map = super::super::testutil::tall_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = crate::rng::DoomRng::new();
        let mut players = players();
        let _player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let vile = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_VILE, 64, 64);

        // a corpse lying where the vile is headed (far enough that the
        // raised corpse doesn't overlap the vile itself)
        let corpse = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 108, 64);
        set_mobj(&mut store, corpse, |m| {
            m.flags |= crate::info::MF_CORPSE;
            m.tics = -1;
            m.health = 0;
            m.momx = Fixed::ZERO;
            m.momy = Fixed::ZERO;
        });

        // target the player so the vile faces/charges it
        let player_mo = players[0].mo;
        set_mobj(&mut store, vile, |m| {
            m.target = player_mo;
            m.movedir = super::super::chase::DI_EAST;
            m.movecount = 0;
            m.lastlook = 0;
        });

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
            a_vile_chase(&mut ctx, vile);
        }

        // the corpse got raised: health restored, MF_CORPSE gone, target cleared
        let (flags, health, target) = store
            .mobj(corpse)
            .map(|m| (m.flags, m.health, m.target))
            .unwrap_or((0, -1, Some(vile)));
        assert_eq!(
            flags & crate::info::MF_CORPSE,
            0,
            "corpse should be raised, not still a corpse"
        );
        assert!(health > 0, "raised monster should have its spawn health");
        assert_eq!(target, None);

        // ...and the vile went into its heal state
        assert_eq!(
            store.mobj(vile).map(|m| m.state),
            Some(crate::info::StateId::S_VILE_HEAL1)
        );
    }

    #[test]
    fn a_brain_spit_cycles_targets() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = crate::rng::DoomRng::new();
        let mut players = players();
        spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let brain = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_BOSSBRAIN, 64, 64);
        let target_a = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_BOSSTARGET, 128, 64);
        let target_b = spawn_thing(
            &mut store,
            &map,
            &mut rng,
            MobjType::MT_BOSSTARGET,
            192,
            192,
        );

        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);

        // awake: targets collected
        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            a_brain_awake(&mut ctx, brain);
        }
        assert_eq!(world.brain.targets_len(), 2);

        // spit twice: cubes fly at consecutive targets
        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            a_brain_spit(&mut ctx, brain);
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
            a_brain_spit(&mut ctx, brain);
        }

        let cubes: Vec<_> = store
            .thinkers()
            .filter_map(|(id, t)| match t {
                Thinker::Mobj(m) if m.mobj_type == MobjType::MT_SPAWNSHOT => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(cubes.len(), 2);

        // the cube targets alternate
        let cube_targets: Vec<_> = cubes
            .iter()
            .filter_map(|id| store.mobj(*id).and_then(|m| m.target))
            .collect();
        let set: std::collections::HashSet<_> = cube_targets.into_iter().collect();
        assert!(set.contains(&target_a));
        assert!(set.contains(&target_b));

        // both spits produced the ritual sound
        let spits = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::Sound {
                        sfx: SfxId::sfx_bospit,
                        actor: None
                    }
                )
            })
            .count();
        assert_eq!(spits, 2);
    }

    #[test]
    fn a_bfg_spray_damages_the_monster_ahead() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = crate::rng::DoomRng::new();
        let mut players = players();
        let player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 64, 64);

        // the player fired east-to-west: missile heading west with the player as originator
        set_mobj(&mut store, players[0].mo.unwrap(), |m| {
            m.angle = Angle::ANG180
        });
        let missile = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_BFG, 100, 64);
        set_mobj(&mut store, missile, |m| {
            m.target = Some(player);
            m.angle = Angle::ANG180;
        });

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
            a_bfg_spray(&mut ctx, missile);
        }

        let extra_bfgs = store
            .thinkers()
            .filter(|(_id, t)| match t {
                Thinker::Mobj(m) => m.mobj_type == MobjType::MT_EXTRABFG,
                _ => false,
            })
            .count();
        assert!(extra_bfgs >= 1, "expected at least one MT_EXTRABFG spawned");

        let damaged = events
            .iter()
            .any(|e| matches!(e, GameEvent::Damage { target, .. } if *target == monster));
        assert!(damaged, "expected the monster to take BFG spray damage");
    }

    #[test]
    fn a_spawn_fly_materializes_a_monster() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = crate::rng::DoomRng::new();
        let mut players = players();
        let _player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let target_spot = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_BOSSTARGET, 128, 64);

        // the flying cube, zero reaction time left: it bursts now
        let cube = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_SPAWNSHOT, 64, 64);
        set_mobj(&mut store, cube, |m| {
            m.target = Some(target_spot);
            m.reactiontime = 1; // will hit zero after the decrement
        });

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
            a_spawn_fly(&mut ctx, cube);
        }

        // teleport fog appeared at the spot
        let fog = store.thinkers().any(|(_id, t)| match t {
            Thinker::Mobj(m) => m.mobj_type == MobjType::MT_SPAWNFIRE,
            _ => false,
        });
        assert!(fog, "expected spawn fog");

        // a new monster was materialized
        let spawned_monster = store.thinkers().any(|(id, t)| match t {
            Thinker::Mobj(m) => {
                id != cube
                    && matches!(
                        m.mobj_type,
                        MobjType::MT_TROOP
                            | MobjType::MT_SERGEANT
                            | MobjType::MT_SHADOWS
                            | MobjType::MT_PAIN
                            | MobjType::MT_HEAD
                            | MobjType::MT_VILE
                            | MobjType::MT_UNDEAD
                            | MobjType::MT_BABY
                            | MobjType::MT_FATSO
                            | MobjType::MT_KNIGHT
                            | MobjType::MT_BRUISER
                    )
            }
            _ => false,
        });
        assert!(spawned_monster, "expected a freshly spawned monster");

        // the cube is gone
        assert!(store.mobj(cube).is_none());
    }

    #[test]
    fn a_tracer_only_tracks_every_fourth_tic() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = crate::rng::DoomRng::new();
        let mut players = players();
        let _player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let proj = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TRACER, 100, 100);
        set_mobj(&mut store, proj, |m| {
            m.tracer = Some(_player);
            m.angle = Angle::ANG90; // flying away from the player
            m.momx = Fixed::ZERO;
            m.momy = Fixed::from_raw(655360); // ~10 units/tic upwards
            m.momz = Fixed::ZERO;
        });

        // off-cadence tics change nothing
        let mut events = Vec::new();
        let mut world = TestWorld::new(&map);
        let momy_before = store.mobj(proj).map(|m| m.momy);
        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            ctx.leveltime = 1; // 1 & 3 != 0
            a_tracer(&mut ctx, proj);
        }
        assert_eq!(store.mobj(proj).map(|m| m.momy), momy_before);

        // on-cadence tics steer the rocket towards the player
        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut events,
            );
            ctx.leveltime = 4; // 4 & 3 == 0
            a_tracer(&mut ctx, proj);
        }
        let momy_after = store.mobj(proj).map(|m| m.momy).unwrap_or_default();
        assert!(
            momy_after != momy_before.unwrap_or_default(),
            "tracer should have re-aimed"
        );
    }
}
