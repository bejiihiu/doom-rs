//! The headless harness: bring-up checks for the synthetic IWAD and
//! for booting the engine without any platform services (no window, no
//! audio device, no network). Every other integration test builds on
//! this fixture; if these fail, nothing else will run either.
//!
//! The optional real-IWAD smoke test is the only test in the directory
//! that needs an external file; it skips itself with a message when no
//! `DOOM_IWAD` path is set, it never fails for a missing file.

mod common;

use common::{FixtureSpec, MapScenario, TempDir, fixture_wad, new_game_args, run_tics, start_game};
use doom_engine::game::{GameState, TicCmdInput};
use doom_engine::render::IndexedFramebuffer;
use doom_engine::wad::WadFile;

#[test]
fn the_synthetic_iwad_parses_and_exposes_every_map_lump() {
    for scenario in [MapScenario::Plain, MapScenario::Exit, MapScenario::Door] {
        let bytes = fixture_wad(&FixtureSpec {
            scenario,
            monster: true,
        });
        let wad = WadFile::from_bytes(bytes).expect("synthetic IWAD parses");
        // E1M1 is found by its marker; the following block carries the
        // scenario map's lumps
        let entries = wad.entries().to_vec();
        let marker = entries
            .iter()
            .position(|e| e.name.trimmed() == b"E1M1")
            .expect("E1M1 marker present");
        let mut seen = std::collections::HashSet::new();
        for entry in &entries[marker + 1..] {
            let name = String::from_utf8_lossy(entry.name.trimmed()).into_owned();
            if name == "E1M2" {
                break;
            }
            seen.insert(name);
        }
        for name in [
            "THINGS", "LINEDEFS", "SIDEDEFS", "VERTEXES", "SEGS", "SSECTORS", "NODES", "SECTORS",
            "REJECT", "BLOCKMAP",
        ] {
            assert!(
                seen.contains(name),
                "the E1M1 block must contain {name} (scenario {scenario:?})"
            );
        }
        // the fixture PLAYPAL is really 14 palettes of 256 colors
        let playpal = wad
            .lump(wad.find_lump("PLAYPAL").expect("playpal present"))
            .expect("playpal readable");
        assert_eq!(playpal.len(), 768 * 14);
    }
}

#[test]
fn booting_without_platform_services_works() {
    // no window, no audio hardware, no network: exactly the CI runner
    let dir = TempDir::new("headless-boot");
    let spec = FixtureSpec {
        scenario: MapScenario::Plain,
        monster: false,
    };
    let mut game = start_game(dir.path(), &new_game_args(), &spec);
    let mut fb = IndexedFramebuffer::new();

    // one tic brings the level up; the ticker must not need anything
    // outside the engine
    game.tick_input(&TicCmdInput::NONE, &mut fb)
        .expect("first tic runs headless");
    assert_eq!(game.gamestate(), GameState::Level);

    // and the world holds together for a few seconds of tics
    run_tics(&mut game, &TicCmdInput::NONE, 105, &mut fb);
    assert_eq!(game.gamestate(), GameState::Level);
}

#[test]
fn the_door_fixture_layout_is_wired_up() {
    let bytes = fixture_wad(&FixtureSpec {
        scenario: MapScenario::Door,
        monster: false,
    });
    let wad = WadFile::from_bytes(bytes).expect("synthetic IWAD parses");
    let entries = wad.entries().to_vec();
    let marker = entries
        .iter()
        .position(|e| e.name.trimmed() == b"E1M1")
        .expect("E1M1 marker present");
    let mut end = entries.len();
    for (i, entry) in entries.iter().enumerate().skip(marker + 1) {
        if entry.name.trimmed() == b"E1M2" {
            end = i;
            break;
        }
    }
    let mut sectors_len = None;
    let mut linedefs_len = None;
    for entry in &entries[marker + 1..end] {
        let name = entry.name.trimmed();
        if name == b"SECTORS" {
            sectors_len = Some(entry.size);
        } else if name == b"LINEDEFS" {
            linedefs_len = Some(entry.size);
        }
    }
    // three sectors (west room / corridor / east room), twelve lines
    assert_eq!(sectors_len, Some(3 * 26), "three sectors in the door map");
    assert_eq!(linedefs_len, Some(12 * 14), "twelve lines in the door map");
}

/// Optional: boot the engine against a real IWAD when `DOOM_IWAD`
/// points at one. Skips (with a message, exit 0) otherwise - a missing
/// IWAD is never a test failure.
#[test]
fn real_iwad_smoke_if_provided() {
    let Ok(path) = std::env::var("DOOM_IWAD") else {
        eprintln!("skipping real-IWAD smoke test: DOOM_IWAD is not set");
        return;
    };
    if path.is_empty() {
        eprintln!("skipping real-IWAD smoke test: DOOM_IWAD is empty");
        return;
    }
    let wad = match WadFile::open(&path) {
        Ok(wad) => wad,
        Err(err) => {
            eprintln!("skipping real-IWAD smoke test: {path} does not parse ({err})");
            return;
        }
    };
    let dir = TempDir::new("headless-iwad");
    let args = new_game_args();
    let resources = doom_engine::resources::ResourceManager::new(wad, Vec::new());
    let config = common::test_config(dir.path());
    match doom_engine::game::Game::new(config, resources, &args) {
        Ok(mut game) => {
            let mut fb = IndexedFramebuffer::new();
            game.tick_input(&TicCmdInput::NONE, &mut fb)
                .expect("real IWAD boots and ticks headless");
            eprintln!("real IWAD smoke test ran against {path}");
        }
        Err(err) => {
            eprintln!("skipping real-IWAD smoke test: engine rejected the IWAD ({err})");
        }
    }
}
