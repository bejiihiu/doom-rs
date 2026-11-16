//! Integration tests for the `doom-demo-analyzer` binary.
//!
//! Builds synthetic `.lmp` demos in a temp directory (nothing binary is
//! committed) and runs the real binary against them.

use std::path::{Path, PathBuf};
use std::process::Command;

const VERSION_110: u8 = 110;
const VERSION_109: u8 = 109;
const DEMO_MARKER: u8 = 0x80;

/// Build a demo image: header, tic bytes (forward, side as raw bytes, angle
/// byte, buttons byte), optional DEMOMARKER.
#[allow(clippy::too_many_arguments)]
fn build_demo(
    version: u8,
    skill: u8,
    episode: u8,
    map: u8,
    deathmatch: u8,
    respawn: u8,
    fast: u8,
    nomonsters: u8,
    console_player: u8,
    playeringame: [u8; 4],
    tics: &[(i8, i8, u8, u8)],
    marker: bool,
) -> Vec<u8> {
    let mut blob = vec![
        version,
        skill,
        episode,
        map,
        deathmatch,
        respawn,
        fast,
        nomonsters,
        console_player,
    ];
    blob.extend_from_slice(&playeringame);
    for (forward, side, angle, buttons) in tics {
        blob.push(*forward as u8);
        blob.push(*side as u8);
        blob.push(*angle);
        blob.push(*buttons);
    }
    if marker {
        blob.push(DEMO_MARKER);
    }
    blob
}

/// A sane default demo: version 110, skill 2, E1M1, duel off, one player,
/// one neutral tic.
fn default_demo() -> Vec<u8> {
    build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &[(0, 0, 0, 0)],
        true,
    )
}

struct TempDemo {
    path: PathBuf,
}

