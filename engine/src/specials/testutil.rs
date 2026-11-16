//! Shared fixtures for the specials tests. Test-only.
//!
//! The workhorse is [`split_map`]: a 256x256 room split at y=128 into a
//! south sector (floor 0, ceiling 128) and a north sector with configurable
//! heights and tag. Lines are wound clockwise, like real DOOM maps.
//! [`test_pass`] gives thinker handlers the pass-shaped context without
//! standing up a store.

use crate::map::Map;
use crate::map::MapLumps;
use crate::map::testutil::{
    NodeSpec, blockmap_grid, linedefs, nodes, reject, sectors, segs, sidedefs, subsectors, vertexes,
};
use crate::map::types::{ML_BLOCKING, ML_TWOSIDED, NF_SUBSECTOR};
use crate::player::{
    MAXPLAYERS, NUM_AMMO, NUM_CARDS, NUM_POWERS, NUM_PSPRITES, NUM_WEAPONS, Player, PlayerState,
    PspDef, TicCmd, Weapon,
};

/// Builds the split-room map. `north_floor`/`north_ceil` are the north
/// sector heights; `north_tag` its tag.
pub(crate) fn split_map(north_floor: i16, north_ceil: i16, north_tag: i16) -> Map {
    let vertexes = vertexes(&[(0, 0), (256, 0), (256, 256), (0, 256), (0, 128), (256, 128)]);
    let linedefs = linedefs(&[
        (1, 0, ML_BLOCKING, 0, 0, 0, -1), // south wall
        (2, 1, ML_BLOCKING, 0, 0, 1, -1), // east wall
        (3, 2, ML_BLOCKING, 0, 0, 2, -1), // north wall
        (0, 3, ML_BLOCKING, 0, 0, 3, -1), // west wall
        (4, 5, ML_TWOSIDED, 0, 0, 4, 5),  // divider
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
        (0, 128, "FLOOR4_8", "CEIL3_5", 176, 0, 0),
        (
            north_floor,
            north_ceil,
            "FLOOR4_8",
            "CEIL3_5",
            176,
            0,
            north_tag,
        ),
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
    Map::load(&lumps).expect("synthetic split map should load")
}

/// The split room with configurable floor pics and sector specials.
/// The south sector sits at floor 0, the north at floor 32. The pics are
/// raw names, uppercased+padded here like the loader stores them.
pub(crate) fn split_map_custom(
    south_pic: &str,
    south_special: i16,
    north_pic: &str,
    north_special: i16,
) -> Map {
    let vertexes = vertexes(&[(0, 0), (256, 0), (256, 256), (0, 256), (0, 128), (256, 128)]);
    let linedefs = linedefs(&[
        (1, 0, ML_BLOCKING, 0, 0, 0, -1), // south wall
        (2, 1, ML_BLOCKING, 0, 0, 1, -1), // east wall
        (3, 2, ML_BLOCKING, 0, 0, 2, -1), // north wall
        (0, 3, ML_BLOCKING, 0, 0, 3, -1), // west wall
        (4, 5, ML_TWOSIDED, 0, 0, 4, 5),  // divider
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
        (0, 128, south_pic, "CEIL3_5", 176, south_special, 0),
        (32, 128, north_pic, "CEIL3_5", 176, north_special, 0),
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
    Map::load(&lumps).expect("synthetic split map should load")
}

/// Three stacked sectors for the donut elevator test.
///
/// ```text
/// y=256  north sector (floor 32, pic HOLE1)
/// y=170  -- divider 1 (two-sided)
///        middle sector (the rising slime, floor 0, pic SLIME1)
/// y=85   -- divider 0 (two-sided)
///        south sector, tagged, the lowering ring (floor 64, pic RING1)
/// y=0
/// ```
///
/// The divider linedefs come first so the ring's first line
/// (`sector->lines[0]`) is the south-to-middle divider, as the donut
/// expects.
pub(crate) fn donut_map() -> Map {
    let vertexes = vertexes(&[
        (0, 0),
        (256, 0),
        (256, 85),
        (0, 85),
        (256, 170),
        (0, 170),
        (256, 256),
        (0, 256),
    ]);
    // divider linedefs come first so the ring's first line is the
    // south-to-middle divider, as the donut expects. the divider fronts
    // face the *slime* sector (middle), backs face south/north, which is
    // what the donut's "skip the line back to s1" check relies on.
    let linedefs = linedefs(&[
        (2, 3, ML_TWOSIDED, 0, 0, 0, 1), // divider south-middle (front middle)
        (5, 4, ML_TWOSIDED, 0, 0, 2, 3), // divider middle-north (front middle)
        (1, 0, ML_BLOCKING, 0, 0, 4, -1), // south wall
        (2, 1, ML_BLOCKING, 0, 0, 5, -1), // east south
        (4, 2, ML_BLOCKING, 0, 0, 6, -1), // east middle
        (6, 4, ML_BLOCKING, 0, 0, 7, -1), // east north
        (7, 6, ML_BLOCKING, 0, 0, 8, -1), // north wall
        (5, 7, ML_BLOCKING, 0, 0, 9, -1), // west north
        (3, 5, ML_BLOCKING, 0, 0, 10, -1), // west middle
        (0, 3, ML_BLOCKING, 0, 0, 11, -1), // west south
    ]);
    let sidedefs = sidedefs(&[
        (0, 0, "-", "-", "WALL1", 1), // 0: dividerSM front (middle)
        (0, 0, "-", "-", "WALL1", 0), // 1: dividerSM back (south)
        (0, 0, "-", "-", "WALL1", 1), // 2: dividerMN front (middle)
        (0, 0, "-", "-", "WALL1", 2), // 3: dividerMN back (north)
        (0, 0, "-", "-", "WALL1", 0), // 4: south wall
        (0, 0, "-", "-", "WALL1", 0), // 5: east south
        (0, 0, "-", "-", "WALL1", 1), // 6: east middle
        (0, 0, "-", "-", "WALL1", 2), // 7: east north
        (0, 0, "-", "-", "WALL1", 2), // 8: north wall
        (0, 0, "-", "-", "WALL1", 2), // 9: west north
        (0, 0, "-", "-", "WALL1", 1), // 10: west middle
        (0, 0, "-", "-", "WALL1", 0), // 11: west south
    ]);
    let sectors = sectors(&[
        (64, 128, "RING1", "CEIL3_5", 176, 0, 99), // south ring, tagged 99
        (0, 128, "SLIME1", "CEIL3_5", 160, 0, 0),  // middle slime
        (32, 128, "HOLE1", "CEIL3_5", 176, 0, 0),  // north center island
    ]);
    let segs = segs(&[
        (1, 0, 0, 2, 0, 0), // south wall
        (2, 1, 0, 3, 0, 0), // east south
        (0, 3, 0, 9, 0, 0), // west south
        (3, 2, 0, 0, 1, 0), // divider SM, side 1 (south facing)
        (4, 2, 0, 4, 0, 0), // east middle
        (2, 3, 0, 0, 0, 0), // divider SM, side 0 (middle facing)
        (3, 5, 0, 8, 0, 0), // west middle
        (5, 4, 0, 1, 0, 0), // divider MN, side 0 (middle facing)
        (6, 4, 0, 5, 0, 0), // east north
        (6, 7, 0, 6, 0, 0), // north wall
        (5, 7, 0, 7, 0, 0), // west north
        (4, 5, 0, 1, 1, 0), // divider MN, side 1 (north facing)
    ]);
    let subsectors = subsectors(&[(4, 0), (4, 4), (4, 8)]);
    let nodes = nodes(&[
        NodeSpec {
            x: 0,
            y: 170,
            dx: 256,
            dy: 0,
            bbox: [[170, 0, 256, 0], [256, 170, 256, 0]],
            children: [NF_SUBSECTOR | 1, NF_SUBSECTOR | 2],
        },
        NodeSpec {
            x: 0,
            y: 85,
            dx: 256,
            dy: 0,
            bbox: [[85, 0, 256, 0], [256, 85, 256, 0]],
            children: [NF_SUBSECTOR, 0],
        },
    ]);
    let blockmap = blockmap_grid(
        (-8, -8),
        3,
        3,
        &[
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
        ],
    );
    let reject = reject(&[0x00, 0x00]);

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
    Map::load(&lumps).expect("synthetic donut map should load")
}

/// A player with neutral inventory, one card slot flag each off by default.
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

/// Spawns an `MT_PLAYER` avatar at `(x, y)` map units and wires
/// `players[0]` to it. Also links the avatar back to the player record
/// (`mobj.player`), like the spawn code does for real avatars.
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
    // the reverse link — mobj.player drives the player-only code paths
    if let Some(m) = store.mobj_mut(id) {
        m.player = Some(0);
    }
    id
}

/// Spawns a non-player mobj at `(x, y)` map units snapped to the floor.
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

/// Pass-shaped context for driving thinker handlers directly (no store).
pub(crate) fn test_pass<'a>(
    map: &'a mut Map,
    rng: &'a mut crate::rng::DoomRng,
    events: &'a mut Vec<crate::specials::SpecialEvent>,
) -> crate::specials::PassCtx<'a> {
    crate::specials::PassCtx {
        map,
        rng,
        leveltime: 0,
        events,
    }
}
