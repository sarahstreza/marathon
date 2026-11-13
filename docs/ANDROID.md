# Android client (M10) — design

PLAN.md §10 M10 asks for a Kotlin + Jetpack Compose Android client, using
Compose for TV for the 10-foot UI and libmpv from the mpv-android build, with an
API client generated from the server's `openapi.json`. It also asks for a planning
doc before implementation starts. This is that doc. PLAN.md stays the source
of truth for semantics (§1), the API (§8), and the client behaviour (§9). This
doc only covers how the Android client meets them.

The Android client is a **thin client** like the desktop one (PLAN.md §2
decision 3). It plays what the server says, reports what happened, draws the
overlay, and maps input to `Action`s. It does not embed the server. An embedded
server on Android via `cargo-ndk` is listed after the MVP.

---

## Toolchain

| What | Version | Notes |
|---|---|---|
| JDK | 25 (LTS) | Fedora 44 no longer packages 21. Gradle 9.1+ and AGP 9 run on 25. Bytecode targets 17 |
| Gradle | wrapper, pinned with `distributionSha256Sum` | `android/gradlew` |
| Android Gradle Plugin | 9.5 | AGP 9 has Kotlin support built in |
| Kotlin | 2.4 | Compose compiler plugin at the same version |
| compileSdk / targetSdk | 37 | |
| minSdk | 26 | The libmpv AAR's own minimum |
| libmpv | `dev.jdtech.mpv:libmpv` 1.0.0 (Maven Central, April 2026) | mpv-android's native libraries for all four ABIs plus a small `MPVLib` JNI wrapper. MIT wrapper; the bundled mpv/FFmpeg are GPL (see Licensing) |
| Compose | BOM 2026.09, Material 3 | One UI for phones and TVs; see "Why not Compose for TV" below |

Every version lives in `android/gradle/libs.versions.toml`. Check the current
release before bumping one, as CLAUDE.md asks for Rust crates.

### Setting up a Linux machine

The dev machine is Bazzite, an immutable Fedora variant, so the toolchain lives in
the `dev` distrobox. The SDK lives in the shared home directory, so the host can
use `adb` too.

```sh
# inside the distrobox
sudo dnf install java-25-openjdk-devel
# Download commandlinetools-linux-*_latest.zip from
# https://developer.android.com/studio#command-line-tools-only, check its
# SHA-256 against that page, and unpack it so that this path exists:
#   ~/Android/Sdk/cmdline-tools/latest/bin/sdkmanager
export ANDROID_HOME=$HOME/Android/Sdk   # e.g. in ~/.bashrc.d/android.sh
PATH=$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools:$ANDROID_HOME/emulator:$PATH
sdkmanager --licenses                    # read and accept them yourself
sdkmanager "platform-tools" "platforms;android-37.0" "build-tools;37.0.0"
```

`distrobox enter dev -- adb …` does not start a login shell, so `PATH` is not
set. Use `distrobox enter dev` and then run commands, or give the full path.

For emulators, add `"emulator"`, `"system-images;android-37.0;google_apis;x86_64"`,
and `"system-images;android-36;android-tv;x86_64"`, and create AVDs with
`avdmanager create avd`. **Known issue:** on the Bazzite dev machine (kernel 7.2,
emulator 37.2.13), the emulator's headless binary segfaults during boot, both
inside the distrobox and on the host. The windowed emulator hasn't been tested
from a desktop session yet. Real devices are the main target anyway.

To use a phone, turn on Developer options → Wireless debugging, then run
`adb pair <ip>:<pairing-port>` with the code it shows, and
`adb connect <ip>:<port>`. If pairing fails, connect once over USB on the host,
run `adb tcpip 5555`, then use `adb connect <ip>:5555`.

macOS: `brew install --cask temurin@25 android-commandlinetools`.

---

## Layout

```
android/                       Gradle project (Kotlin DSL, version catalog)
  settings.gradle.kts
  gradle/libs.versions.toml
  api/openapi.json             committed snapshot of the server's spec
  api/                         :api   JVM library, Kotlin client generated at build time
  core/                        :core  pure Kotlin/JVM: the marathon-client port
  app/                         :app   Android: mpv, Compose UI, input, lifecycle
scripts/gen-android-api.sh     refresh the snapshot and regenerate :api (one command)
scripts/check-tuner-parity.sh  every Rust Tuner scenario has a Kotlin twin
```

`:core` has no Android dependency, which mirrors `marathon-client`'s "no platform
code" rule. Its tests run on a plain JVM in seconds, with no device or
emulator. `:app` depends on both `:core` and `:api`.

---

## API client (`:api`)

