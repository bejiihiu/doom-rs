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
