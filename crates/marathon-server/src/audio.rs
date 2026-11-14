//! Audio-only streams (PLAN.md M12): the server runs ffmpeg to pull an
//! item's audio out of its video and transcode it to stereo Opus, for
//! clients that only want to listen (the Android client's background
//! radio).
//!
//! Audio is optional. [`Audio::detect`] runs when the server starts and
//! checks that the configured ffmpeg works; if it doesn't, or `[audio]`
//! turns it off, the server runs without audio streams and says so in its
//! log.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::config::AudioConfig;

/// How long `ffmpeg -version` may take at startup before ffmpeg counts as
/// unusable.
const DETECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The server's audio streaming, when it is available.
#[derive(Debug)]
#[expect(
    dead_code,
    reason = "read by duration probing and the audio route, added next"
)]
pub(crate) struct Audio {
    /// The ffmpeg program, known to run.
    pub(crate) ffmpeg: PathBuf,
    /// The ffprobe program, if it runs. Without it, durations are not
    /// probed and clients learn them only as they play.
    pub(crate) ffprobe: Option<PathBuf>,
    pub(crate) bitrate_kbps: u32,
    /// One permit per ffmpeg process allowed to run at once.
    pub(crate) streams: Arc<Semaphore>,
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
        })
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

    #[tokio::test]
    async fn a_failing_program_is_not_usable() {
        // `false -version` runs but exits non-zero.
        let err = runs(Path::new("false")).await.unwrap_err();
        assert!(err.contains("failed"), "{err}");
    }
}
