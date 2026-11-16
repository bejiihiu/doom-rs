# Contributing

Thanks for wanting to hack on doom-rs. Read this page first; it keeps
review fast and keeps the engine honest.

## Ground rules

- **Behavioral fidelity first.** The reference for every gameplay
  decision is `linuxdoom-1.10` (id Software, GPL). If a change
  deviates from the original on purpose, the deviation must be
  documented in the module docs with a reason, or it doesn't belong
  here. "Because the original was weird" is often the right reason.
- **No game data in the repository.** IWADs, save games, configs,
  demos recorded with a real IWAD — none of it gets committed. Tests
  build synthetic WADs in memory.
- **No stray additions.** No widescreen, no OpenGL, no entity-component
  rewrite, no `unsafe` without a documented justification *and* tests.
  Determinism is a hard requirement: nothing wall-clock or float enters
  the simulation.

## Layout

See `docs/architecture.md` for the map. The short version: the
`engine/` library holds all game logic and never touches a window,
`doom-rs/` is the platform shell (winit/softbuffer/cpal/gilrs), and
`tools/` contains CLI utilities that consume `doom-engine` as a normal
dependency. If you find yourself editing `Cargo.toml`, think twice —
only touch what your change needs, with a reason.

## Workflow

1. **Branch** from `main` (`feature/<short-name>`). `main` is the only
   long-lived branch.
2. **Change one thing at a time.** Engine changes land in `engine/`,
   shell changes in `doom-rs/`, tool changes in `tools/`. If a change
   spans layers, split it.
3. **Prove it works.** Everything below must be green locally before
   you open a PR:

   ```
   cargo fmt --all
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   cargo doc --workspace --no-deps
   ```

4. **Open a PR** against `main` and fill in the description honestly.
   If something is incomplete, say so; known gaps with a clear note
   beat a PR that hides them.
5. **Review** — maintainer reviews, then squash-merge. Conflicts are
   resolved before merge, CI must be green, no merges with failing
   tests.

## Tests

- Unit tests live next to the code (`#[cfg(test)]` modules). Write them
  like specifications of vanilla behavior, not like assertions about
  implementation details.
- Integration tests for the whole engine live in `engine/tests/`
  (headless boot, PNG snapshot, determinism, compatibility runner).
  They use a synthetic WAD fixture from `engine/tests/common/` — add
  lumps there if your change needs a new fixture, and keep the fixture
  deterministic.
- Determinism is tested by running the same scripted session twice and
  comparing state tic-for-tic. If you touch the tic loop, mobj
  thinkers, or RNG call sites, expect that test to be the judge.
- If you fix a bug, add the test that fails without your fix.

## Style

- Format with `cargo fmt`; lint with clippy at `-D warnings`.
- Comments in code are informal English (`// like this`), human and
  specific. User-facing strings (docs, errors, UI) are proper English.
- Don't narrator-document what the code does. Document *why* —
  especially for deviations from the original, which always need a
  reason next to them.
- Commit messages: Conventional Commits, subject ≤50 chars
  (`fix(menu): tolerate missing patches`).

## What to work on

- Items in `docs/compatibility.md` under "Known limitations" are the
  honest list of gaps. Picking one up is welcome; say so in an issue or
  the PR so work isn't duplicated.
- The first end-to-end run with a real IWAD is the critical open item.
  If you can test with a legal IWAD on your machine, reports of what
  breaks are extremely valuable — include your platform and IWAD
  variant.

## Reporting bugs

Open a GitHub issue with:

- platform and `doom-rs --version` (or the commit),
- the IWAD family (no file, just "DOOM II" / "Ultimate Doom" / etc.),
- exact command line and what you expected vs. saw,
- whether a demo is involved (attach the `.lmp` if you have one —
  demos don't contain copyrighted data and are the best bug reports
  there are).
