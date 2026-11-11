# Marathon

Marathon recreates the lean-back experience of television from a personal
video collection. You define **channels**, each a set of videos plus a rule
for what plays next. Flip to a channel and it resumes where you left off; let
it run and it keeps going.

**Status:** early development. Milestones M0 (scaffold), M1 (rules engine),
and M2 (server) are done. M3 (minimal desktop client: video, keyboard, no
overlay), M3.5 (seams: pluggable input, `Action`/`Event` control plane,
skipping unplayable items, windowing kept at the edge), M4 (embedded
mode: the desktop client runs its own server), M5 (on-screen overlay,
mouse, and gamepad), M6 (remote control and events over the API), M7
(HDMI-CEC: the TV's remote, and pausing when the TV switches away), and M8
(Jellyfin libraries as a source) are implemented and awaiting their manual
checks. M9 (macOS) is not started; it needs Mac hardware. M10 (the Android
client, [`docs/ANDROID.md`](docs/ANDROID.md)) is implemented and builds an
APK, but hasn't been run on a device yet. [`PLAN.md`](PLAN.md) has the design, API, and
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
| `crates/marathon-input-cec` | HDMI-CEC input (TV remote, input switching) via libcec |
| `apps/marathond` | Server binary |
| `apps/marathon-desktop` | Desktop client binary |
| `android/` | Android client (phones and Android TV): Gradle project with `:api` (generated), `:core` (Tuner port), `:app` |
| `config/example.toml` | Annotated example configuration |
| `scripts/make-test-library.sh` | Generates a fake video library for testing |
| `docs/CONTROLS.md` | User guide: watching Marathon and its controls |
| `docs/REMOTE.md` | Remote control and events over the API |
| `docs/ANDROID.md` | Android client (M10): design, toolchain setup, manual checks |
| `docs/TODO.md` | Known follow-ups |
| `examples/` | Shell scripts that send actions and follow events |

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

HDMI-CEC support (the TV's remote) is optional and off by default. To build
it, add libcec's development files (libcec 4 or newer: `libcec-devel` on
Fedora, `libcec-dev libp8-platform-dev` on Debian/Ubuntu, `brew install
libcec` on macOS) and build with `--features input-cec`. libcec is
GPL-2.0-or-later, so such builds are covered by the GPL.

Every change must pass:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs these on Linux and macOS (`.github/workflows/ci.yml`), plus
`scripts/check-seams.sh`, which checks that `winit`/`glutin`/`gilrs` stay
inside `apps/marathon-desktop/src/platform/` and out of `marathon-client`.

### The Android client

Needs JDK 25 and the Android SDK (platform 37, build-tools 37); setting
them up, including on Bazzite/Silverblue, is in
[`docs/ANDROID.md`](docs/ANDROID.md). Then:

```sh
cd android
./gradlew check :app:assembleDebug     # tests, lint, and the APK
adb install app/build/outputs/apk/debug/app-debug.apk
```

The Kotlin API client is generated from `android/api/openapi.json`, a
snapshot of the server's API. After changing the HTTP API or the
`Action`/`Event` types, refresh it with `scripts/gen-android-api.sh`
(`cargo test` fails until you do). The Android Tuner is a port of
`marathon-client`'s; `scripts/check-tuner-parity.sh` checks that every
Rust tuner test has a Kotlin twin.

### Running the server

```sh
cargo run -p marathond -- --config path/to/config.toml
```

See [`config/example.toml`](config/example.toml) for the config format.

By default the server listens on `127.0.0.1:7878`, so only the same machine
can reach it. To serve other devices on the network (the Android app, a
desktop client on another machine), set `server.bind` in the config, or
override it with `--bind`:

```sh
cargo run -p marathond -- --config path/to/config.toml --bind 0.0.0.0:7878
```

A non-loopback address requires `server.token`; clients then send it as a
bearer token (`--token` on the desktop client).
Sources are folders (`kind = "filesystem"`, including mounted network
shares) or Jellyfin servers (`kind = "jellyfin"`, with a `url` and an
`api_key`). A Jellyfin library appears as a folder named after it, so
channels select from it the same way; Marathon streams its files through
itself, so players only ever talk to Marathon. The
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

**Controls:** see [`docs/CONTROLS.md`](docs/CONTROLS.md), the user guide
for keyboard, mouse, and game controller. In short: PgUp/PgDn (or the
arrows, the controller's shoulder buttons, or the mouse wheel) change
channel; G opens the guide; Space pauses; N/P skip episodes; I shows
what's on; Q quits.

**TV remote (HDMI-CEC):** built with `--features input-cec` and run with
`--cec`, the TV's own remote controls Marathon, and switching the TV to
another input (or turning it off) pauses it. Marathon never switches or
powers the TV by itself. See the TV remote section of
[`docs/CONTROLS.md`](docs/CONTROLS.md).

**Remote control:** the client keeps a connection open to its server,
so anything else can drive it and follow along over the API, for example
`curl -X POST localhost:7878/api/v1/clients/default/actions -d
'{"action":"channel_up"}'`. See [`docs/REMOTE.md`](docs/REMOTE.md) and
the scripts in [`examples/`](examples).

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
