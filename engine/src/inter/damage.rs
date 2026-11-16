//! Damage and death, ported from `p_inter.c` (`P_DamageMobj`, `P_KillMobj`).
//!
//! Damage flows in from the attack layers (missiles, melee, splash, the
//! teleport stomp); death flows out through [`kill_mobj`] on both paths.
//! The deathstate/painstate chains run through the enemy layer's
//! dispatcher, exactly like vanilla's inline `P_SetMobjState`.

use crate::bsp::point_to_angle_2;
use crate::enemy::set_mobj_state_and_run_actions;
use crate::fixed::{FRACUNIT, Fixed};
use crate::info::{
    MF_CORPSE, MF_COUNTKILL, MF_DROPOFF, MF_DROPPED, MF_FLOAT, MF_JUSTHIT, MF_NOCLIP, MF_NOGRAVITY,
    MF_SKULLFLY, MF_SOLID, MOBJ_INFO, MobjType, StateId,
};
use crate::mobj::{SpawnSpec, SpawnZ};
use crate::movement::MF_SHOOTABLE;
use crate::player::{PlayerState, Power, Weapon};
use crate::tables::{ANGLE_TO_FINE_SHIFT, Angle, FINE_COSINE, FINE_SINE};
use crate::thinker::ThinkerId;

use super::{BASE_THRESHOLD, InterContext, InterEvent};

/// Runs a state through the enemy dispatcher and forwards the A_* side
/// effects into the shared [`crate::enemy::GameEvent`] channel. This is the
/// counterpart of vanilla's inline `P_SetMobjState` for the kill/pain paths.
fn set_state_and_forward(ctx: &mut InterContext<'_>, target: ThinkerId, state: StateId) {
    let mut enemy_ctx = ctx.enemy_context();
    set_mobj_state_and_run_actions(&mut enemy_ctx, target, state);
}

/// Vanilla `P_KillMobj` (p_inter.c): transition a dead mobj into its death
/// state, count kills/frags, drop the monster's item, handle player death.
pub fn kill_mobj(ctx: &mut InterContext<'_>, source: Option<ThinkerId>, target: ThinkerId) {
    let Some(target_mobj) = ctx.store.mobj(target) else {
        tracing::warn!("P_KillMobj: stale target handle");
        return;
    };

    let target_type = target_mobj.mobj_type;
    let target_flags = target_mobj.flags;
    let target_player = target_mobj.player;
    let target_x = target_mobj.x;
    let target_y = target_mobj.y;
    let target_health = target_mobj.health;

    // target->flags &= ~(MF_SHOOTABLE|MF_FLOAT|MF_SKULLFLY); MF_NOGRAVITY
    // stays for skulls.
    let mut new_flags = target_flags & !(MF_SHOOTABLE | MF_FLOAT | MF_SKULLFLY);
    if target_type != MobjType::MT_SKULL {
        new_flags &= !MF_NOGRAVITY;
    }
    new_flags |= MF_CORPSE | MF_DROPOFF;
    let new_height = Fixed::from_raw(target_mobj.height.to_raw() >> 2);
    if let Some(t) = ctx.store.mobj_mut(target) {
        t.flags = new_flags;
        t.height = new_height;
    }

    // count for intermission
    let source_player = source
        .and_then(|s| ctx.store.mobj(s))
        .and_then(|m| m.player);
    if let Some(src_pi) = source_player {
        if target_flags & MF_COUNTKILL != 0 {
            if let Some(p) = ctx.players.get_mut(src_pi) {
                p.killcount += 1;
            }
        }
        if let Some(tgt_pi) = target_player {
            if let Some(p) = ctx.players.get_mut(src_pi) {
                p.frags[tgt_pi] += 1;
            }
        }
    } else if !ctx.rules.netgame && (target_flags & MF_COUNTKILL != 0) {
        // count all monster deaths, even those caused by other monsters
        if let Some(p) = ctx.players.get_mut(0) {
            p.killcount += 1;
        }
    }

    if let Some(tgt_pi) = target_player {
        // count environment kills against you
        if source.is_none() {
            if let Some(p) = ctx.players.get_mut(tgt_pi) {
                p.frags[tgt_pi] += 1;
            }
        }

        if let Some(t) = ctx.store.mobj_mut(target) {
            t.flags &= !MF_SOLID;
        }
        if let Some(p) = ctx.players.get_mut(tgt_pi) {
            p.playerstate = PlayerState::Dead;
        }
        ctx.events.push(InterEvent::DropWeapon { player: tgt_pi });

        if tgt_pi == ctx.consoleplayer && ctx.automap_active {
            // don't die in auto map, switch view prior to dying
            ctx.events.push(InterEvent::StopAutomap);
        }
    }

    // go into the xdeath state when blown apart, otherwise the regular
    // death state.
    let info = &MOBJ_INFO[target_type as usize];
    let death_state = if target_health < -info.spawnhealth && info.xdeathstate != StateId::S_NULL {
        info.xdeathstate
    } else {
        info.deathstate
    };
    // vanilla P_SetMobjState (runs the state's A_* actions inline)
    set_state_and_forward(ctx, target, death_state);

    // target->tics -= P_Random()&3; keep the state on screen
    if let Some(t) = ctx.store.mobj_mut(target) {
        t.tics -= i32::from(ctx.rng.next()) & 3;
        if t.tics < 1 {
            t.tics = 1;
        }
    }

    // Drop stuff.
    // This determines the kind of object spawned
    // during the death frame of a thing.
    let dropped = match target_type {
        MobjType::MT_WOLFSS | MobjType::MT_POSSESSED => Some(MobjType::MT_CLIP),
        MobjType::MT_SHOTGUY => Some(MobjType::MT_SHOTGUN),
        MobjType::MT_CHAINGUY => Some(MobjType::MT_CHAINGUN),
        _ => None,
    };
    if let Some(item) = dropped {
        let id = ctx.store.spawn_mobj(
            ctx.map,
            ctx.rng,
            SpawnSpec {
                skill: ctx.rules.skill,
                x: target_x,
                y: target_y,
                z: SpawnZ::Floor,
                mobj_type: item,
            },
        );
        if let Some(mo) = ctx.store.mobj_mut(id) {
            mo.flags |= MF_DROPPED; // special versions of items
        }
    }
}

