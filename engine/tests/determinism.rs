//! Determinism checks: two identical runs of the same scripted input
//! sequence must produce the identical world, tic for tic.
//!
//! The comparison happens on three levels, from coarse to precise:
//!
//! 1. public per-tic snapshots - gametic, leveltime, game state, player
//!    position/angle, the kill/item/secret tallies, and a checksum of
//!    the rendered framebuffer;
//! 2. the save-game block ([`doom_engine::game::Game::save_game_slot`]) -
//!    the engine's own serializer captures actor health and ammo, thinker
//!    states, the RNG index and the sector/line state, so a byte compare
//!    of the two save files is a full-world comparison, not an opinion;
//! 3. the headline counters on their own, with a readable report on
//!    mismatch - the test names the first tic that diverged.
//!
//! The input script exercises the interesting paths: level bring-up,
//! movement, turning, firing (the pistol RNG), a chasing monster, and
//! the status bar's animated face. Anything frame-time based would break
//! this test, which is exactly why it exists.
//!
//! If a real IWAD demo (`-playdemo`) is ever available this harness
//! should drive it the same way; the synthetic map is the only fixture
//! CI can count on, so that is what runs here.

mod common;

use common::{FixtureSpec, MapScenario, TempDir, new_game_args, start_game};
use doom_engine::game::{Game, GameState, TicCmdInput};
use doom_engine::render::IndexedFramebuffer;

/// One frame of observable world state through the public API.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TicSnapshot {
    gametic: i32,
    leveltime: i32,
    state: GameState,
    x: doom_engine::fixed::Fixed,
    y: doom_engine::fixed::Fixed,
    angle: u32,
    kills: i32,
    items: i32,
    secrets: i32,
    framebuffer_sum: u64,
}

