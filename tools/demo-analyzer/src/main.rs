//! `doom-demo-analyzer` — command-line analyzer for DOOM demo recordings.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

/// Analyzes DOOM demo recordings (.lmp): header metadata, gametic counts and
/// the ticcmd stream.
#[derive(Parser)]
#[command(name = "doom-demo-analyzer", version, about, long_about = None)]
struct Cli {
    /// Demo (.lmp) files to analyze.
    #[arg(value_name = "FILE", action = clap::ArgAction::Append)]
    files: Vec<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    run(cli);
    ExitCode::SUCCESS
}

fn run(cli: Cli) {
    println!(
        "doom-demo-analyzer {} (doom-engine {})",
        env!("CARGO_PKG_VERSION"),
        doom_engine::VERSION
    );
    println!(
        "Analyzes DOOM demo recordings (.lmp): header metadata, gametic counts and the ticcmd stream."
    );
    if cli.files.is_empty() {
        println!("No files given. Pass one or more demo files to analyze.");
    } else {
        println!(
            "Demo parsing is not implemented yet; {} file(s) will be analyzed once it is.",
            cli.files.len()
        );
    }
}
