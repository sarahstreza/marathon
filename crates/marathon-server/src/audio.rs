//! Audio-only streams (PLAN.md M12): the server runs ffmpeg to pull an
//! item's audio out of its video and transcode it to stereo Opus, for
//! clients that only want to listen (the Android client's background
//! radio).
//!
//! Audio is optional. [`Audio::detect`] runs when the server starts and
//! checks that the configured ffmpeg works; if it doesn't, or `[audio]`
//! turns it off, the server runs without audio streams and says so in its
//! log.
//!
//! [`Audio::stream`] serves `GET /audio/{item_id}?start_ms=`: ffmpeg
//! seeks to `start_ms`, keeps the first audio track, downmixes it to
//! stereo, encodes Opus at `bitrate_kbps`, and writes Ogg to a pipe that
//! becomes the response body. ffmpeg works as fast as the client reads:
//! when the client stops reading (paused), TCP backpressure stalls the
//! pipe and ffmpeg waits. The process is killed when the response is
//! dropped, which is when the client disconnects. A pipe cannot answer
//! `Range` requests, so clients seek by asking for a new `start_ms`.
//!
//! A piped audio stream has no duration a player can read, yet clients
//! need one (for the finished threshold, PLAN.md §1, and progress
//! reports). So while audio is on, the server probes a filesystem item's
//! duration with ffprobe when it first hands the item out in a `now`
//! without one ([`App::with_probed_durations`]), and stores it. Jellyfin
//! reports durations itself.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use futures_util::{StreamExt, stream};
use marathon_api::Now;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::app::App;
use crate::config::AudioConfig;
use crate::error::ApiError;
use crate::http::safe_join;
use crate::sources::Source;

/// How long `ffmpeg -version` may take at startup before ffmpeg counts as
/// unusable.
const DETECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long probing one item's duration may take. ffprobe reads only the
/// container's headers, so this is generous even for a network share.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long ffmpeg may take to produce the first bytes of a stream (it
/// may have to seek through a Jellyfin stream) before the request fails.
const FIRST_BYTES_TIMEOUT: Duration = Duration::from_secs(30);

/// Bytes read from ffmpeg per body chunk: about 2.7 s of 96 kbit/s Opus.
const CHUNK_BYTES: usize = 32 * 1024;

/// ffmpeg's error output lines logged per stream; the rest are counted.
const MAX_LOGGED_LINES: usize = 20;

/// The server's audio streaming, when it is available.
#[derive(Debug)]
pub(crate) struct Audio {
    /// The ffmpeg program, known to run.
    pub(crate) ffmpeg: PathBuf,
    /// The ffprobe program, if it runs. Without it, durations are not
    /// probed and clients learn them only as they play.
    pub(crate) ffprobe: Option<PathBuf>,
    pub(crate) bitrate_kbps: u32,
    /// One permit per ffmpeg process allowed to run at once.
    pub(crate) streams: Arc<Semaphore>,
    /// Items ffprobe could not find a duration for, so they are not probed
    /// on every tune-in. Forgotten when the server restarts.
    unprobeable: Mutex<HashSet<String>>,
}

