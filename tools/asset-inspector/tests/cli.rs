//! Integration tests for the `doom-asset-inspector` binary.
//!
//! Builds synthetic WADs in the temp directory (nothing binary is
//! committed) and runs the real binary against them.

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

fn playpal_lump(num_palettes: usize) -> Vec<u8> {
    // 2 distinct, deterministic palettes; enough to tell indices apart
    let mut data = Vec::with_capacity(num_palettes * 768);
    for p in 0..num_palettes {
        for c in 0..256usize {
            let base = (p * 256 + c) as u32;
            data.push(((base * 3 + 11) & 0xff) as u8);
            data.push(((base * 5 + 37) & 0xff) as u8);
            data.push(((base * 7 + 91) & 0xff) as u8);
        }
    }
    data
}

fn colormap_lump(num_maps: usize) -> Vec<u8> {
    // map k: identity shifted down by k steps, clamped at 0 — monotone,
    // like the real diminishing-brightness maps
    let mut data = Vec::with_capacity(num_maps * 256);
    for k in 0..num_maps {
        for x in 0..256usize {
            data.push((x as i32 - (k as i32) * 7).max(0) as u8);
        }
    }
    data
}

fn flat_lump() -> Vec<u8> {
    (0..4096usize).map(|i| (i * 29 + 7) as u8).collect()
}

/// One patch definition as raw bytes: `(topdelta, pixels)` per column.
fn patch_lump(
    width: i16,
    height: i16,
    left_offset: i16,
    top_offset: i16,
    columns: &[Vec<(u8, Vec<u8>)>],
) -> Vec<u8> {
    assert_eq!(columns.len(), width as usize);

    let mut data = Vec::new();
    data.extend_from_slice(&width.to_le_bytes());
    data.extend_from_slice(&height.to_le_bytes());
    data.extend_from_slice(&left_offset.to_le_bytes());
    data.extend_from_slice(&top_offset.to_le_bytes());

    let table_start = data.len();
    data.resize(table_start + columns.len() * 4, 0);

    for (i, posts) in columns.iter().enumerate() {
        let column_offset = data.len() as i32;
        data[table_start + i * 4..table_start + (i + 1) * 4]
            .copy_from_slice(&column_offset.to_le_bytes());
        for &(topdelta, ref pixels) in posts {
            data.push(topdelta);
            data.push(pixels.len() as u8);
            data.push(0); // unused pad
            data.extend_from_slice(pixels);
            data.push(0); // unused pad
        }
        data.push(0xff); // column terminator
    }
    data
}

fn pnames_lump(names: &[&str]) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&(names.len() as i32).to_le_bytes());
    for name in names {
        let mut raw = [0u8; 8];
        for (dst, src) in raw.iter_mut().zip(name.as_bytes().iter()) {
            *dst = *src;
        }
        data.extend_from_slice(&raw);
    }
    data
}

fn texture_def(
    name: &str,
    masked: i32,
    width: i16,
    height: i16,
    patches: &[(i16, i16, i16)],
) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut raw = [0u8; 8];
    for (dst, src) in raw.iter_mut().zip(name.as_bytes().iter()) {
        *dst = *src;
    }
    bytes.extend_from_slice(&raw);
    bytes.extend_from_slice(&masked.to_le_bytes());
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(&0i32.to_le_bytes()); // obsolete column directory
    bytes.extend_from_slice(&(patches.len() as i16).to_le_bytes());
    for &(x, y, patch) in patches {
        bytes.extend_from_slice(&x.to_le_bytes());
        bytes.extend_from_slice(&y.to_le_bytes());
        bytes.extend_from_slice(&patch.to_le_bytes());
        bytes.extend_from_slice(&0i16.to_le_bytes()); // stepdir
        bytes.extend_from_slice(&0i16.to_le_bytes()); // colormap
    }
    bytes
}

