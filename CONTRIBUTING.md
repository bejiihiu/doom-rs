# Contributing

See [docs/contributing.md](docs/contributing.md) for the full guide:
ground rules, workflow, tests, and style.

The short version: branch from `main`, keep the engine behaviorally
faithful to the original DOOM, never add game data to the repository,
and make sure `cargo fmt --all`, `cargo clippy --workspace
--all-targets -- -D warnings`, and `cargo test --workspace` are green
before opening a PR.
