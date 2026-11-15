//! `doom-asset-inspector` — dumps DOOM engine assets to image files.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

/// Dumps DOOM engine assets (PLAYPAL palettes, COLORMAP tables, patches and
/// flats) to image files for inspection.
#[derive(Parser)]
#[command(name = "doom-asset-inspector", version, about, long_about = None)]
struct Cli {
    /// WAD files to extract assets from.
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
        "doom-asset-inspector {} (doom-engine {})",
        env!("CARGO_PKG_VERSION"),
        doom_engine::VERSION
    );
    println!("Dumps DOOM engine assets (palettes, colormaps, patches and flats) to image files.");
    if cli.files.is_empty() {
        println!("No files given. Pass one or more WAD files to extract assets from.");
    } else {
        println!(
            "Asset decoding is not implemented yet; {} file(s) will be dumpable once it is.",
            cli.files.len()
        );
    }
}