fn texture_lump(defs: &[Vec<u8>]) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&(defs.len() as i32).to_le_bytes());
    let table_start = data.len();
    data.resize(table_start + defs.len() * 4, 0);
    for (i, def) in defs.iter().enumerate() {
        let offset = data.len() as i32;
        data[table_start + i * 4..table_start + (i + 1) * 4].copy_from_slice(&offset.to_le_bytes());
        data.extend_from_slice(def);
    }
    data
}

/// The patch every fixture texture/sky uses: 4x6, one visible pixel
/// somewhere in each column.
fn test_patch() -> Vec<u8> {
    patch_lump(
        4,
        6,
        2,
        5,
        &[
            vec![(1, vec![10, 11, 12])],
            vec![],
            vec![(0, vec![20, 21, 40, 41, 42, 43])],
            vec![(2, vec![30])],
        ],
    )
}

/// A well-formed WAD exercising every command.
fn full_wad() -> Vec<u8> {
    let playpal = playpal_lump(2);
    let colormap = colormap_lump(34);
    let pnames = pnames_lump(&["TESTP1"]);
    let texture1 = texture_lump(&[
        texture_def("WALL01", 0, 4, 6, &[(0, 0, 0)]),
        texture_def("SKY1", 0, 4, 6, &[(0, 0, 0)]),
    ]);
    let patch = test_patch();
    let flat = flat_lump();
    let empty: &[u8] = &[];
    let lumps: Vec<(&str, &[u8])> = vec![
        ("PLAYPAL", &playpal),
        ("COLORMAP", &colormap),
        ("PNAMES", &pnames),
        ("TEXTURE1", &texture1),
        ("TESTP1", &patch),
        ("F_START", empty),
        ("FLAT01", &flat),
        ("F_END", empty),
    ];
    build_wad(b"PWAD", &lumps)
}

struct TempPath {
    path: PathBuf,
}

