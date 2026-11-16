//! The player weapons: the psprite state machines, the ammo bookkeeping
//! and the `A_*` callbacks they hang off, ported from `p_pspr.c`
//! (plus `d_items.c`'s `weaponinfo[]`, which is what drives the machine,
//! and `p_mobj.c`'s `P_SpawnPlayerMissile`, which the spawner weapons
//! fire through).
//!
//! Scope of this module, one-to-one with the original:
//!
//! | vanilla source               | where it lives                     |
//! |------------------------------|------------------------------------|
//! | `P_SetPsprite` / `P_MovePsprites` / `P_SetupPsprites` / `P_BringUpWeapon` / `P_DropWeapon` | this module |
//! | `P_CheckAmmo` / `P_FireWeapon` | [`check_ammo`], [`fire_weapon`]  |
//! | `P_CalcSwing`                 | [`calc_swing`]                     |
//! | `P_BulletSlope` / `P_GunShot` / `P_SpawnPlayerMissile` | [`spawn_player_missile`] (bullet slope is a helper there) |
//! | `weaponinfo[]` (`d_items.c`) | [`WEAPON_INFO`]                    |
//! | `A_WeaponReady`, `A_Lower`, `A_Raise`, `A_GunFlash`, `A_Punch`, `A_Saw`, `A_FirePistol`, `A_FireShotgun`, `A_FireShotgun2`, `A_FireCGun`, `A_FireMissile`, `A_FirePlasma`, `A_FireBFG`, `A_ReFire`, `A_CheckReload`, `A_Light0..2`, `A_BFGsound`, `A_OpenShotgun2`, `A_LoadShotgun2`, `A_CloseShotgun2` | [`actions`] |
//!
//! The `A_*` bodies keep their vanilla names lowercased (`a_fire_pistol`,
//! ...) for greppability, same convention as the monster layer.
//!
//! ## Player state and borrows
//!
//! The original threads `player_t*` everywhere. The port keeps that
//! shape but the players live inside [`EnemyContext`] (the one world
//! context the gameplay layer builds), so every entry point takes a
//! `player_index` into `ctx.players` instead of a raw pointer. The
//! overlay sprites are [`crate::player::PspDef`] slots on the player
//! (`psprites[PSP_WEAPON]`, `psprites[PSP_FLASH]` in vanilla).
//!
//! `Pendingweapon` is modelled as `Option<Weapon>` on the player (`None`
//! is the vanilla `wp_nochange` sentinel); `Readyweapon` is a plain
//! `Weapon`.
//!
//! ## Vanilla globals that moved
//!
//! | vanilla global      | where it lives now                |
//! |---------------------|-----------------------------------|
//! | `bulletslope`/`linetarget` | the [`AimOutcome`] that `aim_line_attack` returns; `bullet_slope` threads it by value |
//! | `swingx`/`swingy`   | [`calc_swing`] returns the pair   |
//! | `LOWERSPEED`/`RAISESPEED`/`WEAPONBOTTOM`/`WEAPONTOP`/`BFGCELLS` | the matching constants here |
//! | `gamemode`          | `game_mode: GameMode` (from `crate::menu`) passed explicitly to everything that branches on it — same pattern as the intermission layer |
//!
//! ## `misc1`/`misc2` (dropped)
//!
//! Vanilla `P_SetPsprite` snaps `sx`/`sy` to `state->misc1 << FRACBITS`
//! when the `state->misc1` field is non-zero. The `states[]` table in
//! `linuxdoom-1.10` never sets those fields (every row ends in
//! `,0,0},`), so the branch is dead in the original and is not ported.
//! The fields themselves are not in the Rust `State` table either.
//!
//! ## `weapons without a flash`
//!
//! The fist and the chainsaw carry `S_NULL` as their `flashstate`; the
//! flash overlay is simply never armed for them, which is what
//! `S_NULL` meant to `P_SetPsprite` anyway (it clears the sprite).
//!
//! ## State chain deferral
//!
//! `P_SetPsprite` runs the actions of a zero-tic state chain inline in
//! the original, and the outer loop re-reads `psp->state->nextstate`
//! after each action — so an action that swaps the overlay (e.g.
//! `A_ReFire` firing a new attack state) feeds the *next* iteration of
//! the loop. The port applies the zero-tic part of the chain atomically
//! and dispatches the actions afterwards, the same deferral the mobj
//! layer uses (see [`crate::mobj`] module docs for why no
//! `states[]`-chain behavior changes). Zero-tic weapon states
//! (`S_LIGHTDONE`, `S_SAW3`, `S_CHAIN3`, `S_MISSILE3`) only fire
//! `A_Light0`/`A_ReFire`, which neither reads nor writes the overlay
//! state in a way the deferral can observe.
//!
//! The apply loop is fenced with an explicit iteration cap; the vanilla
//! table has no zero-tic cycles, so the cap is unreachable for shipped
//! data and turns a corrupt table into a loud panic instead of an
//! infinite recursion.
//!
//! ## `S_DSWUP` — a note on names
//!
//! There is no `S_DSWUP` in `linuxdoom-1.10`. The DOOM II super
//! shotgun's states are `S_DSGUN*` (up `S_DSGUNUP`, down
//! `S_DSGUNDOWN`, ready `S_DSGUN`, fire `S_DSGUN1..2`, reload
//! `S_DSGUN4..9`, flash `S_DSGUNFLASH1..2`) plus the unused
//! `S_DSNR1`/`S_DSNR2` "silent" rows that nothing references. All of
//! them are in the Rust `states[]` table; the module ports the behavior,
//! not the dead rows.
//!
//! ## RNG
//!
//! [`crate::rng::DoomRng`] is consumed call-for-call where the original
//! consumed `P_Random()`. The order matters for demo replay.

mod actions;

pub use actions::{
    a_bfg_sound, a_check_reload, a_close_shotgun2, a_fire_bfg, a_fire_cgun, a_fire_missile,
    a_fire_pistol, a_fire_plasma, a_fire_shotgun, a_fire_shotgun2, a_gun_flash, a_light0, a_light1,
    a_light2, a_load_shotgun2, a_lower, a_open_shotgun2, a_punch, a_raise, a_refire, a_saw,
    a_weapon_ready, dispatch_weapon_action,
};

use crate::enemy::{
    EnemyContext, GameEvent, MISSILERANGE, aim_line_attack, check_missile_spawn, line_attack,
    noise_alert, set_mobj_state_and_run_actions,
};
use crate::fixed::{FRACUNIT, Fixed};
use crate::info::{MOBJ_INFO, MobjType, STATES, SfxId, StateAction, StateId};
use crate::menu::GameMode;
use crate::player::{AmmoKind, NUM_AMMO, NUM_WEAPONS, Player, Power, PspNumber, Weapon};
use crate::render::draw_psprite;
use crate::render::{FuzzState, IndexedFramebuffer, SpriteDefinitions, SpriteLumpTables, WallView};
use crate::resources::ResourceManager;
use crate::tables::{Angle, FINE_ANGLES, FINE_COSINE, FINE_MASK, FINE_SINE};
use crate::thinker::ThinkerId;

/// Vanilla `LOWERSPEED` (`p_pspr.c`).
pub const LOWERSPEED: Fixed = Fixed::from_raw(FRACUNIT * 6);
/// Vanilla `RAISESPEED` (`p_pspr.c`).
pub const RAISESPEED: Fixed = Fixed::from_raw(FRACUNIT * 6);
/// Vanilla `WEAPONBOTTOM` (`p_pspr.c`): overlay Y when the weapon is fully lowered.
pub const WEAPONBOTTOM: Fixed = Fixed::from_raw(128 * FRACUNIT);
/// Vanilla `WEAPONTOP` (`p_pspr.c`): overlay Y when the weapon is fully raised.
pub const WEAPONTOP: Fixed = Fixed::from_raw(32 * FRACUNIT);
/// Vanilla `BFGCELLS` (`p_pspr.c`): cells the BFG consumes per shot.
pub const BFGCELLS: i32 = 40;

/// Vanilla `BT_ATTACK` (`d_event.h`): the attack button bit in `ticcmd_t.buttons`.
pub const BT_ATTACK: u8 = 1;

/// Vanilla `MELEERANGE + 1` from `A_Saw` — the +1 keeps the "don't skip
/// the flash" puff in range; it is one *raw fixed unit*, not one whole
/// unit.
const MELEERANGE_PLUS_1: Fixed = Fixed::from_raw(64 * FRACUNIT + 1);

/// Vanilla `ANG90/20` (int division of the BAM constant, exactly how the C code does it).
const ANG90_DIV_20: u32 = Angle::ANG90.to_raw() / 20;
/// Vanilla `ANG90/21`.
const ANG90_DIV_21: u32 = Angle::ANG90.to_raw() / 21;
/// Vanilla `1 << 26` aim elevation sweep step (`P_SpawnPlayerMissile`, `P_BulletSlope`).
const AIM_STEP: u32 = 1 << 26;

