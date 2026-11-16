//! The `A_*` weapon callbacks and the dispatcher that runs the actions a
//! psprite state chain produces, ported from `p_pspr.c` (plus the three
//! super-shotgun sounds from `p_enemy.c`, which are player weapon
//! actions in disguise).
//!
//! Every handler keeps the vanilla `(player, psp)` shape by taking
//! `player_index` and the overlay `position`; the weapon overlay is
//! `PspNumber::Weapon`, the muzzle flash is `PspNumber::Flash`. Sounds,
//! damage and state effects come out as plain-data events or store
//! mutations on the player's avatar, exactly like the monster layer.
//!
//! The dispatcher is a chain target for the gameplay layer: when
//! [`crate::enemy::dispatch_action`] returns `false` for a psprite
//! action, the caller lands here. Returns `false` for anything that is
//! not a weapon action (the gameplay layer owns those).

use crate::bsp::point_to_angle_2;
use crate::enemy::{
    EnemyContext, MELEERANGE, MISSILERANGE, aim_line_attack, line_attack,
    set_mobj_state_and_run_actions,
};
use crate::fixed::Fixed;
use crate::info::{MF_JUSTATTACKED, SfxId, StateAction, StateId};
use crate::menu::GameMode;
use crate::player::{PlayerState, Power, PspNumber, Weapon};
use crate::tables::{Angle, FINE_ANGLES, FINE_COSINE, FINE_MASK, FINE_SINE};

use super::{
    ANG90_DIV_20, ANG90_DIV_21, BFGCELLS, BT_ATTACK, LOWERSPEED, MELEERANGE_PLUS_1, RAISESPEED,
    WEAPON_INFO, WEAPONBOTTOM, WEAPONTOP, bring_up_weapon, bullet_slope, check_ammo,
    emit_player_sound, emit_sound, fire_weapon, flash_with_offset, gun_shot, ready_ammo,
    set_psprite, spawn_player_missile, spend_ammo,
};

/// Vanilla `A_WeaponReady`.
///
/// The player can fire the weapon or change to another weapon at this
/// time. Follows after getting the weapon up, or after the previous
/// attack/fire sequence.
pub fn a_weapon_ready(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    position: PspNumber,
    game_mode: GameMode,
) {
    // get out of attack state
    if let Some(mo) = ctx.players[player_index].mo {
        let current = ctx.store.mobj(mo).map(|m| m.state);
        if current == Some(StateId::S_PLAY_ATK1) || current == Some(StateId::S_PLAY_ATK2) {
            set_mobj_state_and_run_actions(ctx, mo, StateId::S_PLAY);
        }
    }

    if ctx.players[player_index].readyweapon == Weapon::Chainsaw {
        let sawing =
            ctx.players[player_index].psprites[position.index()].state == Some(StateId::S_SAW);
        if sawing {
            emit_player_sound(ctx, player_index, SfxId::sfx_sawidl);
        }
    }

    // check for change — if the player is dead, put the weapon away
    if ctx.players[player_index].pendingweapon.is_some() || ctx.players[player_index].health == 0 {
        // (pending weapon should already be validated)
        let ready = ctx.players[player_index].readyweapon;
        let newstate = WEAPON_INFO[ready.index()].downstate;
        set_psprite(ctx, player_index, position, newstate, game_mode);
        return;
    }

    // check for fire — the missile launcher and the bfg do not auto fire
    if ctx.players[player_index].cmd.buttons & BT_ATTACK != 0 {
        let ready = ctx.players[player_index].readyweapon;
        let no_missile_bfg = ready != Weapon::MissileLauncher && ready != Weapon::Bfg;
        if !ctx.players[player_index].attackdown || no_missile_bfg {
            ctx.players[player_index].attackdown = true;
            fire_weapon(ctx, player_index, game_mode);
            return;
        }
    } else {
        ctx.players[player_index].attackdown = false;
    }

    // bob the weapon based on movement speed
    let angle = ((128 * ctx.leveltime) & FINE_MASK as i32) as usize;
    let bob = ctx.players[player_index].bob;
    let sx = Fixed::ONE + bob * FINE_COSINE[angle];

    let angle = angle & (FINE_ANGLES / 2 - 1);
    let sy = WEAPONTOP + bob * FINE_SINE[angle];
    let psp = &mut ctx.players[player_index].psprites[position.index()];
    psp.sx = sx;
    psp.sy = sy;
}