- `marathond --print-openapi` prints the OpenAPI document and exits. It needs no
  config and starts no server.
- `scripts/gen-android-api.sh` writes that output to `android/api/openapi.json`
  and runs `./gradlew :api:openApiGenerate`. **Regenerating after a server change
  is that one command** (M10 criterion).
- The Gradle `org.openapi.generator` plugin generates into `build/`, so generated
  code is never committed. Only the snapshot is.
- CI fails if the snapshot is stale, i.e. if it differs from what `marathond
  --print-openapi` prints.
- `Action` and `Event` are internally tagged enums, so in OpenAPI they are
  `oneOf` + discriminator. OpenAPI generators handle that unevenly. `:core` defines
  them by hand as `@Serializable sealed interface`s with a class discriminator.
  To guarantee they match the Rust shapes, `crates/marathon-api/tests/control_fixtures.rs`
  writes every `Action::examples()` and `Event::examples()` value to
  `crates/marathon-api/tests/fixtures/control.json`, and fails if that file is
  stale. A `:core` test decodes and re-encodes every fixture and compares the
  JSON exactly.

---

## Tuner port (`:core`)

A direct port of `crates/marathon-client`, keeping the same names so the two
can be read side by side:

| Rust (`marathon-client`) | Kotlin (`:core`, package `marathon.core`) |
|---|---|
| `tuner.rs`: `Tuner`, `TunerConfig`, `Clock`, `Flow`, `State` | same |
| `player.rs`: `Player`, `LoadRequest`, `SubtitleTrack`, `PlayerEvent` | same |
| `api.rs`: `Api` | `Api` interface; `HttpApi` over OkHttp |
| `navigation.rs`, `digits.rs`, `ui.rs` (`UiEvent`, `Banner`, `Guide`) | same |
| `bus.rs`: action bus, `InputSource` | same, over a blocking queue |
| `remote.rs`: `Backoff`, `RemoteSource`, replay on reconnect | same, over OkHttp's WebSocket |
| `marathon-api` request/response types used by the client | `marathon.core.model`, `@Serializable` |

**Threading.** The Tuner stays synchronous and has a single owner, exactly as
in Rust. One dedicated thread (`TunerLoop`) owns it. It takes the next input from
one queue (actions from every `InputSource`, and `PlayerEvent`s) and wakes for
`Tuner.nextDeadline()`. `Api` calls block that thread, as the desktop's blocking
reqwest calls block its tuner thread. That keeps the port line-for-line and the
tests free of coroutines.

**Same scenarios.** `crates/marathon-client/src/tuner/tests.rs` is ported to
`core/src/test/kotlin/marathon/core/TunerTest.kt` with the same mock
`Player`/`Api`/clock design and **the same snake_case test names**.
`scripts/check-tuner-parity.sh` fails if any Rust test is missing from the
Kotlin file, unless it appears in the script's allowlist with a reason. This
makes "passes the same scenarios as the Rust version" checkable by machine.

---

## Playback (`:app`, `MpvPlayer`)

`MpvPlayer` implements `Player` using `dev.jdtech.mpv.MPVLib`:

- Options: `vo=gpu`, `gpu-context=android`, `hwdec=mediacodec-copy`,
  `keep-open=yes`. With `keep-open`, mpv pauses on the last frame instead of
  unloading. `http-header-fields` carries `Authorization: Bearer <token>` when
  a token is set.
- Video draws into a `SurfaceView` hosted by Compose (`AndroidView`). It is
  attached with `attachSurface`, and detached when the surface is destroyed.
- `MPVLib`'s event callback passes only the event ID, not `end-file`'s
  reason or playlist entry. So the latest load's fate is read from
  properties instead:
  - `FILE_LOADED` while mpv's `path` is the URL last loaded → `Started`.
  - `eof-reached` becoming true (with `keep-open`) → `Ended`.
  - `idle-active` becoming true while the latest load is pending or
    playing → `Failed`. mpv only goes idle when a file couldn't be opened,
    broke mid-play, or was stopped. Replacing a file never goes idle.
  - Events for replaced files are ignored. That honours the `Player`
    contract that `Ended`/`Failed` only ever refer to the latest file
    loaded, even when channel flipping replaces a file before mpv starts it.
- Position, duration, and pause state are cached from observed properties, so
  `positionMs()` never blocks on mpv.
- Sidecar subtitles are added with `sub-add <url> auto <title> <lang>` after
  `FILE_LOADED`. Bitmap subtitles (PGS) render inside mpv, which is the reason
  to use mpv instead of ExoPlayer.
- mpv callbacks only post to the tuner's queue. They never call into the Tuner.

---

## UI (`:app`)