/// Vanilla `AIM_RANGE` is `16*64*FRACUNIT` (the `P_AimLineAttack` tracing
/// distance for bullets and player missiles).
const BULLET_AIM_RANGE: Fixed = Fixed::from_raw(16 * 64 * FRACUNIT);

/// One row of the vanilla `weaponinfo[]` (`d_items.c`).
///
/// `flashstate` is `S_NULL` for weapons that never arm the flash overlay
/// (fist, chainsaw). `ammo` is `None` for `am_noammo`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WeaponInfo {
    /// Ammunition kind this weapon consumes; `None` for bare hands.
    pub ammo: Option<AmmoKind>,
    /// State entered when the weapon is raised from the bottom of the screen.
    pub upstate: StateId,
    /// State entered while the weapon lowers off the screen.
    pub downstate: StateId,
    /// State the weapon rests in while waiting for input.
    pub readystate: StateId,
    /// State holding the attack animation.
    pub atkstate: StateId,
    /// Overlay muzzle flash state; `S_NULL` disarms the flash sprite.
    pub flashstate: StateId,
}

/// Vanilla `weaponinfo[]` (`d_items.c`).
pub static WEAPON_INFO: [WeaponInfo; NUM_WEAPONS] = [
    // fist
    WeaponInfo {
        ammo: None,
        upstate: StateId::S_PUNCHUP,
        downstate: StateId::S_PUNCHDOWN,
        readystate: StateId::S_PUNCH,
        atkstate: StateId::S_PUNCH1,
        flashstate: StateId::S_NULL,
    },
    // pistol
    WeaponInfo {
        ammo: Some(AmmoKind::Clip),
        upstate: StateId::S_PISTOLUP,
        downstate: StateId::S_PISTOLDOWN,
        readystate: StateId::S_PISTOL,
        atkstate: StateId::S_PISTOL1,
        flashstate: StateId::S_PISTOLFLASH,
    },
    // shotgun
    WeaponInfo {
        ammo: Some(AmmoKind::Shell),
        upstate: StateId::S_SGUNUP,
        downstate: StateId::S_SGUNDOWN,
        readystate: StateId::S_SGUN,
        atkstate: StateId::S_SGUN1,
        flashstate: StateId::S_SGUNFLASH1,
    },
    // chaingun
    WeaponInfo {
        ammo: Some(AmmoKind::Clip),
        upstate: StateId::S_CHAINUP,
        downstate: StateId::S_CHAINDOWN,
        readystate: StateId::S_CHAIN,
        atkstate: StateId::S_CHAIN1,
        flashstate: StateId::S_CHAINFLASH1,
    },
    // missile launcher
    WeaponInfo {
        ammo: Some(AmmoKind::Missile),
        upstate: StateId::S_MISSILEUP,
        downstate: StateId::S_MISSILEDOWN,
        readystate: StateId::S_MISSILE,
        atkstate: StateId::S_MISSILE1,
        flashstate: StateId::S_MISSILEFLASH1,
    },
    // plasma rifle
    WeaponInfo {
        ammo: Some(AmmoKind::Cell),
        upstate: StateId::S_PLASMAUP,
        downstate: StateId::S_PLASMADOWN,
        readystate: StateId::S_PLASMA,
        atkstate: StateId::S_PLASMA1,
        flashstate: StateId::S_PLASMAFLASH1,
    },
    // bfg 9000
    WeaponInfo {
        ammo: Some(AmmoKind::Cell),
        upstate: StateId::S_BFGUP,
        downstate: StateId::S_BFGDOWN,
        readystate: StateId::S_BFG,
        atkstate: StateId::S_BFG1,
        flashstate: StateId::S_BFGFLASH1,
    },
    // chainsaw
    WeaponInfo {
        ammo: None,
        upstate: StateId::S_SAWUP,
        downstate: StateId::S_SAWDOWN,
        readystate: StateId::S_SAW,
        atkstate: StateId::S_SAW1,
        flashstate: StateId::S_NULL,
    },
    // super shotgun
    WeaponInfo {
        ammo: Some(AmmoKind::Shell),
        upstate: StateId::S_DSGUNUP,
        downstate: StateId::S_DSGUNDOWN,
        readystate: StateId::S_DSGUN,
        atkstate: StateId::S_DSGUN1,
        flashstate: StateId::S_DSGUNFLASH1,
    },
];

/// The number of ammunition kinds the overlay spends; re-exported for
/// callers that index `player.ammo` directly.
pub const AMMO_COUNT: usize = NUM_AMMO;

/// Vanilla `S_CHAINFLASH1`/`S_CHAINFLASH2` and
/// `S_PLASMAFLASH1`/`S_PLASMAFLASH2` are consecutive in `states[]` and
/// this is the only thing `A_FireCGun` and `A_FirePlasma` ever need, so
/// the flash arithmetic stays inside two explicitly matched pairs.
/// Anything else returns `base`.
fn flash_with_offset(base: StateId, offset: u16) -> StateId {
    match (base, offset) {
        (StateId::S_CHAINFLASH1, 1) => StateId::S_CHAINFLASH2,
        (StateId::S_PLASMAFLASH1, 1) => StateId::S_PLASMAFLASH2,
        _ => base,
    }
}

/// Vanilla `S_StartSound(mo, sfx)` as an event.
pub(crate) fn emit_sound(ctx: &mut EnemyContext<'_>, actor: Option<ThinkerId>, sfx: SfxId) {
    ctx.events.push(GameEvent::Sound { actor, sfx });
}

/// A sound on the player's avatar; a player without an avatar is a game
/// bug, but firing a weapon must not panic.
pub(crate) fn emit_player_sound(ctx: &mut EnemyContext<'_>, player_index: usize, sfx: SfxId) {
    match ctx.players[player_index].mo {
        Some(mo) => emit_sound(ctx, Some(mo), sfx),
        None => tracing::warn!("weapon sound with no avatar for player {player_index}"),
    }
}

/// Spend `amount` of the ready weapon's ammo kind. Weapons without an
/// ammo kind (fist, chainsaw) spend nothing. Saturates at zero — no
/// legal fire path drives ammo negative, and a saturating decrement
/// keeps direct state-machine tests honest.
pub(crate) fn spend_ammo(ctx: &mut EnemyContext<'_>, player_index: usize, amount: i32) {
    let ready = ctx.players[player_index].readyweapon;
    let Some(kind) = WEAPON_INFO[ready.index()].ammo else {
        return;
    };
    let cell = &mut ctx.players[player_index].ammo[kind.index()];
    *cell = (*cell - amount).max(0);
}

/// The ready weapon's current ammo count.
pub(crate) fn ready_ammo(player: &Player) -> Option<i32> {
    WEAPON_INFO[player.readyweapon.index()]
        .ammo
        .map(|kind| player.ammo[kind.index()])
}

/// Vanilla `P_SetPsprite`.
///
/// Applies the zero-tic part of the state chain atomically, then
/// dispatches the actions the chain would have invoked, in order —
/// see the module docs for why the deferral is behavior-neutral.
/// `StateId::S_NULL` removes the sprite (the vanilla `stnum == 0` case).
pub fn set_psprite(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    position: PspNumber,
    state_id: StateId,
    game_mode: GameMode,
) {
    let actions = apply_psprite(ctx, player_index, position, state_id);
    for action in actions {
        dispatch_weapon_action(ctx, player_index, position, action, game_mode);
    }
}

/// Vanilla `P_SetPsprite` without the action dispatch — applies the
/// state row and follows the zero-tic chain, collecting the actions.
fn apply_psprite(
    ctx: &mut EnemyContext<'_>,
    player_index: usize,
    position: PspNumber,
    mut state_id: StateId,
) -> Vec<StateAction> {
    let mut actions = Vec::new();
    // the vanilla table has no zero-tic cycles; the fence turns a
    // corrupt one into a panic instead of an infinite loop
    const MAX_STATE_CHAIN: usize = 64;
    for _ in 0..MAX_STATE_CHAIN {
        if state_id == StateId::S_NULL {
            // sprite removed itself
            ctx.players[player_index].psprites[position.index()].state = None;
            return actions;
        }
        let state = STATES[state_id as usize];
        {
            let psp = &mut ctx.players[player_index].psprites[position.index()];
            psp.state = Some(state_id);
            psp.tics = i32::from(state.tics);
        }
        // (vanilla snaps sx/sy from state->misc1/2 here; the table never
        //  sets them, see the module docs)
        if state.action != StateAction::NoAction {
            actions.push(state.action);
        }
        if state.tics != 0 {
            return actions;
        }
        state_id = state.next;
    }
    panic!("psprite state chain exceeded {MAX_STATE_CHAIN} zero-tic transitions");
}

/// Vanilla `P_SetupPsprites`. Called once per level, per player.
///
/// Removes every overlay sprite and re-arms the pending weapon.
pub fn setup_psprites(ctx: &mut EnemyContext<'_>, player_index: usize, game_mode: GameMode) {
    for psp in &mut ctx.players[player_index].psprites {
        psp.state = None;
    }

    let ready = ctx.players[player_index].readyweapon;
    ctx.players[player_index].pendingweapon = Some(ready);
    bring_up_weapon(ctx, player_index, game_mode);
}