impl TempDemo {
    fn new(label: &str, bytes: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!(
            "doom-demo-analyzer-test-{}-{label}-{}.lmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, bytes).expect("write temp demo");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDemo {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_doom-demo-analyzer"))
        .args(args)
        .output()
        .expect("run doom-demo-analyzer")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn file_arg(demo: &TempDemo) -> String {
    demo.path().to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// info
// ---------------------------------------------------------------------------

#[test]
fn info_reports_the_header() {
    let bytes = build_demo(
        VERSION_110,
        2,
        1,
        3,
        1,
        0,
        1,
        0,
        0,
        [1, 0, 0, 0],
        &[(25, 0, 0, 0)],
        true,
    );
    let demo = TempDemo::new("info", &bytes);
    let output = run(&["info", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(0));
    let out = stdout(&output);
    assert!(out.contains("Version (magic): 110"), "{out}");
    assert!(out.contains("Skill: 2 (Hurt me plenty)"), "{out}");
    assert!(out.contains("Episode: 1"), "{out}");
    assert!(out.contains("Map: 3"), "{out}");
    assert!(out.contains("E1M3 (episode)"), "{out}");
    assert!(
        out.contains("Commercial reading") || out.contains("MAP03"),
        "{out}"
    );
    assert!(out.contains("Deathmatch: duel (1)"), "{out}");
    assert!(out.contains("Fast monsters: yes"), "{out}");
    assert!(out.contains("Respawn monsters: no"), "{out}");
    assert!(out.contains("No monsters: no"), "{out}");
    assert!(out.contains("Console player: 0"), "{out}");
    assert!(out.contains("[1,0,0,0]"), "{out}");
    assert!(out.contains("1 in game"), "{out}");
    assert!(out.contains("Tics: 1"), "{out}");
}

#[test]
fn info_reports_commercial_map_reading() {
    let bytes = build_demo(
        VERSION_110,
        3,
        1,
        25,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &[],
        true,
    );
    let demo = TempDemo::new("info-commercial", &bytes);
    let output = run(&["info", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(0));
    let out = stdout(&output);
    assert!(out.contains("MAP25 (commercial)"), "{out}");
    assert!(out.contains("Skill: 3 (Ultra-Violence)"), "{out}");
}

#[test]
fn info_reports_multiplayer_flags() {
    let bytes = build_demo(VERSION_110, 2, 2, 4, 0, 0, 0, 0, 1, [1, 1, 0, 1], &[], true);
    let demo = TempDemo::new("info-players", &bytes);
    let output = run(&["info", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(0));
    let out = stdout(&output);
    assert!(out.contains("[1,1,0,1]"), "{out}");
    assert!(out.contains("3 in game"), "{out}");
    assert!(out.contains("player 1, player 2, player 4"), "{out}");
}

#[test]
fn info_unknown_version_still_prints_header() {
    let mut bytes = default_demo();
    bytes[0] = 42;
    let demo = TempDemo::new("info-version", &bytes);
    let output = run(&["info", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).contains("Version (magic): 42"));
}

#[test]
fn info_missing_file_is_an_error() {
    let output = run(&["info", "C:\\definitely\\not\\a\\real-file.lmp"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("error:"));
}

// ---------------------------------------------------------------------------
// stats
// ---------------------------------------------------------------------------

#[test]
fn stats_counts_running_and_attacks() {
    let tics = [
        (50, 0, 0, 1),  // forward + attack
        (50, 0, 0, 0),  // forward
        (-25, 0, 0, 1), // backward + attack
        (0, 0, 0, 0),   // idle
    ];
    let bytes = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &tics,
        true,
    );
    let demo = TempDemo::new("stats", &bytes);
    let output = run(&["stats", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(0));
    let out = stdout(&output);
    assert!(out.contains("Tics: 4 (0.1 s"), "{out}");
    assert!(
        out.contains("RUNNING (forward > 0): 2 tics (50.0%)"),
        "{out}"
    );
    assert!(out.contains("ATTACK: 2 tics (50.0%)"), "{out}");
    assert!(out.contains("USE: 0 tics"), "{out}");
    assert!(out.contains("forward mean: +18.8"), "{out}");
    assert!(out.contains("longest attack run: 1 tics"), "{out}");
    assert!(out.contains("longest idle run: 1 tics"), "{out}");
}

#[test]
fn stats_detects_an_attack_burst() {
    // 35 tics of solid firing - one full window, far above the average
    let tics: Vec<(i8, i8, u8, u8)> = (0..35).map(|_| (0, 0, 0, 1)).collect();
    let bytes = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &tics,
        true,
    );
    let demo = TempDemo::new("stats-burst", &bytes);
    let output = run(&["stats", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(0));
    let out = stdout(&output);
    assert!(
        out.contains("busiest 35-tic window: tics 0..34 with 35 attack(s)"),
        "{out}"
    );
    assert!(out.contains("unusual bursts"), "{out}");
    assert!(out.contains("longest attack run: 35 tics"), "{out}");
}

#[test]
fn stats_reports_turn_statistics() {
    // angle bytes 0x40 (+90 deg) and 0xC0 (-90 deg)
    let tics = [(0, 0, 0x40, 0), (0, 0, 0xC0, 0), (0, 0, 0x40, 0)];
    let bytes = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &tics,
        true,
    );
    let demo = TempDemo::new("stats-turn", &bytes);
    let output = run(&["stats", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(0));
    let out = stdout(&output);
    assert!(out.contains("nonzero turns: 3 tics (100.0%)"), "{out}");
    assert!(
        out.contains("angle_turn BAM min/max: -16384 / 16384"),
        "{out}"
    );
    assert!(
        out.contains("total absolute rotation: 270.0 degrees"),
        "{out}"
    );
}

#[test]
fn stats_weapon_change_reports_weapon_names() {
    // weapon change to chaingun (weapon 3): BT_CHANGE | (3 << 3)
    let chaingun = 4 | (3 << 3);
    let tics = [(0, 0, 0, chaingun)];
    let bytes = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &tics,
        true,
    );
    let demo = TempDemo::new("stats-weapon", &bytes);
    let output = run(&["stats", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(0));
    let out = stdout(&output);
    assert!(out.contains("WEAPON CHANGE: 1 tics"), "{out}");
    assert!(out.contains("chaingun (3)x1"), "{out}");
}

// ---------------------------------------------------------------------------
// dump-tics
// ---------------------------------------------------------------------------

#[test]
fn dump_tics_plain_output() {
    let tics = [(50, -5, 0x40, 1), (25, 0, 0x00, 0)];
    let bytes = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &tics,
        true,
    );
    let demo = TempDemo::new("dump", &bytes);
    let output = run(&["dump-tics", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(0));
    let out = stdout(&output);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "# tic forward_move side_move buttons angle_turn");
    assert_eq!(lines[1], "0 50 -5 1 16384");
    assert_eq!(lines[2], "1 25 0 0 0");
    assert_eq!(lines.len(), 3);
}

#[test]
fn dump_tics_limit() {
    let tics: Vec<(i8, i8, u8, u8)> = (0..10).map(|i| (i, 0, 0, 0)).collect();
    let bytes = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &tics,
        true,
    );
    let demo = TempDemo::new("dump-limit", &bytes);
    let output = run(&["dump-tics", &file_arg(&demo), "--limit", "3"]);
    assert_eq!(output.status.code(), Some(0));
    let out = stdout(&output);
    assert_eq!(out.lines().count(), 4); // header + 3 tics
    assert!(out.lines().nth(3).unwrap().starts_with('2'));
}

#[test]
fn dump_tics_jsonl_is_valid_json() {
    let tics = [(50, -5, 0x40, 1)];
    let bytes = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &tics,
        true,
    );
    let demo = TempDemo::new("dump-jsonl", &bytes);
    let output = run(&["dump-tics", &file_arg(&demo), "--format", "jsonl"]);
    assert_eq!(output.status.code(), Some(0));
    let out = stdout(&output);
    let line = out.lines().next().expect("one line");
    let value: serde_json::Value = serde_json::from_str(line).expect("valid json");
    assert_eq!(value["tic"], 0);
    assert_eq!(value["forward_move"], 50);
    assert_eq!(value["side_move"], -5);
    assert_eq!(value["buttons"], 1);
    assert_eq!(value["angle_turn"], 16384);
    assert_eq!(value["angle_turn_byte"], 64);
    assert_eq!(value["attack"], true);
    assert_eq!(value["use"], false);
    assert_eq!(value["weapon_change"], false);
    assert_eq!(value["weapon"], serde_json::Value::Null);
}

#[test]
fn dump_tics_jsonl_decodes_weapon_change() {
    // weapon change to plasma rifle (weapon 5)
    let tics = [(0, 0, 0, 4 | (5 << 3))];
    let bytes = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &tics,
        true,
    );
    let demo = TempDemo::new("dump-jsonl-weapon", &bytes);
    let output = run(&["dump-tics", &file_arg(&demo), "--format", "jsonl"]);
    assert_eq!(output.status.code(), Some(0));
    let out = stdout(&output);
    let value: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(value["weapon_change"], true);
    assert_eq!(value["weapon"], 5);
}

// ---------------------------------------------------------------------------
// compare
// ---------------------------------------------------------------------------

#[test]
fn compare_identical_demos() {
    let bytes = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &[(25, 0, 0, 0)],
        true,
    );
    let demo_a = TempDemo::new("cmp-a", &bytes);
    let demo_b = TempDemo::new("cmp-b", &bytes);
    let output = run(&["compare", &file_arg(&demo_a), &file_arg(&demo_b)]);
    assert_eq!(output.status.code(), Some(0));
    let out = stdout(&output);
    assert!(out.contains("Header: identical"), "{out}");
    assert!(out.contains("all 1 tic(s) identical"), "{out}");
    assert!(out.contains("Result: IDENTICAL"), "{out}");
}

#[test]
fn compare_reports_header_difference() {
    let a = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &[(25, 0, 0, 0)],
        true,
    );
    let b = build_demo(
        VERSION_110,
        3,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &[(25, 0, 0, 0)],
        true,
    );
    let demo_a = TempDemo::new("cmp-ha", &a);
    let demo_b = TempDemo::new("cmp-hb", &b);
    let output = run(&["compare", &file_arg(&demo_a), &file_arg(&demo_b)]);
    assert_eq!(output.status.code(), Some(1));
    let out = stdout(&output);
    assert!(out.contains("Header: DIFFERENT"), "{out}");
    assert!(out.contains("- skill: 2 vs 3"), "{out}");
    assert!(out.contains("Result: DIFFERENT"), "{out}");
}