impl Audio {
    /// Checks that the configured ffmpeg (and ffprobe) run, and returns the
    /// audio settings if they do. `config` is the `[audio]` table, `None`
    /// when the config has none.
    ///
    /// Never fails: without a working ffmpeg the server just has no audio
    /// streams. If `[audio]` was configured, that is logged as a warning;
    /// without it, ffmpeg was only looked for, so it is logged as info.
    pub(crate) async fn detect(config: Option<&AudioConfig>) -> Option<Self> {
        let explicit = config.is_some();
        let config = config.cloned().unwrap_or_default();
        if !config.enabled {
            tracing::info!("audio streams are turned off (audio.enabled = false)");
            return None;
        }
        if let Err(reason) = runs_with_opus(&config.ffmpeg).await {
            let ffmpeg = config.ffmpeg.display();
            if explicit {
                tracing::warn!(%ffmpeg, %reason, "ffmpeg is not usable; audio streams are off");
            } else {
                tracing::info!(%ffmpeg, %reason, "ffmpeg not found; audio streams are off");
            }
            return None;
        }
        let ffprobe = match runs(&config.ffprobe).await {
            Ok(()) => Some(config.ffprobe.clone()),
            Err(reason) => {
                tracing::warn!(ffprobe = %config.ffprobe.display(), %reason,
                    "ffprobe is not usable; audio streams will work, but the server \
                     cannot work out durations for them");
                None
            }
        };
        tracing::info!(ffmpeg = %config.ffmpeg.display(), bitrate_kbps = config.bitrate_kbps,
            max_streams = config.max_streams, "audio streams are on");
        Some(Self {
            ffmpeg: config.ffmpeg,
            ffprobe,
            bitrate_kbps: config.bitrate_kbps,
            streams: Arc::new(Semaphore::new(
                usize::try_from(config.max_streams).unwrap_or(usize::MAX),
            )),
            unprobeable: Mutex::new(HashSet::new()),
        })
    }
}

/// Where ffmpeg reads an item from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Input {
    /// A local file.
    File(PathBuf),
    /// An HTTP URL (a Jellyfin stream), with the request headers to send,
    /// each ending in CRLF.
    Http { url: String, headers: String },
}

impl Audio {
    /// Starts ffmpeg on `input` from `start_ms` and returns the response
    /// that streams its output (see the module docs).
    ///
    /// Fails with `503 unavailable` when `max_streams` are already
    /// running, and `502 upstream` when ffmpeg produces nothing (the item
    /// has no audio, or its source is unreadable).
    pub(crate) async fn stream(&self, input: Input, start_ms: u64) -> Result<Response, ApiError> {
        let permit = self.streams.clone().try_acquire_owned().map_err(|_| {
            ApiError::Unavailable(
                "too many audio streams are running; try again later (audio.max_streams)"
                    .to_owned(),
            )
        })?;
        let mut child = Command::new(&self.ffmpeg)
            .args(ffmpeg_args(&input, start_ms, self.bitrate_kbps))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| ApiError::Internal(err.into()))?;
        // Both are `Some`: they were just set to `piped`.
        let (Some(mut stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            return Err(ApiError::Internal(
                std::io::Error::other("ffmpeg's output pipes are missing").into(),
            ));
        };
        let errors = tokio::spawn(log_stderr(stderr));

        // Read the first chunk before answering, so a stream that fails
        // straight away is an error status rather than an empty 200.
        let mut first = vec![0; CHUNK_BYTES];
        let read = tokio::time::timeout(FIRST_BYTES_TIMEOUT, stdout.read(&mut first)).await;
        let n = match read {
            Err(_) => {
                return Err(ApiError::Upstream(format!(
                    "ffmpeg produced no audio within {FIRST_BYTES_TIMEOUT:?}"
                )));
            }
            Ok(Err(err)) => return Err(ApiError::Internal(err.into())),
            Ok(Ok(n)) => n,
        };
        if n == 0 {
            let _ = child.wait().await;
            let detail = errors.await.unwrap_or_default();
            return Err(ApiError::Upstream(format!(
                "ffmpeg produced no audio for this item: {}",
                if detail.is_empty() {
                    "no error given"
                } else {
                    &detail
                }
            )));
        }
        first.truncate(n);

        let rest = stream::unfold(
            Some(Running {
                stdout,
                child,
                permit,
            }),
            next_chunk,
        );
        let body = stream::once(async move { Ok(Bytes::from(first)) }).chain(rest);
        Ok((
            [
                (header::CONTENT_TYPE, HeaderValue::from_static("audio/ogg")),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
            Body::from_stream(body),
        )
            .into_response())
    }
}

/// A running ffmpeg, owned by the response body: dropping it (the client
/// went away) kills the process and frees its stream slot.
struct Running {
    stdout: ChildStdout,
    #[expect(
        dead_code,
        reason = "held so dropping the body kills ffmpeg (kill_on_drop)"
    )]
    child: Child,
    #[expect(dead_code, reason = "held for as long as the stream runs")]
    permit: OwnedSemaphorePermit,
}

