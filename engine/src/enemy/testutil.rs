//! Shared fixtures for the AI tests. Test-only.
//!
//! The workhorse is [`split_map`]: a 256x256 world split by a two-sided
//! divider at y = 128 into a south sector (floor 0, ceiling 128) and a
//! north sector with a configurable floor/ceiling. Coordinates are whole
//! map units, and the walls are wound clockwise so the interior faces the
//! room, like in real DOOM maps.

#![cfg(test)]

use crate::map::testutil::{
    NodeSpec, blockmap_grid, linedefs, nodes, reject, sectors, segs, sidedefs, subsectors, vertexes,
};
use crate::map::types::{ML_BLOCKING, ML_TWOSIDED, NF_SUBSECTOR};
use crate::map::{Map, MapLumps};
use crate::player::{
    MAXPLAYERS, NUM_AMMO, NUM_CARDS, NUM_POWERS, NUM_PSPRITES, NUM_WEAPONS, Player, PlayerState,
    PspDef, TicCmd, Weapon,
};

/// Builds the split-room map with the given north sector and reject lump.
///
/// * `north_floor`/`north_ceil` — the north sector's floor/ceiling heights
///   (128/0 closes the divider into a wall, 100/128 leaves a window).
/// * `divider_flags` — flags for the divider linedef.
/// * `reject_byte` — raw REJECT payload (2 sectors: bit (s1*2+s2) blocks
///   sight from sector s1 to sector s2).
pub(crate) fn split_map(
    north_floor: i16,
    north_ceil: i16,
    divider_flags: i16,
    reject_byte: u8,
) -> Map {
    let vertexes = vertexes(&[(0, 0), (256, 0), (256, 256), (0, 256), (0, 128), (256, 128)]);
    let linedefs = linedefs(&[
        (1, 0, ML_BLOCKING, 0, 0, 0, -1),  // L0 south wall: (256,0) -> (0,0)
        (2, 1, ML_BLOCKING, 0, 0, 1, -1),  // L1 east wall:  (256,256) -> (256,0)
        (3, 2, ML_BLOCKING, 0, 0, 2, -1),  // L2 north wall: (0,256) -> (256,256)
        (0, 3, ML_BLOCKING, 0, 0, 3, -1),  // L3 west wall:  (0,0) -> (0,256)
        (4, 5, divider_flags, 0, 0, 4, 5), // L4 divider:   (0,128) -> (256,128)
    ]);
    let sidedefs = sidedefs(&[
        (0, 0, "-", "-", "WALL1", 0),  // 0: south wall, sector 0
        (0, 0, "-", "-", "WALL1", 0),  // 1: east wall, sector 0
        (0, 0, "-", "-", "WALL1", 1),  // 2: north wall, sector 1
        (0, 0, "-", "-", "WALL1", 0),  // 3: west wall, sector 0
        (0, 0, "-", "-", "FLOOR1", 0), // 4: divider front, sector 0
        (0, 0, "-", "-", "FLOOR1", 1), // 5: divider back, sector 1
    ]);
    let sectors = sectors(&[
        (0, 128, "FLOOR4_8", "CEIL3_5", 176, 0, 0),
        (north_floor, north_ceil, "FLOOR4_8", "CEIL3_5", 176, 0, 0),
    ]);
    let segs = segs(&[
        (0, 1, 0, 0, 0, 0), // L0 front
        (1, 5, 0, 1, 0, 0), // L1, south part
        (4, 5, 0, 4, 0, 0), // divider front
        (2, 3, 0, 2, 0, 0), // L2 front
        (5, 2, 0, 1, 0, 0), // L1, north part
        (5, 4, 0, 4, 1, 0), // divider back
    ]);
    let subsectors = subsectors(&[(3, 0), (3, 3)]);
    let nodes = nodes(&[NodeSpec {
        x: 0,
        y: 128,
        dx: 256,
        dy: 0,
        bbox: [[128, 0, 256, 0], [256, 128, 256, 0]],
        children: [NF_SUBSECTOR, NF_SUBSECTOR | 1],
    }]);
    let blockmap = blockmap_grid(
        (-8, -8),
        3,
        3,
        &[
            &[0, 3], // (0,0) SW: south wall + west wall
            &[0],    // (1,0) S middle
            &[0, 1], // (2,0) SE: south wall + east wall
            &[3, 4], // (0,1) W middle: west wall + divider
            &[4],    // (1,1) middle: divider
            &[1, 4], // (2,1) E middle: east wall + divider
            &[2, 3], // (0,2) NW: north wall + west wall
            &[2],    // (1,2) N middle
            &[1, 2], // (2,2) NE: east wall + north wall
        ],
    );
    let reject = reject(&[reject_byte]);

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
    Map::load(&lumps).expect("synthetic split map should load")
}

/// The open two-sector map: north sector floor 0 ceiling 128, no REJECT bits.
pub(crate) fn open_map() -> Map {
    split_map(0, 128, ML_TWOSIDED, 0x00)
}

