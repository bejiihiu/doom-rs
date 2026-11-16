//! `doom-asset-inspector` — dumps DOOM engine graphics assets to images.
//!
//! Every command decodes through `doom-engine`'s WAD and gfx APIs; this
//! binary only does presentation on top of them.
//!
//! Exit codes (every command): 2 = fatal error, 1 = finished with
//! warnings, 0 = clean success.

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod assets;
mod image;

/// Dumps DOOM engine graphics assets (PLAYPAL palettes, COLORMAP tables,
/// patches, textures, flats and sky textures) to PNG files.
///
/// Images are rendered through PLAYPAL palette 0 unless a command says
/// otherwise. Patch and sprite pixels that no post covers stay
/// transparent (alpha 0).
#[derive(Parser)]
#[command(name = "doom-asset-inspector", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the palettes in PLAYPAL: color count, distinct colors and the
    /// RGB min/max range per channel.
    ///
    /// With `--full`, the complete RGB table for every palette is printed.
    /// With `--out <prefix>`, each palette is written as a 512x512 PNG
    /// grid of 16x16 cells (32x32 pixels each) to `<prefix>-<index>.png`.
    ///
    /// Exit codes: 0 = ok, 2 = fatal error.
    Palette {
        /// WAD file to read from.
        file: PathBuf,
        /// Print the full per-color RGB table for every palette.
        #[arg(long)]
        full: bool,
        /// Output path prefix; one PNG per palette is written to
        /// `<prefix>-<index>.png`.
        #[arg(long, value_name = "PREFIX")]
        out: Option<PathBuf>,
    },
    /// Print one PLAYPAL palette as a 16x16 grid of hex colors, or write
    /// it as a 512x512 PNG with `--out` (cell i at column i % 16, row
    /// i / 16, each cell 32x32 pixels).
    ///
    /// Exit codes: 0 = ok, 2 = fatal error.
    PaletteColors {
        /// WAD file to read from.
        file: PathBuf,
        /// Palette index (0-based; vanilla WADs ship 14 palettes).
        palette_index: usize,
        /// Output PNG path. Without it, the grid is printed as text.
        #[arg(long, value_name = "PNG")]
        out: Option<PathBuf>,
    },
    /// COLORMAP tables as a report, or as one stacked PNG with `--out`:
    /// every map one 256x16 band, glued vertically (256x528 for the
    /// vanilla 33 maps). Each texel reads PLAYPAL palette 0 through the
    /// colormap, so the bands show the diminishing brightness levels and
    /// the invulnerability (inverse) map.
    ///
    /// Exit codes: 0 = ok, 2 = fatal error.
    Colormap {
        /// WAD file to read from.
        file: PathBuf,
        /// Output PNG path for the stacked image.
        #[arg(long, value_name = "PNG")]
        out: Option<PathBuf>,
    },
    /// Export a patch (sprites, weapon frames, status bar graphics,
    /// texture building blocks) as a PNG. Pixels no post covers are
    /// transparent.
    ///
    /// Without `--out`, only patch info is printed (size, offsets,
    /// columns, posts, pixels).
    ///
    /// Exit codes: 0 = ok, 1 = finished with warnings, 2 = fatal error.
    Patch {
        /// WAD file to read from.
        file: PathBuf,
        /// Patch lump name, case-insensitive.
        name: String,
        /// Output PNG path.
        #[arg(long, value_name = "PNG")]
        out: Option<PathBuf>,
    },
    /// Alias of `patch` — sprites are patches stored in the sprite
    /// namespace.
    ///
    /// Exit codes: 0 = ok, 1 = finished with warnings, 2 = fatal error.
    Sprite {
        /// WAD file to read from.
        file: PathBuf,
        /// Sprite lump name, case-insensitive (e.g. TROOA1).
        name: String,
        /// Output PNG path.
        #[arg(long, value_name = "PNG")]
        out: Option<PathBuf>,
    },
    /// Decode a TEXTURE1/TEXTURE2 composite (the patches referenced by
    /// PNAMES, drawn back to front) and show it through PLAYPAL palette
    /// 0. Texels no patch covers are transparent for masked textures.
    ///
    /// Without `--out`, only texture info is printed (size, masked flag,
    /// patch list).
    ///
    /// Exit codes: 0 = ok, 2 = fatal error.
    Texture {
        /// WAD file to read from.
        file: PathBuf,
        /// Texture name, case-insensitive.
        name: String,
        /// Output PNG path.
        #[arg(long, value_name = "PNG")]
        out: Option<PathBuf>,
    },
    /// Export a flat (64x64 floor/ceiling texture) as a PNG through
    /// PLAYPAL palette 0.
    ///
    /// Without `--out`, only flat info is printed.
    ///
    /// Exit codes: 0 = ok, 2 = fatal error.
    Flat {
        /// WAD file to read from.
        file: PathBuf,
        /// Flat lump name, case-insensitive.
        name: String,
        /// Output PNG path.
        #[arg(long, value_name = "PNG")]
        out: Option<PathBuf>,
    },
    /// List the flats in the F_START..F_END namespace with their sizes
    /// and validate that every one of them is exactly 4096 bytes.
    ///
    /// Exit codes: 0 = all flats valid, 1 = warnings (invalid flat, or
    /// no namespace markers found), 2 = fatal error.
    Flats {
        /// WAD file to inspect.
        file: PathBuf,
    },
    /// Find the sky textures (TEXTURE1/TEXTURE2 entries whose name
    /// starts with SKY), print what was found and, with `--out`, write
    /// each of them as a PNG through PLAYPAL palette 0.
    ///
    /// When exactly one sky texture is found, `--out` is used as the
    /// output path; with several, one file per texture is written to
    /// `<out>-<NAME>.png` (a trailing `.png` in `--out` is stripped
    /// first).
    ///
    /// Exit codes: 0 = ok, 1 = no sky textures found, 2 = fatal error.
    Sky {
        /// WAD file to read from.
        file: PathBuf,
        /// Output PNG path (single sky) or prefix (multiple skies).
        #[arg(long, value_name = "PNG")]
        out: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let result = match cli.command {
        Command::Palette {
            file,
            full,
            out: dest,
        } => assets::cmd_palette(&mut out, &file, full, dest.as_deref()),
        Command::PaletteColors {
            file,
            palette_index,
            out: dest,
        } => assets::cmd_palette_colors(&mut out, &file, palette_index, dest.as_deref()),
        Command::Colormap { file, out: dest } => {
            assets::cmd_colormap(&mut out, &file, dest.as_deref())
        }
        Command::Patch {
            file,
            name,
            out: dest,
        } => assets::cmd_patch(&mut out, &file, &name, dest.as_deref()),
        Command::Sprite {
            file,
            name,
            out: dest,
        } => assets::cmd_sprite(&mut out, &file, &name, dest.as_deref()),
        Command::Texture {
            file,
            name,
            out: dest,
        } => assets::cmd_texture(&mut out, &file, &name, dest.as_deref()),
        Command::Flat {
            file,
            name,
            out: dest,
        } => assets::cmd_flat(&mut out, &file, &name, dest.as_deref()),
        Command::Flats { file } => assets::cmd_flats(&mut out, &file),
        Command::Sky { file, out: dest } => assets::cmd_sky(&mut out, &file, dest.as_deref()),
    };

    match result {
        Ok(code) => ExitCode::from(code),
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::from(2)
        }
    }
}