fn framebuffer_sum(fb: &IndexedFramebuffer) -> u64 {
    // FNV-1a over the palette indices; plenty for change detection
    let mut hash = 0xcbf29ce484222325u64;
    for y in 0..200 {
        for &px in fb.row(y).expect("row in range") {
            hash ^= u64::from(px);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

fn snapshot(game: &Game, fb: &IndexedFramebuffer) -> TicSnapshot {
    let (x, y, angle) = game.player_position().expect("player in game");
    TicSnapshot {
        gametic: game.gametic(),
        leveltime: game.leveltime(),
        state: game.gamestate(),
        x,
        y,
        angle: angle.to_raw(),
        kills: game.total_kills(),
        items: game.total_items(),
        secrets: game.secrets_found(),
        framebuffer_sum: framebuffer_sum(fb),
    }
}

/// The scripted input: a movement-and-combat walk across the room.
/// `(tics, input)` pairs, run in order.
const SCRIPT: &[(usize, TicCmdInput)] = &[
    (40, TicCmdInput::NONE), // level comes up, monster wakes up
    (
        30,
        TicCmdInput {
            forward: true,
            ..TicCmdInput::NONE
        },
    ),
    (
        25,
        TicCmdInput {
            forward: true,
            speed: true,
            ..TicCmdInput::NONE
        },
    ),
    (
        15,
        TicCmdInput {
            turn_right: true,
            ..TicCmdInput::NONE
        },
    ),
    (
        50,
        TicCmdInput {
            attack: true,
            ..TicCmdInput::NONE
        },
    ),
    (
        40,
        TicCmdInput {
            forward: true,
            attack: true,
            ..TicCmdInput::NONE
        },
    ),
    (
        20,
        TicCmdInput {
            strafe_left: true,
            ..TicCmdInput::NONE
        },
    ),
    (
        30,
        TicCmdInput {
            attack: true,
            ..TicCmdInput::NONE
        },
    ),
    (
        12,
        TicCmdInput {
            turn_left: true,
            ..TicCmdInput::NONE
        },
    ),
    (60, TicCmdInput::NONE),
];

const SCRIPT_LEN: usize = 40 + 30 + 25 + 15 + 50 + 40 + 20 + 30 + 12 + 60;

fn run_scripted_scenario(dir: &TempDir, spec: &FixtureSpec) -> (Vec<TicSnapshot>, Vec<u8>) {
    let mut game = start_game(dir.path(), &new_game_args(), spec);
    let mut fb = IndexedFramebuffer::new();
    let mut snaps = Vec::with_capacity(SCRIPT_LEN);

    for &(tics, ref input) in SCRIPT {
        for _ in 0..tics {
            game.tick_input(input, &mut fb).expect("tic runs");
            snaps.push(snapshot(&game, &fb));
        }
    }

    // the save-game block is the engine's own full-world serializer;
    // the description is fixed so the bytes are a pure function of the
    // world state
    game.save_game_slot(0, "probe");
    let save = std::fs::read(dir.path().join("doomsav0.dsg")).expect("save written");
    (snaps, save)
}

fn diff_snapshot(index: usize, a: &TicSnapshot, b: &TicSnapshot) -> String {
    let mut what = Vec::new();
    if a.gametic != b.gametic {
        what.push(format!("gametic {} vs {}", a.gametic, b.gametic));
    }
    if a.leveltime != b.leveltime {
        what.push(format!("leveltime {} vs {}", a.leveltime, b.leveltime));
    }
    if a.state != b.state {
        what.push(format!("state {:?} vs {:?}", a.state, b.state));
    }
    if a.x != b.x || a.y != b.y || a.angle != b.angle {
        what.push(format!(
            "pose ({}, {}, angle {:#x}) vs ({}, {}, angle {:#x})",
            a.x, a.y, a.angle, b.x, b.y, b.angle
        ));
    }
    if a.kills != b.kills || a.items != b.items || a.secrets != b.secrets {
        what.push(format!(
            "tallies (k={} i={} s={}) vs (k={} i={} s={})",
            a.kills, a.items, a.secrets, b.kills, b.items, b.secrets
        ));
    }
    if a.framebuffer_sum != b.framebuffer_sum {
        what.push(format!(
            "framebuffer checksum {:#x} vs {:#x}",
            a.framebuffer_sum, b.framebuffer_sum
        ));
    }
    format!("tic {index}: {}", what.join("; "))
}

#[test]
fn two_runs_produce_identical_world_state() {
    let spec = FixtureSpec {
        scenario: MapScenario::Plain,
        monster: true,
    };
    let dir_a = TempDir::new("det-a");
    let dir_b = TempDir::new("det-b");

    let (snaps_a, save_a) = run_scripted_scenario(&dir_a, &spec);
    let (snaps_b, save_b) = run_scripted_scenario(&dir_b, &spec);

    assert_eq!(snaps_a.len(), SCRIPT_LEN);
    assert_eq!(
        snaps_a.len(),
        snaps_b.len(),
        "both runs produced the full script"
    );

    // report every diverging tic, not just the first, so a real desync
    // is debuggable from the failure message alone
    let mut divergences = Vec::new();
    for (index, (a, b)) in snaps_a.iter().zip(&snaps_b).enumerate() {
        if a != b {
            divergences.push(diff_snapshot(index, a, b));
        }
    }
    assert!(
        divergences.is_empty(),
        "runs diverged on {} tic(s):\n{}",
        divergences.len(),
        divergences.join("\n")
    );

    if save_a != save_b {
        let first = save_a
            .iter()
            .zip(&save_b)
            .position(|(x, y)| x != y)
            .map(|i| i.to_string())
            .unwrap_or_else(|| "length differs".to_string());
        panic!(
            "save-game blocks diverge (save_a = {} bytes, save_b = {} bytes, first differing byte: {first})",
            save_a.len(),
            save_b.len()
        );
    }
}

#[test]
fn the_script_advances_gametic() {
    let spec = FixtureSpec {
        scenario: MapScenario::Plain,
        monster: true,
    };
    let dir = TempDir::new("det-sanity");
    let (snaps, _save) = run_scripted_scenario(&dir, &spec);
    let first = snaps.first().expect("script ran");
    let last = snaps.last().expect("script ran");
    assert!(last.gametic > first.gametic, "gametic advanced");
    assert_eq!(snaps.len(), SCRIPT_LEN);
    // sanity: after spawning, the player is somewhere in the room
    assert!(last.x > doom_engine::fixed::Fixed::from_int(1));
}