/// Vanilla `P_MovePsprites`. Called every tic by the player thinker.
///
/// Ticks both overlays; when the `ps_flash` overlay expires, its
/// position is re-synced to the weapon overlay afterwards.
pub fn move_psprites(ctx: &mut EnemyContext<'_>, player_index: usize, game_mode: GameMode) {
    for position in [PspNumber::Weapon, PspNumber::Flash] {
        let next_state = {
            let psp = &mut ctx.players[player_index].psprites[position.index()];
            if psp.state.is_none() {
                continue;
            }
            // a -1 tic count never changes
            if psp.tics == -1 {
                continue;
            }
            psp.tics -= 1;
            if psp.tics != 0 {
                continue;
            }
            let state = psp.state.expect("checked above");
            STATES[state as usize].next
        };
        set_psprite(ctx, player_index, position, next_state, game_mode);
    }

    let sx = ctx.players[player_index].psprites[PspNumber::Weapon.index()].sx;
    let sy = ctx.players[player_index].psprites[PspNumber::Weapon.index()].sy;
    let flash = &mut ctx.players[player_index].psprites[PspNumber::Flash.index()];
    flash.sx = sx;
    flash.sy = sy;
}

/// Vanilla `P_BringUpWeapon`.
///
/// Starts bringing the pending weapon up from the bottom of the screen.
/// Consumes the pending weapon selection.
pub fn bring_up_weapon(ctx: &mut EnemyContext<'_>, player_index: usize, game_mode: GameMode) {
    let player = &mut ctx.players[player_index];
    if player.pendingweapon.is_none() {
        player.pendingweapon = Some(player.readyweapon);
    }
    // bring_up_weapon only reaches the arm below with a pending weapon
    let pending = player
        .pendingweapon
        .expect("pending weapon was just defaulted");

    if pending == Weapon::Chainsaw {
        emit_player_sound(ctx, player_index, SfxId::sfx_sawup);
    }

    let newstate = WEAPON_INFO[pending.index()].upstate;
    ctx.players[player_index].pendingweapon = None;
    ctx.players[player_index].psprites[PspNumber::Weapon.index()].sy = WEAPONBOTTOM;

    set_psprite(ctx, player_index, PspNumber::Weapon, newstate, game_mode);
}

/// Vanilla `P_DropWeapon`.
///
/// The player died, so put the weapon away (down off the bottom of the
/// screen).
pub fn drop_weapon(ctx: &mut EnemyContext<'_>, player_index: usize, game_mode: GameMode) {
    let ready = ctx.players[player_index].readyweapon;
    let downstate = WEAPON_INFO[ready.index()].downstate;
    set_psprite(ctx, player_index, PspNumber::Weapon, downstate, game_mode);
}

/// Vanilla `P_CheckAmmo`.
///
/// Returns `true` when there is enough ammunition to shoot. If not,
/// selects the next weapon to use and returns `false` (the weapon
/// machine starts lowering the current weapon off the screen; `A_Lower`
/// finishes the swap).
///
/// Preferences are set here, in the original order; the shareware gate
/// on plasma/BFG (`gamemode != shareware`) and the commercial gate on
/// the super shotgun (`gamemode == commercial`) are kept exactly.
pub fn check_ammo(ctx: &mut EnemyContext<'_>, player_index: usize, game_mode: GameMode) -> bool {
    let player = &ctx.players[player_index];
    let ready = player.readyweapon;
    let ammo = WEAPON_INFO[ready.index()].ammo;

    // Minimal amount for one shot varies.
    let count = if ready == Weapon::Bfg {
        BFGCELLS
    } else if ready == Weapon::SuperShotgun {
        2
    } else {
        1
    };

    // Some weapons do not need ammunition anyway.
    let Some(kind) = ammo else {
        return true;
    };
    if player.ammo[kind.index()] >= count {
        return true;
    }

    // Out of ammo, pick a weapon to change to. Preferences are set here.
    // Every arm assigns, so the loop runs exactly once; the shape is the
    // original's do-while for clarity.
    let can_plasma = player.weaponowned[Weapon::PlasmaRifle.index()]
        && player.ammo[AmmoKind::Cell.index()] > 0
        && game_mode != GameMode::Shareware;
    let can_supershotgun = player.weaponowned[Weapon::SuperShotgun.index()]
        && player.ammo[AmmoKind::Shell.index()] > 2
        && game_mode == GameMode::Commercial;
    let can_chaingun =
        player.weaponowned[Weapon::Chaingun.index()] && player.ammo[AmmoKind::Clip.index()] > 0;
    let can_shotgun =
        player.weaponowned[Weapon::Shotgun.index()] && player.ammo[AmmoKind::Shell.index()] > 0;
    let can_pistol = player.ammo[AmmoKind::Clip.index()] > 0;
    let can_chainsaw = player.weaponowned[Weapon::Chainsaw.index()];
    let can_missile = player.weaponowned[Weapon::MissileLauncher.index()]
        && player.ammo[AmmoKind::Missile.index()] > 0;
    let can_bfg = player.weaponowned[Weapon::Bfg.index()]
        && player.ammo[AmmoKind::Cell.index()] > 40
        && game_mode != GameMode::Shareware;

    let pending = if can_plasma {
        Weapon::PlasmaRifle
    } else if can_supershotgun {
        Weapon::SuperShotgun
    } else if can_chaingun {
        Weapon::Chaingun
    } else if can_shotgun {
        Weapon::Shotgun
    } else if can_pistol {
        Weapon::Pistol
    } else if can_chainsaw {
        Weapon::Chainsaw
    } else if can_missile {
        Weapon::MissileLauncher
    } else if can_bfg {
        Weapon::Bfg
    } else {
        // if everything fails
        Weapon::Fist
    };
    ctx.players[player_index].pendingweapon = Some(pending);

    // Now set appropriate weapon overlay.
    let ready = ctx.players[player_index].readyweapon;
    let downstate = WEAPON_INFO[ready.index()].downstate;
    set_psprite(ctx, player_index, PspNumber::Weapon, downstate, game_mode);
    false
}

/// Vanilla `P_FireWeapon`.
///
/// Fires the current weapon if there is ammo; switches weapons if not.
pub fn fire_weapon(ctx: &mut EnemyContext<'_>, player_index: usize, game_mode: GameMode) {
    if !check_ammo(ctx, player_index, game_mode) {
        return;
    }

    if let Some(mo) = ctx.players[player_index].mo {
        set_mobj_state_and_run_actions(ctx, mo, StateId::S_PLAY_ATK1);
    }

    let ready = ctx.players[player_index].readyweapon;
    let newstate = WEAPON_INFO[ready.index()].atkstate;
    set_psprite(ctx, player_index, PspNumber::Weapon, newstate, game_mode);

    if let Some(mo) = ctx.players[player_index].mo {
        noise_alert(ctx.map, ctx.store, ctx.flood, mo, mo);
    }
}

/// Vanilla `P_CalcSwing`.
///
/// Bobbing of the overlay sprites; returns the `(sx, sy)` pair instead
/// of filling the original's file-scope globals. Dead code in the
/// original too, kept for parity with the port.
pub fn calc_swing(player: &Player, leveltime: i32) -> (Fixed, Fixed) {
    let swing = player.bob;

    let angle = ((FINE_ANGLES as i32 / 70 * leveltime) & FINE_MASK as i32) as usize;
    let swingx = swing * FINE_SINE[angle];

    let angle = ((FINE_ANGLES as i32 / 70 * leveltime + FINE_ANGLES as i32 / 2) & FINE_MASK as i32)
        as usize;
    let swingy = -(swingx * FINE_SINE[angle]);
    (swingx, swingy)
}

/// Vanilla `P_BulletSlope`.
///
/// Sets a slope so a near miss is at approximately the height of the
/// intended target. Returns the slope (the original stashes it in the
/// `bulletslope` global; `P_GunShot` reads it back).
fn bullet_slope(ctx: &mut EnemyContext<'_>, mo: ThinkerId) -> Fixed {
    let Some(m) = ctx.store.mobj(mo) else {
        return Fixed::ZERO;
    };
    let mut an = m.angle;
    let mut outcome = aim_line_attack(ctx, mo, an, BULLET_AIM_RANGE);

    if outcome.target.is_none() {
        an += Angle::from_raw(AIM_STEP);
        outcome = aim_line_attack(ctx, mo, an, BULLET_AIM_RANGE);
        if outcome.target.is_none() {
            an -= Angle::from_raw(AIM_STEP * 2);
            outcome = aim_line_attack(ctx, mo, an, BULLET_AIM_RANGE);
        }
    }

    outcome.slope
}

