//! Integration tests for the game loop, driven against a synthetic
//! WAD built in-memory. No window, no audio hardware, no network -
//! exactly how a CI runner sees the engine.
//!
//! The fixture WAD contains two identical maps (`E1M1` and `E1M2`) with
//! a single sector room. `E1M1` additionally carries a line with the
//! exit-level special (`11`) in front of the player start, which the
//! exit-flow test activates with the `use` button.

use crate::args::EngineArgs;
use crate::fixed::Fixed;
use crate::game::{Game, GameConfig, GameState, InputEvent, TicCmdInput, keys};
use crate::menu::GameMode;
use crate::render::IndexedFramebuffer;
use crate::wad::WadFile;
use crate::wad::testutil::build_wad;

// ---------------------------------------------------------------------------
// fixture WAD construction
// ---------------------------------------------------------------------------

/// A valid patch filled with index-0x42 pixels.
fn dummy_patch(width: i16, height: i16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&0i16.to_le_bytes()); // left offset
    out.extend_from_slice(&0i16.to_le_bytes()); // top offset

    let column_len = 3 + usize::try_from(height).expect("positive height") + 1 + 1;
    let column_start = 8 + usize::try_from(width).expect("positive width") * 4;
    for i in 0..usize::try_from(width).expect("positive width") {
        out.extend_from_slice(&((column_start + i * column_len) as i32).to_le_bytes());
    }
    for _ in 0..width {
        out.push(0x00); // topdelta: start at row 0
        out.push(height as u8); // length
        out.push(0); // pad
        out.extend(std::iter::repeat_n(0x42u8, height as usize));
        out.push(0); // pad
        out.push(0xff); // terminator
    }
    out
}

fn flat_lump() -> Vec<u8> {
    vec![0x10; 4096]
}

fn dmx_sound() -> Vec<u8> {
    // tag=3, rate=11025, length=8 samples; unpadded layout
    let mut out = Vec::new();
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&11025u16.to_le_bytes());
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]);
    out
}

/// A 64x64 patch of index-100 pixels - the WALL1 texture source.
fn wall1_patch() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&64i16.to_le_bytes()); // width
    out.extend_from_slice(&64i16.to_le_bytes()); // height
    out.extend_from_slice(&0i16.to_le_bytes()); // left offset
    out.extend_from_slice(&0i16.to_le_bytes()); // top offset

    let column_body = |color: u8| -> Vec<u8> {
        let mut out = Vec::new();
        out.push(0x00); // topdelta: start at row 0
        out.push(64); // length: all 64 rows
        out.push(0); // pad
        out.extend(std::iter::repeat_n(color, 64));
        out.push(0); // pad
        out.push(0xff); // terminator
        out
    };
    let column_len = column_body(0).len();
    let column_start = 8 + 64 * 4;
    for i in 0..64usize {
        out.extend_from_slice(&((column_start + i * column_len) as i32).to_le_bytes());
    }
    for i in 0..64usize {
        out.extend_from_slice(&column_body(i as u8 + 90));
    }
    out
}

fn colormap_lump() -> Vec<u8> {
    // 34 diminishing colormaps + the fuzz map + the invulnerability map;
    // every ramp is the identity, which is all the smoke tests need
    let mut out = Vec::new();
    for _ in 0..34 {
        out.extend_from_slice(&(0u8..=255).collect::<Vec<_>>());
    }
    out
}

fn pnames_lump() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1i32.to_le_bytes());
    let mut name = [0u8; 8];
    name[..5].copy_from_slice(b"WALL1");
    out.extend_from_slice(&name);
    out
}

fn texture1_lump() -> Vec<u8> {
    // one texture: WALL1, 64x64, one patch at origin 0,0
    let mut out = Vec::new();
    out.extend_from_slice(&1i32.to_le_bytes()); // num_textures
    out.extend_from_slice(&8i32.to_le_bytes()); // offset of the definition
    // texture def
    let mut name = [0u8; 8];
    name[..5].copy_from_slice(b"WALL1");
    out.extend_from_slice(&name);
    out.extend_from_slice(&0i32.to_le_bytes()); // mask
    out.extend_from_slice(&64i16.to_le_bytes()); // width
    out.extend_from_slice(&64i16.to_le_bytes()); // height
    out.extend_from_slice(&0i32.to_le_bytes()); // obsolete columndirectory
    out.extend_from_slice(&1i16.to_le_bytes()); // patchcount
    out.extend_from_slice(&0i16.to_le_bytes()); // origin x
    out.extend_from_slice(&0i16.to_le_bytes()); // origin y
    out.extend_from_slice(&0i16.to_le_bytes()); // patch index
    out.extend_from_slice(&1i16.to_le_bytes()); // stepdir
    out.extend_from_slice(&0i16.to_le_bytes()); // colormap
    out
}