/// The body's next chunk from ffmpeg, ending the stream at end of file or
/// after an error.
async fn next_chunk(
    state: Option<Running>,
) -> Option<(Result<Bytes, std::io::Error>, Option<Running>)> {
    let mut running = state?;
    let mut buf = vec![0; CHUNK_BYTES];
    match running.stdout.read(&mut buf).await {
        Ok(0) => None,
        Ok(n) => {
            buf.truncate(n);
            Some((Ok(Bytes::from(buf)), Some(running)))
        }
        Err(err) => Some((Err(err), None)),
    }
}

/// Logs what ffmpeg writes to stderr (only errors, with `-loglevel
/// error`), up to [`MAX_LOGGED_LINES`] lines. Returns those lines, for
/// an error response if the stream never started.
async fn log_stderr(stderr: ChildStderr) -> String {
    let mut lines = BufReader::new(stderr).lines();
    let mut kept = Vec::new();
    let mut dropped = 0usize;
    while let Ok(Some(line)) = lines.next_line().await {
        if kept.len() < MAX_LOGGED_LINES {
            tracing::warn!(ffmpeg = %line, "audio stream");
            kept.push(line);
        } else {
            dropped += 1;
        }
    }
    if dropped > 0 {
        tracing::warn!(dropped, "audio stream: more ffmpeg errors not logged");
    }
    kept.join("; ")
}

/// ffmpeg's arguments for an audio stream of `input` from `start_ms`.
pub(crate) fn ffmpeg_args(input: &Input, start_ms: u64, bitrate_kbps: u32) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-nostdin", "-hide_banner", "-loglevel", "error"]
        .into_iter()
        .map(OsString::from)
        .collect();
    if let Input::Http { headers, .. } = input {
        args.push("-headers".into());
        args.push(headers.into());
    }
    // Before `-i`: seek in the input, which is fast and starts the output's
    // timestamps at zero.
    args.push("-ss".into());
    args.push(format!("{}.{:03}", start_ms / 1000, start_ms % 1000).into());
    args.push("-i".into());
    args.push(match input {
        Input::File(path) => file_url(path),
        Input::Http { url, .. } => url.into(),
    });
    for arg in [
        // The first audio track only: no video, subtitles, or data.
        "-map", "0:a:0", "-vn", "-sn", "-dn",
        // Stereo Opus, whatever the source's channel layout.
        "-ac", "2", "-c:a", "libopus",
    ] {
        args.push(arg.into());
    }
    args.push("-b:a".into());
    args.push(format!("{bitrate_kbps}k").into());
    for arg in ["-f", "ogg", "pipe:1"] {
        args.push(arg.into());
    }
    args
}

impl App {
    /// Fills in `now`'s missing durations by probing, when audio is on
    /// (see the module docs). The current item is probed before
    /// returning, so the client gets its duration; the next item is
    /// probed in the background, ready for when the channel gets to it.
    ///
    /// Never fails: an item that cannot be probed keeps no duration, as it
    /// would without audio.
    pub(crate) async fn with_probed_durations(&self, mut now: Now) -> Now {
        let Some(audio) = &self.inner.audio else {
            return now;
        };
        if audio.ffprobe.is_none() {
            return now;
        }
        if now.item.duration_ms.is_none() {
            now.item.duration_ms = self.probe_and_store(&now.item.id).await;
        }
        if let Some(next) = &mut now.next
            && next.item.id == now.item.id
        {
            // A one-item channel: its next item is this one again.
            next.item.duration_ms = now.item.duration_ms;
        }
        if let Some(next) = &now.next
            && next.item.duration_ms.is_none()
        {
            let app = self.clone();
            let id = next.item.id.clone();
            tokio::spawn(async move { app.probe_and_store(&id).await });
        }
        now
    }

