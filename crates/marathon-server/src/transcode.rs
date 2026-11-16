//! Live transcoding (PLAN.md M13): the server's ability to re-encode an
//! item at a lower quality, which depends on what ffmpeg can do on the
//! machine it runs on.
//!
//! Like audio streams, transcoding is optional. [`Transcode::detect`] runs
//! when the server starts: it checks that ffmpeg works and picks an
//! encoder (see [`crate::encoder`]). Without a working H.264 encoder, or
//! with `[transcode] enabled = false`, the server runs without
//! transcoding and every client plays the original files.

// The sessions and routes that use these settings come in later commits,
// which remove this allow.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::config::TranscodeConfig;
use crate::encoder::{self, Capabilities};

/// Transcoding, when it is available.
#[derive(Debug)]
pub(crate) struct Transcode {
    pub(crate) ffmpeg: PathBuf,
    /// What the encoder detection found.
    pub(crate) capabilities: Capabilities,
    pub(crate) config: TranscodeConfig,
    /// One permit per transcoding session allowed to run at once.
    pub(crate) sessions: Arc<Semaphore>,
}

impl Transcode {
    /// Picks the encoder for `config` (the `[transcode]` table, `None`
    /// when the config has none) and returns the transcoding settings, or
    /// `None` if transcoding is off or unusable.
    ///
    /// Never fails: without a working encoder the server just has no
    /// transcoding. If `[transcode]` was configured, that is logged as a
    /// warning; without it, ffmpeg was only looked for, so it is logged as
    /// info.
    pub(crate) async fn detect(config: Option<&TranscodeConfig>) -> Option<Self> {
        let explicit = config.is_some();
        let config = config.cloned().unwrap_or_default();
        if !config.enabled {
            tracing::info!("transcoding is turned off (transcode.enabled = false)");
            return None;
        }
        let Some(capabilities) =
            encoder::detect(&config.ffmpeg, config.hardware, &config.vaapi_device).await
        else {
            let ffmpeg = config.ffmpeg.display();
            if explicit {
                tracing::warn!(%ffmpeg, hardware = ?config.hardware,
                    "no H.264 encoder works with this ffmpeg; transcoding is off");
            } else {
                tracing::info!(%ffmpeg, "no working ffmpeg H.264 encoder found; transcoding is off");
            }
            return None;
        };
        tracing::info!(
            encoder = capabilities.encoder.name(),
            hardware = capabilities.encoder.is_hardware(),
            tonemap = ?capabilities.tonemap,
            max_sessions = config.max_sessions,
            "transcoding is on"
        );
        if capabilities.tonemap.is_none() {
            tracing::warn!(
                "no HDR tone mapping works with this ffmpeg; HDR video will look washed out \
                 when transcoded"
            );
        }
        Some(Self {
            ffmpeg: config.ffmpeg.clone(),
            capabilities,
            sessions: Arc::new(Semaphore::new(
                usize::try_from(config.max_sessions).unwrap_or(usize::MAX),
            )),
            config,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[tokio::test]
    async fn turned_off_or_missing_ffmpeg_means_no_transcoding() {
        let off = TranscodeConfig {
            enabled: false,
            ..TranscodeConfig::default()
        };
        assert!(Transcode::detect(Some(&off)).await.is_none());
        let missing = TranscodeConfig {
            ffmpeg: PathBuf::from("/nonexistent/ffmpeg"),
            ..TranscodeConfig::default()
        };
        assert!(Transcode::detect(Some(&missing)).await.is_none());
    }
}
