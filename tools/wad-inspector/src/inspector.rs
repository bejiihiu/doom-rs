//! Analysis and reporting for `doom-wad-inspector`.
//!
//! Everything here builds on [`doom_engine::wad`]; the engine owns the
//! format and the bounds rules, this module only selects, formats and
//! judges. A WAD that fails to parse is a fatal error for the command.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use doom_engine::wad::{DirectoryEntry, LumpName, WadFile, WadKind};

/// Lumps the game needs in an IWAD. Missing ones are warnings — the file
/// still parses, but nothing in the engine is going to boot off it.
const CORE_IWAD_LUMPS: [&str; 4] = ["PLAYPAL", "COLORMAP", "TEXTURE1", "PNAMES"];

/// Lumps every vanilla map block carries after its marker lump.
const EXPECTED_MAP_LUMPS: [&str; 10] = [
    "THINGS", "LINEDEFS", "SIDEDEFS", "VERTEXES", "SEGS", "SSECTORS", "NODES", "SECTORS", "REJECT",
    "BLOCKMAP",
];

/// Header size of a WAD file.
const HEADER_SIZE: usize = 12;
/// Size of one directory entry.
const DIRECTORY_ENTRY_SIZE: usize = 16;
/// How many items a long list prints before it gets an "and N more" tail.
const MAX_LISTED: usize = 8;

/// Opens and parses a WAD, mapping any failure to a fatal command error.
pub(crate) fn load(file: &Path) -> Result<WadFile, String> {
    WadFile::open(file).map_err(|e| e.to_string())
}

fn kind_label(kind: WadKind) -> &'static str {
    match kind {
        WadKind::Iwad => "IWAD",
        WadKind::Pwad => "PWAD",
    }
}