#[test]
fn compare_finds_the_first_differing_tic() {
    let a_tics = [(25, 0, 0, 0); 10];
    let b_tics = [(25, 0, 0, 0); 10];
    let mut a = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &a_tics,
        true,
    );
    let mut b = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &b_tics,
        true,
    );
    // tic index 5, buttons byte (offset 13 + 5*4 + 3)
    a[13 + 5 * 4 + 3] = 1;
    b[13 + 5 * 4 + 3] = 2;
    let demo_a = TempDemo::new("cmp-ta", &a);
    let demo_b = TempDemo::new("cmp-tb", &b);
    let output = run(&["compare", &file_arg(&demo_a), &file_arg(&demo_b)]);
    assert_eq!(output.status.code(), Some(1));
    let out = stdout(&output);
    assert!(out.contains("first difference at tic 5"), "{out}");
    assert!(out.contains("buttons: 1 vs 2 (delta +1)"), "{out}");
    assert!(out.contains("differing tics: 1 of 10 compared"), "{out}");
    assert!(out.contains("first 10 differing tics: 5"), "{out}");
}

#[test]
fn compare_lists_first_ten_differing_tics() {
    let tics = [(25, 0, 0, 0); 20];
    let a = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &tics,
        true,
    );
    let mut b = a.clone();
    // flip buttons on the first 12 tics
    for i in 0..12 {
        b[13 + i * 4 + 3] = 1;
    }
    let demo_a = TempDemo::new("cmp-ten-a", &a);
    let demo_b = TempDemo::new("cmp-ten-b", &b);
    let output = run(&["compare", &file_arg(&demo_a), &file_arg(&demo_b)]);
    assert_eq!(output.status.code(), Some(1));
    let out = stdout(&output);
    assert!(out.contains("differing tics: 12 of 20 compared"), "{out}");
    assert!(
        out.contains("first 10 differing tics: 0, 1, 2, 3, 4, 5, 6, 7, 8, 9"),
        "{out}"
    );
}