/// Vanilla `P_DamageMobj` (p_inter.c): damage both enemies and players.
///
/// `inflictor` is the thing that caused the damage (missile, melee
/// source), `source` is the thing to chase afterwards. Source and
/// inflictor are the same for melee attacks. Either can be `None` (slime,
/// barrel explosions).
///
/// Armor absorbs a third (green) or half (blue) of the damage; the god
/// cheat and the invulnerability power reject damage below 1000 outright
/// (after the thrust is applied, like vanilla).
pub fn damage_mobj(
    ctx: &mut InterContext<'_>,
    target: ThinkerId,
    inflictor: Option<ThinkerId>,
    source: Option<ThinkerId>,
    damage: i32,
) {
    // shouldn't happen...
    let Some(target_mobj) = ctx.store.mobj(target) else {
        tracing::warn!("P_DamageMobj: stale target handle");
        return;
    };
    // pull everything off the mobj up front so the borrow is over before we
    // start mutating the store
    let target_flags = target_mobj.flags;
    let target_health_before = target_mobj.health;
    let target_x = target_mobj.x;
    let target_y = target_mobj.y;
    let target_z = target_mobj.z;
    let target_mass = target_mobj.info().mass;
    let target_player = target_mobj.player;
    let target_skullfly = target_flags & MF_SKULLFLY != 0;
    let target_noclip = target_flags & MF_NOCLIP != 0;

    if target_flags & MF_SHOOTABLE == 0 {
        return;
    }
    if target_health_before <= 0 {
        return; // already dead
    }

    if target_skullfly {
        if let Some(t) = ctx.store.mobj_mut(target) {
            t.momx = Fixed::ZERO;
            t.momy = Fixed::ZERO;
            t.momz = Fixed::ZERO;
        }
    }
    let mut damage = damage;
    if target_player.is_some() && ctx.rules.skill == crate::mobj::Skill::Baby {
        damage >>= 1; // take half damage in trainer mode
    }

    // closest combat weapons should not inflict thrust and push the victim
    // out of reach; thus kick away unless using the chainsaw.
    let source_is_chainsaw = source
        .and_then(|s| ctx.store.mobj(s))
        .and_then(|m| m.player)
        .and_then(|pi| ctx.players.get(pi))
        .is_some_and(|p| p.readyweapon == Weapon::Chainsaw);

    if let Some(infl) = inflictor {
        if !target_noclip && !source_is_chainsaw {
            if let Some(infl_mobj) = ctx.store.mobj(infl) {
                let (i_x, i_y, i_z) = (infl_mobj.x, infl_mobj.y, infl_mobj.z);

                let ang = point_to_angle_2(i_x, i_y, target_x, target_y);

                // 64-bit intermediate: vanilla wraps 32-bit fixed_t here for
                // absurd damage values (the teleport stomp). See the module
                // docs for the divergence note.
                let thrust_raw = ((i64::from(damage) * i64::from(FRACUNIT >> 3) * 100)
                    / i64::from(target_mass.max(1)))
                .clamp(i64::from(i32::MIN), i64::from(i32::MAX))
                    as i32;
                let mut thrust = Fixed::from_raw(thrust_raw);
                let mut ang = ang;

                // make fall forwards sometimes
                if damage < 40
                    && damage > target_health_before
                    && target_z.to_raw() as i64 - i_z.to_raw() as i64 > 64 * i64::from(FRACUNIT)
                    && (i32::from(ctx.rng.next()) & 1) != 0
                {
                    ang += Angle::ANG180;
                    thrust = Fixed::from_raw(thrust.to_raw().wrapping_mul(4)); // vanilla wraps
                }

                let fine = (ang.to_raw() >> ANGLE_TO_FINE_SHIFT) as usize;
                if let Some(t) = ctx.store.mobj_mut(target) {
                    t.momx += thrust * FINE_COSINE[fine];
                    t.momy += thrust * FINE_SINE[fine];
                }
            } else {
                tracing::warn!("P_DamageMobj: stale inflictor handle; skipping thrust");
            }
        }
    }

    // player specific
    if let Some(pi) = target_player {
        let hell_hack = sector_special(ctx, target) == Some(11);
        let player_health = ctx.players.get(pi).map(|p| p.health).unwrap_or(0);
        if hell_hack && damage >= player_health {
            // damage = target->health - 1;  // end of game hell hack
            damage = player_health - 1;
        }

        // Below certain threshold, ignore damage in GOD mode or with the
        // INVUL power.
        let invulnerable = ctx.players.get(pi).is_some_and(|p| {
            p.cheats & crate::player::CF_GODMODE != 0
                || p.powers[Power::Invulnerability.index()] != 0
        });
        if damage < 1000 && invulnerable {
            return;
        }

        // armor absorbs green 1/3, blue 1/2; depleted armor caps the save
        if let Some(p) = ctx.players.get_mut(pi) {
            if p.armortype > 0 {
                let mut saved = if p.armortype == 1 {
                    damage / 3
                } else {
                    damage / 2
                };
                if p.armorpoints <= saved {
                    // armor is used up
                    saved = p.armorpoints;
                    p.armortype = 0;
                }
                p.armorpoints -= saved;
                damage -= saved;
            }
        }

        // vanilla: player->health -= damage;  // mirror mobj health here for
        // Dave. Note the comment means the opposite of what it reads like:
        // the *player* field mirrors the mobj. The mobj health is
        // decremented unconditionally in the shared tail below, exactly
        // like vanilla ("for Dave") — do not sync them here or the avatar
        // loses the hit twice.
        if let Some(p) = ctx.players.get_mut(pi) {
            p.health -= damage;
            if p.health < 0 {
                p.health = 0;
            }
            p.attacker = source;
            p.damagecount += damage; // add damage after armor / invuln
            if p.damagecount > 100 {
                p.damagecount = 100; // teleport stomp does 10k points...
            }
        }

        // vanilla: I_Tactile(40,10,40+temp*2) — no haptic backend in this
        // tree; the rest of the path is unaffected.
    }

    // do the damage
    // (target->health goes negative; the clamped copy lives on the player)
    let died = match ctx.store.mobj_mut(target) {
        Some(t) => {
            t.health -= damage;
            t.health <= 0
        }
        None => return,
    };
    if died {
        kill_mobj(ctx, source, target);
        return;
    }

    // pain chance
    let pain_state = {
        let info = target_mobj_info_for(ctx, target);
        let roll = i32::from(ctx.rng.next());
        let in_pain = match info {
            Some(i) => roll < i.painchance,
            None => false,
        };
        if in_pain {
            let t_flags = ctx.store.mobj(target).map(|t| t.flags).unwrap_or(0);
            if t_flags & MF_SKULLFLY == 0 {
                if let Some(t) = ctx.store.mobj_mut(target) {
                    t.flags |= MF_JUSTHIT; // fight back!
                }
                info.map(|i| i.painstate)
            } else {
                None
            }
        } else {
            None
        }
    };
    if let Some(state) = pain_state {
        set_state_and_forward(ctx, target, state);
    }

    // we're awake now...
    if let Some(t) = ctx.store.mobj_mut(target) {
        t.reactiontime = 0;
    }

    // if not intent on another player, chase after this one
    let (threshold, target_type) = ctx
        .store
        .mobj(target)
        .map(|t| (t.threshold, t.mobj_type))
        .unwrap_or((1, MobjType::MT_PLAYER));
    let source_is_vile = source
        .and_then(|s| ctx.store.mobj(s))
        .is_some_and(|m| m.mobj_type == MobjType::MT_VILE);
    if (threshold == 0 || target_type == MobjType::MT_VILE)
        && source.is_some()
        && source != Some(target)
        && !source_is_vile
    {
        if let Some(t) = ctx.store.mobj_mut(target) {
            t.target = source;
            t.threshold = BASE_THRESHOLD;
        }
        // if the target is in its spawn state, go to the see state
        let (spawn_state, seestate, cur_state) = match ctx.store.mobj(target) {
            Some(t) => (t.info().spawnstate, t.info().seestate, t.state),
            None => return,
        };
        if cur_state == spawn_state && seestate != StateId::S_NULL {
            set_state_and_forward(ctx, target, seestate);
        }
    }
}

