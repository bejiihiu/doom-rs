//! Command implementations for `doom-asset-inspector`.
//!
//! Everything here builds on `doom-engine`'s WAD and gfx APIs; this module
//! only selects, decodes and presents. A WAD that fails to parse, or a
//! lump the command needs that fails to decode, is a fatal error for the
//! command (exit code 2). Non-fatal oddities are reported as warnings
//! and surface as exit code 1.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use doom_engine::gfx::flat::Flat;
use doom_engine::gfx::palette::{
    self, COLORMAP_BYTES, Colormap, Palette, parse_colormap, parse_playpal,
};
use doom_engine::gfx::patch::{Patch, PatchColumn, decode_patch};
use doom_engine::gfx::pnames::Pnames;
use doom_engine::gfx::texture::{Texture, parse_texture_lump};
use doom_engine::render::build_texture_pixels;
use doom_engine::wad::WadSet;

use crate::image::{RgbaImage, opaque, write_png};

/// Exit codes shared by every command: 2 = fatal error, 1 = warnings,
/// 0 = clean success.
pub(crate) const EXIT_OK: u8 = 0;
pub(crate) const EXIT_WARNINGS: u8 = 1;

/// Opens a single WAD into a one-file set, mapping any failure to a
/// fatal command error.
pub(crate) fn load(file: &Path) -> Result<WadSet, String> {
    WadSet::from_paths([file.to_path_buf()]).map_err(|e| e.to_string())
}

/// Fetches the raw bytes of a named lump.
fn lump_named<'a>(wad: &'a WadSet, name: &str) -> Result<&'a [u8], String> {
    wad.lump_data_named(name)
        .map_err(|e| format!("lump {name}: {e}"))
}

/// Decodes the PLAYPAL lump (every palette in it).
fn load_palettes(wad: &WadSet) -> Result<Vec<Palette>, String> {
    let data = lump_named(wad, "PLAYPAL")?;
    parse_playpal(data).map_err(|e| format!("lump PLAYPAL: {e}"))
}

/// The first PLAYPAL palette — the one the engine draws patches,
/// textures, flats and sprites through.
fn first_palette(wad: &WadSet) -> Result<Palette, String> {
    Ok(load_palettes(wad)?
        .into_iter()
        .next()
        .expect("parse_playpal guarantees at least one palette"))
}

/// Decodes every COLORMAP table in the lump.
fn load_colormaps(wad: &WadSet) -> Result<Vec<Colormap>, String> {
    let data = lump_named(wad, "COLORMAP")?;
    parse_colormap(data).map_err(|e| format!("lump COLORMAP: {e}"))
}

/// Writes a small aligned table: two-space separated columns, a dashed
/// separator under the header, right-aligned numeric columns.
fn write_table(
    w: &mut dyn Write,
    headers: &[&str],
    rows: &[Vec<String>],
    right_aligned: &[usize],
) -> io::Result<()> {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }

    let render_row = |cells: &[String]| -> String {
        let mut line = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i > 0 {
                line.push_str("  ");
            }
            if right_aligned.contains(&i) {
                let _ = write!(line, "{:>width$}", cell, width = widths[i]);
            } else {
                let _ = write!(line, "{:<width$}", cell, width = widths[i]);
            }
        }
        line.trim_end().to_string()
    };

    let header_cells: Vec<String> = headers.iter().map(|h| h.to_string()).collect();
    writeln!(w, "{}", render_row(&header_cells))?;
    let separator: Vec<String> = widths.iter().map(|&n| "-".repeat(n)).collect();
    writeln!(w, "{}", separator.join("  "))?;
    for row in rows {
        writeln!(w, "{}", render_row(row))?;
    }
    Ok(())
}