fn hex_offset(offset: usize) -> String {
    format!("0x{offset:08x}")
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

// ---------------------------------------------------------------- list ----

pub(crate) fn cmd_list(w: &mut dyn Write, wad: &WadFile) -> Result<u8, String> {
    let headers = ["index", "name", "size", "offset"];
    let rows: Vec<Vec<String>> = wad
        .entries()
        .iter()
        .enumerate()
        .map(|(i, e)| {
            vec![
                i.to_string(),
                e.name.to_string(),
                e.size.to_string(),
                hex_offset(e.file_pos),
            ]
        })
        .collect();
    write_table(w, &headers, &rows, &[0, 2]).map_err(|e| e.to_string())?;
    writeln!(w, "{} lump(s)", rows.len()).map_err(|e| e.to_string())?;
    Ok(0)
}

// --------------------------------------------------------------- stats ----

pub(crate) fn cmd_stats(w: &mut dyn Write, file: &Path, wad: &WadFile) -> Result<u8, String> {
    let file_len = std::fs::metadata(file)
        .map_err(|e| format!("could not stat {}: {e}", file.display()))?
        .len() as usize;

    let entries = wad.entries();
    let num_lumps = entries.len();
    let dir_bytes = num_lumps * DIRECTORY_ENTRY_SIZE;

    let total_lump_bytes: usize = entries.iter().map(|e| e.size).sum();
    let coverage = union_coverage(entries);
    let coverage_pct = if file_len == 0 {
        0.0
    } else {
        coverage as f64 * 100.0 / file_len as f64
    };

    let mut smallest: Option<(usize, &DirectoryEntry)> = None;
    let mut largest: Option<(usize, &DirectoryEntry)> = None;
    for (i, e) in entries.iter().enumerate() {
        if smallest.is_none_or(|(_, cur)| e.size < cur.size) {
            smallest = Some((i, e));
        }
        if largest.is_none_or(|(_, cur)| e.size > cur.size) {
            largest = Some((i, e));
        }
    }
    let average = total_lump_bytes.checked_div(num_lumps).unwrap_or(0);
    let empty_lumps = entries.iter().filter(|e| e.size == 0).count();

    writeln!(w, "file:             {}", file.display()).map_err(|e| e.to_string())?;
    writeln!(w, "format:           {}", kind_label(wad.kind())).map_err(|e| e.to_string())?;
    writeln!(w, "file size:        {file_len} bytes").map_err(|e| e.to_string())?;
    writeln!(w, "lump count:       {num_lumps}").map_err(|e| e.to_string())?;
    writeln!(w, "header:           {HEADER_SIZE} bytes").map_err(|e| e.to_string())?;
    writeln!(
        w,
        "directory:        {dir_bytes} bytes ({num_lumps} entries x {DIRECTORY_ENTRY_SIZE} bytes)"
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "lump data:        {total_lump_bytes} bytes (sum of lump sizes)"
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "lump coverage:    {coverage} bytes ({coverage_pct:.1}% of file)"
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "unaccounted:      {} bytes (header + directory + gaps)",
        file_len - coverage
    )
    .map_err(|e| e.to_string())?;
    if let Some((i, e)) = smallest {
        writeln!(
            w,
            "smallest lump:    {} ({} bytes) at index {i}",
            e.name, e.size
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some((i, e)) = largest {
        writeln!(
            w,
            "largest lump:     {} ({} bytes) at index {i}",
            e.name, e.size
        )
        .map_err(|e| e.to_string())?;
    }
    writeln!(w, "average lump:     {average} bytes").map_err(|e| e.to_string())?;
    writeln!(
        w,
        "empty lumps:      {empty_lumps} (namespace markers are empty)"
    )
    .map_err(|e| e.to_string())?;
    Ok(0)
}

/// Bytes of the file covered by at least one lump (overlapping ranges counted once).
fn union_coverage(entries: &[DirectoryEntry]) -> usize {
    let mut ranges: Vec<(usize, usize)> = entries
        .iter()
        .filter(|e| e.size > 0)
        .map(|e| (e.file_pos, e.file_pos + e.size))
        .collect();
    ranges.sort_unstable();
    let mut total = 0usize;
    let mut cur: Option<(usize, usize)> = None;
    for (start, end) in ranges {
        match cur {
            Some((_, ref mut cur_end)) if start <= *cur_end => {
                *cur_end = (*cur_end).max(end);
            }
            Some((cur_start, cur_end)) => {
                total += cur_end - cur_start;
                cur = Some((start, end));
            }
            None => cur = Some((start, end)),
        }
    }
    if let Some((start, end)) = cur {
        total += end - start;
    }
    total
}

// ---------------------------------------------------------------- dump ----

pub(crate) fn cmd_dump(
    w: &mut dyn Write,
    file: &Path,
    wad: &WadFile,
    lump: &str,
    out: Option<&str>,
) -> Result<u8, String> {
    let name = normalize_query(lump)?;
    let index = wad
        .find_lump(lump)
        .ok_or_else(|| format!("lump '{name}' not found"))?;
    let bytes = wad.lump(index).map_err(|e| e.to_string())?;

    let Some(dest) = out else {
        return Err(
            "nothing to do: pass --out <PATH> to write the lump, or --out - for stdout".to_string(),
        );
    };

    if dest == "-" {
        w.write_all(bytes).map_err(|e| format!("stdout: {e}"))?;
        w.flush().map_err(|e| format!("stdout: {e}"))?;
        eprintln!(
            "dumped {} byte(s) of lump #{index} '{}' to stdout",
            bytes.len(),
            wad.entries()[index].name
        );
        return Ok(0);
    }

    let dest_path = PathBuf::from(dest);
    refuse_same_file(file, &dest_path)?;
    std::fs::write(&dest_path, bytes)
        .map_err(|e| format!("could not write {}: {e}", dest_path.display()))?;
    writeln!(
        w,
        "wrote {} byte(s) of lump #{index} '{}' to {}",
        bytes.len(),
        wad.entries()[index].name,
        dest_path.display()
    )
    .map_err(|e| e.to_string())?;
    Ok(0)
}

fn refuse_same_file(input: &Path, dest: &Path) -> Result<(), String> {
    let same = match (std::fs::canonicalize(input), std::fs::canonicalize(dest)) {
        (Ok(a), Ok(b)) => a == b,
        // dest does not exist yet -> it cannot be the input file
        _ => input == dest,
    };
    if same {
        return Err(format!(
            "refusing to overwrite the input file {}",
            input.display()
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------- find ----

pub(crate) fn cmd_find(
    w: &mut dyn Write,
    wad: &WadFile,
    name: &str,
    all: bool,
) -> Result<u8, String> {
    let query = normalize_query(name)?;
    let query_name = LumpName::from(name);

    let exact: Vec<usize> = wad
        .entries()
        .iter()
        .enumerate()
        .filter(|(_, e)| e.name == query_name)
        .map(|(i, _)| i)
        .collect();
    let active = exact.last().copied();

    let print_rows = |w: &mut dyn Write, indices: &[usize]| -> io::Result<()> {
        let headers = ["index", "size", "offset", "note"];
        let rows: Vec<Vec<String>> = indices
            .iter()
            .map(|&i| {
                let e = &wad.entries()[i];
                let note = if Some(i) == active { "[active]" } else { "" };
                vec![
                    i.to_string(),
                    e.size.to_string(),
                    hex_offset(e.file_pos),
                    note.to_string(),
                ]
            })
            .collect();
        write_table(w, &headers, &rows, &[0, 1])
    };

    if exact.is_empty() && !all {
        writeln!(w, "no lump named '{query}'").map_err(|e| e.to_string())?;
        return Ok(1);
    }

    let mut found = !exact.is_empty();

    if !exact.is_empty() {
        writeln!(w, "{} occurrence(s) of '{query}':", exact.len()).map_err(|e| e.to_string())?;
        print_rows(w, &exact).map_err(|e| e.to_string())?;
        if let Some(a) = active {
            writeln!(
                w,
                "vanilla override: the last occurrence (index {a}) is the one the engine uses"
            )
            .map_err(|e| e.to_string())?;
        }
    } else {
        writeln!(w, "no lump named '{query}'").map_err(|e| e.to_string())?;
    }

    if all {
        let prefix: Vec<usize> = wad
            .entries()
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                let n = e.name.trimmed();
                n.starts_with(query.as_bytes()) && e.name != query_name
            })
            .map(|(i, _)| i)
            .collect();
        if prefix.is_empty() {
            writeln!(w, "no other names start with '{query}'").map_err(|e| e.to_string())?;
        } else {
            writeln!(w, "names starting with '{query}' (not exact matches):")
                .map_err(|e| e.to_string())?;
            let headers = ["index", "name", "size", "offset"];
            let rows: Vec<Vec<String>> = prefix
                .iter()
                .map(|&i| {
                    let e = &wad.entries()[i];
                    vec![
                        i.to_string(),
                        e.name.to_string(),
                        e.size.to_string(),
                        hex_offset(e.file_pos),
                    ]
                })
                .collect();
            write_table(w, &headers, &rows, &[0, 2]).map_err(|e| e.to_string())?;
            found = true;
        }
    }

    Ok(if found { 0 } else { 1 })
}

fn normalize_query(name: &str) -> Result<String, String> {
    let trimmed = LumpName::from(name).trimmed().to_vec();
    if trimmed.is_empty() {
        return Err("empty lump name".to_string());
    }
    Ok(String::from_utf8_lossy(&trimmed).into_owned())
}

// ---------------------------------------------------------------- maps ----

/// A map block in the directory: the marker lump and everything after it
/// up to the next map marker or the end of the directory.
#[derive(Debug, Clone)]
struct MapBlock {
    marker_index: usize,
    name: String,
    /// `[marker_index, run_end)` — the whole block including the marker.
    run: std::ops::Range<usize>,
}

impl MapBlock {
    /// Indices of the block's payload lumps (everything after the marker).
    fn payload(&self) -> std::ops::Range<usize> {
        self.marker_index + 1..self.run.end
    }

    fn lump_count(&self) -> usize {
        self.run.end - self.marker_index
    }
}

/// `MAP01`..`MAP32` (Doom II / Final Doom) and `E1M1`..`E4M9` (Doom).
fn is_map_marker(bytes: &[u8]) -> bool {
    if bytes.len() == 5 && bytes[0] == b'M' && bytes[1] == b'A' && bytes[2] == b'P' {
        let n = two_digits(bytes[3], bytes[4]);
        return matches!(n, Some(1..=32));
    }
    if bytes.len() == 4 && bytes[0] == b'E' && bytes[2] == b'M' {
        let episode = ascii_digit(bytes[1]);
        let map = ascii_digit(bytes[3]);
        return matches!(episode, Some(1..=4)) && matches!(map, Some(1..=9));
    }
    false
}

fn ascii_digit(b: u8) -> Option<u8> {
    if b.is_ascii_digit() {
        Some(b - b'0')
    } else {
        None
    }
}

fn two_digits(a: u8, b: u8) -> Option<u8> {
    Some(ascii_digit(a)? * 10 + ascii_digit(b)?)
}

fn find_maps(entries: &[DirectoryEntry]) -> Vec<MapBlock> {
    let markers: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| is_map_marker(e.name.trimmed()))
        .map(|(i, _)| i)
        .collect();
    let mut blocks = Vec::with_capacity(markers.len());
    for (pos, &marker_index) in markers.iter().enumerate() {
        let run_end = markers.get(pos + 1).copied().unwrap_or(entries.len());
        blocks.push(MapBlock {
            marker_index,
            name: entries[marker_index].name.to_string(),
            run: marker_index..run_end,
        });
    }
    blocks
}

/// Names of the expected lumps missing from a map block.
fn missing_map_lumps<'a>(entries: &'a [DirectoryEntry], block: &MapBlock) -> Vec<&'a str> {
    let present: std::collections::HashSet<&[u8]> =
        block.payload().map(|i| entries[i].name.trimmed()).collect();
    EXPECTED_MAP_LUMPS
        .into_iter()
        .filter(|name| !present.contains(name.as_bytes()))
        .collect()
}

pub(crate) fn cmd_maps(w: &mut dyn Write, wad: &WadFile) -> Result<u8, String> {
    let entries = wad.entries();
    let maps = find_maps(entries);
    if maps.is_empty() {
        writeln!(
            w,
            "no maps found (expected MAP01..MAP32 or E1M1..E4M9 markers)"
        )
        .map_err(|e| e.to_string())?;
        return Ok(0);
    }
    let headers = ["map", "index", "lumps", "missing"];
    let rows: Vec<Vec<String>> = maps
        .iter()
        .map(|block| {
            let missing = missing_map_lumps(entries, block);
            let missing_cell = if missing.is_empty() {
                "ok".to_string()
            } else {
                missing.join(",")
            };
            vec![
                block.name.clone(),
                block.marker_index.to_string(),
                block.lump_count().to_string(),
                missing_cell,
            ]
        })
        .collect();
    write_table(w, &headers, &rows, &[1, 2]).map_err(|e| e.to_string())?;
    writeln!(w, "{} map(s) found", maps.len()).map_err(|e| e.to_string())?;
    Ok(0)
}

// ------------------------------------------------------------- validate ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Ok,
    Note,
    Warn,
}

struct Finding {
    level: Level,
    message: String,
}

impl Finding {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            level: Level::Ok,
            message: message.into(),
        }
    }
    fn note(message: impl Into<String>) -> Self {
        Self {
            level: Level::Note,
            message: message.into(),
        }
    }
    fn warn(message: impl Into<String>) -> Self {
        Self {
            level: Level::Warn,
            message: message.into(),
        }
    }
}

