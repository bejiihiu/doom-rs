//! `doom-wad-inspector` — command-line inspector for DOOM WAD archives.
//!
//! Parses through `doom-engine`'s WAD API ([`doom_engine::wad`]); this binary
//! only does presentation and reporting on top of it.

#![forbid(unsafe_code)]

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod inspector;

/// Inspects DOOM WAD archives: the lump directory, stacking order and
/// structural validation. Parsing is strict — a malformed archive is
/// rejected with a descriptive message instead of panicking.
#[derive(Parser)]
#[command(name = "doom-wad-inspector", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the lump directory: index, name, size and offset of every lump.
    List {
        /// WAD file to inspect.
        file: PathBuf,
    },
    /// Print a summary: lump count, file size, smallest/largest lump and
    /// how much of the file is covered by lump data.
    Stats {
        /// WAD file to inspect.
        file: PathBuf,
    },
    /// Write a lump's raw bytes to a file, or to stdout with `--out -`
    /// (binary-safe pipe). Without `--out` the command does nothing but
    /// report an error, so archive bytes never leak into a terminal.
    ///
    /// When the name occurs multiple times, the vanilla-active lump (the
    /// last one in the directory) is dumped.
    Dump {
        /// WAD file to read from.
        file: PathBuf,
        /// Lump name, case-insensitive.
        lump: String,
        /// Output path, or `-` for stdout (binary-safe). Refuses to
        /// overwrite the input file.
        #[arg(long, value_name = "PATH", allow_hyphen_values = true)]
        out: Option<String>,
    },
    /// Find every occurrence of a lump name.
    ///
    /// By default this lists each occurrence and marks the vanilla-active
    /// one (the last occurrence, matching the engine's override rule) with
    /// `[active]`. With `--all`, names that merely start with the query
    /// (e.g. TEXTUR -> TEXTURE1, TEXTURE2) are listed as well.
    ///
    /// Exit codes: 0 = found, 1 = not found, 2 = error.
    Find {
        /// WAD file to search.
        file: PathBuf,
        /// Lump name, case-insensitive.
        name: String,
        /// Also list names that merely start with the query (prefix
        /// search), in addition to exact matches.
        #[arg(long)]
        all: bool,
    },
    /// Validate the archive and print a report.
    ///
    /// Checks: format magic, directory and lump bounds (both enforced by
    /// the parser), duplicate names, namespace markers
    /// (`F_START`/`F_END`/`S_START`/`S_END`), presence of the core IWAD
    /// lumps (`PLAYPAL`, `COLORMAP`, `TEXTURE1`, `PNAMES`) and per-map
    /// completeness (`THINGS` … `BLOCKMAP`).
    ///
    /// Exit codes: 0 = OK, 1 = warnings found, 2 = fatal error.
    Validate {
        /// WAD file to validate.
        file: PathBuf,
    },
    /// List all maps (`MAP01`..`MAP32`, `E1M1`..`E4M9`) with the marker
    /// index, the number of lumps per map and whether the expected map
    /// lumps are all present.
    Maps {
        /// WAD file to inspect.
        file: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let result = match cli.command {
        Command::List { file } => {
            inspector::load(&file).and_then(|wad| inspector::cmd_list(&mut out, &wad))
        }
        Command::Stats { file } => {
            inspector::load(&file).and_then(|wad| inspector::cmd_stats(&mut out, &file, &wad))
        }
        Command::Dump {
            file,
            lump,
            out: dest,
        } => inspector::load(&file)
            .and_then(|wad| inspector::cmd_dump(&mut out, &file, &wad, &lump, dest.as_deref())),
        Command::Find { file, name, all } => {
            inspector::load(&file).and_then(|wad| inspector::cmd_find(&mut out, &wad, &name, all))
        }
        Command::Validate { file } => inspector::validate_report(&mut out, &file),
        Command::Maps { file } => {
            inspector::load(&file).and_then(|wad| inspector::cmd_maps(&mut out, &wad))
        }
    };

    match result {
        Ok(code) => ExitCode::from(code),
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::from(2)
        }
    }
}
