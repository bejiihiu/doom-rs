//! Area-effect damage: `P_RadiusAttack` and `PIT_RadiusAttack`, ported
//! from p_map.c.
//!
//! The original iterates the blockmap cells in a box around the spot and
//! damages everything shootable in it, with the spider and cyborg immune.
//! The damage falls off linearly with distance from the spot's edge.
//!
//! `validcount` becomes a local `HashSet` per call (a thing in two cells
//! must only be hit once).

use crate::fixed::FRACUNIT;
use crate::info::MobjType;
use crate::map::{UnitBox, block_range};
use crate::math::BBox;
use crate::movement::MF_SHOOTABLE;
use crate::thinker::ThinkerId;
use std::collections::HashSet;

use super::{EnemyContext, GameEvent};

/// Vanilla `P_RadiusAttack`.
///
/// `spot` is the explosion's center (the thing the radius attack is
/// anchored to — for missile explosions, the exploding missile),
/// `source` is the shooter. `damage` is the full damage at zero
/// distance.
pub fn radius_attack(
    ctx: &mut EnemyContext<'_>,
    spot: ThinkerId,
    source: Option<ThinkerId>,
    damage: i32,
) {
    let Some(bombspot) = ctx.store.mobj(spot) else {
        tracing::warn!("P_RadiusAttack: stale spot handle");
        return;
    };
    let spot_x = bombspot.x;
    let spot_y = bombspot.y;

    // dist = (damage+MAXRADIUS)<<FRACBITS
    let dist = crate::fixed::Fixed::from_raw((damage + 32).wrapping_mul(FRACUNIT));

    let bbox = BBox {
        top: spot_y + dist,
        bottom: spot_y - dist,
        right: spot_x + dist,
        left: spot_x - dist,
    };

    let unit = UnitBox::from_fixed(&bbox);
    let Some(range) = block_range(&ctx.map.blockmap, unit) else {
        return;
    };

    let columns = usize::from(ctx.map.blockmap.columns);
    let mut seen: HashSet<ThinkerId> = HashSet::new();

    for x in range.left..=range.right {
        for y in range.bottom..=range.top {
            let cell = usize::from(y) * columns + usize::from(x);
            for thing in ctx.store.blockmap_members(cell).collect::<Vec<_>>() {
                if !seen.insert(thing) {
                    continue; // already checked in a shared cell
                }
                pit_radius_attack(ctx, spot, source, damage, thing);
            }
        }
    }
}

/// Vanilla `PIT_RadiusAttack`.
fn pit_radius_attack(
    ctx: &mut EnemyContext<'_>,
    spot: ThinkerId,
    source: Option<ThinkerId>,
    damage: i32,
    thing: ThinkerId,
) {
    let Some(spot_mobj) = ctx.store.mobj(spot) else {
        return;
    };
    let spot_x = spot_mobj.x;
    let spot_y = spot_mobj.y;

    let Some(target) = ctx.store.mobj(thing) else {
        return;
    };

    if !target.has_flag(MF_SHOOTABLE) {
        return;
    }

    // boss spider and cyborg
    // take no damage from concussion.
    if target.mobj_type == MobjType::MT_CYBORG || target.mobj_type == MobjType::MT_SPIDER {
        return;
    }

    let dx = i64::from(target.x.to_raw() - spot_x.to_raw()).abs();
    let dy = i64::from(target.y.to_raw() - spot_y.to_raw()).abs();

    let dist = dx.max(dy);
    let dist = (dist - i64::from(target.radius.to_raw())) >> 16;

    let dist = if dist < 0 { 0 } else { dist };

    if dist >= i64::from(damage) {
        return; // out of range
    }

    // must be in direct path
    if super::sight::check_sight(ctx.map, target, spot_mobj) {
        ctx.events.push(GameEvent::Damage {
            target: thing,
            inflictor: Some(spot),
            source,
            damage: damage - dist as i32,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::enemy::testutil::{
        TestWorld, open_map, players, spawn_player, spawn_thing, split_map, test_context,
    };
    use crate::map::types::ML_TWOSIDED;
    use crate::mobj::MobjStore;
    use crate::rng::DoomRng;

    #[test]
    fn radius_attack_hurts_shootable_things_in_range() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let victim = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 96, 64);
        let rocket = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOPSHOT, 80, 64);
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
            radius_attack(&mut ctx, rocket, Some(victim), 128);
        }

        let damaged = events
            .iter()
            .any(|e| matches!(e, GameEvent::Damage { target, .. } if *target == victim));
        assert!(damaged, "the imp next to the blast should take damage");
    }

    #[test]
    fn radius_attack_spares_the_far_and_the_bosses() {
        let map = open_map();
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 64);
        let far_away = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 240, 240);
        let spider = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_SPIDER, 96, 64);
        let cyborg = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_CYBORG, 96, 96);
        let rocket = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOPSHOT, 80, 64);
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
            radius_attack(&mut ctx, rocket, None, 128);
        }

        let damaged_far = events
            .iter()
            .any(|e| matches!(e, GameEvent::Damage { target, .. } if *target == far_away));
        let damaged_spider = events
            .iter()
            .any(|e| matches!(e, GameEvent::Damage { target, .. } if *target == spider));
        let damaged_cyborg = events
            .iter()
            .any(|e| matches!(e, GameEvent::Damage { target, .. } if *target == cyborg));
        assert!(!damaged_far, "far things take no damage");
        assert!(!damaged_spider, "spider is immune to the blast");
        assert!(!damaged_cyborg, "cyborg is immune to the blast");
    }

    #[test]
    fn radius_attack_needs_line_of_sight() {
        // closed divider: the imp and the rocket are on different sides
        let map = split_map(0, 0, ML_TWOSIDED, 0x00);
        let mut store = MobjStore::new();
        store.reset_for_level(&map);
        let mut rng = DoomRng::new();
        let mut players = players();
        let _player = spawn_player(&mut store, &map, &mut rng, &mut players, 192, 192);
        let victim = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOP, 192, 192);
        let rocket = spawn_thing(&mut store, &map, &mut rng, MobjType::MT_TROOPSHOT, 64, 64);
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
            radius_attack(&mut ctx, rocket, None, 128);
        }

        let damaged = events
            .iter()
            .any(|e| matches!(e, GameEvent::Damage { target, .. } if *target == victim));
        assert!(!damaged, "no direct path, no blast damage");
    }
}
