//! Regression test for the status bar staying visible across frames.
//!
//! The view rasterizes all 200 rows every frame (this port keeps the
//! full-screen view), so in the real game the floor/wall pass paints
//! rows ST_Y..200 before the bar gets a chance to redraw. Vanilla never
//! sees this because its view is capped at 168 rows and the bar
//! re-establishes itself on the same rows. These tests emulate the view
//! clobbering the bar rows and require the bar to be fully restored by
//! the overlay pass that follows in the very same frame.
//!
//! The synthetic IWAD fills every status-bar patch with 0x42, so the
//! bottom 32 rows (ST_Y..200) must be uniformly 0x42 whenever the bar is
//! actually drawn.

mod common;

use common::{FixtureSpec, MapScenario, TempDir, new_game_args, run_tics, start_game};
use doom_engine::game::TicCmdInput;
use doom_engine::render::{IndexedFramebuffer, SCREEN_HEIGHT};

const STBAR_FILL: u8 = 0x42;
const FLOOR_FILL: u8 = 0x10; // FLOOR4_8 fixture flat: what the view would paint there

fn assert_bar_visible(fb: &IndexedFramebuffer, stage: &str) {
    for y in 168..SCREEN_HEIGHT {
        for (x, &pixel) in fb.row(y).expect("row in range").iter().enumerate() {
            assert_eq!(
                pixel, STBAR_FILL,
                "bar pixel ({x}, {y}) is not the bar background at stage: {stage}"
            );
        }
    }
}

/// What the live view does to rows ST_Y..200 every frame.
fn clobber_bar_rows(fb: &mut IndexedFramebuffer) {
    for y in 168..SCREEN_HEIGHT {
        if let Some(row) = fb.row_mut(y) {
            row.fill(FLOOR_FILL);
        }
    }
}

#[test]
fn status_bar_survives_a_view_pass_over_the_bar_rows() {
    let dir = TempDir::new("bar-visible");
    let spec = FixtureSpec {
        scenario: MapScenario::Plain,
        monster: false,
    };
    let mut game = start_game(dir.path(), &new_game_args(), &spec);
    let mut fb = IndexedFramebuffer::new();

    // settle: entering the level and the opening melt run for these tics
    run_tics(&mut game, &TicCmdInput::NONE, 150, &mut fb);
    assert_bar_visible(&fb, "after level load");

    // the view paints over the bar rows, as it does every live frame;
    // one more tic (with no gameplay change at all) must restore them
    clobber_bar_rows(&mut fb);
    run_tics(&mut game, &TicCmdInput::NONE, 1, &mut fb);
    assert_bar_visible(&fb, "after the view pass");

    // same deal during combat: widgets redraw, but so does the background
    let fire = TicCmdInput {
        attack: true,
        ..TicCmdInput::NONE
    };
    clobber_bar_rows(&mut fb);
    run_tics(&mut game, &fire, 30, &mut fb);
    assert_bar_visible(&fb, "during combat");

    clobber_bar_rows(&mut fb);
    run_tics(&mut game, &TicCmdInput::NONE, 1, &mut fb);
    assert_bar_visible(&fb, "after combat");
}

#[test]
fn status_bar_stays_visible_across_quiet_frames() {
    let dir = TempDir::new("bar-stable");
    let spec = FixtureSpec {
        scenario: MapScenario::Plain,
        monster: false,
    };
    let mut game = start_game(dir.path(), &new_game_args(), &spec);
    let mut fb = IndexedFramebuffer::new();

    run_tics(&mut game, &TicCmdInput::NONE, 150, &mut fb);
    assert_bar_visible(&fb, "after level load");

    // quiet frames: nothing in the world changes, the bar must persist
    for _ in 0..3 {
        run_tics(&mut game, &TicCmdInput::NONE, 30, &mut fb);
        assert_bar_visible(&fb, "quiet frames");
    }

    // firing: ammo/face/weapon state changes, the bar must persist
    let fire = TicCmdInput {
        attack: true,
        ..TicCmdInput::NONE
    };
    run_tics(&mut game, &fire, 30, &mut fb);
    assert_bar_visible(&fb, "firing");
}