fn blockmap_lump(num_lines: u16) -> Vec<u8> {
    // 3x3 grid covering the 256x256 room, every cell lists all lines
    let mut out = Vec::new();
    out.extend_from_slice(&0i16.to_le_bytes()); // origin x
    out.extend_from_slice(&0i16.to_le_bytes()); // origin y
    out.extend_from_slice(&3i16.to_le_bytes()); // columns
    out.extend_from_slice(&3i16.to_le_bytes()); // rows
    let cell_shorts = 2 + usize::from(num_lines); // lead word + indices + terminator
    let header_shorts = 4 + 9; // header + offset table
    let mut offsets = Vec::with_capacity(9);
    for cell in 0..9 {
        offsets.push((header_shorts + cell * cell_shorts) as u16);
    }
    for offset in offsets {
        out.extend_from_slice(&(offset as i16).to_le_bytes());
    }
    for _ in 0..9 {
        out.extend_from_slice(&0x0000i16.to_le_bytes()); // lead word
        for line in 0..num_lines {
            out.extend_from_slice(&line.to_le_bytes());
        }
        out.extend_from_slice(&0xffffu16.to_le_bytes()); // terminator
    }
    out
}

fn vertexes_lump(exit_line: bool) -> Vec<u8> {
    let verts: &[(i16, i16)] = &[
        (0, 0),
        (256, 0),
        (256, 256),
        (0, 256),
        (160, 64),
        (160, 192),
    ];
    let mut out = Vec::new();
    let count = if exit_line { 6 } else { 4 };
    for (x, y) in verts.iter().take(count) {
        out.extend_from_slice(&x.to_le_bytes());
        out.extend_from_slice(&y.to_le_bytes());
    }
    out
}

fn linedefs_lump(exit_line: bool) -> Vec<u8> {
    // 0 south, 1 east, 2 north, 3 west + optional exit line
    let mut lines: Vec<(u16, u16, i16, i16, i16, i16)> = vec![
        (1, 0, 1, 0, 0, 0), // south wall, sidedef 0
        (2, 1, 1, 0, 0, 1), // east wall, sidedef 1
        (3, 2, 1, 0, 0, 2), // north wall, sidedef 2
        (0, 3, 1, 0, 0, 3), // west wall, sidedef 3
    ];
    if exit_line {
        // front side faces west, toward the player start
        lines.push((5, 4, 1, 11, 0, 4)); // exit line, special 11, sidedef 4
    }
    let mut out = Vec::new();
    for (v1, v2, flags, special, tag, sidenum) in lines {
        out.extend_from_slice(&(v1 as i16).to_le_bytes());
        out.extend_from_slice(&(v2 as i16).to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&special.to_le_bytes());
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&sidenum.to_le_bytes());
        out.extend_from_slice(&(-1i16).to_le_bytes()); // no back side
    }
    out
}

fn sidedefs_lump(exit_line: bool) -> Vec<u8> {
    let count = if exit_line { 5 } else { 4 };
    let mut out = Vec::new();
    for _ in 0..count {
        out.extend_from_slice(&0i16.to_le_bytes()); // texture offset
        out.extend_from_slice(&0i16.to_le_bytes()); // row offset
        out.extend_from_slice(b"-\0\0\0\0\0\0\0"); // top: none
        out.extend_from_slice(b"-\0\0\0\0\0\0\0"); // bottom: none
        out.extend_from_slice(b"WALL1\0\0\0"); // mid: WALL1
        out.extend_from_slice(&0i16.to_le_bytes()); // sector
    }
    out
}

