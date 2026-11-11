# Marathon — Project Plan

Marathon recreates the lean-back experience of television from a personal video
collection. The user defines **channels**, each a list of **items** (episodes,
films, essays) plus a **programming rule** that decides what plays next. Flip to a
channel and it resumes exactly where you left off; let it run and it keeps going.

This document is the build plan for the MVP: a Rust server and a Rust desktop
client for Linux and macOS, with an Android client to follow.

---

## Product principle: Marathon is HDMI input 4

The user's Apple TV or PS5 is on input 1. It is their media hub: polished,
powerful, the main thing they do on the TV. Marathon is not trying to replace
it. Marathon is the thing on input 4, the PC app on a second monitor, the phone
propped up while you eat: background television you can glance up at while
cleaning the kitchen. Because it has to fit into the user's life rather than
be the centre of it, it must be **extensible**. When in doubt, decisions follow
from this:

- **Bendable over polished.** Every way to control or observe Marathon is a
  pluggable adapter (keyboard, gamepad, CEC, HTTP, home automation, AI agents,
  things we haven't thought of). The network API is a first-class interface,
  not an afterthought.
- **Polite.** Marathon never takes over the TV or the house uninvited. It does
  not switch the TV's input on boot, does not power the TV on or off unless
  configured to, and coexists with whatever is on input 1.
- **Glanceable.** UI must be readable from across a room. Nothing blocks
  playback waiting for a response; there is no "are you still watching?".
- **Keeps going.** The default state is "playing something". Interruptions
  (errors, missing files, unreachable sources) skip ahead and log rather than
  stopping the channel.

---

## 1. Core semantics

Get these right before anything else; every component depends on them.

- **Channels resume; they do not broadcast.** A channel is frozen while nobody is
  watching it. Tuning in returns the item that was playing and its position.
  There is no wall clock, no EPG with start times.
- **Item**: one playable video. Has a stable server-assigned ID, a source, a path
  within that source, parsed metadata (show / season / episode / title), and a
  duration once known.
- **Programming rule**: a pure function `(rule, items, cursor) -> (next item, new cursor)`.
  The cursor is opaque serialized state owned by the rule (permutation seed,
  index, group position, etc.).
- **Position**: per channel, the current item and the playback offset within it.
- **Finished**: an item counts as finished when the player reaches end of file,
  or when tuning in finds the saved position within `max(30s, 2% of duration)` of
  the end. Finished items advance the channel.
- **Resume rewind**: on tune-in, start a few seconds (default 5s) before the saved
  position so the viewer regains context.
- **One server is the source of truth.** Clients hold no durable playback state.
  Multiple clients may watch the same channel; last write wins, and advancing is
  idempotent (see `advance` in the API).

---

## 2. Architecture

```
┌───────────────────────────────┐        ┌────────────────────────────────────┐
│ Client (per platform)         │  HTTP  │ Server (marathond, or in-process)  │
│  • libmpv playback            │◄──────►│  • Config + channel definitions    │
│  • Overlay UI, input          │  JSON  │  • Catalog: scan sources → items   │
│  • Tuner state machine        │        │  • Rules engine (marathon-rules)   │
│  • Reports progress           │◄───────│  • Playback state + event log      │
└───────────────────────────────┘ stream │  • Media streaming (HTTP + Range)  │
                                         │  • Sources: filesystem, Jellyfin…  │
                                         └────────────────────────────────────┘
```

### Key decisions

1. **Everything playable is an HTTP URL served by the Marathon server**, with
   full `Range` support. Players never see SMB, NFS, Jellyfin, or anything else.
   This means libmpv (and any future player) only ever needs to open an HTTP URL;
   no custom stream callbacks are required. Sources are a server-side concern.
2. **Embedded mode uses the same HTTP path.** The desktop client can start the
   server in-process on a Tokio runtime, bound to `127.0.0.1:0` (OS-assigned
   port). There is exactly one code path for clients. (Loopback TCP rather than a
   Unix socket, because libmpv must be able to fetch the stream URLs.)
3. **The server owns all logic.** Scheduling, rules, state, history. Clients are
   thin: play what they're told, report what happened, render UI, map input.
   This keeps the Android client small and avoids sharing Rust code across a
   language boundary for the client side.
4. **The API contract is OpenAPI.** Shared Rust types live in `marathon-api`;
   the server publishes an OpenAPI document (via `utoipa`) from which the
   Android/Kotlin client is generated later.
5. **Event log plus derived state.** Every playback event is appended to an
   `events` table. Current position lives in `channel_state` for fast reads, but
   history, stats, and search are derived from `events`.
6. **Control is pluggable in both directions.** Inputs are adapters that emit
   `Action`s; observers subscribe to state events. Local adapters run in the
   client; remote adapters reach the client through the server. See §9,
   "Input and control".
7. **Platform edges stay at the edges.** Windowing and input libraries (`winit`,
   `glutin`, `gilrs`) never leak into the renderer, Tuner, or overlay UI, so a
   bare-metal DRM/KMS backend can be added later for an appliance build (§13).

### Consequences to remember

- A client that cannot reach its server has no channels. For remote use, the
  expected answer is the user's own VPN (Tailscale etc.). Offline-first sync is a
  non-goal.
- **Sidecar subtitles do not auto-load over HTTP.** mpv only discovers
  `Episode.en.srt` next to `Episode.mkv` for local paths. The server must detect
  sidecar subtitle files during scan, expose them on the item, and serve them;
  the client adds them with mpv's `sub-add` command. Embedded subtitles work
  without any of this.
- Durations come from the client (mpv reports them) or from catalog providers
  that know them (Jellyfin). The MVP does not run ffprobe on the server.

---

## 3. Stack

| Concern | Choice | Notes |
|---|---|---|
| Language | Rust stable, edition 2024 | Cargo workspace |
| Async runtime | `tokio` | |
| HTTP server | `axum` + `tower-http` | `ServeFile` handles `Range` for local files |
| HTTP client | `reqwest` (rustls) | Client API calls; Jellyfin proxying |
| Database | SQLite via `sqlx` | Runtime-checked queries (`query_as`), not compile-time macros; `sqlx` migrations |
| Serialization | `serde`, `serde_json`, `toml` | |
| IDs | `uuid` v7 | Time-sortable |
| OpenAPI | `utoipa` | Spec served at `/api/v1/openapi.json` |
| Logging | `tracing`, `tracing-subscriber` | |
| Errors | `thiserror` in libraries, `anyhow` in binaries | |
| Windowing / GL | `winit` + `glutin` + `glow` | One window, one GL context |
| Video | libmpv, OpenGL render API | Thin hand-written bindings in `marathon-mpv`, linked via `pkg-config` (chosen over `libmpv2` in M3; see §12) |
| Overlay UI | `egui` via `egui_glow` | Drawn over the video each frame; isolated behind a module so it can be replaced. `egui_glow` without its `winit` feature, so the overlay needs no windowing library; egui 0.37 needs Rust 1.95 and its `default_fonts` feature |
| Gamepad | `gilrs` | |
| HDMI-CEC | libcec via `cec-rs` | Covers Raspberry Pi, the Linux kernel CEC framework, and Pulse-Eight USB adapters |
| Remote control / events | axum WebSocket + Server-Sent Events | Client control channel and event stream |
| Testing | `proptest`, `tempfile`, `tokio::test` | |

**Version alignment warning:** `egui-winit`/`egui_glow` pin specific `winit`
versions. Choose `egui` first, then use the `winit`/`glutin` versions it expects.
`winit` 0.30+ uses the `ApplicationHandler` trait API; write against that.

**Licensing:** Desktop and Android builds may link a standard (GPL) libmpv. If
an Apple App Store build ever happens, it will need mpv built with
`-Dgpl=false` and an LGPL-only FFmpeg; avoid depending on GPL-only mpv features
so that path stays open. Marathon's own code should use a GPL-compatible license.

---

## 4. Workspace layout

```
marathon/
  Cargo.toml                 # workspace
  CLAUDE.md
  PLAN.md
  config/
    example.toml
  scripts/
    make-test-library.sh     # generates a fake library of tiny ffmpeg clips
  crates/
    marathon-api/            # request/response types, Action and Event enums, OpenAPI derive
    marathon-rules/          # pure programming-rules engine; no I/O
    marathon-server/         # library: config, catalog, sources, db, routes, streaming, client relay
    marathon-client/         # library: API client, Tuner state machine, Player and InputSource traits, action bus
    marathon-input-cec/      # CEC InputSource (libcec), behind a cargo feature
    marathon-mpv/            # safe wrapper over libmpv incl. render API
  apps/
    marathond/               # server binary
    marathon-desktop/        # desktop client; optional --embedded server
```

Dependency direction: `marathon-rules` and `marathon-api` depend on nothing
internal. `marathon-server` uses both. `marathon-client` uses `marathon-api`.
`marathon-desktop` uses `marathon-client`, `marathon-mpv`, and (for embedded mode)
`marathon-server`.

---

## 5. Configuration

Channels are defined in TOML for the MVP. A UI for editing them comes later.

```toml
[server]
bind = "127.0.0.1:7878"
data_dir = "~/.local/share/marathon"   # SQLite lives here
# token = "change-me"                  # required if bind is not loopback

[[source]]
id = "nas"
kind = "filesystem"
root = "/mnt/nas/video"

[[channel]]
id = "office"
number = 1
name = "The Office"
items = [{ source = "nas", path = "TV/The Office (US)" }]
rule = { kind = "ordered" }

[[channel]]
id = "taskmaster"
number = 6
name = "Taskmaster"
items = [{ source = "nas", path = "TV/Taskmaster" }]
rule = { kind = "grouped", by = "season", groups = "shuffled", within = "ordered" }

[[channel]]
id = "essays"
number = 3
name = "Game Design Essays"
items = [
  { source = "nas", path = "YouTube/Game Design", recursive = true },
  { source = "nas", path = "YouTube/Level Design", recursive = true },
]
rule = { kind = "shuffled" }
```

Filesystem sources treat mounted SMB/NFS shares as ordinary paths, which covers
networked storage on desktop servers for the MVP.

---

## 6. Programming rules (`marathon-rules`)

Rules are data, composed from a small set of primitives:

- `ordered` — natural order (by season/episode when parsed, else natural sort of
  path). Loops at the end.
- `shuffled` — bag shuffle: every item plays once per cycle before any repeats.
  Reshuffle with a new seed per cycle, and avoid the last item of one cycle
  being the first of the next.
- `grouped { by, groups, within }` — partition items (`by = "season" | "folder"`),
  order the groups with `groups` (`ordered` | `shuffled`), and order items within
  each group with `within`. Taskmaster is `grouped(season, shuffled, ordered)`.

API shape (illustrative):

```rust
pub fn next(rule: &Rule, items: &[ItemKey], cursor: Option<&Cursor>) -> Step;
pub fn previous(rule: &Rule, items: &[ItemKey], cursor: &Cursor) -> Option<Step>;
pub struct Step { pub item: ItemKey, pub cursor: Cursor }
```

- `Cursor` is serde-serializable and stored as JSON in `channel_state`.
- Items may be added or removed between calls (library rescans). Rules must
  tolerate this: a missing current item advances gracefully; new items join the
  next cycle (for shuffles) or slot into order (for ordered).
- Randomness comes from a seed in the cursor (e.g. `rand_chacha`), never from
  global state, so behavior is reproducible in tests.

**Property tests required:** shuffled visits every item exactly once per cycle;
grouped never interleaves groups and preserves within-group order; ordered is
stable across rescans that do not change the item set; all rules handle 0 and 1
items.

---

## 7. Data model (SQLite)

```sql
sources       (id TEXT PK, kind TEXT, config_json TEXT)
items         (id TEXT PK, source_id TEXT, path TEXT, size INTEGER, mtime INTEGER,
               show TEXT, season INTEGER, episode INTEGER, title TEXT,
               duration_ms INTEGER NULL, subtitles_json TEXT,
               missing INTEGER DEFAULT 0, UNIQUE(source_id, path))
channels      (id TEXT PK, number INTEGER, name TEXT, rule_json TEXT, selectors_json TEXT)
channel_items (channel_id TEXT, item_id TEXT, PRIMARY KEY(channel_id, item_id))
channel_state (channel_id TEXT PK, item_id TEXT, position_ms INTEGER,
               cursor_json TEXT, updated_at INTEGER)
events        (id TEXT PK, ts INTEGER, client_id TEXT, channel_id TEXT,
               item_id TEXT, kind TEXT, position_ms INTEGER)
               -- kind: tuned | progress | ended | skipped | previous | paused | resumed
```

- Channels in config are upserted into `channels` on startup; config is
  authoritative for definitions, DB is authoritative for state.
- Files that disappear on rescan are marked `missing`, not deleted, so history
  survives temporary unmounts.
- `progress` events are throttled (client reports every ~5s); consider
  compacting old progress events later.

**Filename parsing** for show/season/episode: support `S01E02`, `s1e2`, `1x02`,
and a `Season N` parent folder. Fall back to natural sort by path.

---

## 8. HTTP API (v1)

All JSON. Prefix `/api/v1`. If a token is configured, require
`Authorization: Bearer <token>` on every route including streams (mpv sends it
via `http-header-fields`).

| Method | Path | Purpose |
|---|---|---|
| GET | `/health`, `/version` | Liveness, API version |
| GET | `/channels` | List channels with current item summary |
| GET | `/channels/{id}/now` | Current item, resume position, stream URL, subtitle URLs, and the **next** item's stream URL for prefetch |
| POST | `/channels/{id}/progress` | `{item_id, position_ms, duration_ms?, paused}` |
| POST | `/channels/{id}/advance` | `{from_item_id, reason: "ended" \| "skipped"}` → new `now`. **Idempotent:** if `from_item_id` is not the channel's current item, return the current `now` without advancing |
| POST | `/channels/{id}/previous` | `{from_item_id}` → new `now`. Step back via the rule's `previous`; idempotent like `advance` |
| GET | `/items/{id}` | Item metadata |
| GET | `/stream/{item_id}` | Media bytes with `Range` support |
| GET | `/subtitles/{item_id}/{n}` | Sidecar subtitle file |
| GET | `/history?limit=` | Recent events, newest first |
| POST | `/library/rescan` | Rescan all sources |
| GET | `/clients` | Connected clients, with what each is playing |
| GET (WebSocket) | `/clients/connect` | A client's control channel: server pushes relayed `Action`s, client pushes `Event`s |
| POST | `/clients/{id}/actions` | Relay an `Action` to a client. `{id}` may be `default` (the most recently active client) |
| GET | `/events` | Server-Sent Events stream of all client and channel events |
| GET | `/openapi.json` | OpenAPI document |

Clients identify themselves with an `X-Marathon-Client` header (a UUID generated
once and stored locally).

---

## 9. Desktop client (`marathon-desktop`)

### Render loop

- One `winit` window, one `glutin` GL context. mpv created with `vo=libmpv` and
  `hwdec=auto-safe`; render context via the OpenGL render API.
- Each frame: if mpv signalled a new frame, `mpv_render_context_render` into the
  default framebuffer; draw the egui overlay on top; swap; call
  `mpv_render_context_report_swap`.
- mpv's update callback wakes the event loop through an `EventLoopProxy`. Never
  call mpv render functions from the callback itself.
- Use `ControlFlow::Wait` when nothing is animating and no frame is pending, so
  the app does not spin. Request redraws continuously only while an overlay
  animation is running.
- On macOS the event loop and GL context must stay on the main thread.
- **Keep winit at the edge.** The renderer takes a `Surface` abstraction (GL
  function loader, drawable size, scale factor, present) and nothing
  winit-specific. All `winit`/`glutin` code lives in one platform module of
  `marathon-desktop`. A future appliance backend supplies a `Surface` from
  GBM/EGL/KMS and input from libinput, without touching the renderer, Tuner,
  or overlay.

### Tuner state machine (`marathon-client`)

Player-agnostic and unit-testable against a mock `Player`:

```
Idle → Tuning(channel) → Playing(channel, item) ─ended→ Advancing → Playing
                              │
                              └─user switches channel→ (report progress) → Tuning(other)
```

- On tune: `GET now`, `loadfile <url> replace start=<position>`, add subtitles.
- While playing: report progress every 5s, plus immediately on pause, channel
  change, and quit.
- On end of file: `POST advance {from_item_id, reason: "ended"}`, load the
  returned item. Use the prefetched next URL so the gap between episodes is
  minimal.
- Show the channel banner the instant a switch begins, before the video loads,
  so switching feels immediate even when the stream takes a moment to open.

### Input and control

Every way of controlling Marathon is an **adapter** that emits `Action`s, and
every way of observing it subscribes to **`Event`s**. Extensibility in both
directions is a core requirement (see the product principle): users will wire
Marathon into setups nobody can predict.

```
 Local adapters (in the client)        Remote adapters (through the server)
   keyboard, mouse, gamepad, CEC,        HTTP/curl, home automation,
   libinput (appliance, later)           AI agents (MCP), phone remotes
              │                                       │
              │                          POST /clients/{id}/actions
              ▼                                       ▼
        ┌──────────── Action bus (client) ◄── WebSocket relay (server)
        ▼
   Tuner + overlay UI ──► Events ──► server ──► GET /events (SSE) ──► observers
```

**Actions.** `Action` and `Event` live in `marathon-api` because the server
relays them. Both serialize as tagged JSON with snake_case names, e.g.
`{"action": "go_to_channel", "number": 4}`. Initial set:

```
ChannelUp, ChannelDown, GoToChannel(u32), GoToChannelId(String),
TogglePause, Play, Pause, SeekRelative(i32), NextItem, PreviousItem,
Navigate(Up | Down | Left | Right), Select, Back,
ShowInfo, ToggleGuide, ToggleSubtitles, ToggleFullscreen, Standby, Wake, Quit
```

(`ToggleSubtitles` and `ToggleFullscreen` came from M3, whose manual checks
need them. Variants with data are struct variants on the wire, e.g.
`{"action": "seek_relative", "seconds": -10}`.)

Actions are semantic, not device-shaped. The UI resolves `Navigate` by context:
with no overlay open, Up/Down change channel and Left/Right seek; with the
guide open, they move the selection. This makes a TV remote's D-pad, a gamepad
D-pad, and arrow keys behave identically without per-device logic.

**Events (initial set).** `NowPlaying {channel, item, position}`,
`ChannelChanged`, `Paused`, `Resumed`, `ItemEnded`, `Standby`, `Wake`,
`Error {message}`.

**Local adapters** implement an `InputSource` trait: started with a sender
into the action bus, run on their own task or thread, and stopped cleanly.
Any number run at once. Each lives behind a cargo feature (`input-keyboard`,
`input-gamepad`, `input-cec`, ...) so builds include only what a platform needs.

**Remote adapters** go through the server, since it's the network-facing
process. Each client keeps a WebSocket to the server (reconnecting with
backoff); `POST /clients/{id}/actions` relays an `Action` to it, and the client
publishes its `Event`s back up the same socket for `GET /events`.

**No in-process plugin ABI.** Rust has no stable ABI, and dynamically loaded
plugins are a maintenance trap. Third-party extensions are separate programs
that speak the HTTP API. A Home Assistant integration, an MQTT bridge, an MCP
server for AI agents, or a phone remote page is each a small standalone program
over that API. The API's completeness is what makes Marathon extensible, so
anything the UI can do must also be possible through it.

**Conflicts.** Last action wins. No input source can lock out another, and
there is no modal state that an adapter could leave stuck.

**MVP bindings.** Keyboard: arrows navigate (per the context rule above),
PgUp/PgDn change channel, digits for direct entry (committed on Enter or after
1.5s), Enter select, Space pause, N/P next/previous item, I info, G guide,
S subtitles, F fullscreen, Esc back, Q quit. Mouse: wheel changes
channel, click selects, movement shows the cursor. Gamepad: shoulders change
channel, D-pad navigates, A select/info, B back, Start pause; and, so a
controller can do everything the keyboard can (M5), Y guide, X subtitles,
triggers next/previous item, Select info, left-stick click fullscreen, Mode
quit. Only seeking (Left/Right) repeats when held. CEC: see M7.

### Overlay UI (MVP)

- **Channel banner** on tune and on `ShowInfo`: number, channel name, item title
  (e.g. `S03E14 · The Injury`), progress bar. Fade in ~150ms, hold ~4s, fade out
  ~300ms, eased, driven by frame time at the display's refresh rate.
- **Guide**: vertical list of channels with what each is currently on. Selecting
  one tunes to it.
- Hide the mouse cursor after inactivity. Fullscreen by default; `--windowed`
  flag for development.

### Embedded mode

`marathon-desktop --embedded --config path/to/config.toml` starts
`marathon-server` in-process on `127.0.0.1:0` and connects to it.
`marathon-desktop --server http://host:7878` connects to a remote server.

---

## 10. Milestones

Work the milestones in order. A milestone is complete only when **every**
criterion below it is met. Criteria are tagged:

- **[auto]** — verified by a test or command that runs in CI.
- **[manual]** — needs a human to run the app and look. Claude Code should list
  these at the end of the milestone with exact steps to check, and must not
  mark them done itself.

Every milestone also inherits these baseline criteria:

- [auto] `cargo fmt --all --check` passes.
- [auto] `cargo clippy --workspace --all-targets -- -D warnings` passes.
- [auto] `cargo test --workspace` passes.
- [auto] CI is green on every platform it covers at that milestone.
- No `TODO`s left in code without a matching entry in `docs/TODO.md` or an issue.

---

### M0 — Scaffold

Workspace, empty crates, CI (GitHub Actions on Linux and macOS),
`config/example.toml`, and `scripts/make-test-library.sh`, which uses ffmpeg's
`testsrc` to generate a small fake library. Clips are a few seconds long with
the filename burned into the frame.

**Complete when:**
- [auto] All crates and apps from §4 exist and `cargo build --workspace` succeeds
  on Linux and macOS in CI.
- [auto] The internal dependency graph matches §4 (no crate depends on one it
  shouldn't; `marathon-rules` and `marathon-api` have no internal deps).
- [auto] `config/example.toml` parses into the config types (a test loads it).
- [manual] `scripts/make-test-library.sh <dir>` produces: a show with 3 seasons
  of 4 episodes each named `Show Name - SxxExx - Title.mkv` inside
  `Season N` folders; a folder of 6 loosely named "essay" clips; one episode
  with a sidecar `.en.srt`; and one clip whose name contains spaces, unicode,
  and an apostrophe.
- [manual] Each generated clip plays in plain `mpv` and shows its own filename
  on screen.
- [auto] Generated media is ignored by `.gitignore`.

### M1 — Rules engine

`marathon-rules` with `ordered`, `shuffled`, `grouped`, `next`/`previous`,
and serializable cursors.

**Complete when:**
- [auto] `ordered`: walks items in season/episode order, then loops to the
  first item.
- [auto] `shuffled` (property test): over any item set of size 0–200, every item
  appears exactly once per cycle; the last item of a cycle is never the first
  of the next (for sets of 2+).
- [auto] `grouped` (property test): groups never interleave; within-group order
  matches `within`; every group plays once before any group repeats when
  `groups = "shuffled"`.
- [auto] Same seed and same inputs produce the same sequence (determinism test).
- [auto] Cursors round-trip through JSON, and resuming from a deserialized cursor
  continues the identical sequence.
- [auto] Rescan tolerance: removing the current item advances to a valid item
  without panicking; adding items includes them no later than the next cycle;
  an unchanged item set leaves the sequence unchanged.
- [auto] `previous` after `next` returns the prior item for every rule.
- [auto] Edge cases: 0 items returns `None`/empty step without panicking; 1 item
  repeats itself.
- [auto] The crate has no dependencies on I/O, time, or global RNG (enforced by
  its `Cargo.toml` dependency list; review in PR).

### M2 — Server

Config loading, filesystem source and scanner, filename parsing, sidecar
subtitle detection, SQLite schema and migrations, the API in §8, and range
streaming. Binary: `marathond --config ...`.

**Complete when:**
- [auto] Filename parser unit tests cover `S01E02`, `s1e2`, `1x02`, `Season N`
  folders, and fallback to natural sort (`Ep 2` sorts before `Ep 10`).
- [auto] Scanner integration test against a temp library: the right number of
  items is created with correct show/season/episode; sidecar subtitles are
  attached to their video; a deleted file is marked `missing`, not removed; a
  restored file is unmarked.
- [auto] Migrations run cleanly on an empty database and are idempotent on
  restart.
- [auto] Config channels are upserted on startup; editing a channel's rule in
  config and restarting changes the rule but preserves `channel_state`.
- [auto] Every route in §8 has at least one integration test (via
  `tower::ServiceExt::oneshot` or a real bound port).
- [auto] `GET /stream/{id}` returns `200` with full body, `206` with correct
  `Content-Range` for a range request, and `416` for an unsatisfiable range.
- [auto] `advance` idempotency: two calls with the same `from_item_id` advance
  the channel exactly once.
- [auto] `progress` updates `channel_state` and appends an event; `history`
  returns events newest first.
- [auto] With a token configured, requests without it get `401`; with
  `bind = 127.0.0.1` and no token, requests succeed. Startup fails with a clear
  error if bind is non-loopback and no token is set.
- [auto] `GET /openapi.json` returns a valid OpenAPI 3 document covering every
  route.
- [manual] `mpv http://127.0.0.1:7878/api/v1/stream/<id>` plays from the test
  library, and seeking forward and backward works without stalls.
- [manual] Calling `advance` repeatedly with `curl` walks each test channel in
  the order its rule dictates.

### M3 — Desktop client, minimal

`marathon-mpv` wrapper, window + GL + mpv render API, Tuner state machine,
keyboard input. No overlay; channel changes are logged to the console.

**Complete when:**
- [auto] Tuner state machine tests against a mock `Player` cover: tune →
  play at saved position minus rewind; end of file → `advance` → next item;
  channel switch reports progress before tuning away; progress reported on the
  5s interval, on pause, and on quit.
- [auto] A tune-in where the saved position is within the finished threshold
  advances instead of resuming.
- [auto] `Action` mapping test: every keyboard binding in §9 produces the
  expected `Action`.
- [manual] Channel up/down switches channels; each channel resumes within a
  few seconds of where it was left.
- [manual] An episode reaching its end advances to the next item with no user
  input, and the gap between items is under ~1 second on local files.
- [manual] Quit, relaunch, and the last-watched channel resumes at the right
  item and position.
- [manual] An episode with a sidecar `.srt` shows subtitles; toggling
  subtitles works.
- [manual] Pause, seek ±10s, next item, and previous item all behave as
  described in §9.
- [manual] Window resize and fullscreen toggling don't corrupt or stretch the
  video.
- [manual] Hardware decode is active on the dev machine (confirm via mpv's
  `hwdec-current` property in the log).

### M3.5 — Seams (refactor of the existing M3 code)

M0–M3 were built before the input/control architecture and the appliance
direction were added to this plan. This milestone brings the existing code in
line with §2 decisions 6–7, §9 "Input and control", and the product principle,
without adding user-facing features. Do it before M4 so later milestones
build on the right shape.

Changes:
- Move `Action` from `marathon-client` to `marathon-api`, expand it to the full
  set in §9, add `Event`, and make both serialize as tagged snake_case JSON.
- Introduce the `InputSource` trait and the client action bus; port keyboard
  input to an `InputSource` behind the `input-keyboard` feature.
- Implement context resolution for `Navigate` and update keyboard bindings to
  §9 (arrows navigate, PgUp/PgDn change channel, Enter selects).
- Introduce the `Surface` abstraction and confine all `winit`/`glutin` code to
  one platform module in `marathon-desktop`.
- "Keeps going": an item that fails to load or play is skipped with an
  `Error` event, rather than stopping the channel.

**Complete when:**
- [auto] `Action` and `Event` live in `marathon-api`; a round-trip serialization
  test covers every variant and asserts the JSON shape for at least
  `channel_up`, `go_to_channel`, and `navigate`.
- [auto] Variants the client doesn't handle yet (e.g. `Standby`, `Wake`) are
  logged and ignored, never panic.
- [auto] Two mock `InputSource`s running concurrently both deliver actions to
  the bus, and stopping one leaves the other working.
- [auto] `Navigate` resolution is unit tested: with no overlay, Up/Down change
  channel and Left/Right seek; with the guide state open, they move the
  selection.
- [auto] Keyboard mapping test updated to the §9 bindings.
- [auto] A CI check confirms `marathon-client` has no `winit`, `glutin`, or
  `gilrs` dependency (`cargo tree`), and that `winit`/`glutin` are referenced
  only inside `marathon-desktop`'s platform module (a grep script).
- [auto] Tuner test with a mock `Player` that fails to load: the channel skips
  to the next item and emits `Error`. After a configurable number of
  consecutive failures (default 5) it stops retrying and surfaces a message
  instead of looping forever.
- [manual] Every M3 manual criterion still passes (regression check).
- [manual] Renaming or deleting a file in the test library mid-channel causes a
  skip, not a stall.

### M4 — Embedded mode

`marathon-desktop --embedded --config <path>` runs the server in-process.

**Complete when:**
- [auto] An integration test starts the embedded server, resolves its
  OS-assigned port, and makes a successful API call.
- [auto] Embedded server binds only to `127.0.0.1`.
- [manual] Every M3 manual criterion passes with no separate `marathond`
  running.
- [manual] Quitting the app shuts the embedded server down cleanly (no orphan
  process, no lingering port, SQLite not left locked).
- [manual] `--server http://host:7878` still works against a standalone
  `marathond`, and the two modes share no state unless pointed at the same
  `data_dir`.

### M5 — Overlay and gamepad

egui overlay with channel banner and guide, eased fades, cursor hiding,
`gilrs` input.

**Complete when:**
- [auto] `Action` mapping test covers every gamepad binding in §9.
- [auto] Fade timing logic (opacity as a function of elapsed time) is unit
  tested independent of rendering.
- [manual] The channel banner appears the instant a switch begins, before the
  new video loads, and shows number, channel name, item title, and progress.
- [manual] Fades are visibly smooth at the display's full refresh rate
  (check on a 120Hz+ display) with no hitching while video plays underneath.
- [manual] Video playback does not stutter or drop frames while the overlay
  animates (check mpv's dropped-frame count in the log).
- [manual] With video playing and no overlay visible, the app's CPU usage is
  close to plain `mpv` playing the same file (the UI is not redrawing
  needlessly).
- [manual] The guide lists every channel with its current item, and selecting
  one tunes to it.
- [manual] A controller can perform every action the keyboard can; plugging
  and unplugging the controller mid-session works without restart.
- [manual] The mouse cursor hides after inactivity and reappears on movement.

### M6 — Remote control and events

Client WebSocket control channel, action relay, and event stream, as in §8 and
§9 "Input and control". This is the extension surface everything else
(home automation, AI agents, phone remotes) will build on.

**Complete when:**
- [auto] A client registers over `/clients/connect` and reconnects with backoff
  after a server restart, without user intervention.
- [auto] `POST /clients/{id}/actions` delivers the `Action` to that client's
  action bus (integration test with a headless client and a mock `Player`).
- [auto] `default` targets the most recently active client; an unknown client
  id returns `404`; a malformed action returns `400` with a useful message.
- [auto] `GET /events` delivers `ChannelChanged` and `NowPlaying` within one
  second of the change.
- [auto] Action and event routes require the bearer token when one is
  configured.
- [auto] Every `Action` and `Event` variant appears in `openapi.json` with its
  JSON shape, and a round-trip serialization test covers each variant.
- [auto] Every action the keyboard can trigger is also reachable via the API
  (a test enumerates `Action` variants against the relay).
- [manual] `curl -X POST .../clients/default/actions -d '{"action":"channel_up"}'`
  changes channel on a running desktop client.
- [manual] Interleaving keyboard and HTTP actions rapidly never leaves the UI
  or Tuner in a stuck state.
- [manual] `examples/` contains a working shell script that sends actions and
  one that tails `/events`.

### M7 — HDMI-CEC input

`marathon-input-cec` via libcec, behind the `input-cec` feature. Develop on a
Raspberry Pi (native CEC) or a desktop with a Pulse-Eight USB adapter.

Behavior follows the product principle: Marathon is a polite guest on input 4.

- Registers as a playback device with OSD name "Marathon".
- **Never requests active source on its own** at startup or wake
  (`cec.claim_on_start = false`), so it never pulls the TV away from input 1.
- When the TV switches to Marathon's input, resume playback
  (`cec.resume_on_select = true`). When the TV switches away or enters
  standby, pause, report progress, and emit `Standby`.
- Never sends standby or power-on to the TV unless configured to.
- Remote keys map to `Action`s: D-pad → `Navigate`, OK → `Select`,
  Back/Return → `Back`, Channel up/down, digits, play/pause/stop, and
  Info/Guide where the TV forwards them. Colour buttons are user-mappable.

**Complete when:**
- [auto] CEC key code → `Action` mapping is unit tested without hardware.
- [auto] Lifecycle logic (switched to us, switched away, TV standby) is tested
  against a mock CEC adapter.
- [auto] The crate builds and its tests run in CI without libcec hardware;
  `marathon-desktop` builds with and without the `input-cec` feature.
- [manual] Marathon appears as "Marathon" in the TV's input or device list.
- [manual] Turning the TV on with Marathon running does **not** switch the TV
  away from whatever input it was on.
- [manual] Switching the TV to Marathon's input resumes playback, and switching
  away pauses it, on any TV that reports the routing change.
- [manual] TV standby pauses playback and the channel resumes at the right
  position afterward.
- [manual] The TV remote's D-pad, OK, Back, channel, and play/pause buttons all
  control Marathon, including navigating the guide.
- [manual] Unplugging the HDMI cable or CEC adapter does not crash the client,
  and CEC recovers when it's reconnected.

### M8 — Jellyfin source

Catalog from the Jellyfin API (shows, seasons, episodes, durations); the
server proxies streams with `Range` pass-through.

**Complete when:**
- [auto] Jellyfin client tests run against recorded or mocked API responses
  (no live server required in CI).
- [auto] Proxied streams return correct `206`/`Content-Range` responses for
  range requests, verified against a mock upstream.
- [auto] Durations from Jellyfin populate `items.duration_ms` without waiting
  for a client report.
- [auto] Jellyfin credentials come from config and never appear in logs or API
  responses.
- [manual] A channel built from a real Jellyfin library plays, seeks, resumes,
  and advances exactly like a filesystem channel.
- [manual] Jellyfin being unreachable produces a clear error in the client
  rather than a hang, and filesystem channels keep working.
- [manual] Rescanning picks up episodes added in Jellyfin.

### M9 — macOS

Desktop client builds and runs on macOS against Homebrew libmpv.

**Complete when:**
- [auto] CI builds and tests the workspace on macOS (Apple Silicon runner).
- [auto] A script produces a `.app` bundle with libmpv and its dependencies
  bundled and their paths rewritten, so it runs on a Mac without Homebrew.
- [manual] Every M3, M4, M5, and M6 manual criterion passes on macOS.
- [manual] HiDPI: video and overlay render at native resolution on a Retina
  display, not upscaled or quarter-sized.
- [manual] Fullscreen uses native macOS fullscreen and behaves correctly
  across Spaces and a second display.
- [manual] Hardware decode is active (VideoToolbox, confirmed via
  `hwdec-current`).
- [manual] The bundle launches from Finder (after the usual unsigned-app
  approval) with no terminal.

### M10 — Android client

Kotlin + Jetpack Compose (Compose for TV for the 10-foot UI), libmpv from the
mpv-android build, API client generated from `openapi.json`. Gets its own
planning doc before implementation starts.

**Complete when:**
- [auto] The Kotlin API client is generated from the server's `openapi.json` by
  a script, and regenerating it after a server change is one command.
- [auto] The ported Tuner state machine passes the same scenarios as the Rust
  version (tune, resume rewind, advance on end, report-before-switch, finished
  threshold).
- [auto] A Gradle build produces an installable APK in CI.
- [manual] On an Android TV device, a remote's D-pad and channel buttons drive
  every action in §9.
- [manual] On an Android phone, touch controls provide the same actions.
- [manual] Channel switching, resume, and auto-advance pass the M3 manual
  criteria against a networked `marathond`.
- [manual] The Android client registers over `/clients/connect` and responds
  to relayed actions exactly as the desktop client does (M6 criteria).
- [manual] A file Android's native player can't decode (e.g. 10-bit HEVC with
  PGS subtitles) plays correctly with subtitles.
- [manual] Watching a channel on desktop, then tuning to it on Android, resumes
  at the same item and position (and vice versa).
- [manual] Backgrounding the app pauses playback and reports progress; returning
  resumes.

### After the MVP (not now)

Example adapters over the M6 API, each a separate small program: a phone
remote web page, an MCP server for AI agents, a Home Assistant integration,
an MQTT bridge. Also: native SMB/NFS sources (for servers that cannot mount
shares, e.g. the appliance), embedded server on Android via `cargo-ndk`, a
libinput `InputSource` and DRM/KMS `Surface` for the appliance (§13), optional
loudness normalization across items (background TV should not jump in volume
between episodes), a sleep timer, mDNS discovery, stats views, search, a
channel editor UI, multiple profiles, content fingerprinting to survive file
moves, server-side transcoding.

---

## 11. Non-goals for the MVP

No transcoding. No multi-user or accounts (a single bearer token at most). No
native SMB/NFS. No YouTube integration. No EPG with times. No phone remote UI
(the action API exists; front-ends for it come later). No in-process plugin
system. No Windows build (keep code portable, but do not test it yet).

---

## 12. Open decisions (defaults chosen)

| Decision | Default | Revisit when |
|---|---|---|
| Overlay UI toolkit | egui | If the 10-foot look needs more than egui gives easily; the overlay is isolated so it can be swapped for a custom GL renderer or another toolkit |
| libmpv bindings | Decided in M3: hand-written declarations in `marathon-mpv` (no `bindgen`, so no libclang). `libmpv2` was LGPL-2.1, panicked inside FFI callbacks, and allocated per frame; the needed surface is ~20 functions | If the needed surface grows a lot |
| Progress report interval | 5s | If DB growth matters |
| Finished threshold | `max(30s, 2%)` from end | After living with it; credits vary |
| Resume rewind | 5s | Personal taste |
| CEC claims active source on start | Off | Never by default; it's input 4 |
| CEC resumes when TV switches to Marathon | On | If some TVs report routing changes unreliably |
| Project license | MIT OR Apache-2.0 | Before publishing |

---

## 13. Appliance direction (not in scope; shapes today's design)

The long-term goal is a small box that plugs into a TV and is controlled
entirely by the TV remote over HDMI-CEC. Planned path, modelled on LibreELEC:

1. **Kiosk recipe.** Stock minimal Debian, a systemd unit running the `cage`
   kiosk compositor with `marathon-desktop --embedded` inside it. No custom
   distro; documentation plus a setup script.
2. **Image builder.** A Buildroot-based build producing per-board images
   (generic x86_64 and Raspberry Pi first): systemd, read-only squashfs root,
   separate data partition, A/B updates (e.g. RAUC), boot splash from
   bootloader to first frame, no visible Linux.
3. **Bare-metal display.** A DRM/KMS `Surface` (GBM + EGL) and libinput
   `InputSource` replace cage, enabling refresh-rate matching, HDR output, and
   faster boot. Audio goes straight to HDMI via ALSA for bitstream passthrough.

What this requires of the MVP is already in the plan: pluggable input
(§9), winit kept at the edge (§9 render loop), CEC as an ordinary
`InputSource` (M7), and the "polite input 4" CEC defaults.
