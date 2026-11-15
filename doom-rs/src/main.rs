//! `doom-rs` — the modern DOOM engine rewritten in Rust.

use std::process::ExitCode;

use clap::Parser;

/// A modern rewrite of the DOOM engine in Rust.
#[derive(Parser)]
#[command(name = "doom-rs", version, about, long_about = None)]
struct Cli {}

fn main() -> ExitCode {
    let cli = Cli::parse();
    run(cli)
}

fn run(_cli: Cli) -> ExitCode {
    println!(
        "doom-rs {} (doom-engine {}) — the engine is still being assembled; the game is not playable yet.",
        env!("CARGO_PKG_VERSION"),
        doom_engine::VERSION
    );
    ExitCode::SUCCESS
}