fn sectors_lump() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0i16.to_le_bytes()); // floor height
    out.extend_from_slice(&128i16.to_le_bytes()); // ceiling height
    let mut floor = [0u8; 8];
    floor[..8].copy_from_slice(b"FLOOR4_8");
    out.extend_from_slice(&floor);
    let mut ceil = [0u8; 8];
    ceil[..7].copy_from_slice(b"CEIL3_5");
    out.extend_from_slice(&ceil);
    out.extend_from_slice(&176i16.to_le_bytes()); // light level
    out.extend_from_slice(&0i16.to_le_bytes()); // special
    out.extend_from_slice(&0i16.to_le_bytes()); // tag
    out
}

fn segs_lump(exit_line: bool) -> Vec<u8> {
    // v1, v2, angle, linedef, side, offset
    let mut segs: Vec<(u16, u16, i16, u16, u16, i16)> = vec![
        (0, 1, 0, 0, 0, 0), // south wall seg
        (1, 2, 0, 1, 0, 0), // east wall seg
        (2, 3, 0, 2, 0, 0), // north wall seg
        (3, 0, 0, 3, 0, 0), // west wall seg
    ];
    if exit_line {
        segs.push((4, 5, 0, 4, 0, 0)); // exit line seg
    }
    let mut out = Vec::new();
    for (v1, v2, angle, linedef, side, offset) in segs {
        out.extend_from_slice(&(v1 as i16).to_le_bytes());
        out.extend_from_slice(&(v2 as i16).to_le_bytes());
        out.extend_from_slice(&angle.to_le_bytes());
        out.extend_from_slice(&(linedef as i16).to_le_bytes());
        out.extend_from_slice(&(side as i16).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
    }
    out
}

fn subsectors_lump(exit_line: bool) -> Vec<u8> {
    let num_segs = if exit_line { 5u16 } else { 4u16 };
    let mut out = Vec::new();
    out.extend_from_slice(&num_segs.to_le_bytes()); // num segs
    out.extend_from_slice(&0i16.to_le_bytes()); // first seg
    out
}

fn nodes_lump() -> Vec<u8> {
    Vec::new()
}

fn reject_lump() -> Vec<u8> {
    vec![0x00]
}

fn things_lump(monster: bool) -> Vec<u8> {
    let mut things: Vec<(i16, i16, i16, i16, i16)> = vec![
        (128, 128, 0, 1, 7), // player 1, angle 0 (east), skill bits
    ];
    if monster {
        things.push((100, 100, 0, 3004, 7)); // MT_POSSESSED
    }
    let mut out = Vec::new();
    for (x, y, angle, thing_type, flags) in things {
        out.extend_from_slice(&x.to_le_bytes());
        out.extend_from_slice(&y.to_le_bytes());
        out.extend_from_slice(&angle.to_le_bytes());
        out.extend_from_slice(&thing_type.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
    }
    out
}

fn map_lumps(marker: &str, exit_line: bool, monster: bool) -> Vec<(String, Vec<u8>)> {
    let num_lines: u16 = if exit_line { 5 } else { 4 };
    vec![
        (marker.to_string(), Vec::new()),
        ("THINGS".to_string(), things_lump(monster)),
        ("LINEDEFS".to_string(), linedefs_lump(exit_line)),
        ("SIDEDEFS".to_string(), sidedefs_lump(exit_line)),
        ("VERTEXES".to_string(), vertexes_lump(exit_line)),
        ("SEGS".to_string(), segs_lump(exit_line)),
        ("SSECTORS".to_string(), subsectors_lump(exit_line)),
        ("NODES".to_string(), nodes_lump()),
        ("SECTORS".to_string(), sectors_lump()),
        ("REJECT".to_string(), reject_lump()),
        ("BLOCKMAP".to_string(), blockmap_lump(num_lines)),
    ]
}

fn game_wad(monster: bool) -> Vec<u8> {
    // resources the engine needs at startup
    let mut lumps: Vec<(String, Vec<u8>)> = vec![
        ("COLORMAP".to_string(), colormap_lump()),
        ("PLAYPAL".to_string(), vec![0u8; 768 * 14]),
        ("PNAMES".to_string(), pnames_lump()),
        ("TEXTURE1".to_string(), texture1_lump()),
    ];
    lumps.push(("DSPISTOL".to_string(), dmx_sound()));
    lumps.push(("WALL1".to_string(), wall1_patch()));

    // flats namespace
    lumps.push(("F_START".to_string(), Vec::new()));
    lumps.push(("F_SKY1".to_string(), flat_lump()));
    lumps.push(("FLOOR4_8".to_string(), flat_lump()));
    lumps.push(("CEIL3_5".to_string(), flat_lump()));
    lumps.push(("F_END".to_string(), Vec::new()));

    // empty sprites namespace: empty definitions are fine
    lumps.push(("S_START".to_string(), Vec::new()));
    lumps.push(("S_END".to_string(), Vec::new()));

    // HUD font (STCFN033..=STCFN095)
    for i in 33..=95u8 {
        lumps.push((format!("STCFN{:03}", i), dummy_patch(4, 6)));
    }

    // status bar patches
    lumps.push(("STBAR".to_string(), dummy_patch(320, 32)));
    lumps.push(("STARMS".to_string(), dummy_patch(104, 32)));
    for i in 0..4 {
        lumps.push((format!("STFB{i}"), dummy_patch(24, 32)));
    }
    for i in 0..10 {
        lumps.push((format!("STTNUM{i}"), dummy_patch(16, 8)));
        lumps.push((format!("STYSNUM{i}"), dummy_patch(16, 8)));
    }
    lumps.push(("STTPRCNT".to_string(), dummy_patch(8, 8)));
    lumps.push(("STTMINUS".to_string(), dummy_patch(8, 8)));
    for i in 0..6 {
        lumps.push((format!("STKEYS{i}"), dummy_patch(8, 5)));
    }
    for i in 2..=7 {
        lumps.push((format!("STGNUM{i}"), dummy_patch(8, 8)));
    }
    for pain in 0..5 {
        for straight in 0..3 {
            lumps.push((format!("STFST{pain}{straight}"), dummy_patch(24, 32)));
        }
        lumps.push((format!("STFTR{pain}0"), dummy_patch(24, 32)));
        lumps.push((format!("STFTL{pain}0"), dummy_patch(24, 32)));
        lumps.push((format!("STFOUCH{pain}"), dummy_patch(24, 32)));
        lumps.push((format!("STFEVL{pain}"), dummy_patch(24, 32)));
        lumps.push((format!("STFKILL{pain}"), dummy_patch(24, 32)));
    }
    lumps.push(("STFGOD0".to_string(), dummy_patch(24, 32)));
    lumps.push(("STFDEAD0".to_string(), dummy_patch(24, 32)));

    // menu patches
    for name in [
        "M_DOOM", "M_NEWG", "M_SKILL", "M_EPISOD", "M_OPTTTL", "M_NGAME", "M_OPTION", "M_LOADG",
        "M_SAVEG", "M_RDTHIS", "M_QUITG", "M_EPI1", "M_EPI2", "M_EPI3", "M_EPI4", "M_JKILL",
        "M_ROUGH", "M_HURT", "M_ULTRA", "M_NMARE", "M_ENDGAM", "M_MESSG", "M_DETAIL", "M_SCRNSZ",
        "M_MSENS", "M_SVOL", "M_SFXVOL", "M_MUSVOL", "M_GDHIGH", "M_GDLOW", "M_MSGOFF", "M_MSGON",
        "HELP", "HELP1", "HELP2", "CREDIT", "M_LSLEFT", "M_LSCNTR", "M_LSRGHT", "M_THERML",
        "M_THERMM", "M_THERMR", "M_THERMO", "M_SKULL1", "M_SKULL2",
    ] {
        lumps.push((name.to_string(), dummy_patch(32, 16)));
    }

    // title screen for GS_DEMOSCREEN
    lumps.push(("TITLEPIC".to_string(), dummy_patch(320, 200)));
    lumps.push(("M_PAUSE".to_string(), dummy_patch(80, 16)));

    // two maps: E1M1 (with exit line) and E1M2 (plain)
    lumps.extend(map_lumps("E1M1", true, monster));
    lumps.extend(map_lumps("E1M2", false, monster));

    build_wad(
        b"IWAD",
        &lumps
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_slice()))
            .collect::<Vec<_>>(),
    )
}

