//! Headless framebuffer snapshots.
//!
//! Renders one fixed scenario (the door fixture map, whose west barrier
//! faces the player spawn) for a fixed number of tics and writes the
//! frame to `target/frames/` as a PNG. The test validates the PNG it
//! wrote - signature, IHDR dimensions, color type - and reads it back
//! with the same decoder to prove the pixels round-trip. Nothing here
//! compares against a golden reference; it exists so a human can eyeball
//! what the engine drew, and CI can assert the file is well-formed.

mod common;

use common::{
    FixtureSpec, MapScenario, TempDir, frames_dir, new_game_args, playpal_bytes, run_tics,
    start_game,
};
use doom_engine::game::TicCmdInput;
use doom_engine::render::{IndexedFramebuffer, SCREEN_HEIGHT, SCREEN_WIDTH};

fn encode_png_rgba(
    path: &std::path::Path,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(std::io::Error::other)?;
    writer.write_image_data(rgba).map_err(std::io::Error::other)
}

fn framebuffer_rgba(fb: &IndexedFramebuffer) -> Vec<u8> {
    let playpal =
        doom_engine::gfx::palette::parse_playpal(&playpal_bytes()).expect("fixture PLAYPAL parses");
    let palette = &playpal[0];
    let mut rgba = Vec::with_capacity(SCREEN_WIDTH * SCREEN_HEIGHT * 4);
    for y in 0..SCREEN_HEIGHT {
        let row = fb.row(y).expect("row in range");
        for &index in row {
            let [r, g, b] = palette.color(index);
            rgba.extend_from_slice(&[r, g, b, 0xff]);
        }
    }
    rgba
}

fn parse_ihdr(bytes: &[u8]) -> (u32, u32, u8, u8) {
    assert_eq!(
        &bytes[..8],
        &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
        "PNG signature"
    );
    assert_eq!(&bytes[12..16], b"IHDR", "first chunk must be IHDR");
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    (width, height, bytes[24], bytes[25])
}

#[test]
fn render_frame_and_write_png_snapshot() {
    let dir = TempDir::new("snapshot");
    let spec = FixtureSpec {
        scenario: MapScenario::Door,
        monster: true,
    };
    let mut game = start_game(dir.path(), &new_game_args(), &spec);
    let mut fb = IndexedFramebuffer::new();

    // boot the level and let the opening melt finish so the frame is the
    // live view, not the transition
    run_tics(&mut game, &TicCmdInput::NONE, 150, &mut fb);
    run_tics(
        &mut game,
        &TicCmdInput {
            attack: true,
            ..TicCmdInput::NONE
        },
        10,
        &mut fb,
    );

    // the west barrier fills the middle of the frame: plenty of distinct
    // palette indices, and not all of them the clear color
    let mut distinct = std::collections::HashSet::new();
    for y in 0..SCREEN_HEIGHT {
        for &px in fb.row(y).expect("row in range") {
            distinct.insert(px);
        }
    }
    assert!(
        distinct.len() > 8,
        "expected a non-trivial frame, got {} distinct palette indices",
        distinct.len()
    );

    let frames = frames_dir();
    std::fs::create_dir_all(&frames).expect("frames dir creatable");
    let path = frames.join("snapshot-door-room.png");

    let rgba = framebuffer_rgba(&fb);
    encode_png_rgba(&path, &rgba, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32).expect("PNG encodes");

    let bytes = std::fs::read(&path).expect("snapshot was written");
    let (width, height, depth, color_type) = parse_ihdr(&bytes);
    assert_eq!((width, height), (SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32));
    assert_eq!(depth, 8, "bit depth must be 8");
    assert_eq!(color_type, 6, "color type must be truecolor with alpha");
    assert!(bytes.len() > 33, "PNG too small: {} bytes", bytes.len());

    // read it back with the same decoder and compare the pixels
    let decoder = png::Decoder::new(std::io::BufReader::new(std::io::Cursor::new(&bytes[..])));
    let mut reader = decoder.read_info().expect("png info readable");
    let mut buf = vec![0; reader.output_buffer_size().expect("buffer size known")];
    let info = reader.next_frame(&mut buf).expect("frame decodes");
    assert_eq!(info.width, SCREEN_WIDTH as u32);
    assert_eq!(info.height, SCREEN_HEIGHT as u32);
    assert_eq!(
        &buf[..rgba.len()],
        rgba.as_slice(),
        "PNG round-trips the pixels"
    );

    eprintln!("snapshot written to {}", path.display());
}