/// Vanilla `A_ReFire`.
///
/// The player can re-fire the weapon without lowering it entirely.
pub fn a_refire(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    _position: PspNumber,
    game_mode: GameMode,
) {
    // check for fire — a weapon change in flight wins over refiring
    let wants_fire = ctx.players[player_index].cmd.buttons & BT_ATTACK != 0;
    if wants_fire
        && ctx.players[player_index].pendingweapon.is_none()
        && ctx.players[player_index].health != 0
    {
        ctx.players[player_index].refire += 1;
        fire_weapon(ctx, player_index, game_mode);
    } else {
        ctx.players[player_index].refire = 0;
        let _ = check_ammo(ctx, player_index, game_mode);
    }
}

/// Vanilla `A_CheckReload`.
///
/// Out of ammo? `P_CheckAmmo` picks the next weapon instead.
pub fn a_check_reload(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    _position: PspNumber,
    game_mode: GameMode,
) {
    let _ = check_ammo(ctx, player_index, game_mode);
}

/// Vanilla `A_Lower`.
///
/// Lowers the current weapon, and changes the weapon at the bottom.
pub fn a_lower(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    position: PspNumber,
    game_mode: GameMode,
) {
    ctx.players[player_index].psprites[position.index()].sy += LOWERSPEED;

    // is already down?
    if ctx.players[player_index].psprites[position.index()].sy < WEAPONBOTTOM {
        return;
    }

    // player is dead — don't bring the weapon back up
    if ctx.players[player_index].playerstate == PlayerState::Dead {
        ctx.players[player_index].psprites[position.index()].sy = WEAPONBOTTOM;
        return;
    }

    // the old weapon has been lowered off the screen,
    // so change the weapon and start raising it
    if ctx.players[player_index].health == 0 {
        set_psprite(ctx, player_index, position, StateId::S_NULL, game_mode);
        return;
    }

    match ctx.players[player_index].pendingweapon {
        Some(pending) => ctx.players[player_index].readyweapon = pending,
        // vanilla would read `wp_nochange` here and the raise below
        // would arm the current weapon again — keep the weapon instead
        None => tracing::warn!("A_Lower: no pending weapon; keeping the current one"),
    }

    bring_up_weapon(ctx, player_index, game_mode);
}

/// Vanilla `A_Raise`.
///
/// Raises the current weapon from the bottom of the screen; settles in
/// the ready state at the top.
pub fn a_raise(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    position: PspNumber,
    game_mode: GameMode,
) {
    ctx.players[player_index].psprites[position.index()].sy -= RAISESPEED;

    if ctx.players[player_index].psprites[position.index()].sy > WEAPONTOP {
        return;
    }

    ctx.players[player_index].psprites[position.index()].sy = WEAPONTOP;

    // the weapon has been raised all the way, so change to the ready state
    let ready = ctx.players[player_index].readyweapon;
    let newstate = WEAPON_INFO[ready.index()].readystate;
    set_psprite(ctx, player_index, position, newstate, game_mode);
}

/// Vanilla `A_GunFlash`.
///
/// Bumps the avatar into the attack frame and arms the muzzle flash
/// overlay of the current weapon.
pub fn a_gun_flash(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    _position: PspNumber,
    game_mode: GameMode,
) {
    if let Some(mo) = ctx.players[player_index].mo {
        set_mobj_state_and_run_actions(ctx, mo, StateId::S_PLAY_ATK2);
    }
    let ready = ctx.players[player_index].readyweapon;
    let flashstate = WEAPON_INFO[ready.index()].flashstate;
    set_psprite(ctx, player_index, PspNumber::Flash, flashstate, game_mode);
}

// ---------------------------------------------------------------------
// WEAPON ATTACKS
// ---------------------------------------------------------------------

/// Vanilla `A_Punch`.
///
/// Melee swing; fist damage is `(P_Random()%10+1)<<1`, times ten when
/// the player picked up a berserk pack (`pw_strength`).
pub fn a_punch(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    _position: PspNumber,
    _game_mode: GameMode,
) {
    let mut damage = ((ctx.rng.next() % 10 + 1) << 1) as i32;
    if ctx.players[player_index].powers[Power::Strength as usize] != 0 {
        damage *= 10;
    }

    let Some(mo) = ctx.players[player_index].mo else {
        return;
    };
    let Some(m) = ctx.store.mobj(mo) else {
        return;
    };
    let angle =
        m.angle + Angle::from_raw(((ctx.rng.next() as i32 - ctx.rng.next() as i32) << 18) as u32);

    let aim = aim_line_attack(ctx, mo, angle, MELEERANGE);
    line_attack(ctx, mo, angle, MELEERANGE, aim.slope, damage);

    // turn to face target
    if let Some(target) = aim.target {
        emit_sound(ctx, Some(mo), SfxId::sfx_punch);
        if let (Some(m), Some(t)) = (ctx.store.mobj(mo), ctx.store.mobj(target)) {
            let face = point_to_angle_2(m.x, m.y, t.x, t.y);
            if let Some(m) = ctx.store.mobj_mut(mo) {
                m.angle = face;
            }
        }
    }
}