fn unique_test_dir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "doom-rs-test-{}-{nanos}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

fn test_config(save_dir: &std::path::Path) -> GameConfig {
    GameConfig {
        game_mode: GameMode::Registered,
        save_dir: save_dir.to_path_buf(),
        ..GameConfig::default()
    }
}

fn default_args() -> EngineArgs {
    EngineArgs::default()
}

fn walk_forward() -> TicCmdInput {
    TicCmdInput {
        forward: true,
        ..TicCmdInput::NONE
    }
}

fn load_game(save_dir: &std::path::Path, args: &EngineArgs) -> Game {
    load_game_with_monster(save_dir, args, false)
}

fn load_game_with_monster(save_dir: &std::path::Path, args: &EngineArgs, monster: bool) -> Game {
    std::fs::create_dir_all(save_dir).expect("save dir creatable");
    let wad = WadFile::from_bytes(game_wad(monster)).expect("synthetic IWAD parses");
    let resources = crate::resources::ResourceManager::new(wad, Vec::new());
    Game::new(test_config(save_dir), resources, args).expect("game builds from the synthetic WAD")
}

fn run_tics(game: &mut Game, input: &TicCmdInput, tics: usize, fb: &mut IndexedFramebuffer) {
    for _ in 0..tics {
        game.tick_input(input, fb).expect("tic runs");
    }
}

