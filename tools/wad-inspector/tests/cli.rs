//! Integration tests for the `doom-wad-inspector` binary.
//!
//! Builds synthetic WADs in a temp directory (nothing binary is committed)
//! and runs the real binary against them.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Builds a minimal but well-formed WAD image around `lumps`.
fn build_wad(magic: &[u8; 4], lumps: &[(&str, &[u8])]) -> Vec<u8> {
    let mut blob = Vec::new();
    blob.extend_from_slice(&[0; 12]); // header, filled in at the end

    let mut entries = Vec::with_capacity(lumps.len());
    for (name, payload) in lumps {
        let file_pos = blob.len();
        blob.extend_from_slice(payload);
        entries.push((*name, file_pos, payload.len()));
    }
    let dir_offset = blob.len();
    for (name, file_pos, size) in &entries {
        blob.extend_from_slice(&(*file_pos as i32).to_le_bytes());
        blob.extend_from_slice(&(*size as i32).to_le_bytes());
        let mut name_bytes = [0u8; 8];
        for (i, b) in name.as_bytes().iter().take(8).enumerate() {
            name_bytes[i] = b.to_ascii_uppercase();
        }
        blob.extend_from_slice(&name_bytes);
    }

    blob[..4].copy_from_slice(magic);
    blob[4..8].copy_from_slice(&(entries.len() as i32).to_le_bytes());
    blob[8..12].copy_from_slice(&(dir_offset as i32).to_le_bytes());
    blob
}

struct TempWad {
    path: PathBuf,
}

