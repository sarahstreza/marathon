# Marathon — Project Plan

Marathon recreates the lean-back experience of television from a personal video
collection. The user defines **channels**, each a list of **items** (episodes,
films, essays) plus a **programming rule** that decides what plays next. Flip to a
channel and it resumes exactly where you left off; let it run and it keeps going.

This document is the build plan for the MVP: a Rust server and a Rust desktop
client for Linux and macOS, with an Android client to follow.

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
| Video | libmpv, OpenGL render API | `libmpv2` crate if it supports the render API cleanly at current versions; otherwise thin `bindgen` bindings in `marathon-mpv` (the needed surface is small) |
| Overlay UI | `egui` via `egui_glow` | Drawn over the video each frame; isolated behind a module so it can be replaced |
| Gamepad | `gilrs` | |
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
    marathon-api/            # request/response types, shared enums, OpenAPI derive
    marathon-rules/          # pure programming-rules engine; no I/O
    marathon-server/         # library: config, catalog, sources, db, routes, streaming
    marathon-client/         # library: API client, Tuner state machine, Player trait, Action enum
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
| POST | `/channels/{id}/previous` | Step back via the rule's `previous` |
| GET | `/items/{id}` | Item metadata |
| GET | `/stream/{item_id}` | Media bytes with `Range` support |
| GET | `/subtitles/{item_id}/{n}` | Sidecar subtitle file |
| GET | `/history?limit=` | Recent events, newest first |
| POST | `/library/rescan` | Rescan all sources |
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

### Input

All input maps to one `Action` enum, so every control method drives the same
behavior:

```
ChannelUp, ChannelDown, GoToChannel(u32), TogglePause, SeekRelative(i32),
NextItem, PreviousItem, ShowInfo, ToggleGuide, Back, Quit
```

MVP bindings — keyboard: PgUp/PgDn or Up/Down for channels, digits for direct
entry, Space pause, Left/Right seek ±10s, N/P next/previous item, I info,
G guide, Esc back, Q quit. Gamepad: shoulders change channel, A info/select,
B back, Start pause, D-pad navigates the guide.

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

Each milestone ends with `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
and `cargo test --workspace` passing.

**M0 — Scaffold.** Workspace, empty crates, CI (GitHub Actions on Linux and
macOS), `config/example.toml`, and `scripts/make-test-library.sh`, which uses
ffmpeg's `testsrc` to generate a small fake library: a 3-season show with
`SxxExx` names, a folder of loose "essays", one file with a sidecar `.srt`.
Clips should be a few seconds long with the filename burned into the frame.

**M1 — Rules engine.** `marathon-rules` with `ordered`, `shuffled`, `grouped`,
`next`/`previous`, serializable cursors, and the property tests in §6.
*Done when:* all property tests pass; no I/O in the crate.

**M2 — Server.** Config loading, filesystem source and scanner, filename parsing,
sidecar subtitle detection, SQLite schema and migrations, the API in §8, and
range streaming. `marathond --config ...`.
*Done when:* integration tests run against a temp library; and manually,
`mpv http://127.0.0.1:7878/api/v1/stream/<id>` plays and seeks, and repeated
`advance` calls walk a channel in the order its rule dictates.

**M3 — Desktop client, minimal.** `marathon-mpv` wrapper, window + GL + mpv
render API, Tuner state machine, keyboard input. No overlay yet; log channel
changes to the console.
*Done when:* channel up/down switches channels, each channel resumes where it
was left, episodes advance automatically at end of file, and quitting and
relaunching resumes correctly.

**M4 — Embedded mode.** `--embedded` flag as in §9.
*Done when:* M3's acceptance holds with no separately running server.

**M5 — Overlay and gamepad.** egui overlay with banner and guide, eased fades,
cursor hiding, `gilrs` input.
*Done when:* fades are visibly smooth on a high-refresh display, idle CPU stays
low while video plays with no overlay showing, and a controller can do
everything the keyboard can.

**M6 — Jellyfin source.** Catalog from the Jellyfin API (shows, seasons,
episodes, durations); server proxies streams with `Range` pass-through.
*Done when:* a channel built from a Jellyfin library behaves identically to a
filesystem one.

**M7 — macOS.** Build and run the desktop client on macOS with Homebrew libmpv;
fix main-thread and HiDPI issues; produce an app bundle.

**M8 — Android client.** Separate planning doc. Kotlin + Jetpack Compose
(Compose for TV for the 10-foot UI), libmpv from the mpv-android build, API
client generated from `openapi.json`. Ports the Tuner state machine (small by
design).

### After the MVP (not now)

Native SMB/NFS sources (for servers that cannot mount shares, e.g. the
appliance), embedded server on Android via `cargo-ndk`, phone remote
(server relays commands to clients over WebSocket), HDMI-CEC via libcec,
mDNS discovery, stats views, search, a channel editor UI, multiple profiles,
content fingerprinting to survive file moves, server-side transcoding.

---

## 11. Non-goals for the MVP

No transcoding. No multi-user or accounts (a single bearer token at most). No
native SMB/NFS. No YouTube integration. No EPG with times. No remote control
from phones. No Windows build (keep code portable, but do not test it yet).

---

## 12. Open decisions (defaults chosen)

| Decision | Default | Revisit when |
|---|---|---|
| Overlay UI toolkit | egui | If the 10-foot look needs more than egui gives easily; the overlay is isolated so it can be swapped for a custom GL renderer or another toolkit |
| libmpv bindings | `libmpv2` crate if suitable, else own `bindgen` | M3 start |
| Progress report interval | 5s | If DB growth matters |
| Finished threshold | `max(30s, 2%)` from end | After living with it; credits vary |
| Resume rewind | 5s | Personal taste |
| Project license | MIT OR Apache-2.0 | Before publishing |
