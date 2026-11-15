//! `doom-wad-inspector` — command-line inspector for DOOM WAD archives.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

/// Inspects DOOM WAD archives: lump directories, IWAD/PWAD stacking and
/// structural validation.
#[derive(Parser)]
#[command(name = "doom-wad-inspector", version, about, long_about = None)]
struct Cli {
    /// WAD files to inspect.
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
        "doom-wad-inspector {} (doom-engine {})",
        env!("CARGO_PKG_VERSION"),
        doom_engine::VERSION
    );
    println!(
        "Inspects DOOM WAD archives: lump directories, IWAD/PWAD stacking and structural validation."
    );
    if cli.files.is_empty() {
        println!("No files given. Pass one or more WAD files to inspect.");
    } else {
        println!(
            "WAD reading is not implemented yet; {} file(s) will be inspectable once it is.",
            cli.files.len()
        );
    }
}