pub(crate) fn validate_report(w: &mut dyn Write, file: &Path) -> Result<u8, String> {
    let file_len = std::fs::metadata(file)
        .map_err(|e| format!("could not stat {}: {e}", file.display()))?
        .len();

    writeln!(w, "file: {} ({file_len} bytes)", file.display()).map_err(|e| e.to_string())?;

    let wad = match WadFile::open(file) {
        Ok(wad) => wad,
        Err(err) => {
            writeln!(w, "[fatal] {err}").map_err(|e| e.to_string())?;
            writeln!(w, "result: FATAL").map_err(|e| e.to_string())?;
            return Ok(2);
        }
    };

    let kind = wad.kind();
    let is_iwad = kind == WadKind::Iwad;
    let entries = wad.entries();
    let num_lumps = entries.len();

    // map blocks are detected up front; expected map lumps recur in every
    // map, so occurrences inside map blocks are not interesting as
    // "duplicates" — a 32-map IWAD would light the report up like a tree
    let maps = find_maps(entries);
    let in_map_block: std::collections::HashSet<usize> =
        maps.iter().flat_map(|m| m.run.clone()).collect();

    let mut findings: Vec<Finding> = Vec::new();
    findings.push(Finding::ok(format!(
        "format: {}, {num_lumps} lump(s), directory at {}",
        kind_label(kind),
        hex_offset(wad.header().dir_offset as usize)
    )));
    findings.push(Finding::ok(format!(
        "bounds: all {num_lumps} lump(s) fit inside the file (checked at load)"
    )));

    // duplicate names; the last one wins on lookup, like vanilla.
    // expected map lumps inside map blocks are exempt — they are supposed to recur
    let mut by_name: HashMap<LumpName, Vec<usize>> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        let is_expected_map_lump = EXPECTED_MAP_LUMPS
            .iter()
            .any(|m| m.as_bytes() == e.name.trimmed());
        if in_map_block.contains(&i) && is_expected_map_lump {
            continue;
        }
        by_name.entry(e.name).or_default().push(i);
    }
    let mut duplicates: Vec<(&LumpName, &Vec<usize>)> =
        by_name.iter().filter(|(_, v)| v.len() > 1).collect();
    duplicates.sort_by_key(|(_, v)| v[0]);
    if duplicates.is_empty() {
        findings.push(Finding::ok("duplicates: none"));
    } else {
        for (name, indices) in duplicates.iter().take(MAX_LISTED) {
            let list = index_list(indices);
            let message = format!(
                "duplicate lump '{name}': {} occurrence(s) at indices {list}; the last one wins",
                indices.len()
            );
            findings.push(if is_iwad {
                Finding::warn(message)
            } else {
                Finding::note(message)
            });
        }
        if duplicates.len() > MAX_LISTED {
            findings.push(Finding::note(format!(
                "... and {} more duplicated name(s)",
                duplicates.len() - MAX_LISTED
            )));
        }
    }

    // namespace markers: presence, pairing, order and emptiness
    for (start, end, label) in [("F_START", "F_END", "flat"), ("S_START", "S_END", "sprite")] {
        let starts: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.name == LumpName::from(start))
            .map(|(i, _)| i)
            .collect();
        let ends: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.name == LumpName::from(end))
            .map(|(i, _)| i)
            .collect();
        match (starts.first().copied(), ends.last().copied()) {
            (Some(s), Some(e)) => {
                if s >= e {
                    findings.push(Finding::warn(format!(
                        "{label} namespace inverted: {start} at index {s} is not before {end} at index {e}"
                    )));
                } else if e - s <= 1 {
                    findings.push(Finding::note(format!(
                        "{label} namespace is empty ({start} at {s}, {end} at {e})"
                    )));
                } else {
                    findings.push(Finding::ok(format!(
                        "{label} namespace: {start} at {s} .. {end} at {e} ({} lump(s) inside)",
                        e - s - 1
                    )));
                }
            }
            (Some(s), None) => findings.push(Finding::warn(format!(
                "{label} namespace: {start} at index {s} but no {end} — the namespace never closes"
            ))),
            (None, Some(e)) => findings.push(Finding::warn(format!(
                "{label} namespace: {end} at index {e} but no {start} — the namespace never opens"
            ))),
            (None, None) => {
                if is_iwad {
                    findings.push(Finding::warn(format!(
                        "{label} namespace markers ({start}/{end}) absent"
                    )));
                } else {
                    findings.push(Finding::note(format!(
                        "{label} namespace markers ({start}/{end}) absent (PWAD may inherit them)"
                    )));
                }
            }
        }
    }

    // core IWAD lumps
    if is_iwad {
        let mut missing = Vec::new();
        for lump in CORE_IWAD_LUMPS {
            if wad.find_lump(lump).is_none() {
                missing.push(lump);
            }
        }
        if missing.is_empty() {
            findings.push(Finding::ok(format!(
                "core lumps: all present ({})",
                CORE_IWAD_LUMPS.join(", ")
            )));
        } else {
            findings.push(Finding::warn(format!(
                "core IWAD lump(s) missing: {}",
                missing.join(", ")
            )));
        }
    } else {
        findings.push(Finding::note(
            "PWAD: core IWAD lumps (PLAYPAL, COLORMAP, TEXTURE1, PNAMES) are inherited from the IWAD, not checked",
        ));
    }

    // maps and their completeness
    if maps.is_empty() {
        findings.push(Finding::note("maps: none found"));
    } else {
        for block in &maps {
            let missing = missing_map_lumps(entries, block);
            if missing.is_empty() {
                findings.push(Finding::ok(format!(
                    "map {}: all expected lumps present ({} lump(s) in block)",
                    block.name,
                    block.lump_count()
                )));
            } else {
                findings.push(Finding::warn(format!(
                    "map {}: missing expected lump(s): {}",
                    block.name,
                    missing.join(", ")
                )));
            }
        }
    }

    // layout: overlapping lump ranges and lumps covering the directory
    let mut sorted: Vec<(usize, usize, usize)> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.size > 0)
        .map(|(i, e)| (e.file_pos, e.file_pos + e.size, i))
        .collect();
    sorted.sort_unstable();
    let mut overlaps = 0usize;
    for pair in sorted.windows(2) {
        let (_, prev_end, _) = pair[0];
        let (next_start, _, _) = pair[1];
        if next_start < prev_end {
            overlaps += 1;
        }
    }
    if overlaps == 0 {
        findings.push(Finding::ok("layout: no overlapping lump ranges"));
    } else {
        findings.push(Finding::warn(format!(
            "layout: {overlaps} overlapping lump range pair(s)"
        )));
    }

    let dir_start = wad.header().dir_offset as usize;
    let dir_end = dir_start + num_lumps * DIRECTORY_ENTRY_SIZE;
    let dir_hits: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.size > 0 && e.file_pos < dir_end && e.file_pos + e.size > dir_start)
        .map(|(i, _)| i)
        .collect();
    if dir_hits.is_empty() {
        findings.push(Finding::ok("layout: no lump covers the directory"));
    } else {
        findings.push(Finding::warn(format!(
            "layout: {} lump(s) overlap the directory: {}",
            dir_hits.len(),
            index_list(&dir_hits)
        )));
    }

    // empty lumps that are not known namespace markers
    const KNOWN_EMPTY: [&str; 4] = ["F_START", "F_END", "S_START", "S_END"];
    let empty_markers: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.size == 0 && !KNOWN_EMPTY.contains(&e.name.to_string().as_str()))
        .map(|(i, _)| i)
        .collect();
    // map marker lumps are allowed to be empty too
    let empty_non_map: Vec<usize> = empty_markers
        .into_iter()
        .filter(|&i| !is_map_marker(entries[i].name.trimmed()))
        .collect();
    if empty_non_map.is_empty() {
        findings.push(Finding::ok("empty lumps: only namespace markers"));
    } else {
        findings.push(Finding::note(format!(
            "{} zero-size lump(s) besides markers at indices {}",
            empty_non_map.len(),
            index_list(&empty_non_map)
        )));
    }

    let mut warnings = 0usize;
    let mut notes = 0usize;
    for finding in &findings {
        let tag = match finding.level {
            Level::Ok => "ok",
            Level::Note => {
                notes += 1;
                "note"
            }
            Level::Warn => {
                warnings += 1;
                "warn"
            }
        };
        writeln!(w, "[{tag}] {}", finding.message).map_err(|e| e.to_string())?;
    }

    writeln!(w, "summary: {warnings} warning(s), {notes} note(s)").map_err(|e| e.to_string())?;
    if warnings > 0 {
        writeln!(w, "result: WARNINGS").map_err(|e| e.to_string())?;
        Ok(1)
    } else {
        writeln!(w, "result: OK").map_err(|e| e.to_string())?;
        Ok(0)
    }
}