/// Vanilla `A_Saw`.
///
/// The chainsaw: a short-range hitscan with saw damage and a snapped
/// turn toward whatever it hit. Uses `MELEERANGE + 1` so the puff
/// doesn't skip the swing flash (one raw fixed unit, not one unit).
pub fn a_saw(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    _position: PspNumber,
    _game_mode: GameMode,
) {
    let damage = 2 * (ctx.rng.next() % 10 + 1) as i32;
    let Some(mo) = ctx.players[player_index].mo else {
        return;
    };
    let Some(m) = ctx.store.mobj(mo) else {
        return;
    };
    let angle =
        m.angle + Angle::from_raw(((ctx.rng.next() as i32 - ctx.rng.next() as i32) << 18) as u32);

    let aim = aim_line_attack(ctx, mo, angle, MELEERANGE_PLUS_1);
    line_attack(ctx, mo, angle, MELEERANGE_PLUS_1, aim.slope, damage);

    let Some(target) = aim.target else {
        emit_sound(ctx, Some(mo), SfxId::sfx_sawful);
        return;
    };
    emit_sound(ctx, Some(mo), SfxId::sfx_sawhit);

    // turn to face target
    let Some((fx, fy, tx, ty)) = ctx
        .store
        .mobj(mo)
        .and_then(|m| ctx.store.mobj(target).map(|t| (m.x, m.y, t.x, t.y)))
    else {
        return;
    };
    let face = point_to_angle_2(fx, fy, tx, ty);

    if let Some(m) = ctx.store.mobj_mut(mo) {
        let cur = m.angle.to_raw();
        let raw = face.to_raw();
        let diff = raw.wrapping_sub(cur) as i32;
        // vanilla compares unsigned diff > ANG180; the signed cast is the same bit pattern
        if diff < 0 {
            if diff > -(ANG90_DIV_20 as i32) {
                m.angle = Angle::from_raw(raw.wrapping_add(ANG90_DIV_21));
            } else {
                m.angle = Angle::from_raw(cur.wrapping_sub(ANG90_DIV_20));
            }
        } else {
            if diff > ANG90_DIV_20 as i32 {
                m.angle = Angle::from_raw(raw.wrapping_sub(ANG90_DIV_21));
            } else {
                m.angle = Angle::from_raw(cur.wrapping_add(ANG90_DIV_20));
            }
        }
        m.flags |= MF_JUSTATTACKED;
    }
}

/// Vanilla `A_FireMissile` — rocket launcher.
pub fn a_fire_missile(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    _position: PspNumber,
    _game_mode: GameMode,
) {
    spend_ammo(ctx, player_index, 1);
    let Some(mo) = ctx.players[player_index].mo else {
        return;
    };
    spawn_player_missile(ctx, mo, crate::info::MobjType::MT_ROCKET);
}

/// Vanilla `A_FireBFG` — BFG 9000.
pub fn a_fire_bfg(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    _position: PspNumber,
    _game_mode: GameMode,
) {
    spend_ammo(ctx, player_index, BFGCELLS);
    let Some(mo) = ctx.players[player_index].mo else {
        return;
    };
    spawn_player_missile(ctx, mo, crate::info::MobjType::MT_BFG);
}

/// Vanilla `A_FirePlasma` — plasma rifle.
pub fn a_fire_plasma(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    _position: PspNumber,
    game_mode: GameMode,
) {
    spend_ammo(ctx, player_index, 1);

    let ready = ctx.players[player_index].readyweapon;
    let flashstate = WEAPON_INFO[ready.index()].flashstate;
    // alternate the flash frame per shot
    let flash = flash_with_offset(flashstate, (ctx.rng.next() & 1) as u16);
    set_psprite(ctx, player_index, PspNumber::Flash, flash, game_mode);

    let Some(mo) = ctx.players[player_index].mo else {
        return;
    };
    spawn_player_missile(ctx, mo, crate::info::MobjType::MT_PLASMA);
}

