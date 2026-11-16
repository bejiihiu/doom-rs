//! Teleportation, ported from `p_telept.c` (`EV_Teleport`).
//!
//! The line specials that call this are the teleport triggers (39, 97,
//! 125, 126) and the Heretic-style re-triggers. A monster-only trigger is
//! enforced by the caller (`P_CrossSpecialLine`), not here.
//!
//! Destination search: iterate the sectors with the line's tag, find an
//! `MT_TELEPORTMAN` mobj standing in that sector, teleport the thing to
//! its position and angle. The fog and the sound are spawned/played
//! through events so the game loop can wire audio and particles.

use crate::fixed::Fixed;
use crate::info::{MF_MISSILE, MobjType, SfxId};
use crate::movement::MoveContext;
use crate::specials::{SpecialContext, SpecialEvent};
use crate::thinker::ThinkerId;

/// Vanilla `EV_Teleport`. Returns true when the thing was teleported.
///
/// * `side` is the side of the line the thing is on; only the front side
///   (`side == 0`) teleports — this is what keeps the player from being
///   teleported back and forth across the line every step.
/// * `thing` is the mobj crossing the line. Missiles never teleport.
pub fn ev_teleport(ctx: &mut SpecialContext, line: usize, side: i32, thing: ThinkerId) -> bool {
    // don't teleport missiles
    let Some(mover) = ctx.store.mobj(thing) else {
        tracing::warn!("ev_teleport: stale mobj handle");
        return false;
    };
    if (mover.flags & MF_MISSILE) != 0 {
        return false;
    }

    // don't teleport if hit back of line, so you can get out of
    // the teleporter
    if side != 0 {
        return false;
    }

    let tag = ctx.map.linedefs[line].tag;
    let map_sectors = ctx.map.sectors.len();

    for sector_index in 0..map_sectors {
        if ctx.map.sectors[sector_index].tag != tag {
            continue;
        }

        // find a teleport destination in this sector
        let destinations: Vec<ThinkerId> = ctx
            .store
            .thinkers()
            .filter_map(|(id, thinker)| match thinker {
                crate::thinker::Thinker::Mobj(mobj) => Some((id, mobj)),
                _ => None,
            })
            .filter(|(_, mobj)| mobj.mobj_type == MobjType::MT_TELEPORTMAN)
            .filter(|(_, mobj)| mobj.sector == Some(sector_index))
            .map(|(id, _)| id)
            .collect();

        for destination in destinations {
            let Some(dest) = ctx.store.mobj(destination) else {
                continue;
            };
            let (dest_x, dest_y, dest_angle) = (dest.x, dest.y, dest.angle);

            // the old position, for the departure fog
            let (old_x, old_y, old_z) = {
                let mobj = ctx.store.mobj(thing).expect("mover checked above");
                (mobj.x, mobj.y, mobj.z)
            };

            // P_TeleportMove (stomping move, no fit checks)
            let mut move_ctx = MoveContext::new(ctx.map, ctx.store, ctx.players, ctx.rng);
            move_ctx.leveltime = ctx.leveltime;
            let report = crate::movement::teleport_move(
                &mut move_ctx,
                thing,
                dest_x,
                dest_y,
                ctx.boss_level,
            );
            // reroute movement events through our channel
            for event in report.events {
                ctx.events.push(SpecialEvent::Move(event));
            }
            if !report.ok {
                return false;
            }

            // thing->z = thing->floorz (not needed per vanilla's fixme,
            // kept for parity) + player view z
            let new_z = ctx
                .store
                .mobj(thing)
                .map(|m| m.floorz)
                .unwrap_or(Fixed::ZERO);
            let player_index = ctx.store.mobj(thing).and_then(|m| m.player);
            if let Some(pi) = player_index {
                let viewheight = ctx.players[pi].viewheight;
                ctx.players[pi].viewz = new_z + viewheight;
            }
            if let Some(mobj) = ctx.store.mobj_mut(thing) {
                mobj.z = new_z;
            }

            // spawn teleport fog at source and destination
            let fog_source = ctx.store.spawn_mobj(
                ctx.map,
                ctx.rng,
                crate::mobj::SpawnSpec {
                    skill: ctx.skill,
                    x: old_x,
                    y: old_y,
                    z: crate::mobj::SpawnZ::At(old_z),
                    mobj_type: MobjType::MT_TFOG,
                },
            );
            ctx.events.push(SpecialEvent::ActorSound {
                actor: fog_source,
                sfx: SfxId::sfx_telept,
            });

            // fog at the destination, offset 20 units into the facing direction
            let an = dest_angle.fine_angle();
            let offset = Fixed::from_int(20);
            let fog_x = dest_x + offset * crate::tables::FINE_COSINE[an];
            let fog_y = dest_y + offset * crate::tables::FINE_SINE[an];
            let fog_dest = ctx.store.spawn_mobj(
                ctx.map,
                ctx.rng,
                crate::mobj::SpawnSpec {
                    skill: ctx.skill,
                    x: fog_x,
                    y: fog_y,
                    z: crate::mobj::SpawnZ::At(new_z),
                    mobj_type: MobjType::MT_TFOG,
                },
            );
            ctx.events.push(SpecialEvent::ActorSound {
                actor: fog_dest,
                sfx: SfxId::sfx_telept,
            });

            // don't move for a bit
            if player_index.is_some()
                && let Some(mobj) = ctx.store.mobj_mut(thing)
            {
                mobj.reactiontime = 18;
            }

            if let Some(mobj) = ctx.store.mobj_mut(thing) {
                mobj.angle = dest_angle;
                mobj.momx = Fixed::ZERO;
                mobj.momy = Fixed::ZERO;
                mobj.momz = Fixed::ZERO;
            }

            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::info::MobjType;
    use crate::mobj::MobjStore;
    use crate::rng::DoomRng;
    use crate::specials::testutil::{spawn_player, spawn_thing, split_map, test_player};
    use crate::specials::{SpecialContext, SpecialEvent};
    use crate::tables::Angle;

    struct TeleportWorld {
        map: crate::map::Map,
        store: MobjStore,
        players: Vec<crate::player::Player>,
        rng: DoomRng,
    }

    /// South room with the divider marked as a teleport trigger (special 39)
    /// leading to the north sector (tag 99).
    fn world() -> TeleportWorld {
        let mut map = split_map(0, 128, 99);
        map.linedefs[4].special = 39;
        map.linedefs[4].tag = 99;
        TeleportWorld {
            map,
            store: MobjStore::new(),
            players: vec![test_player()],
            rng: DoomRng::with_seed(0),
        }
    }

    fn ctx<'a>(w: &'a mut TeleportWorld) -> SpecialContext<'a> {
        SpecialContext::new(&mut w.map, &mut w.store, &mut w.players, &mut w.rng)
    }

    #[test]
    fn teleports_to_the_destination_thing_with_fog_and_sound() {
        let mut w = world();
        let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 32, 32);
        let dest = spawn_thing(
            &mut w.store,
            &w.map,
            &mut w.rng,
            MobjType::MT_TELEPORTMAN,
            128,
            192,
        );
        // face the destination east
        if let Some(d) = w.store.mobj_mut(dest) {
            d.angle = Angle::ANG90;
        }
        let mut ctx = ctx(&mut w);

        assert!(ev_teleport(&mut ctx, 4, 0, avatar));

        let mobj = ctx.store.mobj(avatar).expect("avatar exists");
        assert_eq!(mobj.x, crate::fixed::Fixed::from_int(128));
        assert_eq!(mobj.y, crate::fixed::Fixed::from_int(192));
        assert_eq!(mobj.angle, Angle::ANG90);
        // momenta are zeroed by the teleport
        assert_eq!(mobj.momx, crate::fixed::Fixed::ZERO);
        assert_eq!(mobj.momy, crate::fixed::Fixed::ZERO);
        assert_eq!(mobj.momz, crate::fixed::Fixed::ZERO);
        // the player avatar freezes for a bit
        assert_eq!(mobj.reactiontime, 18);
        // fog sounds at source and destination
        let fog_sounds = ctx
            .events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    SpecialEvent::ActorSound {
                        sfx: SfxId::sfx_telept,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(fog_sounds, 2);
        // fog mobjs were spawned (two MT_TFOG in the registry)
        let fogs = ctx
            .store
            .thinkers()
            .filter(|(_, t)| match t {
                crate::thinker::Thinker::Mobj(m) => m.mobj_type == MobjType::MT_TFOG,
                _ => false,
            })
            .count();
        assert_eq!(fogs, 2);
    }

    #[test]
    fn no_teleport_from_the_back_side() {
        let mut w = world();
        let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 32, 32);
        let _dest = spawn_thing(
            &mut w.store,
            &w.map,
            &mut w.rng,
            MobjType::MT_TELEPORTMAN,
            128,
            192,
        );
        let mut ctx = ctx(&mut w);

        assert!(!ev_teleport(&mut ctx, 4, 1, avatar));
        let mobj = ctx.store.mobj(avatar).unwrap();
        assert_eq!(mobj.x, crate::fixed::Fixed::from_int(32));
    }

    #[test]
    fn missiles_never_teleport() {
        let mut w = world();
        let rocket = spawn_thing(
            &mut w.store,
            &w.map,
            &mut w.rng,
            MobjType::MT_ROCKET,
            32,
            32,
        );
        let _dest = spawn_thing(
            &mut w.store,
            &w.map,
            &mut w.rng,
            MobjType::MT_TELEPORTMAN,
            128,
            192,
        );
        let mut ctx = ctx(&mut w);

        assert!(!ev_teleport(&mut ctx, 4, 0, rocket));
        let mobj = ctx.store.mobj(rocket).unwrap();
        assert_eq!(mobj.x, crate::fixed::Fixed::from_int(32));
    }

    #[test]
    fn wrong_sector_destination_is_ignored() {
        let mut w = world();
        let avatar = spawn_player(&mut w.store, &w.map, &mut w.rng, &mut w.players, 32, 32);
        // a teleportman in the SOUTH sector (not the tagged north sector)
        let _dest = spawn_thing(
            &mut w.store,
            &w.map,
            &mut w.rng,
            MobjType::MT_TELEPORTMAN,
            64,
            64,
        );
        let mut ctx = ctx(&mut w);

        assert!(!ev_teleport(&mut ctx, 4, 0, avatar));
    }

    #[test]
    fn non_player_teleports_without_freeze() {
        let mut w = world();
        let monster = spawn_thing(
            &mut w.store,
            &w.map,
            &mut w.rng,
            MobjType::MT_POSSESSED,
            32,
            32,
        );
        let _dest = spawn_thing(
            &mut w.store,
            &w.map,
            &mut w.rng,
            MobjType::MT_TELEPORTMAN,
            128,
            192,
        );
        let mut ctx = ctx(&mut w);

        assert!(ev_teleport(&mut ctx, 4, 0, monster));
        let mobj = ctx.store.mobj(monster).unwrap();
        // monsters keep their spawn reactiontime (vanilla mobjinfo.c has
        // reactiontime 8 for MT_POSSESSED) and never get the 18-tic
        // player-only teleport freeze
        assert_eq!(mobj.reactiontime, 8, "monsters get no teleport freeze");
    }

    #[test]
    fn boss_level_flag_is_plumbed() {
        // just check the field exists and defaults sanely
        let mut w = world();
        let ctx = ctx(&mut w);
        assert!(!ctx.boss_level);
        assert_eq!(ctx.skill, crate::mobj::Skill::Medium);
    }
}
