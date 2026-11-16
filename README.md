# doom-rs

Original gameplay. Modern internals. Pure Rust.

[![CI](https://github.com/bejiihiu/doom-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/bejiihiu/doom-rs/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-GPL--2.0--or--later-blue.svg)](LICENSE)

## What this is

doom-rs is a rewrite of the classic DOOM engine (id Software, 1993;
referenced against the GPL-licensed `linuxdoom-1.10` source) in safe Rust.
The goal is behavioral fidelity: the same simulation model, the same
fixed-point math, the same gametic timing, so that demos and net games
behave the way the original does. The engine is a library; the windowed
binary is a thin shell on top of it.

**Status: work in progress.** The engine is feature complete and the test
suite is green, and the windowed shell builds and opens a 320x200 view.
It has not yet had an end-to-end run with a real IWAD — that verification
is in progress. Expect the first real experience to be rough; do not
delete your original executable.

## Features

- `engine/` — the `doom-engine` library: fixed-point math, WAD handling,
  the software renderer (columns, spans, visplanes, vissprites), BSP
  traversal and clipping, the full gameplay layer (monsters, weapons,
  specials, intermission, finale), save games, `.lmp` demo record/play,
  and UDP net play, ported from `linuxdoom-1.10`.
- `doom-rs/` — the runnable binary (`doom-rs`): winit + softbuffer window
  with integer scaling, cpal audio out, gamepad support via gilrs.
- `tools/wad-inspector` — WAD directory inspector and validator.
- `tools/asset-inspector` — dumps PLAYPAL/COLORMAP/patches/textures/flats/sprites to PNG.
- `tools/demo-analyzer` — parses `.lmp` demos: headers, tic stats, comparisons.

## Requirements

- A stable Rust toolchain (edition 2024). The repository pins `stable`
  via `rust-toolchain.toml`; no nightly features are used.
- x86_64 hosts (Windows, Linux, macOS). MSVC and windows-gnu both work;
  on Windows, `windows-gnu` needs `mingw-w64` on your `PATH` (see
  [docs/building.md](docs/building.md)).
- A legally owned IWAD (`doom1.wad`, `doom.wad`, `doom2.wad`, or an
  ultimate/Plutonia/TNT variant). **No game data is included.**
- Linux builds need the ALSA development headers (`libasound2-dev`)
  for the audio backend.

## Quick start

Clone the repository and build a release binary. Download the shareware
`DOOM1.WAD` from id Software's freely distributable copy (or point
`-iwad` at your own `doom2.wad` / `doom.wad` / `doomu.wad`). Run the
binary with `-iwad` pointing at the file.

Your `doom.cfg` is read from and written to the current directory by
default; save games and demos land there too. **Do not commit the WAD,
your config, or your save games** — the repository's ignore rules exist
so they stay yours.

See [docs/building.md](docs/building.md) for toolchain details and
[docs/compatibility.md](docs/compatibility.md) for what is currently
known to work.

## Controls

Defaults (rebinding goes through `doom.cfg`):

- **Arrows / WASD** — move and turn (vanilla layout)
- **Ctrl or left mouse button** — fire
- **Space or right mouse button** — use
- **Shift** — run (speed modifier)
- **Alt or Q** — strafe modifier
- **, / .** — strafe left / right
- **Tab** — automap
- **Escape** — menu
- **1–9** — select weapon
- **F1** — Read This (help), **F2** — save, **F3** — load,
  **F5** — detail level, **F6** — quicksave, **F7** — end game,
  **F8** — toggle messages, **F9** — quickload, **F10** — quit prompt
- **Mouse wheel** — cycle weapons
- **Gamepad** — right stick turns, left stick strafes/moves forward,
  D-pad acts as the arrows, right trigger fires, left trigger uses,
  South uses, East fires, West holds run, Start opens the menu,
  Select opens the automap, North cycles weapons. Deadzone 0.25.

Net play is UDP with explicit unicast addresses (vanilla packet format;
broadcast discovery does not exist in the 1.10 sources). See
`doom-rs --help` for the command line.

## Install

There is no published package or release yet; install means building
from source:

1. Clone the repository.
2. Build the release binary.
3. Put the binary somewhere on your `PATH` (optional; it runs from
   wherever it is).

Prebuilt binaries will be attached to GitHub releases once the first
milestone ships; until then, building from source is the only path.
`cargo install` is not applicable — the crates are not published.

## Documentation

- [docs/architecture.md](docs/architecture.md) — the engine's design:
  layout, data model, the simulation pipeline, the renderer, net code.
- [docs/building.md](docs/building.md) — how to build and run, on all
  three platforms.
- [docs/porting-map.md](docs/porting-map.md) — file-by-file disposition
  of the original C source tree.
- [docs/compatibility.md](docs/compatibility.md) — supported features,
  known limitations.
- [docs/contributing.md](docs/contributing.md) — how to contribute:
  PR flow, tests, clippy.

## License

doom-rs is **GPL-2.0-or-later**. The original `linuxdoom-1.10` sources
are GPL-licensed by id Software and form the behavioral reference for
this rewrite. No IWAD data files are distributed with this project;
you need your own legally obtained IWAD to run the game.

## Credits

- id Software — the original DOOM engine.
- `linuxdoom-1.10` — the GPL Linux port that serves as the reference
  behavior for this rewrite.
- [cachegirl](https://crates.io/crates/cachegirl) (by bejiihiu) — atomic
  write staging for config and save games.
- [winit](https://crates.io/crates/winit) — windowing.
- [softbuffer](https://crates.io/crates/softbuffer) — framebuffer present.
- [cpal](https://crates.io/crates/cpal) — audio output.
- [opl-audio](https://crates.io/crates/opl-audio) — OPL FM synthesis for MUS music.
- [criterion](https://crates.io/crates/criterion) — benchmarks.
- [thiserror](https://crates.io/crates/thiserror) — error types.
- [clap](https://crates.io/crates/clap) — command-line interface.