    /// Probes a filesystem item's duration and stores it, unless one is
    /// stored already. `None` if there is nothing to probe with, the item is not a file, or the probe
    /// failed (which is remembered, so it isn't tried again).
    async fn probe_and_store(&self, item_id: &str) -> Option<u64> {
        let audio = self.inner.audio.as_ref()?;
        let ffprobe = audio.ffprobe.as_ref()?;
        if audio.is_unprobeable(item_id) {
            return None;
        }
        let row = self.db().item(item_id).await.ok().flatten()?;
        if let Some(known) = row.duration_ms {
            // Stored since the caller looked (by a client, or another probe).
            return u64::try_from(known).ok();
        }
        let Some(Source::Filesystem { root }) = self.source(&row.source_id) else {
            return None;
        };
        let file = safe_join(root, &row.path)?;
        match probe_duration(ffprobe, &file).await {
            Ok(duration_ms) => {
                tracing::debug!(item = item_id, duration_ms, "probed duration");
                let stored = i64::try_from(duration_ms).unwrap_or(i64::MAX);
                if let Err(err) = self.db().set_duration(item_id, stored).await {
                    tracing::warn!(item = item_id, %err, "cannot store probed duration");
                }
                Some(duration_ms)
            }
            Err(reason) => {
                tracing::info!(item = item_id, path = %row.path, %reason,
                    "cannot probe duration; the item stays without one");
                audio.mark_unprobeable(item_id);
                None
            }
        }
    }
}

impl Audio {
    fn is_unprobeable(&self, item_id: &str) -> bool {
        self.unprobeable
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(item_id)
    }

    fn mark_unprobeable(&self, item_id: &str) {
        self.unprobeable
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(item_id.to_owned());
    }
}

/// Asks ffprobe for a media file's duration, in milliseconds.
async fn probe_duration(ffprobe: &Path, file: &Path) -> Result<u64, String> {
    let mut command = Command::new(ffprobe);
    command
        .args(["-v", "error", "-show_entries", "format=duration"])
        .args(["-of", "default=noprint_wrappers=1:nokey=1"])
        // After `--`-less options, a path starting with `-` would be read
        // as an option; `file:` makes it a path whatever it looks like.
        .arg(file_url(file))
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(PROBE_TIMEOUT, command.output()).await {
        Err(_) => return Err(format!("ffprobe took over {PROBE_TIMEOUT:?}")),
        Ok(Err(err)) => return Err(format!("cannot run ffprobe: {err}")),
        Ok(Ok(output)) => output,
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "ffprobe failed ({}): {}",
            output.status,
            stderr.trim()
        ));
    }
    parse_duration_ms(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| "ffprobe reported no duration".to_owned())
}

/// Parses ffprobe's `format=duration` (seconds, e.g. `1234.567000`) into
/// whole milliseconds. `N/A`, empty, negative, or zero means none.
fn parse_duration_ms(text: &str) -> Option<u64> {
    let seconds: f64 = text.trim().parse().ok()?;
    // Positive and finite, so the cast cannot wrap; it saturates for
    // absurd values.
    (seconds.is_finite() && seconds > 0.0).then(|| (seconds * 1000.0).round() as u64)
}

/// A local path as an ffmpeg `file:` URL, so a name that looks like an
/// option or another protocol (`-x.mkv`, `http:...`) is still a file.
pub(crate) fn file_url(path: &Path) -> std::ffi::OsString {
    let mut url = std::ffi::OsString::from("file:");
    url.push(path);
    url
}

