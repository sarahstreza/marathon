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
//! A piped audio stream has no duration a player can read, yet clients
//! need one (for the finished threshold, PLAN.md §1, and progress
//! reports). So while audio is on, the server probes a filesystem item's
//! duration with ffprobe when it first hands the item out in a `now`
//! without one ([`App::with_probed_durations`]), and stores it. Jellyfin
//! reports durations itself.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use marathon_api::Now;
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::app::App;
use crate::config::AudioConfig;
use crate::http::safe_join;
use crate::sources::Source;

/// How long `ffmpeg -version` may take at startup before ffmpeg counts as
/// unusable.
const DETECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long probing one item's duration may take. ffprobe reads only the
/// container's headers, so this is generous even for a network share.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The server's audio streaming, when it is available.
#[derive(Debug)]
#[expect(dead_code, reason = "read by the audio route, added next")]
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
        if let Err(reason) = runs(&config.ffmpeg).await {
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