/// Vanilla `A_FirePistol` — pistol.
pub fn a_fire_pistol(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    _position: PspNumber,
    game_mode: GameMode,
) {
    emit_player_sound(ctx, player_index, SfxId::sfx_pistol);

    if let Some(mo) = ctx.players[player_index].mo {
        set_mobj_state_and_run_actions(ctx, mo, StateId::S_PLAY_ATK2);
    }

    spend_ammo(ctx, player_index, 1);

    let ready = ctx.players[player_index].readyweapon;
    let flashstate = WEAPON_INFO[ready.index()].flashstate;
    set_psprite(ctx, player_index, PspNumber::Flash, flashstate, game_mode);

    let Some(mo) = ctx.players[player_index].mo else {
        return;
    };
    let slope = bullet_slope(ctx, mo);
    let accurate = ctx.players[player_index].refire == 0;
    gun_shot(ctx, mo, accurate, slope);
}

/// Vanilla `A_FireShotgun` — shotgun: seven pellets, all spread.
pub fn a_fire_shotgun(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    _position: PspNumber,
    game_mode: GameMode,
) {
    emit_player_sound(ctx, player_index, SfxId::sfx_shotgn);

    if let Some(mo) = ctx.players[player_index].mo {
        set_mobj_state_and_run_actions(ctx, mo, StateId::S_PLAY_ATK2);
    }

    spend_ammo(ctx, player_index, 1);

    let ready = ctx.players[player_index].readyweapon;
    let flashstate = WEAPON_INFO[ready.index()].flashstate;
    set_psprite(ctx, player_index, PspNumber::Flash, flashstate, game_mode);

    let Some(mo) = ctx.players[player_index].mo else {
        return;
    };
    let slope = bullet_slope(ctx, mo);
    for _ in 0..7 {
        gun_shot(ctx, mo, false, slope);
    }
}

/// Vanilla `A_FireShotgun2` — super shotgun: twenty pellets, double
/// damage spread.
pub fn a_fire_shotgun2(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    _position: PspNumber,
    game_mode: GameMode,
) {
    emit_player_sound(ctx, player_index, SfxId::sfx_dshtgn);

    if let Some(mo) = ctx.players[player_index].mo {
        set_mobj_state_and_run_actions(ctx, mo, StateId::S_PLAY_ATK2);
    }

    spend_ammo(ctx, player_index, 2);

    let ready = ctx.players[player_index].readyweapon;
    let flashstate = WEAPON_INFO[ready.index()].flashstate;
    set_psprite(ctx, player_index, PspNumber::Flash, flashstate, game_mode);

    let Some(mo) = ctx.players[player_index].mo else {
        return;
    };
    let slope = bullet_slope(ctx, mo);
    for _ in 0..20 {
        let Some(m) = ctx.store.mobj(mo) else {
            break;
        };
        let angle = m.angle
            + Angle::from_raw(((ctx.rng.next() as i32 - ctx.rng.next() as i32) << 19) as u32);
        let damage = 5 * (ctx.rng.next() % 3 + 1) as i32;
        let jittered = Fixed::from_raw(
            slope
                .to_raw()
                .wrapping_add((ctx.rng.next() as i32 - ctx.rng.next() as i32) << 5),
        );
        line_attack(ctx, mo, angle, MISSILERANGE, jittered, damage);
    }
}

/// Vanilla `A_FireCGun` — chaingun.
///
/// Rapid fire with a per-frame flash twin (`S_CHAINFLASH1`/`S_CHAINFLASH2`).
pub fn a_fire_cgun(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    position: PspNumber,
    game_mode: GameMode,
) {
    emit_player_sound(ctx, player_index, SfxId::sfx_pistol);

    if ready_ammo(&ctx.players[player_index]).unwrap_or(0) == 0 {
        // out of ammo: sound, no shot
        return;
    }

    if let Some(mo) = ctx.players[player_index].mo {
        set_mobj_state_and_run_actions(ctx, mo, StateId::S_PLAY_ATK2);
    }

    spend_ammo(ctx, player_index, 1);

    // the flash tracks the chain frame it fired from
    let ready = ctx.players[player_index].readyweapon;
    let offset = match ctx.players[player_index].psprites[position.index()].state {
        Some(StateId::S_CHAIN2) => 1,
        _ => 0,
    };
    let flash = flash_with_offset(WEAPON_INFO[ready.index()].flashstate, offset);
    set_psprite(ctx, player_index, PspNumber::Flash, flash, game_mode);

    let Some(mo) = ctx.players[player_index].mo else {
        return;
    };
    let slope = bullet_slope(ctx, mo);
    let accurate = ctx.players[player_index].refire == 0;
    gun_shot(ctx, mo, accurate, slope);
}