impl TempPath {
    fn new(label: &str, extension: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "doom-asset-inspector-test-{label}-{}-{}.{extension}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        Self { path }
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn temp_wad(label: &str, bytes: &[u8]) -> TempPath {
    let temp = TempPath::new(label, "wad");
    std::fs::write(&temp.path, bytes).expect("write temp wad");
    temp
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_doom-asset-inspector"))
        .args(args)
        .output()
        .expect("run doom-asset-inspector")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_png(path: &Path, width: u32, height: u32) {
    let bytes = std::fs::read(path).expect("read png");
    assert!(bytes.len() > 33, "png too small: {}", bytes.len());
    assert_eq!(
        &bytes[..8],
        &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
    );
    assert_eq!(&bytes[12..16], b"IHDR");
    // IHDR: width/height big-endian at bytes 16..24, bit depth at 24,
    // color type at 25 (6 = truecolor with alpha)
    let w = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    assert_eq!((w, h), (width, height));
    assert_eq!(bytes[24], 8, "bit depth must be 8");
    assert_eq!(bytes[25], 6, "color type must be truecolor with alpha");
}

// ----------------------------------------------------------------- tests ----

#[test]
fn palette_reports_summary_table() {
    let wad = temp_wad("palette-list", &full_wad());
    let output = run(&["palette", wad.path.to_str().unwrap()]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(text.contains("PLAYPAL: 2 palette(s)"));
    assert!(text.contains("distinct"));
    assert!(text.contains("r min-max"));
}

#[test]
fn palette_out_writes_one_png_per_palette() {
    let wad = temp_wad("palette-out", &full_wad());
    let prefix = TempPath::new("palette-prefix", "pfx");
    let prefix_arg = prefix.path.to_string_lossy().into_owned();
    let output = run(&["palette", wad.path.to_str().unwrap(), "--out", &prefix_arg]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    // --out prefix gets "-<index>.png" appended
    for index in 0..2 {
        let generated = PathBuf::from(format!("{prefix_arg}-{index}.png"));
        assert_png(&generated, 512, 512);
        let _ = std::fs::remove_file(&generated);
    }
}

#[test]
fn palette_colors_writes_grid_png() {
    let wad = temp_wad("palette-colors", &full_wad());
    let out = TempPath::new("palette-colors-out", "png");
    let output = run(&[
        "palette-colors",
        wad.path.to_str().unwrap(),
        "0",
        "--out",
        out.path.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_png(&out.path, 512, 512);
}

#[test]
fn palette_colors_text_grid_without_out() {
    let wad = temp_wad("palette-colors-text", &full_wad());
    let output = run(&["palette-colors", wad.path.to_str().unwrap(), "0"]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(text.contains("palette 0"));
    assert!(text.lines().filter(|line| line.starts_with('#')).count() >= 16);
}

#[test]
fn palette_colors_index_out_of_range_is_fatal() {
    let wad = temp_wad("palette-colors-oob", &full_wad());
    let output = run(&["palette-colors", wad.path.to_str().unwrap(), "99"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("out of range"));
}

#[test]
fn colormap_reports_kinds() {
    let wad = temp_wad("colormap-list", &full_wad());
    let output = run(&["colormap", wad.path.to_str().unwrap()]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(text.contains("COLORMAP: 34 map(s)"));
    assert!(text.contains("invulnerability"));
}

#[test]
fn colormap_out_writes_stacked_png() {
    let wad = temp_wad("colormap-out", &full_wad());
    let out = TempPath::new("colormap-out-png", "png");
    let output = run(&[
        "colormap",
        wad.path.to_str().unwrap(),
        "--out",
        out.path.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_png(&out.path, 256, 16 * 34);
}

#[test]
fn patch_info_without_out() {
    let wad = temp_wad("patch-info", &full_wad());
    let output = run(&["patch", wad.path.to_str().unwrap(), "TESTP1"]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(text.contains("size:        4x6"));
    assert!(text.contains("columns:     4"));
    assert!(text.contains("posts:"));
    assert!(text.contains("pixels:"));
}

#[test]
fn patch_out_writes_rgba_png() {
    let wad = temp_wad("patch-out", &full_wad());
    let out = TempPath::new("patch-out-png", "png");
    let output = run(&[
        "patch",
        wad.path.to_str().unwrap(),
        "TESTP1",
        "--out",
        out.path.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_png(&out.path, 4, 6);
}

#[test]
fn sprite_is_an_alias_of_patch() {
    let wad = temp_wad("sprite-alias", &full_wad());
    let out = TempPath::new("sprite-alias-png", "png");
    let output = run(&[
        "sprite",
        wad.path.to_str().unwrap(),
        "testp1", // case-insensitive
        "--out",
        out.path.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_png(&out.path, 4, 6);
}

#[test]
fn patch_missing_lump_is_fatal() {
    let wad = temp_wad("patch-missing", &full_wad());
    let output = run(&["patch", wad.path.to_str().unwrap(), "NOPE"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("NOPE"));
}

#[test]
fn texture_info_and_out() {
    let wad = temp_wad("texture-out", &full_wad());
    let out = TempPath::new("texture-out-png", "png");
    let output = run(&[
        "texture",
        wad.path.to_str().unwrap(),
        "WALL01",
        "--out",
        out.path.to_str().unwrap(),
    ]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(text.contains("texture WALL01 (TEXTURE1)"));
    assert!(text.contains("4x6"));
    assert_png(&out.path, 4, 6);
}

#[test]
fn texture_unknown_name_is_fatal() {
    let wad = temp_wad("texture-missing", &full_wad());
    let output = run(&["texture", wad.path.to_str().unwrap(), "DOESNOTEXIST"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("not found"));
}

#[test]
fn flat_out_writes_64x64_png() {
    let wad = temp_wad("flat-out", &full_wad());
    let out = TempPath::new("flat-out-png", "png");
    let output = run(&[
        "flat",
        wad.path.to_str().unwrap(),
        "FLAT01",
        "--out",
        out.path.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_png(&out.path, 64, 64);
}

#[test]
fn flats_reports_valid_namespace() {
    let wad = temp_wad("flats-valid", &full_wad());
    let output = run(&["flats", wad.path.to_str().unwrap()]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(text.contains("FLAT01"));
    assert!(text.contains("ok"));
    assert!(text.contains("1 flat(s)"));
}

#[test]
fn flats_flags_invalid_size_as_warning() {
    let playpal = playpal_lump(1);
    let bad_flat = [1u8, 2, 3];
    let empty: &[u8] = &[];
    let lumps: Vec<(&str, &[u8])> = vec![
        ("PLAYPAL", &playpal),
        ("F_START", empty),
        ("BADFLAT", &bad_flat),
        ("F_END", empty),
    ];
    let wad = temp_wad("flats-invalid", &build_wad(b"PWAD", &lumps));
    let output = run(&["flats", wad.path.to_str().unwrap()]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(1));
    assert!(text.contains("BADFLAT"));
    assert!(text.contains("invalid"));
}

#[test]
fn flats_without_namespace_falls_back_to_4096_byte_lumps() {
    let playpal = playpal_lump(1);
    let flat = flat_lump();
    let small = [0u8; 12];
    let lumps: Vec<(&str, &[u8])> =
        vec![("PLAYPAL", &playpal), ("MYSTERY", &flat), ("SMALL", &small)];
    let wad = temp_wad("flats-fallback", &build_wad(b"PWAD", &lumps));
    let output = run(&["flats", wad.path.to_str().unwrap()]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(1));
    assert!(text.contains("no F_START/F_END markers"));
    assert!(text.contains("MYSTERY"));
    assert!(!text.contains("SMALL"));
}

#[test]
fn sky_finds_sky_texture_and_writes_png() {
    let wad = temp_wad("sky-out", &full_wad());
    let out = TempPath::new("sky-out-png", "png");
    let output = run(&[
        "sky",
        wad.path.to_str().unwrap(),
        "--out",
        out.path.to_str().unwrap(),
    ]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(text.contains("SKY1"));
    // a single sky texture uses --out as the exact path
    assert_png(&out.path, 4, 6);
}

#[test]
fn sky_with_multiple_skies_writes_one_png_per_sky() {
    let texture1 = texture_lump(&[
        texture_def("SKY1", 0, 4, 6, &[(0, 0, 0)]),
        texture_def("SKY2", 0, 4, 6, &[(0, 0, 0)]),
    ]);
    let playpal = playpal_lump(1);
    let pnames = pnames_lump(&["TESTP1"]);
    let patch = test_patch();
    let lumps: Vec<(&str, &[u8])> = vec![
        ("PLAYPAL", &playpal),
        ("PNAMES", &pnames),
        ("TEXTURE1", &texture1),
        ("TESTP1", &patch),
    ];
    let wad = temp_wad("sky-multi", &build_wad(b"PWAD", &lumps));
    let prefix = TempPath::new("sky-multi-prefix", "pfx");
    let prefix_arg = prefix.path.to_string_lossy().into_owned();
    let output = run(&["sky", wad.path.to_str().unwrap(), "--out", &prefix_arg]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    for name in ["SKY1", "SKY2"] {
        let generated = PathBuf::from(format!("{prefix_arg}-{name}.png"));
        assert_png(&generated, 4, 6);
        let _ = std::fs::remove_file(&generated);
    }
}

#[test]
fn sky_without_sky_texture_warns() {
    let no_sky = texture_lump(&[texture_def("WALL01", 0, 4, 6, &[(0, 0, 0)])]);
    let playpal = playpal_lump(1);
    let pnames = pnames_lump(&["TESTP1"]);
    let patch = test_patch();
    let lumps: Vec<(&str, &[u8])> = vec![
        ("PLAYPAL", &playpal),
        ("PNAMES", &pnames),
        ("TEXTURE1", &no_sky),
        ("TESTP1", &patch),
    ];
    let wad = temp_wad("sky-missing", &build_wad(b"PWAD", &lumps));
    let output = run(&["sky", wad.path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn missing_wad_is_fatal() {
    let missing = TempPath::new("does-not-exist", "wad");
    let output = run(&["palette", missing.path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert!(!stderr(&output).is_empty());
}