// ---------------------------------------------------------------------------
// the tests
// ---------------------------------------------------------------------------

fn new_game_args() -> EngineArgs {
    EngineArgs {
        skill: Some(1),
        ..Default::default()
    }
}

#[test]
fn title_screen_starts_without_autostart() {
    let dir = unique_test_dir();
    let game = load_game(&dir, &default_args());
    assert_eq!(game.gamestate(), GameState::DemoScreen);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn new_game_loads_level_after_one_tic() {
    let dir = unique_test_dir();
    let mut game = load_game(&dir, &new_game_args());
    let mut fb = IndexedFramebuffer::new();
    game.tick_input(&TicCmdInput::NONE, &mut fb)
        .expect("tic runs");
    assert_eq!(game.gamestate(), GameState::Level);
    let position = game.player_position().expect("player spawned");
    assert_eq!(position.0, Fixed::from_int(128));
    assert_eq!(position.1, Fixed::from_int(128));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deterministic_player_movement_across_two_runs() {
    let dir = unique_test_dir();
    let mut fb = IndexedFramebuffer::new();

    let mut game_a = load_game(&dir, &new_game_args());
    run_tics(&mut game_a, &TicCmdInput::NONE, 1, &mut fb);
    run_tics(&mut game_a, &walk_forward(), 35, &mut fb);

    let mut game_b = load_game(&dir, &new_game_args());
    run_tics(&mut game_b, &TicCmdInput::NONE, 1, &mut fb);
    run_tics(&mut game_b, &walk_forward(), 35, &mut fb);

    let position_a = game_a.player_position().expect("a in game");
    let position_b = game_b.player_position().expect("b in game");
    assert_eq!(position_a, position_b);
    assert!(position_a.0 > Fixed::from_int(128), "player moved east");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn angle_turn_rotates_the_view() {
    let dir = unique_test_dir();
    let mut game = load_game(&dir, &new_game_args());
    let mut fb = IndexedFramebuffer::new();
    game.tick_input(&TicCmdInput::NONE, &mut fb)
        .expect("tic runs");
    let before = game.player_position().expect("spawned");
    let turn_right = TicCmdInput {
        turn_right: true,
        ..TicCmdInput::NONE
    };
    run_tics(&mut game, &turn_right, 20, &mut fb);
    let after = game.player_position().expect("still in game");
    assert_ne!(before.2, after.2, "angle changed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn use_button_activates_the_exit_line_and_loads_the_next_map() {
    let dir = unique_test_dir();
    let mut game = load_game(&dir, &new_game_args());
    let mut fb = IndexedFramebuffer::new();

    // load the level
    game.tick_input(&TicCmdInput::NONE, &mut fb)
        .expect("tic runs");
    assert_eq!(game.gamestate(), GameState::Level);

    // press use against the exit line and let the action loop run
    let use_button = TicCmdInput {
        use_button: true,
        ..TicCmdInput::NONE
    };
    game.tick_input(&use_button, &mut fb).expect("tic runs");
    game.tick_input(&TicCmdInput::NONE, &mut fb)
        .expect("tic runs");
    game.tick_input(&TicCmdInput::NONE, &mut fb)
        .expect("tic runs");

    assert_eq!(game.gamestate(), GameState::Level);
    assert_eq!(game.gamemap, 2, "exit line carried the player to E1M2");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn demo_record_and_playback_roundtrips_through_the_same_tics() {
    let dir = unique_test_dir();
    let mut fb = IndexedFramebuffer::new();

    // record a demo
    let record_args = EngineArgs {
        record: Some("demo".to_string()),
        ..new_game_args()
    };
    let mut game_a = load_game(&dir, &record_args);
    game_a
        .tick_input(&TicCmdInput::NONE, &mut fb)
        .expect("tic runs");
    run_tics(&mut game_a, &walk_forward(), 10, &mut fb);
    game_a.stop_demo_recording();
    let end_position = game_a.player_position().expect("recorded run ends in game");

    // the recorded demo exists and parses
    let demo_path = dir.join("demo.lmp");
    let bytes = std::fs::read(&demo_path).expect("demo file written");
    let mut playback = crate::game::DemoPlayback::new(bytes).expect("demo parses");
    assert_eq!(playback.header().episode, 1);
    assert_eq!(playback.header().map, 1);
    let mut tics = 0;
    while !playback.finished() {
        playback.read_ticcmd();
        tics += 1;
    }
    assert_eq!(
        tics, 12,
        "eleven recorded tics plus the marker read that flips finished"
    );

    // replay it: the same tics produce the same end position
    let play_args = EngineArgs {
        play_demo: Some(demo_path.to_string_lossy().to_string()),
        ..Default::default()
    };
    let mut game_b = load_game(&dir, &play_args);
    game_b
        .tick_input(&TicCmdInput::NONE, &mut fb)
        .expect("tic runs");
    run_tics(&mut game_b, &TicCmdInput::NONE, 10, &mut fb);
    let replayed = game_b.player_position().expect("playback run ends in game");
    assert_eq!(replayed, end_position, "demo replays deterministically");
    // let the marker get consumed: single-demo mode asks the shell to quit
    run_tics(&mut game_b, &TicCmdInput::NONE, 2, &mut fb);
    assert!(
        game_b.should_quit(),
        "single-demo mode quits after playback"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn save_and_load_restores_the_player_state() {
    let dir = unique_test_dir();
    let mut fb = IndexedFramebuffer::new();

    let mut game = load_game(&dir, &new_game_args());
    game.tick_input(&TicCmdInput::NONE, &mut fb)
        .expect("tic runs");
    run_tics(&mut game, &walk_forward(), 10, &mut fb);

    game.save_game_slot(0, "roundtrip");
    let saved_position = game.player_position().expect("saved");
    let saved_health = game.players[0].health;

    // move on and wound the player so the restore has something to prove
    // (backward so the player does not pin itself against the exit line)
    let walk_backward = TicCmdInput {
        backward: true,
        ..TicCmdInput::NONE
    };
    run_tics(&mut game, &walk_backward, 15, &mut fb);
    game.damage_mobj(game.players[0].mo.expect("avatar"), None, None, 10);
    let moved_position = game.player_position().expect("moved");
    assert_ne!(saved_position, moved_position);

    game.load_game_slot(0);
    let restored = game.player_position().expect("restored");
    assert_eq!(restored, saved_position, "position restored from save");
    assert_eq!(
        game.players[0].health, saved_health,
        "health restored from save"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn menu_opens_and_closes_on_escape() {
    let dir = unique_test_dir();
    let mut game = load_game(&dir, &new_game_args());
    assert!(!game.menu.is_active());
    assert!(game.handle_event(&InputEvent::KeyDown(keys::ESCAPE)));
    assert!(game.menu.is_active());
    assert!(game.handle_event(&InputEvent::KeyDown(keys::ESCAPE)));
    assert!(!game.menu.is_active());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_world_runs_for_several_seconds_without_incidents() {
    // a smoke run: the monster AI, the ticker, the renderer and the
    // event machinery all get exercised together
    let dir = unique_test_dir();
    let mut game = load_game_with_monster(&dir, &new_game_args(), true);
    let mut fb = IndexedFramebuffer::new();
    game.tick_input(&TicCmdInput::NONE, &mut fb)
        .expect("tic runs");
    for _ in 0..(35 * 8) {
        game.tick_input(&walk_forward(), &mut fb).expect("tic runs");
    }
    let position = game.player_position().expect("player still in the game");
    assert!(position.0 > Fixed::from_int(128));
    let _ = std::fs::remove_dir_all(&dir);
}