/// Vanilla `A_Light0` — gun flash light boost off.
pub fn a_light0(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    _position: PspNumber,
    _game_mode: GameMode,
) {
    ctx.players[player_index].extralight = 0;
}

/// Vanilla `A_Light1` — gun flash light boost 1.
pub fn a_light1(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    _position: PspNumber,
    _game_mode: GameMode,
) {
    ctx.players[player_index].extralight = 1;
}

/// Vanilla `A_Light2` — gun flash light boost 2.
pub fn a_light2(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    _position: PspNumber,
    _game_mode: GameMode,
) {
    ctx.players[player_index].extralight = 2;
}

/// Vanilla `A_BFGsound` — the bfg charge growl.
pub fn a_bfg_sound(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    _position: PspNumber,
    _game_mode: GameMode,
) {
    emit_player_sound(ctx, player_index, SfxId::sfx_bfg);
}

/// Vanilla `A_OpenShotgun2` — super shotgun break-action open clack.
pub fn a_open_shotgun2(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    _position: PspNumber,
    _game_mode: GameMode,
) {
    emit_player_sound(ctx, player_index, SfxId::sfx_dbopn);
}

/// Vanilla `A_LoadShotgun2` — super shotgun shell load clack.
pub fn a_load_shotgun2(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    _position: PspNumber,
    _game_mode: GameMode,
) {
    emit_player_sound(ctx, player_index, SfxId::sfx_dbload);
}

/// Vanilla `A_CloseShotgun2` — super shotgun break-action close clack,
/// then a `A_ReFire` check.
pub fn a_close_shotgun2(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    position: PspNumber,
    game_mode: GameMode,
) {
    emit_player_sound(ctx, player_index, SfxId::sfx_dbcls);
    a_refire(ctx, player_index, position, game_mode);
}

/// Dispatches one weapon `A_*` action.
///
/// Returns `true` when the action was handled. `false` for actions the
/// psprite layer does not own (`A_BFGSpray` is the monster layer's;
/// `NoAction` never reaches here because the chain skips it).
pub fn dispatch_weapon_action(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    position: PspNumber,
    action: StateAction,
    game_mode: GameMode,
) -> bool {
    match action {
        StateAction::A_WeaponReady => a_weapon_ready(ctx, player_index, position, game_mode),
        StateAction::A_Lower => a_lower(ctx, player_index, position, game_mode),
        StateAction::A_Raise => a_raise(ctx, player_index, position, game_mode),
        StateAction::A_Punch => a_punch(ctx, player_index, position, game_mode),
        StateAction::A_ReFire => a_refire(ctx, player_index, position, game_mode),
        StateAction::A_FirePistol => a_fire_pistol(ctx, player_index, position, game_mode),
        StateAction::A_FireShotgun => a_fire_shotgun(ctx, player_index, position, game_mode),
        StateAction::A_FireShotgun2 => a_fire_shotgun2(ctx, player_index, position, game_mode),
        StateAction::A_CheckReload => a_check_reload(ctx, player_index, position, game_mode),
        StateAction::A_OpenShotgun2 => a_open_shotgun2(ctx, player_index, position, game_mode),
        StateAction::A_LoadShotgun2 => a_load_shotgun2(ctx, player_index, position, game_mode),
        StateAction::A_CloseShotgun2 => a_close_shotgun2(ctx, player_index, position, game_mode),
        StateAction::A_FireCGun => a_fire_cgun(ctx, player_index, position, game_mode),
        StateAction::A_GunFlash => a_gun_flash(ctx, player_index, position, game_mode),
        StateAction::A_FireMissile => a_fire_missile(ctx, player_index, position, game_mode),
        StateAction::A_Saw => a_saw(ctx, player_index, position, game_mode),
        StateAction::A_FirePlasma => a_fire_plasma(ctx, player_index, position, game_mode),
        StateAction::A_BFGsound => a_bfg_sound(ctx, player_index, position, game_mode),
        StateAction::A_FireBFG => a_fire_bfg(ctx, player_index, position, game_mode),
        StateAction::A_Light0 => a_light0(ctx, player_index, position, game_mode),
        StateAction::A_Light1 => a_light1(ctx, player_index, position, game_mode),
        StateAction::A_Light2 => a_light2(ctx, player_index, position, game_mode),
        _ => return false,
    }
    true
}