/// Reads the `mobjinfo` entry for a live mobj. Split out so the pain path
/// doesn't re-borrow the store in the middle of a mutation.
fn target_mobj_info_for(
    ctx: &InterContext<'_>,
    target: ThinkerId,
) -> Option<&'static crate::info::MobjInfo> {
    ctx.store.mobj(target).map(|t| t.info())
}

/// The `special` of the sector the target stands in, vanilla's
/// `target->subsector->sector->special`.
fn sector_special(ctx: &InterContext<'_>, target: ThinkerId) -> Option<i16> {
    let subsector = ctx.store.mobj(target)?.subsector;
    let sector = ctx
        .map
        .subsectors
        .get(subsector)
        .and_then(|ss| ss.sector)
        .or_else(|| ctx.store.mobj(target)?.sector)?;
    Some(ctx.map.sectors[sector].special)
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;
    use crate::info::{MF_CORPSE, MF_DROPPED, MF_JUSTHIT, MF_NOGRAVITY, MF_SOLID, MOBJ_INFO};
    use crate::map::Map;
    use crate::mobj::MobjStore;
    use crate::player::{Player, Power, Weapon};
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

    fn spawn_possessed(
        store: &mut MobjStore,
        map: &Map,
        rng: &mut DoomRng,
        x: i32,
        y: i32,
    ) -> ThinkerId {
        spawn_thing(store, map, rng, crate::info::MobjType::MT_POSSESSED, x, y)
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

    #[test]
    fn armor_green_saves_a_third() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 64, 64);
        players[0].armortype = 1;
        players[0].armorpoints = 90;

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, avatar, None, None, 30);
            },
        );

        assert_eq!(players[0].armorpoints, 80); // 90 - 30/3
        assert_eq!(players[0].health, 80); // 100 - 20
        assert_eq!(players[0].armortype, 1);
        assert_eq!(store.mobj(avatar).unwrap().health, 80);
    }

    #[test]
    fn mega_armor_blue_class_absorbs_half() {
        // mega armor is make-believe name for armortype 2 + 200 points;
        // vanilla treats it exactly like blue armor
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 64, 64);
        players[0].armortype = 2;
        players[0].armorpoints = 200;

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, avatar, None, None, 40);
            },
        );

        assert_eq!(players[0].armorpoints, 180); // 200 - 40/2
        assert_eq!(players[0].health, 80); // 100 - 20
        assert_eq!(players[0].armortype, 2);
    }

    #[test]
    fn armor_blue_saves_a_half() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 64, 64);
        players[0].armortype = 2;
        players[0].armorpoints = 100;

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, avatar, None, None, 30);
            },
        );

        assert_eq!(players[0].armorpoints, 85); // 100 - 30/2
        assert_eq!(players[0].health, 85); // 100 - 15
    }

    #[test]
    fn armor_used_up_caps_the_save_and_clears_class() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 64, 64);
        players[0].armortype = 1;
        players[0].armorpoints = 5;

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, avatar, None, None, 30);
            },
        );

        assert_eq!(players[0].armorpoints, 0); // all of it went
        assert_eq!(players[0].armortype, 0); // class cleared
        assert_eq!(players[0].health, 75); // 100 - 25
    }

    #[test]
    fn armor_save_exactly_equal_to_pool_is_used_up() {
        // vanilla: player->armorpoints <= saved -> armor used up
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 64, 64);
        players[0].armortype = 1;
        players[0].armorpoints = 10;

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, avatar, None, None, 30); // 30/3 == 10 == armorpoints
            },
        );

        assert_eq!(players[0].armorpoints, 0);
        assert_eq!(players[0].armortype, 0);
        assert_eq!(players[0].health, 80);
    }

    #[test]
    fn monster_takes_full_damage() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let possessed = spawn_possessed(&mut store, &map, &mut rng, 32, 32);

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, possessed, None, None, 10);
            },
        );

        let m = store.mobj(possessed).unwrap();
        assert_eq!(m.health, 10); // 20 - 10
        assert_ne!(m.flags & MF_SOLID, 0); // only players lose solidity
    }

    #[test]
    fn kill_switches_to_deathstate_and_keeps_corpse() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let possessed = spawn_possessed(&mut store, &map, &mut rng, 32, 32);

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, possessed, None, None, 25); // 20 - 25 = -5
            },
        );

        let m = store.mobj(possessed).unwrap();
        assert_eq!(m.health, -5);
        assert_eq!(m.state, crate::info::StateId::S_POSS_DIE1);
        assert_eq!(m.flags & crate::movement::MF_SHOOTABLE, 0); // shootable cleared
        assert_ne!(m.flags & MF_CORPSE, 0);
        assert_ne!(m.flags & MF_DROPOFF, 0);
        assert_eq!(m.height.to_raw(), (56 * crate::fixed::FRACUNIT) >> 2);
        assert!(m.tics >= 1);
    }

    #[test]
    fn big_damage_uses_xdeathstate() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let possessed = spawn_possessed(&mut store, &map, &mut rng, 32, 32);

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, possessed, None, None, 45); // 20 - 45 = -25 < -20
            },
        );

        let m = store.mobj(possessed).unwrap();
        assert_eq!(m.health, -25);
        assert_eq!(m.state, crate::info::StateId::S_POSS_XDIE1);
    }

    #[test]
    fn player_death_clears_solid_and_reports_drop_weapon() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 64, 64);

        let (game_events, events) = run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, avatar, None, None, 1000);
            },
        );
        let _ = game_events;

        assert_eq!(players[0].playerstate, crate::player::PlayerState::Dead);
        let m = store.mobj(avatar).unwrap();
        assert_eq!(m.flags & MF_SOLID, 0);
        assert_eq!(players[0].frags[0], 1); // environment suicide counts against you
        assert!(events.contains(&super::InterEvent::DropWeapon { player: 0 }));
        // automap is not up in this test: no stop event
        assert!(!events.contains(&super::InterEvent::StopAutomap));
    }

    #[test]
    fn console_player_death_with_automap_stops_automap() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 64, 64);

        let (game_events, events) = run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                ctx.automap_active = true;
                damage_mobj(ctx, avatar, None, None, 1000);
            },
        );
        let _ = game_events;

        assert!(events.contains(&super::InterEvent::StopAutomap));
    }

    #[test]
    fn kill_by_another_player_increments_frags() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let killer = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        let victim = spawn_player(&mut store, &map, &mut rng, &mut players, 1, 96, 96);
        let _ = killer;

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, victim, None, Some(killer), 1000);
            },
        );

        assert_eq!(players[0].frags[1], 1);
        assert_eq!(players[1].frags[1], 0);
    }

    #[test]
    fn kill_of_monster_by_player_counts_for_that_player() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let killer = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        let monster = spawn_possessed(&mut store, &map, &mut rng, 96, 96);

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, monster, None, Some(killer), 1000);
            },
        );

        assert_eq!(players[0].killcount, 1);
        assert!(store.mobj(monster).is_some()); // corpse stays
    }

    #[test]
    fn environmental_monster_death_still_counts_offline() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let monster = spawn_possessed(&mut store, &map, &mut rng, 64, 64);

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, monster, None, None, 1000);
            },
        );

        assert_eq!(players[0].killcount, 1);
    }

    #[test]
    fn god_mode_blocks_normal_damage_but_not_stomps() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 64, 64);
        players[0].cheats = crate::player::CF_GODMODE;

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, avatar, None, None, 10);
            },
        );
        assert_eq!(players[0].health, 100);

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, avatar, None, None, 1000); // >= 1000 bypasses god mode
            },
        );
        assert_eq!(players[0].health, 0); // dead (1000 stomps avatar and player)
    }

    #[test]
    fn invulnerability_power_blocks_normal_damage() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 64, 64);
        players[0].powers[Power::Invulnerability.index()] = 1050;

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, avatar, None, None, 99);
            },
        );
        assert_eq!(players[0].health, 100);
    }

    #[test]
    fn baby_skill_halves_player_damage() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 64, 64);

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                ctx.rules.skill = crate::mobj::Skill::Baby;
                damage_mobj(ctx, avatar, None, None, 30);
            },
        );

        assert_eq!(players[0].health, 85); // 100 - 15
    }

    #[test]
    fn hell_special_sector_leaves_one_health() {
        // sector special 11 = "end of game hell hack"
        let map = one_sector_map(11);
        let mut store = MobjStore::new();
        let mut players = players();
        let mut rng = DoomRng::with_seed(3);
        let mut world = TestWorld::new(&map);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 64, 64);
        players[0].health = 50;

        let mut game_events = Vec::new();
        let mut events = Vec::new();
        {
            let mut ctx = test_context(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                &mut game_events,
                &mut events,
            );
            damage_mobj(&mut ctx, avatar, None, None, 100);
        }

        assert_eq!(players[0].health, 1);
    }

    #[test]
    fn chainsaw_wielder_inflicts_no_thrust() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 64, 64);
        players[0].readyweapon = Weapon::Chainsaw;
        let monster = spawn_possessed(&mut store, &map, &mut rng, 128, 64);

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, monster, Some(avatar), Some(avatar), 10);
            },
        );

        let m = store.mobj(monster).unwrap();
        assert_eq!(m.momx.to_raw(), 0);
        assert_eq!(m.momy.to_raw(), 0);
    }

    #[test]
    fn regular_attacker_inflicts_thrust() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
        let monster = spawn_possessed(&mut store, &map, &mut rng, 64, 32); // due east

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, monster, Some(avatar), Some(avatar), 10);
            },
        );

        let m = store.mobj(monster).unwrap();
        assert!(m.momx.to_raw() > 0); // pushed away from the attacker (+x)
        // not exactly zero: FINE_SINE[0] == 25, the table's zero-crossing
        // is slightly sloped by design (vanilla uses the same value)
        assert!(m.momy.to_raw().abs() <= 64);
    }

    #[test]
    fn skullfly_zeroes_momentum() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let skull = spawn_thing(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_SKULL,
            64,
            64,
        );
        store.mobj_mut(skull).unwrap().flags |= crate::info::MF_SKULLFLY;
        store.mobj_mut(skull).unwrap().momx = crate::fixed::Fixed::from_int(5);
        store.mobj_mut(skull).unwrap().momy = crate::fixed::Fixed::from_int(5);

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, skull, None, None, 10);
            },
        );

        let m = store.mobj(skull).unwrap();
        assert_eq!(m.momx.to_raw(), 0);
        assert_eq!(m.momy.to_raw(), 0);
    }

    #[test]
    fn monster_never_takes_pain_at_zero_painchance() {
        let mut pained = 0;
        for seed in 0..64 {
            let (map, mut store, mut players, mut rng, mut world) = fixture(seed);
            let fire = spawn_thing(
                &mut store,
                &map,
                &mut rng,
                crate::info::MobjType::MT_FIRE,
                64,
                64,
            );
            run(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                |ctx| {
                    damage_mobj(ctx, fire, None, None, 1);
                },
            );
            let m = store.mobj(fire).unwrap();
            if m.flags & MF_JUSTHIT != 0 {
                pained += 1;
            }
        }
        assert_eq!(pained, 0);
    }

    #[test]
    fn player_painchance_255_always_hurts() {
        for seed in 0..8 {
            let (map, mut store, mut players, mut rng, mut world) = fixture(seed);
            let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 64, 64);
            run(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                |ctx| {
                    damage_mobj(ctx, avatar, None, None, 1);
                },
            );
            let m = store.mobj(avatar).unwrap();
            assert_ne!(m.flags & MF_JUSTHIT, 0);
            assert_eq!(
                m.state,
                MOBJ_INFO[crate::info::MobjType::MT_PLAYER as usize].painstate
            );
        }
    }

    #[test]
    fn painchance_200_splits_seeds_into_pain_and_no_pain() {
        let mut pain = 0;
        let mut no_pain = 0;
        for seed in 0..128 {
            let (map, mut store, mut players, mut rng, mut world) = fixture(seed);
            let possessed = spawn_possessed(&mut store, &map, &mut rng, 64, 64);
            run(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                |ctx| {
                    damage_mobj(ctx, possessed, None, None, 1);
                },
            );
            let m = store.mobj(possessed).unwrap();
            if m.flags & MF_JUSTHIT != 0 {
                pain += 1;
            } else {
                no_pain += 1;
            }
        }
        assert!(pain > 32, "expected some pain seeds, got {pain}");
        assert!(no_pain > 8, "expected some calm seeds, got {no_pain}");
    }

    #[test]
    fn damage_wakes_sleeping_monster_and_sets_threshold() {
        // a possessed grunt that does NOT roll pain this tick flips from
        // its spawn state into the see state
        for seed in 0..64 {
            let (map, mut store, mut players, mut rng, mut world) = fixture(seed);
            let killer = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);
            let monster = spawn_possessed(&mut store, &map, &mut rng, 96, 96);
            store.mobj_mut(monster).unwrap().reactiontime = 8;

            run(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                |ctx| {
                    damage_mobj(ctx, monster, Some(killer), Some(killer), 5);
                },
            );

            let m = store.mobj(monster).unwrap();
            if m.flags & MF_JUSTHIT != 0 {
                continue; // rolled pain; covered by the sibling test
            }
            assert_eq!(m.reactiontime, 0); // awake now
            // vanilla P_SetMobjState runs A_Chase inline from the see-state
            // transition, and A_Chase burns one threshold tick
            assert_eq!(m.threshold, BASE_THRESHOLD - 1);
            assert_eq!(m.target, Some(killer));
            // the see-transition fires (or A_Chase already attacked from it);
            // either way it is no longer standing around doing nothing
            assert_ne!(
                m.state,
                MOBJ_INFO[crate::info::MobjType::MT_POSSESSED as usize].spawnstate
            );
            return;
        }
        panic!("no pain-free seed found in 0..64");
    }

    #[test]
    fn pain_prevents_the_seestate_transition_like_vanilla() {
        // a possessed grunt that rolls pain stays in the pain state this
        // tick; vanilla orders the pain transition before the chase block,
        // and the chase block only fires from the spawn state
        for seed in 0..64 {
            let (map2, mut store2, mut players2, mut rng2, mut world2) = fixture(seed);
            let killer2 = spawn_player(&mut store2, &map2, &mut rng2, &mut players2, 0, 32, 32);
            let monster2 = spawn_possessed(&mut store2, &map2, &mut rng2, 96, 96);
            run(
                &map2,
                &mut store2,
                &mut players2,
                &mut rng2,
                &mut world2,
                |ctx| {
                    damage_mobj(ctx, monster2, Some(killer2), Some(killer2), 5);
                },
            );
            let m = store2.mobj(monster2).unwrap();
            if m.state == MOBJ_INFO[crate::info::MobjType::MT_POSSESSED as usize].painstate {
                assert_eq!(m.reactiontime, 0);
                assert_eq!(m.threshold, BASE_THRESHOLD);
                assert_eq!(m.target, Some(killer2));
                return;
            }
        }
        panic!("no pain seed found in 0..64");
    }

    #[test]
    fn vile_source_does_not_enrage_target() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let vile = spawn_thing(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_VILE,
            32,
            32,
        );
        let monster = spawn_possessed(&mut store, &map, &mut rng, 96, 96);

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, monster, Some(vile), Some(vile), 5);
            },
        );

        let m = store.mobj(monster).unwrap();
        assert_eq!(m.threshold, 0);
        assert_eq!(m.target, None);
    }

    #[test]
    fn damage_to_dead_monster_is_a_noop() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let monster = spawn_possessed(&mut store, &map, &mut rng, 64, 64);
        store.mobj_mut(monster).unwrap().health = 0;

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, monster, None, None, 100);
            },
        );

        assert_eq!(store.mobj(monster).unwrap().health, 0);
    }

    #[test]
    fn possessed_drops_a_clip_marked_dropped() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let monster = spawn_possessed(&mut store, &map, &mut rng, 64, 64);

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, monster, None, None, 1000);
            },
        );

        let dropped = store
            .thinkers()
            .filter_map(|(_, t)| match t {
                crate::thinker::Thinker::Mobj(m)
                    if m.mobj_type == crate::info::MobjType::MT_CLIP =>
                {
                    Some(m)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(dropped.len(), 1);
        assert_ne!(dropped[0].flags & MF_DROPPED, 0);
    }

    #[test]
    fn shotguy_drops_shotgun_chainguy_drops_chaingun() {
        for (doom, drop_type) in [
            (
                crate::info::MobjType::MT_SHOTGUY,
                crate::info::MobjType::MT_SHOTGUN,
            ),
            (
                crate::info::MobjType::MT_CHAINGUY,
                crate::info::MobjType::MT_CHAINGUN,
            ),
        ] {
            let (map, mut store, mut players, mut rng, mut world) = fixture(3);
            let monster = spawn_thing(&mut store, &map, &mut rng, doom, 64, 64);
            run(
                &map,
                &mut store,
                &mut players,
                &mut rng,
                &mut world,
                |ctx| {
                    damage_mobj(ctx, monster, None, None, 10000);
                },
            );
            let mut found = false;
            for (_id, t) in store.thinkers() {
                let crate::thinker::Thinker::Mobj(m) = t else {
                    continue;
                };
                if m.mobj_type == drop_type {
                    assert_ne!(m.flags & MF_DROPPED, 0);
                    found = true;
                }
            }
            assert!(found, "expected dropped {:?}", drop_type);
        }
    }

    #[test]
    fn sergeant_drops_nothing() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let monster = spawn_thing(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_SERGEANT,
            64,
            64,
        );
        let before = store.len();
        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, monster, None, None, 10000);
            },
        );
        assert_eq!(store.len(), before); // corpse, no new item
    }

    #[test]
    fn skull_keeps_nogravity_when_killed() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let skull = spawn_thing(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_SKULL,
            64,
            64,
        );
        assert_ne!(store.mobj(skull).unwrap().flags & MF_NOGRAVITY, 0);

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, skull, None, None, 1000);
            },
        );

        let m = store.mobj(skull).unwrap();
        assert_ne!(m.flags & MF_NOGRAVITY, 0);
        assert_ne!(m.flags & MF_CORPSE, 0);
    }

    #[test]
    fn attacker_is_remembered_after_damage() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 32, 32);

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, avatar, None, Some(avatar), 5);
            },
        );

        assert_eq!(players[0].attacker, Some(avatar));
        assert!(players[0].damagecount > 0);
    }

    #[test]
    fn damagecount_is_capped_at_100() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        let avatar = spawn_player(&mut store, &map, &mut rng, &mut players, 0, 64, 64);
        players[0].damagecount = 95;

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, avatar, None, None, 30);
            },
        );

        assert_eq!(players[0].damagecount, 100);
    }

    #[test]
    fn non_shooter_target_is_ignored() {
        let (map, mut store, mut players, mut rng, mut world) = fixture(3);
        // a clip is not shootable: damage must bounce off harmlessly
        let clip = spawn_thing(
            &mut store,
            &map,
            &mut rng,
            crate::info::MobjType::MT_CLIP,
            64,
            64,
        );

        run(
            &map,
            &mut store,
            &mut players,
            &mut rng,
            &mut world,
            |ctx| {
                damage_mobj(ctx, clip, None, None, 1000);
            },
        );

        assert_eq!(
            store.mobj(clip).unwrap().health,
            MOBJ_INFO[crate::info::MobjType::MT_CLIP as usize].spawnhealth
        );
    }
}