/// Vanilla `P_GunShot`.
///
/// A single hitscan pellet from `mo`; `accurate` suppresses the spread
/// (first shot of a refire burst).
fn gun_shot(ctx: &mut EnemyContext<'_>, mo: ThinkerId, accurate: bool, bulletslope: Fixed) {
    let Some(m) = ctx.store.mobj(mo) else {
        return;
    };
    let damage = 5 * (ctx.rng.next() % 3 + 1) as i32;
    let mut angle = m.angle;

    if !accurate {
        angle += Angle::from_raw(((ctx.rng.next() as i32 - ctx.rng.next() as i32) << 18) as u32);
    }

    line_attack(ctx, mo, angle, MISSILERANGE, bulletslope, damage);
}

/// Vanilla `P_SpawnPlayerMissile`.
///
/// Tries to aim at a nearby monster with a three-way sweep, then fires
/// the missile at the hit elevation; with no target in sight the
/// missile flies straight (`momz == 0`).
pub fn spawn_player_missile(ctx: &mut EnemyContext<'_>, source: ThinkerId, mobj_type: MobjType) {
    let Some(s) = ctx.store.mobj(source) else {
        tracing::warn!("P_SpawnPlayerMissile: stale source handle");
        return;
    };
    let an0 = s.angle;
    let (x, y, z, speed) = (
        s.x,
        s.y,
        s.z,
        crate::fixed::Fixed::from_raw(MOBJ_INFO[mobj_type as usize].speed),
    );

    // see which target is to be aimed at
    let mut an = an0;
    let mut outcome = aim_line_attack(ctx, source, an, BULLET_AIM_RANGE);

    if outcome.target.is_none() {
        an += Angle::from_raw(AIM_STEP);
        outcome = aim_line_attack(ctx, source, an, BULLET_AIM_RANGE);

        if outcome.target.is_none() {
            an -= Angle::from_raw(AIM_STEP * 2);
            outcome = aim_line_attack(ctx, source, an, BULLET_AIM_RANGE);
        }

        if outcome.target.is_none() {
            an = an0;
            outcome = crate::enemy::AimOutcome {
                slope: Fixed::ZERO,
                target: None,
            };
        }
    }

    let z = z + Fixed::from_raw(4 * 8 * FRACUNIT);
    let spawn = crate::mobj::SpawnSpec {
        skill: ctx.rules.skill,
        x,
        y,
        z: crate::mobj::SpawnZ::At(z),
        mobj_type,
    };
    let missile = ctx.store.spawn_mobj(ctx.map, ctx.rng, spawn);

    let seesound = MOBJ_INFO[mobj_type as usize].seesound;
    if seesound != SfxId::sfx_None {
        emit_sound(ctx, Some(missile), seesound);
    }

    if let Some(m) = ctx.store.mobj_mut(missile) {
        m.target = Some(source);
        m.angle = an;
        let fine = (an.to_raw() >> crate::tables::ANGLE_TO_FINE_SHIFT) as usize;
        m.momx = speed * FINE_COSINE[fine];
        m.momy = speed * FINE_SINE[fine];
        m.momz = speed * outcome.slope;
    }

    check_missile_spawn(ctx, missile);
}

/// Draw the overlay sprites of one player, ported from
/// `R_DrawPlayerSprites` (the `p_pspr` half of the view).
///
/// `spritelights` is the scalelight row of the view sector (pick it with
/// [`crate::render::select_row`] and
/// [`crate::render::LightTables::scalelight_row`]). Draws the weapon
/// overlay first and the flash overlay second, like the original's
/// `psprites[]` order. Returns the number of overlays actually drawn.
#[allow(clippy::too_many_arguments)]
pub fn draw_player_psprites(
    fb: &mut IndexedFramebuffer,
    player: &Player,
    view: &WallView<'_>,
    defs: &SpriteDefinitions,
    lumps: &SpriteLumpTables,
    colormaps: &[crate::gfx::palette::Colormap],
    spritelights: &[usize; crate::render::MAXLIGHTSCALE],
    manager: &ResourceManager,
    translations: &crate::render::TranslationTables,
    fuzz: &mut FuzzState,
) -> usize {
    let invisibility = player.powers[Power::Invisibility as usize];
    let mut drawn = 0;
    for psp in &player.psprites {
        let Some(state_id) = psp.state else {
            continue;
        };
        let state = STATES[state_id as usize];
        if draw_psprite(
            fb,
            state.sprite as usize,
            state.frame,
            psp.sx,
            psp.sy,
            invisibility,
            view,
            defs,
            lumps,
            colormaps,
            spritelights,
            manager,
            translations,
            fuzz,
        ) {
            drawn += 1;
        }
    }
    drawn
}

