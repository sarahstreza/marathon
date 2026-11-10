# Marathon

Marathon recreates the lean-back experience of television from a personal
video collection. You define **channels**, each a set of videos plus a rule
for what plays next. Flip to a channel and it resumes where you left off; let
it run and it keeps going.

**Status:** early development. Milestone M0 (scaffold) is done; the server and
desktop client are stubs. [`PLAN.md`](PLAN.md) has the design, API, and
milestone checklists. Read it before changing anything substantial.

> **AI-generated project.** Everything in this repository, including the
> plan (`PLAN.md`) and agent instructions (`CLAUDE.md`), is written by AI
> (Anthropic's Claude) under human direction. The code, tests, and scripts
> are written with Claude Code. A human checks the parts that need eyes on
> a screen, as listed in each milestone's `[manual]` criteria. Commits made
> by the AI carry a `Co-Authored-By: Claude` trailer.

## Layout

| Path | What it is |
|---|---|
| `crates/marathon-api` | HTTP API types shared by server and clients |
| `crates/marathon-rules` | Pure programming-rules engine (no I/O) |
| `crates/marathon-server` | Server library: config, catalog, database, routes, streaming |
| `crates/marathon-client` | Client library: API client, Tuner state machine, input actions |
| `crates/marathon-mpv` | Safe libmpv wrapper |
| `apps/marathond` | Server binary |
| `apps/marathon-desktop` | Desktop client binary |
| `config/example.toml` | Annotated example configuration |
| `scripts/make-test-library.sh` | Generates a fake video library for testing |

Which crates may depend on which is fixed by PLAN.md §4 and enforced by
`apps/marathond/tests/dependency_graph.rs`.

## Development

Requires Rust stable (edition 2024). Every change must pass:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs these on Linux and macOS (`.github/workflows/ci.yml`).

### Test media

```sh
scripts/make-test-library.sh test-library
```

needs ffmpeg built with the `drawtext` filter. It writes about two minutes
of labelled test-pattern clips: a three-season show, loosely named "essays",
a sidecar subtitle, and an awkward unicode filename. Each frame shows the
clip's filename and timestamp. Generated media is git-ignored; never commit
it.

## License

MIT OR Apache-2.0 (provisional; see PLAN.md §12).
