//! Integration tests for the F8 frame-stats overlay: the toggle key and
//! the "drawn on every screen, above everything" property.

mod common;

use common::{FixtureSpec, MapScenario, TempDir, new_game_args, run_tics, start_game};
use doom_engine::game::{GameState, InputEvent, TicCmdInput, keys};
use doom_engine::render::IndexedFramebuffer;

#[test]
fn f8_toggles_the_overlay_on_and_off() {
    let dir = TempDir::new("fps-toggle");
    let spec = FixtureSpec {
        scenario: MapScenario::Plain,
        monster: false,
    };
    let mut game = start_game(dir.path(), &new_game_args(), &spec);
    let mut fb = IndexedFramebuffer::new();
    run_tics(&mut game, &TicCmdInput::NONE, 2, &mut fb);

    assert!(!game.show_fps(), "overlay starts disabled");
    game.handle_event(&InputEvent::KeyDown(keys::F8));
    assert!(game.show_fps(), "F8 enables the overlay");
    game.handle_event(&InputEvent::KeyDown(keys::F8));
    assert!(!game.show_fps(), "F8 disables the overlay again");
}

#[test]
fn f8_toggles_the_overlay_on_the_title_screen_too() {
    let dir = TempDir::new("fps-toggle-title");
    let spec = FixtureSpec {
        scenario: MapScenario::Plain,
        monster: false,
    };
    let mut game = start_game(dir.path(), &new_game_args(), &spec);
    // a brand-new game sits on the demo screen; F8 must work there as well
    assert_eq!(game.gamestate(), GameState::DemoScreen);
    game.handle_event(&InputEvent::KeyDown(keys::F8));
    assert!(game.show_fps());
}

#[test]
fn overlay_draws_a_black_band_over_the_top_of_the_frame() {
    let dir = TempDir::new("fps-draw");
    // the door scene actually rasterizes something in the top rows
    let spec = FixtureSpec {
        scenario: MapScenario::Door,
        monster: false,
    };
    let mut game = start_game(dir.path(), &new_game_args(), &spec);
    let mut fb = IndexedFramebuffer::new();

    run_tics(&mut game, &TicCmdInput::NONE, 150, &mut fb);
    assert!(
        fb.row(2)
            .expect("row in range")
            .iter()
            .any(|&px| px != 0),
        "expected a rendered view in the top rows before the overlay"
    );

    game.handle_event(&InputEvent::KeyDown(keys::F8));
    run_tics(&mut game, &TicCmdInput::NONE, 1, &mut fb);

    // the fixture's STCFN dummy glyphs are filled with 0x42, so the band
    // region holds exactly two values: band black or glyph fill — never
    // the wall-gradient indices from the view
    let mut found_black = false;
    for y in 2..10usize {
        for &px in fb.row(y).expect("row in range").iter().take(28) {
            assert!(px == 0 || px == 0x42, "unexpected pixel {px} in row {y}");
            found_black |= px == 0;
        }
    }
    assert!(found_black, "expected some solid black band pixels");
}