// ------------------------------------------------------------------
// tests
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enemy::{BrainState, SoundFlood};
    use crate::map::testutil::{
        blockmap_grid, linedefs, nodes, reject, sectors, segs, sidedefs, subsectors, vertexes,
    };
    use crate::map::types::ML_BLOCKING;
    use crate::map::{Map, MapLumps};
    use crate::mobj::{MobjStore, Skill, SpawnSpec, SpawnZ};
    use crate::player::{PlayerState, TicCmd};
    use crate::rng::DoomRng;

    /// A minimal open room (single sector, four CW-wound walls), same
    /// shape as the AI suite's fixture but self-contained.
    fn open_room() -> Map {
        let vertexes = vertexes(&[(0, 0), (256, 0), (256, 256), (0, 256)]);
        let linedefs = linedefs(&[
            (1, 0, ML_BLOCKING, 0, 0, 0, -1), // L0 south wall: (256,0) -> (0,0)
            (2, 1, ML_BLOCKING, 0, 0, 1, -1), // L1 east wall:  (256,256) -> (256,0)
            (3, 2, ML_BLOCKING, 0, 0, 2, -1), // L2 north wall: (0,256) -> (256,256)
            (0, 3, ML_BLOCKING, 0, 0, 3, -1), // L3 west wall:  (0,0) -> (0,256)
        ]);
        let sidedefs = sidedefs(&[
            (0, 0, "-", "-", "WALL1", 0),
            (0, 0, "-", "-", "WALL1", 0),
            (0, 0, "-", "-", "WALL1", 0),
            (0, 0, "-", "-", "WALL1", 0),
        ]);
        let sectors = sectors(&[(0, 128, "FLOOR4_8", "CEIL3_5", 176, 0, 0)]);
        let segs = segs(&[
            (0, 1, 0, 0, 0, 0),
            (1, 2, 0, 1, 0, 0),
            (2, 3, 0, 2, 0, 0),
            (3, 0, 0, 3, 0, 0),
        ]);
        let subsectors = subsectors(&[(4, 0)]);
        let nodes = nodes(&[]);
        let blockmap = blockmap_grid(
            (-8, -8),
            3,
            3,
            &[
                &[0, 3],
                &[0],
                &[0, 1],
                &[3],
                &[],
                &[1],
                &[2, 3],
                &[2],
                &[1, 2],
            ],
        );
        let reject = reject(&[0x00]);

        let lumps = MapLumps {
            vertexes: &vertexes,
            linedefs: &linedefs,
            sidedefs: &sidedefs,
            sectors: &sectors,
            segs: &segs,
            subsectors: &subsectors,
            nodes: &nodes,
            blockmap: &blockmap,
            reject: &reject,
            things: &[],
        };
        Map::load(&lumps).expect("synthetic room should load")
    }

    fn test_player() -> Player {
        Player {
            mo: None,
            playerstate: PlayerState::Live,
            cmd: TicCmd::default(),
            viewz: Fixed::ZERO,
            viewheight: crate::player::VIEWHEIGHT,
            deltaviewheight: Fixed::ZERO,
            bob: Fixed::ZERO,
            health: 100,
            armorpoints: 0,
            armortype: 0,
            powers: [0; crate::player::NUM_POWERS],
            cards: [false; crate::player::NUM_CARDS],
            backpack: false,
            frags: [0; crate::player::MAXPLAYERS],
            readyweapon: Weapon::Fist,
            pendingweapon: None,
            weaponowned: [false; crate::player::NUM_WEAPONS],
            ammo: [0; crate::player::NUM_AMMO],
            maxammo: [0; crate::player::NUM_AMMO],
            attackdown: false,
            usedown: false,
            cheats: 0,
            refire: 0,
            killcount: 0,
            itemcount: 0,
            secretcount: 0,
            message: None,
            damagecount: 0,
            bonuscount: 0,
            attacker: None,
            extralight: 0,
            fixedcolormap: 0,
            colormap: 0,
            psprites: [crate::player::PspDef::default(); crate::player::NUM_PSPRITES],
            didsecret: false,
        }
    }

    fn players() -> Vec<Player> {
        vec![test_player()]
    }

    fn spawn_player(
        store: &mut MobjStore,
        map: &Map,
        rng: &mut DoomRng,
        players: &mut Vec<Player>,
        x: i32,
        y: i32,
    ) -> ThinkerId {
        let id = store.spawn_mobj(
            map,
            rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(x),
                y: Fixed::from_int(y),
                z: SpawnZ::Floor,
                mobj_type: MobjType::MT_PLAYER,
            },
        );
        if players.is_empty() {
            players.push(test_player());
        }
        players[0].mo = Some(id);
        players[0].health = 100;
        id
    }

    fn spawn_thing(
        store: &mut MobjStore,
        map: &Map,
        rng: &mut DoomRng,
        mobj_type: MobjType,
        x: i32,
        y: i32,
    ) -> ThinkerId {
        store.spawn_mobj(
            map,
            rng,
            SpawnSpec {
                skill: Skill::Medium,
                x: Fixed::from_int(x),
                y: Fixed::from_int(y),
                z: SpawnZ::Floor,
                mobj_type,
            },
        )
    }

    fn damage_events_against(events: &[GameEvent], target: ThinkerId) -> Vec<i32> {
        events
            .iter()
            .filter_map(|e| match e {
                GameEvent::Damage {
                    target: t, damage, ..
                } if *t == target => Some(*damage),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn weapon_state_table_matches_the_original_order() {
        // d_items.c weaponinfo order, 1:1 with the vanilla nuum-weapons enum
        assert_eq!(Weapon::Fist.index(), 0);
        assert_eq!(Weapon::Pistol.index(), 1);
        assert_eq!(Weapon::Shotgun.index(), 2);
        assert_eq!(Weapon::Chaingun.index(), 3);
        assert_eq!(Weapon::MissileLauncher.index(), 4);
        assert_eq!(Weapon::PlasmaRifle.index(), 5);
        assert_eq!(Weapon::Bfg.index(), 6);
        assert_eq!(Weapon::Chainsaw.index(), 7);
        assert_eq!(Weapon::SuperShotgun.index(), 8);

        assert_eq!(
            WEAPON_INFO[Weapon::Pistol.index()].ammo,
            Some(AmmoKind::Clip)
        );
        assert_eq!(
            WEAPON_INFO[Weapon::Shotgun.index()].ammo,
            Some(AmmoKind::Shell)
        );
        assert_eq!(
            WEAPON_INFO[Weapon::Chaingun.index()].ammo,
            Some(AmmoKind::Clip)
        );
        assert_eq!(
            WEAPON_INFO[Weapon::MissileLauncher.index()].ammo,
            Some(AmmoKind::Missile)
        );
        assert_eq!(
            WEAPON_INFO[Weapon::PlasmaRifle.index()].ammo,
            Some(AmmoKind::Cell)
        );
        assert_eq!(WEAPON_INFO[Weapon::Bfg.index()].ammo, Some(AmmoKind::Cell));
        assert_eq!(
            WEAPON_INFO[Weapon::SuperShotgun.index()].ammo,
            Some(AmmoKind::Shell)
        );
        assert_eq!(WEAPON_INFO[Weapon::Fist.index()].ammo, None);
        assert_eq!(WEAPON_INFO[Weapon::Chainsaw.index()].ammo, None);
        assert_eq!(
            WEAPON_INFO[Weapon::Fist.index()].flashstate,
            StateId::S_NULL
        );
        assert_eq!(
            WEAPON_INFO[Weapon::Chainsaw.index()].flashstate,
            StateId::S_NULL
        );
    }

    #[test]
    fn setup_psprites_arms_weapon_and_hides_flash() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        players[0].readyweapon = Weapon::Pistol;
        let mut events = Vec::new();
        let mut world = crate::enemy::SoundFlood::new(map.sectors.len());
        let mut brain = crate::enemy::BrainState::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        setup_psprites(&mut ctx, 0, GameMode::Registered);

        let weapon = ctx.players[0].psprites[PspNumber::Weapon.index()];
        assert_eq!(weapon.state, Some(StateId::S_PISTOLUP));
        // S_PISTOLUP's own action is A_Raise; P_SetPsprite runs it inline
        assert_eq!(weapon.sy, WEAPONBOTTOM - RAISESPEED);
        assert!(ctx.players[0].pendingweapon.is_none());
        assert!(
            ctx.players[0].psprites[PspNumber::Flash.index()]
                .state
                .is_none()
        );
    }

    #[test]
    fn bring_up_raises_weapon_to_ready() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        players[0].readyweapon = Weapon::Pistol;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();
        players[0].pendingweapon = Some(Weapon::Pistol);

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );

        bring_up_weapon(&mut ctx, 0, GameMode::Registered);
        // S_PISTOLUP's own action is A_Raise — one raise step runs when the state is armed
        assert_eq!(
            ctx.players[0].psprites[PspNumber::Weapon.index()].sy,
            WEAPONBOTTOM - RAISESPEED
        );
        assert_eq!(
            ctx.players[0].psprites[PspNumber::Weapon.index()].state,
            Some(StateId::S_PISTOLUP)
        );

        // drive the raise sequence until the weapon settles in the ready state
        for _ in 0..64 {
            let state = ctx.players[0].psprites[PspNumber::Weapon.index()].state;
            if state == Some(StateId::S_PISTOL) {
                break;
            }
            a_raise(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        }
        let weapon = ctx.players[0].psprites[PspNumber::Weapon.index()];
        assert_eq!(weapon.state, Some(StateId::S_PISTOL));
        assert_eq!(weapon.sy, WEAPONTOP);
    }

    #[test]
    fn lower_swaps_weapon_and_raises_the_new_one() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        players[0].readyweapon = Weapon::Pistol;
        players[0].pendingweapon = Some(Weapon::Shotgun);
        players[0].psprites[PspNumber::Weapon.index()].state = Some(StateId::S_PISTOL);
        players[0].psprites[PspNumber::Weapon.index()].sy = WEAPONTOP;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        // drain the lower sequence
        for _ in 0..128 {
            if ctx.players[0].readyweapon == Weapon::Shotgun
                && ctx.players[0].psprites[PspNumber::Weapon.index()].state
                    == Some(StateId::S_SGUNUP)
            {
                break;
            }
            a_lower(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        }
        assert_eq!(ctx.players[0].readyweapon, Weapon::Shotgun);
        assert_eq!(
            ctx.players[0].psprites[PspNumber::Weapon.index()].state,
            Some(StateId::S_SGUNUP)
        );
        // the bring-up arm applies one A_Raise step inline (S_SGUNUP's own action)
        assert_eq!(
            ctx.players[0].psprites[PspNumber::Weapon.index()].sy,
            WEAPONBOTTOM - RAISESPEED
        );
        assert!(ctx.players[0].pendingweapon.is_none());
    }

    #[test]
    fn dead_player_lower_clamps_and_never_re_raises() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        players[0].playerstate = PlayerState::Dead;
        players[0].readyweapon = Weapon::Pistol;
        players[0].pendingweapon = Some(Weapon::Shotgun);
        players[0].psprites[PspNumber::Weapon.index()].state = Some(StateId::S_PISTOLDOWN);
        players[0].psprites[PspNumber::Weapon.index()].sy = WEAPONBOTTOM - Fixed::from_int(1);
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        a_lower(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        let weapon = ctx.players[0].psprites[PspNumber::Weapon.index()];
        assert_eq!(weapon.sy, WEAPONBOTTOM);
        // still the old weapon, still no switch
        assert_eq!(ctx.players[0].readyweapon, Weapon::Pistol);
        assert_eq!(weapon.state, Some(StateId::S_PISTOLDOWN));
    }

    #[test]
    fn weapon_ready_fires_pistol_and_decrements_ammo() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 192, 64);
        players[0].readyweapon = Weapon::Pistol;
        players[0].weaponowned[Weapon::Pistol.index()] = true;
        players[0].ammo[AmmoKind::Clip.index()] = 10;
        players[0].cmd.buttons = BT_ATTACK;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        a_weapon_ready(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);

        // the attack swing starts; ammo is spent when the "gun up" frame ends
        assert_eq!(ctx.players[0].ammo[AmmoKind::Clip.index()], 10);
        assert!(ctx.players[0].attackdown);
        assert_eq!(
            ctx.players[0].psprites[PspNumber::Weapon.index()].state,
            Some(StateId::S_PISTOL1)
        );

        // tick the psprite machine: S_PISTOL1 (4 tics) -> S_PISTOL2 (fire!)
        for _ in 0..4 {
            move_psprites(&mut ctx, 0, GameMode::Registered);
        }

        assert_eq!(ctx.players[0].ammo[AmmoKind::Clip.index()], 9);
        assert_eq!(
            ctx.players[0].psprites[PspNumber::Flash.index()].state,
            Some(StateId::S_PISTOLFLASH)
        );
        assert_eq!(
            ctx.players[0]
                .mo
                .and_then(|m| ctx.store.mobj(m))
                .map(|m| m.state),
            Some(StateId::S_PLAY_ATK2)
        );
        let hits = damage_events_against(ctx.events, monster);
        assert_eq!(hits.len(), 1, "one pellet per pistol shot");
        assert!(hits[0] > 0);
        // the gun fired, so the noise alert must have rung
        assert!(ctx.flood.sound_target(0).is_some());
    }

    #[test]
    fn weapon_ready_does_not_refire_missile_launcher_without_a_new_press() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        players[0].readyweapon = Weapon::MissileLauncher;
        players[0].weaponowned[Weapon::MissileLauncher.index()] = true;
        players[0].ammo[AmmoKind::Missile.index()] = 2;
        players[0].cmd.buttons = BT_ATTACK;
        players[0].attackdown = false;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        a_weapon_ready(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        assert!(ctx.players[0].attackdown);
        // the rocket fires at the S_MISSILE2 frame, not at the ready state
        assert_eq!(ctx.players[0].ammo[AmmoKind::Missile.index()], 2);
        for _ in 0..8 {
            move_psprites(&mut ctx, 0, GameMode::Registered);
        }
        assert_eq!(ctx.players[0].ammo[AmmoKind::Missile.index()], 1);

        // holding the button must not fire a second missile from the ready check
        a_weapon_ready(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        assert_eq!(ctx.players[0].ammo[AmmoKind::Missile.index()], 1);

        // release, drain the rest of the attack cycle, and re-press
        ctx.players[0].cmd.buttons = 0;
        for _ in 0..16 {
            move_psprites(&mut ctx, 0, GameMode::Registered);
        }
        a_weapon_ready(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        assert!(!ctx.players[0].attackdown);
        ctx.players[0].cmd.buttons = BT_ATTACK;
        a_weapon_ready(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        for _ in 0..8 {
            move_psprites(&mut ctx, 0, GameMode::Registered);
        }
        assert_eq!(ctx.players[0].ammo[AmmoKind::Missile.index()], 0);
    }

    #[test]
    fn out_of_ammo_switches_to_next_weapon_preference() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        players[0].readyweapon = Weapon::Pistol;
        players[0].weaponowned[Weapon::Pistol.index()] = true;
        players[0].weaponowned[Weapon::Shotgun.index()] = true;
        players[0].ammo[AmmoKind::Shell.index()] = 1;
        players[0].ammo[AmmoKind::Clip.index()] = 0;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        assert!(!check_ammo(&mut ctx, 0, GameMode::Registered));
        assert_eq!(ctx.players[0].pendingweapon, Some(Weapon::Shotgun));
        assert_eq!(
            ctx.players[0].psprites[PspNumber::Weapon.index()].state,
            Some(StateId::S_PISTOLDOWN)
        );
    }

    #[test]
    fn out_of_everything_falls_back_to_fist() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        players[0].readyweapon = Weapon::Pistol;
        players[0].weaponowned[Weapon::Pistol.index()] = true;
        players[0].ammo[AmmoKind::Clip.index()] = 0;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        assert!(!check_ammo(&mut ctx, 0, GameMode::Registered));
        assert_eq!(ctx.players[0].pendingweapon, Some(Weapon::Fist));
    }

    #[test]
    fn bfg_preference_is_gated_by_game_mode() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();

        for (mode, expect_bfg) in [
            (GameMode::Shareware, false),
            (GameMode::Registered, true),
            (GameMode::Commercial, true),
            (GameMode::Retail, true),
        ] {
            let mut players = players();
            let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
            players[0].readyweapon = Weapon::Pistol;
            players[0].weaponowned[Weapon::Pistol.index()] = true;
            players[0].weaponowned[Weapon::Bfg.index()] = true;
            players[0].ammo[AmmoKind::Clip.index()] = 0;
            players[0].ammo[AmmoKind::Cell.index()] = 50;
            let mut world = SoundFlood::new(map.sectors.len());
            let mut brain = BrainState::new();
            let mut events = Vec::new();
            let mut ctx = EnemyContext::new(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut brain,
                &mut events,
            );
            assert!(!check_ammo(&mut ctx, 0, mode));
            assert_eq!(
                ctx.players[0].pendingweapon,
                Some(if expect_bfg {
                    Weapon::Bfg
                } else {
                    Weapon::Fist
                }),
                "game mode {mode:?}"
            );
        }
    }

    #[test]
    fn bfg_needs_forty_cells_to_fire() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        players[0].readyweapon = Weapon::Bfg;
        players[0].weaponowned[Weapon::Bfg.index()] = true;
        players[0].ammo[AmmoKind::Cell.index()] = BFGCELLS - 1;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        assert!(!check_ammo(&mut ctx, 0, GameMode::Registered));
        ctx.players[0].ammo[AmmoKind::Cell.index()] = BFGCELLS;
        assert!(check_ammo(&mut ctx, 0, GameMode::Registered));
    }

    #[test]
    fn super_shotgun_needs_two_shells_and_spends_both() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 192, 64);
        players[0].readyweapon = Weapon::SuperShotgun;
        players[0].weaponowned[Weapon::SuperShotgun.index()] = true;
        players[0].ammo[AmmoKind::Shell.index()] = 1;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        assert!(!check_ammo(&mut ctx, 0, GameMode::Commercial));

        ctx.players[0].ammo[AmmoKind::Shell.index()] = 2;
        assert!(check_ammo(&mut ctx, 0, GameMode::Commercial));

        a_fire_shotgun2(&mut ctx, 0, PspNumber::Weapon, GameMode::Commercial);
        assert_eq!(ctx.players[0].ammo[AmmoKind::Shell.index()], 0);
        let hits = damage_events_against(ctx.events, monster);
        assert_eq!(hits.len(), 20, "twenty pellets per super shotgun shot");
        assert!(hits.iter().all(|&d| (5..=15).contains(&d)));
    }

    #[test]
    fn shotgun_fires_seven_pellets() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 192, 64);
        players[0].readyweapon = Weapon::Shotgun;
        players[0].weaponowned[Weapon::Shotgun.index()] = true;
        players[0].ammo[AmmoKind::Shell.index()] = 4;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        a_fire_shotgun(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        assert_eq!(ctx.players[0].ammo[AmmoKind::Shell.index()], 3);
        let hits = damage_events_against(ctx.events, monster);
        assert_eq!(hits.len(), 7);
        assert!(hits.iter().all(|&d| (5..=15).contains(&d)));
    }

    #[test]
    fn chaingun_alternates_flash_with_the_chain_frame() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        let _monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 192, 64);
        players[0].readyweapon = Weapon::Chaingun;
        players[0].weaponowned[Weapon::Chaingun.index()] = true;
        players[0].ammo[AmmoKind::Clip.index()] = 10;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        ctx.players[0].psprites[PspNumber::Weapon.index()].state = Some(StateId::S_CHAIN1);
        a_fire_cgun(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        assert_eq!(
            ctx.players[0].psprites[PspNumber::Flash.index()].state,
            Some(StateId::S_CHAINFLASH1)
        );

        ctx.players[0].psprites[PspNumber::Weapon.index()].state = Some(StateId::S_CHAIN2);
        a_fire_cgun(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        assert_eq!(
            ctx.players[0].psprites[PspNumber::Flash.index()].state,
            Some(StateId::S_CHAINFLASH2)
        );
        assert_eq!(ctx.players[0].ammo[AmmoKind::Clip.index()], 8);
    }

    #[test]
    fn chaingun_click_on_empty_magazine_plays_sound_but_not_shot() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        players[0].readyweapon = Weapon::Chaingun;
        players[0].weaponowned[Weapon::Chaingun.index()] = true;
        players[0].ammo[AmmoKind::Clip.index()] = 0;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        a_fire_cgun(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        let sounds: Vec<_> = ctx
            .events
            .iter()
            .filter_map(|e| match e {
                GameEvent::Sound { sfx, .. } => Some(*sfx),
                _ => None,
            })
            .collect();
        assert_eq!(sounds, vec![SfxId::sfx_pistol]);
        assert!(
            ctx.events
                .iter()
                .all(|e| !matches!(e, GameEvent::Damage { .. }))
        );
        // the avatar stays in its spawn state; no attack frame entered
        let state = ctx.players[0]
            .mo
            .and_then(|m| ctx.store.mobj(m))
            .map(|m| m.state);
        assert_ne!(state, Some(StateId::S_PLAY_ATK2));
    }

    #[test]
    fn punch_damage_scales_with_strength() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);

        let mut damages = |strength: i32| {
            // wipe the world so the previous run cannot intercept the punch
            store.reset_for_level(&map);
            let mut rng = DoomRng::new();
            let mut players = players();
            let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
            let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 100, 64);
            players[0].powers[Power::Strength as usize] = strength;
            let mut world = SoundFlood::new(map.sectors.len());
            let mut brain = BrainState::new();
            let mut events = Vec::new();
            let mut ctx = EnemyContext::new(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut brain,
                &mut events,
            );
            a_punch(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
            damage_events_against(ctx.events, monster)
        };

        let normal = damages(0);
        let berserk = damages(1);
        assert_eq!(normal.len(), 1);
        assert_eq!(berserk.len(), 1);
        assert_eq!(berserk[0], normal[0] * 10);
        assert!((2..=20).contains(&normal[0]));
    }

    #[test]
    fn punch_turns_to_face_the_target() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        // slightly off the east axis: the punch can hit it and the player turns to face it
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 100, 70);
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        a_punch(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        let expected = crate::bsp::point_to_angle_2(
            Fixed::from_int(64),
            Fixed::from_int(64),
            ctx.store.mobj(monster).map(|m| m.x).unwrap_or(Fixed::ZERO),
            ctx.store.mobj(monster).map(|m| m.y).unwrap_or(Fixed::ZERO),
        );
        assert_eq!(
            ctx.players[0]
                .mo
                .and_then(|m| ctx.store.mobj(m))
                .map(|m| m.angle),
            Some(expected)
        );
    }

    #[test]
    fn saw_hits_and_marks_just_attacked() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 100, 64);
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        a_saw(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        let hits = damage_events_against(ctx.events, monster);
        assert_eq!(hits.len(), 1);
        assert!((2..=20).contains(&hits[0]));
        assert!(ctx.events.iter().any(|e| matches!(
            e,
            GameEvent::Sound {
                sfx: SfxId::sfx_sawhit,
                ..
            }
        )));
        let flags = ctx.players[0]
            .mo
            .and_then(|m| ctx.store.mobj(m))
            .map(|m| m.flags);
        assert_eq!(
            flags.map(|f| f & crate::info::MF_JUSTATTACKED),
            Some(crate::info::MF_JUSTATTACKED)
        );
    }

    #[test]
    fn saw_miss_plays_sawful() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        // nothing in front of the player
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        a_saw(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        assert!(ctx.events.iter().any(|e| matches!(
            e,
            GameEvent::Sound {
                sfx: SfxId::sfx_sawful,
                ..
            }
        )));
        assert!(
            !ctx.events
                .iter()
                .any(|e| matches!(e, GameEvent::Damage { .. }))
        );
    }

    #[test]
    fn saw_snaps_angle_toward_target_to_the_right() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        // slightly to the right of due east: within ANG90/20 of facing
        let _monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 124, 65);
        let before = ctx_angle(&store, players[0].mo.expect("avatar"));
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        a_saw(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        let after = ctx_angle(ctx.store, ctx.players[0].mo.expect("avatar"));
        assert_eq!(
            after,
            Angle::from_raw(before.to_raw().wrapping_add(ANG90_DIV_20))
        );
    }

    fn ctx_angle(store: &MobjStore, id: ThinkerId) -> Angle {
        store.mobj(id).map(|m| m.angle).expect("mobj present")
    }

    #[test]
    fn refire_counts_and_resets() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        let _monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 192, 64);
        players[0].readyweapon = Weapon::Pistol;
        players[0].weaponowned[Weapon::Pistol.index()] = true;
        players[0].ammo[AmmoKind::Clip.index()] = 10;
        players[0].cmd.buttons = BT_ATTACK;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        a_refire(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        assert_eq!(ctx.players[0].refire, 1);
        a_refire(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        assert_eq!(ctx.players[0].refire, 2);

        ctx.players[0].cmd.buttons = 0;
        a_refire(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        assert_eq!(ctx.players[0].refire, 0);
    }

    #[test]
    fn refire_with_empty_mag_swaps_weapon_instead_of_firing() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        let _monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 192, 64);
        players[0].readyweapon = Weapon::Pistol;
        players[0].weaponowned[Weapon::Pistol.index()] = true;
        players[0].ammo[AmmoKind::Clip.index()] = 0;
        players[0].cmd.buttons = BT_ATTACK;
        players[0].refire = 1;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        a_refire(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        assert!(
            !ctx.events
                .iter()
                .any(|e| matches!(e, GameEvent::Damage { .. }))
        );
        // the weapon goes down; pendingweapon selects the fallback
        assert_eq!(ctx.players[0].pendingweapon, Some(Weapon::Fist));
        assert_eq!(
            ctx.players[0].psprites[PspNumber::Weapon.index()].state,
            Some(StateId::S_PISTOLDOWN)
        );
    }

    #[test]
    fn muzzle_flash_expires_and_clears_extralight() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        let _monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 192, 64);
        players[0].readyweapon = Weapon::Pistol;
        players[0].weaponowned[Weapon::Pistol.index()] = true;
        players[0].ammo[AmmoKind::Clip.index()] = 10;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        a_fire_pistol(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);

        assert_eq!(ctx.players[0].extralight, 1, "flash arms the light boost");
        // S_PISTOLFLASH has 7 tics; tick it out through move_psprites
        for _ in 0..6 {
            move_psprites(&mut ctx, 0, GameMode::Registered);
        }
        assert_eq!(
            ctx.players[0].psprites[PspNumber::Flash.index()].state,
            Some(StateId::S_PISTOLFLASH)
        );
        move_psprites(&mut ctx, 0, GameMode::Registered);
        assert_eq!(
            ctx.players[0].psprites[PspNumber::Flash.index()].state,
            None
        );
        assert_eq!(
            ctx.players[0].extralight, 0,
            "flash off resets the light boost"
        );
    }

    #[test]
    fn weapon_and_flash_cast_final_positions_stay_in_sync() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        players[0].bob = Fixed::from_int(1);
        players[0].psprites[PspNumber::Weapon.index()].state = Some(StateId::S_PISTOL);
        players[0].psprites[PspNumber::Flash.index()].state = Some(StateId::S_PISTOLFLASH);
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        ctx.leveltime = 35;
        a_weapon_ready(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        move_psprites(&mut ctx, 0, GameMode::Registered);

        let weapon = ctx.players[0].psprites[PspNumber::Weapon.index()];
        let flash = ctx.players[0].psprites[PspNumber::Flash.index()];
        assert_eq!(flash.sx, weapon.sx);
        assert_eq!(flash.sy, weapon.sy);
        assert_ne!(
            weapon.sx,
            Fixed::ONE,
            "bobbing must move the overlay off center"
        );
    }

    #[test]
    fn calc_swing_is_deterministic() {
        let mut players = players();
        players[0].bob = Fixed::from_int(2);

        let a = calc_swing(&players[0], 0);
        let b = calc_swing(&players[0], 0);
        assert_eq!(a, b, "same inputs, same outputs");
        let c = calc_swing(&players[0], 7);
        assert_ne!(a, c, "different tics, different swing");
    }

    #[test]
    fn chainsaw_arms_sawup_sound_on_bring_up() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        players[0].readyweapon = Weapon::Chainsaw;
        players[0].weaponowned[Weapon::Chainsaw.index()] = true;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();
        players[0].pendingweapon = Some(Weapon::Chainsaw);

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        bring_up_weapon(&mut ctx, 0, GameMode::Registered);
        assert!(ctx.events.iter().any(|e| matches!(
            e,
            GameEvent::Sound {
                sfx: SfxId::sfx_sawup,
                ..
            }
        )));
    }

    #[test]
    fn fire_weapon_does_not_shoot_without_ammo() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        let _monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 192, 64);
        players[0].readyweapon = Weapon::Pistol;
        players[0].weaponowned[Weapon::Pistol.index()] = true;
        players[0].ammo[AmmoKind::Clip.index()] = 0;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        fire_weapon(&mut ctx, 0, GameMode::Registered);
        assert!(
            !ctx.events
                .iter()
                .any(|e| matches!(e, GameEvent::Damage { .. }))
        );
        assert_eq!(
            ctx.players[0]
                .mo
                .and_then(|m| ctx.store.mobj(m))
                .map(|m| m.state),
            Some(StateId::S_PLAY), // untouched: S_PLAY_ATK1 never entered
        );
    }

    #[test]
    fn spawn_player_missile_spawns_rocket_toward_facing() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        spawn_player_missile(&mut ctx, avatar, MobjType::MT_ROCKET);
        let rockets: Vec<_> = ctx
            .store
            .thinkers()
            .filter(|(_id, t)| match t {
                crate::thinker::Thinker::Mobj(m) => m.mobj_type == MobjType::MT_ROCKET,
                _ => false,
            })
            .collect();
        assert_eq!(rockets.len(), 1);
        let crate::thinker::Thinker::Mobj(rocket) = rockets[0].1 else {
            panic!("the rocket thinker was not a mobj");
        };
        assert_eq!(rocket.target, Some(avatar));
        // no target anywhere: the shot flies straight (zero elevation slope)
        assert_eq!(rocket.momz, Fixed::ZERO);
        assert!(
            rocket.momx > Fixed::ZERO,
            "rocket must fly east like the player faces"
        );
        let _ = &mut ctx;
    }

    #[test]
    fn psprite_overlay_draws_weapon_and_flash_frames() {
        use crate::wad::testutil::build_wad;

        fn patch_bytes(width: i16, height: i16, fill: u8) -> Vec<u8> {
            let mut data = Vec::new();
            data.extend_from_slice(&width.to_le_bytes());
            data.extend_from_slice(&height.to_le_bytes());
            data.extend_from_slice(&4i16.to_le_bytes()); // left offset
            data.extend_from_slice(&8i16.to_le_bytes()); // top offset
            let mut ptrs = Vec::with_capacity(width as usize);
            let mut body = Vec::new();
            let mut offset = 8 + width as usize * 4;
            for _ in 0..width {
                ptrs.push(offset as i32);
                body.extend_from_slice(&[0u8, height as u8, 0]);
                body.extend_from_slice(&vec![fill; height as usize]);
                body.push(0);
                offset += 4 + height as usize;
            }
            body.push(0xff);
            for ptr in &ptrs {
                data.extend_from_slice(&ptr.to_le_bytes());
            }
            data.extend_from_slice(&body);
            data
        }

        let wad = build_wad(
            b"IWAD",
            &[
                ("PNAMES", &0i32.to_le_bytes()),
                ("TEXTURE1", &0i32.to_le_bytes()),
                ("S_START", &[]),
                ("PISGA0", &patch_bytes(8, 12, 42)),
                ("PISFA0", &patch_bytes(8, 12, 99)),
                ("S_END", &[]),
            ],
        );
        let manager =
            ResourceManager::new(crate::wad::WadFile::from_bytes(wad).unwrap(), Vec::new());

        // the tables index sprites by their ordinal in sprnames[], so build
        // the full list; the two patched sprites are the only ones with frames
        let all_names: Vec<[u8; 4]> = crate::info::SPRITE_NAMES.iter().map(|n| **n).collect();
        let defs = SpriteDefinitions::build(&all_names, &manager).unwrap();
        let lumps = SpriteLumpTables::build(&manager).unwrap();
        let colormaps: Vec<crate::gfx::palette::Colormap> = (0..33)
            .map(|_| crate::gfx::palette::Colormap(core::array::from_fn(|i| i as u8)))
            .collect();
        let tables = crate::bsp::ViewTables::new(320, Fixed::from_int(160));
        let projection = crate::render::WallProjection::build(&tables, 320);
        let light = crate::render::LightTables::build(320);
        let translations = crate::render::TranslationTables::build();
        let textures = crate::render::TextureSet::from_resources(&manager).unwrap();

        let view = crate::render::WallView {
            tables: &tables,
            projection: &projection,
            light: &light,
            textures: &textures,
            colormaps: &colormaps,
            view_x: Fixed::ZERO,
            view_y: Fixed::ZERO,
            view_z: Fixed::ZERO,
            view_angle: Angle::ZERO,
            view_cos: crate::tables::FINE_COSINE[0],
            view_sin: crate::tables::FINE_SINE[0],
            view_width: 320,
            view_height: 200,
            center_y: 100,
            center_x_frac: Fixed::from_int(160),
            center_y_frac: Fixed::from_int(100),
            projection_distance: Fixed::from_int(160),
            extralight: 0,
            fixed_colormap: None,
            detail_shift: 0,
            sky_flat: None,
        };

        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);

        let mut fb = IndexedFramebuffer::new();
        let mut fuzz = FuzzState::new();
        let spritelights = [7usize; crate::render::MAXLIGHTSCALE];

        // pistol in the ready state paints one overlay
        players[0].psprites[PspNumber::Weapon.index()].state = Some(StateId::S_PISTOL);
        players[0].psprites[PspNumber::Weapon.index()].sy = Fixed::from_int(32);
        let drawn = draw_player_psprites(
            &mut fb,
            &players[0],
            &view,
            &defs,
            &lumps,
            &colormaps,
            &spritelights,
            &manager,
            &translations,
            &mut fuzz,
        );
        assert_eq!(drawn, 1);
        let painted = fb.as_slice().iter().filter(|&&p| p != 0).count();
        assert!(painted > 0, "the overlay must paint some pixels");

        // arming the flash overlay paints the second one, fullbright
        players[0].psprites[PspNumber::Flash.index()].state = Some(StateId::S_PISTOLFLASH);
        players[0].psprites[PspNumber::Flash.index()].sy = Fixed::from_int(32);
        let drawn = draw_player_psprites(
            &mut fb,
            &players[0],
            &view,
            &defs,
            &lumps,
            &colormaps,
            &spritelights,
            &manager,
            &translations,
            &mut fuzz,
        );
        assert_eq!(drawn, 2);
    }

    #[test]
    fn super_shotgun_full_cycle_fires_and_reloads() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        let _monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 192, 64);
        players[0].readyweapon = Weapon::SuperShotgun;
        players[0].weaponowned[Weapon::SuperShotgun.index()] = true;
        players[0].ammo[AmmoKind::Shell.index()] = 4;
        players[0].cmd.buttons = BT_ATTACK;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        // fire like the attack sequence would: atk state then drain the whole cycle
        a_weapon_ready(&mut ctx, 0, PspNumber::Weapon, GameMode::Commercial);
        assert_eq!(
            ctx.players[0].psprites[PspNumber::Weapon.index()].state,
            Some(StateId::S_DSGUN1)
        );
        ctx.players[0].cmd.buttons = 0;
        for _ in 0..80 {
            move_psprites(&mut ctx, 0, GameMode::Commercial);
        }

        let sounds: Vec<SfxId> = ctx
            .events
            .iter()
            .filter_map(|e| match e {
                GameEvent::Sound { sfx, .. } => Some(*sfx),
                _ => None,
            })
            .collect();
        for expected in [
            SfxId::sfx_dshtgn,
            SfxId::sfx_dbopn,
            SfxId::sfx_dbload,
            SfxId::sfx_dbcls,
        ] {
            assert!(
                sounds.contains(&expected),
                "expected {expected:?} in the super shotgun cycle, got {sounds:?}"
            );
        }
        assert_eq!(ctx.players[0].ammo[AmmoKind::Shell.index()], 2);
        assert_eq!(ctx.players[0].pendingweapon, None, "the weapon stayed up");
        assert_eq!(
            ctx.players[0].psprites[PspNumber::Weapon.index()].state,
            Some(StateId::S_DSGUN)
        );
        assert_eq!(ctx.players[0].refire, 0);
    }

    #[test]
    fn super_shotgun_cycle_retires_early_when_empty() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        players[0].readyweapon = Weapon::SuperShotgun;
        players[0].weaponowned[Weapon::SuperShotgun.index()] = true;
        players[0].ammo[AmmoKind::Shell.index()] = 2;
        players[0].cmd.buttons = BT_ATTACK;
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        a_weapon_ready(&mut ctx, 0, PspNumber::Weapon, GameMode::Commercial);
        ctx.players[0].cmd.buttons = 0;
        for _ in 0..80 {
            move_psprites(&mut ctx, 0, GameMode::Commercial);
        }

        let sounds: Vec<SfxId> = ctx
            .events
            .iter()
            .filter_map(|e| match e {
                GameEvent::Sound { sfx, .. } => Some(*sfx),
                _ => None,
            })
            .collect();
        assert!(sounds.contains(&SfxId::sfx_dshtgn));
        assert_eq!(ctx.players[0].ammo[AmmoKind::Shell.index()], 0);
        // A_CheckReload saw an empty magazine and flipped to the fallback
        // before the break-open clack — the reload sequence never plays
        assert!(!sounds.contains(&SfxId::sfx_dbopn));
        assert!(!sounds.contains(&SfxId::sfx_dbload));
        assert!(!sounds.contains(&SfxId::sfx_dbcls));
        // the fallback finished raising: the fist is up
        assert_eq!(ctx.players[0].readyweapon, Weapon::Fist);
        assert_eq!(
            ctx.players[0].psprites[PspNumber::Weapon.index()].state,
            Some(StateId::S_PUNCH)
        );
    }

    #[test]
    fn pistol_fire_chain_full_cycle_from_ready_state() {
        let map = open_room();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 64, 64);
        let monster = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 192, 64);
        players[0].readyweapon = Weapon::Pistol;
        players[0].weaponowned[Weapon::Pistol.index()] = true;
        players[0].ammo[AmmoKind::Clip.index()] = 2;
        players[0].pendingweapon = Some(Weapon::Pistol);
        let mut world = SoundFlood::new(map.sectors.len());
        let mut brain = BrainState::new();
        let mut events = Vec::new();

        let mut ctx = EnemyContext::new(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            &mut brain,
            &mut events,
        );
        bring_up_weapon(&mut ctx, 0, GameMode::Registered);
        // raise sequence into ready state
        for _ in 0..64 {
            if ctx.players[0].psprites[PspNumber::Weapon.index()].state == Some(StateId::S_PISTOL) {
                break;
            }
            a_raise(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
        }
        assert_eq!(
            ctx.players[0].psprites[PspNumber::Weapon.index()].state,
            Some(StateId::S_PISTOL)
        );

        // fire twice, releasing the trigger between shots and letting the
        // psprite machine drain the attack sequence
        for _ in 0..2 {
            ctx.players[0].cmd.buttons = BT_ATTACK;
            a_weapon_ready(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
            ctx.players[0].cmd.buttons = 0;
            a_weapon_ready(&mut ctx, 0, PspNumber::Weapon, GameMode::Registered);
            for _ in 0..32 {
                move_psprites(&mut ctx, 0, GameMode::Registered);
            }
        }
        assert_eq!(ctx.players[0].ammo[AmmoKind::Clip.index()], 0);
        let hits = damage_events_against(ctx.events, monster);
        assert_eq!(hits.len(), 2);
    }
}
