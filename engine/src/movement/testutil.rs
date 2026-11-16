//! Shared synthetic fixtures for movement tests. Test-only.
//!
//! The workhorse is [`split_map`]: a 256x256 world split by a two-sided
//! divider at y = 128 into a south sector (floor 0, ceiling 128) and a north
//! sector with a configurable floor/ceiling. All coordinates are map units.

#![cfg(test)]

use crate::map::testutil::{
    NodeSpec, blockmap_grid, linedefs, nodes, reject, sectors, segs, sidedefs, subsectors, vertexes,
};
use crate::map::types::{ML_BLOCKING, ML_TWOSIDED, NF_SUBSECTOR};
use crate::map::{Map, MapLumps};

/// Builds the split-room map.
///
/// * `north_floor`/`north_ceil` — the north sector's floor/ceiling heights
///   (use 128/0 for a closed window, or anything else to test steps).
/// * `north_ceil_pic` — the north sector's ceiling flat name.
/// * `divider_flags` — flags for the divider linedef (`ML_TWOSIDED | ...`).
/// * `divider_special` — the divider's special, if any.
fn build_map(
    north_floor: i16,
    north_ceil: i16,
    north_ceil_pic: &str,
    divider_flags: i16,
    divider_special: i16,
) -> Map {
    // a 256x256 room wound CLOCKWISE — interior faces (point_on_side == 0)
    // point into the room, like real DOOM maps; the slide traversal rejects
    // the back side of one-sided lines
    let vertexes = vertexes(&[(0, 0), (256, 0), (256, 256), (0, 256), (0, 128), (256, 128)]);
    let linedefs = linedefs(&[
        (1, 0, ML_BLOCKING, 0, 0, 0, -1), // L0 south wall: (256,0) -> (0,0)
        (2, 1, ML_BLOCKING, 0, 0, 1, -1), // L1 east wall:  (256,256) -> (256,0)
        (3, 2, ML_BLOCKING, 0, 0, 2, -1), // L2 north wall: (0,256) -> (256,256)
        (0, 3, ML_BLOCKING, 0, 0, 3, -1), // L3 west wall:  (0,0) -> (0,256)
        (4, 5, divider_flags, divider_special, 0, 4, 5), // L4 divider:   (0,128) -> (256,128)
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
        (
            north_floor,
            north_ceil,
            "FLOOR4_8",
            north_ceil_pic,
            176,
            0,
            0,
        ),
    ]);
    // subsector 0: the south room; subsector 1: the north room
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
    // 3x3 grid at (-8, -8): cells are 128 units wide
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
    let reject = reject(&[0x00]);
    let things: Vec<u8> = Vec::new();

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
        things: &things,
    };
    Map::load(&lumps).expect("synthetic split map should load")
}

/// The default open two-sector map: north sector floor 0 ceiling 128.
pub(crate) fn open_map() -> Map {
    split_map(0, 128, ML_TWOSIDED, 0)
}

/// The split-room map with a configurable north sector.
pub(crate) fn split_map(
    north_floor: i16,
    north_ceil: i16,
    divider_flags: i16,
    divider_special: i16,
) -> Map {
    build_map(
        north_floor,
        north_ceil,
        "CEIL3_5",
        divider_flags,
        divider_special,
    )
}

/// A map whose north ceiling is the sky flat — missiles must not explode
/// against the divider here. The north ceiling is lowered to 4 units by the
/// line itself (high enough for the south sector, below a missile's height),
/// which is how vanilla's sky-hack walls actually work: the line clips the
/// ceiling, and the sector behind it is a sky.
pub(crate) fn sky_map() -> Map {
    build_map(0, 4, "F_SKY1", ML_TWOSIDED, 0)
}

/// A fresh (map, store) pair for the default open two-sector map, with the
/// store's runtime tables sized.
pub(crate) fn fixture() -> (Map, crate::mobj::MobjStore) {
    let map = open_map();
    let mut store = crate::mobj::MobjStore::new();
    store.reset_for_level(&map);
    (map, store)
}

/// The closed-cap map: north sector ceiling 0 — a zero-height window.
pub(crate) fn closed_map() -> Map {
    split_map(0, 0, ML_TWOSIDED, 0)
}

/// A baseline player with neutral inventory and no cheats. Tests tweak the
/// fields they care about from this.
pub(crate) fn test_player() -> crate::player::Player {
    use crate::player::{
        MAXPLAYERS, NUM_AMMO, NUM_CARDS, NUM_POWERS, NUM_PSPRITES, NUM_WEAPONS, Player,
        PlayerState, PspDef, TicCmd, Weapon,
    };

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

/// Spawns a mobj of `mobj_type` at whole-unit coordinates
/// `(x, y)`, snapping z to the floor.
pub(crate) fn spawn_at(
    store: &mut crate::mobj::MobjStore,
    map: &Map,
    rng: &mut crate::rng::DoomRng,
    mobj_type: crate::info::MobjType,
    x: i32,
    y: i32,
) -> crate::thinker::ThinkerId {
    use crate::mobj::{Skill, SpawnSpec, SpawnZ};

    store.spawn_mobj(
        map,
        rng,
        SpawnSpec {
            skill: Skill::Medium,
            x: crate::fixed::Fixed::from_int(x),
            y: crate::fixed::Fixed::from_int(y),
            z: SpawnZ::Floor,
            mobj_type,
        },
    )
}