fn index_list(indices: &[usize]) -> String {
    let shown: Vec<String> = indices
        .iter()
        .take(MAX_LISTED)
        .map(|i| i.to_string())
        .collect();
    let mut out = shown.join(",");
    if indices.len() > MAX_LISTED {
        let _ = write!(out, ",... (+{} more)", indices.len() - MAX_LISTED);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pos: usize, size: usize, n: &str) -> DirectoryEntry {
        DirectoryEntry {
            file_pos: pos,
            size,
            name: LumpName::from(n),
        }
    }

    #[test]
    fn map_markers_match_vanilla_ranges() {
        assert!(is_map_marker(b"MAP01"));
        assert!(is_map_marker(b"MAP09"));
        assert!(is_map_marker(b"MAP32"));
        assert!(!is_map_marker(b"MAP00"));
        assert!(!is_map_marker(b"MAP33"));
        assert!(!is_map_marker(b"MAP1"));
        assert!(is_map_marker(b"E1M1"));
        assert!(is_map_marker(b"E4M9"));
        assert!(!is_map_marker(b"E0M1"));
        assert!(!is_map_marker(b"E5M1"));
        assert!(!is_map_marker(b"E1M0"));
        assert!(!is_map_marker(b"E1MA"));
        assert!(!is_map_marker(b"MAP0A"));
        assert!(!is_map_marker(b"TEXTURE1"));
        // marker names arrive exactly as stored (8 bytes, NUL padding) — trimmed by caller
        assert!(is_map_marker(b"E2M3"));
    }

    #[test]
    fn union_coverage_counts_overlaps_once() {
        let entries = vec![
            entry(100, 50, "A"),  // 100..150
            entry(120, 50, "B"),  // 120..170, overlaps A
            entry(200, 10, "C"),  // 200..210
            entry(0, 0, "EMPTY"), // zero size: excluded
        ];
        // A ∪ B = 100..170 (70 bytes), C adds 10
        assert_eq!(union_coverage(&entries), 80);
    }

    #[test]
    fn union_coverage_of_non_overlapping_ranges() {
        let entries = vec![entry(10, 10, "A"), entry(30, 20, "B")];
        assert_eq!(union_coverage(&entries), 30);
    }

    #[test]
    fn index_list_truncates_long_lists() {
        let all: Vec<usize> = (0..20).collect();
        let out = index_list(&all);
        assert!(out.contains("+12 more"), "unexpected tail: {out}");
        assert_eq!(index_list(&[1, 2, 3]), "1,2,3");
    }
}
