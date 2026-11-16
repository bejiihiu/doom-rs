//! `doom-demo-analyzer` - command-line analyzer for DOOM demo recordings.
//!
//! Parses the `.lmp` wire format locally (layout: version byte, the 13
//! header bytes from `G_BeginRecording`, then 4 bytes per tic until the
//! `0x80` DEMOMARKER, `g_game.c`). The engine's demo reader is not used
//! for analysis - it only accepts vanilla 1.10 demos and gives no access
//! to the raw bytes; see `demo.rs` for the rationale.

#![forbid(unsafe_code)]

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

mod analyzer;
mod demo;

/// Analyzes DOOM demo recordings (.lmp): header metadata, tic streams and
/// side-by-side comparison.
#[derive(Parser)]
#[command(name = "doom-demo-analyzer", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the 13-byte header: version byte, skill, episode, map, the
    /// level reading (episode/commercial), deathmatch mode, flags and
    /// the players in game.
    Info {
        /// Demo (.lmp) file to read.
        file: PathBuf,
    },
    /// Statistics over the tic stream: tic counts, forward/side
    /// distribution, RUNNING percentage, attack/use/weapon-change rates,
    /// turn statistics and bursts of unusual activity.
    Stats {
        /// Demo (.lmp) file to read.
        file: PathBuf,
    },
    /// Dump the tic stream, one tic per line. Plain text by default;
    /// `--format jsonl` emits JSON Lines for scripting.
    DumpTics {
        /// Demo (.lmp) file to read.
        file: PathBuf,
        /// Stop after N tics (default: the whole stream).
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
        /// Output format.
        #[arg(long, value_enum, default_value_t = TicFormatArg::Plain)]
        format: TicFormatArg,
    },
    /// Compare two demos: headers, the tic streams tic-by-tic and the
    /// stream endings. Exit codes: 0 = identical, 1 = different, 2 = error.
    Compare {
        /// First demo (.lmp) file.
        file1: PathBuf,
        /// Second demo (.lmp) file.
        file2: PathBuf,
    },
    /// Validate a demo: version, skill, episode/map ranges, console
    /// player, player flags, deathmatch mode, boolean flags, tic count
    /// and the DEMOMARKER terminator. Exit codes: 0 = OK, 1 = warnings,
    /// 2 = fatal error. Note: NCMD_* flags do not exist in `.lmp` demos -
    /// they are net-packet flags from `d_net.c`; a demo ends with the
    /// 0x80 byte.
    Validate {
        /// Demo (.lmp) file to validate.
        file: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum TicFormatArg {
    Plain,
    Jsonl,
}

impl From<TicFormatArg> for analyzer::TicFormat {
    fn from(value: TicFormatArg) -> Self {
        match value {
            TicFormatArg::Plain => analyzer::TicFormat::Plain,
            TicFormatArg::Jsonl => analyzer::TicFormat::Jsonl,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let result = match cli.command {
        Command::Info { file } => {
            demo::load(&file).and_then(|demo| analyzer::cmd_info(&mut out, &file, &demo))
        }
        Command::Stats { file } => {
            demo::load(&file).and_then(|demo| analyzer::cmd_stats(&mut out, &file, &demo))
        }
        Command::DumpTics {
            file,
            limit,
            format,
        } => demo::load(&file)
            .and_then(|demo| analyzer::cmd_dump_tics(&mut out, &demo, limit, format.into())),
        Command::Compare { file1, file2 } => compare(&mut out, &file1, &file2),
        Command::Validate { file } => match demo::load(&file) {
            Ok(demo) => analyzer::cmd_validate(&mut out, &file, &demo),
            Err(msg) => {
                // a demo we cannot even parse fails validation fatally
                let _ = writeln!(out, "File: {}", file.display());
                let _ = writeln!(out, "[fatal] {msg}");
                let _ = writeln!(out);
                let _ = writeln!(out, "Result: FATAL (1 fatal error)");
                Ok(2)
            }
        },
    };

    match result {
        Ok(code) => ExitCode::from(code),
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::from(2)
        }
    }
}

fn compare(
    out: &mut dyn io::Write,
    file1: &std::path::Path,
    file2: &std::path::Path,
) -> Result<u8, String> {
    let a = demo::load(file1)?;
    let b = demo::load(file2)?;
    analyzer::cmd_compare(out, file1, &a, file2, &b)
}