#[test]
fn compare_reports_extra_tics() {
    let a_tics = [(25, 0, 0, 0); 10];
    let b_tics = [(25, 0, 0, 0); 7];
    let a = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &a_tics,
        true,
    );
    let b = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &b_tics,
        true,
    );
    let demo_a = TempDemo::new("cmp-ex-a", &a);
    let demo_b = TempDemo::new("cmp-ex-b", &b);
    let output = run(&["compare", &file_arg(&demo_a), &file_arg(&demo_b)]);
    assert_eq!(output.status.code(), Some(1));
    let out = stdout(&output);
    assert!(out.contains("a has 3 extra tic(s) (10 vs 7)"), "{out}");
}

#[test]
fn compare_missing_file_is_an_error() {
    let demo = TempDemo::new("cmp-err", &default_demo());
    let output = run(&["compare", &file_arg(&demo), "C:\\definitely\\not\\real.lmp"]);
    assert_eq!(output.status.code(), Some(2));
}

// ---------------------------------------------------------------------------
// validate
// ---------------------------------------------------------------------------

#[test]
fn validate_ok_demo() {
    let demo = TempDemo::new("val-ok", &default_demo());
    let output = run(&["validate", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(0));
    let out = stdout(&output);
    assert!(out.contains("[ok] version byte 110"), "{out}");
    assert!(out.contains("[ok] skill 2"), "{out}");
    assert!(out.contains("ends with DEMOMARKER"), "{out}");
    assert!(out.contains("Result: OK"), "{out}");
}

#[test]
fn validate_version_109_is_ok() {
    let mut bytes = default_demo();
    bytes[0] = VERSION_109;
    let demo = TempDemo::new("val-109", &bytes);
    let output = run(&["validate", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).contains("[ok] version byte 109 (DOOM 1.9)"));
}

#[test]
fn validate_rejects_unknown_version() {
    let mut bytes = default_demo();
    bytes[0] = 42;
    let demo = TempDemo::new("val-version", &bytes);
    let output = run(&["validate", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).contains("[fatal] unknown demo version 42"));
}

#[test]
fn validate_rejects_bad_skill() {
    let mut bytes = default_demo();
    bytes[1] = 7;
    let demo = TempDemo::new("val-skill", &bytes);
    let output = run(&["validate", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).contains("skill 7 out of range"));
}

#[test]
fn validate_rejects_map_zero() {
    let mut bytes = default_demo();
    bytes[3] = 0;
    let demo = TempDemo::new("val-map0", &bytes);
    let output = run(&["validate", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).contains("map 0"));
}

#[test]
fn validate_rejects_map_too_big() {
    let mut bytes = default_demo();
    bytes[3] = 40;
    let demo = TempDemo::new("val-map40", &bytes);
    let output = run(&["validate", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).contains("out of range"));
}

#[test]
fn validate_rejects_console_player_out_of_range() {
    let mut bytes = default_demo();
    bytes[8] = 4;
    let demo = TempDemo::new("val-console", &bytes);
    let output = run(&["validate", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).contains("console player 4 out of range"));
}

#[test]
fn validate_rejects_zero_players_in_game() {
    let bytes = build_demo(VERSION_110, 2, 1, 1, 0, 0, 0, 0, 0, [0, 0, 0, 0], &[], true);
    let demo = TempDemo::new("val-noplayers", &bytes);
    let output = run(&["validate", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).contains("no players in game"));
}

#[test]
fn validate_missing_marker_is_fatal() {
    let bytes = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &[(25, 0, 0, 0)],
        false,
    );
    let demo = TempDemo::new("val-nomarker", &bytes);
    let output = run(&["validate", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).contains("does not end with DEMOMARKER"));
}

