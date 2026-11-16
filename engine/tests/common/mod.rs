//! Shared fixtures for the headless integration tests.
//!
//! Everything here builds a synthetic in-memory IWAD — no real `DOOM.WAD`
//! is ever needed, and every test in this directory must run on a bare CI
//! machine. The fixture carries two maps:
//!
//! - `E1M1` — the scenario under test (a plain room, the room with an
//!   exit line, or a two-room layout with a manual door in the middle),
//! - `E1M2` — always a plain room; the level after an exit in `E1M1`.
//!
//! The resource lumps are the minimum the engine decodes at startup:
//! PLAYPAL, COLORMAP, PNAMES, TEXTURE1, the flats/sprites namespaces,
//! one texture source patch, the HUD/status-bar/menu fonts and patches,
//! and the intermission art (so the exit flow can actually reach the
//! `GS_INTERMISSION` state instead of falling back to "assets missing,
//! skip to next level").

#![allow(dead_code)] // not every test uses every helper

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use doom_engine::args::EngineArgs;
use doom_engine::game::{Game, GameConfig, TicCmdInput};
use doom_engine::menu::GameMode;
use doom_engine::render::IndexedFramebuffer;
use doom_engine::wad::WadFile;

// ---------------------------------------------------------------------------
// palette / resources
// ---------------------------------------------------------------------------

/// A 256-entry palette that is deliberately not a gray ramp, so "some
/// pixels differ" assertions mean something. All 14 palette slots are
/// identical ramps.
pub fn playpal_bytes() -> Vec<u8> {
    let mut out = Vec::with_capacity(PALETTE_RAMP_BYTES);
    for _ in 0..14 {
        for i in 0..256u32 {
            out.push(((i * 5 + 40) % 256) as u8);
            out.push(((i * 3 + 80) % 256) as u8);
            out.push(((i * 7 + 160) % 256) as u8);
        }
    }
    out
}

const PALETTE_RAMP_BYTES: usize = 768 * 14;

/// 34 diminishing colormaps + fuzz + invulnerability; every ramp is the
/// identity, which is all the headless tests need.
pub fn colormap_lump() -> Vec<u8> {
    let mut out = Vec::with_capacity(256 * 34);
    for _ in 0..34 {
        out.extend_from_slice(&(0u8..=255).collect::<Vec<_>>());
    }
    out
}

/// A valid patch filled with a single palette index.
pub fn dummy_patch(width: i16, height: i16, fill: u8) -> Vec<u8> {
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
        out.push(0x00); // topdelta
        out.push(height as u8); // length
        out.push(0); // pad
        out.extend(std::iter::repeat_n(fill, height as usize));
        out.push(0); // pad
        out.push(0xff); // terminator
    }
    out
}

pub fn flat_lump(fill: u8) -> Vec<u8> {
    vec![fill; 4096]
}

fn dmx_sound() -> Vec<u8> {
    // tag=3 (decoded), rate=11025, 4 bytes of samples
    let mut out = Vec::new();
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&11025u16.to_le_bytes());
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]);
    out
}

/// A 64x64 patch with a per-column color ramp - the WALL1 texture source.
fn wall1_patch() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&64i16.to_le_bytes()); // width
    out.extend_from_slice(&64i16.to_le_bytes()); // height
    out.extend_from_slice(&0i16.to_le_bytes()); // left offset
    out.extend_from_slice(&0i16.to_le_bytes()); // top offset

    let column_body = |color: u8| -> Vec<u8> {
        let mut body = Vec::new();
        body.push(0x00); // topdelta
        body.push(64); // length
        body.push(0); // pad
        body.extend(std::iter::repeat_n(color, 64));
        body.push(0); // pad
        body.push(0xff); // terminator
        body
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

// ---------------------------------------------------------------------------
// maps
// ---------------------------------------------------------------------------

/// Which layout `E1M1` carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapScenario {
    /// A single 256x256 room, four walls, no specials.
    Plain,
    /// The room with a free-standing exit line (special 11) in front of
    /// the player start.
    Exit,
    /// West room (player), a thin tagged door corridor in the middle,
    /// and an east room behind it. The door line is two-sided, special 1
    /// (manual door), tag 1; the corridor sector is the door sector
    /// (floor 0, ceiling 0 = sealed).
    Door,
}

