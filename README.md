# Marathon

Marathon recreates the lean-back experience of television from a personal
video collection. You define **channels**, each a set of videos plus a rule
for what plays next. Flip to a channel and it resumes where you left off; let
it run and it keeps going.

**Status:** early development. Milestones M0 (scaffold), M1 (rules engine),
and M2 (server) are done. M3 (minimal desktop client: video, keyboard, no
overlay), M3.5 (seams: pluggable input, `Action`/`Event` control plane,
skipping unplayable items, windowing kept at the edge), and M4 (embedded
mode: the desktop client runs its own server), and M5 (on-screen overlay,
mouse, and gamepad) are implemented and awaiting their manual checks. [`PLAN.md`](PLAN.md) has the design, API, and
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

Requires Rust stable (edition 2024, 1.95 or newer) and libmpv with its
headers and `pkg-config` file (client API 2.0+, i.e. mpv 0.35 or newer).
On Linux, gamepad support also needs libudev's headers (or build the
desktop app with `--no-default-features` to leave gamepads out):

| Platform | Packages |
|---|---|
| Fedora (incl. a toolbox/distrobox on Silverblue, Bazzite, ...) | `sudo dnf install mpv-devel systemd-devel` |
| Debian / Ubuntu | `sudo apt install libmpv-dev libudev-dev` |
| macOS | `brew install mpv pkgconf` |

Every change must pass:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs these on Linux and macOS (`.github/workflows/ci.yml`), plus
`scripts/check-seams.sh`, which checks that `winit`/`glutin`/`gilrs` stay
inside `apps/marathon-desktop/src/platform/` and out of `marathon-client`.

### Running the server

```sh
cargo run -p marathond -- --config path/to/config.toml
```

See [`config/example.toml`](config/example.toml) for the config format. The
API is served under `/api/v1` (PLAN.md §8); its OpenAPI document is at
`/api/v1/openapi.json`. The SQLite database lives in `data_dir`. Set
`RUST_LOG=debug` for more logging.

For example, against the test library (see [Test media](#test-media)):

```sh
curl -s localhost:7878/api/v1/channels | jq
curl -s localhost:7878/api/v1/channels/<id>/now | jq
mpv http://localhost:7878/api/v1/stream/<item-id>
```

### Running the desktop client

With a server running:

```sh
cargo run -p marathon-desktop -- --windowed
```

Or, with no separate server, run one inside the app (embedded mode):

```sh
cargo run -p marathon-desktop -- --embedded --config path/to/config.toml --windowed
```

The embedded server reads the same config as `marathond` and uses its
`data_dir`. It listens on `127.0.0.1` only, on a port the OS picks
(`server.bind` is ignored), and stops when the app quits. If the config
sets a `token`, the app sends it. Embedded mode and a standalone
`marathond` share state only if they use the same `data_dir`.

`--server URL` (or `MARATHON_SERVER`) points it at a server other than
`http://127.0.0.1:7878`, `--token` (or `MARATHON_TOKEN`) supplies a bearer
token, and `--channel N` picks the starting channel; by default it resumes
the channel this client watched last. Without `--windowed` it starts
fullscreen. Set `RUST_LOG=info,mpv=debug` to see mpv's own log.

On screen, a channel banner (number, name, item, progress) appears as soon
as a channel switch starts, and for a few seconds on info. The guide lists
every channel and what it is on. Errors show briefly in the top right. The
mouse cursor hides after 3s without mouse activity.

| Key | Gamepad | Mouse | Does |
|---|---|---|---|
| PgUp / PgDn | Right / left shoulder | Wheel up / down | Channel up / down |
| Up / Down | D-pad up / down | | Channel up / down; in the guide, move the highlight |
| Left / Right | D-pad left / right | | Seek −10s / +10s (nothing in the guide) |
| Enter | A (South) | Click | In the guide, tune to the highlighted channel; otherwise show info |
| digits, then Enter or a pause | | | Go to channel number |
| Space | Start | | Pause / resume |
| N / P | Right / left trigger | | Next / previous item |
| S | X (West) | | Cycle subtitles (on/off with one track) |
| F | Left stick click | | Toggle fullscreen |
| I | Select (Back/View) | | Show the banner |
| G | Y (North) | | Open / close the guide |
| Esc | B (East) | | Close the guide (or the banner) |
| Q | Mode (Xbox/PS button) | | Quit (saves the position) |

Controllers can be plugged in and out while the app runs. Holding
Left/Right (key or D-pad) keeps seeking.

Items that cannot be played (e.g. a file renamed after the last scan) are
skipped with an on-screen message; after five failures in a row the
client stops trying until you change channel or press N.

The client stores only a random client ID, in `marathon/client-id` under
the user's config directory (`~/.config` on Linux).

### Test media

```sh
scripts/make-test-library.sh test-library
```

needs ffmpeg built with the `drawtext` filter. It writes labelled
test-pattern clips: a three-season show, loosely named "essays", a sidecar
subtitle, and an awkward unicode filename. Clips are 6s long by default;
set `DURATION=<seconds>` for longer ones. Clips under 30s count as finished
as soon as they have started (PLAN.md §1), so checking that channels resume
needs e.g. `DURATION=120`. Each frame shows the
clip's filename and timestamp. Generated media is git-ignored; never commit
it.

It also writes `test-library/marathon.toml`, a config with one channel per
programming rule over that library, so serving it is one command:

```sh
cargo run -p marathond -- --config test-library/marathon.toml
# or, without a separate server:
cargo run -p marathon-desktop -- --embedded --config test-library/marathon.toml --windowed
```

The server's database goes in `test-library/.marathon/`; delete that
directory to reset every channel to its start.

## License

MIT OR Apache-2.0 (provisional; see PLAN.md §12).
