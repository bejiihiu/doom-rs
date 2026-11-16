//! Item pickups, ported from `p_inter.c`
//! (`P_GiveAmmo`/`P_GiveWeapon`/`P_GiveBody`/`P_GiveArmor`/`P_GiveCard`/
//! `P_GivePower`/`P_TouchSpecialThing`).
//!
//! The switch is driven by the sprite of the special thing, exactly like
//! the original; the ammo values live in [`crate::inter::MAX_AMMO`] /
//! [`crate::inter::CLIP_AMMO`] and `weaponinfo[]` is
//! [`crate::weapons::WEAPON_INFO`].

use crate::args::DeathmatchMode;
use crate::fixed::FRACUNIT;
use crate::hud::HudMessage;
use crate::info::{MF_COUNTITEM, MF_DROPPED, MF_SHADOW, SfxId, Sprite};
use crate::player::{AmmoKind, Card, MAXHEALTH, NUM_AMMO, Power, Weapon};
use crate::thinker::ThinkerId;
use crate::weapons::WEAPON_INFO;

use super::{BONUS_ADD, INFRA_TICS, INVIS_TICS, INVUL_TICS, IRON_TICS, InterContext, InterEvent};

/// All ammo kinds in table order (vanilla iterates `0..NUMAMMO`).
const ALL_AMMO: [AmmoKind; NUM_AMMO] = [
    AmmoKind::Clip,
    AmmoKind::Shell,
    AmmoKind::Cell,
    AmmoKind::Missile,
];

/// Vanilla `P_GiveAmmo` (p_inter.c).
///
/// `num` is the number of clip loads, not the individual count
/// (`0` = half a clip). Returns false when the ammo can't be picked up at
/// all (already at max).
pub fn give_ammo(ctx: &mut InterContext<'_>, player: usize, ammo: AmmoKind, num: i32) -> bool {
    let baby_or_nightmare = matches!(
        ctx.rules.skill,
        crate::mobj::Skill::Baby | crate::mobj::Skill::Nightmare
    );

    let Some(p) = ctx.players.get_mut(player) else {
        return false;
    };
    let ai = ammo.index();

    if p.ammo[ai] == p.maxammo[ai] {
        return false; // don't pick up
    }

    let mut num = if num != 0 {
        num * super::CLIP_AMMO[ai]
    } else {
        super::CLIP_AMMO[ai] / 2
    };
    if baby_or_nightmare {
        num <<= 1; // give double ammo in trainer mode, you'll need it in nightmare
    }

    let oldammo = p.ammo[ai];
    p.ammo[ai] += num;
    if p.ammo[ai] > p.maxammo[ai] {
        p.ammo[ai] = p.maxammo[ai];
    }

    // If non zero ammo, don't change weapons, the player was lower on
    // purpose. We were down to zero, so select a new weapon. Preferences
    // are not user selectable.
    if oldammo == 0 {
        match ammo {
            AmmoKind::Clip => {
                if p.readyweapon == Weapon::Fist {
                    p.pendingweapon = if p.weaponowned[Weapon::Chaingun.index()] {
                        Some(Weapon::Chaingun)
                    } else {
                        Some(Weapon::Pistol)
                    };
                }
            }
            AmmoKind::Shell => {
                if (p.readyweapon == Weapon::Fist || p.readyweapon == Weapon::Pistol)
                    && p.weaponowned[Weapon::Shotgun.index()]
                {
                    p.pendingweapon = Some(Weapon::Shotgun);
                }
            }
            AmmoKind::Cell => {
                if (p.readyweapon == Weapon::Fist || p.readyweapon == Weapon::Pistol)
                    && p.weaponowned[Weapon::PlasmaRifle.index()]
                {
                    p.pendingweapon = Some(Weapon::PlasmaRifle);
                }
            }
            AmmoKind::Missile => {
                if p.readyweapon == Weapon::Fist && p.weaponowned[Weapon::MissileLauncher.index()] {
                    p.pendingweapon = Some(Weapon::MissileLauncher);
                }
            }
        }
    }

    true
}