#[derive(Clone, Copy, Debug)]
pub struct FixtureSpec {
    pub scenario: MapScenario,
    /// Add a MT_POSSESSED thing at (192, 128).
    pub monster: bool,
}

fn blockmap_lump(num_lines: u16) -> Vec<u8> {
    // 3x3 grid covering the whole map, every cell lists all lines
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

fn vertexes_lump(scenario: MapScenario) -> Vec<u8> {
    let verts: &[(i16, i16)] = match scenario {
        MapScenario::Plain => &[(0, 0), (256, 0), (256, 256), (0, 256)],
        MapScenario::Exit => &[
            (0, 0),
            (256, 0),
            (256, 256),
            (0, 256),
            (160, 64),
            (160, 192),
        ],
        MapScenario::Door => &[
            (0, 0),
            (256, 0),
            (256, 256),
            (0, 256),
            (160, 0),
            (160, 96),
            (160, 160),
            (160, 256),
            (192, 0),
            (192, 96),
            (192, 160),
            (192, 256),
        ],
    };
    let mut out = Vec::new();
    for (x, y) in verts {
        out.extend_from_slice(&x.to_le_bytes());
        out.extend_from_slice(&y.to_le_bytes());
    }
    out
}

fn sidedef_bytes(out: &mut Vec<u8>, sector: i16) {
    out.extend_from_slice(&0i16.to_le_bytes()); // texture offset
    out.extend_from_slice(&0i16.to_le_bytes()); // row offset
    out.extend_from_slice(b"-\0\0\0\0\0\0\0"); // top: none
    out.extend_from_slice(b"-\0\0\0\0\0\0\0"); // bottom: none
    out.extend_from_slice(b"WALL1\0\0\0"); // mid: WALL1
    out.extend_from_slice(&sector.to_le_bytes());
}

/// Sidedef layout per scenario. One entry per line, `None` side = -1.
fn sidedefs_lump(scenario: MapScenario) -> Vec<u8> {
    let mut out = Vec::new();
    match scenario {
        MapScenario::Plain => {
            for sd_sector in [0i16, 0, 0, 0] {
                sidedef_bytes(&mut out, sd_sector);
            }
        }
        MapScenario::Exit => {
            for sd_sector in [0i16, 0, 0, 0, 0] {
                sidedef_bytes(&mut out, sd_sector);
            }
        }
        MapScenario::Door => {
            // indices:
            // 0..=6   one-sided lines L0..L6 (sector 0/2 as listed)
            // 7,8     the door line L7 (front west room / back corridor)
            // 9       one-sided line L8
            // 10      one-sided line L9
            // 11,12   the east door face L10 (front east room / back corridor)
            // 13      one-sided line L11
            let sectors: &[i16] = &[
                0, 2, 2, 2, 0, 0, 0, // L0..L6 one-sided
                0, 1, // L7 door: front sector 0, back sector 1
                0, // L8
                2, // L9
                2, 1, // L10 east face: front sector 2, back sector 1
                2, // L11
            ];
            for &sector in sectors {
                sidedef_bytes(&mut out, sector);
            }
        }
    }
    out
}

fn line_geometry(scenario: MapScenario) -> Vec<(u16, u16, i16, i16, i16, i16, i16)> {
    // (v1, v2, flags, special, tag, sidenum0, sidenum1); -1 = no side
    match scenario {
        MapScenario::Plain => vec![
            (1, 0, 1, 0, 0, 0, -1), // south
            (2, 1, 1, 0, 0, 1, -1), // east
            (3, 2, 1, 0, 0, 2, -1), // north
            (0, 3, 1, 0, 0, 3, -1), // west
        ],
        MapScenario::Exit => {
            let mut lines = vec![
                (1, 0, 1, 0, 0, 0, -1),
                (2, 1, 1, 0, 0, 1, -1),
                (3, 2, 1, 0, 0, 2, -1),
                (0, 3, 1, 0, 0, 3, -1),
            ];
            // exit line, special 11; front side faces west, toward the player
            lines.push((5, 4, 1, 11, 0, 4, -1));
            lines
        }
        MapScenario::Door => vec![
            (0, 4, 1, 0, 0, 0, -1),    // L0 south, west room
            (4, 1, 1, 0, 0, 1, -1),    // L1 south, east room
            (1, 2, 1, 0, 0, 2, -1),    // L2 east
            (2, 7, 1, 0, 0, 3, -1),    // L3 north, east room
            (7, 3, 1, 0, 0, 4, -1),    // L4 north, west room
            (3, 0, 1, 0, 0, 5, -1),    // L5 west
            (4, 5, 1, 0, 0, 6, -1),    // L6 west barrier, south of the door
            (6, 5, 4, 1, 1, 7, 8),     // L7 THE DOOR: special 1, tag 1 (higher y first)
            (6, 7, 1, 0, 0, 9, -1),    // L8 west barrier, north of the door
            (8, 9, 1, 0, 0, 10, -1),   // L9 east barrier, south of the corridor
            (9, 10, 4, 0, 0, 11, 12),  // L10 east face of the corridor
            (10, 11, 1, 0, 0, 13, -1), // L11 east barrier, north of the corridor
        ],
    }
}

fn linedefs_lump(scenario: MapScenario) -> Vec<u8> {
    let lines = line_geometry(scenario);
    let mut out = Vec::new();
    for (v1, v2, flags, special, tag, s0, s1) in lines {
        out.extend_from_slice(&(v1 as i16).to_le_bytes());
        out.extend_from_slice(&(v2 as i16).to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&special.to_le_bytes());
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&s0.to_le_bytes());
        out.extend_from_slice(&s1.to_le_bytes());
    }
    out
}

