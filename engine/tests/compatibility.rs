//! Scripted end-to-end scenarios through the public `Game` API only.
//!
//! Each test drives the same synthetic map the other integration tests
//! use, and asserts on observable state transitions rather than on
//! internals: spawn position, level state, monster death, the door
//! thinker (by walking through the doorway it unseals), the intermission
//! flow out of a level exit, and the save/load round-trip. If a future
//! refactor silently breaks one of these flows, this is the file that
//! should notice.

mod common;

use common::{FixtureSpec, MapScenario, TempDir, new_game_args, run_tics, start_game};
use doom_engine::fixed::Fixed;
use doom_engine::game::{GameState, TicCmdInput};
use doom_engine::render::IndexedFramebuffer;

#[test]
fn player_spawns_at_the_thing_position() {
    let dir = TempDir::new("compat-spawn");
    let spec = FixtureSpec {
        scenario: MapScenario::Plain,
        monster: false,
    };
    let mut game = start_game(dir.path(), &new_game_args(), &spec);
    let mut fb = IndexedFramebuffer::new();

    // the deferred new game starts on the first tic
    game.tick_input(&TicCmdInput::NONE, &mut fb)
        .expect("first tic runs");
    assert_eq!(
        game.gamestate(),
        GameState::Level,
        "new game starts in the level"
    );
    let position = game.player_position().expect("player spawned");
    assert_eq!(position.0, Fixed::from_int(128));
    assert_eq!(position.1, Fixed::from_int(128));
    assert_eq!(position.2.to_raw(), 0, "thing angle 0 = east");

    // standing still for a second must not move the player
    let before = position;
    run_tics(&mut game, &TicCmdInput::NONE, 35, &mut fb);
    assert_eq!(game.player_position(), Some(before), "no drift while idle");
}

#[test]
fn weapon_fire_changes_the_world() {
    let dir = TempDir::new("compat-fire");
    let spec = FixtureSpec {
        scenario: MapScenario::Plain,
        monster: true,
    };
    let mut game = start_game(dir.path(), &new_game_args(), &spec);
    let mut fb = IndexedFramebuffer::new();
    run_tics(&mut game, &TicCmdInput::NONE, 1, &mut fb);

    // snapshot the world before firing: the save block captures ammo,
    // the psprites and the RNG, so a byte compare sees *any* change.
    // `game.total_kills()` is the level's monster count (vanilla counts
    // them at spawn), so the player's kills come out of the save block.
    game.save_game_slot(0, "probe");
    let before = std::fs::read(dir.path().join("doomsav0.dsg")).expect("save written");
    let block_before = doom_engine::game::SaveBlock::read(&before).expect("save parses");
    assert_eq!(block_before.players[0].killcount, 0, "no kills yet");
    assert_eq!(block_before.players[0].ammo[0], 50, "fresh pistol clips");
    assert_eq!(game.total_kills(), 1, "the fixture spawns one monster");

    let fire = TicCmdInput {
        attack: true,
        ..TicCmdInput::NONE
    };
    // hold the trigger; a possessed is 20 HP against 5-15 per shot
    let mut killed = false;
    for _ in 0..10 {
        run_tics(&mut game, &fire, 30, &mut fb);
        game.save_game_slot(0, "probe");
        let now = std::fs::read(dir.path().join("doomsav0.dsg")).expect("save written");
        let block = doom_engine::game::SaveBlock::read(&now).expect("save parses");
        if block.players[0].killcount > 0 {
            killed = true;
            break;
        }
    }

    game.save_game_slot(0, "probe");
    let after = std::fs::read(dir.path().join("doomsav0.dsg")).expect("save written");
    let block_after = doom_engine::game::SaveBlock::read(&after).expect("save parses");

    assert_ne!(before, after, "firing the pistol changed no state at all");
    assert!(killed, "the possessed should fall within 300 attack tics");
    assert_eq!(block_after.players[0].killcount, 1);
    assert!(
        block_after.players[0].ammo[0] < block_before.players[0].ammo[0],
        "the pistol should have spent rounds: {0} -> {1}",
        block_before.players[0].ammo[0],
        block_after.players[0].ammo[0]
    );
}