/// Vanilla `P_GiveWeapon` (p_inter.c).
///
/// `dropped` is the monster-drop case: a dropped weapon grants one clip
/// load, a placed one two. Returns whether something was taken.
pub fn give_weapon(
    ctx: &mut InterContext<'_>,
    player: usize,
    weapon: Weapon,
    dropped: bool,
) -> bool {
    // leave placed weapons forever on net games
    if ctx.rules.netgame && ctx.deathmatch != DeathmatchMode::Alt && !dropped {
        let Some(p) = ctx.players.get_mut(player) else {
            return false;
        };
        if p.weaponowned[weapon.index()] {
            return false; // weapon already there, leave it
        }
        p.bonuscount += BONUS_ADD;
        p.weaponowned[weapon.index()] = true;
        // am_noammo weapons (fist/chainsaw) get nothing, like vanilla's
        // P_GiveAmmo(am_noammo) early-out
        if let Some(ammo) = WEAPON_INFO[weapon.index()].ammo {
            if ctx.deathmatch == DeathmatchMode::Duel {
                give_ammo(ctx, player, ammo, 5);
            } else {
                give_ammo(ctx, player, ammo, 2);
            }
        }
        let Some(p) = ctx.players.get_mut(player) else {
            return false;
        };
        p.pendingweapon = Some(weapon);

        if player == ctx.consoleplayer {
            ctx.events.push(InterEvent::Sound {
                sfx: SfxId::sfx_wpnup,
            });
        }
        return false; // netgames never consume weapons
    }

    // give one clip with a dropped weapon, two with a found one
    let gaveammo = match WEAPON_INFO[weapon.index()].ammo {
        Some(ammo) => give_ammo(ctx, player, ammo, if dropped { 1 } else { 2 }),
        None => false,
    };

    let Some(p) = ctx.players.get_mut(player) else {
        return false;
    };
    let gaveweapon = if p.weaponowned[weapon.index()] {
        false
    } else {
        p.weaponowned[weapon.index()] = true;
        p.pendingweapon = Some(weapon);
        true
    };

    gaveweapon || gaveammo
}

/// Vanilla `P_GiveBody` (p_inter.c). Returns false if the body isn't
/// needed at all (health already full).
pub fn give_body(ctx: &mut InterContext<'_>, player: usize, num: i32) -> bool {
    let avatar = ctx.players.get(player).and_then(|p| p.mo);
    let Some(p) = ctx.players.get_mut(player) else {
        return false;
    };
    if p.health >= MAXHEALTH {
        return false;
    }
    p.health += num;
    if p.health > MAXHEALTH {
        p.health = MAXHEALTH;
    }
    let new_health = p.health;
    if let Some(mo) = avatar {
        if let Some(m) = ctx.store.mobj_mut(mo) {
            m.health = new_health;
        }
    }
    true
}

/// Vanilla `P_GiveArmor` (p_inter.c). Returns false when the offered
/// armor class is worse than (or equal to) the current one.
pub fn give_armor(ctx: &mut InterContext<'_>, player: usize, armortype: i32) -> bool {
    let hits = armortype * 100;
    let Some(p) = ctx.players.get_mut(player) else {
        return false;
    };
    if p.armorpoints >= hits {
        return false; // don't pick up
    }
    p.armortype = armortype;
    p.armorpoints = hits;
    true
}

/// Vanilla `P_GiveCard` (p_inter.c).
pub fn give_card(ctx: &mut InterContext<'_>, player: usize, card: Card) {
    let Some(p) = ctx.players.get_mut(player) else {
        return;
    };
    if p.cards[card.index()] {
        return;
    }
    p.bonuscount = BONUS_ADD;
    p.cards[card.index()] = true;
}

/// Vanilla `P_GivePower` (p_inter.c).
pub fn give_power(ctx: &mut InterContext<'_>, player: usize, power: Power) -> bool {
    let avatar = ctx.players.get(player).and_then(|p| p.mo);
    match power {
        Power::Invulnerability => {
            if let Some(p) = ctx.players.get_mut(player) {
                p.powers[power.index()] = INVUL_TICS;
            }
            true
        }
        Power::Invisibility => {
            if let Some(p) = ctx.players.get_mut(player) {
                p.powers[power.index()] = INVIS_TICS;
            }
            if let Some(mo) = avatar {
                if let Some(m) = ctx.store.mobj_mut(mo) {
                    m.flags |= MF_SHADOW;
                }
            }
            true
        }
        Power::Infrared => {
            if let Some(p) = ctx.players.get_mut(player) {
                p.powers[power.index()] = INFRA_TICS;
            }
            true
        }
        Power::IronFeet => {
            if let Some(p) = ctx.players.get_mut(player) {
                p.powers[power.index()] = IRON_TICS;
            }
            true
        }
        Power::Strength => {
            give_body(ctx, player, 100);
            if let Some(p) = ctx.players.get_mut(player) {
                p.powers[power.index()] = 1;
            }
            true
        }
        Power::AllMap => {
            let already = ctx
                .players
                .get(player)
                .is_some_and(|p| p.powers[power.index()] != 0);
            if already {
                return false; // already got it
            }
            if let Some(p) = ctx.players.get_mut(player) {
                p.powers[power.index()] = 1;
            }
            true
        }
    }
}

/// Mirrors the player's (possibly over-cap) health onto the avatar mobj,
/// like vanilla assignments to `player->mo->health`.
fn sync_avatar_health(ctx: &mut InterContext<'_>, player: usize) {
    let health = ctx.players.get(player).map(|p| p.health);
    let mo = ctx.players.get(player).and_then(|p| p.mo);
    if let (Some(health), Some(mo)) = (health, mo) {
        if let Some(m) = ctx.store.mobj_mut(mo) {
            m.health = health;
        }
    }
}