fn sectors_lump(scenario: MapScenario) -> Vec<u8> {
    let push_sector = |out: &mut Vec<u8>,
                       floor: i16,
                       ceil: i16,
                       floorpic: &[u8; 8],
                       ceilpic: &[u8; 8],
                       light: i16,
                       special: i16,
                       tag: i16| {
        out.extend_from_slice(&floor.to_le_bytes());
        out.extend_from_slice(&ceil.to_le_bytes());
        out.extend_from_slice(floorpic);
        out.extend_from_slice(ceilpic);
        out.extend_from_slice(&light.to_le_bytes());
        out.extend_from_slice(&special.to_le_bytes());
        out.extend_from_slice(&tag.to_le_bytes());
    };
    let floor = *b"FLOOR4_8";
    let ceil = *b"CEIL3_5\0";
    let mut out = Vec::new();
    match scenario {
        MapScenario::Plain | MapScenario::Exit => {
            push_sector(&mut out, 0, 128, &floor, &ceil, 176, 0, 0);
        }
        MapScenario::Door => {
            push_sector(&mut out, 0, 128, &floor, &ceil, 176, 0, 0); // west room
            push_sector(&mut out, 0, 0, &floor, &ceil, 176, 0, 1); // the corridor (door)
            push_sector(&mut out, 0, 128, &floor, &ceil, 176, 0, 0); // east room
        }
    }
    out
}

fn subsectors_lump(scenario: MapScenario) -> Vec<u8> {
    let num_segs = match scenario {
        MapScenario::Plain => 4u16,
        MapScenario::Exit => 5,
        MapScenario::Door => 12,
    };
    let mut out = Vec::new();
    out.extend_from_slice(&num_segs.to_le_bytes());
    out.extend_from_slice(&0i16.to_le_bytes()); // first seg
    out
}