One `ComponentActivity`, fullscreen and immersive, with the screen kept on
while it is in front.
The Watch screen is landscape (`sensorLandscape`, so either way up); Setup
follows the device, since typing a URL is easier in portrait. Android 16+
ignores orientation requests on large screens, which is fine there.

- **Setup screen:** server URL and optional token, stored in private
  `SharedPreferences`, with a Test button that calls `/health`. It shows on first launch, or when the
  server can't be reached at start. Discovery (mDNS) is listed after the MVP.
- **Watch screen:** the video, with the overlay on top:
  - **Channel banner:** number, name, `S03E14 · Title`, and a progress bar. It
    fades in over 150 ms, holds for 4 s, and fades out over 300 ms, eased. The
    timing is a pure function (`Fade`), ported from the desktop and unit tested
    (M5).
  - **Guide:** a vertical list of channels with what each is on, with the
    Tuner's highlight. It is driven entirely by `UiEvent`s, as on desktop, so
    the remote, keys, touch, and the HTTP API all move the same highlight.
  - Text is sized to be readable from across a room on a TV and from arm's
    length on a phone. Like the desktop, the overlay lays out in "points"
    that are 1/540 of the screen's height.
  - Error messages (`Event::Error`, unreachable server) show as a toast for
    6 s, as on desktop.

**Why not Compose for TV.** PLAN.md names Compose for TV (`androidx.tv`)
for the 10-foot UI. Its value is D-pad focus handling for interactive
TV screens. Marathon's overlay is display-only, like the desktop's egui
overlay: every key becomes an `Action` at the activity level, and the
Tuner moves the guide's highlight. So there's no focus to manage, and
plain Compose draws the same overlay on phones and TVs. The only focusable
screen is Setup, where standard Compose text fields work with a D-pad.

### Keys → `Action` (`KeyMapping`, unit tested)

| Key | Action |
|---|---|
| D-pad | `Navigate` (the Tuner resolves it in context, as on desktop) |
| D-pad centre, Enter | `Select` |
| Back, Escape | `Back` |
| Channel up/down, Page up/down | `ChannelUp` / `ChannelDown` |
| 0–9 | digit entry (shared `Digits`, commits on Enter/OK or after 1.5 s) |
| Play/pause, Space | `TogglePause` |
| Play / Pause / Stop | `Play` / `Pause` / `Pause` |
| Rewind / Fast-forward | `SeekRelative(∓10)` (repeats when held) |
| Next / Previous track, N / P | `NextItem` / `PreviousItem` |
| Info, I, Menu | `ShowInfo` |
| Guide, G | `ToggleGuide` |
| Captions, S | `ToggleSubtitles` |
| Game controller | As on desktop: A select, B back, shoulders channel, triggers next/previous item, Start pause, Y guide, X subtitles, Select info |

Only seeking repeats when held, as on desktop. Colour buttons are left
unmapped, and keys Marathon doesn't use (volume, Home) go to the system.

### Touch → `Action` (phone)

| Gesture | Action |
|---|---|
| Swipe up / down | `ChannelUp` / `ChannelDown`, unless it starts in the system's gesture strip at the top or bottom edge (going home, recent apps, notifications) |
| Double-tap right / left third | `SeekRelative(+10)` / `SeekRelative(-10)` |
| Double-tap the middle | `TogglePause` |
| Single tap | `ShowInfo`, and reveals a control strip for 4 s |
| Control strip | Guide, previous item, pause/play, next item, subtitles, server settings |
| System back | Clears a half-typed channel number, then `Back` (closes the guide or banner), then leaves the app |

Back reaches apps through the back dispatcher, not `onKeyDown` (the
default from Android 16), so `MainActivity` handles it with an
`OnBackPressedCallback`.

Together with the remote API, this covers every §9 action that means anything on
Android. `ToggleFullscreen` is ignored (the app is always fullscreen).
`Quit` saves progress and finishes the activity.

### Manifest

- Launcher intents for both `LAUNCHER` and `LEANBACK_LAUNCHER`, with a TV banner.
- `android.software.leanback` and `android.hardware.touchscreen` are both
  `required="false"`, so one APK installs on phones and TVs.
- `INTERNET`, plus a network security config that allows cleartext HTTP.
  marathond on a LAN or a Tailscale address is plain HTTP, and PLAN.md leaves
  TLS to the user's VPN.

---

## Control channel and lifecycle

- **`RemoteSource`** connects to `/clients/connect` over a WebSocket with
  `X-Marathon-Client`. The client ID is a UUID generated on first launch and
  stored, like `client_id.rs`. It reconnects with 0.5 s backoff doubling to 30 s,
  and on every (re)connect it replays `NowPlaying` (then `Paused` or `Standby`).
  Events from the Tuner's event sink go up the socket. This is the same protocol
  as desktop, so `POST /clients/{id}/actions` and `GET /events` work unchanged.