#[test]
fn validate_truncated_tic_is_fatal() {
    let mut bytes = default_demo();
    bytes.truncate(15); // header + 2 stray bytes: half a tic
    let demo = TempDemo::new("val-truncated", &bytes);
    let output = run(&["validate", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).contains("truncated tic"));
}

#[test]
fn validate_trailing_garbage_is_a_warning() {
    let mut bytes = default_demo();
    bytes.extend_from_slice(&[0xDE, 0xAD]);
    let demo = TempDemo::new("val-trailing", &bytes);
    let output = run(&["validate", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(1));
    let out = stdout(&output);
    assert!(out.contains("trailing byte(s) after DEMOMARKER"), "{out}");
    assert!(out.contains("Result: WARNINGS"), "{out}");
}

#[test]
fn validate_undefined_button_bit_is_a_warning() {
    let bytes = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        0,
        0,
        0,
        0,
        [1, 0, 0, 0],
        &[(0, 0, 0, 0x40)],
        true,
    );
    let demo = TempDemo::new("val-0x40", &bytes);
    let output = run(&["validate", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains("button bit 0x40"));
}

#[test]
fn validate_flags_are_checked() {
    let bytes = build_demo(
        VERSION_110,
        2,
        1,
        1,
        0,
        2, // respawn: 2 is invalid
        0,
        0,
        0,
        [1, 0, 0, 0],
        &[],
        true,
    );
    let demo = TempDemo::new("val-flags", &bytes);
    let output = run(&["validate", &file_arg(&demo)]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains("respawn flag = 2"));
}

#[test]
fn validate_unreadable_file_fails_fatally() {
    let output = run(&["validate", "C:\\definitely\\not\\a\\real-file.lmp"]);
    assert_eq!(output.status.code(), Some(2));
    let out = stdout(&output);
    assert!(out.contains("[fatal]"), "{out}");
    assert!(out.contains("Result: FATAL"), "{out}");
}