fn things_lump(monster: bool) -> Vec<u8> {
    let mut things: Vec<(i16, i16, i16, i16, i16)> = vec![
        (128, 128, 0, 1, 7), // player 1, angle 0
        (160, 128, 0, 2, 7), // player 2 start (net games spawn both)
    ];
    if monster {
        things.push((192, 128, 0, 3004, 7)); // MT_POSSESSED
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

/// Seg geometry per scenario. The renderer expects the front of a
/// one-sided seg to face the room it bounds (interior on the left of
/// the seg direction). The Plain/Exit linedefs walk the room outline
/// backwards (the fixture's own convention), so there the seg flips
/// the linedef; the Door linedefs already walk each room's boundary
/// the right way, so there the seg keeps the line direction.
///
/// The seg angle lumps carry the seg's direction as a BAM (the
/// renderer keys its texture projection off it); they are derived
/// from the vertex table, so they always match the geometry.
fn seg_geometry(scenario: MapScenario) -> Vec<(u16, u16, i16, u16)> {
    let vertexes = vertex_coords(scenario);
    line_geometry(scenario)
        .iter()
        .enumerate()
        .map(|(index, &(v1, v2, ..))| {
            let (x1, y1) = vertexes[v1 as usize];
            let (x2, y2) = vertexes[v2 as usize];
            let x1 = doom_engine::fixed::Fixed::from_int(i32::from(x1));
            let y1 = doom_engine::fixed::Fixed::from_int(i32::from(y1));
            let x2 = doom_engine::fixed::Fixed::from_int(i32::from(x2));
            let y2 = doom_engine::fixed::Fixed::from_int(i32::from(y2));
            let raw = doom_engine::bsp::point_to_angle_2(x1, y1, x2, y2).to_raw();
            let bam_i16 = ((raw >> 16) & 0xffff) as u16 as i16;
            match scenario {
                MapScenario::Door => (v1, v2, bam_i16, index as u16),
                MapScenario::Plain | MapScenario::Exit => {
                    (v2, v1, bam_i16.wrapping_add(-32768), index as u16)
                }
            }
        })
        .collect()
}

fn vertex_coords(scenario: MapScenario) -> Vec<(i16, i16)> {
    match scenario {
        MapScenario::Plain => vec![(0, 0), (256, 0), (256, 256), (0, 256)],
        MapScenario::Exit => vec![
            (0, 0),
            (256, 0),
            (256, 256),
            (0, 256),
            (160, 64),
            (160, 192),
        ],
        MapScenario::Door => vec![
            (0, 0),
            (256, 0),
            (256, 256),
            (0, 256),
            (160, 0),
            (160, 96),
            (160, 160),
            (160, 256),
            (192, 0),
            (192, 96),
            (192, 160),
            (192, 256),
        ],
    }
}

fn segs_lump_geometry(scenario: MapScenario) -> Vec<u8> {
    let mut out = Vec::new();
    for (v1, v2, angle, linedef) in seg_geometry(scenario) {
        out.extend_from_slice(&(v1 as i16).to_le_bytes());
        out.extend_from_slice(&(v2 as i16).to_le_bytes());
        out.extend_from_slice(&angle.to_le_bytes());
        out.extend_from_slice(&(linedef as i16).to_le_bytes());
        out.extend_from_slice(&0i16.to_le_bytes()); // side
        out.extend_from_slice(&0i16.to_le_bytes()); // offset
    }
    out
}

/// REJECT needs ceil(n_sectors^2 / 8) bytes.
fn reject_lump(scenario: MapScenario) -> Vec<u8> {
    let sectors: usize = match scenario {
        MapScenario::Plain | MapScenario::Exit => 1,
        MapScenario::Door => 3,
    };
    let bits = sectors * sectors;
    vec![0x00; bits.div_ceil(8)]
}

fn map_lumps(marker: &str, scenario: MapScenario, monster: bool) -> Vec<(String, Vec<u8>)> {
    let num_lines = match scenario {
        MapScenario::Plain => 4u16,
        MapScenario::Exit => 5,
        MapScenario::Door => 12,
    };
    vec![
        (marker.to_string(), Vec::new()),
        ("THINGS".to_string(), things_lump(monster)),
        ("LINEDEFS".to_string(), linedefs_lump(scenario)),
        ("SIDEDEFS".to_string(), sidedefs_lump(scenario)),
        ("VERTEXES".to_string(), vertexes_lump(scenario)),
        ("SEGS".to_string(), segs_lump_geometry(scenario)),
        ("SSECTORS".to_string(), subsectors_lump(scenario)),
        ("NODES".to_string(), Vec::new()),
        ("SECTORS".to_string(), sectors_lump(scenario)),
        ("REJECT".to_string(), reject_lump(scenario)),
        ("BLOCKMAP".to_string(), blockmap_lump(num_lines)),
    ]
}

// ---------------------------------------------------------------------------
// the synthetic IWAD
// ---------------------------------------------------------------------------

/// Layout of the synthetic archive; only the magic differs from a PWAD.
fn wad_bytes(lumps: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"IWAD");
    out.extend_from_slice(&(lumps.len() as i32).to_le_bytes());
    let dir_offset_pos = out.len();
    out.extend_from_slice(&0i32.to_le_bytes()); // patched below

    let mut dir = Vec::with_capacity(lumps.len() * 16);
    for (name, data) in lumps {
        let file_pos = out.len() as i32;
        let size = data.len() as i32;
        out.extend_from_slice(data);
        let mut name8 = [0u8; 8];
        let bytes = name.as_bytes();
        let len = bytes.len().min(8);
        name8[..len].copy_from_slice(&bytes[..len]);
        dir.extend_from_slice(&file_pos.to_le_bytes());
        dir.extend_from_slice(&size.to_le_bytes());
        dir.extend_from_slice(&name8);
    }
    let dir_offset = out.len() as i32;
    out.extend_from_slice(&dir);
    out[dir_offset_pos..dir_offset_pos + 4].copy_from_slice(&dir_offset.to_le_bytes());
    out
}

/// Build the fixture IWAD for `spec`: intermission art included so the
/// exit flow can pass through `GS_INTERMISSION`.
pub fn fixture_wad(spec: &FixtureSpec) -> Vec<u8> {
    let mut lumps: Vec<(String, Vec<u8>)> = vec![
        ("COLORMAP".to_string(), colormap_lump()),
        ("PLAYPAL".to_string(), playpal_bytes()),
        ("PNAMES".to_string(), pnames_lump()),
        ("TEXTURE1".to_string(), texture1_lump()),
    ];
    lumps.push(("DSPISTOL".to_string(), dmx_sound()));
    lumps.push(("WALL1".to_string(), wall1_patch()));

    // flats namespace
    lumps.push(("F_START".to_string(), Vec::new()));
    lumps.push(("F_SKY1".to_string(), flat_lump(0x20)));
    lumps.push(("FLOOR4_8".to_string(), flat_lump(0x10)));
    lumps.push(("CEIL3_5".to_string(), flat_lump(0x30)));
    lumps.push(("F_END".to_string(), Vec::new()));

    // empty sprites namespace: no sprite lumps needed for these scenarios
    lumps.push(("S_START".to_string(), Vec::new()));
    lumps.push(("S_END".to_string(), Vec::new()));

    // HUD font (STCFN033..=STCFN095)
    for i in 33..=95u8 {
        lumps.push((format!("STCFN{i:03}"), dummy_patch(4, 6, 0x42)));
    }

    // status bar patches
    lumps.push(("STBAR".to_string(), dummy_patch(320, 32, 0x42)));
    lumps.push(("STARMS".to_string(), dummy_patch(104, 32, 0x42)));
    for i in 0..4 {
        lumps.push((format!("STFB{i}"), dummy_patch(24, 32, 0x42)));
    }
    for i in 0..10 {
        lumps.push((format!("STTNUM{i}"), dummy_patch(16, 8, 0x42)));
        lumps.push((format!("STYSNUM{i}"), dummy_patch(16, 8, 0x42)));
    }
    lumps.push(("STTPRCNT".to_string(), dummy_patch(8, 8, 0x42)));
    lumps.push(("STTMINUS".to_string(), dummy_patch(8, 8, 0x42)));
    for i in 0..6 {
        lumps.push((format!("STKEYS{i}"), dummy_patch(8, 5, 0x42)));
    }
    for i in 2..=7 {
        lumps.push((format!("STGNUM{i}"), dummy_patch(8, 8, 0x42)));
    }
    for pain in 0..5 {
        for straight in 0..3 {
            lumps.push((format!("STFST{pain}{straight}"), dummy_patch(24, 32, 0x42)));
        }
        lumps.push((format!("STFTR{pain}0"), dummy_patch(24, 32, 0x42)));
        lumps.push((format!("STFTL{pain}0"), dummy_patch(24, 32, 0x42)));
        lumps.push((format!("STFOUCH{pain}"), dummy_patch(24, 32, 0x42)));
        lumps.push((format!("STFEVL{pain}"), dummy_patch(24, 32, 0x42)));
        lumps.push((format!("STFKILL{pain}"), dummy_patch(24, 32, 0x42)));
    }
    lumps.push(("STFGOD0".to_string(), dummy_patch(24, 32, 0x42)));
    lumps.push(("STFDEAD0".to_string(), dummy_patch(24, 32, 0x42)));

    // menu patches
    for name in [
        "M_DOOM", "M_NEWG", "M_SKILL", "M_EPISOD", "M_OPTTTL", "M_NGAME", "M_OPTION", "M_LOADG",
        "M_SAVEG", "M_RDTHIS", "M_QUITG", "M_EPI1", "M_EPI2", "M_EPI3", "M_EPI4", "M_JKILL",
        "M_ROUGH", "M_HURT", "M_ULTRA", "M_NMARE", "M_ENDGAM", "M_MESSG", "M_DETAIL", "M_SCRNSZ",
        "M_MSENS", "M_SVOL", "M_SFXVOL", "M_MUSVOL", "M_GDHIGH", "M_GDLOW", "M_MSGOFF", "M_MSGON",
        "HELP", "HELP1", "HELP2", "CREDIT", "M_LSLEFT", "M_LSCNTR", "M_LSRGHT", "M_THERML",
        "M_THERMM", "M_THERMR", "M_THERMO", "M_SKULL1", "M_SKULL2",
    ] {
        lumps.push((name.to_string(), dummy_patch(32, 16, 0x42)));
    }

    // title screen + pause patch (GS_DEMOSCREEN)
    lumps.push(("TITLEPIC".to_string(), dummy_patch(320, 200, 0x42)));
    lumps.push(("M_PAUSE".to_string(), dummy_patch(80, 16, 0x42)));

    // intermission art: background, level names, you-are-here, digits,
    // player numbers, the minus, the percent sign, and the episode-0
    // (E1) animations. Registered-game E1 maps use the episode-0
    // table (WIMAP0 / WILV00.. / WIA00000..); episode 1 in the setup
    // is 0-based there. Ten anims x three frames.
    lumps.push(("WIMAP0".to_string(), dummy_patch(320, 200, 0x11)));
    for i in 0..9 {
        lumps.push((format!("WILV0{i}"), dummy_patch(32, 16, 0x22)));
    }
    lumps.push(("WIURH0".to_string(), dummy_patch(16, 16, 0x33)));
    lumps.push(("WIURH1".to_string(), dummy_patch(16, 16, 0x33)));
    lumps.push(("WISPLAT".to_string(), dummy_patch(16, 16, 0x33)));
    for i in 0..10 {
        lumps.push((format!("WINUM{i}"), dummy_patch(16, 8, 0x44)));
    }
    lumps.push(("WIMINUS".to_string(), dummy_patch(8, 8, 0x44)));
    lumps.push(("WIPCNT".to_string(), dummy_patch(8, 8, 0x44)));
    lumps.push(("WICOLON".to_string(), dummy_patch(8, 8, 0x44)));
    lumps.push(("WIF".to_string(), dummy_patch(8, 8, 0x44)));
    lumps.push(("WIENTER".to_string(), dummy_patch(32, 8, 0x44)));
    lumps.push(("WIOSTK".to_string(), dummy_patch(64, 8, 0x44)));
    lumps.push(("WIOSTS".to_string(), dummy_patch(64, 8, 0x44)));
    lumps.push(("WISCRT2".to_string(), dummy_patch(64, 8, 0x44)));
    lumps.push(("WIOSTI".to_string(), dummy_patch(64, 8, 0x44)));
    lumps.push(("WIFRGS".to_string(), dummy_patch(64, 8, 0x44)));
    lumps.push(("WITIME".to_string(), dummy_patch(64, 8, 0x44)));
    lumps.push(("WISUCKS".to_string(), dummy_patch(64, 8, 0x44)));
    lumps.push(("WIPAR".to_string(), dummy_patch(64, 8, 0x44)));
    lumps.push(("WIKILRS".to_string(), dummy_patch(64, 8, 0x44)));
    lumps.push(("WIVCTMS".to_string(), dummy_patch(64, 8, 0x44)));
    lumps.push(("WIMSTT".to_string(), dummy_patch(64, 8, 0x44)));
    for i in 0..4 {
        lumps.push((format!("STPB{i}"), dummy_patch(16, 16, 0x55)));
        lumps.push((format!("WIBP{}", i + 1), dummy_patch(16, 8, 0x55)));
    }
    for j in 0..10 {
        for i in 0..3 {
            lumps.push((format!("WIA0{j:02}{i:02}"), dummy_patch(8, 8, 0x66)));
        }
    }

    // maps: E1M1 is the scenario, E1M2 is always plain (no exit spec)
    lumps.extend(map_lumps("E1M1", spec.scenario, spec.monster));
    lumps.extend(map_lumps("E1M2", MapScenario::Plain, false));

    wad_bytes(&lumps)
}

// ---------------------------------------------------------------------------
// running the game
// ---------------------------------------------------------------------------

static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique temp directory; removed on drop (and best-effort on process
/// exit if the test panics, since the path is unique).
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "doom-rs-it-{}-{}-{nanos}-{}",
            std::process::id(),
            tag,
            UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("scratch dir creatable");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub fn new_game_args() -> EngineArgs {
    EngineArgs {
        skill: Some(1),
        ..Default::default()
    }
}

pub fn test_config(save_dir: &Path) -> GameConfig {
    GameConfig {
        game_mode: GameMode::Registered,
        save_dir: save_dir.to_path_buf(),
        ..GameConfig::default()
    }
}

/// Build a game from the fixture IWAD parked in `save_dir`.
pub fn start_game(save_dir: &Path, args: &EngineArgs, spec: &FixtureSpec) -> Game {
    let wad = WadFile::from_bytes(fixture_wad(spec)).expect("synthetic IWAD parses");
    let resources = doom_engine::resources::ResourceManager::new(wad, Vec::new());
    Game::new(test_config(save_dir), resources, args).expect("game builds from the synthetic WAD")
}

pub fn run_tics(game: &mut Game, input: &TicCmdInput, tics: usize, fb: &mut IndexedFramebuffer) {
    for _ in 0..tics {
        game.tick_input(input, fb).expect("tic runs");
    }
}

/// Where the snapshot/compat frames land: `<workspace>/target/frames`.
pub fn frames_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        let dir = PathBuf::from(dir);
        if dir.is_absolute() {
            return dir.join("frames");
        }
    }
    // integration tests run with CWD = the package root; the workspace
    // root is its parent
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap_or(&manifest).to_path_buf();
    root.join("target").join("frames")
}