- **Backgrounding is standby.** `onStop` sends `Action.Standby`, which pauses,
  reports progress, and hides the overlay. `onStart` sends `Action.Wake`, which
  resumes only if standby was what paused it. This reuses Tuner behaviour that
  is already tested, instead of adding new states.
- **Picture-in-picture.** Leaving the Watch screen for Home or recent apps
  shrinks the video into a floating window instead (`Pip`). Android 12+
  does this by itself (`setAutoEnterEnabled`); before that the activity
  asks from `onUserLeaveHint`. The activity is only paused in the window,
  never stopped, so playback carries on and nothing is sent to the Tuner.
  Closing the window stops the activity, which is standby as above;
  expanding it brings back the full Watch screen. The window follows the
  video's aspect ratio (mpv's `video-params/aspect`, clamped to Android's
  2.39:1 limit) and shows only the video, with three buttons: channel
  down, play/pause, and channel up. They reach the Tuner as Actions from
  the input `pip`, through a broadcast receiver that only Marathon can
  send to. Setup never goes into picture-in-picture.
- **Audio focus.** If another app takes audio focus, Marathon pauses, staying
  polite. It doesn't resume on its own when focus comes back.

---

## CI

A job named `android` in `.github/workflows/ci.yml` runs on ubuntu-latest:

1. `scripts/check-tuner-parity.sh`.
2. Temurin 25 and `gradle/actions/setup-gradle`.
3. `./gradlew check :app:assembleDebug`, which covers `:core` tests, `:api`
   generation, and app unit tests and lint.
4. Uploads `app-debug.apk` as a build artifact.

The Rust `check` job also checks that `android/api/openapi.json` matches
`marathond --print-openapi`, because it already has the Rust toolchain and libmpv.

---

## Licensing

The libmpv AAR bundles mpv and FFmpeg built with GPL components. PLAN.md §3
already accepts GPL libmpv on Android, so an APK built from this repo is
distributed under the GPL. Marathon's own code stays MIT OR Apache-2.0.

---

## Manual checks (M10)

These need a person and a device. They are restated here with steps:

1. Run `marathond` on the LAN with a token (`bind = "0.0.0.0:7878"`) against
   `scripts/make-test-library.sh` output. Install with
   `adb install android/app/build/outputs/apk/debug/app-debug.apk`. Enter the
   server URL and token.
2. **Phone, touch:** swipe up and down to change channel, double-tap to seek,
   tap for info and the controls, use the guide. Check that every action in §9 that
   applies on a phone is reachable.
   With the phone held upright, the Watch screen turns to landscape (either
   way up), and Setup (the controls' Server button) follows the phone again.
   With gesture navigation, swiping up from the bottom edge goes home (or
   to picture-in-picture) and swiping down from the top edge shows the
   status bar, without changing channel; the same swipes from mid-screen
   still change channel. Repeat with three-button navigation.
3. **Phone, M3 criteria:** each channel resumes within a few seconds of where it
   was left, and an episode ending advances on its own with a gap under about 1 s.
   Force-stopping and relaunching resumes the last channel. The sidecar `.srt`
   shows, and subtitles toggle.
4. **Remote control:** `examples/send-action.sh channel_up` changes channel on
   the phone, and `examples/tail-events.sh` shows its events.
5. **Hand-off:** watch a channel on desktop, quit, and tune to it on the phone.
   It should be on the same item and position. Then check the other direction.
6. **Background:** with picture-in-picture turned off for Marathon (system
   settings → Apps → Marathon → Picture-in-picture), pressing Home pauses
   playback and reports progress. Returning resumes playback.
7. **Picture-in-picture:** on the Watch screen, go Home (button, or swipe up
   on gesture navigation). The video shrinks into a window and keeps
   playing, with no banner or guide. The window's shape matches the video
   (try a 4:3 item). Its buttons change channel and pause/resume, and the
   play/pause icon follows the state (including after pausing from
   `examples/send-action.sh pause`). Expanding the window returns to
   fullscreen, still playing. Dismissing the window pauses and reports
   progress (`examples/tail-events.sh` shows `standby`). From the Setup
   screen, Home does not open a window. On Android 8–11, repeat with the
   Home button.
8. **Codec:** a 10-bit HEVC file with PGS subtitles plays with subtitles. The
   test library doesn't generate one, so supply it.
9. **Android TV:** a remote's D-pad, OK, Back, and channel buttons drive every
   §9 action, including the guide. This needs TV hardware, or a working TV
   emulator (see the known issue above).
