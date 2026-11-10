# Marathon — working notes for Claude Code

Read `PLAN.md` first. It defines the semantics, architecture, API, and
milestones. Work one milestone at a time, in order, and do not start the next
until the current one meets its "done when" criteria.

## Conventions

- Rust stable, edition 2024, Cargo workspace. Layout as in PLAN.md §4.
- `thiserror` for library error types, `anyhow` only in binaries.
- `tracing` for all logging; no `println!` outside CLI output.
- No `unwrap()`/`expect()` outside tests, except for invariants with a comment
  explaining why they cannot fail.
- `marathon-rules` must stay pure: no I/O, no clocks, no global RNG.
- `sqlx` with runtime-checked queries (`query_as`), not the compile-time macros.
- Before adding a dependency, check its current version and maintenance status.
  `egui`, `egui_glow`, `winit`, and `glutin` versions must be mutually
  compatible; pick `egui` first and follow its pins.

## Definition of done (every change)

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Testing

- Generate the test library with `scripts/make-test-library.sh` (needs ffmpeg).
  Never commit generated media.
- Server integration tests build libraries in `tempfile` directories.
- The Tuner state machine is tested against a mock `Player`, not real mpv.

## Environment

- Linux dev may happen inside a Fedora toolbox/distrobox. libmpv headers come
  from the distro's mpv development package (name varies by distro; check);
  macOS uses `brew install mpv`.
- When something can only be verified by a human watching the screen (fades,
  frame pacing, playback), say so explicitly and describe what to check, rather
  than claiming it works.
