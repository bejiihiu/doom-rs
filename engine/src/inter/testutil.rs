//! Shared fixtures for the inter-layer tests. Test-only.
//!
//! The workhorse map is a single 256x256 sector; spawned things sit on
//! the floor like on a real map. The `Map` literal is built by hand (no
//! lump loading), which is enough for spawn/damage/kill bookkeeping.

#![cfg(test)]

use crate::fixed::Fixed;
use crate::map::Map;
use crate::map::types::{BlockMapGrid, Sector, SubSector};
use crate::mobj::{SpawnSpec, SpawnZ};
use crate::player::{
    MAXPLAYERS, NUM_AMMO, NUM_CARDS, NUM_POWERS, NUM_PSPRITES, NUM_WEAPONS, Player, PlayerState,
    PspDef, TicCmd, Weapon,
};
use crate::thinker::ThinkerId;

use crate::enemy::{BrainState, SoundFlood};

/// A one-sector 256x256 test world. The sector's `special` is
/// configurable for the hell-level hack test.
pub(crate) fn one_sector_map(special: i16) -> Map {
    Map {
        vertexes: Vec::new(),
        linedefs: Vec::new(),
        sidedefs: Vec::new(),
        sectors: vec![Sector {
            floor_height: Fixed::ZERO,
            ceiling_height: Fixed::from_int(128),
            floor_pic: *b"FLOOR4_8",
            ceiling_pic: *b"CEIL3_5\0",
            light_level: 192,
            special,
            tag: 0,
        }],
        segs: Vec::new(),
        subsectors: vec![SubSector {
            num_segs: 0,
            first_seg: 0,
            sector: Some(0),
        }],
        nodes: Vec::new(),
        blockmap: BlockMapGrid::new(
            Fixed::from_int(-512),
            Fixed::from_int(-512),
            8,
            8,
            vec![Vec::new(); 64],
        ),
        reject: Vec::new(),
        things: Vec::new(),
    }
}

/// A baseline player with neutral inventory and full health.
pub(crate) fn test_player() -> Player {
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
        powers: [0; NUM_POWERS],
        cards: [false; NUM_CARDS],
        backpack: false,
        frags: [0; MAXPLAYERS],
        readyweapon: Weapon::Fist,
        pendingweapon: None,
        weaponowned: [false; NUM_WEAPONS],
        ammo: [0; NUM_AMMO],
        maxammo: [200, 50, 300, 50],
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
        psprites: [PspDef::default(); NUM_PSPRITES],
        didsecret: false,
    }
}

/// The players vector with one live player at full health.
pub(crate) fn players() -> Vec<Player> {
    vec![test_player()]
}

/// Spawns an `MT_PLAYER` avatar at `(x, y)` units and wires the player
/// slot to it.
pub(crate) fn spawn_player(
    store: &mut crate::mobj::MobjStore,
    map: &Map,
    rng: &mut crate::rng::DoomRng,
    players: &mut Vec<Player>,
    player_index: usize,
    x: i32,
    y: i32,
) -> ThinkerId {
    let id = store.spawn_mobj(
        map,
        rng,
        SpawnSpec {
            skill: crate::mobj::Skill::Medium,
            x: Fixed::from_int(x),
            y: Fixed::from_int(y),
            z: SpawnZ::Floor,
            mobj_type: crate::info::MobjType::MT_PLAYER,
        },
    );
    while players.len() <= player_index {
        players.push(test_player());
    }
    players[player_index].mo = Some(id);
    players[player_index].health = 100;
    players[player_index].playerstate = PlayerState::Live;
    // vanilla P_SpawnPlayer: mo->player = player
    store.mobj_mut(id).unwrap().player = Some(player_index);
    id
}

/// Spawns a non-player mobj at `(x, y)` units snapped to the floor.
pub(crate) fn spawn_thing(
    store: &mut crate::mobj::MobjStore,
    map: &Map,
    rng: &mut crate::rng::DoomRng,
    mobj_type: crate::info::MobjType,
    x: i32,
    y: i32,
) -> ThinkerId {
    store.spawn_mobj(
        map,
        rng,
        SpawnSpec {
            skill: crate::mobj::Skill::Medium,
            x: Fixed::from_int(x),
            y: Fixed::from_int(y),
            z: SpawnZ::Floor,
            mobj_type,
        },
    )
}

/// The level-persistent AI state the nested enemy context needs.
pub(crate) struct TestWorld {
    pub flood: SoundFlood,
    pub brain: BrainState,
}

impl TestWorld {
    pub fn new(map: &Map) -> Self {
        TestWorld {
            flood: SoundFlood::new(map.sectors.len()),
            brain: BrainState::new(),
        }
    }
}

/// An `InterContext` reporting into the caller's event buffers.
#[allow(clippy::too_many_arguments)]
pub(crate) fn test_context<'a>(
    map: &'a Map,
    store: &'a mut crate::mobj::MobjStore,
    players: &'a mut Vec<Player>,
    rng: &'a mut crate::rng::DoomRng,
    world: &'a mut TestWorld,
    game_events: &'a mut Vec<crate::enemy::GameEvent>,
    events: &'a mut Vec<super::InterEvent>,
) -> super::InterContext<'a> {
    super::InterContext::new(
        map,
        store,
        players,
        rng,
        &mut world.flood,
        &mut world.brain,
        game_events,
        events,
    )
}