fn hex_color(color: &[u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
}

/// Renders one palette as a 512x512 grid of 16x16 cells, each cell 32x32
/// pixels, index `i` at column `i % 16`, row `i / 16`.
pub(crate) fn palette_grid_image(palette: &Palette) -> RgbaImage {
    const CELL: u32 = 32;
    const GRID: u32 = 16;
    let mut image = RgbaImage::new(CELL * GRID, CELL * GRID);
    for (index, color) in palette.colors.iter().enumerate() {
        let col = (index % GRID as usize) as u32;
        let row = (index / GRID as usize) as u32;
        image.fill_rect(col * CELL, row * CELL, CELL, CELL, opaque(*color));
    }
    image
}

/// The stacked colormap image: every map one 256x16 band, glued
/// vertically, band `i` at `y = 16 * i`. Texels read the palette through
/// the colormap, so each band shows that brightness level's remap of the
/// identity gradient.
pub(crate) fn colormap_stack_image(colormaps: &[Colormap], palette: &Palette) -> RgbaImage {
    const BAND: u32 = 16;
    let width = COLORMAP_BYTES as u32;
    let mut image = RgbaImage::new(width, BAND * colormaps.len() as u32);
    for (index, map) in colormaps.iter().enumerate() {
        let y = BAND * index as u32;
        for x in 0..width {
            let mapped = map.lookup(x as u8);
            image.fill_rect(x, y, 1, BAND, opaque(palette.color(mapped)));
        }
    }
    image
}

/// Renders a decoded patch into an RGBA image. Pixels outside the
/// patch's `height` (legal for tall patches) are dropped; the number of
/// dropped pixels is returned so the caller can warn.
pub(crate) fn patch_to_image(patch: &Patch, palette: &Palette) -> (RgbaImage, u64) {
    let width = patch.header.width as u32;
    let height = patch.header.height as u32;
    let mut image = RgbaImage::new(width, height);
    let mut clipped = 0u64;
    for pixel in patch.pixels() {
        if pixel.row >= patch.header.height as usize {
            clipped += 1;
            continue;
        }
        image.set_pixel(
            pixel.column as u32,
            pixel.row as u32,
            opaque(palette.color(pixel.value)),
        );
    }
    (image, clipped)
}

/// Path of one file of a multi-file `--out` export: `prefix-NAME.png`.
///
/// `prefix` is the raw `--out` value; a trailing `.png` is stripped
/// first so optional extensions behave.
fn numbered_out_path(prefix: &Path, name: &str) -> PathBuf {
    let prefix = prefix.as_os_str().to_string_lossy();
    let stem = prefix.strip_suffix(".png").unwrap_or(&prefix);
    PathBuf::from(format!("{stem}-{name}.png"))
}

// ------------------------------------------------------------- palette ----

pub(crate) fn cmd_palette(
    w: &mut dyn Write,
    file: &Path,
    full: bool,
    out: Option<&Path>,
) -> Result<u8, String> {
    let wad = load(file)?;
    let palettes = load_palettes(&wad)?;
    writeln!(
        w,
        "PLAYPAL: {} palette(s) in {}",
        palettes.len(),
        file.display()
    )
    .map_err(|e| e.to_string())?;

    let rows: Vec<Vec<String>> = palettes
        .iter()
        .enumerate()
        .map(|(i, palette)| {
            let mut distinct: HashSet<[u8; 3]> = HashSet::with_capacity(256);
            let mut min = [u8::MAX; 3];
            let mut max = [0u8; 3];
            for color in &palette.colors {
                distinct.insert(*color);
                for (c, value) in color.iter().enumerate() {
                    min[c] = min[c].min(*value);
                    max[c] = max[c].max(*value);
                }
            }
            vec![
                i.to_string(),
                "256".to_string(),
                distinct.len().to_string(),
                format!("{}-{}", min[0], max[0]),
                format!("{}-{}", min[1], max[1]),
                format!("{}-{}", min[2], max[2]),
            ]
        })
        .collect();
    write_table(
        w,
        &[
            "index",
            "colors",
            "distinct",
            "r min-max",
            "g min-max",
            "b min-max",
        ],
        &rows,
        &[0, 1, 2],
    )
    .map_err(|e| e.to_string())?;

    if full {
        for (i, palette) in palettes.iter().enumerate() {
            writeln!(w, "palette {i}:").map_err(|e| e.to_string())?;
            for (index, color) in palette.colors.iter().enumerate() {
                writeln!(
                    w,
                    "  {index:>3}  R{:>3} G{:>3} B{:>3}  {}",
                    color[0],
                    color[1],
                    color[2],
                    hex_color(color)
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }

    if let Some(prefix) = out {
        for (i, palette) in palettes.iter().enumerate() {
            let path = numbered_out_path(prefix, &i.to_string());
            let image = palette_grid_image(palette);
            write_png(&path, &image)?;
            writeln!(w, "wrote {}", path.display()).map_err(|e| e.to_string())?;
        }
    }
    Ok(EXIT_OK)
}

// -------------------------------------------------------- palette-colors ----

pub(crate) fn cmd_palette_colors(
    w: &mut dyn Write,
    file: &Path,
    index: usize,
    out: Option<&Path>,
) -> Result<u8, String> {
    let wad = load(file)?;
    let palettes = load_palettes(&wad)?;
    let palette = palettes.get(index).ok_or_else(|| {
        format!(
            "palette index {index} out of range: {} has {} palette(s) (0..{})",
            file.display(),
            palettes.len(),
            palettes.len().saturating_sub(1)
        )
    })?;

    if let Some(path) = out {
        let image = palette_grid_image(palette);
        write_png(path, &image)?;
        writeln!(
            w,
            "wrote {} ({}x{})",
            path.display(),
            image.width(),
            image.height()
        )
        .map_err(|e| e.to_string())?;
    } else {
        // terminal rendering: 16 rows of 16 cells, cell = index + hex
        writeln!(w, "palette {index}: 16x16, index = row * 16 + col").map_err(|e| e.to_string())?;
        for row in 0..16 {
            let mut line = String::new();
            for col in 0..16 {
                let i = row * 16 + col;
                if col > 0 {
                    line.push(' ');
                }
                line.push_str(&hex_color(&palette.colors[i]));
            }
            writeln!(w, "{line}").map_err(|e| e.to_string())?;
        }
    }
    Ok(EXIT_OK)
}

// -------------------------------------------------------------- colormap ----

fn colormap_kind(index: usize) -> String {
    if index < palette::NUM_DIMINISHING_COLORMAPS {
        let fuzz = if index == palette::FUZZ_COLORMAP {
            " (fuzz)"
        } else {
            ""
        };
        format!("diminishing level {index}{fuzz}")
    } else if index == palette::INVULNERABILITY_COLORMAP {
        "invulnerability (inverse)".to_string()
    } else {
        "extra map".to_string()
    }
}

pub(crate) fn cmd_colormap(
    w: &mut dyn Write,
    file: &Path,
    out: Option<&Path>,
) -> Result<u8, String> {
    let wad = load(file)?;
    let colormaps = load_colormaps(&wad)?;
    let palette = first_palette(&wad)?;
    writeln!(
        w,
        "COLORMAP: {} map(s) in {}",
        colormaps.len(),
        file.display()
    )
    .map_err(|e| e.to_string())?;

    let rows: Vec<Vec<String>> = colormaps
        .iter()
        .enumerate()
        .map(|(i, map)| {
            let distinct: HashSet<u8> = map.0.iter().copied().collect();
            vec![i.to_string(), colormap_kind(i), distinct.len().to_string()]
        })
        .collect();
    write_table(w, &["index", "kind", "distinct"], &rows, &[0, 2]).map_err(|e| e.to_string())?;

    if let Some(path) = out {
        let image = colormap_stack_image(&colormaps, &palette);
        write_png(path, &image)?;
        writeln!(
            w,
            "wrote {} ({}x{}, {} band(s) of 256x16)",
            path.display(),
            image.width(),
            image.height(),
            colormaps.len()
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(EXIT_OK)
}

// ---------------------------------------------------------------- patch ----

pub(crate) fn cmd_patch(
    w: &mut dyn Write,
    file: &Path,
    name: &str,
    out: Option<&Path>,
) -> Result<u8, String> {
    let wad = load(file)?;
    let palette = first_palette(&wad)?;
    let data = lump_named(&wad, name)?;
    let patch = decode_patch(data).map_err(|e| format!("lump {name}: {e}"))?;

    let posts: usize = patch.columns.iter().map(PatchColumn::post_count).sum();
    writeln!(
        w,
        "patch {} ({})",
        name.to_ascii_uppercase(),
        file.display()
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  size:        {}x{}",
        patch.header.width, patch.header.height
    )
    .map_err(|e| e.to_string())?;
    writeln!(w, "  left offset: {}", patch.header.left_offset).map_err(|e| e.to_string())?;
    writeln!(w, "  top offset:  {}", patch.header.top_offset).map_err(|e| e.to_string())?;
    writeln!(w, "  columns:     {}", patch.column_count()).map_err(|e| e.to_string())?;
    writeln!(w, "  posts:       {posts}").map_err(|e| e.to_string())?;
    writeln!(w, "  pixels:      {}", patch.pixel_count()).map_err(|e| e.to_string())?;

    let mut warnings = 0u8;
    if let Some(path) = out {
        let (image, clipped) = patch_to_image(&patch, &palette);
        write_png(path, &image)?;
        writeln!(
            w,
            "wrote {} ({}x{})",
            path.display(),
            image.width(),
            image.height()
        )
        .map_err(|e| e.to_string())?;
        if clipped > 0 {
            writeln!(
                w,
                "warning: {clipped} pixel(s) past the {}-pixel height were clipped (tall patch)",
                patch.header.height
            )
            .map_err(|e| e.to_string())?;
            warnings = EXIT_WARNINGS;
        }
    }
    Ok(warnings)
}

// Sprite lumps are patches; `sprite` is an alias of `patch` with a
// better name for the S_START namespace crowd.
pub(crate) fn cmd_sprite(
    w: &mut dyn Write,
    file: &Path,
    name: &str,
    out: Option<&Path>,
) -> Result<u8, String> {
    cmd_patch(w, file, name, out)
}

// -------------------------------------------------------------- texture ----

/// How a texture was found, for the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextureTable {
    Texture1,
    Texture2,
}

impl TextureTable {
    fn label(self) -> &'static str {
        match self {
            TextureTable::Texture1 => "TEXTURE1",
            TextureTable::Texture2 => "TEXTURE2",
        }
    }
}

/// PNAMES plus TEXTURE1 (required) and TEXTURE2 (optional), already
/// decoded and merged into one lookup table.
struct TextureTables {
    pnames: Pnames,
    textures: Vec<(Texture, TextureTable)>,
}

fn load_texture_tables(wad: &WadSet) -> Result<TextureTables, String> {
    let pnames_bytes = lump_named(wad, "PNAMES")?;
    let pnames = Pnames::parse(pnames_bytes).map_err(|e| format!("lump PNAMES: {e}"))?;

    let mut textures: Vec<(Texture, TextureTable)> = Vec::new();
    let texture1 = lump_named(wad, "TEXTURE1")?;
    textures.extend(
        parse_texture_lump(texture1, &pnames)
            .map_err(|e| format!("lump TEXTURE1: {e}"))?
            .into_iter()
            .map(|t| (t, TextureTable::Texture1)),
    );

    if wad.find_lump("TEXTURE2").is_some() {
        let texture2 = lump_named(wad, "TEXTURE2")?;
        textures.extend(
            parse_texture_lump(texture2, &pnames)
                .map_err(|e| format!("lump TEXTURE2: {e}"))?
                .into_iter()
                .map(|t| (t, TextureTable::Texture2)),
        );
    }

    Ok(TextureTables { pnames, textures })
}

/// Looks up a texture by name (case-insensitive, normalized on both
/// sides). The error message lists the available names.
fn find_texture<'a>(
    tables: &'a TextureTables,
    name: &str,
) -> Result<&'a (Texture, TextureTable), String> {
    let wanted = name.to_ascii_uppercase();
    match tables.textures.iter().find(|(t, _)| t.name == wanted) {
        Some(found) => Ok(found),
        None => {
            let available: Vec<&str> = tables
                .textures
                .iter()
                .map(|(t, _)| t.name.as_str())
                .collect();
            let list = if available.len() <= 32 {
                available.join(", ")
            } else {
                format!(
                    "{} (and {} more)",
                    available[..32].join(", "),
                    available.len() - 32
                )
            };
            Err(format!("texture {wanted} not found; available: {list}"))
        }
    }
}

/// Decodes the patch at PNAMES index `index`.
fn decode_patch_named(
    wad: &WadSet,
    pnames: &Pnames,
    index: usize,
    texture_name: &str,
) -> Result<Option<Arc<Patch>>, String> {
    let name = pnames
        .get(index)
        .ok_or_else(|| format!("texture {texture_name}: PNAMES index {index} out of range"))?;
    let data = wad
        .lump_data_named(name)
        .map_err(|e| format!("texture {texture_name}: PNAMES patch {name}: {e}"))?;
    let patch = decode_patch(data)
        .map_err(|e| format!("texture {texture_name}: PNAMES patch {name}: {e}"))?;
    Ok(Some(Arc::new(patch)))
}

/// Composes one texture into an RGBA image: palette index texels
/// through PLAYPAL palette 0. Texels no patch covers are transparent
/// when the texture is masked, opaque palette index 0 otherwise.
fn texture_to_image(
    wad: &WadSet,
    tables: &TextureTables,
    texture: &Texture,
    palette: &Palette,
) -> Result<RgbaImage, String> {
    let needed: HashSet<usize> = texture.patches.iter().map(|p| p.patch as usize).collect();
    let mut patches: Vec<Option<Arc<Patch>>> = Vec::with_capacity(tables.pnames.len());
    for index in 0..tables.pnames.len() {
        if needed.contains(&index) {
            patches.push(decode_patch_named(
                wad,
                &tables.pnames,
                index,
                &texture.name,
            )?);
        } else {
            patches.push(None);
        }
    }

    let composite = build_texture_pixels(texture, &patches)
        .map_err(|e| format!("texture {}: {e}", texture.name))?;

    let width = composite.width() as u32;
    let height = composite.height() as u32;
    let mut image = RgbaImage::new(width, height);
    for col in 0..composite.width() {
        let covered: Vec<bool> = if texture.masked {
            let mut covered = vec![false; composite.height() as usize];
            for run in composite.runs(col) {
                let start = run.top.max(0) as usize;
                let end = (run.top + run.len).min(composite.height()) as usize;
                for row in covered.iter_mut().take(end).skip(start) {
                    *row = true;
                }
            }
            covered
        } else {
            vec![true; composite.height() as usize]
        };

        for (row, &is_covered) in covered.iter().enumerate() {
            let texel = composite.texel(col, row as i32);
            let rgba = if is_covered {
                opaque(palette.color(texel))
            } else {
                [0, 0, 0, 0]
            };
            image.set_pixel(col as u32, row as u32, rgba);
        }
    }
    Ok(image)
}

fn write_texture_info(
    w: &mut dyn Write,
    tables: &TextureTables,
    texture: &Texture,
    table: TextureTable,
) -> io::Result<()> {
    writeln!(w, "texture {} ({})", texture.name, table.label())?;
    writeln!(w, "  size:    {}x{}", texture.width, texture.height)?;
    writeln!(
        w,
        "  masked:  {}",
        if texture.masked { "yes" } else { "no" }
    )?;
    writeln!(w, "  patches: {}", texture.patches.len())?;
    for (i, mappatch) in texture.patches.iter().enumerate() {
        let patch_name = tables.pnames.get(mappatch.patch as usize).unwrap_or("<?>");
        writeln!(
            w,
            "    [{i}] {patch_name} at ({}, {})",
            mappatch.origin_x, mappatch.origin_y
        )?;
    }
    Ok(())
}

pub(crate) fn cmd_texture(
    w: &mut dyn Write,
    file: &Path,
    name: &str,
    out: Option<&Path>,
) -> Result<u8, String> {
    let wad = load(file)?;
    let palette = first_palette(&wad)?;
    let tables = load_texture_tables(&wad)?;
    let (texture, table) = find_texture(&tables, name)?;

    write_texture_info(w, &tables, texture, *table).map_err(|e| e.to_string())?;

    if let Some(path) = out {
        let image = texture_to_image(&wad, &tables, texture, &palette)?;
        write_png(path, &image)?;
        writeln!(
            w,
            "wrote {} ({}x{})",
            path.display(),
            image.width(),
            image.height()
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(EXIT_OK)
}

// ------------------------------------------------------------------ flat ----

pub(crate) fn cmd_flat(
    w: &mut dyn Write,
    file: &Path,
    name: &str,
    out: Option<&Path>,
) -> Result<u8, String> {
    let wad = load(file)?;
    let palette = first_palette(&wad)?;
    let data = lump_named(&wad, name)?;
    let flat = Flat::parse(data).map_err(|e| format!("lump {name}: {e}"))?;

    writeln!(w, "flat {} ({})", name.to_ascii_uppercase(), file.display())
        .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "  size: {}x{}",
        doom_engine::gfx::flat::FLAT_WIDTH,
        doom_engine::gfx::flat::FLAT_HEIGHT
    )
    .map_err(|e| e.to_string())?;

    if let Some(path) = out {
        let mut image = RgbaImage::new(
            doom_engine::gfx::flat::FLAT_WIDTH as u32,
            doom_engine::gfx::flat::FLAT_HEIGHT as u32,
        );
        for y in 0..doom_engine::gfx::flat::FLAT_HEIGHT {
            for x in 0..doom_engine::gfx::flat::FLAT_WIDTH {
                image.set_pixel(x as u32, y as u32, opaque(palette.color(flat.pixel(x, y))));
            }
        }
        write_png(path, &image)?;
        writeln!(
            w,
            "wrote {} ({}x{})",
            path.display(),
            image.width(),
            image.height()
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(EXIT_OK)
}

// ----------------------------------------------------------------- flats ----

/// Lumps between the flat namespace markers, or (when the markers are
/// absent) every lump whose size matches a flat — with the fallback
/// reported so the caller can warn.
enum FlatNamespace {
    Marked { first: usize, last: usize },
    Fallback,
}

pub(crate) fn cmd_flats(w: &mut dyn Write, file: &Path) -> Result<u8, String> {
    let wad = load(file)?;
    let marker = match (wad.first_flat(), wad.last_flat()) {
        (Some(first), Some(last)) if first <= last => FlatNamespace::Marked { first, last },
        _ => FlatNamespace::Fallback,
    };

    let entries = wad.entries();
    let indices: Vec<usize> = match &marker {
        FlatNamespace::Marked { first, last } => (*first..=*last).collect(),
        FlatNamespace::Fallback => entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.size == doom_engine::gfx::flat::FLAT_SIZE)
            .map(|(i, _)| i)
            .collect(),
    };

    let mut warnings = 0u8;
    match &marker {
        FlatNamespace::Marked { .. } => {
            writeln!(w, "flat namespace (F_START..F_END) in {}", file.display())
                .map_err(|e| e.to_string())?;
        }
        FlatNamespace::Fallback => {
            writeln!(
                w,
                "warning: no F_START/F_END markers in {}; listing all lumps of exactly {} bytes",
                file.display(),
                doom_engine::gfx::flat::FLAT_SIZE
            )
            .map_err(|e| e.to_string())?;
            warnings = EXIT_WARNINGS;
        }
    }

    let mut rows: Vec<Vec<String>> = Vec::with_capacity(indices.len());
    for index in &indices {
        let entry = &entries[*index];
        let size = entry.size;
        let ok = size == doom_engine::gfx::flat::FLAT_SIZE;
        if !ok {
            warnings = EXIT_WARNINGS;
        }
        rows.push(vec![
            index.to_string(),
            entry.name.to_string(),
            size.to_string(),
            if ok {
                "ok".to_string()
            } else {
                format!("invalid (expected {})", doom_engine::gfx::flat::FLAT_SIZE)
            },
        ]);
    }
    write_table(w, &["index", "name", "size", "status"], &rows, &[0, 2])
        .map_err(|e| e.to_string())?;
    writeln!(w, "{} flat(s)", rows.len()).map_err(|e| e.to_string())?;
    Ok(warnings)
}

// ------------------------------------------------------------------- sky ----

pub(crate) fn cmd_sky(w: &mut dyn Write, file: &Path, out: Option<&Path>) -> Result<u8, String> {
    let wad = load(file)?;
    let palette = first_palette(&wad)?;
    let tables = load_texture_tables(&wad)?;

    let skies: Vec<&(Texture, TextureTable)> = tables
        .textures
        .iter()
        .filter(|(t, _)| t.name.starts_with("SKY"))
        .collect();

    if skies.is_empty() {
        writeln!(
            w,
            "warning: no textures with a name starting with SKY in {}",
            file.display()
        )
        .map_err(|e| e.to_string())?;
        return Ok(EXIT_WARNINGS);
    }

    writeln!(w, "sky textures in {}", file.display()).map_err(|e| e.to_string())?;
    for (texture, table) in &skies {
        write_texture_info(w, &tables, texture, *table).map_err(|e| e.to_string())?;
    }

    if let Some(out) = out {
        for (texture, _) in &skies {
            let image = texture_to_image(&wad, &tables, texture, &palette)?;
            let path = if skies.len() == 1 {
                out.to_path_buf()
            } else {
                numbered_out_path(out, &texture.name)
            };
            write_png(&path, &image)?;
            writeln!(
                w,
                "wrote {} ({}x{})",
                path.display(),
                image.width(),
                image.height()
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use doom_engine::gfx::palette::{COLORMAP_BYTES, MIN_COLORMAPS};

    fn test_palette() -> Palette {
        let mut colors = [[0u8; 3]; 256];
        for (i, color) in colors.iter_mut().enumerate() {
            *color = [
                i as u8,
                (i as u8).wrapping_mul(3),
                (i as u8).wrapping_add(100),
            ];
        }
        Palette { colors }
    }

    #[test]
    fn palette_grid_maps_index_to_cell() {
        let palette = test_palette();
        let image = palette_grid_image(&palette);
        assert_eq!((image.width(), image.height()), (512, 512));

        let px = |x: u32, y: u32| &image.pixels()[((y * 512 + x) * 4) as usize..][..4];
        // index 0 at (0,0), index 255 at (15*32, 15*32)
        assert_eq!(px(0, 0), [0, 0, 100, 255]);
        assert_eq!(px(15 * 32 + 7, 15 * 32 + 7), [255, 253, 99, 255]);
        // index 19 at col 3, row 1
        assert_eq!(px(3 * 32 + 5, 32 + 7), [19, 57, 119, 255]);
    }

    #[test]
    fn colormap_stack_bands_are_palette_through_colormap() {
        let palette = test_palette();
        // colormap 0: identity, colormap 1: maps everything 10 lower (wrapping)
        let mut maps = Vec::with_capacity(MIN_COLORMAPS);
        let mut identity = [0u8; COLORMAP_BYTES];
        for (i, v) in identity.iter_mut().enumerate() {
            *v = i as u8;
        }
        maps.push(Colormap(identity));
        let mut shifted = [0u8; COLORMAP_BYTES];
        for (i, v) in shifted.iter_mut().enumerate() {
            *v = (i as u8).wrapping_sub(10);
        }
        maps.push(Colormap(shifted));
        for k in 2..MIN_COLORMAPS {
            maps.push(Colormap([k as u8; COLORMAP_BYTES]));
        }

        let image = colormap_stack_image(&maps, &palette);
        assert_eq!(
            (image.width(), image.height()),
            (256, 16 * MIN_COLORMAPS as u32)
        );

        let px = |x: u32, y: u32| &image.pixels()[((y * 256 + x) * 4) as usize..][..4];
        // band 0, x=5: palette[5]; band 1, x=5: palette[5-10] wraps to palette[251]
        assert_eq!(px(5, 0), opaque(palette.color(5)));
        assert_eq!(px(5, 16), opaque(palette.color(251)));
    }

    #[test]
    fn patch_to_image_transparent_background_and_pixels() {
        let palette = test_palette();
        // one 2x2 patch with a single pixel at (1, 0), everything else transparent
        let data: Vec<u8> = {
            let mut d = Vec::new();
            d.extend_from_slice(&2i16.to_le_bytes());
            d.extend_from_slice(&2i16.to_le_bytes());
            d.extend_from_slice(&0i16.to_le_bytes());
            d.extend_from_slice(&0i16.to_le_bytes());
            // column 0: nothing (just terminator); column 1: one pixel, index 42
            d.extend_from_slice(&16i32.to_le_bytes());
            d.extend_from_slice(&17i32.to_le_bytes());
            d.push(0xff); // column 0
            d.push(0); // topdelta
            d.push(1); // length
            d.push(0);
            d.push(42);
            d.push(0);
            d.push(0xff); // column 1
            d
        };
        let patch = decode_patch(&data).unwrap();
        let (image, clipped) = patch_to_image(&patch, &palette);
        assert_eq!(clipped, 0);
        assert_eq!((image.width(), image.height()), (2, 2));
        let px = |x: u32, y: u32| &image.pixels()[((y * 2 + x) * 4) as usize..][..4];
        assert_eq!(px(1, 0), opaque(palette.color(42)));
        assert_eq!(px(0, 0), [0, 0, 0, 0]);
        assert_eq!(px(0, 1), [0, 0, 0, 0]);
    }

    #[test]
    fn patch_to_image_clips_rows_past_height() {
        let palette = test_palette();
        // height 2, but the post claims rows 0..=3 (legal on disk)
        let data: Vec<u8> = {
            let mut d = Vec::new();
            d.extend_from_slice(&1i16.to_le_bytes());
            d.extend_from_slice(&2i16.to_le_bytes());
            d.extend_from_slice(&0i16.to_le_bytes());
            d.extend_from_slice(&0i16.to_le_bytes());
            d.extend_from_slice(&12i32.to_le_bytes());
            d.push(0); // topdelta
            d.push(4); // length: rows 0..=3
            d.push(0);
            d.extend_from_slice(&[7, 8, 9, 10]);
            d.push(0);
            d.push(0xff);
            d
        };
        let patch = decode_patch(&data).unwrap();
        let (image, clipped) = patch_to_image(&patch, &palette);
        assert_eq!(clipped, 2);
        let px = |x: u32, y: u32| &image.pixels()[((y + x) * 4) as usize..][..4];
        assert_eq!(px(0, 0), opaque(palette.color(7)));
        assert_eq!(px(0, 1), opaque(palette.color(8)));
    }
}