/// Vanilla `player->message = X`.
fn set_message(ctx: &mut InterContext<'_>, player: usize, message: HudMessage) {
    if let Some(p) = ctx.players.get_mut(player) {
        p.message = Some(message.as_str().to_string());
    }
}

/// The weapon pickup arms of the switch: give the weapon, set the message,
/// use the weapon-up sound.
fn weapon_pickup(
    ctx: &mut InterContext<'_>,
    player: usize,
    weapon: Weapon,
    dropped: bool,
    message: HudMessage,
    sound: &mut SfxId,
) -> bool {
    if !give_weapon(ctx, player, weapon, dropped) {
        return false;
    }
    set_message(ctx, player, message);
    *sound = SfxId::sfx_wpnup;
    true
}

/// The key card arms (they all behave alike, vanilla's six repeated cases).
fn card_pickup(ctx: &mut InterContext<'_>, player: usize, card: Card, message: HudMessage) -> bool {
    let already = ctx
        .players
        .get(player)
        .is_some_and(|p| p.cards[card.index()]);
    if !already {
        set_message(ctx, player, message);
    }
    give_card(ctx, player, card);
    // leave cards for everyone in net games
    if ctx.rules.netgame {
        return false; // keep the card on the floor
    }
    true
}

/// Vanilla `P_TouchSpecialThing` (p_inter.c): the full sprite-keyed switch.
///
/// The caller (the game loop) drains [`super::InterEvent`]s afterwards.
pub fn touch_special_thing(ctx: &mut InterContext<'_>, special: ThinkerId, toucher: ThinkerId) {
    let Some(toucher_mobj) = ctx.store.mobj(toucher) else {
        tracing::warn!("P_TouchSpecialThing: stale toucher handle");
        return;
    };
    let Some(special_mobj) = ctx.store.mobj(special) else {
        // already consumed earlier this tic (double substep or same-frame
        // pickups) — nothing left to react to
        tracing::debug!("P_TouchSpecialThing: stale special handle");
        return;
    };

    // delta = special->z - toucher->z;
    let delta = special_mobj.z - toucher_mobj.z;
    if delta > toucher_mobj.height || delta.to_raw() < -(8 * FRACUNIT) {
        // out of reach
        return;
    }

    // Dead thing touching. Can happen with a sliding player corpse.
    if toucher_mobj.health <= 0 {
        return;
    }

    // Identify by sprite.
    let Some(player_index) = toucher_mobj.player else {
        // monsters never have MF_PICKUP, so this is unreachable in
        // practice; vanilla would dereference NULL
        tracing::warn!("P_TouchSpecialThing: non-player toucher");
        return;
    };

    let mut sound = SfxId::sfx_itemup;
    let sprite = special_mobj.sprite;
    let special_flags = special_mobj.flags;

    let consumed = match sprite {
        // armor
        Sprite::SPR_ARM1 => {
            if !give_armor(ctx, player_index, 1) {
                return;
            }
            set_message(ctx, player_index, HudMessage::GotArmor);
            true
        }
        Sprite::SPR_ARM2 => {
            if !give_armor(ctx, player_index, 2) {
                return;
            }
            set_message(ctx, player_index, HudMessage::GotMega);
            true
        }

        // bonus items
        Sprite::SPR_BON1 => {
            if let Some(p) = ctx.players.get_mut(player_index) {
                p.health += 1; // can go over 100%
                if p.health > 200 {
                    p.health = 200;
                }
            }
            set_message(ctx, player_index, HudMessage::GotHtHBonus);
            sync_avatar_health(ctx, player_index);
            true
        }
        Sprite::SPR_BON2 => {
            if let Some(p) = ctx.players.get_mut(player_index) {
                p.armorpoints += 1; // can go over 100%
                if p.armorpoints > 200 {
                    p.armorpoints = 200;
                }
                if p.armortype == 0 {
                    p.armortype = 1;
                }
            }
            set_message(ctx, player_index, HudMessage::GotArmBonus);
            true
        }
        Sprite::SPR_SOUL => {
            if let Some(p) = ctx.players.get_mut(player_index) {
                p.health += 100;
                if p.health > 200 {
                    p.health = 200;
                }
            }
            set_message(ctx, player_index, HudMessage::GotSuper);
            sync_avatar_health(ctx, player_index);
            sound = SfxId::sfx_getpow;
            true
        }
        Sprite::SPR_MEGA => {
            if !ctx.rules.commercial {
                return; // vanilla: the mega sphere is DOOM II only
            }
            if let Some(p) = ctx.players.get_mut(player_index) {
                p.health = 200;
            }
            sync_avatar_health(ctx, player_index);
            give_armor(ctx, player_index, 2);
            set_message(ctx, player_index, HudMessage::GotMegaSphere);
            sound = SfxId::sfx_getpow;
            true
        }

        // cards
        // leave cards for everyone
        Sprite::SPR_BKEY => card_pickup(ctx, player_index, Card::Blue, HudMessage::GotBlueCard),
        Sprite::SPR_YKEY => card_pickup(ctx, player_index, Card::Yellow, HudMessage::GotYellowCard),
        Sprite::SPR_RKEY => card_pickup(ctx, player_index, Card::Red, HudMessage::GotRedCard),
        Sprite::SPR_BSKU => {
            card_pickup(ctx, player_index, Card::BlueSkull, HudMessage::GotBlueSkull)
        }
        Sprite::SPR_YSKU => card_pickup(
            ctx,
            player_index,
            Card::YellowSkull,
            HudMessage::GotYellowSkull,
        ),
        Sprite::SPR_RSKU => card_pickup(ctx, player_index, Card::RedSkull, HudMessage::GotRedSkull),

        // medikits, heals
        Sprite::SPR_STIM => {
            if !give_body(ctx, player_index, 10) {
                return;
            }
            set_message(ctx, player_index, HudMessage::GotStim);
            true
        }
        Sprite::SPR_MEDI => {
            if !give_body(ctx, player_index, 25) {
                return;
            }
            let health = ctx.players.get(player_index).map(|p| p.health).unwrap_or(0);
            if health < 25 {
                set_message(ctx, player_index, HudMessage::GotMediNeed);
            } else {
                set_message(ctx, player_index, HudMessage::GotMedikit);
            }
            true
        }

        // power ups
        Sprite::SPR_PINV => {
            if !give_power(ctx, player_index, Power::Invulnerability) {
                return;
            }
            set_message(ctx, player_index, HudMessage::GotInvul);
            sound = SfxId::sfx_getpow;
            true
        }
        Sprite::SPR_PSTR => {
            if !give_power(ctx, player_index, Power::Strength) {
                return;
            }
            set_message(ctx, player_index, HudMessage::GotBerserk);
            if let Some(p) = ctx.players.get_mut(player_index) {
                if p.readyweapon != Weapon::Fist {
                    p.pendingweapon = Some(Weapon::Fist);
                }
            }
            sound = SfxId::sfx_getpow;
            true
        }
        Sprite::SPR_PINS => {
            if !give_power(ctx, player_index, Power::Invisibility) {
                return;
            }
            set_message(ctx, player_index, HudMessage::GotInvis);
            sound = SfxId::sfx_getpow;
            true
        }
        Sprite::SPR_SUIT => {
            if !give_power(ctx, player_index, Power::IronFeet) {
                return;
            }
            set_message(ctx, player_index, HudMessage::GotSuit);
            sound = SfxId::sfx_getpow;
            true
        }
        Sprite::SPR_PMAP => {
            if !give_power(ctx, player_index, Power::AllMap) {
                return;
            }
            set_message(ctx, player_index, HudMessage::GotMap);
            sound = SfxId::sfx_getpow;
            true
        }
        Sprite::SPR_PVIS => {
            if !give_power(ctx, player_index, Power::Infrared) {
                return;
            }
            set_message(ctx, player_index, HudMessage::GotVisor);
            sound = SfxId::sfx_getpow;
            true
        }

        // ammo
        Sprite::SPR_CLIP => {
            let dropped = special_flags & MF_DROPPED != 0;
            let loads = if dropped { 0 } else { 1 };
            if !give_ammo(ctx, player_index, AmmoKind::Clip, loads) {
                return;
            }
            set_message(ctx, player_index, HudMessage::GotClip);
            true
        }
        Sprite::SPR_AMMO => {
            if !give_ammo(ctx, player_index, AmmoKind::Clip, 5) {
                return;
            }
            set_message(ctx, player_index, HudMessage::GotClipBox);
            true
        }
        Sprite::SPR_ROCK => {
            if !give_ammo(ctx, player_index, AmmoKind::Missile, 1) {
                return;
            }
            set_message(ctx, player_index, HudMessage::GotRocket);
            true
        }
        Sprite::SPR_BROK => {
            if !give_ammo(ctx, player_index, AmmoKind::Missile, 5) {
                return;
            }
            set_message(ctx, player_index, HudMessage::GotRockBox);
            true
        }
        Sprite::SPR_CELL => {
            if !give_ammo(ctx, player_index, AmmoKind::Cell, 1) {
                return;
            }
            set_message(ctx, player_index, HudMessage::GotCell);
            true
        }
        Sprite::SPR_CELP => {
            if !give_ammo(ctx, player_index, AmmoKind::Cell, 5) {
                return;
            }
            set_message(ctx, player_index, HudMessage::GotCellBox);
            true
        }
        Sprite::SPR_SHEL => {
            if !give_ammo(ctx, player_index, AmmoKind::Shell, 1) {
                return;
            }
            set_message(ctx, player_index, HudMessage::GotShells);
            true
        }
        Sprite::SPR_SBOX => {
            if !give_ammo(ctx, player_index, AmmoKind::Shell, 5) {
                return;
            }
            set_message(ctx, player_index, HudMessage::GotShellBox);
            true
        }
        Sprite::SPR_BPAK => {
            if let Some(p) = ctx.players.get_mut(player_index) {
                if !p.backpack {
                    for ammo in p.maxammo.iter_mut() {
                        *ammo *= 2;
                    }
                    p.backpack = true;
                }
            }
            for ammo in ALL_AMMO {
                give_ammo(ctx, player_index, ammo, 1);
            }
            set_message(ctx, player_index, HudMessage::GotBackpack);
            true
        }

        // weapons
        Sprite::SPR_BFUG => weapon_pickup(
            ctx,
            player_index,
            Weapon::Bfg,
            false,
            HudMessage::GotBfg9000,
            &mut sound,
        ),
        Sprite::SPR_MGUN => weapon_pickup(
            ctx,
            player_index,
            Weapon::Chaingun,
            special_flags & MF_DROPPED != 0,
            HudMessage::GotChaingun,
            &mut sound,
        ),
        Sprite::SPR_CSAW => weapon_pickup(
            ctx,
            player_index,
            Weapon::Chainsaw,
            false,
            HudMessage::GotChainsaw,
            &mut sound,
        ),
        Sprite::SPR_LAUN => weapon_pickup(
            ctx,
            player_index,
            Weapon::MissileLauncher,
            false,
            HudMessage::GotLauncher,
            &mut sound,
        ),
        Sprite::SPR_PLAS => weapon_pickup(
            ctx,
            player_index,
            Weapon::PlasmaRifle,
            false,
            HudMessage::GotPlasma,
            &mut sound,
        ),
        Sprite::SPR_SHOT => weapon_pickup(
            ctx,
            player_index,
            Weapon::Shotgun,
            special_flags & MF_DROPPED != 0,
            HudMessage::GotShotgun,
            &mut sound,
        ),
        Sprite::SPR_SGN2 => weapon_pickup(
            ctx,
            player_index,
            Weapon::SuperShotgun,
            special_flags & MF_DROPPED != 0,
            HudMessage::GotShotgun2,
            &mut sound,
        ),

        unknown => {
            // vanilla: I_Error (abort). We warn loudly and leave the item;
            // one bad map thing cannot take the process down.
            tracing::warn!("P_TouchSpecialThing: unknown gettable sprite {unknown:?}");
            return;
        }
    };

    if !consumed {
        return; // net game weapon/key: it stays on the floor
    }

    // count items for the intermission tally
    if special_flags & MF_COUNTITEM != 0 {
        if let Some(p) = ctx.players.get_mut(player_index) {
            p.itemcount += 1;
        }
    }

    ctx.store.remove_mobj(special, ctx.leveltime);
    if let Some(p) = ctx.players.get_mut(player_index) {
        p.bonuscount += BONUS_ADD;
    }
    if player_index == ctx.consoleplayer {
        ctx.events.push(InterEvent::Sound { sfx: sound });
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;
    use crate::info::{MF_DROPPED, MF_SHADOW};
    use crate::map::Map;
    use crate::mobj::MobjStore;
    use crate::player::{Card, Player, PlayerState, Weapon};
    use crate::rng::DoomRng;
    use crate::thinker::ThinkerId;

    fn fixture(seed: usize) -> (Map, MobjStore, Vec<Player>, DoomRng, TestWorld) {
        let map = one_sector_map(0);
        let store = MobjStore::new();
        let players = players();
        let rng = DoomRng::with_seed(seed);
        let world = TestWorld::new(&map);
        (map, store, players, rng, world)
    }

    /// Runs `f` with a context, collecting both event channels.
    fn run<F>(
        map: &Map,
        store: &mut MobjStore,
        players: &mut Vec<Player>,
        rng: &mut DoomRng,
        world: &mut TestWorld,
        f: F,
    ) -> (Vec<crate::enemy::GameEvent>, Vec<super::InterEvent>)
    where
        F: FnOnce(&mut super::InterContext<'_>),
    {
        let mut game_events = Vec::new();
        let mut events = Vec::new();
        {
            let mut ctx = test_context(
                map,
                store,
                players,
                rng,
                world,
                &mut game_events,
                &mut events,
            );
            f(&mut ctx);
        }
        (game_events, events)
    }

    /// Spawns an item-ish mobj with the given sprite/`MT_MISC2`-style type
    /// snapped to the floor, and returns its handle.
    fn spawn_special(
        store: &mut MobjStore,
        map: &Map,
        rng: &mut DoomRng,
        mobj_type: crate::info::MobjType,
        x: i32,
        y: i32,
    ) -> ThinkerId {
        spawn_thing(store, map, rng, mobj_type, x, y)
    }

    #[test]
    fn clip_pickup_gives_one_load() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        let clip = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_CLIP,
            32,
            32,
        );

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, clip, avatar);
            },
        );

        assert_eq!(players[0].ammo[AmmoKind::Clip.index()], 10);
        assert!(store.mobj(clip).is_none()); // consumed
        assert!(players[0].bonuscount >= 6);
    }

    #[test]
    fn dropped_clip_gives_half_a_load() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        let clip = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_CLIP,
            32,
            32,
        );
        store.mobj_mut(clip).unwrap().flags |= MF_DROPPED;

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, clip, avatar);
            },
        );

        assert_eq!(players[0].ammo[AmmoKind::Clip.index()], 5);
    }

    #[test]
    fn ammo_box_gives_five_loads() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        let box_of_ammo = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC17,
            32,
            32,
        );

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, box_of_ammo, avatar);
            },
        );

        assert_eq!(players[0].ammo[AmmoKind::Clip.index()], 50);
    }

    #[test]
    fn ammo_at_max_returns_false_and_keeps_the_box() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].ammo[AmmoKind::Clip.index()] = players[0].maxammo[AmmoKind::Clip.index()];
        let box_of_ammo = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC17,
            32,
            32,
        );

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, box_of_ammo, avatar);
            },
        );

        assert_eq!(players[0].ammo[AmmoKind::Clip.index()], 200);
        assert!(store.mobj(box_of_ammo).is_some()); // not taken
    }

    #[test]
    fn ammo_accumulates_up_to_max() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].ammo[AmmoKind::Shell.index()] = 45;
        let shells = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC23,
            32,
            32,
        );

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, shells, avatar);
            },
        );

        assert_eq!(players[0].ammo[AmmoKind::Shell.index()], 50); // clamped, not 55
    }

    #[test]
    fn baby_skill_doubles_ammo() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].weaponowned[Weapon::Pistol.index()] = true;
        let clip = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_CLIP,
            32,
            32,
        );

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                ctx.rules.skill = crate::mobj::Skill::Baby;
                touch_special_thing(ctx, clip, avatar);
            },
        );

        assert_eq!(players[0].ammo[AmmoKind::Clip.index()], 20);
    }

    #[test]
    fn nightmare_skill_doubles_ammo() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);

        let clip = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_CLIP,
            32,
            32,
        );

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                ctx.rules.skill = crate::mobj::Skill::Nightmare;
                touch_special_thing(ctx, clip, avatar);
            },
        );

        assert_eq!(players[0].ammo[AmmoKind::Clip.index()], 20);
    }

    #[test]
    fn clip_from_fist_pickup_selects_pistol() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].weaponowned[Weapon::Pistol.index()] = true;
        players[0].ammo[AmmoKind::Clip.index()] = 0;
        let clip = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_CLIP,
            32,
            32,
        );

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, clip, avatar);
            },
        );

        assert_eq!(players[0].pendingweapon, Some(Weapon::Pistol));
    }

    #[test]
    fn clip_from_fist_prefers_chaingun_when_owned() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].weaponowned[Weapon::Pistol.index()] = true;
        players[0].weaponowned[Weapon::Chaingun.index()] = true;
        let clip = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_CLIP,
            32,
            32,
        );

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, clip, avatar);
            },
        );

        assert_eq!(players[0].pendingweapon, Some(Weapon::Chaingun));
    }

    #[test]
    fn shell_from_pistol_promotes_to_shotgun() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].readyweapon = Weapon::Pistol;
        players[0].weaponowned[Weapon::Shotgun.index()] = true;
        let shells = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC23,
            32,
            32,
        );

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, shells, avatar);
            },
        );

        assert_eq!(players[0].pendingweapon, Some(Weapon::Shotgun));
    }

    #[test]
    fn old_ammo_keeps_the_current_weapon() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].ammo[AmmoKind::Clip.index()] = 3; // had some already
        players[0].weaponowned[Weapon::Pistol.index()] = true;
        let clip = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_CLIP,
            32,
            32,
        );

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, clip, avatar);
            },
        );

        assert_eq!(players[0].pendingweapon, None);
    }

    #[test]
    fn backpack_doubles_capacity_once_and_refills() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].ammo = [7; 4]; // non-zero to skip weapon promotion

        let pack1 = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC24,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, pack1, avatar);
            },
        );

        assert_eq!(players[0].maxammo, [400, 100, 600, 100]);
        assert!(players[0].backpack);
        // one load of each was stuffed in
        assert_eq!(players[0].ammo, [17, 11, 27, 8]);

        let pack2 = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC24,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, pack2, avatar);
            },
        );

        assert_eq!(players[0].maxammo, [400, 100, 600, 100]); // no double-doubling
        assert_eq!(players[0].ammo, [27, 15, 47, 9]); // nothing above max
    }

    #[test]
    fn weapon_pickup_sets_owned_and_pending() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);

        let shotgun = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_SHOTGUN,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, shotgun, avatar);
            },
        );

        assert!(players[0].weaponowned[Weapon::Shotgun.index()]);
        assert_eq!(players[0].pendingweapon, Some(Weapon::Shotgun));
        assert_eq!(players[0].ammo[AmmoKind::Shell.index()], 8); // 2 loads
    }

    #[test]
    fn dropped_weapon_gives_one_load() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);

        let shotgun = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_SHOTGUN,
            32,
            32,
        );
        store.mobj_mut(shotgun).unwrap().flags |= MF_DROPPED;
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, shotgun, avatar);
            },
        );

        assert_eq!(players[0].ammo[AmmoKind::Shell.index()], 4); // 1 load
    }

    #[test]
    fn owned_weapon_plus_full_ammo_is_not_consumed() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].weaponowned[Weapon::Shotgun.index()] = true;
        players[0].ammo[AmmoKind::Shell.index()] = players[0].maxammo[AmmoKind::Shell.index()];

        let shotgun = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_SHOTGUN,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, shotgun, avatar);
            },
        );

        assert!(store.mobj(shotgun).is_some()); // still on the floor
    }

    #[test]
    fn netgame_keeps_placed_weapons_forever() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);

        let chaingun = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_CHAINGUN,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                ctx.rules.netgame = true;
                touch_special_thing(ctx, chaingun, avatar);
            },
        );

        // picked up and kept for others: netgame leaves placed weapons
        assert!(players[0].weaponowned[Weapon::Chaingun.index()]);
        assert!(store.mobj(chaingun).is_some());
        assert_eq!(players[0].bonuscount, 6); // netgame branch adds the bonus

        // second pickup of an owned weapon: not taken
        let chaingun2 = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_CHAINGUN,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                ctx.rules.netgame = true;
                touch_special_thing(ctx, chaingun2, avatar);
            },
        );
        assert!(store.mobj(chaingun2).is_some());
    }

    #[test]
    fn netgame_gives_two_clips_and_duel_gives_five() {
        for (mode, want) in [
            (crate::args::DeathmatchMode::Off, 8),   // 2 loads * 4 shells
            (crate::args::DeathmatchMode::Duel, 20), // 5 loads * 4 shells
        ] {
            let (map, mut store, mut players, mut rng, mut world) = fixture(3);
            let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);

            let shotgun = spawn_special(
                &mut store,
                &map,
                &mut rng,
                crate::info::MobjType::MT_SHOTGUN,
                32,
                32,
            );
            run(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                |ctx| {
                    ctx.rules.netgame = true;
                    ctx.deathmatch = mode;
                    touch_special_thing(ctx, shotgun, avatar);
                },
            );

            assert_eq!(players[0].ammo[AmmoKind::Shell.index()], want);
        }
    }

    #[test]
    fn altdeath_treats_weapons_normally() {
        // deathmatch == 2 excludes the "leave forever" branch
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);

        let shotgun = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_SHOTGUN,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                ctx.rules.netgame = true;
                ctx.deathmatch = crate::args::DeathmatchMode::Alt;
                touch_special_thing(ctx, shotgun, avatar);
            },
        );

        assert!(store.mobj(shotgun).is_none()); // consumed like single player
    }

    #[test]
    fn keys_are_taken_in_single_player_left_in_netgame() {
        // single player: card consumed
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        let key = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC5,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, key, avatar);
            },
        );
        assert!(players[0].cards[Card::Red.index()]);
        assert!(store.mobj(key).is_none());

        // netgame: card stays, bonus not repeated
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        let key = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC5,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                ctx.rules.netgame = true;
                touch_special_thing(ctx, key, avatar);
            },
        );
        assert!(players[0].cards[Card::Red.index()]);
        assert!(store.mobj(key).is_some());
    }

    #[test]
    fn key_message_only_on_first_pickup() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].cards[Card::Blue.index()] = true;
        players[0].message = None;

        let key = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC4,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, key, avatar);
            },
        );

        assert_eq!(players[0].message, None); // already had it: no message
    }

    #[test]
    fn soul_sphere_takes_health_to_200() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].health = 150;

        let soul = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC12,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, soul, avatar);
            },
        );

        assert_eq!(players[0].health, 200); // capped, not 250
        assert_eq!(store.mobj(avatar).unwrap().health, 200);
    }

    #[test]
    fn stimpack_at_full_health_is_not_taken() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].health = 100;

        let stim = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC10,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, stim, avatar);
            },
        );

        assert_eq!(players[0].health, 100);
        assert!(store.mobj(stim).is_some()); // still on the floor
    }

    #[test]
    fn medikit_message_reflects_need() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].health = 10;

        let medi = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC11,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, medi, avatar);
            },
        );

        assert_eq!(players[0].health, 10 + 25);
        // post-pickup this is never below 25, so vanilla shows the normal
        // medikit message here
        assert_eq!(
            players[0].message.as_deref(),
            Some(crate::hud::HudMessage::GotMedikit.as_str())
        );
    }

    #[test]
    fn berserk_sets_strength_and_heals_to_100() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].health = 25;

        let berserk = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC13,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, berserk, avatar);
            },
        );

        assert_eq!(players[0].powers[Power::Strength.index()], 1);
        assert_eq!(players[0].health, 100); // +100 clamped at MAXHEALTH
    }

    #[test]
    fn berserk_at_full_health_does_not_overheal() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].health = 100;

        let berserk = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC13,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, berserk, avatar);
            },
        );

        assert_eq!(players[0].health, 100);
        assert_eq!(players[0].powers[Power::Strength.index()], 1);
    }

    #[test]
    fn invulnerability_sets_tics() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);

        let inv = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_INV,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, inv, avatar);
            },
        );

        assert_eq!(
            players[0].powers[Power::Invulnerability.index()],
            crate::inter::INVUL_TICS
        );
    }

    #[test]
    fn invisibility_sets_tic_counter_and_shadow_flag() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);

        let sphere = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_INS,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, sphere, avatar);
            },
        );

        assert_eq!(
            players[0].powers[Power::Invisibility.index()],
            crate::inter::INVIS_TICS
        );
        assert_ne!(store.mobj(avatar).unwrap().flags & MF_SHADOW, 0);
    }

    #[test]
    fn allmap_already_owned_is_not_consumed() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].powers[Power::AllMap.index()] = 1;

        let map_item = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC15,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, map_item, avatar);
            },
        );

        assert!(store.mobj(map_item).is_some()); // left behind
    }

    #[test]
    fn armor_pickup_requires_upgrade() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);

        // green armor at 100 points: armors are not worse than current
        players[0].armorpoints = 100;
        players[0].armortype = 1;
        let armor = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC0,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, armor, avatar);
            },
        );
        assert!(store.mobj(armor).is_some()); // not taken

        // mega armor picks up fine from zero
        let mega = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC1,
            32,
            32,
        );
        players[0].armorpoints = 0;
        players[0].armortype = 0;
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, mega, avatar);
            },
        );
        assert!(store.mobj(mega).is_none());
        assert_eq!(players[0].armorpoints, 200);
        assert_eq!(players[0].armortype, 2);
    }

    #[test]
    fn health_bonus_overcaps_the_bar_but_not_200() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].health = 199;

        let bonus = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC2,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, bonus, avatar);
            },
        );

        assert_eq!(players[0].health, 200); // capped, not 201
    }

    #[test]
    fn itemcount_increments_for_counted_items_only() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);

        // MT_MISC2 (health bonus) counts; MT_CLIP does not
        let bonus = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC2,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, bonus, avatar);
            },
        );
        assert_eq!(players[0].itemcount, 1);

        let clip = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_CLIP,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, clip, avatar);
            },
        );
        assert_eq!(players[0].itemcount, 1); // clips are not counted
    }

    #[test]
    fn sound_goes_to_console_player_only() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);

        let clip = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_CLIP,
            32,
            32,
        );
        let (game_events, events) = run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                ctx.consoleplayer = 1; // we are player 2; the console is someone else
                touch_special_thing(ctx, clip, avatar);
            },
        );

        assert_eq!(events.len(), 0); // no local sound
        assert_eq!(game_events.len(), 0);
    }

    #[test]
    fn out_of_reach_items_are_not_taken() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        // put the item far above the player
        let bonus = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC2,
            32,
            32,
        );
        store.mobj_mut(bonus).unwrap().z = crate::fixed::Fixed::from_int(10_000);

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, bonus, avatar);
            },
        );

        assert!(store.mobj(bonus).is_some()); // untouched
        assert_eq!(players[0].itemcount, 0);
    }

    #[test]
    fn dead_toucher_takes_nothing() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        players[0].playerstate = PlayerState::Dead;
        store.mobj_mut(avatar).unwrap().health = 0;

        let bonus = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MISC2,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                touch_special_thing(ctx, bonus, avatar);
            },
        );

        assert!(store.mobj(bonus).is_some());
    }

    #[test]
    fn mega_sphere_is_commercial_only() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);

        // registered game: the mega sphere does not exist
        let mega = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MEGA,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                ctx.rules.commercial = false;
                touch_special_thing(ctx, mega, avatar);
            },
        );
        assert!(store.mobj(mega).is_some()); // left behind
        assert_eq!(players[0].health, 100);

        // commercial game: health and armor go to 200
        let mega2 = spawn_special(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_MEGA,
            32,
            32,
        );
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                ctx.rules.commercial = true;
                touch_special_thing(ctx, mega2, avatar);
            },
        );
        assert!(store.mobj(mega2).is_none());
        assert_eq!(players[0].health, 200);
        assert_eq!(players[0].armorpoints, 200);
        assert_eq!(players[0].armortype, 2);
    }
}