#[test]
fn door_opens_when_used_and_player_can_walk_through() {
    let dir = TempDir::new("compat-door");
    let spec = FixtureSpec {
        scenario: MapScenario::Door,
        monster: false,
    };
    let mut game = start_game(dir.path(), &new_game_args(), &spec);
    let mut fb = IndexedFramebuffer::new();
    run_tics(&mut game, &TicCmdInput::NONE, 1, &mut fb);

    // walk east until the sealed corridor wall stops the player
    let walk = TicCmdInput {
        forward: true,
        ..TicCmdInput::NONE
    };
    run_tics(&mut game, &walk, 60, &mut fb);
    let blocked = game.player_position().expect("player in game").0;
    assert!(
        blocked < Fixed::from_int(176),
        "player should be stopped by the sealed door, got x={}",
        blocked
    );

    // open it: use the door line from inside the west room
    let use_button = TicCmdInput {
        use_button: true,
        ..TicCmdInput::NONE
    };
    game.tick_input(&use_button, &mut fb).expect("use tic runs");

    // the corridor ceiling rises from 0 to 124 at 2 units per tic
    run_tics(&mut game, &TicCmdInput::NONE, 70, &mut fb);

    // now the doorway is open: walking east must carry the player into
    // the east room, past the corridor (x > 192)
    run_tics(&mut game, &walk, 120, &mut fb);
    let through = game.player_position().expect("player in game").0;
    assert!(
        through > Fixed::from_int(192),
        "player should have walked through the opened door, got x={}",
        through
    );
}

/// Map index as serialized in the save block.
///
/// The layout is `DOOMSAV\0` (8) + version (1) + description (24) +
/// game_mode (1) + gameskill (1) + gameepisode (1) + gamemap (1).
/// Parsing it here, instead of reaching for engine internals, keeps
/// the test honest.
fn saved_gamemap(save_dir: &std::path::Path) -> u8 {
    let path = save_dir.join("doomsav0.dsg");
    let bytes = std::fs::read(&path).expect("save file readable");
    assert!(
        bytes.len() > 36,
        "save block too short: {} bytes",
        bytes.len()
    );
    bytes[36]
}

#[test]
fn exit_line_triggers_the_intermission_flow() {
    let dir = TempDir::new("compat-exit");
    let spec = FixtureSpec {
        scenario: MapScenario::Exit,
        monster: false,
    };
    let mut game = start_game(dir.path(), &new_game_args(), &spec);
    let mut fb = IndexedFramebuffer::new();
    run_tics(&mut game, &TicCmdInput::NONE, 1, &mut fb);

    // the exit line (special 11) sits in front of the player; use it
    let use_button = TicCmdInput {
        use_button: true,
        ..TicCmdInput::NONE
    };
    game.tick_input(&use_button, &mut fb).expect("use tic runs");

    // the level-exit flow runs: the stats screen comes up (the fixture
    // carries the intermission art on purpose)
    let mut saw_intermission = false;
    for _ in 0..5 {
        game.tick_input(&TicCmdInput::NONE, &mut fb)
            .expect("tic runs");
        if game.gamestate() == GameState::Intermission {
            saw_intermission = true;
            break;
        }
    }
    assert!(
        saw_intermission,
        "exit line did not reach the intermission (state = {:?})",
        game.gamestate()
    );

    if saw_intermission {
        // no buttons yet: the screen must stay up
        run_tics(&mut game, &TicCmdInput::NONE, 5, &mut fb);
        assert_eq!(game.gamestate(), GameState::Intermission);
        run_tics(&mut game, &TicCmdInput::NONE, 2, &mut fb);
        assert_eq!(game.gamestate(), GameState::Intermission);

        // press use until the intermission lets go of the player
        let mut advanced = false;
        for _ in 0..60 {
            game.tick_input(&use_button, &mut fb).expect("use tic runs");
            game.tick_input(&TicCmdInput::NONE, &mut fb)
                .expect("tic runs");
            if game.gamestate() == GameState::Level {
                advanced = true;
                break;
            }
        }
        assert!(advanced, "intermission never dismissed");
    }

    // the level after E1M1 for a registered game is E1M2
    assert_eq!(game.gamestate(), GameState::Level);
    game.save_game_slot(0, "probe");
    assert_eq!(
        saved_gamemap(dir.path()),
        2,
        "exit carried the player to E1M2"
    );
}

#[test]
fn save_and_load_round_trips_through_the_public_api() {
    let dir = TempDir::new("compat-save");
    let spec = FixtureSpec {
        scenario: MapScenario::Plain,
        monster: false,
    };
    let mut game = start_game(dir.path(), &new_game_args(), &spec);
    let mut fb = IndexedFramebuffer::new();

    // walk a bit so the save captures a non-spawn state
    let walk = TicCmdInput {
        forward: true,
        ..TicCmdInput::NONE
    };
    run_tics(&mut game, &walk, 25, &mut fb);
    let moved = game.player_position().expect("player in game");

    game.save_game_slot(0, "compat");
    let saved_position = moved;

    // leave the save behind; keep walking so the world visibly differs
    run_tics(
        &mut game,
        &TicCmdInput {
            backward: true,
            ..TicCmdInput::NONE
        },
        10,
        &mut fb,
    );
    let walked_back = game.player_position().expect("player in game");
    assert_ne!(saved_position, walked_back);

    game.load_game_slot(0);
    let restored = game.player_position().expect("player restored");
    assert_eq!(
        restored, saved_position,
        "save/load restored the exact pose"
    );
}