impl TempWad {
    fn new(label: &str, bytes: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!(
            "doom-wad-inspector-test-{}-{label}-{}.wad",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, bytes).expect("write temp wad");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempWad {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_doom-wad-inspector"))
        .args(args)
        .output()
        .expect("run doom-wad-inspector")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A small, complete synthetic IWAD: core lumps, flat and sprite
/// namespaces, one full Doom II map and one episode map.
fn synthetic_iwad() -> Vec<u8> {
    build_wad(
        b"IWAD",
        &[
            ("PLAYPAL", &[0x11; 768]),
            ("COLORMAP", &[0x22; 34 * 256]),
            ("TEXTURE1", b"textures"),
            ("PNAMES", b"patches"),
            ("F_START", b""),
            ("FLAT1", &[0x33; 4096]),
            ("F_END", b""),
            ("S_START", b""),
            ("POSSA1", b"sprite"),
            ("S_END", b""),
            ("MAP01", b""),
            ("THINGS", b"things01"),
            ("LINEDEFS", b"lines01"),
            ("SIDEDEFS", b"sides01"),
            ("VERTEXES", b"vertex01"),
            ("SEGS", b"segs0001"),
            ("SSECTORS", b"ssect001"),
            ("NODES", b"nodes001"),
            ("SECTORS", b"sects001"),
            ("REJECT", b"reject01"),
            ("BLOCKMAP", b"block001"),
        ],
    )
}

#[test]
fn help_and_version_work() {
    let help = run(&["--help"]);
    assert!(help.status.success());
    let text = stdout(&help);
    assert!(text.contains("Usage"), "missing usage: {text}");
    assert!(text.contains("list"), "missing subcommands: {text}");

    let version = run(&["--version"]);
    assert!(version.status.success());
    assert!(stdout(&version).contains("doom-wad-inspector"));
}

#[test]
fn list_prints_index_name_size_offset() {
    let wad = TempWad::new("list", &synthetic_iwad());
    let output = run(&["list", wad.path().to_str().unwrap()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("index"), "missing header: {text}");
    assert!(text.contains("PLAYPAL"), "missing lump: {text}");
    assert!(text.contains("0x0000000c"), "missing offset: {text}");
    assert!(text.contains("21 lump(s)"), "missing count: {text}");
    // the table must have one row per lump plus a header and a separator
    let rows: Vec<&str> = text.lines().collect();
    assert_eq!(rows.len(), 1 + 1 + 21 + 1, "unexpected lines: {text}");
}

#[test]
fn stats_reports_size_coverage_and_extremes() {
    let wad = TempWad::new("stats", &synthetic_iwad());
    let file_len = std::fs::metadata(wad.path()).unwrap().len();
    let output = run(&["stats", wad.path().to_str().unwrap()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("format:           IWAD"),
        "missing format: {text}"
    );
    assert!(text.contains(&format!("file size:        {file_len} bytes")));
    assert!(
        text.contains("lump count:       21"),
        "missing count: {text}"
    );
    assert!(text.contains("smallest lump:"), "missing smallest: {text}");
    assert!(text.contains("largest lump:"), "missing largest: {text}");
    assert!(text.contains("lump coverage:"), "missing coverage: {text}");
    assert!(
        text.contains("COLORMAP"),
        "largest should be COLORMAP: {text}"
    );
}

#[test]
fn dump_writes_the_lump_to_a_file() {
    let wad = TempWad::new("dump", &synthetic_iwad());
    let dest = std::env::temp_dir().join(format!(
        "doom-wad-inspector-dump-{}-{}.bin",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let output = run(&[
        "dump",
        wad.path().to_str().unwrap(),
        "FLAT1",
        "--out",
        dest.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(std::fs::read(&dest).unwrap(), vec![0x33; 4096]);
    let _ = std::fs::remove_file(&dest);
}

#[test]
fn dump_to_stdout_is_binary_safe() {
    let wad = TempWad::new("dump-stdout", &synthetic_iwad());
    let output = run(&[
        "dump",
        wad.path().to_str().unwrap(),
        "COLORMAP",
        "--out",
        "-",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(output.stdout, vec![0x22; 34 * 256]);
}

#[test]
fn dump_without_out_fails_cleanly() {
    let wad = TempWad::new("dump-noout", &synthetic_iwad());
    let output = run(&["dump", wad.path().to_str().unwrap(), "PLAYPAL"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("--out"));
}

#[test]
fn dump_refuses_to_overwrite_the_input() {
    let wad = TempWad::new("dump-self", &synthetic_iwad());
    let before = std::fs::read(wad.path()).unwrap();
    let output = run(&[
        "dump",
        wad.path().to_str().unwrap(),
        "PLAYPAL",
        "--out",
        wad.path().to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("refusing to overwrite"));
    assert_eq!(std::fs::read(wad.path()).unwrap(), before);
}

#[test]
fn dump_missing_lump_fails() {
    let wad = TempWad::new("dump-missing", &synthetic_iwad());
    let output = run(&["dump", wad.path().to_str().unwrap(), "NOPE", "--out", "-"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("not found"));
}

#[test]
fn find_lists_all_occurrences_and_marks_the_active_one() {
    let wad = TempWad::new(
        "find",
        &build_wad(
            b"IWAD",
            &[
                ("TEXTURE1", b"first"),
                ("PLAYPAL", b"palette"),
                ("TEXTURE1", b"second"),
                ("TEXTURE1", b"third"),
            ],
        ),
    );
    let output = run(&["find", wad.path().to_str().unwrap(), "texture1"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("3 occurrence(s)"), "missing count: {text}");
    assert!(text.contains("[active]"), "missing active marker: {text}");
    // exactly one active marker, on the last occurrence (index 3)
    assert_eq!(text.matches("[active]").count(), 1);
    let active_line = text.lines().find(|l| l.contains("[active]")).unwrap();
    assert!(
        active_line.trim_start().starts_with('3'),
        "active must be index 3: {text}"
    );

    // query is case-insensitive
    let upper = run(&["find", wad.path().to_str().unwrap(), "TEXTURE1"]);
    assert!(upper.status.success());
}

#[test]
fn find_without_matches_exits_1() {
    let wad = TempWad::new("find-missing", &synthetic_iwad());
    let output = run(&["find", wad.path().to_str().unwrap(), "NOSUCHLUMP"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains("no lump named"));
}

#[test]
fn find_all_falls_back_to_prefix_matches() {
    let wad = TempWad::new("find-all", &synthetic_iwad());
    // "TEXTUR" is not an exact lump name, but TEXTURE1 starts with it
    let output = run(&["find", wad.path().to_str().unwrap(), "TEXTUR", "--all"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    assert!(text.contains("TEXTURE1"), "missing prefix match: {text}");
}

#[test]
fn validate_passes_on_a_complete_iwad() {
    let wad = TempWad::new("validate-ok", &synthetic_iwad());
    let output = run(&["validate", wad.path().to_str().unwrap()]);
    let text = stdout(&output);
    assert_eq!(
        output.status.code(),
        Some(0),
        "text: {text}\nstderr: {}",
        stderr(&output)
    );
    assert!(text.contains("result: OK"), "missing result: {text}");
    assert!(
        text.contains("[ok] bounds:"),
        "missing bounds check: {text}"
    );
    assert!(
        text.contains("[ok] flat namespace:"),
        "missing flat check: {text}"
    );
    assert!(
        text.contains("map MAP01: all expected lumps present"),
        "missing map check: {text}"
    );
}

#[test]
fn validate_reports_warnings_and_exits_1() {
    // IWAD missing COLORMAP and the sprite namespace + an incomplete map
    let bytes = build_wad(
        b"IWAD",
        &[
            ("PLAYPAL", &[0x11; 768]),
            ("TEXTURE1", b"textures"),
            ("PNAMES", b"patches"),
            ("F_START", b""),
            ("FLAT1", &[0x33; 4096]),
            ("F_END", b""),
            ("E1M1", b""),
            ("THINGS", b"things"),
        ],
    );
    let wad = TempWad::new("validate-warn", &bytes);
    let output = run(&["validate", wad.path().to_str().unwrap()]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(1), "text: {text}");
    assert!(
        text.contains("[warn] core IWAD lump(s) missing: COLORMAP"),
        "text: {text}"
    );
    assert!(
        text.contains("sprite namespace markers (S_START/S_END) absent"),
        "text: {text}"
    );
    assert!(
        text.contains("map E1M1: missing expected lump(s)"),
        "text: {text}"
    );
    assert!(text.contains("result: WARNINGS"), "text: {text}");
}

#[test]
fn validate_does_not_flag_expected_map_lumps_as_duplicates() {
    // two complete maps: every map block carries THINGS/LINEDEFS/etc.,
    // those must not show up as duplicates
    let map = |thing: &[u8]| {
        vec![
            ("THINGS", thing.to_vec()),
            ("LINEDEFS", b"l".to_vec()),
            ("SIDEDEFS", b"s".to_vec()),
            ("VERTEXES", b"v".to_vec()),
            ("SEGS", b"g".to_vec()),
            ("SSECTORS", b"u".to_vec()),
            ("NODES", b"n".to_vec()),
            ("SECTORS", b"c".to_vec()),
            ("REJECT", b"r".to_vec()),
            ("BLOCKMAP", b"b".to_vec()),
        ]
    };
    let mut lumps: Vec<(&str, Vec<u8>)> = vec![
        ("PLAYPAL", vec![0x11; 768]),
        ("COLORMAP", vec![0x22; 34 * 256]),
        ("TEXTURE1", b"textures".to_vec()),
        ("PNAMES", b"patches".to_vec()),
        ("F_START", vec![]),
        ("FLAT1", vec![0x33; 4096]),
        ("F_END", vec![]),
        ("S_START", vec![]),
        ("S_END", vec![]),
        ("MAP01", vec![]),
    ];
    lumps.extend(map(b"things01"));
    lumps.push(("MAP02", vec![]));
    lumps.extend(map(b"things02"));
    let refs: Vec<(&str, &[u8])> = lumps.iter().map(|(n, d)| (*n, d.as_slice())).collect();
    let wad = TempWad::new("validate-maps-dup", &build_wad(b"IWAD", &refs));
    let output = run(&["validate", wad.path().to_str().unwrap()]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(0), "text: {text}");
    assert!(!text.contains("duplicate lump 'THINGS'"), "text: {text}");
}

#[test]
fn validate_reports_duplicates_as_warnings_for_iwads() {
    let bytes = build_wad(
        b"IWAD",
        &[
            ("PLAYPAL", &[0x11; 768]),
            ("COLORMAP", &[0x22; 34 * 256]),
            ("TEXTURE1", b"first"),
            ("PNAMES", b"patches"),
            ("TEXTURE1", b"second"),
            ("F_START", b""),
            ("FLAT1", &[0x33; 4096]),
            ("F_END", b""),
            ("S_START", b""),
            ("S_END", b""),
        ],
    );
    let wad = TempWad::new("validate-dup", &bytes);
    let output = run(&["validate", wad.path().to_str().unwrap()]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(1), "text: {text}");
    assert!(text.contains("duplicate lump 'TEXTURE1'"), "text: {text}");
}

#[test]
fn validate_reports_duplicates_as_notes_for_pwads() {
    let bytes = build_wad(b"PWAD", &[("PLAYPAL", b"mine"), ("PLAYPAL", b"mine-again")]);
    let wad = TempWad::new("validate-pwad-dup", &bytes);
    let output = run(&["validate", wad.path().to_str().unwrap()]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(0), "text: {text}");
    assert!(
        text.contains("[note] duplicate lump 'PLAYPAL'"),
        "text: {text}"
    );
    assert!(text.contains("result: OK"), "text: {text}");
}

#[test]
fn validate_rejects_a_broken_file_with_exit_2() {
    let wad = TempWad::new("validate-fatal", b"this is not a wad at all");
    let output = run(&["validate", wad.path().to_str().unwrap()]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(2));
    assert!(text.contains("[fatal]"), "text: {text}");
    assert!(text.contains("result: FATAL"), "text: {text}");
}

#[test]
fn maps_lists_maps_and_lump_counts() {
    let bytes = build_wad(
        b"IWAD",
        &[
            ("MAP01", b""),
            ("THINGS", b"t"),
            ("LINEDEFS", b"l"),
            ("SIDEDEFS", b"s"),
            ("VERTEXES", b"v"),
            ("SEGS", b"g"),
            ("SSECTORS", b"u"),
            ("NODES", b"n"),
            ("SECTORS", b"c"),
            ("REJECT", b"r"),
            ("BLOCKMAP", b"b"),
            ("E2M3", b""),
            ("THINGS", b"t2"),
            ("LINEDEFS", b"l2"),
        ],
    );
    let wad = TempWad::new("maps", &bytes);
    let output = run(&["maps", wad.path().to_str().unwrap()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("MAP01"), "text: {text}");
    assert!(text.contains("E2M3"), "text: {text}");
    // MAP01 block is the marker plus its ten lumps, E2M3 is marker + two
    let map01_line = text.lines().find(|l| l.starts_with("MAP01")).unwrap();
    let e2m3_line = text.lines().find(|l| l.starts_with("E2M3")).unwrap();
    assert!(map01_line.contains("11"), "MAP01 line: {map01_line}");
    assert!(e2m3_line.contains('3'), "E2M3 line: {e2m3_line}");
    assert!(!e2m3_line.contains("ok"), "E2M3 is incomplete: {e2m3_line}");
    assert!(text.contains("2 map(s) found"), "text: {text}");
}

#[test]
fn maps_rejects_map_numbers_out_of_range_and_false_markers() {
    let bytes = build_wad(
        b"IWAD",
        &[
            ("MAP33", b""),    // out of range for MAPxx
            ("E5M1", b""),     // no episode 5
            ("MAP00", b""),    // no map 0
            ("TEXTURE1", b""), // not a map
        ],
    );
    let wad = TempWad::new("maps-none", &bytes);
    let output = run(&["maps", wad.path().to_str().unwrap()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("no maps found"));
}

#[test]
fn unknown_file_type_fails_the_command() {
    let wad = TempWad::new("bad-magic-list", b"XXAD\x00\x00\x00\x00\x00\x00\x00\x00");
    let output = run(&["list", wad.path().to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("bad magic"));
}