/// Whether `ffmpeg` runs and has the libopus encoder audio streams use.
async fn runs_with_opus(ffmpeg: &Path) -> Result<(), String> {
    runs(ffmpeg).await?;
    let mut command = Command::new(ffmpeg);
    command
        .args(["-hide_banner", "-encoders"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(DETECT_TIMEOUT, command.output()).await {
        Err(_) => return Err(format!("`-encoders` took over {DETECT_TIMEOUT:?}")),
        Ok(Err(err)) => return Err(err.to_string()),
        Ok(Ok(output)) => output,
    };
    let encoders = String::from_utf8_lossy(&output.stdout);
    if encoders.split_whitespace().any(|word| word == "libopus") {
        Ok(())
    } else {
        Err("this ffmpeg has no libopus encoder".to_owned())
    }
}

/// Whether `program -version` runs and succeeds (ffmpeg and ffprobe both
/// take it). The error says why not.
async fn runs(program: &Path) -> Result<(), String> {
    let mut command = Command::new(program);
    command
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    match tokio::time::timeout(DETECT_TIMEOUT, command.status()).await {
        Err(_) => Err(format!("`-version` took over {DETECT_TIMEOUT:?}")),
        Ok(Err(err)) => Err(err.to_string()),
        Ok(Ok(status)) if !status.success() => Err(format!("`-version` failed ({status})")),
        Ok(Ok(_)) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn turned_off_or_missing_ffmpeg_means_no_audio() {
        let off = AudioConfig {
            enabled: false,
            ..AudioConfig::default()
        };
        assert!(Audio::detect(Some(&off)).await.is_none());
        let missing = AudioConfig {
            ffmpeg: PathBuf::from("/nonexistent/ffmpeg"),
            ..AudioConfig::default()
        };
        assert!(Audio::detect(Some(&missing)).await.is_none());
    }

    #[test]
    fn parses_ffprobe_durations() {
        assert_eq!(parse_duration_ms("1234.567000\n"), Some(1_234_567));
        assert_eq!(parse_duration_ms("6.0004"), Some(6_000));
        assert_eq!(parse_duration_ms("N/A"), None);
        assert_eq!(parse_duration_ms(""), None);
        assert_eq!(parse_duration_ms("0.000000"), None);
        assert_eq!(parse_duration_ms("-3"), None);
    }

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter().map(|a| a.into_string().unwrap()).collect()
    }

    #[test]
    fn ffmpeg_args_for_a_file() {
        let args = ffmpeg_args(&Input::File(PathBuf::from("/m/a b.mkv")), 61_005, 96);
        assert_eq!(
            strings(args).join(" "),
            "-nostdin -hide_banner -loglevel error -ss 61.005 -i file:/m/a b.mkv \
             -map 0:a:0 -vn -sn -dn -ac 2 -c:a libopus -b:a 96k -f ogg pipe:1"
        );
    }

    #[test]
    fn ffmpeg_args_for_jellyfin_send_headers_before_the_input() {
        let input = Input::Http {
            url: "http://jf/Videos/x/stream?static=true".to_owned(),
            headers: "Authorization: MediaBrowser Token=\"k\"\r\n".to_owned(),
        };
        let args = strings(ffmpeg_args(&input, 0, 64));
        let at = |s: &str| args.iter().position(|a| a == s).unwrap();
        assert_eq!(
            args[at("-headers") + 1],
            "Authorization: MediaBrowser Token=\"k\"\r\n"
        );
        assert!(at("-headers") < at("-i"));
        assert_eq!(args[at("-ss") + 1], "0.000");
        assert_eq!(args[at("-i") + 1], "http://jf/Videos/x/stream?static=true");
        assert_eq!(args[at("-b:a") + 1], "64k");
    }

    #[test]
    fn file_urls_keep_odd_names_as_files() {
        assert_eq!(
            file_url(Path::new("/m/-x: http.mkv")),
            std::ffi::OsString::from("file:/m/-x: http.mkv")
        );
    }

    #[tokio::test]
    async fn a_failing_program_is_not_usable() {
        // `false -version` runs but exits non-zero.
        let err = runs(Path::new("false")).await.unwrap_err();
        assert!(err.contains("failed"), "{err}");
    }
}
