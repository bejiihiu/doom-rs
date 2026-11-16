# Building doom-rs

This document explains how to build and run doom-rs from a fresh clone.
For what is currently known to work, see [compatibility.md](compatibility.md).

## Prerequisites

- **Rust**: a stable toolchain, edition 2024. The repository pins the
  `stable` channel via `rust-toolchain.toml`, so `rustup` will pick it
  up automatically. No nightly features are used.
- **An IWAD**: doom-rs does not ship game data. You need a legally
  obtained IWAD (`doom1.wad` shareware, `doom.wad`, `doom2.wad`,
  `doomu.wad` / `plutonia.wad` / `tnt.wad`).
- **A C linker** and platform SDK bits, as usual for Rust:

| Platform | What you need |
|---|---|
| Windows (MSVC) | Visual Studio Build Tools (MSVC + Windows SDK) |
| Windows (GNU) | `stable-x86_64-pc-windows-gnu` toolchain + [mingw-w64](https://www.mingw-w64.org/) |
| Linux | `build-essential` (or equivalent), `pkg-config`, **ALSA headers** (`libasound2-dev` on Debian/Ubuntu) |
| macOS | Xcode Command Line Tools |

## Windows: MSVC vs GNU

The default `stable` toolchain on Windows is MSVC. If `link.exe` is not
available (no Visual Studio installation), the build will fail at the
link step. Two options:

**Option A — MSVC.** Install Visual Studio Build Tools with the
"Desktop development with C++" workload. Then:

```
cargo build --workspace --release
```

**Option B — GNU.** Install the GNU toolchain and mingw-w64:

```
rustup toolchain install stable-x86_64-pc-windows-gnu
```

Prepend your mingw-w64 `bin` directory to `PATH` (for example
`C:\mingw64\bin`), then either set the toolchain for your session:

```
set RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-gnu
cargo build --workspace --release
```

or use `rustup override set stable-x86_64-pc-windows-gnu` inside the
repository. To verify everything is wired up before a full build:

```
cargo check --workspace --locked
```

## Linux and macOS

```
sudo apt-get install libasound2-dev   # Debian/Ubuntu only; ALSA headers are required by cpal
cargo build --workspace --release
```

macOS: `xcode-select --install`, then the same cargo command. No extra
system libraries are needed.

## Running

The release binary lands in `target/release/` (`doom-rs.exe` on
Windows, `doom-rs` elsewhere). Run it with `-iwad` pointing at your
IWAD:

```
doom-rs -iwad /path/to/doom2.wad
```

The IWAD is resolved in this order: `-iwad` on the command line, the
`DOOM_IWAD` environment variable, then the current working directory
(`doom2.wad`, `doom2f.wad`, `plutonia.wad`, `tnt.wad`, `doomu.wad`,
`doom.wad`, `doom1.wad`). If the IWAD is missing or unreadable, the
binary prints a hint and exits with status 1.

Useful flags (see `doom-rs --help` for the full list):

- `--scale N` — integer scale factor, 1–8 (default 4)
- `--fullscreen` — borderless fullscreen
- `-warp E M` / `-warp N` — start on a specific map
- `-skill 1..=5` — difficulty
- `-file PWAD ...` — layer PWADs on top of the IWAD
- `-playdemo`, `-timedemo`, `-record` — demo playback (DOOM-style)
- `-net` — net play; see `doom-rs --help` for the UDP setup

The viewport is always 320×200 internally; the shell scales it up with
nearest-neighbour. Game state is deterministic: identical input
sequences produce identical runs.

## Development loop

```
cargo test --workspace                     # unit + integration tests (no IWAD needed)
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo doc --workspace --no-deps            # rustdoc; must stay warning-free
cargo bench -p doom-engine                 # criterion benches
```

The test suite builds synthetic WADs in memory; no copyrighted game
data is required or committed. `target/` is git-ignored and can grow to
several gigabytes; run `cargo clean` if your disk fills up.

## Continuous integration

GitHub Actions run `fmt`, `clippy -D warnings`, `cargo test`, a release
build and `cargo doc` on Windows, Linux and macOS. Linux CI installs
`libasound2-dev` for the audio backend. If you add a system dependency,
`.github/workflows/` must learn about it in the same change.