/// The open map with tall ceilings (floor 0, ceiling 256). Vile-corpse
/// resurrection checks the corpse at four times its height, which the
/// standard 128-unit room can never fit.
pub(crate) fn tall_map() -> Map {
    let vertexes = vertexes(&[(0, 0), (256, 0), (256, 256), (0, 256), (0, 128), (256, 128)]);
    let linedefs = linedefs(&[
        (1, 0, ML_BLOCKING, 0, 0, 0, -1),
        (2, 1, ML_BLOCKING, 0, 0, 1, -1),
        (3, 2, ML_BLOCKING, 0, 0, 2, -1),
        (0, 3, ML_BLOCKING, 0, 0, 3, -1),
        (4, 5, ML_TWOSIDED, 0, 0, 4, 5),
    ]);
    let sidedefs = sidedefs(&[
        (0, 0, "-", "-", "WALL1", 0),
        (0, 0, "-", "-", "WALL1", 0),
        (0, 0, "-", "-", "WALL1", 1),
        (0, 0, "-", "-", "WALL1", 0),
        (0, 0, "-", "-", "FLOOR1", 0),
        (0, 0, "-", "-", "FLOOR1", 1),
    ]);
    let sectors = sectors(&[
        (0, 256, "FLOOR4_8", "CEIL3_5", 176, 0, 0),
        (0, 256, "FLOOR4_8", "CEIL3_5", 176, 0, 0),
    ]);
    let segs = segs(&[
        (0, 1, 0, 0, 0, 0),
        (1, 5, 0, 1, 0, 0),
        (4, 5, 0, 4, 0, 0),
        (2, 3, 0, 2, 0, 0),
        (5, 2, 0, 1, 0, 0),
        (5, 4, 0, 4, 1, 0),
    ]);
    let subsectors = subsectors(&[(3, 0), (3, 3)]);
    let nodes = nodes(&[NodeSpec {
        x: 0,
        y: 128,
        dx: 256,
        dy: 0,
        bbox: [[128, 0, 256, 0], [256, 128, 256, 0]],
        children: [NF_SUBSECTOR, NF_SUBSECTOR | 1],
    }]);
    let blockmap = blockmap_grid(
        (-8, -8),
        3,
        3,
        &[
            &[0, 3],
            &[0],
            &[0, 1],
            &[3, 4],
            &[4],
            &[1, 4],
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
    Map::load(&lumps).expect("synthetic tall map should load")
}

/// A baseline player with neutral inventory and no cheats.
pub(crate) fn test_player() -> Player {
    Player {
        mo: None,
        playerstate: PlayerState::Live,
        cmd: TicCmd::default(),
        viewz: crate::fixed::Fixed::ZERO,
        viewheight: crate::player::VIEWHEIGHT,
        deltaviewheight: crate::fixed::Fixed::ZERO,
        bob: crate::fixed::Fixed::ZERO,
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
        maxammo: [0; NUM_AMMO],
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

/// Spawns an `MT_PLAYER` avatar at `(x, y)` units and wires `players[0]`
/// to it. Returns the avatar's handle.
pub(crate) fn spawn_player(
    store: &mut crate::mobj::MobjStore,
    map: &Map,
    rng: &mut crate::rng::DoomRng,
    players: &mut Vec<Player>,
    x: i32,
    y: i32,
) -> crate::thinker::ThinkerId {
    use crate::mobj::{SpawnSpec, SpawnZ};

    let id = store.spawn_mobj(
        map,
        rng,
        SpawnSpec {
            skill: crate::mobj::Skill::Medium,
            x: crate::fixed::Fixed::from_int(x),
            y: crate::fixed::Fixed::from_int(y),
            z: SpawnZ::Floor,
            mobj_type: crate::info::MobjType::MT_PLAYER,
        },
    );
    if players.is_empty() {
        players.push(test_player());
    }
    players[0].mo = Some(id);
    players[0].health = 100;
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
) -> crate::thinker::ThinkerId {
    use crate::mobj::{SpawnSpec, SpawnZ};

    store.spawn_mobj(
        map,
        rng,
        SpawnSpec {
            skill: crate::mobj::Skill::Medium,
            x: crate::fixed::Fixed::from_int(x),
            y: crate::fixed::Fixed::from_int(y),
            z: SpawnZ::Floor,
            mobj_type,
        },
    )
}

/// The players vector with one live player.
pub(crate) fn players() -> Vec<Player> {
    vec![test_player()]
}

/// The level-persistent AI state (sound flood + brain ring) that a test
/// shares across the many `EnemyContext`s it builds.
pub(crate) struct TestWorld {
    pub flood: super::SoundFlood,
    pub brain: super::BrainState,
}

impl TestWorld {
    pub fn new(map: &Map) -> Self {
        TestWorld {
            flood: super::SoundFlood::new(map.sectors.len()),
            brain: super::BrainState::new(),
        }
    }
}

/// An `EnemyContext` with the given world state, reporting into the
/// given event buffer.
pub(crate) fn test_context<'a>(
    map: &'a Map,
    store: &'a mut crate::mobj::MobjStore,
    players: &'a mut Vec<Player>,
    rng: &'a mut crate::rng::DoomRng,
    world: &'a mut TestWorld,
    events: &'a mut Vec<super::GameEvent>,
) -> super::EnemyContext<'a> {
    super::EnemyContext::new(
        map,
        store,
        players,
        rng,
        &mut world.flood,
        &mut world.brain,
        events,
    )
}
